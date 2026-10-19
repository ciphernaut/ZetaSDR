//! VITA 49.2 (VRT) signal data packet codec. Big-endian, complex int16 payload.

mod context;
pub mod envelope;
mod packet;
mod stream;

pub use context::{CONTEXT_INTERVAL_NS, ContextPacket, IqMeta};
pub use envelope::{envelope, parse_envelope};
pub use stream::{Decoded, VrtDecoder, VrtEncoder};

pub use packet::{
    DataPacket, Header, MAX_SAMPLES_PER_PACKET, PacketKind, VrtError, f32_to_i16, i16_to_f32,
    stream_id_of,
};
