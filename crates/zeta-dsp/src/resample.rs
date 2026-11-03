//! Streaming rational resampler: upsample by L, low-pass, keep every Mth.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind, Timestamp};

use crate::util::lowpass_taps;
use crate::{Input, frame_span, is_seam, run_map};

/// Low-pass cutoff as a fraction of the lower of the two rates.
const CUTOFF: f64 = 0.45;

/// Prototype taps per unit of `max(L, M)`.
const TAPS_PER_FACTOR: u64 = 8;

/// Fewest prototype taps: small factors (96k→48k gives 17 by the formula)
/// would otherwise leave a transition band too wide to stop aliasing.
const MIN_TAPS: u64 = 63;

/// Largest `max(L, M)` accepted, so at most 800 001 prototype taps (the
/// 63-tap floor only applies below `max(L, M) = 8`). Coprime rates (a
/// prime source rate, say) otherwise reduce to factors in the millions and a
/// prototype of tens of megabytes.
pub const MAX_FACTOR: u64 = 100_000;

/// Largest `max(L, M)` kept exact; above it the input rate is snapped.
pub const SNAP_FACTOR: u64 = 2_048;

/// Furthest the input rate may move when snapping, in parts per million.
pub const SNAP_MAX_PPM: u64 = 1_000;

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Streaming resampler from `in_rate_hz` to `out_rate_hz` by the reduced
/// ratio L/M. The prototype is a Hann-windowed sinc low-pass at
/// `0.45 × min(in, out)` designed at the upsampled rate `in × L`, with
/// `max(8 × max(L, M) + 1, 63)` taps (odd either way) and gain L. Evaluation is polyphase: output `m`
/// sits at upsampled index `m × M`, uses phase `p = m × M mod L` and the input
/// samples ending at `m × M / L`, so the zero-stuffed signal never exists.
/// Input history and the output phase carry across [`Resampler::process`]
/// calls, so any block split gives the same output as one call.
#[derive(Debug, Clone)]
pub struct Resampler {
    l: u64,
    m: u64,
    /// `phases[p][j] = L × h[p + j L]`, zero-padded to a common length.
    phases: Vec<Vec<f32>>,
    /// Last `phases[0].len() - 1` input samples, oldest first (the
    /// `taps.len() - 1` upsampled samples the FIR spans, minus the stuffed zeros).
    history: Vec<f32>,
    /// Upsampled index of the next output, relative to the next block's first sample.
    next: u64,
    /// The input rate the ratio was built for: the requested rate, or the snapped one.
    input_rate_hz: u64,
}

impl Resampler {
    /// Resampler from `in_rate_hz` to `out_rate_hz`; both must be at least 1.
    /// The ratio is exact when its reduced larger term is at most
    /// [`SNAP_FACTOR`]; above that the input rate snaps to the nearest rate
    /// within [`SNAP_MAX_PPM`] (searching `in + 1`, `in - 1`, `in + 2`, ...)
    /// whose ratio qualifies, and it is an error only when none does.
    pub fn new(in_rate_hz: u64, out_rate_hz: u64) -> Result<Resampler, String> {
        if in_rate_hz == 0 || out_rate_hz == 0 {
            return Err(format!(
                "resampler rates must be >= 1, got {in_rate_hz} -> {out_rate_hz}"
            ));
        }
        let reduce = |i: u64| {
            let g = gcd(i, out_rate_hz);
            (out_rate_hz / g, i / g)
        };
        let (mut l, mut m) = reduce(in_rate_hz);
        let mut in_rate_hz_used = in_rate_hz;
        if l.max(m) > SNAP_FACTOR {
            let reach = in_rate_hz * SNAP_MAX_PPM / 1_000_000;
            let found = (1..=reach)
                .flat_map(|k| [in_rate_hz.checked_add(k), in_rate_hz.checked_sub(k)])
                .flatten()
                .filter(|&c| c != 0)
                .map(|c| (c, reduce(c)))
                .find(|&(_, (cl, cm))| cl.max(cm) <= SNAP_FACTOR);
            if let Some((c, (cl, cm))) = found {
                let ppm = c.abs_diff(in_rate_hz) as f64 * 1e6 / in_rate_hz as f64;
                eprintln!(
                    "zeta-dsp: resampler {in_rate_hz} -> {out_rate_hz} Hz snapped to {c} ({ppm:.0} ppm)"
                );
                (l, m) = (cl, cm);
                in_rate_hz_used = c;
            }
        }
        let in_rate_hz = in_rate_hz_used;
        if l == 1 && m == 1 {
            return Ok(Resampler {
                l,
                m,
                phases: Vec::new(),
                history: Vec::new(),
                next: 0,
                input_rate_hz: in_rate_hz,
            });
        }
        if l.max(m) > SNAP_FACTOR {
            return Err(format!(
                "resampler {in_rate_hz} -> {out_rate_hz} Hz reduces to {l}/{m}, beyond the {SNAP_FACTOR} limit and no rate within {SNAP_MAX_PPM} ppm snaps"
            ));
        }
        let n = (TAPS_PER_FACTOR * l.max(m) + 1).max(MIN_TAPS) as usize;
        let cutoff = CUTOFF * in_rate_hz.min(out_rate_hz) as f64;
        let h = lowpass_taps(cutoff, in_rate_hz as f64 * l as f64, n);
        let lu = l as usize;
        let k = n.div_ceil(lu);
        let gain = l as f32;
        let phases: Vec<Vec<f32>> = (0..lu)
            .map(|p| {
                (0..k)
                    .map(|j| h.get(p + j * lu).map_or(0.0, |&t| t * gain))
                    .collect()
            })
            .collect();
        Ok(Resampler {
            l,
            m,
            history: vec![0.0; k - 1],
            phases,
            next: 0,
            input_rate_hz: in_rate_hz,
        })
    }

    /// The reduced ratio `(L, M)`: output rate / input rate = L / M.
    pub fn ratio(&self) -> (u64, u64) {
        (self.l, self.m)
    }

    /// The input rate the ratio was built for: the requested rate when exact,
    /// the snapped rate otherwise.
    pub fn input_rate_hz(&self) -> u64 {
        self.input_rate_hz
    }

    /// Resample the next block of input. Over a stream of N inputs fed to a
    /// fresh resampler the outputs total `ceil(N × L / M)`; a single block's
    /// count varies by one because `next` carries the remainder between calls.
    pub fn process(&mut self, x: &[f32]) -> Vec<f32> {
        if self.l == 1 && self.m == 1 {
            return x.to_vec();
        }
        let h = self.history.len();
        let mut ext = Vec::with_capacity(h + x.len());
        ext.extend_from_slice(&self.history);
        ext.extend_from_slice(x);
        let end = x.len() as u64 * self.l;
        let mut out = Vec::with_capacity(((end.saturating_sub(self.next)) / self.m + 1) as usize);
        let mut t = self.next;
        while t < end {
            let i = (t / self.l) as usize;
            let taps = &self.phases[(t % self.l) as usize];
            // ext[h + i] is x[i]; tap j multiplies x[i - j].
            let acc = taps
                .iter()
                .zip(ext[..=h + i].iter().rev())
                .fold(0.0f32, |acc, (&c, &s)| acc + c * s);
            out.push(acc);
            t += self.m;
        }
        self.next = t - end;
        if h > 0 {
            self.history.copy_from_slice(&ext[ext.len() - h..]);
        }
        out
    }
}

/// The stage kind, as graphs name it.
pub const RESAMPLE_KIND: &str = "resample";

/// `resample` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ResampleParams {
    /// The output sample rate in hertz.
    pub out_rate_hz: u64,
}

/// Resamples an IQ stream to `out_rate_hz`, one [`Resampler`] each for I and Q. Both are
/// rebuilt (fresh history) on a rate change, a time seam ([`is_seam`]) or lagged input.
pub struct ResampleStage {
    spec: StageSpec,
}

impl ResampleStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: RESAMPLE_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Iq],
                params_schema: schemars::schema_for!(ResampleParams),
            },
        }
    }
}

impl Default for ResampleStage {
    fn default() -> Self {
        Self::new()
    }
}

/// The I and Q resamplers of the current run of contiguous blocks.
struct Pair {
    in_rate_hz: u64,
    i: Resampler,
    q: Resampler,
}

#[async_trait]
impl Stage for ResampleStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: ResampleParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if p.out_rate_hz == 0 {
            return Err("out_rate_hz: at least 1".into());
        }
        Ok(())
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        self.check_params(&params).map_err(StageError::Failed)?;
        let p: ResampleParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("resample params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let out_rate_hz = p.out_rate_hz;
        let mut pair: Option<Pair> = None;
        let mut prev_end: Option<u64> = None;
        run_map(RESAMPLE_KIND, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    pair = None;
                    prev_end = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "resample stage expects iq frames".into(),
                ));
            };
            let rate = block.sample_rate_hz.0;
            let seam = prev_end.is_some_and(|e| is_seam(e, frame.provenance.span.start_ns.0, rate));
            prev_end = Some(frame.provenance.span.end_ns.0);
            if seam || pair.as_ref().is_some_and(|c| c.in_rate_hz != rate) {
                pair = None;
            }
            if pair.is_none() {
                let make = || {
                    Resampler::new(rate, out_rate_hz).map_err(|e| {
                        StageError::Failed(format!("resample {rate} -> {out_rate_hz} Hz: {e}"))
                    })
                };
                pair = Some(Pair {
                    in_rate_hz: rate,
                    i: make()?,
                    q: make()?,
                });
            }
            let cur = pair.as_mut().expect("built above");
            let re: Vec<f32> = block.samples.iter().map(|c| c.re).collect();
            let im: Vec<f32> = block.samples.iter().map(|c| c.im).collect();
            let (re, im) = (cur.i.process(&re), cur.q.process(&im));
            if re.is_empty() {
                return Ok(Vec::new());
            }
            let t0: Timestamp = block.t0;
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(t0, re.len(), out_rate_hz);
            let samples: Vec<_> = re
                .into_iter()
                .zip(im)
                .map(|(r, i)| num_complex::Complex::new(r, i))
                .collect();
            Ok(vec![Frame::new(
                out.clone(),
                prov,
                Payload::Iq(IqBlock {
                    t0,
                    sample_rate_hz: Hz(out_rate_hz),
                    center_hz: block.center_hz,
                    samples: samples.into(),
                }),
            )])
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use num_complex::Complex;
    use rustfft::FftPlanner;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamId, StreamKind, Timestamp};

    use super::{ResampleStage, Resampler, frame_span};

    const IN_RATE: u64 = 48_000;
    const OUT_RATE: u64 = 38_400;
    const BLOCK: usize = 4_800;
    const TONE_HZ: f64 = 1_000.0;
    const CENTER: u64 = 144_800_000;
    const T0: u64 = 1_000_000_000;
    const SEAM_GAP_NS: u64 = 2_000_000_000;
    const CHECK: usize = 64;
    const SETTLE: Duration = Duration::from_millis(300);

    fn tone(from: usize, n: usize) -> Vec<Complex<f32>> {
        (from..from + n)
            .map(|i| {
                let ph = 2.0 * std::f64::consts::PI * TONE_HZ * i as f64 / IN_RATE as f64;
                Complex::from_polar(1.0, ph as f32)
            })
            .collect()
    }

    fn prov(t0: u64, n: usize) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(t0), n, IN_RATE),
        }
    }

    fn frame(id: &StreamId, t0: u64, samples: Vec<Complex<f32>>) -> Frame {
        Frame::new(
            id.clone(),
            prov(t0, samples.len()),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0),
                sample_rate_hz: Hz(IN_RATE),
                center_hz: Hz(CENTER),
                samples: samples.into(),
            }),
        )
    }

    /// Feed `blocks` of `(t0, samples)` through the stage; every output block.
    async fn run(blocks: Vec<(u64, Vec<Complex<f32>>)>) -> Vec<IqBlock> {
        let bus = Arc::new(StreamBus::new(256));
        let inp = bus.register(StreamKind::Iq, prov(0, 0));
        let out = bus.register(StreamKind::Iq, prov(0, 0));
        let rx_in = bus.subscribe(&inp).unwrap();
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (b, o) = (bus.clone(), out.clone());
        let handle = tokio::spawn(async move {
            ResampleStage::new()
                .run(
                    serde_json::json!({"out_rate_hz": OUT_RATE}),
                    vec![rx_in],
                    vec![o],
                    b,
                    CancellationToken::new(),
                    StageContext::new(SourceId("s".into())),
                )
                .await
        });
        for (t0, s) in blocks {
            bus.publish(frame(&inp, t0, s));
        }
        let mut got = Vec::new();
        while let Ok(Ok(f)) = tokio::time::timeout(SETTLE, rx_out.recv()).await {
            let Payload::Iq(b) = f.payload else {
                panic!("an iq frame")
            };
            got.push(b);
        }
        handle.abort();
        got
    }

    #[tokio::test]
    async fn a_tone_keeps_its_frequency_across_the_rate_change() {
        let step = BLOCK as u64 * 1_000_000_000 / IN_RATE;
        let blocks = (0..4)
            .map(|k| (T0 + k as u64 * step, tone(k * BLOCK, BLOCK)))
            .collect();
        let got = run(blocks).await;
        assert!(got.iter().all(|b| b.sample_rate_hz == Hz(OUT_RATE)));
        assert!(got.iter().all(|b| b.center_hz == Hz(CENTER)));
        assert_eq!(got[0].t0, Timestamp(T0));
        let all: Vec<Complex<f32>> = got.iter().flat_map(|b| b.samples.iter().copied()).collect();
        let want = 4 * BLOCK * OUT_RATE as usize / IN_RATE as usize;
        assert!(all.len().abs_diff(want) <= 1, "{} vs {want}", all.len());
        let n = all.len();
        let mut buf = all;
        FftPlanner::<f32>::new()
            .plan_fft_forward(n)
            .process(&mut buf);
        let peak = (0..n)
            .max_by(|&a, &b| buf[a].norm().total_cmp(&buf[b].norm()))
            .unwrap();
        let bin_hz = OUT_RATE as f64 / n as f64;
        let hz = peak as f64 * bin_hz;
        assert!((hz - TONE_HZ).abs() <= bin_hz, "peak at {hz} Hz");
    }

    #[tokio::test]
    async fn a_seam_rebuilds_the_filter() {
        let first = tone(0, BLOCK);
        let second = tone(BLOCK, BLOCK);
        let t1 = T0 + BLOCK as u64 * 1_000_000_000 / IN_RATE + SEAM_GAP_NS;
        let got = run(vec![(T0, first), (t1, second.clone())]).await;
        assert_eq!(got.len(), 2);
        let mut fresh_i = Resampler::new(IN_RATE, OUT_RATE).unwrap();
        let mut fresh_q = Resampler::new(IN_RATE, OUT_RATE).unwrap();
        let re: Vec<f32> = second.iter().map(|c| c.re).collect();
        let im: Vec<f32> = second.iter().map(|c| c.im).collect();
        let (re, im) = (fresh_i.process(&re), fresh_q.process(&im));
        assert_eq!(got[1].t0, Timestamp(t1));
        for k in 0..CHECK {
            let g = got[1].samples[k];
            assert!(
                (g.re - re[k]).abs() < 1e-5 && (g.im - im[k]).abs() < 1e-5,
                "{k}"
            );
        }
    }

    #[test]
    fn out_rate_zero_is_refused() {
        let st = ResampleStage::new();
        assert_eq!(
            st.check_params(&serde_json::json!({"out_rate_hz": 0})),
            Err("out_rate_hz: at least 1".to_string())
        );
        assert!(
            st.check_params(&serde_json::json!({"out_rate_hz": 1}))
                .is_ok()
        );
        assert!(
            st.check_params(&serde_json::json!({"out_rate_hz": 1, "x": 1}))
                .is_err()
        );
    }
}
