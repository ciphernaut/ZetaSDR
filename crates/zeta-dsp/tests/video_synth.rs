//! The synthesised PAL/NTSC test card and the CCIR 405 shelf.

use serde_json::json;
use zeta_dsp::synth::{TEST_CARD_BAR_LEVELS, composite_test_card, parse_params, synthesise};
use zeta_dsp::video_fm::{SYNC_V, Shelf, VideoStandard, WHITE_V};

const RATE: u32 = 8_000_000;
/// Samples per PAL line at 8 MHz.
const PAL_LINE: usize = 512;

fn min_max(x: &[f32]) -> (f32, f32) {
    x.iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)))
}

#[test]
fn pal_card_has_the_right_length_and_levels() {
    let c = composite_test_card(VideoStandard::Pal, RATE, 2);
    assert_eq!(c.len(), ((312.0 + 313.0) * 64e-6 * 8e6f64).round() as usize);
    let (lo, hi) = min_max(&c);
    assert!((lo - SYNC_V).abs() < 0.02, "min {lo}");
    assert!((hi - WHITE_V).abs() < 0.02, "max {hi}");
    // An equalising pulse is half a sync pulse wide.
    let sync = (0.5 * 4.7e-6 * 8e6f64) as usize;
    assert!(c[..sync].iter().all(|&v| v < -0.25));
}

#[test]
fn ntsc_card_counts_525_lines_over_two_fields() {
    let c = composite_test_card(VideoStandard::Ntsc, RATE, 2);
    assert_eq!(
        c.len(),
        ((262.0 + 263.0) * 63.556e-6 * 8e6f64).round() as usize
    );
}

#[test]
fn bars_step_down_across_a_line() {
    let t = VideoStandard::Pal.table();
    let c = composite_test_card(VideoStandard::Pal, RATE, 2);
    let line = (t.first_active_line - 1 + 50) as usize;
    let base = line * PAL_LINE + (t.active_start_s * 8e6) as usize;
    let w = (t.active_s * 8e6) as usize;
    for (k, level) in TEST_CARD_BAR_LEVELS.iter().enumerate() {
        let a = base + w * k / 8 + w * 3 / 80;
        let b = base + w * k / 8 + w * 7 / 80;
        let mean = c[a..b].iter().sum::<f32>() / (b - a) as f32;
        let want = level * WHITE_V;
        assert!((mean - want).abs() < 0.03, "bar {k}: {mean} vs {want}");
    }
}

#[test]
fn the_block_moves_between_fields() {
    let t = VideoStandard::Pal.table();
    let c = composite_test_card(VideoStandard::Pal, RATE, 2);
    let row = (t.first_active_line - 1 + t.active_lines * 7 / 8) as usize;
    let w = (t.active_s * 8e6) as usize;
    let edge = |field_start_line: usize| {
        let base = (field_start_line + row) * PAL_LINE + (t.active_start_s * 8e6) as usize;
        (0..w)
            .find(|&x| c[base + x] > 0.35)
            .expect("a white block on the row")
    };
    let (e0, e1) = (edge(0), edge(312));
    assert_eq!((e1 + w - e0) % w, 16, "edges {e0} {e1}");
}

#[test]
fn fm_video_is_unit_magnitude_and_parses_from_a_test_block() {
    let params = json!({"family":"video","standard":"pal","deviation_hz":1500000.0,
        "subcarrier_hz":null,"rate_hz":8000000,"fields":2,"snr_db":30});
    let (spec, imp) = parse_params(params.as_object().unwrap().clone()).expect("parses");
    assert_eq!(spec.rate_hz(), 8_000_000);
    let iq = synthesise(&spec, &imp, 8_000_000, 0);
    assert_eq!(
        iq.len(),
        composite_test_card(VideoStandard::Pal, RATE, 2).len()
    );
    assert!(iq.iter().all(|z| (0.5..1.5).contains(&z.norm())));
}

#[test]
fn ccir405_pre_then_de_is_identity() {
    let ramp: Vec<f32> = (0..1000).map(|n| n as f32 / 1000.0).collect();
    let mut x = ramp.clone();
    Shelf::ccir405(VideoStandard::Pal, 8e6).pre(&mut x);
    Shelf::ccir405(VideoStandard::Pal, 8e6).de(&mut x);
    for (n, (got, want)) in x.iter().zip(&ramp).enumerate().skip(50) {
        assert!((got - want).abs() < 1e-3, "n={n}");
    }
}
