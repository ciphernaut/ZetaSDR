//! The end of a `capture` job (design 5.5). A hand-started capture runs the quick
//! classifiers over its first window and records the result under
//! `job_context.capture.classified` (`{labels, tried, allocations}`, or `{error}`); a
//! watch's capture child inherits the watch's `unknown` verdict instead and is reported
//! to its watch, which publishes `captured`.

use std::sync::Arc;

use super::Inner;
use crate::job::JobRecord;
use crate::ops::classify_capture;

/// Called by the launch's supervisor when the `capture` job `rec` (as it stands, its
/// context included) has ended, before its end is recorded: `failed` is the reason of a
/// failed run, `ended` whether it ended on its own (its stage wrote its windows or timed
/// out), not by a stop. An owned run's `job_context.capture` and `failed` are handed to
/// its watch (the owned record goes as soon as it ends); an unowned run that ended on its
/// own and wrote a window classifies that first window.
pub(super) async fn capture_finished(
    inner: &Arc<Inner>,
    rec: &JobRecord,
    failed: Option<&str>,
    ended: bool,
) {
    let capture = rec.context["capture"].clone();
    if rec.owner.is_some() {
        inner.captured.lock().unwrap().insert(
            rec.id.clone(),
            serde_json::json!({"capture": capture, "failed": failed}),
        );
        return;
    }
    if !ended || failed.is_some() {
        return;
    }
    let Some(first) = capture["written"][0].as_str() else {
        return;
    };
    let Some(window) = inner.bursts.get(first) else {
        return;
    };
    let set = inner.classifiers.read().unwrap().clone();
    let classified = match classify_capture(&set, &window).await {
        Ok(c) => serde_json::to_value(c).unwrap_or_default(),
        Err(e) => serde_json::json!({"error": e}),
    };
    let mut capture = capture;
    capture["classified"] = classified;
    let _ = inner.context_merge(&rec.id, serde_json::json!({"capture": capture}));
}
