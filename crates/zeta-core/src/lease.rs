//! Lease manager: decides who may retune a port. A held port is never
//! retuned; a request that does not fit is a conflict naming the holders.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::port::{Port, PortError, Tuning};
use crate::types::{JobId, SourceId};

#[derive(Debug)]
pub enum LeaseError {
    Conflict {
        source: SourceId,
        holders: Vec<JobId>,
        current: Tuning,
        options: Vec<String>,
    },
    Port(PortError),
}

// Hand-written: thiserror treats a field named `source` as the error source.
impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::Conflict {
                source,
                holders,
                current,
                options,
            } => write!(
                f,
                "source {} is held by {:?} at {:?}; options: {}",
                source.0,
                holders.iter().map(|j| j.0.as_str()).collect::<Vec<_>>(),
                current,
                options.join("; ")
            ),
            LeaseError::Port(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LeaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LeaseError::Port(e) => Some(e),
            _ => None,
        }
    }
}

impl From<PortError> for LeaseError {
    fn from(e: PortError) -> Self {
        LeaseError::Port(e)
    }
}

#[derive(Default)]
pub struct LeaseManager {
    leases: Mutex<HashMap<SourceId, Vec<(JobId, Tuning)>>>,
    /// Per-source gate held for the whole of `ensure`, across `port.tune().await`.
    gates: Mutex<HashMap<SourceId, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

fn band(t: &Tuning) -> (u64, u64) {
    let half = t.bandwidth_hz.0 / 2;
    (
        t.center_hz.0.saturating_sub(half),
        t.center_hz.0.saturating_add(half),
    )
}

/// Whether the band `want` lies inside the band `cur`.
fn fits(want: &Tuning, cur: &Tuning) -> bool {
    let (wl, wh) = band(want);
    let (cl, ch) = band(cur);
    wl >= cl && wh <= ch
}

/// Ways out of a conflict between `want` and the port's current band `cur`
/// that the caller can act on. Widening is listed only when the port could
/// capture both bands at once; no operation performs it yet.
fn conflict_options(port: &dyn Port, want: &Tuning, cur: &Tuning) -> Vec<String> {
    let mut options = vec![
        "stop one of the holders".to_string(),
        "choose a band inside current".to_string(),
    ];
    let (wl, wh) = band(want);
    let (cl, ch) = band(cur);
    let n = wh.max(ch) - wl.min(cl);
    if n <= port.capabilities().max_bandwidth_hz.value().0 {
        options.push(format!("widen capture to {n} Hz (not yet available)"));
    }
    options
}

impl LeaseManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// The per-source gate shared by `ensure`, `tune_if_unheld` and `idle_if_unheld`.
    fn gate(&self, source: &SourceId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_default()
            .clone()
    }

    /// Call [`Port::idle`] under the source's gate when nobody holds `port`
    /// (after a `release`). A lease taken meanwhile wins: no idle then.
    pub async fn idle_if_unheld(&self, port: &dyn Port) {
        let gate = self.gate(port.id());
        let _guard = gate.lock().await;
        if self.holders(port.id()).is_empty() {
            port.idle().await;
        }
    }

    /// Grant `holder` the band `want` on `port`, tuning only if the port is unheld.
    pub async fn ensure(
        &self,
        port: &dyn Port,
        holder: JobId,
        want: Tuning,
    ) -> Result<Tuning, LeaseError> {
        let source = port.id().clone();
        let gate = self.gate(&source);
        let _guard = gate.lock().await;
        {
            let mut leases = self.leases.lock().unwrap();
            // A held port lacking a current tuning is taken to be at its
            // first holder's band, as in `tune_if_unheld`; it is never retuned.
            let first = leases.get(&source).and_then(|v| v.first()).map(|(_, t)| *t);
            if let Some(cur) = first.map(|f| port.current().unwrap_or(f)) {
                if fits(&want, &cur) {
                    let v = leases.get_mut(&source).unwrap();
                    v.retain(|(j, _)| j != &holder);
                    v.push((holder, want));
                    return Ok(cur);
                }
                let holders = leases[&source].iter().map(|(j, _)| j.clone()).collect();
                let options = conflict_options(port, &want, &cur);
                return Err(LeaseError::Conflict {
                    source,
                    holders,
                    current: cur,
                    options,
                });
            }
        }
        let got = port.tune(want).await?;
        let mut leases = self.leases.lock().unwrap();
        let v = leases.entry(source).or_default();
        v.retain(|(j, _)| j != &holder);
        v.push((holder, got));
        Ok(got)
    }

    /// Tune `port` to `want` without taking a lease, under the same per-source
    /// gate as `ensure`, so it cannot interleave with a job's lease-and-tune.
    /// A held port is never retuned: the request either fits its current band
    /// (which is returned) or is a conflict.
    pub async fn tune_if_unheld(
        &self,
        port: &dyn Port,
        want: Tuning,
    ) -> Result<Tuning, LeaseError> {
        let source = port.id().clone();
        let gate = self.gate(&source);
        let _guard = gate.lock().await;
        let held = self.holders(&source);
        let Some((_, first)) = held.first() else {
            return Ok(port.tune(want).await?);
        };
        let cur = port.current().unwrap_or(*first);
        if fits(&want, &cur) {
            return Ok(cur);
        }
        let options = conflict_options(port, &want, &cur);
        let holders = held.into_iter().map(|(j, _)| j).collect();
        Err(LeaseError::Conflict {
            source,
            holders,
            current: cur,
            options,
        })
    }

    /// Drop every lease held by `holder`.
    pub fn release(&self, holder: &JobId) {
        let mut leases = self.leases.lock().unwrap();
        leases.retain(|_, v| {
            v.retain(|(j, _)| j != holder);
            !v.is_empty()
        });
    }

    /// Current holders of `source`, in insertion order.
    pub fn holders(&self, source: &SourceId) -> Vec<(JobId, Tuning)> {
        self.leases
            .lock()
            .unwrap()
            .get(source)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{IqReceiver, Port};
    use crate::types::{Capabilities, Claim, Hz, SampleFormat};
    use async_trait::async_trait;

    struct FakePort {
        id: SourceId,
        caps: Capabilities,
        cur: Mutex<Option<Tuning>>,
        tx: tokio::sync::broadcast::Sender<crate::port::IqBlock>,
        tunes: std::sync::atomic::AtomicUsize,
        delay: bool,
    }

    impl FakePort {
        fn new() -> Self {
            Self {
                id: SourceId("fake".into()),
                caps: Capabilities {
                    tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                    tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                    max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
                    formats: vec![SampleFormat::Cf32],
                    channels: 1,
                    rx: true,
                    tx: false,
                    remote: false,
                    push: false,
                },
                cur: Mutex::new(None),
                tx: tokio::sync::broadcast::channel(1).0,
                tunes: std::sync::atomic::AtomicUsize::new(0),
                delay: false,
            }
        }
    }

    #[async_trait]
    impl Port for FakePort {
        fn id(&self) -> &SourceId {
            &self.id
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
            self.tunes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.delay {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            *self.cur.lock().unwrap() = Some(t);
            Ok(t)
        }
        fn current(&self) -> Option<Tuning> {
            *self.cur.lock().unwrap()
        }
        fn subscribe(&self) -> IqReceiver {
            self.tx.subscribe()
        }
    }

    fn t(c: u64, b: u64) -> Tuning {
        Tuning {
            center_hz: Hz(c),
            bandwidth_hz: Hz(b),
        }
    }

    #[tokio::test]
    async fn unheld_port_is_tuned() {
        let (p, m, j1) = (FakePort::new(), LeaseManager::new(), JobId::new());
        let want = t(100_000_000, 1_000_000);
        assert_eq!(m.ensure(&p, j1, want).await.unwrap(), want);
        assert_eq!(p.current(), Some(want));
    }

    #[tokio::test]
    async fn fitting_request_shares_without_retune() {
        let (p, m, j1, j2) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let first = t(100_000_000, 10_000_000);
        m.ensure(&p, j1.clone(), first).await.unwrap();
        m.ensure(&p, j2.clone(), t(102_000_000, 1_000_000))
            .await
            .unwrap();
        assert_eq!(p.current(), Some(first));
        let ids: Vec<JobId> = m.holders(p.id()).into_iter().map(|(j, _)| j).collect();
        assert_eq!(ids, vec![j1, j2]);
    }

    #[tokio::test]
    async fn conflicting_request_errors_naming_holders() {
        let (p, m, j1, j2) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let first = t(100_000_000, 1_000_000);
        m.ensure(&p, j1.clone(), first).await.unwrap();
        match m.ensure(&p, j2, t(433_000_000, 1_000_000)).await {
            Err(LeaseError::Conflict {
                holders,
                current,
                options,
                ..
            }) => {
                assert_eq!(holders, vec![j1]);
                assert_eq!(current, first);
                assert_eq!(
                    options,
                    vec!["stop one of the holders", "choose a band inside current"]
                );
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        assert_eq!(p.current(), Some(first));
    }

    #[tokio::test]
    async fn conflict_offers_widening_only_when_the_port_can() {
        let (p, m) = (FakePort::new(), LeaseManager::new());
        m.ensure(&p, JobId::new(), t(100_000_000, 1_000_000))
            .await
            .unwrap();
        match m.ensure(&p, JobId::new(), t(105_000_000, 1_000_000)).await {
            Err(LeaseError::Conflict { options, .. }) => assert_eq!(
                options,
                vec![
                    "stop one of the holders",
                    "choose a band inside current",
                    "widen capture to 6000000 Hz (not yet available)",
                ]
            ),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn held_port_without_current_is_not_retuned() {
        let (p, m) = (FakePort::new(), LeaseManager::new());
        let first = t(100_000_000, 1_000_000);
        m.ensure(&p, JobId::new(), first).await.unwrap();
        *p.cur.lock().unwrap() = None;
        match m.ensure(&p, JobId::new(), t(433_000_000, 1_000_000)).await {
            Err(LeaseError::Conflict { current, .. }) => assert_eq!(current, first),
            other => panic!("expected conflict, got {other:?}"),
        }
        let inside = t(100_100_000, 100_000);
        assert_eq!(m.ensure(&p, JobId::new(), inside).await.unwrap(), first);
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(m.holders(p.id()).len(), 2);
    }

    #[tokio::test]
    async fn release_frees_port() {
        let (p, m, j1, j2) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        m.ensure(&p, j1.clone(), t(100_000_000, 1_000_000))
            .await
            .unwrap();
        m.release(&j1);
        let want = t(433_000_000, 1_000_000);
        assert_eq!(m.ensure(&p, j2, want).await.unwrap(), want);
    }

    #[tokio::test]
    async fn concurrent_ensure_on_unheld_port_yields_one_tune() {
        let mut p = FakePort::new();
        p.delay = true;
        let m = LeaseManager::new();
        let (j1, j2) = (JobId::new(), JobId::new());
        let (a, b) = tokio::join!(
            m.ensure(&p, j1, t(100_000_000, 1_000_000)),
            m.ensure(&p, j2, t(433_000_000, 1_000_000)),
        );
        let conflicts = [&a, &b]
            .iter()
            .filter(|r| matches!(r, Err(LeaseError::Conflict { .. })))
            .count();
        assert_eq!(conflicts, 1);
        assert_eq!([&a, &b].iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(m.holders(p.id()).len(), 1);
    }
}
