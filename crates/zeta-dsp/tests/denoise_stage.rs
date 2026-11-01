//! Denoise stage: audio frames in, mono 48 kHz audio frames out.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::denoise::{DENOISE_FRAME, DenoiseStage};
use zeta_dsp::fixtures::speech_like;

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

type Run = tokio::task::JoinHandle<Result<(), StageError>>;

fn spawn() -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
    let bus = Arc::new(StreamBus::new(8));
    let out = bus.register(StreamKind::Audio, prov(0));
    let rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(64);
    let h = tokio::spawn(async move {
        DenoiseStage::new()
            .run(
                serde_json::json!({}),
                vec![rx_in],
                vec![out],
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    });
    (tx, rx_out, h)
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

#[tokio::test]
async fn frames_of_4800_samples_come_out_mono_48k_with_t0_kept() {
    let (tx, mut rx, h) = spawn();
    let x = speech_like(48_000, 1.2);
    let mut total = 0;
    for (i, c) in x.chunks(4_800).enumerate() {
        let t0 = 1_000 + i as u64 * 100_000_000;
        tx.send(audio(c.to_vec(), 48_000, 1, t0)).unwrap();
        let f = rx.recv().await.unwrap();
        assert_eq!(f.provenance.span.start_ns, Timestamp(t0));
        let Payload::Audio {
            sample_rate_hz,
            channels,
            samples,
        } = &f.payload
        else {
            panic!("expected audio");
        };
        assert_eq!((sample_rate_hz.0, *channels), (48_000, 1));
        assert_eq!(samples.len(), 4_800);
        assert_eq!(samples.len() % DENOISE_FRAME, 0);
        total += samples.len();
    }
    assert_eq!(total, x.len());
    drop(tx);
    assert!(h.await.unwrap().is_ok());
}

#[tokio::test]
async fn rate_change_fails_with_both_rates() {
    let (tx, _rx, h) = spawn();
    tx.send(audio(vec![0.0; 4_800], 48_000, 1, 0)).unwrap();
    tx.send(audio(vec![0.0; 4_800], 16_000, 1, 0)).unwrap();
    match h.await.unwrap() {
        Err(StageError::Failed(m)) => assert_eq!(m, "sample_rate_hz changed from 48000 to 16000"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn channel_change_fails_with_both_counts() {
    let (tx, _rx, h) = spawn();
    tx.send(audio(vec![0.0; 4_800], 48_000, 1, 0)).unwrap();
    tx.send(audio(vec![0.0; 4_800], 48_000, 2, 0)).unwrap();
    match h.await.unwrap() {
        Err(StageError::Failed(m)) => assert_eq!(m, "channels changed from 1 to 2"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn held_samples_date_the_next_frame_from_their_own_block() {
    // 4 800 samples are ten whole frames; 100 more are held back; the 380 that complete
    // the frame come out dated from the 100's block, not from their own.
    let (tx, mut rx, _h) = spawn();
    let x = speech_like(48_000, 0.2);
    tx.send(audio(x[..4_800].to_vec(), 48_000, 1, 1_000_000_000))
        .unwrap();
    let f = rx.recv().await.unwrap();
    assert_eq!(f.provenance.span.start_ns, Timestamp(1_000_000_000));
    tx.send(audio(x[4_800..4_900].to_vec(), 48_000, 1, 1_100_000_000))
        .unwrap();
    tx.send(audio(x[4_900..5_280].to_vec(), 48_000, 1, 1_102_083_333))
        .unwrap();
    let f = rx.recv().await.unwrap();
    let Payload::Audio { samples, .. } = &f.payload else {
        panic!("expected audio")
    };
    assert_eq!(samples.len(), DENOISE_FRAME);
    assert_eq!(f.provenance.span.start_ns, Timestamp(1_100_000_000));
    assert_eq!(f.provenance.span.end_ns, Timestamp(1_110_000_000));
}
