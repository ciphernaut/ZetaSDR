//! The node's own signature proposals, and the session records of exploration jobs.
//!
//! - A `capture` job that ended on its own with a window written proposes a
//!   features-only signature ([`crate::signatures::CONFIDENCE_FEATURES`]) at its end
//!   ([`capture_ended`]), unless its classify proved a mode, its label is `noise`, or an
//!   accepted signature's band already holds the window's centre.
//! - A `bits` job and a `graph` job propose once when they reach
//!   [`PROPOSE_CRC_FRAMES`] CRC-clean frames: a `hypotheses` event with a CRC candidate
//!   that checks on that many, or that many `frame` events with `crc_ok`
//!   ([`watch_for_proposals`]). A mode row's job (a frame row's) never proposes: its
//!   frames prove the row (its session's `proved`).
//! - Every mode row's job is watched for the row's first proof, which supersedes the
//!   accepted signatures it overlaps (`Inner::supersede_by`; a `learned_*` row's never
//!   does).
//!
//! Each proposal publishes a `signature` event (`state: proposed`) and is noted to the
//! job (`job_context.proposal`, or `job_context.capture.proposal` for a capture). Each
//! of those jobs writes a [`Session`] at its end ([`record_session`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::Inner;
use super::persist::now;
use crate::bursts::CaptureRecord;
use crate::classify::{Band, Classification, FeatureRecord, Measurement, Outcome as Tried};
use crate::job::JobRecord;
use crate::knowledge::Allocation;
use crate::modes::{Decoder, FRAME_DECODE_STAGE, ModeRow};
use crate::ops::{BITS_KIND, CAPTURE_IN_STAGE, GRAPH_KIND, JobKind};
use crate::sessions::{Outcome, Session};
use crate::signatures::{NOISE_LABEL, NewSignature, PROPOSE_CRC_FRAMES, SignatureRecord};
use crate::stream::{Frame, Payload};
use crate::types::{Baud, Hz, Provenance, Span, StreamId, StreamKind, Timestamp};

/// A layer-2 decode label this sure proved its mode.
const PROVED_CONFIDENCE: f32 = 1.0;
/// How a layer-2 decode label's evidence opens (`decode <row>: ...`); a catalogue
/// label (`catalogue <entry>: ...`) also reaches 1.0 on a full score, and proves
/// nothing.
const DECODE_EVIDENCE_PREFIX: &str = "decode ";
/// The suffix `bit_view` gives a CRC that checks with the bytes sent LSB first.
const LSB_FIRST_SUFFIX: &str = " lsb-first";
/// Bits per hex digit of a sync word.
const HEX_DIGIT_BITS: u64 = 4;
/// Who the node's proposals are by.
const PROPOSED_BY: &str = "node";
/// The allocation role of a transmitter's assignment: a proposal's `allocation` is the
/// first one.
const TX_ROLE: &str = "tx";
/// A watched job's modulation when neither its bits plan nor a demodulator node names
/// one.
const UNKNOWN_MODULATION: &str = "unknown";
/// The longest a job's end waits for its proposal watchers to drain (see
/// [`drain_watchers`]).
pub(super) const PROPOSAL_DRAIN_MS: u64 = 2_000;

/// The `capture` job `rec` (its context as `capture_finished` left it) ended on its own
/// with at least one window written; `classified` is a hand run's classify of its
/// first window, `None` for a watch's child (which inherits the watch's unknown
/// verdict). Propose a features-only signature of the first window, as the module says.
pub(super) async fn capture_ended(
    inner: &Arc<Inner>,
    rec: &JobRecord,
    classified: Option<&Classification>,
) {
    let written = strings(&context_of(inner, rec)["capture"]["written"]);
    let Some(window) = written.first().and_then(|id| inner.bursts.get(id)) else {
        return;
    };
    if classified.is_some_and(proved) {
        return;
    }
    let layer1 = classified.and_then(|c| c.labels.iter().find(|l| l.layer == 1));
    let (label, measured) = match layer1 {
        Some(l) => (l.name.clone(), None),
        None => match measure(inner, &window).await {
            Some(m) => (m.label.name, Some(m.features)),
            None => return,
        },
    };
    if label == NOISE_LABEL {
        return;
    }
    let Some(features) = serde_json::from_value::<FeatureRecord>(window.features.clone())
        .ok()
        .or(measured)
    else {
        return;
    };
    let summary = super::watch::measured(&serde_json::to_value(&features).unwrap_or_default());
    let mut jobs = vec![rec.id.clone()];
    jobs.extend(rec.owner.clone());
    let new = NewSignature {
        name: None,
        modulation: label,
        framing: None,
        crc_frames: 0,
        evidence: format!("features only: {}", summary.trim()),
        source: window.source.clone(),
        station: inner.station.lock().unwrap().clone(),
        captures: written,
        jobs,
        allocation: first_tx(&window),
        proposed_by: PROPOSED_BY.into(),
        features,
    };
    let out = inner.jobs.lock().unwrap().get(&rec.id).and_then(|e| {
        e.outputs
            .iter()
            .find(|(k, _)| *k == StreamKind::Event)
            .map(|(_, s)| s.clone())
    });
    let prov = Provenance {
        source: rec.source.clone(),
        job: Some(rec.id.clone()),
        channel: None,
        // `publish` stamps the span.
        span: Span {
            start_ns: Timestamp(0),
            end_ns: Timestamp(0),
        },
    };
    propose(inner, rec, new, out.map(|s| (s, prov)), |ctx, id| {
        let mut capture = ctx["capture"].clone();
        capture["proposal"] = id.into();
        json!({ "capture": capture })
    });
}

/// Whether `c` proved a mode: a quick decode passed its check, or a layer-2 decode
/// label is sure of one (a catalogue label is not a proof, however sure).
fn proved(c: &Classification) -> bool {
    c.tried.iter().any(|t| t.outcome == Tried::Proved)
        || c.labels.iter().any(|l| {
            l.layer == 2
                && l.confidence >= PROVED_CONFIDENCE
                && l.evidence.starts_with(DECODE_EVIDENCE_PREFIX)
        })
}

/// Layer 1's measurement of `window`'s band.
async fn measure(inner: &Arc<Inner>, window: &CaptureRecord) -> Option<Measurement> {
    let path = window.meta_path.clone();
    let (meta, samples) = tokio::task::spawn_blocking(move || crate::capture::read_cf32(&path))
        .await
        .ok()?
        .ok()?;
    let set = inner.classifiers.read().unwrap().clone();
    let band = Band {
        offset_hz: window.center_hz.0 as i64 - meta.center_hz.0 as i64,
        bandwidth_hz: window.bandwidth_hz,
    };
    set.measure(&samples, meta.sample_rate_hz, meta.center_hz, band)
        .await
}

/// The first `tx` assignment of `window`'s allocations.
fn first_tx(window: &CaptureRecord) -> Option<Allocation> {
    serde_json::from_value::<Vec<Allocation>>(window.allocations.clone())
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.role.as_deref() == Some(TX_ROLE))
}

/// `rec`'s context as it stands now (its launch-time copy when it is gone).
fn context_of(inner: &Inner, rec: &JobRecord) -> Value {
    inner
        .jobs
        .lock()
        .unwrap()
        .get(&rec.id)
        .map_or_else(|| rec.context.clone(), |e| e.record.context.clone())
}

/// The strings of a JSON array (none for anything else).
fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

/// The proposal noted to a job's context, either key.
fn proposal_of(ctx: &Value) -> Option<&str> {
    ctx["proposal"]
        .as_str()
        .or_else(|| ctx["capture"]["proposal"].as_str())
}

/// Store `new` for the job `rec` unless the job already proposed or an accepted
/// signature's band holds `new`'s centre; then publish the `signature` event on `out`
/// and merge `patch(context, id)` into the job's context. Serialised by
/// `Inner::proposing`, so two of a job's streams never propose twice. Whether it
/// proposed.
fn propose(
    inner: &Inner,
    rec: &JobRecord,
    new: NewSignature,
    out: Option<(StreamId, Provenance)>,
    patch: impl FnOnce(&Value, &str) -> Value,
) -> bool {
    let _one = inner.proposing.lock().unwrap();
    let ctx = context_of(inner, rec);
    if proposal_of(&ctx).is_some() {
        return false;
    }
    let center = new.features.center_hz;
    if inner
        .signatures
        .accepted()
        .iter()
        .any(|s| s.band_holds(center))
    {
        return false;
    }
    let sig = match inner.signatures.propose(new, now().0) {
        Ok(sig) => sig,
        Err(e) => {
            inner
                .persist_errors
                .lock()
                .unwrap()
                .push(format!("signatures: job {}: {e}", rec.id.0));
            return false;
        }
    };
    if let Some((stream, prov)) = out {
        publish(inner, stream, prov, &sig);
    }
    let _ = inner.context_merge(&rec.id, patch(&ctx, &sig.id));
    true
}

/// Publish `sig`'s `signature` event on `stream`.
fn publish(inner: &Inner, stream: StreamId, prov: Provenance, sig: &SignatureRecord) {
    let t = now();
    let payload = json!({
        "protocol": "signature",
        "state": sig.state.name(),
        "id": sig.id,
        "confidence": sig.confidence,
        "name": sig.name,
        "summary": format!("proposed {} ({})", sig.name, sig.confidence),
        "t_ns": t.0,
    });
    let prov = Provenance {
        span: Span {
            start_ns: t,
            end_ns: t,
        },
        ..prov
    };
    inner
        .bus
        .publish(Frame::new(stream, prov, Payload::Event(payload)));
}

/// The mode row a `Mode` job of kind `kind` runs, if it runs one.
fn mode_row(inner: &Inner, kind: &str) -> Option<ModeRow> {
    match inner.expandable_kind(kind)? {
        JobKind::Mode(id) => inner.modes().get(&id.0).map(|m| m.row.clone()),
        _ => None,
    }
}

/// The frame row `kind` names, if it names one.
fn frame_row(inner: &Inner, kind: &str) -> Option<ModeRow> {
    mode_row(inner, kind).filter(|r| matches!(r.decoder, Decoder::Frame(_)))
}

/// Whether the job `rec` watches its frames for a proposal: a `bits` or `graph` job.
/// A mode row's job (a frame row's, a `learned_*` row's) does not: the row already
/// decodes the signal, and its frames prove the row (its session's `proved`).
fn proposes(rec: &JobRecord) -> bool {
    rec.kind == BITS_KIND || rec.kind == GRAPH_KIND
}

/// Whether the job `rec`'s events are watched ([`watch_for_proposals`]): it
/// [`proposes`], or it runs a mode row (whose first proof supersedes the accepted
/// signatures it overlaps).
pub(super) fn watches(inner: &Inner, rec: &JobRecord) -> bool {
    proposes(rec) || mode_row(inner, &rec.kind).is_some()
}

/// What set a job's proposal off.
enum Trigger<'a> {
    /// A `hypotheses` event and its CRC candidate.
    Hypothesis(&'a Value, &'a Value),
    /// The `n`th CRC-clean `frame` event.
    Frames(u32),
}

/// The first proof of a mode row's job, which supersedes the accepted signatures it
/// overlaps ([`Inner::supersede_by`]): an event of the row's `protocol` (with
/// `identify.proof: field`, one whose field is true). Shared by the job's watch tasks,
/// so one run supersedes once.
struct Proof {
    protocol: String,
    field: Option<String>,
    seen: Arc<AtomicBool>,
}

impl Proof {
    /// Whether the event `v` proves the row.
    fn proves(&self, v: &Value) -> bool {
        v["protocol"] == self.protocol.as_str()
            && self.field.as_ref().is_none_or(|f| v[f.as_str()] == true)
    }
}

/// What one watch task still looks for.
struct Watch {
    /// The mode row's first proof, while unseen (`None` for a job of no mode row).
    proof: Option<Proof>,
    /// Whether the job may still propose.
    propose: bool,
    /// The CRC-clean frames seen so far.
    clean: u32,
}

/// For a job [`watches`] says is watched: watch `rec`'s `Event` outputs among
/// `outputs`, one task per output, each subscribed before this returns, for
/// [`PROPOSE_CRC_FRAMES`] CRC-clean frames when it [`proposes`] (and has not proposed:
/// `job_context.proposal`) and, for a mode row's job, for the row's first proof (see
/// [`Proof`]), which supersedes the accepted signatures it overlaps. A task ends once
/// neither is left to see (the job proposed, by any of its tasks, and the proof was
/// seen), when its stream is unregistered, or on `token` (draining what is buffered);
/// it is not one of the job's stages. The tasks' handles: the job's supervisor awaits
/// them (at most [`PROPOSAL_DRAIN_MS`]) before it writes the session record, so a
/// proposal from the job's last events is in it.
pub(super) fn watch_for_proposals(
    inner: &Arc<Inner>,
    rec: &JobRecord,
    outputs: &[(StreamKind, StreamId)],
    token: &CancellationToken,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();
    let propose = proposes(rec) && proposal_of(&rec.context).is_none();
    let row = mode_row(inner, &rec.kind);
    if !propose && row.is_none() {
        return handles;
    }
    let seen = Arc::new(AtomicBool::new(false));
    let rec = Arc::new(rec.clone());
    for (_, sid) in outputs.iter().filter(|(k, _)| *k == StreamKind::Event) {
        let Some(mut rx) = inner.bus.subscribe(sid) else {
            continue;
        };
        let mut watch = Watch {
            proof: row.as_ref().map(|r| Proof {
                protocol: r.protocol.clone(),
                field: match r.identify.proof {
                    crate::modes::Proof::Field => r.identify.field.clone(),
                    crate::modes::Proof::Event => None,
                },
                seen: seen.clone(),
            }),
            propose,
            clean: 0,
        };
        // Weak: a watcher never keeps the node alive.
        let (inner, rec, sid, token) = (
            Arc::downgrade(inner),
            rec.clone(),
            sid.clone(),
            token.clone(),
        );
        handles.push(tokio::spawn(async move {
            let mut on = |frame: Frame| on_frame(&inner, &rec, &sid, &mut watch, frame);
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => loop {
                        match rx.try_recv() {
                            Ok(frame) => {
                                if on(frame) {
                                    return;
                                }
                            }
                            Err(TryRecvError::Lagged(_)) => continue,
                            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
                        }
                    },
                    r = rx.recv() => match r {
                        Ok(frame) => {
                            if on(frame) {
                                return;
                            }
                        }
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => return,
                    },
                }
            }
        }));
    }
    handles
}

/// Await a job's proposal watchers (they drain what is buffered once its token is
/// cancelled, then end), at most [`PROPOSAL_DRAIN_MS`] in all; one still running then
/// is aborted, so a watcher never holds up the job's end.
pub(super) async fn drain_watchers(watchers: Vec<JoinHandle<()>>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(PROPOSAL_DRAIN_MS);
    for mut w in watchers {
        if tokio::time::timeout_at(deadline, &mut w).await.is_err() {
            w.abort();
        }
    }
}

/// One event of the job `rec`'s stream `sid`: supersede when it is the mode row's first
/// proof, propose when it is the trigger. Whether the watch is over.
fn on_frame(
    inner: &Weak<Inner>,
    rec: &JobRecord,
    sid: &StreamId,
    watch: &mut Watch,
    frame: Frame,
) -> bool {
    let Some(inner) = inner.upgrade() else {
        return true;
    };
    let Payload::Event(v) = &frame.payload else {
        return false;
    };
    if let Some(proof) = &watch.proof {
        if proof.proves(v) && !proof.seen.swap(true, Ordering::SeqCst) {
            inner.supersede_by(&rec.kind, rec.tuning, now().0);
        }
        if proof.seen.load(Ordering::SeqCst) {
            watch.proof = None;
        }
    }
    if watch.propose && proposal_event(&inner, rec, sid, &mut watch.clean, &frame, v) {
        watch.propose = false;
    }
    !watch.propose && watch.proof.is_none()
}

/// The event `v` (of `frame`) of the proposing job `rec`'s stream `sid`: propose when
/// it is the trigger. Whether the job's proposal watch is over.
fn proposal_event(
    inner: &Inner,
    rec: &JobRecord,
    sid: &StreamId,
    clean: &mut u32,
    frame: &Frame,
    v: &Value,
) -> bool {
    let candidate;
    let trigger = if v["protocol"] == "hypotheses" {
        let found = v["crc_candidates"].as_array().and_then(|cs| {
            cs.iter().find(|c| {
                c["frames"]
                    .as_u64()
                    .is_some_and(|f| f >= u64::from(PROPOSE_CRC_FRAMES))
            })
        });
        match found {
            Some(c) => {
                candidate = c;
                Trigger::Hypothesis(v, candidate)
            }
            None => return false,
        }
    } else if v["crc_ok"] == true && v["protocol"] != "signature" {
        *clean += 1;
        if *clean < PROPOSE_CRC_FRAMES {
            return false;
        }
        Trigger::Frames(*clean)
    } else {
        return false;
    };
    if proposal_of(&context_of(inner, rec)).is_none() {
        propose_for_job(inner, rec, trigger, sid, frame.provenance.clone());
    }
    true
}

/// The captures a job reads: `context.bits.captures`, else its graph's first
/// `capture_in`'s `captures`.
fn job_captures(rec: &JobRecord) -> Vec<String> {
    let bits = strings(&rec.context["bits"]["captures"]);
    if !bits.is_empty() {
        return bits;
    }
    rec.graph
        .nodes
        .iter()
        .find(|n| n.kind == CAPTURE_IN_STAGE)
        .map(|n| strings(&n.params["captures"]))
        .unwrap_or_default()
}

/// The feature record of a job with no capture (a frame row's job on a radio): its
/// tuning, and the row's demodulator (else the bits plan's rate and order).
fn tuning_features(rec: &JobRecord, row: Option<&ModeRow>) -> FeatureRecord {
    let demod = row.and_then(|r| match &r.decoder {
        Decoder::Frame(b) => Some(&b.demod),
        _ => None,
    });
    let bits = &rec.context["bits"];
    FeatureRecord {
        bandwidth_hz: rec.tuning.bandwidth_hz,
        center_hz: rec.tuning.center_hz,
        symbol_rate_baud: demod.map(|d| Baud(d.symbol_rate_baud)).or_else(|| {
            bits["symbol_rate_baud"]
                .as_u64()
                .and_then(|b| u32::try_from(b).ok())
                .map(Baud)
        }),
        snr_db: 0.0,
        burst: false,
        duty: None,
        bursts: None,
        burst_ms: None,
        period_ms: None,
        order: demod
            .map(|d| d.order)
            .or_else(|| bits["order"].as_u64().and_then(|o| u8::try_from(o).ok())),
        tone_spacing_hz: None,
        symbol_rate_line: None,
    }
}

/// The modulation a job's demodulator node names: the node `demod`, else the first
/// whose kind ends `_demod` (`fsk_demod` → `fsk`).
fn demod_modulation(rec: &JobRecord) -> Option<String> {
    let nodes = &rec.graph.nodes;
    nodes
        .iter()
        .find(|n| n.id.0 == "demod")
        .or_else(|| nodes.iter().find(|n| n.kind.ends_with("_demod")))
        .and_then(|n| n.kind.strip_suffix("_demod"))
        .map(str::to_string)
}

/// Propose for the watched job `rec` on `trigger`, publishing on `sid`.
fn propose_for_job(
    inner: &Inner,
    rec: &JobRecord,
    trigger: Trigger<'_>,
    sid: &StreamId,
    prov: Provenance,
) {
    let captures = job_captures(rec);
    let first = captures.first().and_then(|c| inner.bursts.get(c));
    let features = first
        .as_ref()
        .and_then(|w| serde_json::from_value::<FeatureRecord>(w.features.clone()).ok())
        .unwrap_or_else(|| tuning_features(rec, None));
    let modulation = rec.context["bits"]["modulation"]
        .as_str()
        .map(str::to_string)
        .or_else(|| demod_modulation(rec))
        .unwrap_or_else(|| UNKNOWN_MODULATION.into());
    let (framing, crc_frames, evidence) = match trigger {
        Trigger::Hypothesis(h, c) => {
            let frames = c["frames"].as_u64().unwrap_or_default();
            let sync = c["sync"].as_str().unwrap_or_default();
            let period = match c["period_bits"].as_u64().unwrap_or_default() {
                0 => "to the next sync".to_string(),
                p => format!("every {p} bits"),
            };
            let evidence = format!(
                "{} on {frames}/{} frames, sync 0x{sync} {period}",
                c["crc"].as_str().unwrap_or_default(),
                c["checked"].as_u64().unwrap_or_default(),
            );
            (
                framing_from_hypothesis(h, c),
                u32::try_from(frames).unwrap_or(u32::MAX),
                evidence,
            )
        }
        Trigger::Frames(n) => {
            let framing = rec
                .graph
                .nodes
                .iter()
                .find(|n| n.kind == FRAME_DECODE_STAGE)
                .map(|n| n.params.clone())
                .unwrap_or(Value::Null);
            (framing, n, format!("{n} CRC-clean frames ({})", rec.kind))
        }
    };
    let new = NewSignature {
        name: None,
        features,
        modulation,
        framing: Some(framing),
        crc_frames,
        evidence,
        source: first
            .as_ref()
            .map_or_else(|| rec.source.clone(), |w| w.source.clone()),
        station: inner.station.lock().unwrap().clone(),
        captures,
        jobs: vec![rec.id.clone()],
        allocation: first.as_ref().and_then(first_tx),
        proposed_by: PROPOSED_BY.into(),
    };
    propose(
        inner,
        rec,
        new,
        Some((sid.clone(), prov)),
        |_, id| json!({ "proposal": id }),
    );
}

/// A `frame_decode` framing from `bit_view`'s hypotheses `h` and its CRC candidate
/// `candidate`: NRZ (`bit_view` hunts syncs and checks CRCs on the line bits as they
/// are, whatever line code it finds likely), the candidate's sync (its width from the
/// `sync_candidates` entry), a fixed length of the period less the sync (frames to
/// the next sync when the period is 0), and the CRC (`bit_order` `lsb` for a
/// ` lsb-first` one).
pub fn framing_from_hypothesis(h: &Value, candidate: &Value) -> Value {
    let sync = candidate["sync"].as_str().unwrap_or_default();
    let bits = h["sync_candidates"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["word"] == sync))
        .and_then(|s| s["bits"].as_u64())
        .unwrap_or(sync.len() as u64 * HEX_DIGIT_BITS);
    let length = match candidate["period_bits"].as_u64().unwrap_or_default() {
        0 => json!({"to_next_sync": true}),
        p => json!({"fixed_bits": p.saturating_sub(bits)}),
    };
    let crc = candidate["crc"].as_str().unwrap_or_default();
    let (crc, bit_order) = match crc.strip_suffix(LSB_FIRST_SUFFIX) {
        Some(c) => (c, "lsb"),
        None => (crc, "msb"),
    };
    json!({
        "encoding": "nrz",
        "sync": {"hex": sync, "bits": bits},
        "preamble": null,
        "whitening": "none",
        "length": length,
        "crc": crc,
        "bit_order": bit_order,
        "max_errors": 0,
    })
}

/// Whether the job `rec` writes a session record: a `capture`, `bits` or `graph` job,
/// or a frame row's.
fn keeps_session(inner: &Inner, rec: &JobRecord) -> bool {
    rec.kind == JobKind::Capture.name()
        || rec.kind == BITS_KIND
        || rec.kind == GRAPH_KIND
        || frame_row(inner, &rec.kind).is_some()
}

/// The session record of the exploration job `rec` (as it stands at its end), ended
/// at `ended_ns`: its captures (`capture.written`, `bits.captures`, its graph's
/// `capture_in`), `bit_view` and `frame_decode` from its context, and its outcome: the
/// proposal noted to it, else `proved` when it is a frame row's job (`frame_row`) that
/// decoded a CRC-clean frame, else none. `features` is left `null` for
/// [`record_session`].
pub(super) fn session_of(rec: &JobRecord, frame_row: bool, ended_ns: u64) -> Session {
    let ctx = &rec.context;
    let captures = if rec.kind == JobKind::Capture.name() {
        strings(&ctx["capture"]["written"])
    } else {
        job_captures(rec)
    };
    let outcome = match proposal_of(ctx) {
        Some(id) => Outcome::Proposal { id: id.into() },
        None if frame_row
            && ctx["frame_decode"]["crc_ok"]
                .as_u64()
                .is_some_and(|n| n >= 1) =>
        {
            Outcome::Proved {
                mode: rec.kind.clone(),
            }
        }
        None => Outcome::None,
    };
    let of = |k: &str| ctx.get(k).cloned().unwrap_or(Value::Null);
    Session {
        job: rec.id.clone(),
        kind: rec.kind.clone(),
        source: rec.source.clone(),
        center_hz: Hz(rec.tuning.center_hz.0),
        bandwidth_hz: rec.tuning.bandwidth_hz,
        captures,
        features: Value::Null,
        graph: rec.graph.clone(),
        hypotheses: of("bit_view"),
        frame_decode: of("frame_decode"),
        outcome,
        started_ns: rec.created_ns.0,
        ended_ns,
    }
}

/// At the end of the job `rec`: write its [`session_of`] when it [`keeps_session`],
/// its features the first capture's record still in the store (a frame row's job
/// with none: its tuning and the row's demodulator); after a `capture` job, sweep the
/// sessions (its windows may have expired others' captures). A failed write is
/// recorded in `persist_errors`.
pub(super) fn record_session(inner: &Inner, rec: &JobRecord) {
    if !keeps_session(inner, rec) {
        return;
    }
    let row = frame_row(inner, &rec.kind);
    let mut s = session_of(rec, row.is_some(), now().0);
    let first = s.captures.first().and_then(|c| inner.bursts.get(c));
    s.features = match (first, row) {
        (Some(w), _) => w.features,
        (None, Some(row)) => {
            serde_json::to_value(tuning_features(rec, Some(&row))).unwrap_or_default()
        }
        (None, None) => Value::Null,
    };
    if let Err(e) = inner.sessions.write(&s) {
        inner
            .persist_errors
            .lock()
            .unwrap()
            .push(format!("sessions/{}.json: {e}", rec.id.0));
    }
    if rec.kind == JobKind::Capture.name() {
        inner.sweep_sessions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_framing_reads_from_a_crc_candidate() {
        let h = json!({
            "encoding": {"likely": "nrz"},
            "sync_candidates": [{"word": "2dd4", "bits": 16, "count": 14, "mean_gap_bits": 1120.0}],
        });
        let c = json!({"crc": "crc16-ccitt", "sync": "2dd4", "period_bits": 1120, "frames": 12, "checked": 14});
        assert_eq!(
            framing_from_hypothesis(&h, &c),
            json!({"encoding": "nrz", "sync": {"hex": "2dd4", "bits": 16}, "preamble": null,
                "whitening": "none", "length": {"fixed_bits": 1104}, "crc": "crc16-ccitt",
                "bit_order": "msb", "max_errors": 0})
        );
        let c = json!({"crc": "crc16-ccitt lsb-first", "sync": "2dd4", "period_bits": 0, "frames": 6, "checked": 6});
        let f = framing_from_hypothesis(&h, &c);
        assert_eq!(f["length"], json!({"to_next_sync": true}));
        assert_eq!(f["crc"], "crc16-ccitt");
        assert_eq!(f["bit_order"], "lsb");
        // bit_view hunts syncs and checks CRCs on the line bits as they are.
        let h = json!({"encoding": {"likely": "manchester"}, "sync_candidates": []});
        assert_eq!(framing_from_hypothesis(&h, &c)["encoding"], "nrz");
    }

    /// `n` pseudo-random bodies of `len` bytes.
    fn bodies(n: usize, len: usize, seed: u64) -> Vec<Vec<u8>> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..n)
            .map(|_| {
                (0..len)
                    .map(|_| {
                        s ^= s << 13;
                        s ^= s >> 7;
                        s ^= s << 17;
                        (s >> 24) as u8
                    })
                    .collect()
            })
            .collect()
    }

    /// Every CRC candidate `bit_view` lists, framed by `framing_from_hypothesis`,
    /// decodes CRC-clean under `frame_decode`'s framing, for every CRC of the table sent
    /// either bit order; and the CRC the frames were written with is among them, its
    /// fixed cut decoding every frame CRC-clean.
    #[test]
    fn every_bit_view_crc_candidate_decodes_crc_clean() {
        use zeta_dsp::framing::{Framing, decode_frames, encode_frames};
        const FRAMES: usize = 20;
        const BODY: usize = 32;
        for (k, spec) in zeta_dsp::crc::CRCS.iter().enumerate() {
            for order in ["msb", "lsb"] {
                let crc_bits = usize::from(spec.width);
                let written: Framing = serde_json::from_value(json!({
                    "encoding": "nrz", "sync": {"hex": "2dd4", "bits": 16},
                    "length": {"fixed_bits": BODY * 8 + crc_bits},
                    "crc": spec.name, "bit_order": order,
                }))
                .unwrap();
                let line = encode_frames(&bodies(FRAMES, BODY, k as u64 + 1), &written);
                let h = serde_json::to_value(zeta_dsp::bit_view::hypotheses(&[&line])).unwrap();
                let want = match order {
                    "lsb" => format!("{}{LSB_FIRST_SUFFIX}", spec.name),
                    _ => spec.name.to_string(),
                };
                let candidates = h["crc_candidates"].as_array().unwrap();
                assert!(
                    candidates.iter().any(|c| c["crc"] == want.as_str()
                        && c["sync"] == "2dd4"
                        && c["period_bits"] != 0),
                    "{want}: not among {candidates:?}"
                );
                for c in candidates {
                    let framing = framing_from_hypothesis(&h, c);
                    let f: Framing = serde_json::from_value(framing.clone()).unwrap();
                    let (frames, _) = decode_frames(&line, &f);
                    let clean = frames.iter().filter(|f| f.crc_ok == Some(true)).count();
                    assert!(
                        clean as u64 >= c["frames"].as_u64().unwrap(),
                        "{want}: candidate {c} decodes {clean} clean of {} under {framing}",
                        frames.len()
                    );
                    // A stray sync in a body cuts a to-the-next-sync frame short, so
                    // only the fixed cut is held to every frame.
                    if c["crc"] == want.as_str() && c["sync"] == "2dd4" && c["period_bits"] != 0 {
                        assert_eq!(frames.len(), FRAMES, "{want}: {framing}");
                        assert!(
                            frames.iter().all(|f| f.crc_ok == Some(true)),
                            "{want}: {framing}"
                        );
                    }
                }
            }
        }
    }
}
