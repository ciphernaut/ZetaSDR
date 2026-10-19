//! RDS decode stage: IQ frames in, events as station data completes.
#![cfg(feature = "test-fixtures")]

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Stage, StageContext, StageError};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::acars::AcarsDecodeStage;
use zeta_dsp::fixtures::acars_packet_iq;

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

type Run = tokio::task::JoinHandle<Result<(), StageError>>;

fn spawn(params: serde_json::Value) -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
    let bus = Arc::new(StreamBus::new(8));
    let out = bus.register(StreamKind::Event, prov());
    let rx_out = bus.subscribe(&out).unwrap();
    let (tx, rx_in) = broadcast::channel(64);
    let h = tokio::spawn(async move {
        AcarsDecodeStage::new()
            .run(
                params,
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

fn iq_frame(iq: &[num_complex::Complex<f32>], rate_hz: u64) -> Frame {
    Frame::new(
        StreamId::new(),
        prov(),
        Payload::Iq(IqBlock {
            t0: Timestamp(0),
            sample_rate_hz: Hz(rate_hz),
            center_hz: Hz(100_000_000),
            samples: iq.to_vec().into(),
        }),
    )
}

fn packet() -> Vec<num_complex::Complex<f32>> {
    acars_packet_iq("VH-ZET", "H1", "#M1BPOS/ZETASDR TEST", 48_000, 0.0)
}

#[tokio::test]
async fn acars_stage_emits_block_event() {
    let (tx, mut rx_out, h) = spawn(serde_json::json!({}));
    for chunk in packet().chunks(4_800) {
        tx.send(iq_frame(chunk, 48_000)).unwrap();
    }
    drop(tx);
    assert!(matches!(h.await.unwrap(), Ok(())));
    let mut events = Vec::new();
    while let Ok(f) = rx_out.try_recv() {
        assert_eq!(f.kind, StreamKind::Event);
        let Payload::Event(v) = &f.payload else {
            panic!("expected an event")
        };
        events.push(v.clone());
    }
    assert_eq!(events.len(), 1, "{events:?}");
    let v = &events[0];
    assert_eq!(v["protocol"], "acars");
    assert_eq!(v["reg"], "VH-ZET");
    assert_eq!(v["label"], "H1");
    assert_eq!(v["text"], "#M1BPOS/ZETASDR TEST");
    assert_eq!(v["crc_ok"], true);
    assert_eq!(v["mode"], "2");
    assert!(v["ack"].is_null(), "{v}");
    assert!(v["msg_no"].is_null(), "{v}");
    assert!(v["flight"].is_null(), "{v}");
}

#[tokio::test]
async fn acars_stage_rejects_unknown_params() {
    let (tx, _rx, h) = spawn(serde_json::json!({"baud": 2400}));
    drop(tx);
    assert!(matches!(h.await.unwrap(), Err(StageError::Failed(_))));
}

#[tokio::test]
async fn acars_stage_fails_when_the_rate_changes() {
    let (tx, _rx, h) = spawn(serde_json::json!({}));
    let iq = packet();
    tx.send(iq_frame(&iq[..4_800], 48_000)).unwrap();
    tx.send(iq_frame(&iq[..4_000], 40_000)).unwrap();
    drop(tx);
    let Err(StageError::Failed(m)) = h.await.unwrap() else {
        panic!("expected a failure")
    };
    assert!(m.contains("sample_rate_hz changed"), "{m}");
}
