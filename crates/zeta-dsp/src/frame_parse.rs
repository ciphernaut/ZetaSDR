//! `frame` events read by a named protocol parser.
//!
//! Reads `frame_decode`'s `frame` events and publishes one event per frame the
//! parser accepts (`protocol` the parser's name, its fields, `t_ns`, `summary`), in the
//! job's mode-row envelope when it runs a row. A frame the parser refuses is counted,
//! not published. Every other event passes through. The parsers are pure
//! bytes-to-fields decoders of clear, standards-based framing behind each protocol's
//! own check; nothing here decrypts, targets a link or transmits.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::util::event;
use crate::{Input, run_map};

/// The stage kind, as graphs name it.
pub const FRAME_PARSE_KIND: &str = "frame_parse";

/// The parsers a graph can name.
pub const PARSERS: &[&str] = &["modbus_rtu", "dnp3", "iec101", "iec104"];

/// `frame_parse`'s parameters. The parser depends on the signal's protocol, so it is
/// required.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameParseParams {
    /// One of `modbus_rtu`, `dnp3`, `iec101`, `iec104`.
    pub parser: String,
}

impl FrameParseParams {
    fn check(&self) -> Result<(), String> {
        if PARSERS.contains(&self.parser.as_str()) {
            Ok(())
        } else {
            Err(format!("parser: one of {}", PARSERS.join(", ")))
        }
    }
}

/// The parsers whose protocol starts its frame with the bytes a framing takes as its
/// sync word (`05 64`, `68`): `frame_decode`'s `bytes` hold the frame after the sync, so
/// these parsers are handed the sync word and the bytes together. Modbus RTU has no
/// start delimiter; a row's sync there is the modem's, not part of the frame.
const SYNC_LEADS_FRAME: &[&str] = &["dnp3", "iec101", "iec104"];

/// The bytes of a hex string, or `None` when it is not whole hex octets.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Reads `frame` events and publishes the fields of those the parser in its params
/// accepts, noting `frame_parse` (`parsed`, `rejected`, `last_error`) after each
/// frame. Frames are consumed; every other event is forwarded unchanged.
pub struct FrameParseStage {
    spec: StageSpec,
}

impl FrameParseStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: FRAME_PARSE_KIND,
                inputs: vec![StreamKind::Event],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(FrameParseParams),
            },
        }
    }
}

impl Default for FrameParseStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for FrameParseStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: FrameParseParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        p.check()
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        self.check_params(&params).map_err(StageError::Failed)?;
        let p: FrameParseParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{FRAME_PARSE_KIND} params: {e}")))?;
        let [events] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        let (mut parsed, mut rejected) = (0u64, 0u64);
        let mut last_error: Option<String> = None;
        run_map(FRAME_PARSE_KIND, inputs, bus, cancel, move |input| {
            let Input::Frame(frame) = input else {
                return Ok(Vec::new());
            };
            let Payload::Event(ev) = &frame.payload else {
                return Ok(Vec::new());
            };
            if !is_frame_event(ev) {
                return Ok(vec![Frame::new(
                    events.clone(),
                    frame.provenance.clone(),
                    frame.payload.clone(),
                )]);
            }
            let result = ev["bytes"]
                .as_str()
                .and_then(unhex)
                .ok_or_else(|| "bytes".to_string())
                .and_then(|mut b| {
                    if SYNC_LEADS_FRAME.contains(&p.parser.as_str()) {
                        let mut sync = ev["sync"].as_str().and_then(unhex).ok_or("sync")?;
                        sync.append(&mut b);
                        b = sync;
                    }
                    crate::parsers::parse(&p.parser, &b)
                });
            let out = match result {
                Ok(mut fields) => {
                    parsed += 1;
                    let t_ns = ev["t_ns"].as_u64().unwrap_or(0);
                    fields["t_ns"] = t_ns.into();
                    let payload = event(&ctx, t_ns, &p.parser, fields);
                    vec![Frame::new(
                        events.clone(),
                        frame.provenance.clone(),
                        Payload::Event(payload),
                    )]
                }
                Err(e) => {
                    rejected += 1;
                    last_error = Some(e);
                    Vec::new()
                }
            };
            ctx.note(serde_json::json!({FRAME_PARSE_KIND: {
                "parsed": parsed,
                "rejected": rejected,
                "last_error": last_error,
            }}));
            Ok(out)
        })
        .await
    }
}

/// Whether `ev` is one of `frame_decode`'s frame events. In a mode row's envelope its
/// `protocol` is the row's, not `frame`, so it is told by the fields only a frame carries
/// (`bits` and `segment` events have none of them).
fn is_frame_event(ev: &serde_json::Value) -> bool {
    ev["bytes"].is_string() && ev["sync"].is_string() && ev["length_bytes"].is_u64()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use zeta_core::stage::Stage;
    use zeta_core::stream::{Frame, Payload};
    use zeta_core::types::{JobId, Provenance, SourceId, Span, StreamId, Timestamp};

    use super::FrameParseStage;
    use crate::bit_view::test_streams::run_events;
    use crate::parsers::hex;
    use crate::parsers::vectors::modbus;

    fn event_frame(ev: Value) -> Frame {
        let t = ev["t_ns"].as_u64().unwrap_or(0);
        Frame::new(
            StreamId::new(),
            Provenance {
                source: SourceId("s".into()),
                job: Some(JobId("job".into())),
                channel: None,
                span: Span {
                    start_ns: Timestamp(t),
                    end_ns: Timestamp(t),
                },
            },
            Payload::Event(ev),
        )
    }

    fn frame_event(bytes: &[u8], t_ns: u64) -> Frame {
        event_frame(json!({"protocol": "frame", "t_ns": t_ns, "sync": "00",
            "length_bytes": bytes.len(), "bytes": hex(bytes), "crc_ok": null,
            "payload": "opaque", "summary": "frame"}))
    }

    #[tokio::test]
    async fn frame_parse_publishes_parsed_frames_in_the_parser_protocol_and_counts_rejects() {
        let good = modbus();
        let mut bad = good.clone();
        bad[7] ^= 1;
        let other = json!({"protocol": "bits", "t_ns": 5});
        let (events, context) = run_events(
            &FrameParseStage::new(),
            json!({"parser": "modbus_rtu"}),
            vec![
                frame_event(&bad, 10),
                event_frame(other.clone()),
                frame_event(&good, 20),
            ],
        )
        .await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0], other);
        let e = &events[1];
        assert_eq!(e["protocol"], "modbus_rtu");
        assert_eq!(e["address"], 1);
        assert_eq!(e["function"], 3);
        assert_eq!(e["data"], "0000000a");
        assert_eq!(e["crc_ok"], true);
        assert_eq!(e["t_ns"], 20);
        assert_eq!(e["summary"], "modbus 1 fn 3");
        let note = &context["frame_parse"];
        assert_eq!(note["parsed"], 1);
        assert_eq!(note["rejected"], 1);
        assert_eq!(note["last_error"], "crc");
    }

    /// A frame event in a row's envelope carries the row's `protocol`, yet is parsed.
    #[tokio::test]
    async fn a_frame_in_a_rows_envelope_is_still_parsed() {
        let mut f = frame_event(&modbus(), 10);
        if let Payload::Event(v) = &mut f.payload {
            v["protocol"] = "my_row".into();
        }
        let (events, _) = run_events(
            &FrameParseStage::new(),
            json!({"parser": "modbus_rtu"}),
            vec![f],
        )
        .await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["address"], 1);
    }

    /// The sync word is the start of a DNP3 or IEC frame: the parser gets sync and bytes.
    #[tokio::test]
    async fn a_sync_led_protocol_is_parsed_from_the_sync_and_the_bytes() {
        let frame = crate::parsers::vectors::dnp3();
        let ev = json!({"protocol": "frame", "t_ns": 7, "sync": hex(&frame[..2]),
            "length_bytes": frame.len() - 2, "bytes": hex(&frame[2..]),
            "crc_ok": null, "payload": "opaque", "summary": "frame"});
        let (events, _) = run_events(
            &FrameParseStage::new(),
            json!({"parser": "dnp3"}),
            vec![event_frame(ev)],
        )
        .await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["protocol"], "dnp3");
        assert_eq!(events[0]["destination"], 1);
    }

    #[test]
    fn an_unknown_parser_is_refused() {
        let s = FrameParseStage::new();
        assert_eq!(
            s.check_params(&json!({"parser": "x"})),
            Err("parser: one of modbus_rtu, dnp3, iec101, iec104".to_string())
        );
        assert_eq!(s.check_params(&json!({"parser": "dnp3"})), Ok(()));
        assert!(s.check_params(&json!({})).is_err());
        assert!(
            s.check_params(&json!({"parser": "dnp3", "extra": 1}))
                .is_err()
        );
    }
}
