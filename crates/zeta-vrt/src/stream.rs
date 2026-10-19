//! Stream encoder and decoder: context packets plus chunked data packets.

use num_complex::Complex;

use crate::MAX_SAMPLES_PER_PACKET;
use crate::context::{CONTEXT_INTERVAL_NS, ContextPacket, IqMeta};
use crate::packet::{DataPacket, Header, PacketKind, VrtError};

/// Turns blocks of samples into context and data packets for one stream.
#[derive(Debug)]
pub struct VrtEncoder {
    stream_id: u32,
    data_count: u8,
    context_count: u8,
    last_meta: Option<IqMeta>,
    last_context_ns: u64,
}

impl VrtEncoder {
    pub fn new(stream_id: u32) -> Self {
        VrtEncoder {
            stream_id,
            data_count: 0,
            context_count: 0,
            last_meta: None,
            last_context_ns: 0,
        }
    }

    /// Encode one block. A context packet comes first when none was sent yet,
    /// when `meta` changed, or when [`CONTEXT_INTERVAL_NS`] has elapsed.
    pub fn encode(&mut self, t0_ns: u64, meta: IqMeta, samples: &[Complex<f32>]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let changed = self.last_meta != Some(meta);
        let due = t0_ns.saturating_sub(self.last_context_ns) >= CONTEXT_INTERVAL_NS;
        if self.last_meta.is_none() || changed || due {
            out.push(
                ContextPacket {
                    stream_id: self.stream_id,
                    count: self.context_count,
                    t0_ns,
                    changed,
                    center_hz: meta.center_hz,
                    bandwidth_hz: meta.bandwidth_hz,
                    sample_rate_hz: meta.sample_rate_hz,
                }
                .encode(),
            );
            self.context_count = (self.context_count + 1) & 0xF;
            self.last_meta = Some(meta);
            self.last_context_ns = t0_ns;
        }
        let mut t = t0_ns;
        for chunk in samples.chunks(MAX_SAMPLES_PER_PACKET) {
            let packet = DataPacket {
                stream_id: self.stream_id,
                count: self.data_count,
                t0_ns: t,
                samples: chunk.to_vec(),
            };
            out.push(packet.encode());
            self.data_count = (self.data_count + 1) & 0xF;
            let step = (chunk.len() as u128 * 1_000_000_000)
                .checked_div(u128::from(meta.sample_rate_hz))
                .unwrap_or(0);
            t = t.saturating_add(u64::try_from(step).unwrap_or(u64::MAX));
        }
        out
    }
}

/// One decoded data packet with the metadata in force.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub t0_ns: u64,
    pub meta: IqMeta,
    pub samples: Vec<Complex<f32>>,
    pub missed_packets: u32,
}

/// Decodes packets of one stream. A failed `feed` leaves state unchanged.
#[derive(Debug, Default)]
pub struct VrtDecoder {
    meta: Option<IqMeta>,
    last_data_count: Option<u8>,
}

impl VrtDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn meta(&self) -> Option<IqMeta> {
        self.meta
    }

    /// Context packets return `Ok(None)`; data packets return samples.
    pub fn feed(&mut self, packet: &[u8]) -> Result<Option<Decoded>, VrtError> {
        let Some(head) = packet.first_chunk::<4>() else {
            return Err(VrtError::Truncated {
                need: 4,
                have: packet.len(),
            });
        };
        match Header::decode(u32::from_be_bytes(*head))?.kind {
            PacketKind::Context => {
                let c = ContextPacket::decode(packet)?;
                self.meta = Some(IqMeta {
                    center_hz: c.center_hz,
                    sample_rate_hz: c.sample_rate_hz,
                    bandwidth_hz: c.bandwidth_hz,
                });
                Ok(None)
            }
            PacketKind::Data => {
                let Some(meta) = self.meta else {
                    return Err(VrtError::Unsupported("data before context".into()));
                };
                let d = DataPacket::decode(packet)?;
                let missed = self
                    .last_data_count
                    .map_or(0, |last| u32::from(d.count.wrapping_sub(last + 1) & 0xF));
                self.last_data_count = Some(d.count);
                Ok(Some(Decoded {
                    t0_ns: d.t0_ns,
                    meta,
                    samples: d.samples,
                    missed_packets: missed,
                }))
            }
        }
    }
}
