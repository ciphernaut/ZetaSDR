use num_complex::Complex;
use zeta_vrt::{DataPacket, IqMeta, MAX_SAMPLES_PER_PACKET, VrtDecoder, VrtEncoder, VrtError};

#[test]
fn encoder_sends_context_first_then_every_second_and_on_change() {
    let meta = IqMeta {
        center_hz: 1_000_000,
        sample_rate_hz: 48_000,
        bandwidth_hz: 48_000,
    };
    let mut e = VrtEncoder::new(1);
    let s = vec![Complex::new(0.0, 0.0); 480];
    assert_eq!(e.encode(0, meta, &s).len(), 2);
    assert_eq!(e.encode(10_000_000, meta, &s).len(), 1);
    assert_eq!(e.encode(1_000_000_000, meta, &s).len(), 2);
    let changed = IqMeta {
        center_hz: 2_000_000,
        ..meta
    };
    assert_eq!(e.encode(1_010_000_000, changed, &s).len(), 2);
}

#[test]
fn encoder_splits_long_blocks_and_advances_t0() {
    let meta = IqMeta {
        center_hz: 0,
        sample_rate_hz: 1_000_000,
        bandwidth_hz: 1_000_000,
    };
    let mut e = VrtEncoder::new(1);
    let s = vec![Complex::new(0.0, 0.0); MAX_SAMPLES_PER_PACKET + 10];
    let packets = e.encode(5_000, meta, &s);
    assert_eq!(packets.len(), 3);
    let d2 = DataPacket::decode(&packets[2]).unwrap();
    assert_eq!(d2.samples.len(), 10);
    assert_eq!(d2.t0_ns, 5_000 + MAX_SAMPLES_PER_PACKET as u64 * 1_000);
}

#[test]
fn decoder_needs_context_counts_loss_and_recovers_after_bad_packet() {
    let meta = IqMeta {
        center_hz: 1,
        sample_rate_hz: 48_000,
        bandwidth_hz: 48_000,
    };
    let mut e = VrtEncoder::new(9);
    let s = vec![Complex::new(0.25, 0.0); 48];
    let first = e.encode(0, meta, &s);
    let mut d = VrtDecoder::new();
    assert!(matches!(d.feed(&first[1]), Err(VrtError::Unsupported(_))));
    assert!(d.feed(&first[0]).unwrap().is_none());
    assert_eq!(d.meta().unwrap().sample_rate_hz, 48_000);
    let got = d.feed(&first[1]).unwrap().unwrap();
    assert_eq!(got.samples.len(), 48);
    assert_eq!(got.missed_packets, 0);
    let _skipped = e.encode(1_000_000, meta, &s);
    let third = e.encode(2_000_000, meta, &s);
    assert!(d.feed(b"\x10\x60\x00\x05junk").is_err());
    let got = d.feed(&third[0]).unwrap().unwrap();
    assert_eq!(got.missed_packets, 1);
    assert_eq!(got.t0_ns, 2_000_000);
}

#[test]
fn decoder_counts_loss_across_mod16_wrap() {
    let meta = IqMeta {
        center_hz: 0,
        sample_rate_hz: 48_000,
        bandwidth_hz: 48_000,
    };
    let context = VrtEncoder::new(3).encode(0, meta, &[]);
    assert_eq!(context.len(), 1);
    let data = |count: u8| {
        DataPacket {
            stream_id: 3,
            count,
            t0_ns: 0,
            samples: vec![Complex::new(0.0, 0.0); 4],
        }
        .encode()
    };
    let mut d = VrtDecoder::new();
    assert!(d.feed(&context[0]).unwrap().is_none());
    assert_eq!(d.feed(&data(15)).unwrap().unwrap().missed_packets, 0);
    assert_eq!(d.feed(&data(0)).unwrap().unwrap().missed_packets, 0);
    assert_eq!(d.feed(&data(15)).unwrap().unwrap().missed_packets, 14);
    assert_eq!(d.feed(&data(2)).unwrap().unwrap().missed_packets, 2);
}

#[test]
fn encoder_handles_rate_zero_and_empty_samples() {
    let zero = IqMeta {
        center_hz: 0,
        sample_rate_hz: 0,
        bandwidth_hz: 0,
    };
    let mut e = VrtEncoder::new(1);
    let s = vec![Complex::new(0.5, -0.5); MAX_SAMPLES_PER_PACKET + 1];
    let packets = e.encode(7, zero, &s);
    assert_eq!(packets.len(), 3);
    let second = DataPacket::decode(&packets[2]).unwrap();
    assert_eq!(second.t0_ns, 7);

    let meta = IqMeta {
        center_hz: 1,
        sample_rate_hz: 48_000,
        bandwidth_hz: 48_000,
    };
    let mut e = VrtEncoder::new(2);
    let first = e.encode(0, meta, &[]);
    assert_eq!(first.len(), 1);
    let mut d = VrtDecoder::new();
    assert!(d.feed(&first[0]).unwrap().is_none());
    assert!(e.encode(10, meta, &[]).is_empty());
}
