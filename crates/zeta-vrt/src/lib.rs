//! VITA 49.2 (VRT) signal data and context packet codec (big-endian, complex int16 payload),
//! and the typed envelope that carries non-IQ frames on the same binary wire.

mod context;
pub mod envelope;
mod packet;
mod stream;
pub mod video;

pub use context::{CONTEXT_INTERVAL_NS, ContextPacket, IqMeta};
pub use envelope::{envelope, parse_envelope};
pub use stream::{Decoded, VrtDecoder, VrtEncoder};

pub use packet::{
    DataPacket, Header, MAX_SAMPLES_PER_PACKET, PacketKind, VrtError, f32_to_i16, i16_to_f32,
    stream_id_of,
};
