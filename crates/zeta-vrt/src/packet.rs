use num_complex::Complex;

/// Largest number of complex samples carried by one packet.
pub const MAX_SAMPLES_PER_PACKET: usize = 16_384;

/// Words before the sample payload: header, stream id, TSI, TSF hi, TSF lo.
const PREFIX_WORDS: usize = 5;

const TYPE_DATA: u32 = 0b0001;
const TYPE_CONTEXT: u32 = 0b0100;
const TSI_UTC: u32 = 1;
const TSF_REAL_TIME: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VrtError {
    #[error("truncated packet: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("size mismatch: header says {header_words} words, packet has {actual_words}")]
    SizeMismatch {
        header_words: u16,
        actual_words: usize,
    },
    #[error("unsupported packet: {0}")]
    Unsupported(String),
    #[error("too many samples: {0} (max {MAX_SAMPLES_PER_PACKET})")]
    TooLong(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKind {
    Data,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: PacketKind,
    /// 4-bit packet count; higher bits are masked off on encode.
    pub count: u8,
    pub size_words: u16,
}

impl Header {
    /// Layout (MSB first): type[31:28], C[27], T[26], reserved[25:24],
    /// TSI[23:22], TSF[21:20], count[19:16], size[15:0].
    pub fn encode(&self) -> u32 {
        let ty = match self.kind {
            PacketKind::Data => TYPE_DATA,
            PacketKind::Context => TYPE_CONTEXT,
        };
        (ty << 28)
            | (TSI_UTC << 22)
            | (TSF_REAL_TIME << 20)
            | (u32::from(self.count & 0xF) << 16)
            | u32::from(self.size_words)
    }

    pub fn decode(word: u32) -> Result<Header, VrtError> {
        let kind = match word >> 28 {
            TYPE_DATA => PacketKind::Data,
            TYPE_CONTEXT => PacketKind::Context,
            other => return Err(VrtError::Unsupported(format!("packet type {other:#06b}"))),
        };
        if word & (1 << 27) != 0 {
            return Err(VrtError::Unsupported("class id (C) bit set".into()));
        }
        if word & (1 << 26) != 0 {
            return Err(VrtError::Unsupported("trailer (T) bit set".into()));
        }
        let tsi = (word >> 22) & 0b11;
        if tsi != TSI_UTC {
            return Err(VrtError::Unsupported(format!("TSI {tsi}, need {TSI_UTC}")));
        }
        let tsf = (word >> 20) & 0b11;
        if tsf != TSF_REAL_TIME {
            return Err(VrtError::Unsupported(format!(
                "TSF {tsf}, need {TSF_REAL_TIME}"
            )));
        }
        Ok(Header {
            kind,
            count: ((word >> 16) & 0xF) as u8,
            size_words: (word & 0xFFFF) as u16,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataPacket {
    pub stream_id: u32,
    pub count: u8,
    pub t0_ns: u64,
    pub samples: Vec<Complex<f32>>,
}

impl DataPacket {
    /// Encode to wire bytes.
    ///
    /// # Panics
    /// Panics only if `samples.len()` exceeds [`MAX_SAMPLES_PER_PACKET`];
    /// use [`DataPacket::try_encode`] for untrusted lengths.
    pub fn encode(&self) -> Vec<u8> {
        self.try_encode()
            .expect("sample count exceeds MAX_SAMPLES_PER_PACKET")
    }

    pub fn try_encode(&self) -> Result<Vec<u8>, VrtError> {
        let n = self.samples.len();
        if n > MAX_SAMPLES_PER_PACKET {
            return Err(VrtError::TooLong(n));
        }
        let words = PREFIX_WORDS + n;
        let header = Header {
            kind: PacketKind::Data,
            count: self.count,
            // n <= 16_384, so this always fits in u16.
            size_words: words as u16,
        };
        let (secs, ps) = split_timestamp(self.t0_ns);
        let mut out = Vec::with_capacity(words * 4);
        out.extend_from_slice(&header.encode().to_be_bytes());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.extend_from_slice(&secs.to_be_bytes());
        out.extend_from_slice(&ps.to_be_bytes());
        for s in &self.samples {
            out.extend_from_slice(&f32_to_i16(s.re).to_be_bytes());
            out.extend_from_slice(&f32_to_i16(s.im).to_be_bytes());
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<DataPacket, VrtError> {
        let Some(head) = bytes.first_chunk::<4>() else {
            return Err(VrtError::Truncated {
                need: 4,
                have: bytes.len(),
            });
        };
        let header = Header::decode(u32::from_be_bytes(*head))?;
        if header.kind != PacketKind::Data {
            return Err(VrtError::Unsupported("not a signal data packet".into()));
        }
        let actual_words = bytes.len() / 4;
        if usize::from(header.size_words) * 4 != bytes.len() {
            return Err(VrtError::SizeMismatch {
                header_words: header.size_words,
                actual_words,
            });
        }
        let prefix = PREFIX_WORDS * 4;
        if bytes.len() < prefix {
            return Err(VrtError::Truncated {
                need: prefix,
                have: bytes.len(),
            });
        }
        let word =
            |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let stream_id = word(4);
        let t0_ns = join_timestamp(word(8), (u64::from(word(12)) << 32) | u64::from(word(16)));
        let samples = bytes[prefix..]
            .chunks_exact(4)
            .map(|c| {
                Complex::new(
                    i16_to_f32(i16::from_be_bytes([c[0], c[1]])),
                    i16_to_f32(i16::from_be_bytes([c[2], c[3]])),
                )
            })
            .collect();
        Ok(DataPacket {
            stream_id,
            count: header.count,
            t0_ns,
            samples,
        })
    }
}

/// FNV-1a 32-bit hash of the stream id string.
pub fn stream_id_of(id: &str) -> u32 {
    id.bytes().fold(0x811C_9DC5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// `round(x * 32767)`, saturating; NaN maps to 0.
pub fn f32_to_i16(x: f32) -> i16 {
    (x * 32767.0).round().clamp(-32767.0, 32767.0) as i16
}

pub fn i16_to_f32(v: i16) -> f32 {
    f32::from(v) / 32767.0
}

/// Nanoseconds to (TSI seconds, TSF picoseconds). Seconds saturate at `u32::MAX`
/// only for timestamps beyond year 2106.
pub(crate) fn split_timestamp(ns: u64) -> (u32, u64) {
    ((ns / 1_000_000_000) as u32, (ns % 1_000_000_000) * 1000)
}

/// (TSI seconds, TSF picoseconds) to nanoseconds, saturating.
pub(crate) fn join_timestamp(secs: u32, ps: u64) -> u64 {
    u64::from(secs)
        .saturating_mul(1_000_000_000)
        .saturating_add(ps / 1000)
}
