use std::io::Cursor;

use num_complex::Complex;
use serde_json::json;
use zeta_core::port::{GainInfo, IqBlock, PortError, PortStats, Tuning};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, Timestamp};
use zeta_source_worker::wire::*;

fn caps() -> Capabilities {
    Capabilities {
        tune_min_hz: Claim::Asserted(Hz(1_000_000)),
        tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
        max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
        formats: vec![SampleFormat::Cf32],
        channels: 1,
        rx: true,
        tx: false,
        remote: false,
        push: false,
    }
}

fn gain() -> GainInfo {
    GainInfo {
        db: None,
        min_db: 0.0,
        max_db: 40.0,
        automatic: true,
        automatic_available: true,
    }
}

#[test]
fn a_block_round_trips_byte_for_byte() {
    let samples = vec![
        Complex::new(0.5f32, -0.25),
        Complex::new(f32::MIN_POSITIVE, 1.0e-30),
        Complex::new(-0.0, 123_456.5),
    ];
    let block = IqBlock {
        t0: Timestamp(1_760_000_000_000_000_000),
        sample_rate_hz: Hz(2_400_000),
        center_hz: Hz(1_090_000_000),
        samples: samples.clone().into(),
    };
    let mut buf = Vec::new();
    write_frame(&mut buf, &Frame::Block(block)).unwrap();
    assert_eq!(buf.len(), 1 + 4 + 28 + 3 * 8);
    let Some(Frame::Block(b)) = read_frame(&mut Cursor::new(buf)).unwrap() else {
        panic!("expected a block");
    };
    assert_eq!(b.t0, Timestamp(1_760_000_000_000_000_000));
    assert_eq!(b.sample_rate_hz, Hz(2_400_000));
    assert_eq!(b.center_hz, Hz(1_090_000_000));
    assert_eq!(b.samples.len(), 3);
    for (a, e) in b.samples.iter().zip(&samples) {
        assert_eq!(a.re.to_bits(), e.re.to_bits());
        assert_eq!(a.im.to_bits(), e.im.to_bits());
    }
}

#[test]
fn control_messages_round_trip_in_the_spec_shape() {
    let tuning = Tuning {
        center_hz: Hz(144_800_000),
        bandwidth_hz: Hz(12_500),
    };
    let cases: Vec<(Control, serde_json::Value)> = vec![
        (
            Control::Hello(Hello {
                protocol: PROTOCOL,
                capabilities: caps(),
                gain: Some(gain()),
                driver: "hackrf".into(),
            }),
            json!({"hello": {
                "protocol": 1,
                "capabilities": serde_json::to_value(caps()).unwrap(),
                "gain": serde_json::to_value(gain()).unwrap(),
                "driver": "hackrf"}}),
        ),
        (
            Control::Reply {
                reply: 3,
                result: Ok(Reply::Tuning(tuning)),
            },
            json!({"reply": 3, "ok": {"center_hz": 144800000, "bandwidth_hz": 12500}}),
        ),
        (
            Control::Reply {
                reply: 4,
                result: Ok(Reply::Gain(gain())),
            },
            json!({"reply": 4, "ok": serde_json::to_value(gain()).unwrap()}),
        ),
        (
            Control::Reply {
                reply: 7,
                result: Err(PortError::OutOfRange {
                    field: "center_hz",
                    value: 1,
                    min: 2,
                    max: 3,
                    suggested: None,
                }),
            },
            json!({"reply": 7, "err": {"kind": "out_of_range", "field": "center_hz",
                "value": 1, "min": 2, "max": 3, "suggested": null}}),
        ),
        (
            Control::Status(Status {
                fault: Some("unplugged".into()),
                current: Some(tuning),
                gain: None,
                stats: Some(PortStats {
                    measured_rate_hz: Some(2_000_000),
                    overflows: 1,
                    timeouts: 2,
                    restarts: 3,
                }),
            }),
            json!({"status": {"fault": "unplugged",
                "current": {"center_hz": 144800000, "bandwidth_hz": 12500},
                "gain": null,
                "stats": {"measured_rate_hz": 2000000, "overflows": 1, "timeouts": 2, "restarts": 3}}}),
        ),
    ];
    for (c, shape) in cases {
        assert_eq!(serde_json::to_value(&c).unwrap(), shape);
        let back: Control = serde_json::from_value(shape).unwrap();
        assert_eq!(back, c);
        let mut buf = Vec::new();
        write_frame(&mut buf, &Frame::Control(c.clone())).unwrap();
        match read_frame(&mut Cursor::new(buf)).unwrap() {
            Some(Frame::Control(d)) => assert_eq!(d, c),
            other => panic!("expected control, got {other:?}"),
        }
    }
    // Ambiguous or empty messages are refused.
    assert!(serde_json::from_value::<Control>(json!({})).is_err());
    assert!(serde_json::from_value::<Control>(json!({"reply": 1})).is_err());
    assert!(
        serde_json::from_value::<Control>(json!({"status": {"fault": null,
        "current": null, "gain": null, "stats": null}, "reply": 1, "ok": null}))
        .is_err()
    );
}

#[test]
fn port_error_display_is_unchanged_after_a_round_trip() {
    let errs = vec![
        PortError::OutOfRange {
            field: "center_hz",
            value: 1,
            min: 2,
            max: 3,
            suggested: None,
        },
        PortError::OutOfRange {
            field: "bandwidth_hz",
            value: 9,
            min: 2,
            max: 3,
            suggested: Some(5),
        },
        PortError::OutOfRange {
            field: "something_else",
            value: 9,
            min: 2,
            max: 3,
            suggested: Some(2),
        },
        PortError::Unsupported("no gain".into()),
        PortError::Io("device lost".into()),
    ];
    assert_eq!(errs[0].to_string(), "center_hz 1 out of range 2..=3");
    for e in errs {
        let back: PortError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.to_string(), e.to_string());
        assert_eq!(back, e);
    }
}

#[test]
fn a_truncated_frame_is_an_error_and_a_clean_eof_is_none() {
    assert!(read_frame(&mut Cursor::new(Vec::new())).unwrap().is_none());
    let mut buf = Vec::new();
    write_frame(
        &mut buf,
        &Frame::Control(Control::Status(Status {
            fault: None,
            current: None,
            gain: None,
            stats: None,
        })),
    )
    .unwrap();
    for cut in [1, 3, 5, buf.len() - 1] {
        assert!(
            read_frame(&mut Cursor::new(buf[..cut].to_vec())).is_err(),
            "cut at {cut}"
        );
    }
    // An unknown kind byte.
    assert!(read_frame(&mut Cursor::new(vec![9, 0, 0, 0, 0])).is_err());
    // A block whose length does not match its header.
    let mut bad = vec![1u8, 29, 0, 0, 0];
    bad.extend([0u8; 29]);
    assert!(read_frame(&mut Cursor::new(bad)).is_err());
}

#[test]
fn requests_parse_from_one_json_line() {
    let r = parse_request(r#"{"id":1,"cmd":"tune","center_hz":144800000,"bandwidth_hz":12500}"#)
        .unwrap();
    assert_eq!(r.id, 1);
    assert_eq!(
        r.cmd,
        Cmd::Tune {
            center_hz: 144_800_000,
            bandwidth_hz: 12_500
        }
    );
    let r = parse_request(r#"{"id":2,"cmd":"set_gain","gain_db":null}"#).unwrap();
    assert_eq!(r.cmd, Cmd::SetGain { gain_db: None });
    let r = parse_request(r#"{"id":3,"cmd":"shutdown"}"#).unwrap();
    assert_eq!(r.cmd, Cmd::Shutdown);
    assert!(parse_request(r#"{"id":4,"cmd":"shutdown","extra":1}"#).is_err());
    assert!(
        parse_request(r#"{"id":4,"cmd":"tune","center_hz":1,"bandwidth_hz":2,"x":1}"#).is_err()
    );
    assert!(parse_request(r#"{"id":4,"cmd":"nope"}"#).is_err());

    let mut buf = Vec::new();
    write_request(
        &mut buf,
        &Request {
            id: 9,
            cmd: Cmd::Shutdown,
        },
    )
    .unwrap();
    assert_eq!(buf, b"{\"id\":9,\"cmd\":\"shutdown\"}\n");
}
