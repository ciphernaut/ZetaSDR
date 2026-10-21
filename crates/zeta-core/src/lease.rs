//! Lease manager: decides who may retune a port. A held port is never
//! retuned; a request that does not fit is a conflict naming the holders.
//! An elastic lease (a `spectrum` job following the capture) holds nothing:
//! rigid requests ignore it, and it never retunes a held port.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

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

/// One holder's lease on a source. A `time_shared` lease (a scan) covers a
/// band wider than any one capture: its holder retunes the port across it,
/// so no other request fits while it is held. An `elastic` lease's band is
/// the capture it joined; it is never a holder for a rigid request. A `pin`
/// lease (under [`pin_holder`]) is rigid but not a job: it holds the source like
/// one and is never listed among the holders.
#[derive(Clone, Debug)]
struct Lease {
    job: JobId,
    band: Tuning,
    time_shared: bool,
    elastic: bool,
    pin: bool,
}

impl Lease {
    fn new(job: JobId, band: Tuning, elastic: bool) -> Self {
        Self {
            job,
            band,
            time_shared: false,
            elastic,
            pin: false,
        }
    }
}

/// The holder id of `source`'s pin: `pin:<source id>`, never a job's (a ULID).
pub fn pin_holder(source: &SourceId) -> JobId {
    JobId(format!("pin:{}", source.0))
}

#[derive(Default)]
pub struct LeaseManager {
    leases: Mutex<HashMap<SourceId, Vec<Lease>>>,
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
/// that the caller can act on. Stopping a holder is listed only when a job
/// holds the source (a pin alone is no holder). Widening is listed only when the port could
/// capture both bands at once; no operation performs it yet. A port under a
/// time-shared lease (a scan) is freed only by stopping that holder. Then, on a
/// `pinned` source, its unpin; last, the request again with `replace` naming the
/// rigid `holders`, when there are any.
fn conflict_options(
    port: &dyn Port,
    want: &Tuning,
    cur: &Tuning,
    shared: bool,
    holders: &[JobId],
    pinned: Option<&SourceId>,
) -> Vec<String> {
    if shared {
        return vec!["stop the scan holding the source".to_string()];
    }
    let mut options = Vec::new();
    if !holders.is_empty() {
        options.push("stop one of the holders".to_string());
    }
    options.push("choose a band inside current".to_string());
    let (wl, wh) = band(want);
    let (cl, ch) = band(cur);
    let n = wh.max(ch) - wl.min(cl);
    if n <= port.capabilities().max_bandwidth_hz.value().0 {
        options.push(format!("widen capture to {n} Hz (not yet available)"));
    }
    if let Some(id) = pinned {
        options.push(format!("unpin source {} (source_unpin)", id.0));
    }
    if !holders.is_empty() {
        let ids: Vec<&str> = holders.iter().map(|j| j.0.as_str()).collect();
        options.push(format!("retry with replace: [{}]", ids.join(", ")));
    }
    options
}

/// The rigid holders among `leases`: those a conflict names (never a pin).
fn rigid(leases: &[Lease]) -> Vec<JobId> {
    leases
        .iter()
        .filter(|l| !l.elastic && !l.pin)
        .map(|l| l.job.clone())
        .collect()
}

/// `source` when `leases` hold a pin on it, for [`conflict_options`].
fn pinned_in<'a>(leases: &[Lease], source: &'a SourceId) -> Option<&'a SourceId> {
    leases.iter().any(|l| l.pin).then_some(source)
}

impl LeaseManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// The per-source gate shared by `ensure`, `tune_if_unheld` and `idle_if_unheld`.
    fn gate(&self, source: &SourceId) -> Arc<tokio::sync::Mutex<()>> {
        self.gates
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_default()
            .clone()
    }

    /// Hold `source`'s gate until the guard is dropped, so a caller can do several
    /// things nothing interleaves with (stop holders, then lease), through the
    /// `_locked` calls. Never call a gate-taking method while holding it: the
    /// gate is not reentrant.
    pub(crate) async fn gate_guard(&self, source: &SourceId) -> OwnedMutexGuard<()> {
        self.gate(source).lock_owned().await
    }

    /// Panics unless `guard` is `source`'s gate: a `_locked` call under another
    /// source's gate would interleave with this one's.
    pub(crate) fn assert_gate(&self, source: &SourceId, guard: &OwnedMutexGuard<()>) {
        assert!(
            Arc::ptr_eq(OwnedMutexGuard::mutex(guard), &self.gate(source)),
            "guard is not the gate of source {}",
            source.0
        );
    }

    /// Call [`Port::idle`] under the source's gate when nobody holds `port`
    /// (after a `release`). A lease taken meanwhile wins: no idle then.
    pub async fn idle_if_unheld(&self, port: &dyn Port) {
        let gate = self.gate(port.id());
        let _guard = gate.lock().await;
        if !self.leases.lock().unwrap().contains_key(port.id()) {
            port.idle().await;
        }
    }

    /// Grant `holder` the band `want` on `port`, tuning only if the port is unheld.
    ///
    /// A rigid request (`elastic` false) is never refused by elastic leases:
    /// when only those hold the port, a band inside the capture they follow
    /// is leased without a tune, and any other band is tuned as if unheld
    /// (the caller relaunches the elastic jobs on the new capture). An elastic request on a held port
    /// joins the current capture (its lease band, and the result, is that
    /// capture) without a tune and never conflicts, except with a time-shared
    /// lease (a scan), which no capture outlasts.
    pub async fn ensure(
        &self,
        port: &dyn Port,
        holder: JobId,
        want: Tuning,
        elastic: bool,
    ) -> Result<Tuning, LeaseError> {
        self.ensure_tuned(port, holder, want, elastic)
            .await
            .map(|(t, _)| t)
    }

    /// [`LeaseManager::ensure`], also saying whether it tuned the port (so the
    /// caller relaunches the elastic jobs that followed the old capture).
    pub(crate) async fn ensure_tuned(
        &self,
        port: &dyn Port,
        holder: JobId,
        want: Tuning,
        elastic: bool,
    ) -> Result<(Tuning, bool), LeaseError> {
        let guard = self.gate_guard(port.id()).await;
        self.ensure_locked(port, holder, want, elastic, &guard)
            .await
    }

    /// [`LeaseManager::ensure_tuned`] under the source's gate the caller holds
    /// (`guard`, from [`LeaseManager::gate_guard`]).
    pub(crate) async fn ensure_locked(
        &self,
        port: &dyn Port,
        holder: JobId,
        want: Tuning,
        elastic: bool,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(Tuning, bool), LeaseError> {
        self.ensure_as(port, holder, want, elastic, false, guard)
            .await
    }

    /// [`LeaseManager::ensure_locked`], the lease marked `pin` when asked (so it
    /// is never listed among the holders, not even for an instant).
    async fn ensure_as(
        &self,
        port: &dyn Port,
        holder: JobId,
        want: Tuning,
        elastic: bool,
        pin: bool,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(Tuning, bool), LeaseError> {
        let lease = |band: Tuning| Lease {
            pin,
            ..Lease::new(holder.clone(), band, elastic)
        };
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        {
            let mut leases = self.leases.lock().unwrap();
            let held = leases.get(&source).cloned().unwrap_or_default();
            let shared = held.iter().any(|l| l.time_shared);
            // The leases this request must respect: every one for an elastic
            // request, the rigid ones for a rigid request.
            let first = held.iter().find(|l| elastic || !l.elastic).map(|l| l.band);
            // A held port lacking a current tuning is taken to be at its
            // first holder's band, as in `tune_if_unheld`; it is never retuned.
            if let Some(cur) = first.map(|f| port.current().unwrap_or(f)) {
                if elastic && !shared {
                    let v = leases.get_mut(&source).unwrap();
                    v.retain(|l| l.job != holder);
                    v.push(lease(cur));
                    return Ok((cur, false));
                }
                if !elastic && !shared && fits(&want, &cur) {
                    let v = leases.get_mut(&source).unwrap();
                    v.retain(|l| l.job != holder);
                    v.push(lease(want));
                    return Ok((cur, false));
                }
                let holders = rigid(&held);
                let pinned = pinned_in(&held, &source);
                let options = conflict_options(port, &want, &cur, shared, &holders, pinned);
                return Err(LeaseError::Conflict {
                    source,
                    holders,
                    current: cur,
                    options,
                });
            }
            // Only elastic leases (or none): a rigid request inside the
            // capture they follow is leased on it untuned, so it never
            // shrinks a shared capture; otherwise it tunes as if unheld.
            if let Some(cur) = (!held.is_empty())
                .then(|| port.current())
                .flatten()
                .filter(|c| fits(&want, c))
            {
                let v = leases.get_mut(&source).unwrap();
                v.retain(|l| l.job != holder);
                v.push(lease(want));
                return Ok((cur, false));
            }
        }
        let got = port.tune(want).await?;
        let mut leases = self.leases.lock().unwrap();
        let v = leases.entry(source).or_default();
        v.retain(|l| l.job != holder);
        // A pin's band is the one asked for: the capture may be wider.
        v.push(lease(if pin { want } else { got }));
        Ok((got, true))
    }

    /// Pin `port` at `want` (see [`pin_holder`]) under the source's gate the
    /// caller holds: any existing pin on the source is set aside, then the band
    /// is leased as a rigid request (`ensure_locked`), the lease marked `pin`. On
    /// an error the old pin, if any, is put back. Also whether it tuned the port.
    pub(crate) async fn pin_locked(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(Tuning, bool), LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let holder = pin_holder(&source);
        let old: Vec<Lease> = {
            let mut leases = self.leases.lock().unwrap();
            let v = leases.entry(source.clone()).or_default();
            let old = v.iter().filter(|l| l.pin).cloned().collect();
            v.retain(|l| !l.pin);
            if v.is_empty() {
                leases.remove(&source);
            }
            old
        };
        let r = self.ensure_as(port, holder, want, false, true, guard).await;
        if r.is_err() && !old.is_empty() {
            self.leases
                .lock()
                .unwrap()
                .entry(source)
                .or_default()
                .extend(old);
        }
        r
    }

    /// [`LeaseManager::pin_locked`], taking the source's gate.
    pub async fn pin(&self, port: &dyn Port, want: Tuning) -> Result<(Tuning, bool), LeaseError> {
        let guard = self.gate_guard(port.id()).await;
        self.pin_locked(port, want, &guard).await
    }

    /// Drop `source`'s pin; the pinned band, or `None` when it had none. The
    /// caller idles the port if nothing else holds it.
    pub fn unpin(&self, source: &SourceId) -> Option<Tuning> {
        let pinned = self.pinned(source)?;
        self.release(&pin_holder(source));
        Some(pinned)
    }

    /// The band `source` is pinned at, if it is.
    pub fn pinned(&self, source: &SourceId) -> Option<Tuning> {
        self.leases
            .lock()
            .unwrap()
            .get(source)
            .and_then(|v| v.iter().find(|l| l.pin).map(|l| l.band))
    }

    /// Grant `holder` a time-shared lease on the whole `span` of `port`
    /// without tuning it (see [`LeaseManager::retune_shared`]). The port must
    /// be unheld by rigid leases (elastic ones are the caller's to stop: no
    /// capture outlasts a step); while the lease is held every other `ensure`
    /// and `tune_if_unheld` is a conflict naming `holder`.
    pub async fn acquire_shared(
        &self,
        port: &dyn Port,
        holder: JobId,
        span: Tuning,
    ) -> Result<(), LeaseError> {
        let guard = self.gate_guard(port.id()).await;
        self.acquire_shared_locked(port, holder, span, &guard)
    }

    /// [`LeaseManager::acquire_shared`] under the source's gate the caller holds.
    pub(crate) fn acquire_shared_locked(
        &self,
        port: &dyn Port,
        holder: JobId,
        span: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let mut leases = self.leases.lock().unwrap();
        let v = leases.entry(source.clone()).or_default();
        v.retain(|l| l.job != holder);
        if let Some(first) = v.iter().find(|l| !l.elastic) {
            let cur = port.current().unwrap_or(first.band);
            let shared = v.iter().any(|l| l.time_shared);
            let holders = rigid(v);
            let pinned = pinned_in(v, &source);
            return Err(LeaseError::Conflict {
                options: conflict_options(port, &span, &cur, shared, &holders, pinned),
                holders,
                source,
                current: cur,
            });
        }
        v.push(Lease {
            job: holder,
            band: span,
            time_shared: true,
            elastic: false,
            pin: false,
        });
        Ok(())
    }

    /// Tune `port` to `step` for the holder of a time-shared lease, under
    /// the source's gate. `None` (and no tune) once `holder` holds no
    /// time-shared lease on `port`, or when `cancel` fires while it waits for
    /// the gate (whoever holds it may be stopping the scan); once the gate is
    /// taken the tune is never abandoned half way.
    pub async fn retune_shared(
        &self,
        port: &dyn Port,
        holder: &JobId,
        step: Tuning,
        cancel: &CancellationToken,
    ) -> Result<Option<Tuning>, PortError> {
        let _guard = tokio::select! {
            _ = cancel.cancelled() => return Ok(None),
            g = self.gate_guard(port.id()) => g,
        };
        let held = self
            .leases
            .lock()
            .unwrap()
            .get(port.id())
            .is_some_and(|v| v.iter().any(|l| &l.job == holder && l.time_shared));
        if !held {
            return Ok(None);
        }
        port.tune(step).await.map(Some)
    }

    /// Tune `port` to `want` without taking a lease, under the same per-source
    /// gate as `ensure`, so it cannot interleave with a job's lease-and-tune.
    /// A held port is never retuned: the request either fits its current band
    /// (which is returned) or is a conflict. Elastic leases do not hold it, but
    /// a band inside the capture they follow is returned untuned.
    pub async fn tune_if_unheld(
        &self,
        port: &dyn Port,
        want: Tuning,
    ) -> Result<Tuning, LeaseError> {
        self.tune_if_unheld_tuned(port, want).await.map(|(t, _)| t)
    }

    /// [`LeaseManager::tune_if_unheld`], also saying whether it tuned the port.
    pub(crate) async fn tune_if_unheld_tuned(
        &self,
        port: &dyn Port,
        want: Tuning,
    ) -> Result<(Tuning, bool), LeaseError> {
        let guard = self.gate_guard(port.id()).await;
        self.tune_if_unheld_locked(port, want, &guard).await
    }

    /// [`LeaseManager::tune_if_unheld_tuned`] under the source's gate the caller
    /// holds (`guard`, from [`LeaseManager::gate_guard`]).
    pub(crate) async fn tune_if_unheld_locked(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(Tuning, bool), LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let held: Vec<Lease> = self
            .leases
            .lock()
            .unwrap()
            .get(&source)
            .map(|v| v.iter().filter(|l| !l.elastic).cloned().collect())
            .unwrap_or_default();
        let Some(first) = held.first() else {
            // Only elastic leases: a band inside their capture needs no tune.
            let elastic = !self.elastic_holders(&source).is_empty();
            if let Some(cur) = elastic
                .then(|| port.current())
                .flatten()
                .filter(|c| fits(&want, c))
            {
                return Ok((cur, false));
            }
            return Ok((port.tune(want).await?, true));
        };
        let cur = port.current().unwrap_or(first.band);
        let shared = held.iter().any(|l| l.time_shared);
        if !shared && fits(&want, &cur) {
            return Ok((cur, false));
        }
        let holders = rigid(&held);
        let pinned = pinned_in(&held, &source);
        let options = conflict_options(port, &want, &cur, shared, &holders, pinned);
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
            v.retain(|l| &l.job != holder);
            !v.is_empty()
        });
    }

    /// Whether `source` is held by a time-shared lease (a scan).
    pub fn time_shared(&self, source: &SourceId) -> bool {
        self.leases
            .lock()
            .unwrap()
            .get(source)
            .is_some_and(|v| v.iter().any(|l| l.time_shared))
    }

    /// Current rigid holders of `source`, in insertion order, with the band
    /// each leased (a time-shared holder's is its whole span). Elastic leases
    /// are not holders (see [`LeaseManager::elastic_holders`]), nor is a pin
    /// ([`LeaseManager::pinned`]).
    pub fn holders(&self, source: &SourceId) -> Vec<(JobId, Tuning)> {
        self.leases
            .lock()
            .unwrap()
            .get(source)
            .map(|v| {
                v.iter()
                    .filter(|l| !l.elastic && !l.pin)
                    .map(|l| (l.job.clone(), l.band))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The jobs holding an elastic lease on `source`, in insertion order.
    pub fn elastic_holders(&self, source: &SourceId) -> Vec<JobId> {
        self.leases
            .lock()
            .unwrap()
            .get(source)
            .map(|v| {
                v.iter()
                    .filter(|l| l.elastic)
                    .map(|l| l.job.clone())
                    .collect()
            })
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
        assert_eq!(m.ensure(&p, j1, want, false).await.unwrap(), want);
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
        m.ensure(&p, j1.clone(), first, false).await.unwrap();
        m.ensure(&p, j2.clone(), t(102_000_000, 1_000_000), false)
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
        m.ensure(&p, j1.clone(), first, false).await.unwrap();
        match m.ensure(&p, j2, t(433_000_000, 1_000_000), false).await {
            Err(LeaseError::Conflict {
                holders,
                current,
                options,
                ..
            }) => {
                assert_eq!(
                    options,
                    vec![
                        "stop one of the holders".to_string(),
                        "choose a band inside current".to_string(),
                        format!("retry with replace: [{}]", j1.0),
                    ]
                );
                assert_eq!(holders, vec![j1]);
                assert_eq!(current, first);
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        assert_eq!(p.current(), Some(first));
    }

    #[tokio::test]
    async fn conflict_offers_widening_only_when_the_port_can() {
        let (p, m, j1) = (FakePort::new(), LeaseManager::new(), JobId::new());
        m.ensure(&p, j1.clone(), t(100_000_000, 1_000_000), false)
            .await
            .unwrap();
        match m
            .ensure(&p, JobId::new(), t(105_000_000, 1_000_000), false)
            .await
        {
            Err(LeaseError::Conflict { options, .. }) => assert_eq!(
                options,
                vec![
                    "stop one of the holders".to_string(),
                    "choose a band inside current".to_string(),
                    "widen capture to 6000000 Hz (not yet available)".to_string(),
                    format!("retry with replace: [{}]", j1.0),
                ]
            ),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn held_port_without_current_is_not_retuned() {
        let (p, m) = (FakePort::new(), LeaseManager::new());
        let first = t(100_000_000, 1_000_000);
        m.ensure(&p, JobId::new(), first, false).await.unwrap();
        *p.cur.lock().unwrap() = None;
        match m
            .ensure(&p, JobId::new(), t(433_000_000, 1_000_000), false)
            .await
        {
            Err(LeaseError::Conflict { current, .. }) => assert_eq!(current, first),
            other => panic!("expected conflict, got {other:?}"),
        }
        let inside = t(100_100_000, 100_000);
        assert_eq!(
            m.ensure(&p, JobId::new(), inside, false).await.unwrap(),
            first
        );
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(m.holders(p.id()).len(), 2);
    }

    #[tokio::test]
    async fn rigid_request_tunes_over_elastic_leases() {
        let (p, m, e, r) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let first = t(100_000_000, 1_000_000);
        assert_eq!(
            m.ensure_tuned(&p, e.clone(), first, true).await.unwrap(),
            (first, true)
        );
        let want = t(433_000_000, 1_000_000);
        assert_eq!(
            m.ensure_tuned(&p, r.clone(), want, false).await.unwrap(),
            (want, true)
        );
        assert_eq!(p.current(), Some(want));
        assert_eq!(m.holders(p.id()), vec![(r, want)]);
        assert_eq!(m.elastic_holders(p.id()), vec![e]);
    }

    #[tokio::test]
    async fn rigid_request_inside_an_elastic_capture_is_leased_without_a_tune() {
        let (p, m, e, r) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let wide = t(100_000_000, 10_000_000);
        m.ensure(&p, e.clone(), wide, true).await.unwrap();
        let narrow = t(102_000_000, 200_000);
        assert_eq!(
            m.ensure_tuned(&p, r.clone(), narrow, false).await.unwrap(),
            (wide, false)
        );
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(p.current(), Some(wide));
        assert_eq!(m.holders(p.id()), vec![(r.clone(), narrow)]);
        assert_eq!(m.elastic_holders(p.id()), vec![e.clone()]);
        // The same for a lease-free tune: inside the elastic capture, no tune.
        m.release(&r);
        assert_eq!(
            m.tune_if_unheld_tuned(&p, narrow).await.unwrap(),
            (wide, false)
        );
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(m.elastic_holders(p.id()), vec![e]);
    }

    #[tokio::test]
    async fn elastic_request_joins_a_held_capture() {
        let (p, m, r, e) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let first = t(100_000_000, 1_000_000);
        m.ensure(&p, r.clone(), first, false).await.unwrap();
        let far = t(433_000_000, 1_000_000);
        assert_eq!(
            m.ensure_tuned(&p, e.clone(), far, true).await.unwrap(),
            (first, false)
        );
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(m.holders(p.id()), vec![(r.clone(), first)]);
        assert_eq!(m.elastic_holders(p.id()), vec![e.clone()]);
        // Only the elastic lease left: unheld for a rigid tune, and still not idle.
        m.release(&r);
        assert_eq!(m.tune_if_unheld_tuned(&p, far).await.unwrap(), (far, true));
        assert_eq!(m.elastic_holders(p.id()), vec![e]);
    }

    #[tokio::test]
    async fn locked_calls_run_under_a_held_gate_and_nothing_interleaves() {
        let (p, m, j1, j2) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let first = t(100_000_000, 1_000_000);
        let guard = m.gate_guard(p.id()).await;
        assert_eq!(
            m.tune_if_unheld_locked(&p, first, &guard).await.unwrap(),
            (first, true)
        );
        // A competing request waits for the guard...
        let other = m.ensure(&p, j2, t(433_000_000, 1_000_000), false);
        tokio::pin!(other);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut other)
                .await
                .is_err()
        );
        // ...while the holder of the guard leases the band (an unheld port is
        // tuned again, as by `ensure`).
        assert_eq!(
            m.ensure_locked(&p, j1.clone(), first, false, &guard)
                .await
                .unwrap(),
            (first, true)
        );
        drop(guard);
        match other.await {
            Err(LeaseError::Conflict { holders, .. }) => assert_eq!(holders, vec![j1]),
            r => panic!("expected conflict, got {r:?}"),
        }
    }

    #[tokio::test]
    async fn retune_shared_waiting_on_the_gate_gives_up_on_cancel() {
        let (p, m, scan) = (FakePort::new(), LeaseManager::new(), JobId::new());
        m.acquire_shared(&p, scan.clone(), t(100_000_000, 40_000_000))
            .await
            .unwrap();
        let guard = m.gate_guard(p.id()).await;
        let cancel = CancellationToken::new();
        let step = m.retune_shared(&p, &scan, t(100_000_000, 10_000_000), &cancel);
        tokio::pin!(step);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut step)
                .await
                .is_err()
        );
        cancel.cancel();
        assert_eq!(step.await.unwrap(), None);
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(guard);
    }

    #[tokio::test]
    async fn release_frees_port() {
        let (p, m, j1, j2) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        m.ensure(&p, j1.clone(), t(100_000_000, 1_000_000), false)
            .await
            .unwrap();
        m.release(&j1);
        let want = t(433_000_000, 1_000_000);
        assert_eq!(m.ensure(&p, j2, want, false).await.unwrap(), want);
    }

    #[tokio::test]
    async fn concurrent_ensure_on_unheld_port_yields_one_tune() {
        let mut p = FakePort::new();
        p.delay = true;
        let m = LeaseManager::new();
        let (j1, j2) = (JobId::new(), JobId::new());
        let (a, b) = tokio::join!(
            m.ensure(&p, j1, t(100_000_000, 1_000_000), false),
            m.ensure(&p, j2, t(433_000_000, 1_000_000), false),
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
