//! `zeta modes check [ID] [--strict]`: run each mode row's `test` on a node over a
//! synthesised (or fixture) capture and print one line per row. Local only: it opens its
//! own nodes, one per row, in a scratch directory under the state directory.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use zeta_core::capture::{CaptureMeta, write_cf32};
use zeta_core::job::JobState;
use zeta_core::modes::{Decoder, LoadedMode, ModeRegistry, ModeState, RowTest, TestSpec};
use zeta_core::ops::{JobKind, Op, OpResult};
use zeta_core::types::{Hz, SourceId, StreamKind, Timestamp};

use crate::node_setup;
use crate::sources::{PluginSpec, SourceSpec};

/// The source id every check's node attaches the capture as.
const SOURCE: &str = "check";

/// The centre a synthesised capture is labelled with when the row names none (RDS: any
/// broadcast station).
const DEFAULT_CENTER_HZ: u64 = 100_000_000;

/// Longest the job's event stream may take to appear after the job starts.
const STREAM_WAIT: Duration = Duration::from_secs(2);

/// The longest one `stream_tail` waits (`TAIL_MAX_TIMEOUT_MS`).
const TAIL_MAX_MS: u64 = 10_000;

/// What every row's check shares: the rows and stage registry one loading produced
/// (plugins probed once), cloned into each row's node.
struct Ctx {
    state_dir: PathBuf,
    registry: zeta_core::stage::StageRegistry,
    modes: Arc<ModeRegistry>,
    modes_dir: Option<PathBuf>,
}

/// How one row's check ended.
enum Outcome {
    Ok,
    Fail(String),
    Skipped(String),
}

/// Check the rows (only `only`, when given) and print `<id>: ok|fail|skipped: <reason>`
/// for each. Exit 0 iff no row failed; with `strict` a program row whose program is
/// missing fails instead of being skipped. A pass of a program row is recorded in
/// `modes-checked.json` in `state_dir`.
pub async fn run(
    state_dir: PathBuf,
    modes_dir: Option<PathBuf>,
    plugins: &[PluginSpec],
    only: Option<String>,
    strict: bool,
) -> ExitCode {
    let (registry, modes, modes_dir) =
        match node_setup::load_modes(&state_dir, plugins, None, modes_dir).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("zeta: {e}");
                return ExitCode::FAILURE;
            }
        };
    if let Some(id) = &only
        && modes.get(id).is_none()
    {
        eprintln!(
            "zeta: unknown mode {id} (known: {})",
            modes.ids().join(", ")
        );
        return ExitCode::FAILURE;
    }
    let ctx = Ctx {
        state_dir,
        registry,
        modes: modes.clone(),
        modes_dir,
    };
    let state_dir = &ctx.state_dir;
    let mut failed = false;
    for m in modes.all() {
        let id = &m.row.id.0;
        if only.as_ref().is_some_and(|o| o != id) {
            continue;
        }
        let outcome = check_row(m, &ctx, strict).await;
        let line = match &outcome {
            Outcome::Ok => format!("{id}: ok"),
            Outcome::Fail(why) => format!("{id}: fail: {why}"),
            Outcome::Skipped(why) => format!("{id}: skipped: {why}"),
        };
        println!("{line}");
        match outcome {
            Outcome::Ok => {
                if matches!(m.row.decoder, Decoder::Program(_)) {
                    let at = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let recorded = std::fs::create_dir_all(state_dir).and_then(|()| {
                        ModeRegistry::record_checked(&node_setup::checked_path(state_dir), id, at)
                    });
                    if let Err(e) = recorded {
                        eprintln!("zeta: modes-checked.json: {e}");
                    }
                }
            }
            Outcome::Fail(_) => failed = true,
            Outcome::Skipped(_) => {}
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One row's outcome: refused or skipped before any node is opened, else run.
async fn check_row(m: &LoadedMode, ctx: &Ctx, strict: bool) -> Outcome {
    if m.state == ModeState::Broken {
        return Outcome::Fail(m.reason.clone().unwrap_or_default());
    }
    let Some(test) = &m.row.test else {
        return Outcome::Skipped("no test".into());
    };
    if m.state == ModeState::Unavailable
        && let Decoder::Program(p) = &m.row.decoder
    {
        let handler = p.programs.first().map_or("program", String::as_str);
        let why = format!("{handler} missing");
        return if strict {
            Outcome::Fail(why)
        } else {
            Outcome::Skipped(why)
        };
    }
    let scratch = ctx
        .state_dir
        .join(format!("check-{}-{}", std::process::id(), m.row.id.0));
    let outcome = match std::fs::create_dir_all(&scratch) {
        Ok(()) => run_test(m, test, &scratch, ctx).await,
        Err(e) => Outcome::Fail(format!("{}: {e}", scratch.display())),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    outcome
}

/// Run `test` of row `m` on a node over its capture, with `scratch` as the node's state
/// directory parent.
async fn run_test(m: &LoadedMode, test: &RowTest, scratch: &Path, ctx: &Ctx) -> Outcome {
    let (capture, center_hz) = match capture(m, test, scratch) {
        Ok(c) => c,
        Err(why) => return Outcome::Fail(why),
    };
    let source = SourceSpec::File {
        id: SOURCE.into(),
        path: capture,
        tune_range: None,
    };
    let opened = node_setup::open_with(
        scratch.join("state"),
        ctx.registry.clone(),
        ctx.modes.clone(),
        ctx.modes_dir.clone(),
        &[source],
        false,
        None,
    )
    .await;
    let node = match opened {
        Ok((node, _remotes)) => node,
        Err(e) => return Outcome::Fail(e),
    };
    let start = Op::JobStart {
        kind: JobKind::from_name(&m.row.id.0).expect("a row id is a job kind name"),
        source: SourceId(SOURCE.into()),
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(m.row.band.bandwidth_hz),
        deviation_hz: None,
        sideband: None,
        elastic: false,
        replace: vec![],
    };
    let job = match node.apply(start).await {
        Ok(OpResult::Job(id)) => id,
        Ok(other) => return Outcome::Fail(format!("job_start answered {other:?}")),
        Err(e) => return Outcome::Fail(serde_json::to_string(&e).unwrap_or_default()),
    };
    let outcome = first_event(&node, &job, test.within_ms).await;
    let _ = node.apply(Op::JobStop { job }).await;
    match outcome {
        Ok(event) => compare(&test.expect, &event),
        Err(why) => Outcome::Fail(why),
    }
}

/// The SigMF pair the test runs on, as its meta path, and the job's centre: a fixture
/// as it is (centre from its meta), a synthesised capture written into `scratch`
/// (labelled with the row's first centre, the signal `offset_hz` above it).
fn capture(m: &LoadedMode, test: &RowTest, scratch: &Path) -> Result<(PathBuf, u64), String> {
    match &test.kind {
        TestSpec::Fixture { path } => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            let meta: Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
            let center = meta["captures"][0]["core:frequency"]
                .as_f64()
                .ok_or_else(|| format!("{path}: no captures[0].core:frequency"))?;
            Ok((PathBuf::from(path), center as u64))
        }
        TestSpec::Synth(params) => {
            let (spec, imp) = zeta_dsp::synth::parse_params(params.clone())?;
            let (rate, center) = (
                spec.rate_hz(),
                m.row
                    .band
                    .centers_hz
                    .first()
                    .copied()
                    .unwrap_or(DEFAULT_CENTER_HZ),
            );
            // The generators assert on what `parse_params` does not catch: a panic is a
            // failed row, not a dead check.
            let iq = catch_unwind(AssertUnwindSafe(|| {
                zeta_dsp::synth::synthesise(&spec, &imp, rate, center)
            }))
            .map_err(|p| {
                let why = p
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| p.downcast_ref::<&str>().copied())
                    .unwrap_or("panic");
                format!("synthesis failed: {why}")
            })?;
            let meta = CaptureMeta {
                sample_rate_hz: Hz(u64::from(rate)),
                center_hz: Hz(center),
                start: Timestamp(0),
                annotations: Vec::new(),
            };
            let path = write_cf32(scratch, "capture", &meta, &iq)
                .map_err(|e| format!("{}: {e}", scratch.display()))?;
            let at = (center as f64 + imp.offset_hz).round() as u64;
            Ok((path, at))
        }
    }
}

/// The payload of the first event on the job's first event stream within `within_ms`
/// (of the stream appearing), or why there is none: the job failed, or the time passed.
async fn first_event(
    node: &zeta_core::node::Node,
    job: &zeta_core::types::JobId,
    within_ms: u64,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + STREAM_WAIT;
    let stream = loop {
        let mut mine: Vec<_> = node
            .bus()
            .list()
            .into_iter()
            .filter(|s| s.provenance.job.as_ref() == Some(job) && s.kind == StreamKind::Event)
            .collect();
        mine.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        if let Some(s) = mine.into_iter().next() {
            break s.id;
        }
        if let Some(why) = failure(node, job) {
            return Err(why);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("the job has no event stream".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let tail = Op::StreamTail {
        stream,
        max_frames: 1,
        timeout_ms: within_ms.clamp(1, TAIL_MAX_MS) as u32,
    };
    match node.apply(tail).await {
        Ok(OpResult::Frames { frames, .. }) => match frames.into_iter().next() {
            Some(f) => Ok(f.payload),
            None => Err(failure(node, job).unwrap_or(format!("no event within {within_ms} ms"))),
        },
        Ok(other) => Err(format!("stream_tail answered {other:?}")),
        Err(e) => Err(serde_json::to_string(&e).unwrap_or_default()),
    }
}

/// Why the job is no longer running, `None` while it runs.
fn failure(node: &zeta_core::node::Node, job: &zeta_core::types::JobId) -> Option<String> {
    let rec = node.job_list().into_iter().find(|r| &r.id == job)?;
    match rec.state {
        JobState::Running | JobState::Created => None,
        JobState::Failed { reason } => Some(format!("job failed: {reason}")),
        other => Some(format!(
            "job {}",
            serde_json::to_string(&other).unwrap_or_default()
        )),
    }
}

/// `expect` (an object) as a subset of `event`: each expected key must equal the event's.
/// `t_ns` and `summary` are only compared when `expect` names them. The failure is the
/// compact diff, `<key>: expected <v>, got <v|missing>` for each key that differs.
fn compare(expect: &Value, event: &Value) -> Outcome {
    let Some(want) = expect.as_object() else {
        return Outcome::Fail(format!("expect is not an object: {expect}"));
    };
    let diff: Vec<String> = want
        .iter()
        .filter(|(k, v)| event.get(k.as_str()) != Some(v))
        .map(|(k, v)| match event.get(k.as_str()) {
            Some(got) => format!("{k}: expected {v}, got {got}"),
            None => format!("{k}: expected {v}, got missing"),
        })
        .collect();
    if diff.is_empty() {
        Outcome::Ok
    } else {
        Outcome::Fail(diff.join("; "))
    }
}
