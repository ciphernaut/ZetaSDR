//! The GPU path on a software adapter, opted in with `ZETA_GPU_ALLOW_SOFTWARE=1`.
//! Its own test binary because it sets a process-wide environment variable.
#![cfg(feature = "gpu")]

use num_complex::Complex;
use zeta_dsp::fft::{CpuFft, FftBackend, Window};
use zeta_dsp::gpu_fft::{ALLOW_SOFTWARE_ENV, GpuFft};

#[test]
fn software_adapter_allowed_by_env() {
    // SAFETY: the only test in this binary; no other thread reads the environment.
    unsafe { std::env::set_var(ALLOW_SOFTWARE_ENV, "1") };
    let Some(gpu) = GpuFft::try_new() else {
        eprintln!("skipped: no adapter, software or hardware");
        return;
    };
    eprintln!("adapter: {}", gpu.adapter_name());
    // Same tone + xorshift noise as tests/gpu_fft.rs: every bin well above rounding noise.
    let mut st: u32 = 0x1234_5678;
    let mut rnd = move || {
        st ^= st << 13;
        st ^= st >> 17;
        st ^= st << 5;
        (st as f32 / u32::MAX as f32) - 0.5
    };
    let x: Vec<Complex<f32>> = (0..4096)
        .map(|i| {
            let ph = 2.0 * std::f64::consts::PI * 6_000.0 * i as f64 / 48_000.0;
            Complex::new(ph.cos() as f32, ph.sin() as f32) * 0.5 + Complex::new(rnd(), rnd()) * 0.01
        })
        .collect();
    let cpu = CpuFft::new().power_spectrum_db(&x, 1024, Window::Hann);
    let g = gpu.power_spectrum_db(&x, 1024, Window::Hann);
    assert_eq!(gpu.fallbacks(), 0, "gpu path fell back to cpu");
    let worst = cpu
        .iter()
        .zip(&g)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    eprintln!("max |cpu - gpu| = {worst} dB over {} bins", g.len());
    assert!(worst <= 0.1, "max deviation {worst} dB");
}
