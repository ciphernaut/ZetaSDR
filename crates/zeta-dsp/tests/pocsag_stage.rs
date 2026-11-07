//! POCSAG decode stage: IQ frames in, one Event per BCH-clean message.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::fixtures::pocsag_packet_iq;
use zeta_dsp::pocsag::PocsagDecodeStage;

fn prov() -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(5),
            end_ns: Timestamp(9),
        },
    }
}

fn mode(id: &str) -> Arc<zeta_core::modes::ModeRow> {
    let mut stages = zeta_core::stage::StageRegistry::new();
    zeta_dsp::register_all(&mut stages);
    let loaded = zeta_core::modes::ModeRegistry::load(&stages, None)
        .get(id)
        .expect("row")
        .clone();
    Arc::new(loaded.row)
}

type Run = tokio::task::JoinHandle<Result<(), StageError>>;

fn spawn(params: serde_json::Value) -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
    let bus = Arc::new(StreamBus::new(8));
    let out = bus.register(StreamKind::Event, prov());
    let rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(64);
    let h = tokio::spawn(async move {
        PocsagDecodeStage::new()
            .run(
                params,
                vec![rx_in],
                vec![out],
                bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())).with_mode(mode("pocsag_decode")),
            )
            .await
    });
    (tx, rx_out, h)
}

fn iq_frame(iq: &[num_complex::Complex<f32>], rate_hz: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(),
        Payload::Iq(IqBlock {
            t0: Timestamp(0),
            sample_rate_hz: Hz(rate_hz),
            center_hz: Hz(152_000_000),
            samples: iq.to_vec().into(),
        }),
    )
}

#[tokio::test]
async fn pocsag_stage_emits_one_event_per_message() {
    let (tx, mut rx_out, h) = spawn(serde_json::json!({}));
    let iq = pocsag_packet_iq(1234567, 3, "HELLO ZETA", 1200, 48_000, 0.0);
    for chunk in iq.chunks(4_800) {
        tx.send(iq_frame(chunk, 48_000)).unwrap();
    }
    drop(tx);
    assert!(matches!(h.await.unwrap(), Ok(())));
    let f = rx_out.recv().await.unwrap();
    assert_eq!(f.kind, StreamKind::Event);
    let Payload::Event(v) = &f.payload else {
        panic!("expected an event")
    };
    assert_eq!(
        *v,
        serde_json::json!({
            "protocol": "pocsag", "address": 1234567, "function": 3, "baud": 1200,
            "kind": "alpha", "text": "HELLO ZETA",
            "summary": "1234567: HELLO ZETA", "t_ns": 5, "access": "restricted",
        })
    );
    assert!(rx_out.try_recv().is_err(), "exactly one event");
}

#[tokio::test]
async fn pocsag_stage_rejects_unknown_params() {
    let (tx, _rx, h) = spawn(serde_json::json!({"baud": 1200}));
    drop(tx);
    assert!(matches!(h.await.unwrap(), Err(StageError::Failed(_))));
}
