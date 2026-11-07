//! The end of a `capture` job (design 5.5). A hand-started capture runs the quick
//! classifiers over its first window and records the result under
//! `job_context.capture.classified` (`{labels, tried, allocations}`, or `{error}`); a
//! watch's capture child inherits the watch's `unknown` verdict instead and is reported
//! to its watch, which publishes `captured`. Either, when it ended on its own with a
//! window written, may propose a features-only signature (see
//! [`super::propose::capture_ended`]), noted under `job_context.capture.proposal`.

use std::sync::Arc;

use super::Inner;
use super::propose::capture_ended;
use crate::job::JobRecord;
use crate::ops::classify_capture;

/// Called by the launch's supervisor when the `capture` job `rec` (as it stands, its
/// context included) has ended, before its end is recorded: `failed` is the reason of a
/// failed run, `ended` whether it ended on its own (its stage wrote its windows or timed
/// out), not by a stop. An owned run that ended on its own proposes, then its
/// `job_context.capture` and `failed` are handed to its watch (the owned record goes as
/// soon as it ends); an unowned run that ended on its own and wrote a window classifies
/// that first window, then proposes.
pub(super) async fn capture_finished(
    inner: &Arc<Inner>,
    rec: &JobRecord,
    failed: Option<&str>,
    ended: bool,
) {
    let on_its_own = ended && failed.is_none();
    if rec.owner.is_some() {
        if on_its_own {
            capture_ended(inner, rec, None).await;
        }
        let capture = inner.jobs.lock().unwrap().get(&rec.id).map_or_else(
            || rec.context["capture"].clone(),
            |e| e.record.context["capture"].clone(),
        );
        inner.captured.lock().unwrap().insert(
            rec.id.clone(),
            serde_json::json!({"capture": capture, "failed": failed}),
        );
        return;
    }
    if !on_its_own {
        return;
    }
    let mut capture = rec.context["capture"].clone();
    let Some(first) = capture["written"][0].as_str() else {
        return;
    };
    let Some(window) = inner.bursts.get(first) else {
        return;
    };
    let set = inner.classifiers.read().unwrap().clone();
    let classified = classify_capture(&set, &window).await;
    capture["classified"] = match &classified {
        Ok(c) => serde_json::to_value(c).unwrap_or_default(),
        Err(e) => serde_json::json!({"error": e}),
    };
    let _ = inner.context_merge(&rec.id, serde_json::json!({"capture": capture}));
    capture_ended(inner, rec, classified.as_ref().ok()).await;
}
