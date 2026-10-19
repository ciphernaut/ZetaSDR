//! VITA 49.2 (VRT) signal data packet codec. Big-endian, complex int16 payload.

mod packet;

pub use packet::{
    DataPacket, Header, MAX_SAMPLES_PER_PACKET, PacketKind, VrtError, f32_to_i16, i16_to_f32,
    stream_id_of,
};
