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

fn spawn(bus: Arc<StreamBus>, rx: broadcast::Receiver<Frame>, outs: Vec<StreamId>) -> Run {
    tokio::spawn(async move {
        VadStage::new()
            .run(
                serde_json::json!({}),
                vec![rx],
                outs,
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    })
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
    let h = spawn(bus, rx_in, vec![a_out, e_out]);

    // Noise-only lead-in, then 3.6 s of bursts; stereo with identical channels.
    let mut mono = vec![0.0; 4_800];
    mono.extend(speech_like(48_000, 3.6));
    awgn_real(&mut mono, 20.0, 3);
    let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
    for (i, c) in stereo.chunks(9_600).enumerate() {
        let t0 = 1_000 + i as u64 * 100_000_000;
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
    // Six bursts; the input ends inside the last one's hangover, so 6 starts, 5 ends.
    assert_eq!(events.len(), 11, "{events:?}");
    for (i, (speech, _)) in events.iter().enumerate() {
        assert_eq!(*speech, i % 2 == 0);
    }

    // Forwarded audio is still 48 kHz stereo and each speech run begins at
    // its event's t_ns (input t0 plus the offset).
    let mut frames = Vec::new();
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
        assert_eq!(samples.len() % 2, 0);
        frames.push((f.provenance.span.start_ns.0, samples.len() / 2));
    }
    for (_, t) in events.iter().filter(|(s, _)| *s) {
        assert!(
            frames.iter().any(|(s, _)| s == t),
            "no audio starts at {t}: {frames:?}"
        );
    }
    let forwarded: usize = frames.iter().map(|(_, n)| n).sum();
    assert!(
        forwarded > 0 && forwarded < mono.len() * 2 / 3,
        "{forwarded}"
    );
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
