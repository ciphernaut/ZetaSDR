//! The detector's defaults against a real receiver's noise: a SigMF capture of a band
//! with nothing in it (9.3 s of the
//! watch's band on a B210 with the input terminated, recorded by the operator), named by
//! `ZETA_NOISE_CAPTURE`, kept outside the repo. Its rows, cut as a `watch` cuts them and
//! replayed for 60 s, must announce no signal.
//!
//! `ZETA_NOISE_CAPTURE=~/zeta-fixtures/b210-noise-403m.sigmf-meta \
//!  cargo test -p zeta-dsp --test detect_noise -- --ignored`
//!
//! The positive control is the same recording with the antenna on
//! (`b210-air-403m`), named by `ZETA_AIR_CAPTURE`: the 404.025 MHz telemetry link that bursts about once a
//! second must be announced within the first `AIR_ANNOUNCE_S` seconds of the replay.
//!
//! `ZETA_AIR_CAPTURE=~/zeta-fixtures/b210-air-403m.sigmf-meta \
//!  cargo test -p zeta-dsp --test detect_noise -- --ignored`

use num_complex::Complex;
use zeta_dsp::detect::{DetectParams, Signal, SignalState, SpectrumRowView, Tracker, runs};
use zeta_dsp::fft::{CpuFft, FftBackend, STEP_DB, Window, quantise_row};

/// How long the noise rows are replayed for.
const REPLAY_S: u64 = 60;
/// Rows a second, as a `watch` cuts them.
const ROWS_PER_S: u64 = zeta_core::ops::SPECTRUM_ROWS_PER_S as u64;
/// The link the antenna-on capture must show, and how far from it the detector's centre
/// may land (it is good to about one bin).
const AIR_LINK_HZ: u64 = 404_025_000;
const AIR_LINK_TOLERANCE_HZ: u64 = 20_000;
/// How soon the link must be announced: its bursts are about a second apart and the
/// detector wants `persist_ms` of sightings before `new`.
const AIR_ANNOUNCE_S: u64 = 10;

/// The capture's samples, its rate and its centre.
fn read_capture(meta_path: &str) -> (Vec<Complex<f32>>, u64, u64) {
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(meta_path).expect("read the meta"))
            .expect("the meta is JSON");
    let global = &meta["global"];
    let rate = global["core:sample_rate"]
        .as_f64()
        .expect("core:sample_rate") as u64;
    let center = meta["captures"][0]["core:frequency"]
        .as_f64()
        .expect("captures[0].core:frequency") as u64;
    let datatype = global["core:datatype"].as_str().expect("core:datatype");
    let data_path = meta_path
        .strip_suffix(".sigmf-meta")
        .map(|stem| format!("{stem}.sigmf-data"))
        .expect("the capture is named by its .sigmf-meta");
    let bytes = std::fs::read(&data_path).expect("read the data");
    let iq = match datatype {
        "cf32_le" => bytes
            .chunks_exact(8)
            .map(|b| {
                Complex::new(
                    f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                    f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                )
            })
            .collect(),
        "ci16_le" => bytes
            .chunks_exact(4)
            .map(|b| {
                Complex::new(
                    f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
                    f32::from(i16::from_le_bytes([b[2], b[3]])) / 32768.0,
                )
            })
            .collect(),
        "ci8" | "ci8_le" => bytes
            .chunks_exact(2)
            .map(|b| Complex::new(f32::from(b[0] as i8) / 128.0, f32::from(b[1] as i8) / 128.0))
            .collect(),
        other => panic!("unsupported core:datatype {other}"),
    };
    (iq, rate, center)
}

/// The capture's rows, cut as a `watch` cuts them and replayed (looped) for `replay_s`
/// through the detector's defaults; the `new` signals announced, in order.
fn announced_new(meta_path: &str, replay_s: u64) -> Vec<Signal> {
    let (iq, rate, center) = read_capture(meta_path);
    let size = zeta_core::ops::watch_fft_size(rate) as usize;
    let per_row = (rate / ROWS_PER_S) as usize;
    let fft = CpuFft::new();
    let rows: Vec<(f32, Vec<u8>)> = iq
        .chunks_exact(per_row)
        .map(|chunk| quantise_row(&fft.power_sum(chunk, size, Window::Hann).db()))
        .collect();
    assert!(!rows.is_empty(), "the capture is shorter than one row");
    let p = DetectParams::default();
    let bin_hz = rate as f64 / size as f64;
    let mut tracker = Tracker::new(&p, bin_hz);
    let mut announced = Vec::new();
    let row_ns = 1_000_000_000 / ROWS_PER_S;
    for n in 0..replay_s * ROWS_PER_S {
        let (floor_db, q) = &rows[n as usize % rows.len()];
        let row = SpectrumRowView {
            bin_hz,
            center_hz: center,
            floor_db: *floor_db,
            step_db: STEP_DB,
            q,
            masked: &[],
        };
        announced.extend(
            tracker
                .observe(n * row_ns, &runs(&row, &p))
                .into_iter()
                .filter(|s| s.state == SignalState::New),
        );
    }
    announced
}

#[test]
#[ignore = "needs a recorded noise capture in ZETA_NOISE_CAPTURE"]
fn a_recorded_noise_capture_yields_no_signal_in_60_s() {
    let path = std::env::var("ZETA_NOISE_CAPTURE")
        .expect("ZETA_NOISE_CAPTURE names the noise capture's .sigmf-meta");
    let announced = announced_new(&path, REPLAY_S);
    assert!(
        announced.is_empty(),
        "{} signals announced on noise: {:?}",
        announced.len(),
        announced.iter().map(|s| s.summary()).collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "needs a recorded antenna-on capture in ZETA_AIR_CAPTURE"]
fn a_recorded_antenna_on_capture_announces_the_404_025_mhz_link() {
    let path = std::env::var("ZETA_AIR_CAPTURE")
        .expect("ZETA_AIR_CAPTURE names the antenna-on capture's .sigmf-meta");
    let announced = announced_new(&path, AIR_ANNOUNCE_S);
    let summaries: Vec<String> = announced.iter().map(|s| s.summary()).collect();
    eprintln!("announced in {AIR_ANNOUNCE_S} s: {summaries:?}");
    let link = announced
        .iter()
        .find(|s| s.center_hz.abs_diff(AIR_LINK_HZ) <= AIR_LINK_TOLERANCE_HZ);
    assert!(
        link.is_some(),
        "no signal within {AIR_LINK_TOLERANCE_HZ} Hz of {AIR_LINK_HZ} Hz in {AIR_ANNOUNCE_S} s; announced: {summaries:?}"
    );
}
