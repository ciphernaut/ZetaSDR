//! CPU/GPU FFT boundary: identical results, selection by availability.
#![cfg(feature = "gpu")]

use num_complex::Complex;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_dsp::fft::{CpuFft, FftBackend, Window};
use zeta_dsp::gpu_fft::GpuFft;

/// Tone plus deterministic pseudo-random noise, 4 chunks of 1024.
fn tone_and_noise(n: usize) -> Vec<Complex<f32>> {
    let mut s: u32 = 0x1234_5678;
    let mut rnd = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s as f32 / u32::MAX as f32) - 0.5
    };
    (0..n)
        .map(|i| {
            let ph = 2.0 * std::f64::consts::PI * 6_000.0 * i as f64 / 48_000.0;
            Complex::new(ph.cos() as f32, ph.sin() as f32) * 0.5 + Complex::new(rnd(), rnd()) * 0.01
        })
        .collect()
}

#[test]
fn gpu_matches_cpu_within_0_1_db() {
    let Some(gpu) = GpuFft::try_new() else {
        eprintln!("skipped: no adapter");
        return;
    };
    assert_eq!(gpu.backend(), Backend::Gpu);
    let x = tone_and_noise(4096);
    let cpu = CpuFft::new().power_spectrum_db(&x, 1024, Window::Hann);
    let g = gpu.power_spectrum_db(&x, 1024, Window::Hann);
    assert_eq!(g.len(), cpu.len());
    for (k, (a, b)) in cpu.iter().zip(&g).enumerate() {
        assert!((a - b).abs() <= 0.1, "bin {k}: cpu {a} dB, gpu {b} dB");
    }
}

#[test]
fn select_prefers_gpu_when_available_else_cpu() {
    let mut reg = StageRegistry::new();
    zeta_dsp::register_all(&mut reg);
    let cpu_only = reg.select("fft", &[Backend::Cpu]).expect("cpu fft");
    assert_eq!(cpu_only.backend(), Backend::Cpu);
    let both = reg
        .select("fft", &[Backend::Cpu, Backend::Gpu])
        .expect("an fft stage is always selectable");
    let has_gpu = GpuFft::try_new().is_some();
    assert_eq!(both.backend() == Backend::Gpu, has_gpu);
    assert_eq!(zeta_dsp::gpu_available(), has_gpu);
    if !has_gpu {
        eprintln!("no adapter: select fell back to cpu");
    }
}
