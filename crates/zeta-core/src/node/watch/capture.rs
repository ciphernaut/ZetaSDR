//! The watch's explore policy (design 5.5): with `on_unknown: capture` on its `detect`
//! node, the first time a signal's identify run ends unknown the watch starts an owned
//! `capture` child (`channel → burst_capture`) on the signal's band, the `capture` block's
//! params on its `burst_capture` node, and publishes `capturing` (`signal`, `job`); when
//! the child ends, `captured` (`signal`, `captures`, `summary`), the ids also listed under
//! the signal's `captures` in `job_context.watch.signals`. A signal is captured once per
//! run (one that goes and comes back included); its retries identify only. The child
//! inherits the watch's unknown verdict (it classifies nothing at its end), runs beside
//! the identify runs inside the watch's capture, and is stopped with the watch.

use std::sync::Arc;

use super::{Event, Inner, Run, Signal, State};
use crate::job::{JOB_RECORD_FORMAT, JobRecord, JobState};
use crate::node::persist::now;
use crate::ops::{BURST_CAPTURE_STAGE, JobKind};
use crate::port::Tuning;
use crate::types::{Hz, JobId};

/// The narrowest band a capture child takes, Hz: one narrowband channel.
pub const CAPTURE_MIN_BANDWIDTH_HZ: u64 = 12_500;
/// A capture child's band over the signal's measured width, as a ratio `NUM / DEN`
/// (1.25): the skirts the detector's occupied width leaves out.
const CAPTURE_WIDTH_NUM: u64 = 5;
const CAPTURE_WIDTH_DEN: u64 = 4;

/// The band a capture of `s` takes: its centre, its width × 1.25, at least
/// [`CAPTURE_MIN_BANDWIDTH_HZ`].
fn band(s: &Signal) -> Tuning {
    let width = (s.bandwidth_hz * CAPTURE_WIDTH_NUM).div_ceil(CAPTURE_WIDTH_DEN);
    Tuning {
        center_hz: Hz(s.center_hz),
        bandwidth_hz: Hz(width.max(CAPTURE_MIN_BANDWIDTH_HZ)),
    }
}

/// The signal `w.signals[i]`'s identify run has just ended unknown: start its capture
/// when the policy asks for one and this run has not captured it yet.
pub(super) async fn on_unknown(inner: &Arc<Inner>, r: &Run, w: &mut State, i: usize) {
    let Some(params) = r.seed.policy.capture.clone() else {
        return;
    };
    if w.signals[i].captured {
        return;
    }
    w.signals[i].captured = true;
    let signal = w.signals[i].signal.clone();
    let allocations = w.signals[i]
        .allocations
        .clone()
        .filter(|a| a.is_array())
        .unwrap_or_else(|| serde_json::json!([]));
    w.captured.push((signal.clone(), Vec::new()));
    match start(inner, r, w, &signal, params, allocations).await {
        Ok(None) => {}
        Ok(Some(job)) => {
            let summary = format!("capturing bursts at {}", signal.mhz());
            let e = Event {
                signal: Some(&signal),
                job: Some(&job),
                ..Event::default()
            };
            inner.watch_event(r, "capturing", summary, e);
        }
        Err(why) => {
            let summary = format!(
                "captured nothing at {}: the capture did not start: {why}",
                signal.mhz()
            );
            let e = Event {
                signal: Some(&signal),
                captures: Some(&[]),
                ..Event::default()
            };
            inner.watch_event(r, "captured", summary, e);
        }
    }
}

/// Start the owned capture child on `signal`'s band with the `burst_capture` params
/// `params` plus the signal, the watch and `allocations`, listed in `capturing` first.
/// Cancellable as identify's launches. `Ok(None)`: cancelled.
async fn start(
    inner: &Arc<Inner>,
    r: &Run,
    w: &mut State,
    signal: &Signal,
    params: serde_json::Value,
    allocations: serde_json::Value,
) -> Result<Option<JobId>, String> {
    let tuning = band(signal);
    let mut graph = JobKind::Capture
        .expand(
            &r.prov.source,
            tuning,
            r.tuning.bandwidth_hz,
            None,
            None,
            None,
            &inner.modes,
        )
        .map_err(|e| e.to_string())?;
    let node = graph
        .nodes
        .iter_mut()
        .find(|n| n.kind == BURST_CAPTURE_STAGE)
        .ok_or("the capture graph has no burst_capture node")?;
    let mut p = match params {
        p @ serde_json::Value::Object(_) => p,
        _ => serde_json::json!({}),
    };
    p["bandwidth_hz"] = node.params["bandwidth_hz"].clone();
    p["signal"] = serde_json::to_value(signal).map_err(|e| e.to_string())?;
    p["watch"] = r.id.0.clone().into();
    p["allocations"] = allocations;
    node.params = p;
    let rec = JobRecord {
        format: JOB_RECORD_FORMAT,
        id: JobId::new(),
        kind: JobKind::Capture.name().to_string(),
        graph,
        source: r.prov.source.clone(),
        tuning,
        state: JobState::Running,
        context: serde_json::json!({}),
        created_ns: now(),
        input: None,
        capture_rate_hz: None,
        elastic: false,
        operator_graph: false,
        owner: Some(r.id.clone()),
    };
    let id = rec.id.clone();
    w.capturing.push((id.clone(), signal.clone()));
    let unlist = |w: &mut State| w.capturing.retain(|(j, _)| *j != id);
    let mut launch = inner.launch_task(rec);
    tokio::select! {
        res = &mut launch => {
            let res = match res {
                Ok(Ok(())) => Ok(Some(id.clone())),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("launch task: {e}")),
            };
            if res.is_err() {
                unlist(w);
            }
            res
        }
        _ = r.cancel.cancelled() => {
            if inner.gated(&r.id) {
                let (inner, id) = (inner.clone(), id.clone());
                tokio::spawn(async move {
                    if matches!(launch.await, Ok(Ok(()))) {
                        let _ = inner.stop(&id).await;
                    }
                });
            } else if matches!(launch.await, Ok(Ok(()))) {
                let _ = inner.stop_gated(&id).await;
            }
            unlist(w);
            Ok(None)
        }
    }
}

/// Publish `captured` for every capture child that has ended, its ids listed under its
/// signal. Whether the state changed.
pub(super) fn ended(inner: &Arc<Inner>, r: &Run, w: &mut State) -> bool {
    let mut changed = false;
    for (job, signal) in std::mem::take(&mut w.capturing) {
        if inner.runs(&job) {
            w.capturing.push((job, signal));
            continue;
        }
        changed = true;
        let end = inner.captured.lock().unwrap().remove(&job);
        let end = end.unwrap_or(serde_json::Value::Null);
        let ids: Vec<String> = end["capture"]["written"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if let Some(done) = w
            .captured
            .iter_mut()
            .find(|(s, _)| s.overlaps(signal.center_hz, signal.bandwidth_hz))
        {
            done.1 = ids.clone();
        }
        if let Some(t) = w
            .signals
            .iter_mut()
            .find(|t| t.signal.overlaps(signal.center_hz, signal.bandwidth_hz))
        {
            t.captures = ids.clone();
        }
        let n = ids.len();
        let mut summary = format!(
            "captured {n} burst{} at {}",
            if n == 1 { "" } else { "s" },
            signal.mhz()
        );
        if end["capture"]["timed_out"] == true {
            summary.push_str(" (timed out)");
        }
        if let Some(why) = end["failed"].as_str() {
            summary.push_str(&format!("; the capture failed: {why}"));
        } else if end.is_null() {
            summary.push_str("; the capture ended without a record");
        }
        let e = Event {
            signal: Some(&signal),
            job: Some(&job),
            captures: Some(&ids),
            ..Event::default()
        };
        inner.watch_event(r, "captured", summary, e);
    }
    changed
}

/// The signal just added last to `w.signals`: when this run captured it before (it went
/// and came back), mark it captured with what that capture wrote.
pub(super) fn seen_before(w: &mut State) {
    let Some(t) = w.signals.last_mut() else {
        return;
    };
    if let Some((_, ids)) = w
        .captured
        .iter()
        .find(|(s, _)| s.overlaps(t.signal.center_hz, t.signal.bandwidth_hz))
    {
        t.captured = true;
        t.captures = ids.clone();
    }
}
