//! Lease manager: decides who may retune a port. A held port is never
//! retuned; a request that does not fit is a conflict naming the holders.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::port::{Port, PortError, Tuning};
use crate::types::{JobId, SourceId};

#[derive(Debug)]
pub enum LeaseError {
    Conflict { source: SourceId, holders: Vec<JobId>, current: Tuning, options: Vec<String> },
    Port(PortError),
}

// Hand-written: thiserror treats a field named `source` as the error source.
impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::Conflict { source, holders, current, options } => write!(
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
}

fn band(t: &Tuning) -> (u64, u64) {
    let half = t.bandwidth_hz.0 / 2;
    (t.center_hz.0.saturating_sub(half), t.center_hz.0.saturating_add(half))
}

fn fits(want: &Tuning, cur: &Tuning) -> bool {
    let (wl, wh) = band(want);
    let (cl, ch) = band(cur);
    wl >= cl && wh <= ch
}

impl LeaseManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant `holder` the band `want` on `port`, tuning only if the port is unheld.
    pub async fn ensure(
        &self,
        port: &dyn Port,
        holder: JobId,
        want: Tuning,
    ) -> Result<Tuning, LeaseError> {
        let source = port.id().clone();
        {
            let mut leases = self.leases.lock().unwrap();
            let held = leases.get(&source).is_some_and(|v| !v.is_empty());
            if held {
                if let Some(cur) = port.current() {
                    if fits(&want, &cur) {
                        let v = leases.get_mut(&source).unwrap();
                        v.retain(|(j, _)| j != &holder);
                        v.push((holder, want));
                        return Ok(cur);
                    }
                    let holders = leases[&source].iter().map(|(j, _)| j.clone()).collect();
                    let mut options = vec!["stop one of the holders".to_string()];
                    let (wl, wh) = band(&want);
                    let (cl, ch) = band(&cur);
                    let n = wh.max(ch) - wl.min(cl);
                    if n <= port.capabilities().max_bandwidth_hz.value().0 {
                        options.push(format!("widen capture to {n} Hz"));
                    }
                    options.push("run a scan job".to_string());
                    return Err(LeaseError::Conflict { source, holders, current: cur, options });
                }
            }
        }
        let got = port.tune(want).await?;
        let mut leases = self.leases.lock().unwrap();
        let v = leases.entry(source).or_default();
        v.retain(|(j, _)| j != &holder);
        v.push((holder, want));
        Ok(got)
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
        self.leases.lock().unwrap().get(source).cloned().unwrap_or_default()
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
        Tuning { center_hz: Hz(c), bandwidth_hz: Hz(b) }
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
        let (p, m, j1, j2) = (FakePort::new(), LeaseManager::new(), JobId::new(), JobId::new());
        let first = t(100_000_000, 10_000_000);
        m.ensure(&p, j1.clone(), first).await.unwrap();
        m.ensure(&p, j2.clone(), t(102_000_000, 1_000_000)).await.unwrap();
        assert_eq!(p.current(), Some(first));
        let ids: Vec<JobId> = m.holders(p.id()).into_iter().map(|(j, _)| j).collect();
        assert_eq!(ids, vec![j1, j2]);
    }

    #[tokio::test]
    async fn conflicting_request_errors_naming_holders() {
        let (p, m, j1, j2) = (FakePort::new(), LeaseManager::new(), JobId::new(), JobId::new());
        let first = t(100_000_000, 1_000_000);
        m.ensure(&p, j1.clone(), first).await.unwrap();
        match m.ensure(&p, j2, t(433_000_000, 1_000_000)).await {
            Err(LeaseError::Conflict { holders, options, .. }) => {
                assert_eq!(holders, vec![j1]);
                assert!(options.iter().any(|o| o.contains("scan")));
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        assert_eq!(p.current(), Some(first));
    }

    #[tokio::test]
    async fn release_frees_port() {
        let (p, m, j1, j2) = (FakePort::new(), LeaseManager::new(), JobId::new(), JobId::new());
        m.ensure(&p, j1.clone(), t(100_000_000, 1_000_000)).await.unwrap();
        m.release(&j1);
        let want = t(433_000_000, 1_000_000);
        assert_eq!(m.ensure(&p, j2, want).await.unwrap(), want);
    }
}
