//! VITA 49.2 context packets: bandwidth, RF reference frequency, sample rate
//! and the fixed complex-int16 payload format.

use crate::packet::{Header, PacketKind, VrtError, join_timestamp, split_timestamp};

/// Interval after which the encoder repeats a context packet.
pub const CONTEXT_INTERVAL_NS: u64 = 1_000_000_000;

/// Signal parameters carried by a context packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IqMeta {
    pub center_hz: u64,
    pub sample_rate_hz: u64,
    pub bandwidth_hz: u64,
}

const CIF0_CHANGE: u32 = 1 << 31;
const CIF0_BANDWIDTH: u32 = 1 << 29;
const CIF0_RF_REF: u32 = 1 << 27;
const CIF0_SAMPLE_RATE: u32 = 1 << 21;
const CIF0_PAYLOAD_FORMAT: u32 = 1 << 15;
const CIF0_FIELDS: u32 = CIF0_BANDWIDTH | CIF0_RF_REF | CIF0_SAMPLE_RATE | CIF0_PAYLOAD_FORMAT;

const PAYLOAD_FORMAT_1: u32 = 0x2000_03CF;
const PAYLOAD_FORMAT_2: u32 = 0;

/// Words: header, stream id, TSI, TSF hi, TSF lo, CIF0, 3 x u64, 2 format words.
const WORDS: usize = 5 + 1 + 6 + 2;

const RADIX: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPacket {
    pub stream_id: u32,
    pub count: u8,
    pub t0_ns: u64,
    pub changed: bool,
    pub center_hz: u64,
    pub bandwidth_hz: u64,
    pub sample_rate_hz: u64,
}

impl ContextPacket {
    pub fn encode(&self) -> Vec<u8> {
        let header = Header {
            kind: PacketKind::Context,
            count: self.count,
            // WORDS is a small constant.
            size_words: WORDS as u16,
        };
        let (secs, ps) = split_timestamp(self.t0_ns);
        let cif0 = CIF0_FIELDS | if self.changed { CIF0_CHANGE } else { 0 };
        let mut out = Vec::with_capacity(WORDS * 4);
        out.extend_from_slice(&header.encode().to_be_bytes());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.extend_from_slice(&secs.to_be_bytes());
        out.extend_from_slice(&ps.to_be_bytes());
        out.extend_from_slice(&cif0.to_be_bytes());
        for hz in [self.bandwidth_hz, self.center_hz, self.sample_rate_hz] {
            out.extend_from_slice(&(hz << RADIX).to_be_bytes());
        }
        out.extend_from_slice(&PAYLOAD_FORMAT_1.to_be_bytes());
        out.extend_from_slice(&PAYLOAD_FORMAT_2.to_be_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<ContextPacket, VrtError> {
        let Some(head) = bytes.first_chunk::<4>() else {
            return Err(VrtError::Truncated {
                need: 4,
                have: bytes.len(),
            });
        };
        let header = Header::decode(u32::from_be_bytes(*head))?;
        if header.kind != PacketKind::Context {
            return Err(VrtError::Unsupported("not a context packet".into()));
        }
        if usize::from(header.size_words) * 4 != bytes.len() {
            return Err(VrtError::SizeMismatch {
                header_words: header.size_words,
                actual_words: bytes.len() / 4,
            });
        }
        if bytes.len() != WORDS * 4 {
            return Err(VrtError::Unsupported(format!(
                "context packet of {} words, need {WORDS}",
                bytes.len() / 4
            )));
        }
        let word =
            |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let dword = |i: usize| (u64::from(word(i)) << 32) | u64::from(word(i + 4));
        let cif0 = word(20);
        if cif0 & !CIF0_CHANGE != CIF0_FIELDS {
            return Err(VrtError::Unsupported(format!("CIF0 {cif0:#010x}")));
        }
        if word(48) != PAYLOAD_FORMAT_1 || word(52) != PAYLOAD_FORMAT_2 {
            return Err(VrtError::Unsupported(format!(
                "payload format {:#010x} {:#010x}",
                word(48),
                word(52)
            )));
        }
        Ok(ContextPacket {
            stream_id: word(4),
            count: header.count,
            t0_ns: join_timestamp(word(8), dword(12)),
            changed: cif0 & CIF0_CHANGE != 0,
            bandwidth_hz: dword(24) >> RADIX,
            center_hz: dword(32) >> RADIX,
            sample_rate_hz: dword(40) >> RADIX,
        })
    }
}
