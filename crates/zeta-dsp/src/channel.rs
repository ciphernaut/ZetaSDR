//! Channel stage: shift a channel to baseband, low-pass it and decimate.

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::port::IqBlock;
use zeta_core::stage::{Backend, Stage, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{ChannelId, Hz, StreamId, StreamKind, Timestamp};

use crate::util::lowpass_taps;
use crate::{frame_span, run_map};

/// Fewest and most taps of the channel low-pass filter.
const MIN_TAPS: usize = 63;
const MAX_TAPS: usize = 1025;

/// Channel low-pass length for decimation by `decim`: `8 · decim + 1` taps,
/// at least [`MIN_TAPS`] and at most [`MAX_TAPS`]. The transition band of a
/// Hann-windowed sinc is about `4 · rate / taps` wide, so scaling the length
/// with the decimation keeps it near half the output rate and the adjacent
/// band that would alias onto the channel well attenuated; the cap bounds the
/// per-sample cost (decimation by 128 and above all use 1025 taps).
fn taps_for(decim: usize) -> usize {
    decim
        .saturating_mul(8)
        .saturating_add(1)
        .clamp(MIN_TAPS, MAX_TAPS)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    #[error("center_hz {center_hz} with its bandwidth leaves the block {min_hz}..={max_hz}")]
    OutOfBand {
        center_hz: u64,
        min_hz: u64,
        max_hz: u64,
    },
    #[error("out_rate_hz {out_rate_hz} must divide rate_hz {rate_hz} by an integer >= 1")]
    Rate { rate_hz: u64, out_rate_hz: u64 },
    #[error("bandwidth_hz {bandwidth_hz} must be in 1..=out_rate_hz {out_rate_hz}")]
    Bandwidth { bandwidth_hz: u64, out_rate_hz: u64 },
}

/// Streaming state of one channel extraction: NCO phase, FIR history and
/// decimation phase, carried across blocks so block boundaries are seamless.
/// It resets itself when the rate, offset, bandwidth or output rate change.
#[derive(Debug, Default)]
pub struct ChannelState {
    key: Option<(u64, i64, u64, u64)>,
    taps: Vec<f32>,
    decim: usize,
    /// NCO phase in cycles, in `[0, 1)`.
    phase: f64,
    /// Last `taps.len() - 1` shifted samples, oldest first.
    history: Vec<Complex<f32>>,
    /// Index in the next block of the next sample to output.
    next: usize,
}

impl ChannelState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Bring a signal at `+offset_hz` to baseband (multiply by `exp(-j2π·offset·t)`),
/// low-pass at `bandwidth_hz / 2` (filter length `8 · decim + 1` taps, 63..=1025) and
/// decimate to `out_rate_hz`, continuing from `state`. `bandwidth_hz` must be
/// in `1..=out_rate_hz`. Returns the samples and the achieved output rate.
pub fn extract_offset_streaming(
    state: &mut ChannelState,
    iq: &[Complex<f32>],
    rate_hz: u64,
    offset_hz: i64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Vec<Complex<f32>>, u64), ChannelError> {
    if out_rate_hz == 0 || out_rate_hz > rate_hz || !rate_hz.is_multiple_of(out_rate_hz) {
        return Err(ChannelError::Rate {
            rate_hz,
            out_rate_hz,
        });
    }
    // Wider than the output rate would fold the band's edges onto itself.
    if bandwidth_hz == 0 || bandwidth_hz > out_rate_hz {
        return Err(ChannelError::Bandwidth {
            bandwidth_hz,
            out_rate_hz,
        });
    }
    let key = (rate_hz, offset_hz, bandwidth_hz, out_rate_hz);
    if state.key != Some(key) {
        let decim = (rate_hz / out_rate_hz) as usize;
        let taps = lowpass_taps(bandwidth_hz as f64 / 2.0, rate_hz as f64, taps_for(decim));
        *state = ChannelState {
            key: Some(key),
            history: vec![Complex::new(0.0, 0.0); taps.len() - 1],
            taps,
            decim,
            phase: 0.0,
            next: 0,
        };
    }

    let step = -(offset_hz as f64) / rate_hz as f64;
    let h = state.history.len();
    let mut ext = Vec::with_capacity(h + iq.len());
    ext.extend_from_slice(&state.history);
    ext.extend(iq.iter().enumerate().map(|(k, &x)| {
        let cycles = (state.phase + k as f64 * step).rem_euclid(1.0);
        let (s, c) = (2.0 * std::f64::consts::PI * cycles).sin_cos();
        x * Complex::new(c as f32, s as f32)
    }));
    state.phase = (state.phase + iq.len() as f64 * step).rem_euclid(1.0);

    let out: Vec<Complex<f32>> = (state.next..iq.len())
        .step_by(state.decim)
        .map(|n| {
            state
                .taps
                .iter()
                .enumerate()
                .fold(Complex::new(0.0, 0.0), |acc, (j, &t)| {
                    acc + ext[h + n - j] * t
                })
        })
        .collect();
    // Outputs cover every index from `next` up to the block end, so this cannot underflow.
    state.next = state.next + out.len() * state.decim - iq.len();
    state.history.copy_from_slice(&ext[ext.len() - h..]);
    Ok((out, out_rate_hz))
}

/// One-shot [`extract_offset_streaming`] from fresh state.
pub fn extract_offset(
    iq: &[Complex<f32>],
    rate_hz: u64,
    offset_hz: i64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Vec<Complex<f32>>, u64), ChannelError> {
    let mut state = ChannelState::new();
    extract_offset_streaming(
        &mut state,
        iq,
        rate_hz,
        offset_hz,
        bandwidth_hz,
        out_rate_hz,
    )
}

/// Extract the channel `center_hz ± bandwidth_hz/2` from a block centred on
/// `block_center_hz` at `rate_hz`, at `out_rate_hz`. The channel must lie inside
/// `block_center_hz ± rate_hz/2`.
pub fn extract_channel(
    iq: &[Complex<f32>],
    rate_hz: u64,
    block_center_hz: u64,
    center_hz: u64,
    bandwidth_hz: u64,
    out_rate_hz: u64,
) -> Result<(Vec<Complex<f32>>, u64), ChannelError> {
    check_in_band(rate_hz, block_center_hz, center_hz, bandwidth_hz)?;
    extract_offset(
        iq,
        rate_hz,
        center_hz as i64 - block_center_hz as i64,
        bandwidth_hz,
        out_rate_hz,
    )
}

/// `center ± bw/2` inside `block_center ± rate/2`, compared doubled in i128 so
/// odd values are exact and nothing underflows.
fn check_in_band(
    rate_hz: u64,
    block_center_hz: u64,
    center_hz: u64,
    bandwidth_hz: u64,
) -> Result<(), ChannelError> {
    let (r, bc, c, bw) = (
        rate_hz as i128,
        block_center_hz as i128,
        center_hz as i128,
        bandwidth_hz as i128,
    );
    if 2 * c - bw < 2 * bc - r || 2 * c + bw > 2 * bc + r {
        return Err(ChannelError::OutOfBand {
            center_hz,
            min_hz: block_center_hz.saturating_sub(rate_hz / 2),
            max_hz: block_center_hz.saturating_add(rate_hz / 2),
        });
    }
    Ok(())
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ChannelParams {
    pub center_hz: u64,
    pub bandwidth_hz: u64,
    pub out_rate_hz: u64,
}

/// Extracts one channel from an IQ stream: shift, low-pass, decimate.
pub struct ChannelStage {
    spec: StageSpec,
}

impl ChannelStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "channel",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Iq],
                params_schema: schemars::schema_for!(ChannelParams),
            },
        }
    }
}

impl Default for ChannelStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for ChannelStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
    ) -> Result<(), StageError> {
        let p: ChannelParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("channel params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let channel = ChannelId(format!("{}/{}", p.center_hz, p.bandwidth_hz));
        let mut state = ChannelState::new();
        run_map(inputs, bus, cancel, move |frame| {
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed("channel stage expects iq frames".into()));
            };
            let rate = block.sample_rate_hz.0;
            check_in_band(rate, block.center_hz.0, p.center_hz, p.bandwidth_hz)
                .map_err(|e| StageError::Failed(e.to_string()))?;
            // Time of the first output sample: the decimation phase carried in.
            let lead = state.next;
            let (samples, out_rate) = extract_offset_streaming(
                &mut state,
                &block.samples,
                rate,
                p.center_hz as i64 - block.center_hz.0 as i64,
                p.bandwidth_hz,
                p.out_rate_hz,
            )
            .map_err(|e| StageError::Failed(e.to_string()))?;
            if samples.is_empty() {
                return Ok(Vec::new());
            }
            let mut prov = frame.provenance.clone();
            prov.channel = Some(channel.clone());
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            let t0 = Timestamp(frame_span(block.t0, lead, rate).end_ns.0);
            Ok(vec![Frame::new(
                out.clone(),
                prov,
                Payload::Iq(IqBlock {
                    t0,
                    sample_rate_hz: Hz(out_rate),
                    center_hz: Hz(p.center_hz),
                    samples: samples.into(),
                }),
            )])
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use zeta_core::port::IqBlock;
    use zeta_core::stream::Payload;
    use zeta_core::types::{ChannelId, Hz, Provenance, SourceId, Span, Timestamp};

    use super::*;

    const RATE: u64 = 48_000;
    const BLOCK_CENTER: u64 = 100_000_000;

    fn tone(len: usize, freq_hz: f64) -> Vec<Complex<f32>> {
        crate::util::nco(len, freq_hz, RATE as f64)
    }

    /// Magnitude of the single DFT bin at `freq_hz`.
    fn bin(x: &[Complex<f32>], freq_hz: f64, rate_hz: f64) -> f64 {
        let (re, im) = x.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, v)| {
            let (sn, cs) = (-2.0 * PI * freq_hz * n as f64 / rate_hz).sin_cos();
            let (vr, vi) = (v.re as f64, v.im as f64);
            (re + vr * cs - vi * sn, im + vr * sn + vi * cs)
        });
        (re * re + im * im).sqrt()
    }

    #[test]
    fn extract_shifts_tone_to_baseband() {
        let iq = tone(9600, 5_000.0);
        let (out, rate) =
            extract_channel(&iq, RATE, BLOCK_CENTER, 100_005_000, 10_000, RATE).unwrap();
        assert_eq!((out.len(), rate), (9600, RATE));
        // Skip the filter start-up; 4800 samples puts 5 kHz exactly on a bin.
        let w = &out[100..4900];
        let dc = bin(w, 0.0, RATE as f64);
        let plus5 = bin(w, 5_000.0, RATE as f64);
        let db = 20.0 * (dc / plus5).log10();
        assert!(db >= 20.0, "dc {dc} vs +5 kHz {plus5}: {db} dB");
    }

    #[test]
    fn extract_rejects_out_of_band_naming_center() {
        let iq = tone(480, 5_000.0);
        let err = extract_channel(&iq, RATE, BLOCK_CENTER, 100_100_000, 10_000, RATE).unwrap_err();
        assert_eq!(
            err,
            ChannelError::OutOfBand {
                center_hz: 100_100_000,
                min_hz: 99_976_000,
                max_hz: 100_024_000,
            }
        );
        assert!(err.to_string().contains("center_hz 100100000"), "{err}");
    }

    #[test]
    fn extract_rejects_non_integer_decimation() {
        let iq = tone(480, 5_000.0);
        let err =
            extract_channel(&iq, RATE, BLOCK_CENTER, 100_005_000, 10_000, 20_000).unwrap_err();
        assert_eq!(
            err,
            ChannelError::Rate {
                rate_hz: RATE,
                out_rate_hz: 20_000,
            }
        );
        assert!(err.to_string().contains("out_rate_hz 20000"), "{err}");
    }

    #[test]
    fn extract_streaming_matches_one_shot_across_blocks() {
        // Block lengths that are not multiples of the decimation exercise the carried state.
        let iq = tone(4801, 5_000.0);
        let (whole, _) = extract_offset(&iq, RATE, 5_000, 10_000, 12_000).unwrap();
        let mut st = ChannelState::new();
        let mut parts = Vec::new();
        for chunk in [&iq[..1001], &iq[1001..1003], &iq[1003..]] {
            let (o, r) =
                extract_offset_streaming(&mut st, chunk, RATE, 5_000, 10_000, 12_000).unwrap();
            assert_eq!(r, 12_000);
            parts.extend(o);
        }
        assert_eq!(parts.len(), whole.len());
        for (a, b) in parts.iter().zip(&whole) {
            assert!((a - b).norm() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn wideband_decimation_rejects_adjacent_channel() {
        const WIDE: u64 = 2_400_000;
        let n = WIDE as usize / 5; // 0.2 s
        // Equal-amplitude in-band tone at +1 kHz and adjacent tone at +48 kHz,
        // which aliases onto 0 Hz after decimation to 48 kHz.
        let a = crate::util::nco(n, 1_000.0, WIDE as f64);
        let b = crate::util::nco(n, 48_000.0, WIDE as f64);
        let iq: Vec<Complex<f32>> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let (out, rate) = extract_offset(&iq, WIDE, 0, 12_500, 48_000).unwrap();
        assert_eq!(rate, 48_000);
        // Skip the filter start-up; 4800 samples put 0 and 1 kHz exactly on bins.
        let w = &out[200..5_000];
        let wanted = bin(w, 1_000.0, 48_000.0);
        let alias = bin(w, 0.0, 48_000.0);
        let db = 20.0 * (wanted / alias).log10();
        assert!(
            db >= 60.0,
            "+1 kHz {wanted} vs aliased +48 kHz {alias}: {db} dB"
        );
    }

    #[test]
    fn bandwidth_above_out_rate_is_rejected() {
        let iq = tone(480, 5_000.0);
        let err = extract_offset(&iq, RATE, 0, 20_000, 12_000).unwrap_err();
        assert_eq!(
            err,
            ChannelError::Bandwidth {
                bandwidth_hz: 20_000,
                out_rate_hz: 12_000,
            }
        );
        let msg = err.to_string();
        assert!(msg.contains("bandwidth_hz 20000"), "{msg}");
        assert!(msg.contains("out_rate_hz 12000"), "{msg}");
    }

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        }
    }

    type Run = tokio::task::JoinHandle<Result<(), StageError>>;

    fn spawn(
        params: serde_json::Value,
        cancel: CancellationToken,
    ) -> (
        broadcast::Sender<Frame>,
        StreamId,
        broadcast::Receiver<Frame>,
        Run,
    ) {
        let bus = Arc::new(StreamBus::new(8));
        let out = bus.register(StreamKind::Iq, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(8);
        let o2 = out.clone();
        let h = tokio::spawn(async move {
            ChannelStage::new()
                .run(params, vec![rx_in], vec![o2], bus, cancel)
                .await
        });
        (tx, out, rx_out, h)
    }

    fn iq_frame(samples: Vec<Complex<f32>>) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(1_000_000_000),
                sample_rate_hz: Hz(RATE),
                center_hz: Hz(BLOCK_CENTER),
                samples: samples.into(),
            }),
        )
    }

    fn params(center_hz: u64) -> serde_json::Value {
        serde_json::json!({"center_hz": center_hz, "bandwidth_hz": 10_000, "out_rate_hz": 12_000})
    }

    #[tokio::test]
    async fn channel_stage_emits_decimated_blocks_with_channel_provenance() {
        let (tx, out, mut rx_out, h) = spawn(params(100_005_000), CancellationToken::new());
        tx.send(iq_frame(tone(4800, 5_000.0))).unwrap();
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.stream, out);
        let Payload::Iq(b) = &f.payload else {
            panic!("expected an iq payload")
        };
        assert_eq!(b.samples.len(), 1200);
        assert_eq!(b.sample_rate_hz, Hz(12_000));
        assert_eq!(b.center_hz, Hz(100_005_000));
        assert_eq!(b.t0, Timestamp(1_000_000_000));
        assert_eq!(
            f.provenance.channel,
            Some(ChannelId("100005000/10000".into()))
        );
        let sp = &f.provenance.span;
        assert_eq!(sp.start_ns, Timestamp(1_000_000_000));
        assert_eq!(sp.end_ns.0 - sp.start_ns.0, 100_000_000);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn channel_stage_fails_naming_the_field_and_honours_cancel() {
        let (tx, _out, _rx, h) = spawn(params(100_100_000), CancellationToken::new());
        tx.send(iq_frame(tone(480, 5_000.0))).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains("center_hz"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }

        let bad = serde_json::json!({"center_hz": 1, "bandwidth_hz": 1});
        let (_tx, _out, _rx, h) = spawn(bad, CancellationToken::new());
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains("out_rate_hz"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }

        let cancel = CancellationToken::new();
        let (_tx, _out, _rx, h) = spawn(params(100_005_000), cancel.clone());
        cancel.cancel();
        assert!(matches!(h.await.unwrap(), Err(StageError::Cancelled)));
    }
}
