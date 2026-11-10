use std::io::Cursor;

use num_complex::Complex;
use serde_json::json;
use zeta_core::port::{
    GainInfo, IqBlock, PortError, PortStats, Starting, TransportInfo, TransportPath, Tuning,
};
use zeta_core::types::{Capabilities, Claim, Depth, Hz, SampleFormat, Timestamp};
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
        usable_fraction: Claim::Asserted(1.0),
        depths: vec![],
    }
}

fn depth() -> Depth {
    Depth {
        bits: 8,
        lossless: false,
        max_bandwidth_hz: Claim::Measured(Hz(30_720_000)),
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
                transport: None,
                depth: None,
            }),
            json!({"hello": {
                "protocol": 2,
                "capabilities": serde_json::to_value(caps()).unwrap(),
                "gain": serde_json::to_value(gain()).unwrap(),
                "driver": "hackrf", "transport": null, "depth": null}}),
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
                    tune_gaps: 4,
                }),
                transport_dropped: 0,
                depth: Some(depth()),
            }),
            json!({"status": {"fault": "unplugged",
                "current": {"center_hz": 144800000, "bandwidth_hz": 12500},
                "gain": null,
                "stats": {"measured_rate_hz": 2000000, "overflows": 1, "timeouts": 2, "restarts": 3,
                    "tune_gaps": 4}, "transport_dropped": 0,
                "depth": {"bits": 8, "lossless": false, "max_bandwidth_hz": {"measured": 30720000}}}}),
        ),
    ];
    // The stats of an older worker, without `tune_gaps`, still load.
    let old: PortStats = serde_json::from_value(
        json!({"measured_rate_hz": null, "overflows": 1, "timeouts": 2, "restarts": 3}),
    )
    .unwrap();
    assert_eq!(old.tune_gaps, 0);
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
            transport_dropped: 0,
            depth: None,
        })),
    )
    .unwrap();
    for cut in [1, 3, 5, buf.len() - 1] {
        assert!(
            read_frame(&mut Cursor::new(buf[..cut].to_vec())).is_err(),
            "cut at {cut}"
        );
    }
    // A descriptor payload one byte short.
    let mut short = vec![2u8, 39, 0, 0, 0];
    short.extend([0u8; 39]);
    let e = read_frame(&mut Cursor::new(short)).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
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

#[test]
fn a_descriptor_round_trips_byte_for_byte() {
    let d = Descriptor {
        seq: 7,
        slot: 3,
        t0_ns: 10,
        sample_rate_hz: 30_720_000,
        center_hz: 5_845_000_000,
        n: 262_144,
    };
    let mut buf = Vec::new();
    write_frame(&mut buf, &Frame::Descriptor(d)).unwrap();
    assert_eq!(buf.len(), 1 + 4 + 40);
    assert_eq!(buf[0], 2);
    assert_eq!(&buf[1..5], &40u32.to_le_bytes());
    assert_eq!(&buf[5..13], &7u64.to_le_bytes());
    assert_eq!(&buf[13..17], &3u32.to_le_bytes());
    assert_eq!(&buf[41..45], &262_144u32.to_le_bytes());
    let Some(Frame::Descriptor(back)) = read_frame(&mut Cursor::new(buf)).unwrap() else {
        panic!("expected a descriptor");
    };
    assert_eq!(back, d);
}

#[test]
fn hello_carries_the_transport_and_reads_without_it() {
    let h = Hello {
        protocol: PROTOCOL,
        capabilities: caps(),
        gain: None,
        driver: "x".into(),
        transport: Some(TransportInfo {
            path: TransportPath::Shm,
            format: "cf32".into(),
            ring_bytes: 67_108_864,
        }),
        depth: None,
    };
    let v = serde_json::to_value(Control::Hello(h.clone())).unwrap();
    assert_eq!(
        v["hello"]["transport"],
        json!({"path": "shm", "format": "cf32", "ring_bytes": 67108864})
    );
    let Control::Hello(back) = serde_json::from_value(v).unwrap() else {
        panic!("expected a hello");
    };
    assert_eq!(back, h);

    let v = json!({"hello": {"protocol": 1,
        "capabilities": serde_json::to_value(caps()).unwrap(),
        "gain": null, "driver": "x"}});
    let Control::Hello(old) = serde_json::from_value(v).unwrap() else {
        panic!("expected a hello");
    };
    assert_eq!(old.transport, None);
}

#[test]
fn status_reads_without_transport_dropped() {
    let v = json!({"status": {"fault": null, "current": null, "gain": null, "stats": null}});
    let Control::Status(s) = serde_json::from_value(v).unwrap() else {
        panic!("expected a status");
    };
    assert_eq!(s.transport_dropped, 0);
}

#[test]
fn set_depth_and_its_reply_round_trip() {
    let r = parse_request(r#"{"id":5,"cmd":"set_depth","bits":8}"#).unwrap();
    assert_eq!(
        r,
        Request {
            id: 5,
            cmd: Cmd::SetDepth { bits: 8 }
        }
    );
    assert!(parse_request(r#"{"id":5,"cmd":"set_depth","bits":8,"x":1}"#).is_err());
    assert!(parse_request(r#"{"id":5,"cmd":"set_depth"}"#).is_err());
    let mut line = Vec::new();
    write_request(&mut line, &r).unwrap();
    assert_eq!(line, b"{\"id\":5,\"cmd\":\"set_depth\",\"bits\":8}\n");

    let reply = Control::Reply {
        reply: 5,
        result: Ok(Reply::Depth(depth())),
    };
    let shape = json!({"reply": 5, "ok": {"bits": 8, "lossless": false,
        "max_bandwidth_hz": {"measured": 30720000}}});
    assert_eq!(serde_json::to_value(&reply).unwrap(), shape);
    assert_eq!(serde_json::from_value::<Control>(shape).unwrap(), reply);
    let mut buf = Vec::new();
    write_frame(&mut buf, &Frame::Control(reply.clone())).unwrap();
    match read_frame(&mut Cursor::new(buf)).unwrap() {
        Some(Frame::Control(c)) => assert_eq!(c, reply),
        other => panic!("expected control, got {other:?}"),
    }
}

#[test]
fn a_hello_without_depth_reads_as_none() {
    let v = json!({"hello": {"protocol": 2,
        "capabilities": serde_json::to_value(caps()).unwrap(),
        "gain": null, "driver": "x", "transport": null}});
    let Control::Hello(h) = serde_json::from_value(v).unwrap() else {
        panic!("expected a hello");
    };
    assert_eq!(h.depth, None);
    let v = json!({"status": {"fault": null, "current": null, "gain": null, "stats": null,
        "transport_dropped": 0}});
    let Control::Status(s) = serde_json::from_value(v).unwrap() else {
        panic!("expected a status");
    };
    assert_eq!(s.depth, None);
    // A hello that names one carries it.
    let mut h = h;
    h.depth = Some(depth());
    let v = serde_json::to_value(Control::Hello(h.clone())).unwrap();
    assert_eq!(
        serde_json::from_value::<Control>(v).unwrap(),
        Control::Hello(h)
    );
}

#[test]
fn progress_round_trips() {
    let p = Control::Progress(Starting {
        what: "measuring".into(),
        detail: "12-bit at 30720000 Hz".into(),
        step: 2,
        of: 6,
        eta_ms: 30_000,
    });
    let shape = json!({"progress": {"what": "measuring", "detail": "12-bit at 30720000 Hz",
        "step": 2, "of": 6, "eta_ms": 30000}});
    assert_eq!(serde_json::to_value(&p).unwrap(), shape);
    assert_eq!(serde_json::from_value::<Control>(shape).unwrap(), p);
    let mut buf = Vec::new();
    write_frame(&mut buf, &Frame::Control(p.clone())).unwrap();
    match read_frame(&mut Cursor::new(buf)).unwrap() {
        Some(Frame::Control(c)) => assert_eq!(c, p),
        other => panic!("expected control, got {other:?}"),
    }
    // Exactly one message: a progress beside a status is refused, and so is a
    // field it does not have.
    let both = json!({"progress": {"what": "measuring", "detail": "x", "step": 1, "of": 1,
        "eta_ms": 0}, "status": {"fault": null, "current": null, "gain": null, "stats": null}});
    assert!(serde_json::from_value::<Control>(both).is_err());
    let extra = json!({"progress": {"what": "measuring", "detail": "x", "step": 1, "of": 1,
        "eta_ms": 0, "x": 1}});
    assert!(serde_json::from_value::<Control>(extra).is_err());
}
