use zeta_vrt::{ContextPacket, VrtError};

fn sample() -> ContextPacket {
    ContextPacket {
        stream_id: 7,
        count: 0,
        t0_ns: 0,
        changed: true,
        center_hz: 100_000_000,
        bandwidth_hz: 48_000,
        sample_rate_hz: 48_000,
    }
}

#[test]
fn context_packet_fields_are_fixed_point_hz() {
    let c = sample();
    let b = c.encode();
    assert_eq!(b.len(), (5 + 1 + 2 + 2 + 2 + 2) * 4);
    assert_eq!(&b[0..4], &0x4060_000Eu32.to_be_bytes());
    assert_eq!(&b[20..24], &0xA820_8000u32.to_be_bytes());
    assert_eq!(&b[24..32], &(48_000u64 << 20).to_be_bytes());
    assert_eq!(&b[32..40], &(100_000_000u64 << 20).to_be_bytes());
    assert_eq!(&b[40..48], &(48_000u64 << 20).to_be_bytes());
    assert_eq!(&b[48..52], &0x2000_03CFu32.to_be_bytes());
    assert_eq!(&b[52..56], &[0, 0, 0, 0]);
    assert_eq!(ContextPacket::decode(&b).unwrap(), c);
}

#[test]
fn context_with_other_payload_format_is_unsupported() {
    let mut b = sample().encode();
    b[48] = 0x2E;
    assert!(matches!(
        ContextPacket::decode(&b),
        Err(VrtError::Unsupported(_))
    ));
}

#[test]
fn context_with_other_cif0_bits_is_unsupported() {
    let mut b = sample().encode();
    b[20] |= 0x01;
    assert!(matches!(
        ContextPacket::decode(&b),
        Err(VrtError::Unsupported(_))
    ));
}

#[test]
fn context_with_zero_sample_rate_is_unsupported() {
    let b = ContextPacket {
        sample_rate_hz: 0,
        ..sample()
    }
    .encode();
    assert!(matches!(
        ContextPacket::decode(&b),
        Err(VrtError::Unsupported(m)) if m.contains("sample rate")
    ));
}
