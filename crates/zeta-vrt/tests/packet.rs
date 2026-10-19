use num_complex::Complex;
use zeta_vrt::{
    DataPacket, Header, MAX_SAMPLES_PER_PACKET, PacketKind, VrtError, f32_to_i16, stream_id_of,
};

#[test]
fn header_roundtrips_and_has_known_bits() {
    let h = Header {
        kind: PacketKind::Data,
        count: 5,
        size_words: 7,
    };
    // type 0001, C=0, T=0, rsvd 00, TSI 01, TSF 10, count 0101, size 0x0007
    assert_eq!(h.encode(), 0x1065_0007);
    assert_eq!(Header::decode(0x1065_0007).unwrap(), h);
}

#[test]
fn data_packet_bytes_are_big_endian_ci16() {
    let p = DataPacket {
        stream_id: 0xDEAD_BEEF,
        count: 1,
        t0_ns: 1_000_000_001_000,
        samples: vec![Complex::new(1.0, -1.0), Complex::new(0.5, 0.0)],
    };
    let b = p.encode();
    assert_eq!(b.len(), 7 * 4);
    assert_eq!(&b[0..4], &0x1061_0007u32.to_be_bytes());
    assert_eq!(&b[4..8], &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(&b[8..12], &1000u32.to_be_bytes()); // TSI seconds
    assert_eq!(&b[12..20], &1_000_000u64.to_be_bytes()); // TSF ps = 1000 ns
    assert_eq!(&b[20..24], &[0x7F, 0xFF, 0x80, 0x01]); // 32767, -32767
    assert_eq!(&b[24..28], &[0x40, 0x00, 0x00, 0x00]); // 16384, 0
    let back = DataPacket::decode(&b).unwrap();
    assert_eq!(back.stream_id, 0xDEAD_BEEF);
    assert_eq!(back.t0_ns, 1_000_000_001_000);
    assert!((back.samples[0].re - 1.0).abs() < 1e-4 && (back.samples[1].re - 0.5).abs() < 1e-4);
}

#[test]
fn f32_conversion_saturates() {
    assert_eq!(f32_to_i16(2.0), 32767);
    assert_eq!(f32_to_i16(-2.0), -32767);
    assert_eq!(f32_to_i16(0.0), 0);
}

#[test]
fn truncated_and_mismatched_sizes_are_errors() {
    let b = DataPacket {
        stream_id: 1,
        count: 0,
        t0_ns: 0,
        samples: vec![Complex::new(0.0, 0.0); 4],
    }
    .encode();
    assert!(matches!(
        DataPacket::decode(&b[..b.len() - 1]),
        Err(VrtError::SizeMismatch { .. }) | Err(VrtError::Truncated { .. })
    ));
    assert!(matches!(
        DataPacket::decode(&b[..3]),
        Err(VrtError::Truncated { .. })
    ));
    let mut wrong = b.clone();
    wrong[3] = 0x02; // size field says 2 words
    assert!(matches!(
        DataPacket::decode(&wrong),
        Err(VrtError::SizeMismatch { .. })
    ));
}

#[test]
fn too_many_samples_is_rejected() {
    let p = DataPacket {
        stream_id: 1,
        count: 0,
        t0_ns: 0,
        samples: vec![Complex::new(0.0, 0.0); MAX_SAMPLES_PER_PACKET + 1],
    };
    assert!(matches!(p.try_encode(), Err(VrtError::TooLong(_))));
}

#[test]
fn random_bytes_never_panic() {
    // xorshift, 10_000 buffers of 0..=96 bytes; only the Result matters
    let mut s = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..10_000 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let len = (s % 97) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|i| (s.rotate_left(i as u32 % 64) & 0xFF) as u8)
            .collect();
        let _ = DataPacket::decode(&bytes);
        if bytes.len() >= 4 {
            let _ = Header::decode(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
        }
    }
}

#[test]
fn stream_id_is_fnv1a32() {
    assert_eq!(stream_id_of(""), 0x811C_9DC5);
    assert_eq!(stream_id_of("a"), 0xE40C_292C);
}
