//! Plugin protocol 1: the byte stream between the host and an external stage. Pure codec,
//! no processes.
//!
//! A child process speaks it on stdin (host to plugin) and stdout (plugin to host); stderr
//! is free text that the host copies to its own stderr prefixed `[plugin <kind>] `. Each
//! message begins with one byte:
//!
//! - `0x7B` (`{`): a **control message**, one compact UTF-8 JSON object ending in `0x0A`, at
//!   most 65 536 bytes including the newline.
//! - `0x00`: a **data frame**: `u32` little-endian header length `H` (1..=65 536), `H` bytes
//!   of UTF-8 JSON header, `u32` little-endian body length `B` (0..=16 777 216), `B` body
//!   bytes.
//! - any other byte: protocol error `unexpected byte 0x<hh> at message start`.
//!
//! Control messages are tagged by `"type"` (snake_case), the tag the websocket's
//! `WsMessage` uses. Plugin to host: `{"type":"hello","protocol":1,"kind":"<k>",
//! "inputs":["iq"],"outputs":["iq"],"params_schema":{…}}` (the first message, within
//! 5000 ms of spawn) and `{"type":"error","message":"<m>"}`. Host to plugin:
//! `{"type":"run","protocol":1,"params":{…},"inputs":[{"kind":"iq"}],
//! "outputs":[{"stream":"<id>","kind":"iq"}]}`, `{"type":"lagged","missed":N}` and
//! `{"type":"closed","reason":"<r>"}` (byte-identical to the websocket's), and
//! `{"type":"stop"}`. Unknown fields are refused.
//!
//! A data frame's header is exactly the websocket's frame message:
//! `{"type":"frame","stream","kind","source","job","channel","start_ns","end_ns","payload"}`
//! with `payload` the frame summary for the kind. Body by kind:
//! - `iq`: payload `{"sample_rate_hz","center_hz","samples":N}`, body `N × 8` bytes,
//!   interleaved `f32` little-endian I then Q (SigMF `cf32_le`); the block's `t0` is
//!   `start_ns`.
//! - `audio`: payload `{"sample_rate_hz","samples":N,"peak"}`, body `N × 4` bytes `f32` LE.
//! - `event`: payload is the event, `B = 0`.
//! - `text`: payload is the string, `B = 0`.
//! - `video` and `sink` are refused in protocol 1.
//!
//! `B` must match `N`. Every length is checked before anything is allocated or indexed: a
//! malformed message is a [`WireError`] naming what was wrong, never a panic.

use std::sync::Arc;

use num_complex::Complex;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use zeta_core::ops::FrameSummary;
use zeta_core::port::IqBlock;
use zeta_core::stream::{Frame, Payload};
use zeta_core::types::{Hz, Provenance, Span, StreamId, StreamKind};

/// The protocol version a `hello` and a `run` carry.
pub const PROTOCOL: u32 = 1;
/// Longest control message, newline included, in bytes.
pub const MAX_CONTROL: usize = 65_536;
/// Longest data-frame header, in bytes.
pub const MAX_HEADER: usize = 65_536;
/// Longest data-frame body, in bytes.
pub const MAX_BODY: usize = 16_777_216;
/// First byte of a control message (`{`, the start of its JSON).
pub const CONTROL_START: u8 = 0x7B;
/// First byte of a data frame.
pub const FRAME_START: u8 = 0x00;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WireError {
    #[error("unexpected byte 0x{0:02x} at message start")]
    UnexpectedByte(u8),
    /// `len` is the declared length, or for a control line without its newline the bytes
    /// read so far (at least `max + 1`).
    #[error("{what} of {len} bytes exceeds the {max}-byte limit")]
    TooLong {
        what: &'static str,
        len: usize,
        max: usize,
    },
    /// EOF inside a message. `what` is `"header length"` or `"body length"` (the 4-byte
    /// prefixes), `"header"`, `"body"`, or `"control line"` (whose `want` is `got + 1`: at
    /// least the newline).
    #[error("stream ended inside the {what}: {got} of {want} bytes")]
    Truncated {
        what: &'static str,
        got: usize,
        want: usize,
    },
    #[error("{} frame of {samples} samples has a {bytes}-byte body", kind_name(*kind))]
    BodyMismatch {
        kind: StreamKind,
        samples: usize,
        bytes: usize,
    },
    #[error("{} frames are not carried by plugin protocol 1", kind_name(*.0))]
    KindNotCarried(StreamKind),
    #[error("bad json: {0}")]
    Json(String),
    #[error("i/o: {0}")]
    Io(String),
}

/// A kind's wire name (`iq`, `audio`, ...).
fn kind_name(k: StreamKind) -> &'static str {
    match k {
        StreamKind::Iq => "iq",
        StreamKind::Audio => "audio",
        StreamKind::Video => "video",
        StreamKind::Text => "text",
        StreamKind::Event => "event",
        StreamKind::Sink => "sink",
    }
}

/// One control message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields,
    from = "ControlIn"
)]
pub enum Control {
    /// Plugin to host, first: what the plugin is.
    Hello(Hello),
    /// Plugin to host: the plugin failed.
    Error { message: String },
    /// Host to plugin: start processing with these params and streams.
    Run(Run),
    /// Host to plugin: `missed` input frames were dropped before reaching it.
    Lagged { missed: u64 },
    /// Host to plugin: the input closed; finish and exit.
    Closed { reason: String },
    /// Host to plugin: stop now.
    Stop,
}

/// [`Control`] as read: serde ignores extra fields on a unit variant of an internally
/// tagged enum, so `stop` is read as an empty struct variant, which refuses them.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ControlIn {
    Hello(Hello),
    Error { message: String },
    Run(Run),
    Lagged { missed: u64 },
    Closed { reason: String },
    Stop {},
}

impl From<ControlIn> for Control {
    fn from(c: ControlIn) -> Self {
        match c {
            ControlIn::Hello(h) => Control::Hello(h),
            ControlIn::Error { message } => Control::Error { message },
            ControlIn::Run(r) => Control::Run(r),
            ControlIn::Lagged { missed } => Control::Lagged { missed },
            ControlIn::Closed { reason } => Control::Closed { reason },
            ControlIn::Stop {} => Control::Stop,
        }
    }
}

/// A plugin's self-description: a `StageSpec` as JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol: u32,
    pub kind: String,
    pub inputs: Vec<StreamKind>,
    pub outputs: Vec<StreamKind>,
    /// Must pass [`check_strict_schema`] or the host refuses the plugin.
    pub params_schema: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub protocol: u32,
    pub params: serde_json::Value,
    pub inputs: Vec<PortKind>,
    pub outputs: Vec<OutPort>,
}

/// One input port of a `run`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortKind {
    pub kind: StreamKind,
}

/// One output port of a `run`: the stream the plugin's frames for it must name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutPort {
    pub stream: StreamId,
    pub kind: StreamKind,
}

/// One decoded message.
#[derive(Clone, Debug)]
pub enum Message {
    Control(Control),
    Frame(Frame),
}

/// Encode a control message: compact JSON and `\n`. Compact JSON escapes every newline
/// inside strings, so the only `0x0A` is the terminator. Length is not checked here: a
/// host-built `run` whose params exceed [`MAX_CONTROL`] will be refused by the plugin.
pub fn encode_control(c: &Control) -> Vec<u8> {
    // Serialising these types cannot fail: every map key is a string.
    let mut line = serde_json::to_vec(c).expect("control messages always serialise");
    line.push(b'\n');
    line
}

/// The data-frame header: the websocket's `WsMessage::Frame`, built from the same
/// `FrameSummary`, so its JSON is the same bytes.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Header {
    Frame(FrameSummary),
}

/// An `iq` header's payload.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IqSummary {
    sample_rate_hz: u64,
    center_hz: u64,
    samples: usize,
}

/// An `audio` header's payload. `peak` is required but informational: it is not checked
/// against the body (it is `null` when a sample is NaN).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioSummary {
    sample_rate_hz: u64,
    samples: usize,
    #[allow(dead_code)]
    peak: Option<f64>,
}

fn json_err(e: serde_json::Error) -> WireError {
    WireError::Json(e.to_string())
}

fn io_err(e: std::io::Error) -> WireError {
    WireError::Io(e.to_string())
}

/// Encode a data frame. The header's `kind` is the payload's; `video` and `sink` are
/// refused, as are a header or body over their limits. An iq block's `t0` is not sent:
/// the receiver takes it from `start_ns`.
pub fn encode_frame(f: &Frame) -> Result<Vec<u8>, WireError> {
    let kind = f.payload.kind();
    let body: Vec<u8> = match &f.payload {
        Payload::Iq(b) => b
            .samples
            .iter()
            .flat_map(|c| {
                let (i, q) = (c.re.to_le_bytes(), c.im.to_le_bytes());
                [i[0], i[1], i[2], i[3], q[0], q[1], q[2], q[3]]
            })
            .collect(),
        Payload::Audio { samples, .. } => samples.iter().flat_map(|s| s.to_le_bytes()).collect(),
        Payload::Text(_) | Payload::Event(_) => Vec::new(),
        Payload::Video | Payload::Sink(_) => return Err(WireError::KindNotCarried(kind)),
    };
    let mut summary = FrameSummary::from_frame(f);
    summary.kind = kind;
    let header = serde_json::to_vec(&Header::Frame(summary)).map_err(json_err)?;
    if header.len() > MAX_HEADER {
        return Err(WireError::TooLong {
            what: "header",
            len: header.len(),
            max: MAX_HEADER,
        });
    }
    if body.len() > MAX_BODY {
        return Err(WireError::TooLong {
            what: "body",
            len: body.len(),
            max: MAX_BODY,
        });
    }
    let mut out = Vec::with_capacity(9 + header.len() + body.len());
    out.push(FRAME_START);
    // Both lengths are within their limits, far below u32::MAX.
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Read into `buf` until it is full or the stream ends; the bytes read.
async fn read_full<R: AsyncRead + Unpin>(r: &mut R, buf: &mut [u8]) -> Result<usize, WireError> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]).await {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_err(e)),
        }
    }
    Ok(got)
}

/// Fill `buf` or fail with `Truncated { what, got, want: buf.len() }`.
async fn read_exact<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
    what: &'static str,
) -> Result<(), WireError> {
    let got = read_full(r, buf).await?;
    if got < buf.len() {
        return Err(WireError::Truncated {
            what,
            got,
            want: buf.len(),
        });
    }
    Ok(())
}

async fn read_u32<R: AsyncRead + Unpin>(r: &mut R, what: &'static str) -> Result<usize, WireError> {
    let mut b = [0u8; 4];
    read_exact(r, &mut b, what).await?;
    Ok(u32::from_le_bytes(b) as usize)
}

/// Read one message. `Ok(None)` only on EOF at a message boundary; EOF anywhere inside a
/// message is [`WireError::Truncated`]. Control lines are read a byte at a time, so give it
/// a buffered reader (`tokio::io::BufReader`) over a pipe. Lengths are checked before any
/// buffer is allocated, so at most [`MAX_HEADER`] + [`MAX_BODY`] bytes are held per frame.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Message>, WireError> {
    let mut first = [0u8; 1];
    if read_full(r, &mut first).await? == 0 {
        return Ok(None);
    }
    match first[0] {
        CONTROL_START => read_control(r).await.map(|c| Some(Message::Control(c))),
        FRAME_START => read_frame(r).await.map(|f| Some(Message::Frame(f))),
        b => Err(WireError::UnexpectedByte(b)),
    }
}

/// The rest of a control line, whose `{` has been read.
async fn read_control<R: AsyncRead + Unpin>(r: &mut R) -> Result<Control, WireError> {
    let mut line = vec![CONTROL_START];
    loop {
        let mut b = [0u8; 1];
        if read_full(r, &mut b).await? == 0 {
            return Err(WireError::Truncated {
                what: "control line",
                got: line.len(),
                want: line.len() + 1,
            });
        }
        if b[0] == b'\n' {
            break;
        }
        if line.len() == MAX_CONTROL {
            // MAX_CONTROL bytes and still no newline: at least one byte too long.
            return Err(WireError::TooLong {
                what: "control line",
                len: MAX_CONTROL + 1,
                max: MAX_CONTROL,
            });
        }
        line.push(b[0]);
    }
    serde_json::from_slice(&line).map_err(json_err)
}

/// The rest of a data frame, whose `0x00` has been read.
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Frame, WireError> {
    let h = read_u32(r, "header length").await?;
    if h > MAX_HEADER {
        return Err(WireError::TooLong {
            what: "header",
            len: h,
            max: MAX_HEADER,
        });
    }
    if h == 0 {
        return Err(WireError::Json("frame header is empty".into()));
    }
    let mut header = vec![0u8; h];
    read_exact(r, &mut header, "header").await?;
    let Header::Frame(s) = serde_json::from_slice(&header).map_err(json_err)?;

    // What the header says the body holds, checked before the body length is read.
    enum Shape {
        Iq(IqSummary),
        Audio(AudioSummary),
        Text(String),
        Event(serde_json::Value),
    }
    let (shape, samples, width) = match s.kind {
        StreamKind::Iq => {
            let p: IqSummary = serde_json::from_value(s.payload).map_err(json_err)?;
            let n = p.samples;
            (Shape::Iq(p), n, 8usize)
        }
        StreamKind::Audio => {
            let p: AudioSummary = serde_json::from_value(s.payload).map_err(json_err)?;
            let n = p.samples;
            (Shape::Audio(p), n, 4)
        }
        StreamKind::Text => {
            let t: String = serde_json::from_value(s.payload).map_err(json_err)?;
            (Shape::Text(t), 0, 0)
        }
        StreamKind::Event => (Shape::Event(s.payload), 0, 0),
        k @ (StreamKind::Video | StreamKind::Sink) => return Err(WireError::KindNotCarried(k)),
    };

    let b = read_u32(r, "body length").await?;
    if b > MAX_BODY {
        return Err(WireError::TooLong {
            what: "body",
            len: b,
            max: MAX_BODY,
        });
    }
    if samples.checked_mul(width) != Some(b) {
        return Err(WireError::BodyMismatch {
            kind: s.kind,
            samples,
            bytes: b,
        });
    }
    let mut body = vec![0u8; b];
    read_exact(r, &mut body, "body").await?;

    let payload = match shape {
        Shape::Iq(p) => Payload::Iq(IqBlock {
            t0: s.start_ns,
            sample_rate_hz: Hz(p.sample_rate_hz),
            center_hz: Hz(p.center_hz),
            samples: body
                .chunks_exact(8)
                .map(|c| {
                    Complex::new(
                        f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                        f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                    )
                })
                .collect::<Arc<[_]>>(),
        }),
        Shape::Audio(p) => Payload::Audio {
            sample_rate_hz: Hz(p.sample_rate_hz),
            samples: body
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        },
        Shape::Text(t) => Payload::Text(t),
        Shape::Event(v) => Payload::Event(v),
    };
    Ok(Frame::new(
        s.stream,
        Provenance {
            source: s.source,
            job: s.job,
            channel: s.channel,
            span: Span {
                start_ns: s.start_ns,
                end_ns: s.end_ns,
            },
        },
        payload,
    ))
}

/// `Ok` when `schema` is strict enough to be a plugin's `params_schema`: an object schema
/// (`"type": "object"`) that forbids extra fields, and every object schema with
/// `properties` anywhere inside it (including `$defs`) has `additionalProperties: false`
/// and requires every property. Otherwise the reason, naming the path.
pub fn check_strict_schema(schema: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    let Some(top) = schema.as_object() else {
        return Err("params_schema is not an object schema".into());
    };
    if top.get("type") != Some(&Value::String("object".into())) {
        return Err("params_schema: \"type\" is not \"object\"".into());
    }
    if top.get("additionalProperties") != Some(&Value::Bool(false)) {
        return Err("params_schema allows extra fields: additionalProperties is not false".into());
    }
    fn walk(v: &Value, path: &str) -> Result<(), String> {
        match v {
            Value::Object(m) => {
                if let Some(Value::Object(props)) = m.get("properties") {
                    if m.get("additionalProperties") != Some(&Value::Bool(false)) {
                        return Err(format!(
                            "{path} allows extra fields: additionalProperties is not false"
                        ));
                    }
                    let mut want: Vec<&str> = props.keys().map(String::as_str).collect();
                    let mut req: Vec<&str> = m
                        .get("required")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    want.sort_unstable();
                    req.sort_unstable();
                    req.dedup();
                    if req != want {
                        return Err(format!("{path} does not list every property as required"));
                    }
                }
                m.iter()
                    .try_for_each(|(k, c)| walk(c, &format!("{path}.{k}")))
            }
            Value::Array(a) => a
                .iter()
                .enumerate()
                .try_for_each(|(i, c)| walk(c, &format!("{path}[{i}]"))),
            _ => Ok(()),
        }
    }
    walk(schema, "params_schema")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use zeta_core::types::{ChannelId, JobId, SourceId, Timestamp};

    use super::*;

    fn frame(payload: Payload) -> Frame {
        Frame::new(
            StreamId("s1".into()),
            Provenance {
                source: SourceId("src".into()),
                job: Some(JobId("j1".into())),
                channel: Some(ChannelId("c1".into())),
                span: Span {
                    start_ns: Timestamp(5_000_000_000),
                    end_ns: Timestamp(5_000_041_666),
                },
            },
            payload,
        )
    }

    fn iq(samples: Vec<Complex<f32>>) -> Payload {
        Payload::Iq(IqBlock {
            t0: Timestamp(5_000_000_000),
            sample_rate_hz: Hz(48_000),
            center_hz: Hz(100_000_000),
            samples: samples.into(),
        })
    }

    /// A raw data frame: `0x00`, `H`, header JSON, `B`, then `body` (which may be shorter
    /// than `B`).
    fn raw_frame(header: &Value, b: u32, body: &[u8]) -> Vec<u8> {
        let h = serde_json::to_vec(header).unwrap();
        let mut out = vec![0x00];
        out.extend_from_slice(&(h.len() as u32).to_le_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(&b.to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn iq_header(samples: usize) -> Value {
        json!({"type":"frame","stream":"s1","kind":"iq","source":"src","job":null,
               "channel":null,"start_ns":7,"end_ns":8,
               "payload":{"sample_rate_hz":48000,"center_hz":100000000,"samples":samples}})
    }

    async fn read_all(mut bytes: &[u8]) -> Result<Option<Message>, WireError> {
        read_message(&mut bytes).await
    }

    fn header_of(bytes: &[u8]) -> (Value, &[u8]) {
        let h = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let header = serde_json::from_slice(&bytes[5..5 + h]).unwrap();
        let b = u32::from_le_bytes(bytes[5 + h..9 + h].try_into().unwrap()) as usize;
        let body = &bytes[9 + h..];
        assert_eq!(body.len(), b);
        (header, body)
    }

    #[tokio::test]
    async fn iq_frame_roundtrips_bytes_exactly() {
        let f = frame(iq(vec![Complex::new(1.0, 2.0), Complex::new(-0.5, 0.25)]));
        let bytes = encode_frame(&f).unwrap();
        assert_eq!(bytes[0], 0x00);
        let (header, body) = header_of(&bytes);
        assert_eq!(
            header,
            json!({"type":"frame","stream":"s1","kind":"iq","source":"src","job":"j1",
                   "channel":"c1","start_ns":5_000_000_000u64,"end_ns":5_000_041_666u64,
                   "payload":{"sample_rate_hz":48000,"center_hz":100000000,"samples":2}})
        );
        let want: Vec<u8> = [1.0f32, 2.0, -0.5, 0.25]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(body, &want[..]);

        let Some(Message::Frame(back)) = read_all(&bytes).await.unwrap() else {
            panic!("expected a frame");
        };
        assert_eq!(back.stream, f.stream);
        assert_eq!(back.kind, StreamKind::Iq);
        assert_eq!(back.provenance, f.provenance);
        let (Payload::Iq(a), Payload::Iq(b)) = (&back.payload, &f.payload) else {
            panic!("expected iq payloads");
        };
        assert_eq!(a.t0, b.t0);
        assert_eq!(a.sample_rate_hz, b.sample_rate_hz);
        assert_eq!(a.center_hz, b.center_hz);
        assert_eq!(a.samples, b.samples);
        // Re-encoding the decoded frame gives the same bytes.
        assert_eq!(encode_frame(&back).unwrap(), bytes);
    }

    #[tokio::test]
    async fn audio_event_text_roundtrip() {
        let audio = frame(Payload::Audio {
            sample_rate_hz: Hz(48_000),
            samples: Arc::from(vec![0.5f32, -0.75, 0.125]),
        });
        let bytes = encode_frame(&audio).unwrap();
        let (header, body) = header_of(&bytes);
        assert_eq!(
            header["payload"],
            json!({"sample_rate_hz":48000,"samples":3,"peak":0.75})
        );
        assert_eq!(body.len(), 12);
        assert_eq!(&body[4..8], &(-0.75f32).to_le_bytes());
        let Some(Message::Frame(back)) = read_all(&bytes).await.unwrap() else {
            panic!("expected a frame");
        };
        let Payload::Audio {
            sample_rate_hz,
            samples,
        } = &back.payload
        else {
            panic!("expected audio");
        };
        assert_eq!(*sample_rate_hz, Hz(48_000));
        assert_eq!(&samples[..], &[0.5f32, -0.75, 0.125]);
        assert_eq!(back.provenance, audio.provenance);

        let event = frame(Payload::Event(json!({"label":"fsk","snr_db":12.5})));
        let bytes = encode_frame(&event).unwrap();
        let (header, body) = header_of(&bytes);
        assert_eq!(header["kind"], "event");
        assert_eq!(header["payload"], json!({"label":"fsk","snr_db":12.5}));
        assert!(body.is_empty());
        let Some(Message::Frame(back)) = read_all(&bytes).await.unwrap() else {
            panic!("expected a frame");
        };
        assert!(matches!(&back.payload, Payload::Event(v) if v["label"] == "fsk"));
        assert_eq!(back.kind, StreamKind::Event);

        let text = frame(Payload::Text("CQ CQ DE VK2".into()));
        let bytes = encode_frame(&text).unwrap();
        let (header, body) = header_of(&bytes);
        assert_eq!(header["payload"], "CQ CQ DE VK2");
        assert!(body.is_empty());
        let Some(Message::Frame(back)) = read_all(&bytes).await.unwrap() else {
            panic!("expected a frame");
        };
        assert!(matches!(&back.payload, Payload::Text(t) if t == "CQ CQ DE VK2"));
        assert_eq!(back.stream, text.stream);
    }

    #[tokio::test]
    async fn control_line_roundtrips() {
        assert_eq!(
            encode_control(&Control::Lagged { missed: 3 }),
            b"{\"type\":\"lagged\",\"missed\":3}\n"
        );
        assert_eq!(encode_control(&Control::Stop), b"{\"type\":\"stop\"}\n");
        let all = [
            Control::Hello(Hello {
                protocol: PROTOCOL,
                kind: "fake".into(),
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Iq, StreamKind::Event],
                params_schema: json!({"type":"object","additionalProperties":false,
                                      "properties":{"gain":{"type":"number"}},
                                      "required":["gain"]}),
            }),
            Control::Error {
                message: "boom\nwith a newline".into(),
            },
            Control::Run(Run {
                protocol: PROTOCOL,
                params: json!({"gain":2.0}),
                inputs: vec![PortKind {
                    kind: StreamKind::Iq,
                }],
                outputs: vec![OutPort {
                    stream: StreamId("out1".into()),
                    kind: StreamKind::Iq,
                }],
            }),
            Control::Lagged { missed: 3 },
            Control::Closed {
                reason: "input closed".into(),
            },
            Control::Stop,
        ];
        let mut stream = Vec::new();
        for c in &all {
            let line = encode_control(c);
            assert_eq!(line[0], b'{');
            assert_eq!(line.last(), Some(&b'\n'));
            assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1, "one line");
            stream.extend_from_slice(&line);
        }
        let run = encode_control(&all[2]);
        let v: Value = serde_json::from_slice(&run).unwrap();
        assert_eq!(
            v,
            json!({"type":"run","protocol":1,"params":{"gain":2.0},"inputs":[{"kind":"iq"}],
                   "outputs":[{"stream":"out1","kind":"iq"}]})
        );

        // Back to back on one stream, then a clean EOF at a message boundary.
        let mut r = &stream[..];
        for c in &all {
            let Some(Message::Control(back)) = read_message(&mut r).await.unwrap() else {
                panic!("expected a control message");
            };
            assert_eq!(&back, c);
        }
        assert!(read_message(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn control_unknown_fields_and_types_are_refused() {
        for bad in [
            &b"{\"type\":\"stop\",\"now\":true}\n"[..],
            b"{\"type\":\"lagged\",\"missed\":3,\"extra\":1}\n",
            b"{\"type\":\"reboot\"}\n",
            b"{\"type\":\"hello\",\"protocol\":1,\"kind\":\"k\",\"inputs\":[],\"outputs\":[],\"params_schema\":{},\"x\":1}\n",
            b"{not json}\n",
            b"{\"type\":\"stop\"} trailing\n",
        ] {
            let err = read_all(bad).await.unwrap_err();
            assert!(matches!(err, WireError::Json(_)), "{err:?}");
        }
    }

    #[tokio::test]
    async fn truncated_body_reports_bytes_read() {
        let bytes = raw_frame(&iq_header(1000), 8000, &[0u8; 12]);
        let err = read_all(&bytes).await.unwrap_err();
        assert_eq!(
            err,
            WireError::Truncated {
                what: "body",
                got: 12,
                want: 8000
            }
        );
        let msg = err.to_string();
        assert!(
            msg.contains("body") && msg.contains("12") && msg.contains("8000"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn truncation_anywhere_is_an_error_not_a_panic() {
        let full = encode_frame(&frame(iq(vec![Complex::new(1.0, 2.0); 4]))).unwrap();
        // Every proper prefix past the first byte is a truncation.
        for n in 1..full.len() {
            let err = read_all(&full[..n]).await.unwrap_err();
            assert!(matches!(err, WireError::Truncated { .. }), "{n}: {err:?}");
        }
        let err = read_all(&full[..3]).await.unwrap_err();
        assert_eq!(
            err,
            WireError::Truncated {
                what: "header length",
                got: 2,
                want: 4
            }
        );
        let err = read_all(b"{\"type\":\"st").await.unwrap_err();
        assert_eq!(
            err,
            WireError::Truncated {
                what: "control line",
                got: 11,
                want: 12
            }
        );
        assert!(read_all(b"").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn limits_and_bad_starts_are_errors() {
        let mut big = vec![0x00];
        big.extend_from_slice(&70_000u32.to_le_bytes());
        assert_eq!(
            read_all(&big).await.unwrap_err(),
            WireError::TooLong {
                what: "header",
                len: 70_000,
                max: MAX_HEADER
            }
        );

        let err = read_all(b"A{}\n").await.unwrap_err();
        assert_eq!(err, WireError::UnexpectedByte(0x41));
        assert_eq!(err.to_string(), "unexpected byte 0x41 at message start");

        let bytes = raw_frame(&iq_header(3), 16, &[0u8; 16]);
        assert_eq!(
            read_all(&bytes).await.unwrap_err(),
            WireError::BodyMismatch {
                kind: StreamKind::Iq,
                samples: 3,
                bytes: 16
            }
        );

        assert_eq!(
            encode_frame(&frame(Payload::Video)).unwrap_err(),
            WireError::KindNotCarried(StreamKind::Video)
        );
        let sink = frame(Payload::Sink(IqBlock {
            t0: Timestamp(0),
            sample_rate_hz: Hz(1),
            center_hz: Hz(1),
            samples: Arc::from(vec![]),
        }));
        assert_eq!(
            encode_frame(&sink).unwrap_err(),
            WireError::KindNotCarried(StreamKind::Sink)
        );
        let mut video = iq_header(0);
        video["kind"] = json!("video");
        video["payload"] = json!({});
        assert_eq!(
            read_all(&raw_frame(&video, 0, &[])).await.unwrap_err(),
            WireError::KindNotCarried(StreamKind::Video)
        );
    }

    #[tokio::test]
    async fn more_limits() {
        // Body over the limit, checked before anything is read or allocated.
        let n = MAX_BODY / 8 + 1;
        let bytes = raw_frame(&iq_header(n), (n * 8) as u32, &[]);
        assert_eq!(
            read_all(&bytes).await.unwrap_err(),
            WireError::TooLong {
                what: "body",
                len: n * 8,
                max: MAX_BODY
            }
        );
        // Header of zero bytes.
        let mut empty = vec![0x00];
        empty.extend_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            read_all(&empty).await.unwrap_err(),
            WireError::Json(_)
        ));
        // Control line with no newline within the limit.
        let mut long = vec![b'{'; MAX_CONTROL + 10];
        long.push(b'\n');
        assert!(matches!(
            read_all(&long).await.unwrap_err(),
            WireError::TooLong {
                what: "control line",
                max: MAX_CONTROL,
                ..
            }
        ));
        // Exactly at the limit is fine as far as length goes (then fails as JSON).
        let mut at = vec![b' '; MAX_CONTROL - 1];
        at[0] = b'{';
        at[MAX_CONTROL - 2] = b'}';
        at.push(b'\n');
        assert!(matches!(
            read_all(&at).await.unwrap_err(),
            WireError::Json(_)
        ));
        // A text frame with a body, an event with a sample count it lacks.
        let mut text = iq_header(0);
        text["kind"] = json!("text");
        text["payload"] = json!("hi");
        assert_eq!(
            read_all(&raw_frame(&text, 1, &[0])).await.unwrap_err(),
            WireError::BodyMismatch {
                kind: StreamKind::Text,
                samples: 0,
                bytes: 1
            }
        );
        text["payload"] = json!(5);
        assert!(matches!(
            read_all(&raw_frame(&text, 0, &[])).await.unwrap_err(),
            WireError::Json(_)
        ));
        // Header that is not a frame, or carries an unknown field.
        let mut h = iq_header(0);
        h["type"] = json!("lagged");
        assert!(matches!(
            read_all(&raw_frame(&h, 0, &[])).await.unwrap_err(),
            WireError::Json(_)
        ));
        let mut h = iq_header(0);
        h["extra"] = json!(1);
        assert!(matches!(
            read_all(&raw_frame(&h, 0, &[])).await.unwrap_err(),
            WireError::Json(_)
        ));
        let mut h = iq_header(0);
        h["payload"]["gain"] = json!(1);
        assert!(matches!(
            read_all(&raw_frame(&h, 0, &[])).await.unwrap_err(),
            WireError::Json(_)
        ));
        // Sample counts whose byte size overflows are a mismatch, not a wrap.
        let bytes = raw_frame(&iq_header(usize::MAX / 4), 8, &[0; 8]);
        assert!(matches!(
            read_all(&bytes).await.unwrap_err(),
            WireError::BodyMismatch { .. }
        ));
        // Encoding refuses a header over the limit.
        let huge = frame(Payload::Text("x".repeat(MAX_HEADER)));
        assert!(matches!(
            encode_frame(&huge).unwrap_err(),
            WireError::TooLong { what: "header", .. }
        ));
    }

    #[tokio::test]
    async fn garbage_never_panics() {
        // Deterministic xorshift noise, plus noise after each valid start byte.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for round in 0..2000 {
            let mut buf = Vec::new();
            for _ in 0..(round % 64) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                buf.push(x as u8);
            }
            for start in [None, Some(0x00), Some(b'{')] {
                let mut b = Vec::new();
                b.extend(start);
                b.extend_from_slice(&buf);
                let _ = read_all(&b).await;
            }
        }
    }

    #[test]
    fn strict_schema_check() {
        assert!(
            check_strict_schema(&json!({"type":"object","additionalProperties":false,
                "properties":{"gain":{"type":"number"},
                              "band":{"type":"object","additionalProperties":false,
                                      "properties":{"lo":{"type":"integer"}},"required":["lo"]}},
                "required":["band","gain"]}))
            .is_ok()
        );
        assert!(
            check_strict_schema(&json!({"type":"object","additionalProperties":false})).is_ok()
        );
        let e = check_strict_schema(&json!({"type":"object","properties":{}})).unwrap_err();
        assert!(e.contains("additionalProperties"), "{e}");
        let e =
            check_strict_schema(&json!({"type":"array","additionalProperties":false})).unwrap_err();
        assert!(e.contains("object"), "{e}");
        let e = check_strict_schema(&json!({"type":"object","additionalProperties":false,
            "properties":{"gain":{"type":"number"}}}))
        .unwrap_err();
        assert!(e.contains("required") && e.contains("params_schema"), "{e}");
        let e = check_strict_schema(&json!({"type":"object","additionalProperties":false,
            "properties":{"x":{"$ref":"#/$defs/X"}},"required":["x"],
            "$defs":{"X":{"type":"object","properties":{"y":{}},"required":["y"]}}}))
        .unwrap_err();
        assert!(
            e.contains("$defs.X") && e.contains("additionalProperties"),
            "{e}"
        );
        assert!(check_strict_schema(&json!(true)).is_err());
    }
}
