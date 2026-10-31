//! The wire format between the node and a source worker child.
//!
//! Requests go to the child's stdin as one JSON line each. Everything the child
//! answers goes to its stdout as length-prefixed frames, so control messages
//! and IQ blocks share one ordered stream: a frame is a kind byte (0 control,
//! 1 IQ block), a payload length (`u32`, little endian) and the payload. A
//! control payload is JSON; a block payload is a 28-byte [`BlockHeader`]
//! followed by `n` complex samples as little-endian `f32` pairs.

use std::io::{self, Read, Write};
use std::sync::Arc;

use num_complex::Complex;
use serde::{Deserialize, Serialize};
use zeta_core::port::{GainInfo, IqBlock, PortError, PortStats, Tuning};
use zeta_core::types::{Capabilities, Hz, Timestamp};

/// The protocol version the child announces in its [`Hello`].
pub const PROTOCOL: u32 = 1;

/// The largest payload `read_frame` accepts (64 MiB), so a corrupt length
/// cannot make the reader allocate without bound.
const MAX_PAYLOAD: u32 = 64 << 20;

const KIND_CONTROL: u8 = 0;
const KIND_BLOCK: u8 = 1;

/// What the node asks of the child.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum Cmd {
    Tune { center_hz: u64, bandwidth_hz: u64 },
    SetGain { gain_db: Option<f64> },
    Shutdown,
}

/// A command with the id its reply carries.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub cmd: Cmd,
}

impl<'de> Deserialize<'de> for Request {
    /// Serde ignores `deny_unknown_fields` next to `flatten`, so the `id` is
    /// taken out by hand and the rest, which must be exactly a command, is
    /// parsed strictly.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut m = serde_json::Map::deserialize(d)?;
        let id = m
            .remove("id")
            .ok_or_else(|| D::Error::missing_field("id"))?;
        let id = u64::deserialize(id).map_err(D::Error::custom)?;
        let fields = m.len();
        let cmd = Cmd::deserialize(serde_json::Value::Object(m)).map_err(D::Error::custom)?;
        // A unit variant ignores the fields left over, so `shutdown` is held
        // to its `cmd` alone here.
        if cmd == Cmd::Shutdown && fields != 1 {
            return Err(D::Error::custom("unknown field for command `shutdown`"));
        }
        Ok(Request { id, cmd })
    }
}

/// The child's first message: what the device is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub protocol: u32,
    pub capabilities: Capabilities,
    pub gain: Option<GainInfo>,
    pub driver: String,
}

/// The child's health, sent whenever it changes and periodically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub fault: Option<String>,
    pub current: Option<Tuning>,
    pub gain: Option<GainInfo>,
    pub stats: Option<PortStats>,
}

/// A successful answer to a request: the band after a tune, the gain after a
/// gain change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reply {
    Tuning(Tuning),
    Gain(GainInfo),
}

/// A control message from the child.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    Hello(Hello),
    Reply {
        reply: u64,
        result: Result<Reply, PortError>,
    },
    Status(Status),
}

/// The JSON shape of a [`Control`]: exactly one of `hello`, `status` or
/// `reply` with `ok` or `err`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hello: Option<Hello>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reply: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<Reply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    err: Option<PortError>,
}

impl Serialize for Control {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut j = ControlJson {
            hello: None,
            status: None,
            reply: None,
            ok: None,
            err: None,
        };
        match self {
            Control::Hello(h) => j.hello = Some(h.clone()),
            Control::Status(st) => j.status = Some(st.clone()),
            Control::Reply { reply, result } => {
                j.reply = Some(*reply);
                match result {
                    Ok(r) => j.ok = Some(r.clone()),
                    Err(e) => j.err = Some(e.clone()),
                }
            }
        }
        j.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Control {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match ControlJson::deserialize(d)? {
            ControlJson {
                hello: Some(h),
                status: None,
                reply: None,
                ok: None,
                err: None,
            } => Ok(Control::Hello(h)),
            ControlJson {
                hello: None,
                status: Some(st),
                reply: None,
                ok: None,
                err: None,
            } => Ok(Control::Status(st)),
            ControlJson {
                hello: None,
                status: None,
                reply: Some(reply),
                ok: Some(r),
                err: None,
            } => Ok(Control::Reply {
                reply,
                result: Ok(r),
            }),
            ControlJson {
                hello: None,
                status: None,
                reply: Some(reply),
                ok: None,
                err: Some(e),
            } => Ok(Control::Reply {
                reply,
                result: Err(e),
            }),
            _ => Err(D::Error::custom(
                "a control message is exactly one of hello, status, or reply with ok or err",
            )),
        }
    }
}

/// The fixed part of a block payload, 28 bytes little endian: `t0_ns`,
/// `sample_rate_hz`, `center_hz` (`u64` each), then the sample count `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub t0_ns: u64,
    pub sample_rate_hz: u64,
    pub center_hz: u64,
    pub n: u32,
}

const HEADER_LEN: usize = 28;

impl BlockHeader {
    fn to_bytes(self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..8].copy_from_slice(&self.t0_ns.to_le_bytes());
        b[8..16].copy_from_slice(&self.sample_rate_hz.to_le_bytes());
        b[16..24].copy_from_slice(&self.center_hz.to_le_bytes());
        b[24..28].copy_from_slice(&self.n.to_le_bytes());
        b
    }

    fn from_bytes(b: &[u8; HEADER_LEN]) -> Self {
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        BlockHeader {
            t0_ns: u64_at(0),
            sample_rate_hz: u64_at(8),
            center_hz: u64_at(16),
            n: u32::from_le_bytes(b[24..28].try_into().unwrap()),
        }
    }
}

/// One frame of the child's stdout.
#[derive(Clone, Debug)]
pub enum Frame {
    Control(Control),
    Block(IqBlock),
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Write one frame. The caller flushes.
pub fn write_frame(w: &mut impl Write, f: &Frame) -> io::Result<()> {
    let (kind, payload) = match f {
        Frame::Control(c) => (
            KIND_CONTROL,
            serde_json::to_vec(c).map_err(|e| bad(e.to_string()))?,
        ),
        Frame::Block(b) => {
            let n = u32::try_from(b.samples.len()).map_err(|_| bad("block too long"))?;
            let header = BlockHeader {
                t0_ns: b.t0.0,
                sample_rate_hz: b.sample_rate_hz.0,
                center_hz: b.center_hz.0,
                n,
            };
            let mut p = Vec::with_capacity(HEADER_LEN + b.samples.len() * 8);
            p.extend_from_slice(&header.to_bytes());
            for s in b.samples.iter() {
                p.extend_from_slice(&s.re.to_le_bytes());
                p.extend_from_slice(&s.im.to_le_bytes());
            }
            (KIND_BLOCK, p)
        }
    };
    let len = u32::try_from(payload.len()).map_err(|_| bad("frame too long"))?;
    w.write_all(&[kind])?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&payload)
}

/// Read one frame: `Ok(None)` at a clean end of stream (before a frame's first
/// byte), an error for a truncated frame, an unknown kind or a malformed
/// payload.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let kind = head[0];
    let len = u32::from_le_bytes(head[1..5].try_into().unwrap());
    if kind != KIND_CONTROL && kind != KIND_BLOCK {
        return Err(bad(format!("unknown frame kind {kind}")));
    }
    if len > MAX_PAYLOAD {
        return Err(bad(format!("frame of {len} bytes is too long")));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    if kind == KIND_CONTROL {
        let c = serde_json::from_slice(&payload).map_err(|e| bad(e.to_string()))?;
        return Ok(Some(Frame::Control(c)));
    }
    let Some((h, body)) = payload.split_first_chunk::<HEADER_LEN>() else {
        return Err(bad("block shorter than its header"));
    };
    let h = BlockHeader::from_bytes(h);
    if body.len() != h.n as usize * 8 {
        return Err(bad(format!(
            "block of {} bytes does not hold {} samples",
            body.len(),
            h.n
        )));
    }
    let f32_at = |b: &[u8]| f32::from_le_bytes(b.try_into().unwrap());
    let samples: Arc<[Complex<f32>]> = body
        .chunks_exact(8)
        .map(|c| Complex::new(f32_at(&c[0..4]), f32_at(&c[4..8])))
        .collect();
    Ok(Some(Frame::Block(IqBlock {
        t0: Timestamp(h.t0_ns),
        sample_rate_hz: Hz(h.sample_rate_hz),
        center_hz: Hz(h.center_hz),
        samples,
    })))
}

/// Write a request as one JSON line. The caller flushes.
pub fn write_request(w: &mut impl Write, r: &Request) -> io::Result<()> {
    let mut line = serde_json::to_vec(r).map_err(|e| bad(e.to_string()))?;
    line.push(b'\n');
    w.write_all(&line)
}

/// Parse one request line; an unknown command or field is an error naming it.
pub fn parse_request(line: &str) -> Result<Request, String> {
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}
