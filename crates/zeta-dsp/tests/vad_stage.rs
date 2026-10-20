//! Vad stage: gated audio on output 0, speech transitions on output 1.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::fixtures::{awgn_real, speech_like};
use zeta_dsp::vad::VadStage;

/// t0 of the first input frame.
const T_BASE: u64 = 1_000;

fn prov(t0: u64) -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(t0),
            end_ns: Timestamp(t0 + 100_000_000),
        },
    }
}

fn audio(samples: Vec<f32>, rate_hz: u64, channels: u16, t0: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(t0),
        Payload::Audio {
            sample_rate_hz: Hz(rate_hz),
            channels,
            samples: samples.into(),
        },
    )
}

type Run = tokio::task::JoinHandle<Result<(), StageError>>;

fn spawn_with(
    params: serde_json::Value,
    bus: Arc<StreamBus>,
    rx: broadcast::Receiver<Frame>,
    outs: Vec<StreamId>,
) -> Run {
    tokio::spawn(async move {
        VadStage::new()
            .run(
                params,
                vec![rx],
                outs,
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    })
}

fn spawn(bus: Arc<StreamBus>, rx: broadcast::Receiver<Frame>, outs: Vec<StreamId>) -> Run {
    spawn_with(serde_json::json!({}), bus, rx, outs)
}

#[tokio::test]
async fn gates_audio_and_emits_transitions() {
    let rate = 48_000u64;
    let bus = Arc::new(StreamBus::new(256));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let e_out = bus.register(StreamKind::Event, prov(0));
    let mut rx_a = bus.subscribe(&a_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(256);
    // A hangover shorter than the 300 ms gaps keeps the bursts apart.
    let h = spawn_with(
        serde_json::json!({"hangover_ms": 200}),
        bus,
        rx_in,
        vec![a_out, e_out],
    );

    // Noise-only lead-in, then 3.6 s of bursts; stereo with identical channels.
    let mut mono = vec![0.0; 4_800];
    mono.extend(speech_like(48_000, 3.6));
    awgn_real(&mut mono, 20.0, 3);
    let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
    for (i, c) in stereo.chunks(9_600).enumerate() {
        let t0 = T_BASE + i as u64 * 100_000_000;
        tx.send(audio(c.to_vec(), rate, 2, t0)).unwrap();
    }
    drop(tx);
    assert!(h.await.unwrap().is_ok());

    let mut events = Vec::new();
    while let Ok(f) = rx_e.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("expected event")
        };
        assert_eq!(v["protocol"], "vad");
        events.push((v["speech"].as_bool().unwrap(), v["t_ns"].as_u64().unwrap()));
    }
    // Six bursts, each one speech run: 12 transitions.
    assert_eq!(events.len(), 12, "{events:?}");
    for (i, (speech, _)) in events.iter().enumerate() {
        assert_eq!(*speech, i % 2 == 0);
    }
    // Burst k starts 100 ms (the lead-in) + k * 600 ms after the first frame's
    // t0; its run begins at its first loud frame, within one 20 ms frame, and
    // lasts the burst plus the 200 ms hangover.
    let frame_ns = 20_000_000u64;
    for k in 0..6usize {
        let burst = T_BASE + 100_000_000 + k as u64 * 600_000_000;
        let (_, on) = events[2 * k];
        let (_, off) = events[2 * k + 1];
        assert!(on.abs_diff(burst) <= frame_ns, "burst {k} on at {on}");
        assert!(
            (off - on).abs_diff(500_000_000) <= frame_ns,
            "burst {k} lasts {}",
            off - on
        );
    }

    // Forwarded audio is 48 kHz stereo; each frame's samples are the matching
    // slice of the original input, located from its start time, and its span
    // length is its sample count at the rate.
    let mut starts = Vec::new();
    let mut forwarded = 0;
    while let Ok(f) = rx_a.try_recv() {
        let Payload::Audio {
            sample_rate_hz,
            channels,
            samples,
        } = &f.payload
        else {
            panic!("expected audio")
        };
        assert_eq!((sample_rate_hz.0, *channels), (rate, 2));
        let n = samples.len() / 2;
        let span = f.provenance.span;
        assert_eq!(
            span.end_ns.0 - span.start_ns.0,
            n as u64 * 1_000_000_000 / rate
        );
        let idx = ((span.start_ns.0 - T_BASE) * rate + 500_000_000) / 1_000_000_000;
        let idx = idx as usize;
        assert_eq!(&stereo[idx * 2..(idx + n) * 2], &samples[..], "at {idx}");
        starts.push(span.start_ns.0);
        forwarded += n;
    }
    for (_, t) in events.iter().filter(|(s, _)| *s) {
        assert!(starts.contains(t), "no audio starts at {t}: {starts:?}");
    }
    // Six runs of about 500 ms.
    assert!(forwarded.abs_diff(6 * 24_000) <= 6 * 2 * 960, "{forwarded}");
}

#[tokio::test]
async fn single_output_skips_events() {
    let bus = Arc::new(StreamBus::new(8));
    let a_out = bus.register(StreamKind::Audio, prov(0));
    let (tx, rx_in) = broadcast::channel(8);
    let h = spawn(bus, rx_in, vec![a_out]);
    tx.send(audio(vec![0.0; 4_800], 48_000, 1, 0)).unwrap();
    drop(tx);
    assert!(h.await.unwrap().is_ok());
}

#[tokio::test]
async fn rate_and_channel_changes_fail() {
    for (rate, ch, msg) in [
        (16_000, 1, "sample_rate_hz changed from 48000 to 16000"),
        (48_000, 2, "channels changed from 1 to 2"),
    ] {
        let bus = Arc::new(StreamBus::new(8));
        let a_out = bus.register(StreamKind::Audio, prov(0));
        let e_out = bus.register(StreamKind::Event, prov(0));
        let (tx, rx_in) = broadcast::channel(8);
        let h = spawn(bus, rx_in, vec![a_out, e_out]);
        tx.send(audio(vec![0.0; 4_800], 48_000, 1, 0)).unwrap();
        tx.send(audio(vec![0.0; 4_800], rate, ch, 0)).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert_eq!(m, msg),
            other => panic!("{other:?}"),
        }
    }
}
