//! Lease manager: decides who may retune a port. A held port is never
//! retuned; a request that does not fit is a conflict naming the holders.
//! An elastic lease (a `spectrum` job following the capture) holds nothing:
//! rigid requests ignore it, and it never retunes a held port. A sweep claim
//! (a scan the source sweeps for by its own means) holds nothing either, and no
//! IQ request sees it; it only keeps the port from being idled.
//!
//! A time-shared lease that gives way ([`LeaseManager::acquire_shared_locked`] with
//! `yields`: a stepped scan that keeps the depth in force) blocks nothing either: every request
//! is decided as if it were not there, and one that is granted takes it away under the
//! source's gate (its holder's token fired, the holder listed as waiting,
//! [`LeaseManager::waiting`]). The holder takes it back with
//! [`LeaseManager::resume_shared_locked`] once nothing rigid, no pin and no other
//! time-shared lease holds the source, watching [`LeaseManager::changes`] (bumped by
//! every yield and every release) rather than polling.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::port::{Port, PortError, Tuning};
use crate::types::{Capabilities, CapturePolicy, Hz, JobId, SourceId};

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
/// so no other request fits while it is held, unless it `yields`: then the
/// first request granted takes it away (the token fired, so its holder stops
/// at once). An `elastic` lease's band is
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
    yields: Option<CancellationToken>,
}

impl Lease {
    fn new(job: JobId, band: Tuning, elastic: bool) -> Self {
        Self {
            job,
            band,
            time_shared: false,
            elastic,
            pin: false,
            yields: None,
        }
    }
}

/// `leases` as a request sees them: without the time-shared leases that give way.
fn standing(leases: &[Lease]) -> Vec<Lease> {
    leases
        .iter()
        .filter(|l| l.yields.is_none())
        .cloned()
        .collect()
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
    /// The scans each source is sweeping for by its own means ([`Port::sweep`]),
    /// in claim order. A claim holds no band and no IQ request reads it: only
    /// [`LeaseManager::sweepers`] and [`LeaseManager::idle_if_unheld`] do.
    sweeps: Mutex<HashMap<SourceId, Vec<JobId>>>,
    /// The holders whose time-shared lease on each source gave way (or never was
    /// granted) and who wait to take it, in order; [`LeaseManager::release`] drops them.
    waiting: Mutex<HashMap<SourceId, Vec<JobId>>>,
    /// Bumped on every yield and every release on a source ([`LeaseManager::changes`]).
    changes: Mutex<HashMap<SourceId, tokio::sync::watch::Sender<u64>>>,
    /// The sources whose capture policy is [`CapturePolicy::Widest`]
    /// ([`LeaseManager::set_capture_policy`]).
    widest: Mutex<HashSet<SourceId>>,
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
/// holds the source (a pin alone is no holder). A request for a band (`want`
/// given) may choose one inside `cur`, and widening is listed only when the port
/// could capture both bands at once ([`LeaseManager::widen_locked`]). A port under a
/// time-shared lease that does not give way (a scan at a lossy depth) is freed only
/// by stopping that holder; one that gives way never makes a conflict. Then, on a
/// `pinned` source, its unpin; last, the request again with `replace` naming the
/// rigid `holders`, when there are any.
fn conflict_options(
    port: &dyn Port,
    want: Option<&Tuning>,
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
    if let Some(want) = want {
        options.push("choose a band inside current".to_string());
        let (wl, wh) = band(want);
        let (cl, ch) = band(cur);
        let (lo, hi) = (wl.min(cl), wh.max(ch));
        let n = hi - lo;
        if n <= port.capabilities().max_bandwidth_hz.value().0 {
            options.push(format!(
                "widen the capture to {}/{n} Hz (source_ensure with widen: the holders restart on it)",
                lo + n / 2
            ));
        }
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

/// What a rigid request of a band would get on a source, decided without touching
/// the port or the leases (see [`LeaseManager::decide`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// The band lies inside the current capture (`Fits.0`): leased there, no tune.
    Fits(Tuning),
    /// Only elastic leases, or none, hold the source and the band is not inside
    /// their capture: the port is tuned to the band.
    Tune,
    /// The band does not fit what holds the source: the fields of
    /// [`LeaseError::Conflict`].
    Conflict {
        holders: Vec<JobId>,
        current: Tuning,
        options: Vec<String>,
    },
}

/// The decision for a rigid request of `want` on `port` held by `held`, as
/// [`LeaseManager::ensure_locked`] takes it. A `pin` request inside a capture only
/// elastic leases follow is `Tune` rather than `Fits` (R13: a pin's purpose is the
/// capture itself).
fn rigid_decision(port: &dyn Port, held: &[Lease], want: &Tuning, pin: bool) -> Decision {
    let source = port.id();
    let shared = held.iter().any(|l| l.time_shared);
    // The leases a rigid request must respect: the rigid ones. A held port lacking
    // a current tuning is taken to be at its first holder's band, as in
    // `tune_if_unheld`; it is never retuned.
    if let Some(cur) = held
        .iter()
        .find(|l| !l.elastic)
        .map(|l| port.current().unwrap_or(l.band))
    {
        if !shared && fits(want, &cur) {
            return Decision::Fits(cur);
        }
        let holders = rigid(held);
        let options = conflict_options(
            port,
            Some(want),
            &cur,
            shared,
            &holders,
            pinned_in(held, source),
        );
        return Decision::Conflict {
            holders,
            current: cur,
            options,
        };
    }
    // Only elastic leases (or none): a rigid request inside the capture they follow
    // is leased on it untuned, so it never shrinks a shared capture; otherwise it
    // tunes as if unheld.
    match (!pin && !held.is_empty())
        .then(|| port.current())
        .flatten()
        .filter(|c| fits(want, c))
    {
        Some(cur) => Decision::Fits(cur),
        None => Decision::Tune,
    }
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
    /// (after a `release`). A lease taken meanwhile wins: no idle then. Nor
    /// while a sweep claim is held: the source's own sweep may need what an
    /// idle stops upstream; nor while a scan waits for the source: it takes it
    /// back at once.
    pub async fn idle_if_unheld(&self, port: &dyn Port) {
        let gate = self.gate(port.id());
        let _guard = gate.lock().await;
        let leased = self.leases.lock().unwrap().contains_key(port.id());
        let waited = self.waiting.lock().unwrap().contains_key(port.id());
        if !leased && !waited && !self.sweeps.lock().unwrap().contains_key(port.id()) {
            port.idle().await;
        }
    }

    /// Record that `holder` (a scan) sweeps on `source` by the source's own
    /// means. It never refuses (what the source cannot do at once it refuses in
    /// [`Port::sweep`]) and takes no band: no IQ request sees it, and a stepped
    /// scan's time-shared lease is granted beside it. A second claim by the same
    /// holder is the same claim; [`LeaseManager::release`] drops it.
    pub fn claim_sweep(&self, source: &SourceId, holder: JobId) {
        let mut sweeps = self.sweeps.lock().unwrap();
        let v = sweeps.entry(source.clone()).or_default();
        if !v.contains(&holder) {
            v.push(holder);
        }
    }

    /// [`LeaseManager::claim_sweep`] under `source`'s gate (`guard`, from
    /// [`LeaseManager::gate_guard`]), so it never interleaves with an
    /// [`LeaseManager::idle_if_unheld`]: an idle under way either ends before the
    /// claim or sees it and leaves the port alone.
    pub(crate) fn claim_sweep_locked(
        &self,
        source: &SourceId,
        holder: JobId,
        guard: &OwnedMutexGuard<()>,
    ) {
        self.assert_gate(source, guard);
        self.claim_sweep(source, holder);
    }

    /// The jobs holding a sweep claim on `source`, in claim order.
    pub fn sweepers(&self, source: &SourceId) -> Vec<JobId> {
        self.sweeps
            .lock()
            .unwrap()
            .get(source)
            .cloned()
            .unwrap_or_default()
    }

    /// The holders waiting to take a time-shared lease on `source` (it gave way to a
    /// request, or the source was held when they started), in order: they hold nothing.
    pub fn waiting(&self, source: &SourceId) -> Vec<JobId> {
        self.waiting
            .lock()
            .unwrap()
            .get(source)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether a lease a request must respect holds `source`: a rigid one, a pin or a
    /// time-shared one that does not give way (never an elastic one, nor a time-shared
    /// one that gives way).
    pub fn held_standing(&self, source: &SourceId) -> bool {
        self.leases
            .lock()
            .unwrap()
            .get(source)
            .is_some_and(|v| v.iter().any(|l| !l.elastic && l.yields.is_none()))
    }

    /// Whether `holder` waits to take a time-shared lease on any source.
    pub fn is_waiting(&self, holder: &JobId) -> bool {
        self.waiting
            .lock()
            .unwrap()
            .values()
            .any(|v| v.contains(holder))
    }

    /// A receiver of `source`'s changes: the value is bumped by every yield and every
    /// release on the source, and the receiver has seen the value at the call, so a
    /// change after it is never missed.
    pub fn changes(&self, source: &SourceId) -> tokio::sync::watch::Receiver<u64> {
        self.changes
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_insert_with(|| tokio::sync::watch::channel(0).0)
            .subscribe()
    }

    /// Bump `source`'s changes.
    fn changed(&self, source: &SourceId) {
        self.changes
            .lock()
            .unwrap()
            .entry(source.clone())
            .or_insert_with(|| tokio::sync::watch::channel(0).0)
            .send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Take away the time-shared leases on `source` in `leases` (the manager's map,
    /// locked by the caller under the source's gate) that give way: each holder's
    /// token fired, each listed as waiting, and the source's changes bumped. A request
    /// does this once it is granted, before it tunes.
    fn yield_shared(&self, leases: &mut HashMap<SourceId, Vec<Lease>>, source: &SourceId) {
        let Some(v) = leases.get_mut(source) else {
            return;
        };
        let gone: Vec<Lease> = v.iter().filter(|l| l.yields.is_some()).cloned().collect();
        if gone.is_empty() {
            return;
        }
        v.retain(|l| l.yields.is_none());
        if v.is_empty() {
            leases.remove(source);
        }
        let mut waiting = self.waiting.lock().unwrap();
        let w = waiting.entry(source.clone()).or_default();
        for l in gone {
            if let Some(t) = &l.yields {
                t.cancel();
            }
            if !w.contains(&l.job) {
                w.push(l.job);
            }
        }
        drop(waiting);
        self.changed(source);
    }

    /// `holder` no longer waits on `source`.
    fn unwait(&self, source: &SourceId, holder: &JobId) {
        let mut waiting = self.waiting.lock().unwrap();
        if let Some(w) = waiting.get_mut(source) {
            w.retain(|j| j != holder);
            if w.is_empty() {
                waiting.remove(source);
            }
        }
    }

    /// Give `holder` (waiting, [`LeaseManager::waiting`]) a time-shared lease that
    /// gives way on the whole `span` of `port`, under the source's gate the caller
    /// holds: granted (the lease's token, fired when it gives way again, and `holder`
    /// no longer waiting) only when no rigid lease, no pin and no other time-shared
    /// lease holds the source (elastic leases never do: the caller stops them); else
    /// the conflict naming the holders, as [`LeaseManager::acquire_shared_locked`]
    /// words it, and `holder` still waiting. It never takes another lease away.
    pub(crate) fn resume_shared_locked(
        &self,
        port: &dyn Port,
        holder: JobId,
        span: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<CancellationToken, LeaseError> {
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
            let options = conflict_options(port, Some(&span), &cur, shared, &holders, pinned);
            return Err(LeaseError::Conflict {
                options,
                holders,
                source,
                current: cur,
            });
        }
        if v.is_empty() {
            leases.remove(&source);
        }
        let token = CancellationToken::new();
        leases.entry(source.clone()).or_default().push(Lease {
            job: holder.clone(),
            band: span,
            time_shared: true,
            elastic: false,
            pin: false,
            yields: Some(token.clone()),
        });
        drop(leases);
        self.unwait(&source, &holder);
        Ok(token)
    }

    /// List `holder` as waiting for a time-shared lease on `source` it was not
    /// granted (a scan started on a held source), under the source's gate the caller
    /// holds.
    pub(crate) fn wait_locked(
        &self,
        source: &SourceId,
        holder: JobId,
        guard: &OwnedMutexGuard<()>,
    ) {
        self.assert_gate(source, guard);
        let mut waiting = self.waiting.lock().unwrap();
        let w = waiting.entry(source.clone()).or_default();
        if !w.contains(&holder) {
            w.push(holder);
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
    /// lease (a scan) that does not give way, which no capture outlasts. A
    /// time-shared lease that gives way is never in the way: a request granted
    /// takes it away first.
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

    /// What a rigid request of `want` on `port` would get under the source's gate the
    /// caller holds (`guard`), without leasing anything or touching the port: `Fits`
    /// (inside the current capture, under rigid holders or elastic ones), `Tune` (only
    /// elastic leases or none hold the source and the band is not inside their
    /// capture) or `Conflict`. [`LeaseManager::ensure_locked`] takes the same decision
    /// for a rigid request, so a caller deciding several sources before tuning any
    /// (`set_start`) gets what the lease will then do, the gates held in between.
    pub(crate) fn decide(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Decision {
        self.assert_gate(port.id(), guard);
        let held = self
            .leases
            .lock()
            .unwrap()
            .get(port.id())
            .map(|v| standing(v))
            .unwrap_or_default();
        rigid_decision(port, &held, &want, false)
    }

    /// [`LeaseManager::ensure_locked`], the lease marked `pin` when asked (so it
    /// is never listed among the holders, not even for an instant). A rigid request
    /// is decided by [`LeaseManager::decide`]'s rule, then leased or tuned.
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
            // A time-shared lease that gives way is decided as if it were not there.
            let held = leases.get(&source).map(|v| standing(v)).unwrap_or_default();
            // An elastic request on a held port joins its capture untuned (a
            // held port lacking a current tuning is at its first holder's band),
            // unless a time-shared lease holds it.
            if elastic && let Some(first) = held.first() {
                let cur = port.current().unwrap_or(first.band);
                if held.iter().any(|l| l.time_shared) {
                    let holders = rigid(&held);
                    let pinned = pinned_in(&held, &source);
                    let options = conflict_options(port, Some(&want), &cur, true, &holders, pinned);
                    return Err(LeaseError::Conflict {
                        source,
                        holders,
                        current: cur,
                        options,
                    });
                }
                self.yield_shared(&mut leases, &source);
                let v = leases.entry(source.clone()).or_default();
                v.retain(|l| l.job != holder);
                v.push(lease(cur));
                return Ok((cur, false));
            }
            // A rigid request (a pin among them): the decision `decide` reports.
            if !elastic {
                match rigid_decision(port, &held, &want, pin) {
                    Decision::Fits(cur) => {
                        self.yield_shared(&mut leases, &source);
                        let v = leases.entry(source.clone()).or_default();
                        v.retain(|l| l.job != holder);
                        v.push(lease(want));
                        return Ok((cur, false));
                    }
                    Decision::Conflict {
                        holders,
                        current,
                        options,
                    } => {
                        return Err(LeaseError::Conflict {
                            source,
                            holders,
                            current,
                            options,
                        });
                    }
                    Decision::Tune => {}
                }
            }
            // Granted: what gives way goes before the tune.
            self.yield_shared(&mut leases, &source);
        }
        // A rigid job's tune of a free port takes the capture the source's policy
        // gives it; an elastic job's and a pin's are the band itself.
        let ask = match elastic || pin {
            true => want,
            false => self.capture_for(port, want),
        };
        let got = port.tune(ask).await?;
        let mut leases = self.leases.lock().unwrap();
        let v = leases.entry(source).or_default();
        v.retain(|l| l.job != holder);
        // A rigid lease's band is the one asked for (the capture may be wider, and
        // may later move around it: `widen_locked`); an elastic one's is the capture.
        v.push(lease(if elastic { got } else { want }));
        Ok((got, true))
    }

    /// Pin `port` at `want` (see [`pin_holder`]) under the source's gate the
    /// caller holds: any existing pin on the source is set aside, then the band
    /// is leased as a rigid request (`ensure_locked`), the lease marked `pin`,
    /// except that with no rigid job holding the source (only elastic leases, or
    /// none) the port is always tuned to the band, even one inside the current
    /// capture (R13; the caller relaunches the elastic jobs). Under rigid job
    /// holders a band inside their capture joins it untuned. On an error the old
    /// pin, if any, is put back. Also whether it tuned the port.
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

    /// Move `holder`'s lease on `port` to `want` under the source's gate the caller
    /// holds (a `job_retune`): the holder's own leases are set aside, then a band
    /// inside the current capture is leased without a tune whatever else holds the
    /// source (unless a time-shared lease does), so even a lone holder, whom
    /// [`LeaseManager::ensure_locked`] would tune, keeps the capture; any other band
    /// is leased as a rigid request (a tune when no other rigid lease or pin holds
    /// the source, else a conflict). On an error the set-aside leases are put back.
    /// Also whether it tuned the port.
    pub(crate) async fn retune_locked(
        &self,
        port: &dyn Port,
        holder: &JobId,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(Tuning, bool), LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let old: Vec<Lease> = {
            let mut leases = self.leases.lock().unwrap();
            let v = leases.entry(source.clone()).or_default();
            let old = v.iter().filter(|l| &l.job == holder).cloned().collect();
            v.retain(|l| &l.job != holder);
            // A time-shared lease that gives way leaves no capture to keep: the band
            // is leased as a rigid request, which takes it away.
            let shared = v.iter().any(|l| l.time_shared);
            if !shared && let Some(cur) = port.current().filter(|c| fits(&want, c)) {
                v.push(Lease::new(holder.clone(), want, false));
                return Ok((cur, false));
            }
            if v.is_empty() {
                leases.remove(&source);
            }
            old
        };
        let r = self
            .ensure_as(port, holder.clone(), want, false, false, guard)
            .await;
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
    /// capture outlasts a step); another time-shared lease that gives way is
    /// taken away, as by any request. With `yields` the new lease gives way too
    /// and its token is returned (fired when it does); without, while it is held
    /// every other `ensure` and `tune_if_unheld` is a conflict naming `holder`.
    pub async fn acquire_shared(
        &self,
        port: &dyn Port,
        holder: JobId,
        span: Tuning,
        yields: bool,
    ) -> Result<Option<CancellationToken>, LeaseError> {
        let guard = self.gate_guard(port.id()).await;
        self.acquire_shared_locked(port, holder, span, yields, &guard)
    }

    /// [`LeaseManager::acquire_shared`] under the source's gate the caller holds.
    pub(crate) fn acquire_shared_locked(
        &self,
        port: &dyn Port,
        holder: JobId,
        span: Tuning,
        yields: bool,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<Option<CancellationToken>, LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let mut leases = self.leases.lock().unwrap();
        let v = leases.entry(source.clone()).or_default();
        v.retain(|l| l.job != holder);
        let held = standing(v);
        if v.is_empty() {
            leases.remove(&source);
        }
        if let Some(first) = held.iter().find(|l| !l.elastic) {
            let cur = port.current().unwrap_or(first.band);
            let shared = held.iter().any(|l| l.time_shared);
            let holders = rigid(&held);
            let pinned = pinned_in(&held, &source);
            return Err(LeaseError::Conflict {
                options: conflict_options(port, Some(&span), &cur, shared, &holders, pinned),
                holders,
                source,
                current: cur,
            });
        }
        self.yield_shared(&mut leases, &source);
        let token = yields.then(CancellationToken::new);
        leases.entry(source.clone()).or_default().push(Lease {
            job: holder.clone(),
            band: span,
            time_shared: true,
            elastic: false,
            pin: false,
            yields: token.clone(),
        });
        drop(leases);
        self.unwait(&source, &holder);
        Ok(token)
    }

    /// `Ok` unless a rigid or time-shared lease or a pin holds `port`, other than
    /// those of the jobs `except` (the ones a caller will stop), under the source's
    /// gate the caller holds; elastic leases never hold it, nor does a time-shared
    /// lease that gives way (taken away when this is `Ok`). The conflict is built
    /// as [`LeaseManager::acquire_shared_locked`]'s, naming every rigid holder,
    /// with no band to choose or widen to: for a request that asks for no band
    /// (`source_measure`).
    pub(crate) fn unheld_locked(
        &self,
        port: &dyn Port,
        except: &[JobId],
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(), LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let mut leases = self.leases.lock().unwrap();
        let v = leases.get(&source).map(|v| standing(v)).unwrap_or_default();
        let Some(first) = v.iter().find(|l| !l.elastic && !except.contains(&l.job)) else {
            // Granted: what gives way goes now.
            self.yield_shared(&mut leases, &source);
            return Ok(());
        };
        let cur = port.current().unwrap_or(first.band);
        let shared = v.iter().any(|l| l.time_shared);
        let holders = rigid(&v);
        let pinned = pinned_in(&v, &source);
        Err(LeaseError::Conflict {
            options: conflict_options(port, None, &cur, shared, &holders, pinned),
            holders,
            source,
            current: cur,
        })
    }

    /// Tune `port` to `step` for the holder of a time-shared lease, under
    /// the source's gate. `None` (and no tune) once `holder` holds no
    /// time-shared lease on `port` (it gave way, or was released), or when
    /// `cancel` fires while it waits for the gate (whoever holds it may be
    /// stopping the scan); once the gate is taken the tune is never abandoned half
    /// way. A request that takes the lease away holds the gate while it does, so a
    /// step's tune is either made before the request's or not at all.
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

    /// Set how `source`'s capture is chosen when a rigid job tunes it free
    /// ([`LeaseManager::capture_for`]).
    pub fn set_capture_policy(&self, source: &SourceId, policy: CapturePolicy) {
        let mut widest = self.widest.lock().unwrap();
        match policy {
            CapturePolicy::Fit => widest.remove(source),
            CapturePolicy::Widest => widest.insert(source.clone()),
        };
    }

    /// `source`'s capture policy ([`CapturePolicy::Fit`] unless set).
    pub fn capture_policy(&self, source: &SourceId) -> CapturePolicy {
        match self.widest.lock().unwrap().contains(source) {
            true => CapturePolicy::Widest,
            false => CapturePolicy::Fit,
        }
    }

    /// The band a rigid job's tune of the free `port` asks for when the job wants
    /// `want`: `want` itself under [`CapturePolicy::Fit`] (the port grants the
    /// narrowest rate that holds it); under [`CapturePolicy::Widest`] the port's
    /// whole `max_bandwidth_hz` about `want`'s centre, so later jobs and a wider
    /// view fit beside it without a retune. On a port a rigid lease or a pin holds
    /// it is `want` whatever the policy.
    pub fn capture_for(&self, port: &dyn Port, want: Tuning) -> Tuning {
        // A source a rigid lease or a pin holds is not free: the request is decided
        // against its capture as it stands.
        let held = self
            .leases
            .lock()
            .unwrap()
            .get(port.id())
            .is_some_and(|v| v.iter().any(|l| !l.elastic && l.yields.is_none()));
        if held {
            return want;
        }
        let max = *port.capabilities().max_bandwidth_hz.value();
        match self.capture_policy(port.id()) {
            CapturePolicy::Widest if want.bandwidth_hz.0 < max.0 => Tuning {
                center_hz: want.center_hz,
                bandwidth_hz: max,
            },
            _ => want,
        }
    }

    /// Move `port`'s capture to `want` though rigid leases hold it, under the
    /// source's gate the caller holds: granted when every rigid lease and pin band
    /// lies inside `want` (the port is tuned; the caller restarts each holder on the
    /// new capture, their leases kept), else a conflict as
    /// [`LeaseManager::tune_if_unheld_locked`] gives, which also decides a port no
    /// rigid lease holds and a band already inside the capture. A time-shared lease
    /// that does not give way is a conflict; one that does gives way. Should the port
    /// grant a band that leaves a holder outside, it is tuned back and the request is
    /// a conflict. Also whether it tuned the port and, when it did, the rigid
    /// holders (never a pin).
    pub(crate) async fn widen_locked(
        &self,
        port: &dyn Port,
        want: Tuning,
        guard: &OwnedMutexGuard<()>,
    ) -> Result<(Tuning, bool, Vec<JobId>), LeaseError> {
        let source = port.id().clone();
        self.assert_gate(&source, guard);
        let held: Vec<Lease> = self
            .leases
            .lock()
            .unwrap()
            .get(&source)
            .map(|v| standing(v).into_iter().filter(|l| !l.elastic).collect())
            .unwrap_or_default();
        let cur = held.first().map(|l| port.current().unwrap_or(l.band));
        let movable = cur.is_some_and(|c| !fits(&want, &c))
            && !held.iter().any(|l| l.time_shared)
            && held.iter().all(|l| fits(&l.band, &want));
        let Some(cur) = cur.filter(|_| movable) else {
            return self
                .tune_if_unheld_locked(port, want, guard)
                .await
                .map(|(t, tuned)| (t, tuned, Vec::new()));
        };
        self.yield_shared(&mut self.leases.lock().unwrap(), &source);
        let got = port.tune(want).await?;
        if held.iter().all(|l| fits(&l.band, &got)) {
            return Ok((got, true, rigid(&held)));
        }
        // The port granted another band than asked: back to the capture the holders run on.
        port.tune(cur).await?;
        let holders = rigid(&held);
        let options = conflict_options(
            port,
            Some(&want),
            &cur,
            false,
            &holders,
            pinned_in(&held, &source),
        );
        Err(LeaseError::Conflict {
            source,
            holders,
            current: cur,
            options,
        })
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
            .map(|v| standing(v).into_iter().filter(|l| !l.elastic).collect())
            .unwrap_or_default();
        let Some(first) = held.first() else {
            // Granted: what gives way goes before the tune.
            self.yield_shared(&mut self.leases.lock().unwrap(), &source);
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
        let options = conflict_options(port, Some(&want), &cur, shared, &holders, pinned);
        Err(LeaseError::Conflict {
            source,
            holders,
            current: cur,
            options,
        })
    }

    /// Drop every lease, sweep claim and wait held by `holder`, and bump the changes
    /// of each source it held a lease on (a scan waiting there may take it).
    pub fn release(&self, holder: &JobId) {
        let mut leases = self.leases.lock().unwrap();
        let mut freed = Vec::new();
        leases.retain(|source, v| {
            let n = v.len();
            v.retain(|l| &l.job != holder);
            if v.len() != n {
                freed.push(source.clone());
            }
            !v.is_empty()
        });
        drop(leases);
        self.sweeps.lock().unwrap().retain(|_, v| {
            v.retain(|j| j != holder);
            !v.is_empty()
        });
        self.waiting.lock().unwrap().retain(|_, v| {
            v.retain(|j| j != holder);
            !v.is_empty()
        });
        for source in freed {
            self.changed(&source);
        }
    }

    /// Whether `source` is held by a time-shared lease (a scan; one that gives way
    /// included, while it holds).
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

    /// The ports of `ports`, in the given order, that could take the band now: each
    /// [`covers`] it and has no rigid holder, no fault, and no pin unless the pin's
    /// band holds the whole band (a pin admits jobs inside it, as `plan_start` inside a
    /// pin does). Elastic jobs never make a port busy, nor does a scan whose lease
    /// gives way. A suggestion, not a
    /// reservation: nothing is leased.
    pub fn free_sources(
        &self,
        ports: &[Arc<dyn Port>],
        center_hz: u64,
        bandwidth_hz: u64,
    ) -> Vec<SourceId> {
        let want = Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        };
        let leases = self.leases.lock().unwrap();
        ports
            .iter()
            .filter(|p| covers(p.capabilities(), center_hz, bandwidth_hz))
            .filter(|p| p.fault().is_none())
            .filter(|p| {
                leases.get(p.id()).is_none_or(|v| {
                    v.iter()
                        .all(|l| l.elastic || l.yields.is_some() || (l.pin && fits(&want, &l.band)))
                })
            })
            .map(|p| p.id().clone())
            .collect()
    }
}

/// Whether a port with `caps` can capture the band `center_hz` ± `bandwidth_hz`/2:
/// both edges inside `tune_min_hz..=tune_max_hz` and `bandwidth_hz` within
/// `max_bandwidth_hz`.
pub fn covers(caps: &Capabilities, center_hz: u64, bandwidth_hz: u64) -> bool {
    let half = bandwidth_hz / 2;
    let (Some(lo), Some(hi)) = (center_hz.checked_sub(half), center_hz.checked_add(half)) else {
        return false;
    };
    caps.tune_min_hz.value().0 <= lo
        && hi <= caps.tune_max_hz.value().0
        && bandwidth_hz <= caps.max_bandwidth_hz.value().0
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
        idles: std::sync::atomic::AtomicUsize,
        delay: bool,
        fault: Option<String>,
    }

    impl FakePort {
        fn named(id: &str) -> Self {
            Self {
                id: SourceId(id.into()),
                ..Self::new()
            }
        }

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
                    usable_fraction: Claim::Asserted(1.0),
                    depths: vec![],
                    sweep: None,
                },
                cur: Mutex::new(None),
                tx: tokio::sync::broadcast::channel(1).0,
                tunes: std::sync::atomic::AtomicUsize::new(0),
                idles: std::sync::atomic::AtomicUsize::new(0),
                delay: false,
                fault: None,
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
        fn fault(&self) -> Option<String> {
            self.fault.clone()
        }
        async fn idle(&self) {
            self.idles.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                    "widen the capture to 102500000/6000000 Hz (source_ensure with widen: the holders restart on it)".to_string(),
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
        m.acquire_shared(&p, scan.clone(), t(100_000_000, 40_000_000), false)
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
    async fn retune_locked_moves_a_lease_inside_the_capture_without_a_tune() {
        let (p, m, j1) = (FakePort::new(), LeaseManager::new(), JobId::new());
        let first = t(100_000_000, 1_000_000);
        m.ensure(&p, j1.clone(), first, false).await.unwrap();
        let want = t(100_200_000, 10_000);
        let guard = m.gate_guard(p.id()).await;
        assert_eq!(
            m.retune_locked(&p, &j1, want, &guard).await.unwrap(),
            (first, false)
        );
        assert_eq!(m.holders(p.id()), vec![(j1, want)]);
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retune_locked_tunes_outside_when_only_elastic_leases_remain_and_restores_on_a_conflict()
     {
        let (p, m, j1, e1, j2) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
            JobId::new(),
        );
        m.ensure(&p, j1.clone(), t(100_000_000, 1_000_000), false)
            .await
            .unwrap();
        m.ensure(&p, e1, t(100_000_000, 1_000_000), true)
            .await
            .unwrap();
        let far = t(433_000_000, 12_500);
        let guard = m.gate_guard(p.id()).await;
        assert_eq!(
            m.retune_locked(&p, &j1, far, &guard).await.unwrap(),
            (far, true)
        );
        assert_eq!(p.current(), Some(far));
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 2);
        // A second rigid job inside the new capture, granted untuned.
        let inside = t(433_001_000, 1_000);
        assert_eq!(
            m.ensure_locked(&p, j2.clone(), inside, false, &guard)
                .await
                .unwrap(),
            (far, false)
        );
        match m
            .retune_locked(&p, &j2, t(900_000_000, 1_000), &guard)
            .await
        {
            Err(LeaseError::Conflict {
                holders, current, ..
            }) => {
                assert_eq!(holders, vec![j1.clone()]);
                assert_eq!(current, far);
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        // j2's lease is put back.
        assert_eq!(m.holders(p.id()), vec![(j1, far), (j2, inside)]);
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 2);
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

    /// A sweep claim holds no band: every IQ request decides as on an unheld port,
    /// and the claim is listed only by `sweepers`.
    #[tokio::test]
    async fn a_sweep_claim_is_invisible_to_every_iq_request() {
        let (p, m, scan) = (Arc::new(FakePort::new()), LeaseManager::new(), JobId::new());
        m.claim_sweep(p.id(), scan.clone());
        // Idempotent: a second claim by the same holder is the same claim.
        m.claim_sweep(p.id(), scan.clone());
        assert_eq!(m.sweepers(p.id()), vec![scan.clone()]);
        assert!(m.holders(p.id()).is_empty());
        assert!(m.elastic_holders(p.id()).is_empty());
        assert!(!m.time_shared(p.id()));
        assert_eq!(m.pinned(p.id()), None);
        let ports: Vec<Arc<dyn Port>> = vec![p.clone()];
        assert_eq!(
            m.free_sources(&ports, 100_000_000, 1_000_000),
            vec![p.id().clone()]
        );
        {
            let guard = m.gate_guard(p.id()).await;
            assert!(m.unheld_locked(p.as_ref(), &[], &guard).is_ok());
            assert_eq!(
                m.decide(p.as_ref(), t(100_000_000, 1_000_000), &guard),
                Decision::Tune
            );
        }
        // A rigid request tunes the port as unheld.
        let (j, want) = (JobId::new(), t(100_000_000, 1_000_000));
        assert_eq!(
            m.ensure(p.as_ref(), j.clone(), want, false).await.unwrap(),
            want
        );
        assert_eq!(p.current(), Some(want));
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 1);
        m.release(&j);
        // So does a tune with no lease.
        let elsewhere = t(433_000_000, 1_000_000);
        assert_eq!(
            m.tune_if_unheld(p.as_ref(), elsewhere).await.unwrap(),
            elsewhere
        );
        // A stepped scan's time-shared lease is granted.
        let stepped = JobId::new();
        m.acquire_shared(
            p.as_ref(),
            stepped.clone(),
            t(100_000_000, 40_000_000),
            false,
        )
        .await
        .unwrap();
        assert!(m.time_shared(p.id()));
        assert_eq!(m.holders(p.id()).len(), 1);
        assert_eq!(m.sweepers(p.id()), vec![scan]);
    }

    /// A claim never refuses: it is taken beside a rigid lease and beside a
    /// time-shared one, and neither lease changes.
    #[tokio::test]
    async fn a_sweep_claim_is_granted_beside_a_rigid_and_a_time_shared_lease() {
        let m = LeaseManager::new();
        let (rigid_port, shared_port) = (FakePort::named("rigid"), FakePort::named("shared"));
        let (demod, stepped) = (JobId::new(), JobId::new());
        let band = t(100_000_000, 1_000_000);
        let span = t(100_000_000, 40_000_000);
        m.ensure(&rigid_port, demod.clone(), band, false)
            .await
            .unwrap();
        m.acquire_shared(&shared_port, stepped.clone(), span, false)
            .await
            .unwrap();
        let (a, b) = (JobId::new(), JobId::new());
        m.claim_sweep(rigid_port.id(), a.clone());
        m.claim_sweep(rigid_port.id(), b.clone());
        m.claim_sweep(shared_port.id(), a.clone());
        assert_eq!(m.sweepers(rigid_port.id()), vec![a.clone(), b]);
        assert_eq!(m.sweepers(shared_port.id()), vec![a]);
        assert_eq!(m.holders(rigid_port.id()), vec![(demod, band)]);
        assert_eq!(m.holders(shared_port.id()), vec![(stepped, span)]);
        assert!(m.time_shared(shared_port.id()));
        assert!(!m.time_shared(rigid_port.id()));
        assert_eq!(rigid_port.current(), Some(band));
        assert_eq!(
            rigid_port.tunes.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            shared_port.tunes.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    /// `release` drops the holder's claims on every source along with its leases,
    /// and nobody else's.
    #[tokio::test]
    async fn release_drops_a_holders_claim() {
        let m = LeaseManager::new();
        let (one, two) = (FakePort::named("one"), FakePort::named("two"));
        let (j, k) = (JobId::new(), JobId::new());
        m.claim_sweep(one.id(), j.clone());
        m.claim_sweep(two.id(), j.clone());
        m.claim_sweep(two.id(), k.clone());
        m.ensure(&one, j.clone(), t(100_000_000, 1_000_000), false)
            .await
            .unwrap();
        m.release(&j);
        assert!(m.sweepers(one.id()).is_empty());
        assert_eq!(m.sweepers(two.id()), vec![k.clone()]);
        assert!(m.holders(one.id()).is_empty());
        m.release(&k);
        assert!(m.sweepers(two.id()).is_empty());
        // A released holder may claim again.
        m.claim_sweep(one.id(), j.clone());
        assert_eq!(m.sweepers(one.id()), vec![j]);
    }

    /// The last lease's release idles the port only once no claim is held either:
    /// the source's own sweep may need what an idle stops upstream.
    #[tokio::test]
    async fn a_port_is_not_idled_under_a_sweep_claim() {
        let (p, m, scan, job) = (
            FakePort::new(),
            LeaseManager::new(),
            JobId::new(),
            JobId::new(),
        );
        let idles = || p.idles.load(std::sync::atomic::Ordering::SeqCst);
        m.claim_sweep(p.id(), scan.clone());
        m.ensure(&p, job.clone(), t(100_000_000, 1_000_000), false)
            .await
            .unwrap();
        m.release(&job);
        m.idle_if_unheld(&p).await;
        assert_eq!(idles(), 0);
        m.release(&scan);
        m.idle_if_unheld(&p).await;
        assert_eq!(idles(), 1);
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

    /// A pin admits jobs inside it: a source pinned over the whole band is free, one
    /// pinned beside it (or over part of it) is not, and a rigid holder inside a
    /// covering pin still makes it busy.
    #[tokio::test]
    async fn free_sources_takes_a_pin_that_covers_the_band() {
        let m = LeaseManager::new();
        let covering = Arc::new(FakePort::named("covering"));
        let beside = Arc::new(FakePort::named("beside"));
        let partial = Arc::new(FakePort::named("partial"));
        let held = Arc::new(FakePort::named("held"));
        m.pin(covering.as_ref(), t(130_925_000, 18_100_000))
            .await
            .unwrap();
        m.pin(beside.as_ref(), t(148_000_000, 8_000_000))
            .await
            .unwrap();
        m.pin(partial.as_ref(), t(132_000_000, 2_000_000))
            .await
            .unwrap();
        m.pin(held.as_ref(), t(130_925_000, 18_100_000))
            .await
            .unwrap();
        m.ensure(held.as_ref(), JobId::new(), t(131_550_000, 10_000), false)
            .await
            .unwrap();
        let ports: Vec<Arc<dyn Port>> = vec![covering, beside, partial, held];
        let ids = |v: Vec<SourceId>| v.into_iter().map(|s| s.0).collect::<Vec<_>>();
        assert_eq!(
            ids(m.free_sources(&ports, 133_503_750, 6_967_500)),
            ["covering"]
        );
    }

    #[tokio::test]
    async fn free_sources_skips_held_pinned_faulted_and_uncovering_ports() {
        let m = LeaseManager::new();
        let held = Arc::new(FakePort::named("held"));
        let pinned = Arc::new(FakePort::named("pinned"));
        let faulted = Arc::new(FakePort {
            fault: Some("gone".into()),
            ..FakePort::named("faulted")
        });
        let elastic = Arc::new(FakePort::named("elastic"));
        let idle = Arc::new(FakePort::named("idle"));
        let want = t(403_000_000, 20_000);
        m.ensure(held.as_ref(), JobId::new(), want, false)
            .await
            .unwrap();
        // Pinned beside the band: busy (a pin over the band is free, the test above).
        m.pin(pinned.as_ref(), t(401_000_000, 20_000))
            .await
            .unwrap();
        m.ensure(elastic.as_ref(), JobId::new(), want, true)
            .await
            .unwrap();
        let ports: Vec<Arc<dyn Port>> = vec![idle, held, pinned, faulted, elastic];
        let ids = |v: Vec<SourceId>| v.into_iter().map(|s| s.0).collect::<Vec<_>>();
        assert_eq!(
            ids(m.free_sources(&ports, 403_000_000, 20_000)),
            ["idle", "elastic"]
        );
        // Edges outside 1 MHz..6 GHz, or wider than 20 MHz: no port covers it.
        assert!(m.free_sources(&ports, 1_000_000, 20_000).is_empty());
        assert!(m.free_sources(&ports, 6_000_000_000, 20_000).is_empty());
        assert!(m.free_sources(&ports, 403_000_000, 20_000_002).is_empty());
        assert!(m.free_sources(&ports, u64::MAX, 4).is_empty());
        // Both edges on the range's ends, the band exactly the widest: covered.
        assert_eq!(
            ids(m.free_sources(&ports, 11_000_000, 20_000_000)),
            ["idle", "elastic"]
        );
    }

    /// The lease states `decide` and `ensure_locked` are compared in: the port at
    /// 100 MHz / 1 MHz, set directly (no tune), and the leases on it inserted
    /// directly, with the request inside or outside that capture.
    #[derive(Clone, Copy, Debug)]
    enum State {
        Unheld,
        ElasticInside,
        ElasticOutside,
        RigidInside,
        RigidOutside,
        ScanInside,
        YieldingScanInside,
    }

    const STATES: [State; 7] = [
        State::Unheld,
        State::ElasticInside,
        State::ElasticOutside,
        State::RigidInside,
        State::RigidOutside,
        State::ScanInside,
        State::YieldingScanInside,
    ];

    /// The port and manager in `state`, the holder its lease names (if any) and the
    /// band requested.
    fn in_state(state: State) -> (FakePort, LeaseManager, JobId, Tuning) {
        let (p, m, j) = (FakePort::new(), LeaseManager::new(), JobId::new());
        let cur = t(100_000_000, 1_000_000);
        *p.cur.lock().unwrap() = Some(cur);
        let lease = match state {
            State::Unheld => None,
            State::ElasticInside | State::ElasticOutside => Some(Lease::new(j.clone(), cur, true)),
            State::RigidInside | State::RigidOutside => Some(Lease::new(j.clone(), cur, false)),
            State::ScanInside => Some(Lease {
                time_shared: true,
                ..Lease::new(j.clone(), t(100_000_000, 40_000_000), false)
            }),
            State::YieldingScanInside => Some(Lease {
                time_shared: true,
                yields: Some(CancellationToken::new()),
                ..Lease::new(j.clone(), t(100_000_000, 40_000_000), false)
            }),
        };
        if let Some(l) = lease {
            m.leases
                .lock()
                .unwrap()
                .entry(p.id().clone())
                .or_default()
                .push(l);
        }
        let want = match state {
            State::ElasticOutside | State::RigidOutside => t(433_000_000, 100_000),
            _ => t(100_100_000, 100_000),
        };
        (p, m, j, want)
    }

    #[tokio::test]
    async fn decide_reports_fits_tune_or_conflict_without_touching_the_port() {
        let cur = t(100_000_000, 1_000_000);
        for state in STATES {
            let (p, m, j, want) = in_state(state);
            let guard = m.gate_guard(p.id()).await;
            let d = m.decide(&p, want, &guard);
            match state {
                // Nothing holds the source (a scan that gives way is as if not there):
                // a rigid start tunes to its own band.
                State::Unheld | State::ElasticOutside | State::YieldingScanInside => {
                    assert_eq!(d, Decision::Tune, "{state:?}")
                }
                State::ElasticInside | State::RigidInside => {
                    assert_eq!(d, Decision::Fits(cur), "{state:?}")
                }
                State::RigidOutside => match d {
                    Decision::Conflict {
                        holders,
                        current,
                        options,
                    } => {
                        assert_eq!(holders, vec![j.clone()]);
                        assert_eq!(current, cur);
                        assert!(options.contains(&format!("retry with replace: [{}]", j.0)));
                    }
                    other => panic!("expected conflict, got {other:?}"),
                },
                // A scan that does not give way (a lossy depth): its band is no capture
                // to fit in, and only stopping it frees the source.
                State::ScanInside => match d {
                    Decision::Conflict { options, .. } => assert_eq!(
                        options,
                        vec!["stop the scan holding the source".to_string()]
                    ),
                    other => panic!("expected conflict, got {other:?}"),
                },
            }
            assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(p.current(), Some(cur), "{state:?}");
        }
    }

    /// A time-shared lease that gives way is taken away by a request granted, under the
    /// gate: its token fires, its holder waits, the source's changes move, and its next
    /// step tunes nothing; it is given back only once nothing rigid holds the source.
    /// Every kind of request takes it: rigid, elastic, a pin, a tune without a lease, a
    /// request for no band, another scan's start.
    #[tokio::test]
    async fn a_lease_that_gives_way_is_taken_by_any_request_and_given_back_when_free() {
        let (p, m) = (FakePort::new(), LeaseManager::new());
        let scan = JobId::new();
        let span = t(100_000_000, 40_000_000);
        let token = m
            .acquire_shared(&p, scan.clone(), span, true)
            .await
            .unwrap()
            .expect("a token");
        let changes = m.changes(p.id());
        let job = JobId::new();
        let want = t(433_000_000, 1_000_000);
        assert_eq!(m.ensure(&p, job.clone(), want, false).await.unwrap(), want);
        assert!(token.is_cancelled());
        assert!(changes.has_changed().unwrap());
        assert_eq!(m.holders(p.id()), vec![(job.clone(), want)]);
        assert_eq!(m.waiting(p.id()), vec![scan.clone()]);
        assert!(m.is_waiting(&scan));
        let n = p.tunes.load(std::sync::atomic::Ordering::SeqCst);
        let step = m
            .retune_shared(
                &p,
                &scan,
                t(90_000_000, 10_000_000),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(step, None);
        assert_eq!(p.tunes.load(std::sync::atomic::Ordering::SeqCst), n);
        assert_eq!(p.current(), Some(want));
        // Not given back while the job holds the source; nor does the try take anything.
        let g = m.gate_guard(p.id()).await;
        match m.resume_shared_locked(&p, scan.clone(), span, &g) {
            Err(LeaseError::Conflict { holders, .. }) => assert_eq!(holders, vec![job.clone()]),
            other => panic!("expected a conflict, got {other:?}"),
        }
        drop(g);
        assert_eq!(m.holders(p.id()), vec![(job.clone(), want)]);
        let changes = m.changes(p.id());
        m.release(&job);
        assert!(changes.has_changed().unwrap(), "a release is a change");
        let g = m.gate_guard(p.id()).await;
        let again = m.resume_shared_locked(&p, scan.clone(), span, &g).unwrap();
        drop(g);
        assert!(!again.is_cancelled());
        assert!(m.waiting(p.id()).is_empty());
        assert_eq!(m.holders(p.id()), vec![(scan.clone(), span)]);

        // Each other kind of request takes it the same way.
        let take = |token: CancellationToken, what: &str| {
            assert!(token.is_cancelled(), "{what} left the scan holding");
            assert_eq!(m.waiting(p.id()), vec![scan.clone()], "{what}");
        };
        let resume = || async {
            let g = m.gate_guard(p.id()).await;
            m.resume_shared_locked(&p, scan.clone(), span, &g).unwrap()
        };
        let other = JobId::new();
        m.ensure(&p, other.clone(), want, true).await.unwrap();
        take(again, "an elastic request");
        m.release(&other);
        let tok = resume().await;
        m.pin(&p, want).await.unwrap();
        take(tok, "a pin");
        m.unpin(p.id());
        let tok = resume().await;
        m.tune_if_unheld(&p, want).await.unwrap();
        take(tok, "a tune without a lease");
        let tok = resume().await;
        let g = m.gate_guard(p.id()).await;
        m.unheld_locked(&p, &[], &g).unwrap();
        drop(g);
        take(tok, "a request for no band");
        let tok = resume().await;
        let second = JobId::new();
        m.acquire_shared(&p, second.clone(), span, true)
            .await
            .unwrap();
        take(tok, "another scan's start");
        // And a waiting scan never takes another's lease away.
        let g = m.gate_guard(p.id()).await;
        assert!(m.resume_shared_locked(&p, scan.clone(), span, &g).is_err());
        drop(g);
        assert_eq!(m.holders(p.id()), vec![(second, span)]);
    }

    /// A time-shared lease that does not give way (a scan at another depth) is a
    /// conflict naming it for every request, as before.
    #[tokio::test]
    async fn a_lease_that_does_not_give_way_is_a_conflict() {
        let (p, m) = (FakePort::new(), LeaseManager::new());
        let scan = JobId::new();
        let span = t(100_000_000, 40_000_000);
        assert!(
            m.acquire_shared(&p, scan.clone(), span, false)
                .await
                .unwrap()
                .is_none()
        );
        let want = t(433_000_000, 1_000_000);
        for elastic in [false, true] {
            match m.ensure(&p, JobId::new(), want, elastic).await {
                Err(LeaseError::Conflict {
                    holders, options, ..
                }) => {
                    assert_eq!(holders, vec![scan.clone()]);
                    assert_eq!(
                        options,
                        vec!["stop the scan holding the source".to_string()]
                    );
                }
                other => panic!("expected a conflict, got {other:?}"),
            }
        }
        assert!(
            m.acquire_shared(&p, JobId::new(), span, true)
                .await
                .is_err()
        );
        assert!(m.waiting(p.id()).is_empty());
        assert_eq!(m.holders(p.id()), vec![(scan, span)]);
    }

    /// A step's tune under way when a request takes the lease away lands before the
    /// request's: the request waits on the gate the step holds, so the port ends at
    /// the request's band whichever goes first.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_step_in_flight_tunes_before_the_request_that_takes_its_lease() {
        let p = Arc::new(FakePort {
            delay: true,
            ..FakePort::new()
        });
        let m = Arc::new(LeaseManager::new());
        let scan = JobId::new();
        let span = t(100_000_000, 40_000_000);
        m.acquire_shared(p.as_ref(), scan.clone(), span, true)
            .await
            .unwrap();
        let (pp, mm, ss) = (p.clone(), m.clone(), scan.clone());
        let step = tokio::spawn(async move {
            mm.retune_shared(
                pp.as_ref(),
                &ss,
                t(90_000_000, 10_000_000),
                &CancellationToken::new(),
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let want = t(433_000_000, 1_000_000);
        let got = m
            .ensure(p.as_ref(), JobId::new(), want, false)
            .await
            .unwrap();
        assert_eq!(got, want);
        step.await.unwrap().unwrap();
        assert_eq!(
            p.current(),
            Some(want),
            "the step's tune landed after the request's"
        );
        assert_eq!(m.waiting(p.id()), vec![scan]);
    }

    #[tokio::test]
    async fn ensure_as_and_decide_agree() {
        for state in STATES {
            let (p, m, _, want) = in_state(state);
            let guard = m.gate_guard(p.id()).await;
            let d = m.decide(&p, want, &guard);
            let r = m.ensure_locked(&p, JobId::new(), want, false, &guard).await;
            match (&d, &r) {
                (Decision::Fits(cur), Ok((got, false))) => assert_eq!(cur, got, "{state:?}"),
                (Decision::Tune, Ok((got, true))) => assert_eq!(*got, want, "{state:?}"),
                (
                    Decision::Conflict {
                        holders,
                        current,
                        options,
                    },
                    Err(LeaseError::Conflict {
                        holders: h,
                        current: c,
                        options: o,
                        ..
                    }),
                ) => assert_eq!((holders, current, options), (h, c, o), "{state:?}"),
                _ => panic!("{state:?}: decide {d:?} but ensure {r:?}"),
            }
        }
    }
}
