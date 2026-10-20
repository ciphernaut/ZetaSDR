//! APRS decode stage: AFSK1200 demodulation, HDLC deframing and AX.25 UI parsing.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::afsk::Afsk1200Demod;
use crate::ax25::{HdlcDeframer, parse_ax25_ui};
use crate::{Input, run_map};

/// `aprs_decode` takes no parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AprsParams {}

/// Decodes APRS packets from an audio stream into one Event per CRC-verified UI frame.
pub struct AprsDecodeStage {
    spec: StageSpec,
}

impl AprsDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "aprs_decode",
                inputs: vec![StreamKind::Audio],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(AprsParams),
            },
        }
    }
}

impl Default for AprsDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-run decoder state, built from the first audio frame's rate.
struct Decoder {
    rate_hz: u64,
    demod: Afsk1200Demod,
    deframer: HdlcDeframer,
}

#[async_trait]
impl Stage for AprsDecodeStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let _: AprsParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("aprs_decode params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let mut dec: Option<Decoder> = None;
        run_map(inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    dec = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &frame.payload
            else {
                return Err(StageError::Failed(
                    "aprs_decode expects audio frames".into(),
                ));
            };
            if *channels != 1 {
                return Err(StageError::Failed(format!(
                    "aprs_decode expects mono audio, got {channels} channels"
                )));
            }
            let rate = sample_rate_hz.0;
            let d = match &mut dec {
                Some(d) if d.rate_hz != rate => {
                    return Err(StageError::Failed(format!(
                        "sample_rate_hz changed from {} to {rate}",
                        d.rate_hz
                    )));
                }
                Some(d) => d,
                None => {
                    let r = u32::try_from(rate).map_err(|_| {
                        StageError::Failed(format!("sample_rate_hz {rate} out of range"))
                    })?;
                    let demod = Afsk1200Demod::new(r).ok_or_else(|| {
                        StageError::Failed(format!("sample_rate_hz {r} too low for afsk1200"))
                    })?;
                    dec.insert(Decoder {
                        rate_hz: rate,
                        demod,
                        deframer: HdlcDeframer::new(),
                    })
                }
            };
            let bits = d.demod.feed(samples);
            let mut events = Vec::new();
            for raw in d.deframer.feed(&bits) {
                // CRC-good frames that are not UI frames are dropped silently.
                let Some(f) = parse_ax25_ui(&raw) else {
                    continue;
                };
                let info_hex = f.info.iter().fold(String::new(), |mut s, b| {
                    let _ = write!(s, "{b:02x}");
                    s
                });
                events.push(Frame::new(
                    out.clone(),
                    frame.provenance.clone(),
                    Payload::Event(serde_json::json!({
                        "protocol": "aprs",
                        "src": f.src,
                        "dst": f.dst,
                        "path": f.path,
                        "info": String::from_utf8_lossy(&f.info),
                        "info_hex": info_hex,
                        "fcs_ok": true,
                    })),
                ));
            }
            Ok(events)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use zeta_core::types::{Hz, Provenance, SourceId, Span, Timestamp};

    use super::*;
    use crate::fixtures::{afsk1200_modulate, ax25_ui_frame, hdlc_frame_bits, nrzi_encode};

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

    fn audio_frame(samples: Vec<f32>, rate_hz: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Audio {
                sample_rate_hz: Hz(rate_hz),
                channels: 1,
                samples: samples.into(),
            },
        )
    }

    type Run = tokio::task::JoinHandle<Result<(), StageError>>;

    fn spawn(
        params: serde_json::Value,
    ) -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Event, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(64);
        let h = tokio::spawn(async move {
            AprsDecodeStage::new()
                .run(
                    params,
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                    zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
                )
                .await
        });
        (tx, rx_out, h)
    }

    #[tokio::test]
    async fn aprs_stage_emits_event_for_packet_audio() {
        let (tx, mut rx_out, h) = spawn(serde_json::json!({}));
        let frame = ax25_ui_frame("VK3ABC", "APRS", &["WIDE1-1"], b">hi");
        let mut audio = vec![0.0f32; 4_800];
        audio.extend(afsk1200_modulate(
            &nrzi_encode(&hdlc_frame_bits(&frame, 32, 4)),
            48_000,
        ));
        audio.extend(vec![0.0f32; 4_800]);
        for chunk in audio.chunks(4_800) {
            tx.send(audio_frame(chunk.to_vec(), 48_000)).unwrap();
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.kind, StreamKind::Event);
        assert_eq!(f.provenance.source, SourceId("s".into()));
        let Payload::Event(v) = &f.payload else {
            panic!("expected an event")
        };
        assert_eq!(
            *v,
            serde_json::json!({
                "protocol": "aprs", "src": "VK3ABC", "dst": "APRS", "path": ["WIDE1-1"],
                "info": ">hi", "info_hex": "3e6869", "fcs_ok": true,
            })
        );
        assert!(rx_out.try_recv().is_err(), "exactly one event");
    }

    #[tokio::test]
    async fn aprs_stage_rejects_rate_change_and_non_audio() {
        let (tx, _rx, h) = spawn(serde_json::json!({}));
        tx.send(audio_frame(vec![0.0; 480], 48_000)).unwrap();
        tx.send(audio_frame(vec![0.0; 480], 44_100)).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => {
                assert_eq!(m, "sample_rate_hz changed from 48000 to 44100")
            }
            r => panic!("expected Failed, got {r:?}"),
        }
        let (tx, _rx, h) = spawn(serde_json::json!({}));
        tx.send(Frame::new(
            StreamId::new(),
            prov(),
            Payload::Event(serde_json::json!({})),
        ))
        .unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert_eq!(m, "aprs_decode expects audio frames"),
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    #[test]
    fn params_are_strict() {
        let s = AprsDecodeStage::new();
        assert!(serde_json::from_value::<AprsParams>(serde_json::json!({})).is_ok());
        assert!(serde_json::from_value::<AprsParams>(serde_json::json!({"x": 1})).is_err());
        let _ = s.spec();
    }

    fn packet_audio(src: &str) -> Vec<f32> {
        let frame = ax25_ui_frame(src, "APRS", &[], b">x");
        afsk1200_modulate(&nrzi_encode(&hdlc_frame_bits(&frame, 32, 4)), 48_000)
    }

    #[tokio::test]
    async fn aprs_lag_mid_packet_discards_partial() {
        let (tx, mut rx_out, h) = spawn(serde_json::json!({}));
        // Packet AAA is cut in two by a lag; its second half must not complete it.
        let a = packet_audio("VK3AAA");
        let (a1, a2) = a.split_at(a.len() * 2 / 3);
        tx.send(audio_frame(a1.to_vec(), 48_000)).unwrap();
        // Let the stage consume the first half before the flood.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Capacity 64: one overwritten frame, then AAA's tail, silence, packet BBB.
        tx.send(audio_frame(vec![0.0; 480], 48_000)).unwrap();
        tx.send(audio_frame(a2.to_vec(), 48_000)).unwrap();
        for _ in 0..62 {
            tx.send(audio_frame(vec![0.0; 480], 48_000)).unwrap();
        }
        tx.send(audio_frame(packet_audio("VK3BBB"), 48_000))
            .unwrap();
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let f = rx_out.recv().await.unwrap();
        let Payload::Event(v) = &f.payload else {
            panic!("expected an event")
        };
        assert_eq!(v["src"], "VK3BBB");
        assert!(rx_out.try_recv().is_err(), "the cut packet yields nothing");
    }
}
