//! GPU FFT backend (feature `gpu`): the per-chunk `|X|^2` runs in a wgpu compute
//! shader; windowing, averaging, normalisation, fftshift and dB stay in
//! [`chunk_average`], shared with [`CpuFft`], so both backends have identical
//! semantics.
//!
//! The shader is a direct O(N^2) DFT, one invocation per output bin. It proves
//! the CPU/GPU stage boundary; a radix-2 FFT shader replaces it when throughput
//! matters.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, mpsc};

use num_complex::Complex;
use wgpu::util::DeviceExt;
use zeta_core::stage::Backend;

use crate::fft::{CpuFft, FftBackend, Window, chunk_average};

const WORKGROUP: u32 = 64;

/// Set to `1` to let [`GpuFft::try_new`] accept a software adapter (lavapipe,
/// llvmpipe), so CI without a GPU still exercises the GPU path. Default off.
pub const ALLOW_SOFTWARE_ENV: &str = "ZETA_GPU_ALLOW_SOFTWARE";

/// Device, queue and pipeline, created at most once per process per adapter policy.
struct Shared {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    adapter: String,
    /// Errors no error scope captured (out of memory, internal, device lost).
    /// Installing the handler also replaces wgpu's default, which panics.
    faults: Arc<AtomicU64>,
}

static HARDWARE: OnceLock<Option<Arc<Shared>>> = OnceLock::new();
static ANY: OnceLock<Option<Arc<Shared>>> = OnceLock::new();

/// wgpu-backed [`FftBackend`]. Construct with [`GpuFft::try_new`].
pub struct GpuFft {
    shared: Arc<Shared>,
    /// Used when a GPU submission fails, so a job never fails for lack of a GPU.
    cpu: CpuFft,
    fallbacks: AtomicU64,
}

impl GpuFft {
    /// A hardware adapter, its device and the DFT pipeline, or `None` when any
    /// step fails (including a pipeline validation error). Software adapters
    /// (lavapipe, llvmpipe) are rejected because they are not a GPU and would
    /// be slower than [`CpuFft`], unless [`ALLOW_SOFTWARE_ENV`] is `1`. The device is
    /// created once per process and shared by every `GpuFft`. Logs nothing;
    /// the caller decides.
    pub fn try_new() -> Option<Self> {
        let allow_software = std::env::var(ALLOW_SOFTWARE_ENV).is_ok_and(|v| v == "1");
        let cell = if allow_software { &ANY } else { &HARDWARE };
        let shared = cell.get_or_init(|| Shared::new(allow_software).map(Arc::new));
        Some(Self {
            shared: shared.clone()?,
            cpu: CpuFft::new(),
            fallbacks: AtomicU64::new(0),
        })
    }

    /// Name of the adapter the device was created on.
    pub fn adapter_name(&self) -> &str {
        &self.shared.adapter
    }

    /// How many `power_spectrum_db` calls fell back to the CPU after a GPU failure.
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks.load(Ordering::Relaxed)
    }

    /// `|DFT(chunk)|^2` per bin in natural order, or why the GPU could not produce it.
    fn dft_mag2(
        &self,
        chunk: &[Complex<f32>],
        twiddles: &wgpu::Buffer,
    ) -> Result<Vec<f32>, String> {
        let sh = &*self.shared;
        let faults = sh.faults.load(Ordering::Relaxed);
        let n = chunk.len();
        let bytes = (n * size_of::<f32>()) as u64;
        let input: Vec<[f32; 2]> = chunk.iter().map(|c| [c.re, c.im]).collect();
        let scope = sh.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let x = sh
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dft x"),
                contents: bytemuck::cast_slice(&input),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params = sh
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("dft params"),
                contents: bytemuck::cast_slice(&[n as u32, 0, 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let out = sh.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dft out"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = sh.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dft staging"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = sh.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dft"),
            layout: &sh.pipeline.get_bind_group_layout(0),
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
        let mut enc = sh
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("dft") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&sh.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((n as u32).div_ceil(WORKGROUP), 1, 1);
        }
        enc.copy_buffer_to_buffer(&out, 0, &staging, 0, bytes);
        sh.queue.submit([enc.finish()]);
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(format!("validation: {e}"));
        }

        let slice = staging.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        sh.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("poll: {e}"))?;
        rx.recv()
            .map_err(|e| format!("map: {e}"))?
            .map_err(|e| format!("map: {e}"))?;
        let mag2 = {
            let view = slice
                .get_mapped_range()
                .map_err(|e| format!("mapped range: {e}"))?;
            bytemuck::cast_slice::<u8, f32>(&view).to_vec()
        };
        staging.unmap();
        if sh.faults.load(Ordering::Relaxed) != faults {
            return Err("uncaptured device error".into());
        }
        Ok(mag2)
    }

    /// `exp(-j*2*pi*m/n)` for `m in 0..n`, computed in f64 and uploaded once per call.
    fn twiddles(&self, n: usize) -> Result<wgpu::Buffer, String> {
        let tw: Vec<[f32; 2]> = (0..n)
            .map(|m| {
                let a = -2.0 * std::f64::consts::PI * m as f64 / n as f64;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        let device = &self.shared.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dft twiddles"),
            contents: bytemuck::cast_slice(&tw),
            usage: wgpu::BufferUsages::STORAGE,
        });
        match pollster::block_on(scope.pop()) {
            Some(e) => Err(format!("validation: {e}")),
            None => Ok(buf),
        }
    }

    /// The GPU spectrum, or the first failure's reason.
    fn try_spectrum(
        &self,
        x: &[Complex<f32>],
        size: usize,
        window: Window,
    ) -> Result<Vec<f32>, String> {
        let twiddles = self.twiddles(size)?;
        let failure = std::cell::RefCell::new(None);
        let db = chunk_average(x, size, window, |chunk| {
            if failure.borrow().is_some() {
                return vec![0.0; size];
            }
            self.dft_mag2(chunk, &twiddles).unwrap_or_else(|e| {
                *failure.borrow_mut() = Some(e);
                vec![0.0; size]
            })
        });
        match failure.into_inner() {
            Some(e) => Err(e),
            None => Ok(db),
        }
    }
}

impl Shared {
    fn new(allow_software: bool) -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu && !allow_software {
            return None;
        }
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
        let faults = Arc::new(AtomicU64::new(0));
        let f = faults.clone();
        device.on_uncaptured_error(Arc::new(move |_| {
            f.fetch_add(1, Ordering::Relaxed);
        }));
        let f = faults.clone();
        device.set_device_lost_callback(move |_, _| {
            f.fetch_add(1, Ordering::Relaxed);
        });
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
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
        if pollster::block_on(scope.pop()).is_some() || faults.load(Ordering::Relaxed) > 0 {
            return None;
        }
        Some(Self {
            device,
            queue,
            pipeline,
            adapter: info.name,
            faults,
        })
    }
}

impl FftBackend for GpuFft {
    fn backend(&self) -> Backend {
        Backend::Gpu
    }

    fn power_spectrum_db(&self, x: &[Complex<f32>], size: usize, window: Window) -> Vec<f32> {
        self.try_spectrum(x, size, window).unwrap_or_else(|reason| {
            self.fallbacks.fetch_add(1, Ordering::Relaxed);
            eprintln!("zeta-dsp: gpu fft failed ({reason}); computed on cpu");
            self.cpu.power_spectrum_db(x, size, window)
        })
    }
}
