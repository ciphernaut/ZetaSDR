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
    /// The end of the first block after which the decoder was not locked.
    unlocked_at: Option<usize>,
    /// The slowest block's `push`.
    slowest: Duration,
}

fn feed(dec: &mut CompositeDecoder, x: &[f32], rate: u32, t0_ns: u64) -> Run {
    let block = (rate / 1000) as usize;
    let mut fields = Vec::new();
    let mut locked_at = None;
    let mut unlocked_at = None;
    let mut slowest = Duration::ZERO;
    for (i, c) in x.chunks(block).enumerate() {
        let t = t0_ns + (i * block) as u64 * 1_000_000_000 / u64::from(rate);
        let started = Instant::now();
        fields.extend(dec.push(c, u64::from(rate), t));
        slowest = slowest.max(started.elapsed());
        let locked = dec.lock().locked;
        if locked_at.is_none() && locked {
            locked_at = Some(i * block + c.len());
        }
        if unlocked_at.is_none() && !locked {
            unlocked_at = Some(i * block + c.len());
        }
    }
    Run {
        fields,
        locked_at,
        unlocked_at,
        slowest,
    }
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
    eprintln!("pal: slowest 1 ms block {:?}", run.slowest);
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

/// The time of sample `n` at `rate`, ns.
fn ns(n: usize, rate: u32) -> u64 {
    n as u64 * 1_000_000_000 / u64::from(rate)
}

/// A decoder locked on 4 fields of the PAL card: it and the time the card ended.
fn locked_on_pal() -> (CompositeDecoder, u64) {
    let mut dec = CompositeDecoder::new(&params(StandardChoice::Pal));
    let card = composite_test_card(VideoStandard::Pal, RATE, 4);
    feed(&mut dec, &card, RATE, 0);
    assert!(dec.lock().locked, "{:?}", dec.lock());
    (dec, ns(card.len(), RATE))
}

#[test]
fn flat_input_after_lock_unlocks() {
    let (mut dec, t0) = locked_on_pal();
    let flat = vec![0.2f32; 3 * RATE as usize];
    let run = feed(&mut dec, &flat, RATE, t0);
    let fl = field_len(VideoStandard::Pal, RATE);
    let at = run.unlocked_at.expect("unlocked");
    eprintln!("flat: unlocked after {:.2} fields", at as f64 / fl as f64);
    assert!(at <= 3 * fl, "unlocked at {at}");
    assert!(!dec.lock().locked, "{:?}", dec.lock());
    assert!(
        run.fields.iter().all(|f| f.start_ns < t0),
        "a field from the flat input"
    );
}

#[test]
fn a_dc_step_after_lock_relocks() {
    for step in [0.4f32, -0.4] {
        let (mut dec, t0) = locked_on_pal();
        let mut card = composite_test_card(VideoStandard::Pal, RATE, 5);
        card.iter_mut().for_each(|v| *v += step);
        let run = feed(&mut dec, &card, RATE, t0);
        let l = dec.lock();
        eprintln!(
            "dc step {step:+}: {l:?}, {} fields after the step",
            run.fields.iter().filter(|f| f.start_ns >= t0).count()
        );
        assert!(l.locked, "step {step}: {l:?}");
        let f = run
            .fields
            .iter()
            .rfind(|f| f.start_ns >= t0)
            .unwrap_or_else(|| panic!("step {step}: no field after the step"));
        assert_bars(f, 24.0, &format!("dc step {step:+}"));
    }
}

#[test]
fn a_job_started_on_noise_locks_when_the_card_appears() {
    let mut x = gaussian(RATE as usize, 0.1, 11);
    let start = x.len();
    let mut card = composite_test_card(VideoStandard::Pal, RATE, 8);
    card.iter_mut().for_each(|v| *v += 0.3);
    x.extend(card);
    let (dec, run) = decode(StandardChoice::Auto, &x, RATE);
    let fl = field_len(VideoStandard::Pal, RATE);
    let at = run.locked_at.expect("locked");
    eprintln!(
        "noise then card: locked {:.2} fields after the card began, {:?}",
        (at - start) as f64 / fl as f64,
        dec.lock()
    );
    assert!(at > start, "locked on noise");
    assert!(at - start <= 8 * fl);
    assert_eq!(dec.lock().standard, Some(VideoStandard::Pal));
    assert!(run.fields.iter().all(|f| f.start_ns >= ns(start, RATE)));
    assert!(!run.fields.is_empty());
}

#[test]
fn brightness_does_not_depend_on_the_demod_deviation() {
    for demod_dev in [1_500_000u64, 8_000_000] {
        let (x, rate) = fm_chain(
            json!({"family":"video","standard":"pal","deviation_hz":1.5e6,"subcarrier_hz":null,
                "rate_hz":8_000_000,"fields":6,"preemphasis":true,"snr_db":25}),
            VideoFmParams {
                deviation_hz: demod_dev,
                deemphasis: "ccir405".into(),
                composite_max_hz: 16_000_000,
                standard: Some(VideoStandard::Pal),
            },
        );
        let (dec, run) = decode(StandardChoice::Pal, &x, rate);
        assert!(dec.lock().locked, "demod {demod_dev}: {:?}", dec.lock());
        let f = run.fields.last().expect("a field");
        assert_bars(f, 24.0, &format!("demod deviation {demod_dev}"));
        let white = bar_errors(f, 100)[0] + 255.0;
        assert!(white >= 230.0, "demod {demod_dev}: white bar {white}");
    }
}

/// How many times the lock changed over `run`'s blocks, from unlocked.
fn lock_changes(dec: &mut CompositeDecoder, x: &[f32]) -> (usize, usize) {
    let block = (RATE / 1000) as usize;
    let (mut changes, mut was, mut fields) = (0, false, 0);
    for (i, c) in x.chunks(block).enumerate() {
        fields += dec.push(c, u64::from(RATE), ns(i * block, RATE)).len();
        let now = dec.lock().locked;
        if now != was {
            changes += 1;
            was = now;
        }
    }
    (changes, fields)
}

#[test]
fn a_loop_seam_does_not_unlock() {
    // 25 fields is not whole frames: each repeat starts half a line off the grid and
    // with the wrong field parity, as a looped capture of that length does.
    let card = composite_test_card(VideoStandard::Pal, RATE, 25);
    let x: Vec<f32> = card.iter().copied().cycle().take(4 * card.len()).collect();
    let mut dec = CompositeDecoder::new(&params(StandardChoice::Pal));
    let (changes, fields) = lock_changes(&mut dec, &x);
    eprintln!("loop of 25 fields x4: {changes} lock changes, {fields} fields of 100");
    assert_eq!(changes, 1, "the lock changed {changes} times");
    assert!(fields >= 90, "{fields} fields");
}

/// The served fixture's signal (`gen_video`: PAL, 1.5 MHz deviation, CCIR 405, 25 dB)
/// of `fields` fields, its IQ looped `loops` times as a node loops a capture, through
/// `video_fm_demod` at its defaults as the `video_decode` row runs it.
fn looped_fixture(fields: u32, loops: usize) -> Vec<f32> {
    let synth = json!({"family":"video","standard":"pal","deviation_hz":1.5e6,
        "subcarrier_hz":null,"rate_hz":8_000_000,"fields":fields,"preemphasis":true,
        "snr_db":25});
    let (spec, imp) = parse_params(synth.as_object().unwrap().clone()).expect("synth params");
    let iq: Vec<Complex<f32>> = synthesise(&spec, &imp, spec.rate_hz(), 0);
    let looped: Vec<Complex<f32>> = iq.iter().copied().cycle().take(loops * iq.len()).collect();
    let mut d = VideoFmDemod::new(&VideoFmParams {
        deviation_hz: 8_000_000,
        deemphasis: "ccir405".into(),
        composite_max_hz: 16_000_000,
        standard: None,
    })
    .expect("demod params");
    let mut out = Vec::new();
    for c in looped.chunks(8_000) {
        out.extend(d.process(c, 8_000_000).expect("demod").0);
    }
    out
}

#[test]
fn the_looped_fixture_holds_its_lock() {
    for fields in [25u32, 26] {
        let x = looped_fixture(fields, 4);
        let mut dec = CompositeDecoder::new(&params(StandardChoice::Auto));
        let (changes, got) = lock_changes(&mut dec, &x);
        eprintln!(
            "fixture of {fields} fields looped x4: {changes} lock changes, {got} fields of {}",
            4 * fields
        );
        assert_eq!(
            changes, 1,
            "{fields} fields: the lock changed {changes} times"
        );
        assert!(
            got as u32 >= 4 * fields - 10,
            "{fields} fields: {got} fields"
        );
    }
}

#[test]
fn lost_input_costs_a_field_not_the_lock() {
    let (mut dec, _) = locked_on_pal();
    // The next card, with 3 ms lost every 50 ms: each loss is a stage queue drop.
    let card = composite_test_card(VideoStandard::Pal, RATE, 25);
    let block = (RATE / 1000) as usize;
    let t0 = ns(4 * field_len(VideoStandard::Pal, RATE), RATE);
    let (mut fields, mut unlocked) = (0, 0);
    for (i, c) in card.chunks(block).enumerate() {
        if i % 50 < 3 {
            if i % 50 == 2 {
                dec.gap();
            }
            continue;
        }
        fields += dec.push(c, u64::from(RATE), t0 + ns(i * block, RATE)).len();
        unlocked += usize::from(!dec.lock().locked);
    }
    eprintln!("3 ms lost every 50 ms: {fields} fields of 25, {unlocked} blocks unlocked");
    assert_eq!(unlocked, 0, "the lock went");
    assert!(fields >= 12, "{fields} fields");
}

/// The node's path for the served fixture: 4800-sample blocks (the file radio's), the
/// `channel` stage's direct filter at the job's 7.68 MHz, `video_fm_demod` at its
/// defaults, the decoder in the same blocks.
#[test]
fn the_served_chain_decodes_every_field() {
    use zeta_dsp::channel::{ChannelState, extract_offset_streaming};
    let synth = json!({"family":"video","standard":"pal","deviation_hz":1.5e6,
        "subcarrier_hz":null,"rate_hz":8_000_000,"fields":26,"preemphasis":true,
        "snr_db":25});
    let (spec, imp) = parse_params(synth.as_object().unwrap().clone()).expect("synth params");
    let iq: Vec<Complex<f32>> = synthesise(&spec, &imp, spec.rate_hz(), 0);
    let looped: Vec<Complex<f32>> = iq.iter().copied().cycle().take(4 * iq.len()).collect();
    let mut ch = ChannelState::new();
    let mut demod = VideoFmDemod::new(&VideoFmParams {
        deviation_hz: 8_000_000,
        deemphasis: "ccir405".into(),
        composite_max_hz: 16_000_000,
        standard: None,
    })
    .expect("demod params");
    let mut dec = CompositeDecoder::new(&params(StandardChoice::Auto));
    let (mut fields, mut changes, mut was) = (0usize, 0usize, false);
    for (i, c) in looped.chunks(4800).enumerate() {
        let (y, r) = extract_offset_streaming(&mut ch, c, 8_000_000, 0, 7_680_000, 8_000_000)
            .expect("channel");
        let (x, comp) = demod.process(&y, r).expect("demod");
        fields += dec.push(&x, comp, ns(i * 4800, RATE)).len();
        let now = dec.lock().locked;
        if now != was {
            changes += 1;
            was = now;
        }
    }
    eprintln!("served chain: {fields} fields of 104, {changes} lock changes");
    assert_eq!(changes, 1);
    // Measured 98: the fields before the lock (about 2) and one or two at the IQ loop.
    assert!(fields >= 96, "{fields} fields of 104");
}

/// What [`run_lossy`] saw: the events and the number of fields.
struct Lossy {
    events: Vec<serde_json::Value>,
    fields: usize,
}

/// The stage over 1.2 s of the PAL card in 1 ms blocks into an 8-deep input, a block every
/// 2 ms so nothing is lost but: the blocks in `skip` (never sent: a gap in the spans' time,
/// as a block lost upstream arrives), and the ten blocks from `burst` sent at once (the
/// input lags by two: a real `Lagged`).
async fn run_lossy(skip: &[usize], burst: Option<usize>) -> Lossy {
    let bus = Arc::new(StreamBus::new(256));
    let v_out = bus.register(StreamKind::Video, prov(0, 0, RATE));
    let e_out = bus.register(StreamKind::Event, prov(0, 0, RATE));
    let mut rx_v = bus.subscribe(&v_out).unwrap();
    let mut rx_e = bus.subscribe(&e_out).unwrap();
    let (tx, rx_in) = broadcast::channel(8);
    let outs = vec![v_out, e_out];
    let stage_bus = bus.clone();
    let h = tokio::spawn(async move {
        CompositeDecodeStage::new()
            .run(
                json!({"standard": "pal", "report_s": 1}),
                vec![rx_in],
                outs,
                stage_bus,
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await
    });
    let card = composite_test_card(VideoStandard::Pal, RATE, 60);
    let block = (RATE / 1000) as usize;
    let (mut events, mut fields) = (Vec::new(), 0);
    for (i, c) in card.chunks(block).enumerate() {
        if skip.contains(&i) {
            continue;
        }
        let t0 = ns(i * block, RATE);
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
        if burst.is_none_or(|b| !(b..b + 9).contains(&i)) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        while let Ok(f) = rx_v.try_recv() {
            assert!(matches!(f.payload, Payload::Video { .. }));
            fields += 1;
        }
    }
    drop(tx);
    let r = tokio::time::timeout(Duration::from_secs(30), h)
        .await
        .expect("stage ended")
        .unwrap();
    assert!(r.is_ok(), "{r:?}");
    fields += std::iter::from_fn(|| rx_v.try_recv().ok()).count();
    while let Ok(f) = rx_e.try_recv() {
        let Payload::Event(v) = f.payload else {
            panic!("expected an event")
        };
        events.push(v);
    }
    Lossy { events, fields }
}

/// Every event from the first lock on is locked, and the last says `dropped` is at least
/// `min`.
fn assert_kept_lock(run: &Lossy, min: u64, what: &str) {
    let first = run
        .events
        .iter()
        .position(|e| e["locked"] == true)
        .unwrap_or_else(|| panic!("{what}: no lock in {:?}", run.events));
    assert!(
        run.events[first..].iter().all(|e| e["locked"] == true),
        "{what}: the lock went: {:?}",
        run.events
    );
    let last = run.events.last().unwrap();
    assert!(
        last["dropped"].as_u64().unwrap() >= min,
        "{what}: {:?}",
        run.events
    );
}

/// The clean run the lossy ones are measured against: the lock, and at most one input
/// lost (a slow 2-CPU host may drop one even at a block every 2 ms).
async fn clean_run() -> Lossy {
    let clean = run_lossy(&[], None).await;
    assert_kept_lock(&clean, 0, "clean");
    let dropped = clean.events.last().unwrap()["dropped"].as_u64().unwrap();
    assert!(dropped <= 1, "clean: {:?}", clean.events);
    clean
}

/// A lossy run's fields against the clean one's: no more, and the loss costs a field or
/// two, never the picture (measured: 57 against 58).
fn assert_cost_a_field(lossy: &Lossy, clean: &Lossy, what: &str) {
    eprintln!("{what}: {} fields, clean {}", lossy.fields, clean.fields);
    assert!(
        lossy.fields <= clean.fields && lossy.fields + 3 >= clean.fields,
        "{what}: {} fields, clean {}",
        lossy.fields,
        clean.fields
    );
}

#[tokio::test]
async fn a_lagged_input_keeps_the_lock_costs_a_field_and_counts() {
    let clean = clean_run().await;
    let lagged = run_lossy(&[], Some(400)).await;
    assert_kept_lock(&lagged, 2, "lagged");
    assert_cost_a_field(&lagged, &clean, "lagged");
}

#[tokio::test]
async fn a_gap_in_the_input_time_keeps_the_lock_costs_a_field_and_counts() {
    let clean = clean_run().await;
    let gap = run_lossy(&[400, 401, 402], None).await;
    assert_kept_lock(&gap, 1, "gap");
    assert_cost_a_field(&gap, &clean, "gap");
}
