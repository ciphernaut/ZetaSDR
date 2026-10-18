//! GPU FFT backend (feature `gpu`): the per-chunk `|X|^2` runs in a wgpu compute
//! shader; windowing, averaging, normalisation, fftshift and dB stay in
//! [`chunk_average`], shared with [`CpuFft`], so both backends have identical
//! semantics.
//!
//! The shader is a direct O(N^2) DFT, one invocation per output bin. It proves
//! the CPU/GPU stage boundary; a radix-2 FFT shader replaces it when throughput
//! matters.

use std::sync::mpsc;

use num_complex::Complex;
use wgpu::util::DeviceExt;
use zeta_core::stage::Backend;

use crate::fft::{CpuFft, FftBackend, Window, chunk_average};

const WORKGROUP: u32 = 64;

/// wgpu-backed [`FftBackend`]. Construct with [`GpuFft::try_new`].
pub struct GpuFft {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    /// Used when a GPU submission fails mid-job, so a job never fails for lack of a GPU.
    cpu: CpuFft,
}

impl GpuFft {
    /// A hardware adapter, its device and the DFT pipeline, or `None` when any
    /// step fails. Software adapters (e.g. lavapipe) are rejected: they are not
    /// a GPU and would be slower than [`CpuFft`]. Logs nothing; the caller decides.
    pub fn try_new() -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dft"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/dft.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("dft"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Some(Self {
            device,
            queue,
            pipeline,
            cpu: CpuFft::new(),
        })
    }

    /// `|DFT(chunk)|^2` per bin in natural order, or `None` if the GPU fails.
    fn dft_mag2(&self, chunk: &[Complex<f32>], twiddles: &wgpu::Buffer) -> Option<Vec<f32>> {
        let n = chunk.len();
        let bytes = (n * size_of::<f32>()) as u64;
        let input: Vec<[f32; 2]> = chunk.iter().map(|c| [c.re, c.im]).collect();
        let x = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dft x"),
                contents: bytemuck::cast_slice(&input),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dft params"),
                contents: bytemuck::cast_slice(&[n as u32, 0, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dft out"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dft staging"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dft"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: x.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: twiddles.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: out.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("dft") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((n as u32).div_ceil(WORKGROUP), 1, 1);
        }
        enc.copy_buffer_to_buffer(&out, 0, &staging, 0, bytes);
        self.queue.submit([enc.finish()]);

        let slice = staging.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let view = slice.get_mapped_range().ok()?;
        let mag2 = bytemuck::cast_slice::<u8, f32>(&view).to_vec();
        drop(view);
        staging.unmap();
        Some(mag2)
    }

    /// `exp(-j*2*pi*m/n)` for `m in 0..n`, computed in f64 and uploaded once per call.
    fn twiddles(&self, n: usize) -> wgpu::Buffer {
        let tw: Vec<[f32; 2]> = (0..n)
            .map(|m| {
                let a = -2.0 * std::f64::consts::PI * m as f64 / n as f64;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dft twiddles"),
                contents: bytemuck::cast_slice(&tw),
                usage: wgpu::BufferUsages::STORAGE,
            })
    }
}

impl FftBackend for GpuFft {
    fn backend(&self) -> Backend {
        Backend::Gpu
    }

    fn power_spectrum_db(&self, x: &[Complex<f32>], size: usize, window: Window) -> Vec<f32> {
        let twiddles = self.twiddles(size);
        let failed = std::cell::Cell::new(false);
        let db = chunk_average(x, size, window, |chunk| {
            if failed.get() {
                return vec![0.0; size];
            }
            self.dft_mag2(chunk, &twiddles).unwrap_or_else(|| {
                failed.set(true);
                vec![0.0; size]
            })
        });
        if failed.get() {
            self.cpu.power_spectrum_db(x, size, window)
        } else {
            db
        }
    }
}
