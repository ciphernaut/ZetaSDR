//! Offline chain runner: real stages over a span of IQ on a private bus, with no node.
//! The layer-4 tests and `identify`'s quick attempt run a chain this way.

use std::sync::Arc;

use num_complex::Complex;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, JobId, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};

/// Input time per IQ block fed to the chain, in milliseconds.
const BLOCK_MS: u64 = 50;
/// Start of the fed IQ, in ns since the epoch.
const T0_NS: u64 = 1_000_000_000;
/// Frames each bus stream holds before a slow reader lags: more than any chain makes.
const BUS_CAPACITY: usize = 1 << 14;
const NS_PER_S: u64 = 1_000_000_000;
const MS_PER_S: u64 = 1_000;

/// The stage implementing `kind`.
fn stage_of(kind: &str) -> Arc<dyn Stage> {
    match kind {
        "channel" => Arc::new(crate::channel::ChannelStage::new()),
        "resample" => Arc::new(crate::resample::ResampleStage::new()),
        "fsk_demod" => Arc::new(crate::fsk::FskDemodStage::new()),
        "ook_demod" => Arc::new(crate::ook::OokDemodStage::new()),
        "psk_demod" => Arc::new(crate::psk::PskDemodStage::new()),
        "clock_recover" => Arc::new(crate::clock::ClockRecoverStage::new()),
        "slicer" => Arc::new(crate::slicer::SlicerStage::new()),
        "frame_decode" => Arc::new(crate::frame_decode::FrameDecodeStage::new()),
        "correlate" => Arc::new(crate::correlate::CorrelateStage::new()),
        other => panic!("no stage {other} in the chain harness"),
    }
}

fn prov(span: Span) -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: Some(JobId("job".into())),
        channel: None,
        span,
    }
}

/// Run `stages` in order over `iq` at `rate_hz` on a test bus: one stream registered per
/// stage output, each stage reading the previous stage's first output (the first stage
/// the IQ, fed as [`BLOCK_MS`] blocks with contiguous spans). Once the input is fed and
/// dropped every stage runs to its end, its outputs closed behind it; the result is every
/// event payload of the last stage, in order. Every stage flushes at the end except
/// `resample`, so a chain through it loses its last symbol or so (about 0.8 symbol at
/// 4800 Bd): compare such a chain allowing for that tail.
pub async fn run_chain(stages: &[(&str, Value)], iq: &[Complex<f32>], rate_hz: u64) -> Vec<Value> {
    run_chain_at(stages, iq, rate_hz, 0).await
}

/// [`run_chain`] with the IQ blocks centred on `center_hz` (a `channel` stage cuts its
/// band relative to the block's centre).
pub async fn run_chain_at(
    stages: &[(&str, Value)],
    iq: &[Complex<f32>],
    rate_hz: u64,
    center_hz: u64,
) -> Vec<Value> {
    let bus = Arc::new(StreamBus::new(BUS_CAPACITY));
    let block = (rate_hz * BLOCK_MS / MS_PER_S) as usize;
    let blocks = iq.chunks(block).count();
    let (tx, rx) = broadcast::channel(blocks.max(1));
    let mut input = rx;
    let mut runs = Vec::new();
    let mut last_events = Vec::new();
    let zero = Span {
        start_ns: Timestamp(0),
        end_ns: Timestamp(0),
    };
    for (i, (kind, params)) in stages.iter().enumerate() {
        let stage = stage_of(kind);
        let outputs: Vec<StreamId> = stage
            .spec()
            .outputs
            .iter()
            .map(|&k| bus.register(k, prov(zero.clone())))
            .collect();
        let next = bus.subscribe(&outputs[0]).expect("registered above");
        if i + 1 == stages.len() {
            last_events = stage
                .spec()
                .outputs
                .iter()
                .zip(&outputs)
                .filter(|(k, _)| **k == StreamKind::Event)
                .map(|(_, id)| bus.subscribe(id).expect("registered above"))
                .collect();
        }
        let (bus, params, rx) = (bus.clone(), params.clone(), input);
        runs.push(tokio::spawn(async move {
            let r = stage
                .run(
                    params,
                    vec![rx],
                    outputs.clone(),
                    bus.clone(),
                    CancellationToken::new(),
                    StageContext::new(SourceId("s".into())),
                )
                .await;
            for id in &outputs {
                bus.unregister(id);
            }
            r
        }));
        input = next;
    }
    drop(input);
    let iq_stream = StreamId::new();
    let mut t0 = T0_NS;
    for chunk in iq.chunks(block) {
        let end = t0 + chunk.len() as u64 * NS_PER_S / rate_hz;
        tx.send(Frame::new(
            iq_stream.clone(),
            prov(Span {
                start_ns: Timestamp(t0),
                end_ns: Timestamp(end),
            }),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0),
                sample_rate_hz: Hz(rate_hz),
                center_hz: Hz(center_hz),
                samples: chunk.to_vec().into(),
            }),
        ))
        .expect("the first stage subscribed");
        t0 = end;
    }
    drop(tx);
    for r in runs {
        r.await.expect("stage task").expect("stage run");
    }
    let mut events = Vec::new();
    for mut rx in last_events {
        while let Ok(f) = rx.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("an event payload")
            };
            events.push(v);
        }
    }
    events
}
