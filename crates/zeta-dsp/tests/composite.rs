//! The `composite_decode` stage: sync, line lock, the vertical interval and gray8 fields
//! from the synthesised PAL/NTSC test card.

use std::sync::Arc;
use std::time::{Duration, Instant};

use num_complex::Complex;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Stage, StageContext};
use zeta_core::stream::{Frame, Payload, StreamBus, VideoFormat};
use zeta_core::types::{Hz, Provenance, SourceId, Span, StreamId, StreamKind, Timestamp};
use zeta_dsp::composite::{
    CompositeDecodeStage, CompositeDecoder, CompositeParams, Field, Scale, StandardChoice,
};
use zeta_dsp::synth::{TEST_CARD_BAR_LEVELS, composite_test_card, parse_params, synthesise};
use zeta_dsp::video_fm::{VideoFmDemod, VideoFmParams, VideoStandard};

const RATE: u32 = 8_000_000;

fn params(standard: StandardChoice) -> CompositeParams {
    CompositeParams {
        standard,
        scale: Scale::Full,
        report_s: 5,
    }
}

/// Samples in one field (half a frame) of `standard` at `rate`.
fn field_len(standard: VideoStandard, rate: u32) -> usize {
    let t = standard.table();
    let lines = f64::from(t.lines_per_field[0] + t.lines_per_field[1]) / 2.0;
    (lines * t.line_s * f64::from(rate)).round() as usize
}

/// The card field (counted from 0) that starts at `start_ns`, the card starting at 0.
fn card_field(standard: VideoStandard, start_ns: u64) -> u64 {
    let ns = field_len(standard, 1_000_000_000) as f64;
    (start_ns as f64 / ns).round() as u64
}

/// Every field's index has its card field's parity: even fields start on a whole line.
fn assert_parity(standard: VideoStandard, fields: &[Field]) {
    for f in fields {
        let k = card_field(standard, f.start_ns);
        assert_eq!(
            f.index % 2,
            k % 2,
            "{standard:?}: index {} for card field {k}",
            f.index
        );
    }
}

/// What feeding `x` in 1 ms blocks gave: the fields, and the sample index (counted from
/// `x`'s start) of the end of the first block after which the decoder was locked.
struct Run {
    fields: Vec<Field>,
    locked_at: Option<usize>,
}

fn feed(dec: &mut CompositeDecoder, x: &[f32], rate: u32, t0_ns: u64) -> Run {
    let block = (rate / 1000) as usize;
    let mut fields = Vec::new();
    let mut locked_at = None;
    for (i, c) in x.chunks(block).enumerate() {
        let t = t0_ns + (i * block) as u64 * 1_000_000_000 / u64::from(rate);
        fields.extend(dec.push(c, u64::from(rate), t));
        if locked_at.is_none() && dec.lock().locked {
            locked_at = Some(i * block + c.len());
        }
    }
    Run { fields, locked_at }
}

fn decode(standard: StandardChoice, x: &[f32], rate: u32) -> (CompositeDecoder, Run) {
    let mut dec = CompositeDecoder::new(&params(standard));
    let run = feed(&mut dec, x, rate, 0);
    (dec, run)
}

fn within(got: f64, want: f64, frac: f64) -> bool {
    (got - want).abs() <= want * frac
}

#[test]
fn pal_card_locks_within_three_fields() {
    let card = composite_test_card(VideoStandard::Pal, RATE, 6);
    let (dec, run) = decode(StandardChoice::Pal, &card, RATE);
    let l = dec.lock();
    assert!(l.locked, "{l:?}");
    assert_eq!(l.standard, Some(VideoStandard::Pal));
    assert!(within(l.line_hz, 15_625.0, 0.01), "line_hz {}", l.line_hz);
    let at = run.locked_at.expect("locked");
    eprintln!(
        "pal: locked after {:.2} fields, {} fields out",
        at as f64 / field_len(VideoStandard::Pal, RATE) as f64,
        run.fields.len()
    );
    assert!(
        at <= 3 * field_len(VideoStandard::Pal, RATE),
        "locked at {at}"
    );
    assert_parity(VideoStandard::Pal, &run.fields);
    assert!(run.fields.len() >= 3, "{} fields", run.fields.len());
    for f in &run.fields {
        assert_eq!((f.width, f.height), (720, 288));
        assert_eq!(f.luma.len(), 720 * 288);
    }
}

#[test]
fn ntsc_card_locks_and_has_240_lines() {
    let card = composite_test_card(VideoStandard::Ntsc, RATE, 6);
    let (dec, run) = decode(StandardChoice::Ntsc, &card, RATE);
    let l = dec.lock();
    assert!(l.locked, "{l:?}");
    assert_eq!(l.standard, Some(VideoStandard::Ntsc));
    assert!(within(l.line_hz, 15_734.0, 0.01), "line_hz {}", l.line_hz);
    let at = run.locked_at.expect("locked");
    eprintln!(
        "ntsc: locked after {:.2} fields, {} fields out",
        at as f64 / field_len(VideoStandard::Ntsc, RATE) as f64,
        run.fields.len()
    );
    assert!(
        at <= 3 * field_len(VideoStandard::Ntsc, RATE),
        "locked at {at}"
    );
    assert_parity(VideoStandard::Ntsc, &run.fields);
    assert!(run.fields.len() >= 3, "{} fields", run.fields.len());
    for f in &run.fields {
        assert_eq!((f.width, f.height), (720, 240));
        assert_eq!(f.luma.len(), 720 * 240);
    }
}

#[test]
fn auto_tells_pal_from_ntsc() {
    for standard in [VideoStandard::Pal, VideoStandard::Ntsc] {
        let card = composite_test_card(standard, RATE, 8);
        let (dec, run) = decode(StandardChoice::Auto, &card, RATE);
        let l = dec.lock();
        assert_eq!(l.standard, Some(standard), "{l:?}");
        assert!(l.locked, "{standard:?}: {l:?}");
        assert!(!run.fields.is_empty(), "{standard:?}: no fields");
        assert_eq!(run.fields[0].height, standard.table().active_lines);
    }
}

/// Bar `k`'s middle 40 % on `row` of `f`: the mean less the card's level, for each bar.
fn bar_errors(f: &Field, row: usize) -> Vec<f64> {
    let w = f.width as usize;
    let line = &f.luma[row * w..(row + 1) * w];
    (0..8)
        .map(|k| {
            let (a, b) = (w * k / 8 + w * 3 / 80, w * k / 8 + w * 7 / 80);
            let mean = line[a..b].iter().map(|&v| f64::from(v)).sum::<f64>() / (b - a) as f64;
            let want = (TEST_CARD_BAR_LEVELS[k] * 255.0).round() as f64;
            mean - want
        })
        .collect()
}

fn assert_bars(f: &Field, tol: f64, what: &str) {
    let e = bar_errors(f, 100);
    eprintln!("{what}: bar errors {e:.1?}");
    assert!(e.iter().all(|d| d.abs() <= tol), "{what}: bar errors {e:?}");
}

#[test]
fn bars_are_recovered_within_tolerance() {
    let card = composite_test_card(VideoStandard::Pal, RATE, 6);
    let (_, run) = decode(StandardChoice::Pal, &card, RATE);
    let f = run.fields.last().expect("a field");
    assert_bars(f, 24.0, "pal");
    // The block: one eighth of the active width, 16 card samples right per card field,
    // on the bottom quarter's middle rows.
    let t = VideoStandard::Pal.table();
    let field_ns = 312.5 * t.line_s * 1e9;
    let card_field = (f.start_ns as f64 / field_ns).round() as u64;
    let card_w = (t.active_s * f64::from(RATE)).round();
    let w = f.width as f64;
    let left = ((card_field * 16) as f64 % card_w) * w / card_w;
    let bw = w / 8.0;
    let row = 252usize;
    let line = &f.luma[row * f.width as usize..(row + 1) * f.width as usize];
    let at = |x: f64| line[(x.rem_euclid(w)) as usize];
    for d in [0.2, 0.5, 0.8] {
        let v = at(left + d * bw);
        assert!(
            v >= 200,
            "inside the block at {d}: {v} (field {card_field})"
        );
    }
    for d in [-1.0, -0.5, 1.5, 2.0, 4.0] {
        let v = at(left + d * bw);
        assert!(v <= 40, "beside the block at {d}: {v} (field {card_field})");
    }
}

/// `n` samples of Gaussian noise of standard deviation `sigma`.
fn gaussian(n: usize, sigma: f64, seed: u64) -> Vec<f32> {
    let mut s = seed;
    let mut uniform = || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((s >> 11) + 1) as f64 / (1u64 << 53) as f64
    };
    (0..n)
        .map(|_| {
            let (u1, u2) = (uniform(), uniform());
            (sigma * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
        })
        .collect()
}

#[test]
fn noise_never_locks_and_publishes_no_fields() {
    let x = gaussian(RATE as usize, 0.1, 7);
    let started = Instant::now();
    let (dec, run) = decode(StandardChoice::Auto, &x, RATE);
    let took = started.elapsed();
    eprintln!("1 s of noise decoded in {took:?}");
    assert!(!dec.lock().locked);
    assert!(run.locked_at.is_none());
    assert!(run.fields.is_empty(), "{} fields", run.fields.len());
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn a_rate_change_relocks_within_three_fields() {
    let mut dec = CompositeDecoder::new(&params(StandardChoice::Pal));
    let card = composite_test_card(VideoStandard::Pal, RATE, 4);
    let first = feed(&mut dec, &card, RATE, 0);
    assert!(first.locked_at.is_some() && dec.lock().locked);
    let rate2 = 10_000_000;
    let card2 = composite_test_card(VideoStandard::Pal, rate2, 3);
    let t0 = card.len() as u64 * 1_000_000_000 / u64::from(RATE);
    // The first block at the new rate resets the decoder.
    let block = (rate2 / 1000) as usize;
    dec.push(&card2[..block], u64::from(rate2), t0);
    assert!(!dec.lock().locked, "still locked after a rate change");
    let rest = feed(&mut dec, &card2[block..], rate2, t0 + 1_000_000);
    let at = rest.locked_at.expect("relocked") + block;
    eprintln!(
        "relocked after {:.2} fields",
        at as f64 / field_len(VideoStandard::Pal, rate2) as f64
    );
    assert!(
        at <= 3 * field_len(VideoStandard::Pal, rate2),
        "relocked at {at}"
    );
    assert!(within(dec.lock().line_hz, 15_625.0, 0.01));
}

#[test]
fn half_scale_halves_the_width() {
    let card = composite_test_card(VideoStandard::Pal, RATE, 6);
    let mut p = params(StandardChoice::Pal);
    p.scale = Scale::Half;
    let mut dec = CompositeDecoder::new(&p);
    let run = feed(&mut dec, &card, RATE, 0);
    let f = run.fields.last().expect("a field");
    assert_eq!((f.width, f.height), (360, 288));
    assert_eq!(f.luma.len(), 360 * 288);
}

/// The synth's FM test card through `VideoFmDemod`: composite volts and their rate.
fn fm_chain(synth: serde_json::Value, demod: VideoFmParams) -> (Vec<f32>, u32) {
    let (spec, imp) = parse_params(synth.as_object().unwrap().clone()).expect("synth params");
    let iq: Vec<Complex<f32>> = synthesise(&spec, &imp, spec.rate_hz(), 0);
    let rate = u64::from(spec.rate_hz());
    let mut d = VideoFmDemod::new(&demod).expect("demod params");
    let mut out = Vec::new();
    let mut comp = 0;
    for c in iq.chunks((rate / 1000) as usize) {
        let (y, r) = d.process(c, rate).expect("demod");
        out.extend(y);
        comp = r;
    }
    (out, comp as u32)
}

#[test]
fn a_6_5_mhz_subcarrier_does_not_disturb_the_picture() {
    let (x, rate) = fm_chain(
        json!({"family":"video","standard":"pal","deviation_hz":2.0e6,"subcarrier_hz":6.5e6,
            "rate_hz":16_000_000,"fields":6,"preemphasis":false,"snr_db":30}),
        VideoFmParams {
            deviation_hz: 2_000_000,
            deemphasis: "none".into(),
            composite_max_hz: 16_000_000,
            standard: Some(VideoStandard::Pal),
        },
    );
    assert_eq!(rate, 16_000_000);
    let (dec, run) = decode(StandardChoice::Pal, &x, rate);
    assert!(dec.lock().locked, "{:?}", dec.lock());
    assert_bars(run.fields.last().expect("a field"), 32.0, "subcarrier");
}

#[test]
fn the_fm_chain_decodes_end_to_end() {
    let (x, rate) = fm_chain(
        json!({"family":"video","standard":"pal","deviation_hz":1.5e6,"subcarrier_hz":null,
            "rate_hz":8_000_000,"fields":6,"preemphasis":true,"snr_db":25}),
        VideoFmParams {
            deviation_hz: 1_500_000,
            deemphasis: "ccir405".into(),
            composite_max_hz: 16_000_000,
            standard: Some(VideoStandard::Pal),
        },
    );
    let (dec, run) = decode(StandardChoice::Pal, &x, rate);
    let l = dec.lock();
    eprintln!("fm chain: {l:?}");
    assert!(l.locked, "{l:?}");
    assert_bars(run.fields.last().expect("a field"), 32.0, "fm chain");
}

fn prov(t0: u64, len: usize, rate: u32) -> Provenance {
    Provenance {
        source: SourceId("s".into()),
        job: None,
        channel: None,
        span: Span {
            start_ns: Timestamp(t0),
            end_ns: Timestamp(t0 + len as u64 * 1_000_000_000 / u64::from(rate)),
        },
    }
}

#[tokio::test]
async fn the_stage_emits_a_lock_event_and_fields() {
    let bus = Arc::new(StreamBus::new(16));
    let v_out = bus.register(StreamKind::Video, prov(0, 0, RATE));
    let e_out = bus.register(StreamKind::Event, prov(0, 0, RATE));
    let mut rx_v = bus.subscribe(&v_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(64);
    let outs = vec![v_out, e_out];
    let stage_bus = bus.clone();
    let h = tokio::spawn(async move {
        CompositeDecodeStage::new()
            .run(
                json!({"standard": "pal"}),
                vec![rx_in],
                outs,
                stage_bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    });
    // Whole fields per frame, so the 8-deep queue never drops one.
    let card = composite_test_card(VideoStandard::Pal, RATE, 6);
    let step = field_len(VideoStandard::Pal, RATE);
    for (i, c) in card.chunks(step).enumerate() {
        let t0 = (i * step) as u64 * 1_000_000_000 / u64::from(RATE);
        tx.send(Frame::new(
            StreamId::new(),
            prov(t0, c.len(), RATE),
            Payload::Audio {
                sample_rate_hz: Hz(u64::from(RATE)),
                channels: 1,
                samples: c.to_vec().into(),
            },
        ))
        .unwrap();
    }
    drop(tx);
    let r = tokio::time::timeout(Duration::from_secs(30), h)
        .await
        .expect("stage ended")
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
    let mut events = Vec::new();
    while let Ok(f) = rx_e.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("expected an event")
        };
        events.push(v);
    }
    let mut videos = 0;
    while let Ok(f) = rx_v.try_recv() {
        let Payload::Video {
            width,
            height,
            format,
            frame,
        } = f.payload
        else {
            panic!("expected video")
        };
        assert_eq!(format, VideoFormat::Gray8);
        assert_eq!((width, height), (720, 288));
        assert_eq!(frame.len(), 720 * 288);
        let span = f.provenance.span;
        assert!(span.end_ns.0 > span.start_ns.0);
        videos += 1;
    }
    assert!(videos >= 3, "{videos} video frames");
    let lock = events
        .iter()
        .find(|e| e["protocol"] == "video" && e["locked"] == true)
        .unwrap_or_else(|| panic!("no locked event in {events:?}"));
    assert_eq!(lock["standard"], "pal");
    assert_eq!(lock["system"], "PAL 625/50");
    assert_eq!(lock["lock"], "locked");
    assert!(lock["dropped"].is_u64());
    assert!(lock["snr_db"].is_i64());
    // The first event is the lock.
    assert_eq!(events[0]["locked"], true, "{events:?}");
}
