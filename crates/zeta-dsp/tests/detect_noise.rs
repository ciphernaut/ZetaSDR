//! The detector's defaults against a real receiver's noise: a SigMF capture of a band
//! with nothing in it (`scratch/live/record-noise.sh` records 10 s of 400-406 MHz on
//! the field host's B210), named by `ZETA_NOISE_CAPTURE`, kept outside the repo. Its rows,
//! cut as a `watch` cuts them and replayed for 60 s, must announce no signal.
//!
//! `ZETA_NOISE_CAPTURE=~/zeta-fixtures/b210-noise-403m.sigmf-meta \
//!  cargo test -p zeta-dsp --test detect_noise -- --ignored`

use num_complex::Complex;
use zeta_dsp::detect::{DetectParams, SignalState, SpectrumRowView, Tracker, runs};
use zeta_dsp::fft::{CpuFft, FftBackend, STEP_DB, Window, quantise_row};

/// How long the rows are replayed for.
const REPLAY_S: u64 = 60;
/// Rows a second, as a `watch` cuts them.
const ROWS_PER_S: u64 = zeta_core::ops::SPECTRUM_ROWS_PER_S as u64;

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
        .expect("ZETA_NOISE_CAPTURE names a .sigmf-meta");
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

#[test]
#[ignore = "needs a recorded noise capture in ZETA_NOISE_CAPTURE"]
fn a_recorded_noise_capture_yields_no_signal_in_60_s() {
    let path = std::env::var("ZETA_NOISE_CAPTURE")
        .expect("ZETA_NOISE_CAPTURE names the noise capture's .sigmf-meta");
    let (iq, rate, center) = read_capture(&path);
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
    for n in 0..REPLAY_S * ROWS_PER_S {
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
    assert!(
        announced.is_empty(),
        "{} signals announced on noise: {:?}",
        announced.len(),
        announced.iter().map(|s| s.summary()).collect::<Vec<_>>()
    );
}
