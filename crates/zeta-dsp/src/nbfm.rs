//! NBFM demodulator stage: phase discriminator, then resampling to the audio rate.

use std::f64::consts::PI;
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::plans::GUARD_DB_DEFAULT;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, Span, StreamId, StreamKind, Timestamp};

use crate::denoise::{Denoiser, denoised};
use crate::resample::Resampler;
use crate::squelch::{
    db, default_true, squelch_event, squelch_outputs, squelch_streams, stage_squelch,
};
use crate::util::{StreamingFir, lowpass_taps};
use crate::wfm::deemphasis;
use crate::{Input, frame_span, run_map};

/// Taps of the adjacent-channel guard's channel filter (a Hann low-pass at
/// `channel_hz / 2`), on the centre and on each neighbour.
pub const GUARD_TAPS: usize = 63;
/// Lowest and highest accepted `guard_db`, dB.
pub const GUARD_DB_RANGE: (i64, i64) = (0, 60);

/// Phase-discriminator FM demodulation: `arg(x[n] · conj(x[n-1]))` scaled by
/// `rate_hz / (2π · deviation_hz)`, so a tone at full deviation comes out at
/// amplitude 1.0. `prev` is the sample before `iq[0]` and is left holding the
/// last sample. Output is clamped to ±1.0; a zero-magnitude or non-finite
/// sample yields 0.0 (non-finite samples are carried forward as zero).
pub fn nbfm_demod(
    iq: &[Complex<f32>],
    rate_hz: u64,
    deviation_hz: u64,
    prev: &mut Complex<f32>,
) -> Vec<f32> {
    let gain = (rate_hz as f64 / (2.0 * PI * deviation_hz as f64)) as f32;
    iq.iter()
        .map(|&x| {
            let x = if x.re.is_finite() && x.im.is_finite() {
                x
            } else {
                Complex::new(0.0, 0.0)
            };
            // atan2(0, 0) is 0, so a zero sample on either side gives 0.
            let v = (x * prev.conj()).arg() * gain;
            *prev = x;
            if v.is_nan() { 0.0 } else { v.clamp(-1.0, 1.0) }
        })
        .collect()
}

/// Streaming resampler from the discriminator rate to the audio rate (a
/// [`Resampler`], low-pass at `0.45 × min(in, audio)`), carrying history and
/// phase across blocks. Rebuilt when the input or audio rate changes.
#[derive(Debug, Default)]
pub struct AudioFilter {
    key: Option<(u64, u64)>,
    resampler: Option<Resampler>,
}

impl AudioFilter {
    /// Resample `x` at `in_rate_hz` to `audio_rate_hz`; any pair of non-zero
    /// rates, up or down.
    pub fn process(
        &mut self,
        x: &[f32],
        in_rate_hz: u64,
        audio_rate_hz: u64,
    ) -> Result<Vec<f32>, String> {
        let key = (in_rate_hz, audio_rate_hz);
        if self.key != Some(key) {
            // Never keep streaming through the old pair's state if the new one fails.
            self.resampler = None;
            self.key = None;
            self.resampler = Some(Resampler::new(in_rate_hz, audio_rate_hz)?);
            self.key = Some(key);
        }
        Ok(self
            .resampler
            .as_mut()
            .map_or_else(Vec::new, |r| r.process(x)))
    }
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct NbfmParams {
    pub deviation_hz: u64,
    pub audio_rate_hz: u64,
    /// Fixed squelch threshold, dBFS (-120..=0); `null` tracks the noise floor.
    #[serde(default)]
    pub squelch_db: Option<i64>,
    /// The squelch is on (the default); `false` passes every block.
    #[serde(default = "default_true")]
    pub squelch: bool,
    /// Adjacent-channel guard: the neighbours this far either side of the channel are
    /// measured, and the squelch held closed while the stronger is over `guard_db`
    /// above the channel; `null`: no guard. Needs `channel_hz`.
    #[serde(default)]
    pub guard_hz: Option<u64>,
    /// How much stronger a neighbour holds the squelch closed, dB (0..=60); `null`: 12.
    #[serde(default)]
    pub guard_db: Option<i64>,
    /// The channel's own width, Hz: when set the stage low-pass filters its input at
    /// `channel_hz / 2` (a channel cut wider for the guard), then squelches and
    /// demodulates that.
    #[serde(default)]
    pub channel_hz: Option<u64>,
    /// Audio low-pass cutoff, Hz, applied after the resampler (`null`: flat, what a
    /// decoder reading the discriminator wants; 3400 for voice).
    #[serde(default)]
    pub audio_lowpass_hz: Option<u64>,
    /// De-emphasis time constant, µs, a one-pole roll-off of 6 dB per octave (`null`:
    /// none; 530 for PMR voice).
    #[serde(default)]
    pub deemphasis_us: Option<u64>,
    /// RNNoise on the audio output (speech only: a tone or a data burst comes out
    /// mangled). Adds one 10 ms frame of delay, which the frames' spans account for.
    #[serde(default)]
    pub denoise: bool,
}

/// What shapes the audio after the resampler: de-emphasis, the low-pass and RNNoise,
/// each as the params ask. Built for one audio rate.
struct Voice {
    rate_hz: u64,
    deemphasis_us: Option<u64>,
    deemph: f32,
    lpf: Option<StreamingFir>,
    denoiser: Option<Denoiser>,
}

impl Voice {
    /// For `p` at its `audio_rate_hz`; `None` when the params ask for nothing.
    fn new(p: &NbfmParams) -> Result<Option<Self>, String> {
        if p.audio_lowpass_hz.is_none() && p.deemphasis_us.is_none() && !p.denoise {
            return Ok(None);
        }
        let rate_hz = p.audio_rate_hz;
        if let Some(c) = p.audio_lowpass_hz
            && (c == 0 || c as f64 >= 0.5 * rate_hz as f64)
        {
            return Err(format!(
                "nbfm_demod params: audio_lowpass_hz {c} must be 1..{} (under half the audio rate)",
                rate_hz / 2
            ));
        }
        if p.deemphasis_us == Some(0) {
            return Err("nbfm_demod params: deemphasis_us must be >= 1".into());
        }
        let lpf = p
            .audio_lowpass_hz
            .map(|c| StreamingFir::new(lowpass_taps(c as f64, rate_hz as f64, VOICE_LPF_TAPS)));
        let denoiser = p.denoise.then(|| Denoiser::new(rate_hz, 1)).transpose()?;
        Ok(Some(Self {
            rate_hz,
            deemphasis_us: p.deemphasis_us,
            deemph: 0.0,
            lpf,
            denoiser,
        }))
    }

    /// Shape `x` (audio at the stage's rate, its first sample at `t0`), in order:
    /// de-emphasis, low-pass, RNNoise; with the span the output covers (the denoiser's
    /// held samples date it from their own block).
    fn process(&mut self, mut x: Vec<f32>, t0: Timestamp) -> (Vec<f32>, Span) {
        if let Some(tau) = self.deemphasis_us {
            deemphasis(&mut x, self.rate_hz, tau, &mut self.deemph);
        }
        if let Some(f) = self.lpf.as_mut() {
            x = f.process(&x);
        }
        match self.denoiser.as_mut() {
            Some(d) => denoised(d, &x, t0, self.rate_hz),
            None => {
                let span = frame_span(t0, x.len(), self.rate_hz);
                (x, span)
            }
        }
    }

    /// Forget the history: the audio before a gap is not shaped into what follows it.
    fn reset(&mut self) {
        self.deemph = 0.0;
        if let Some(f) = self.lpf.as_mut() {
            f.reset();
        }
        if let Some(d) = self.denoiser.as_mut() {
            *d = Denoiser::new(self.rate_hz, 1).expect("rate checked at build");
        }
    }
}

/// Taps of the voice audio low-pass.
const VOICE_LPF_TAPS: usize = 127;

/// A streaming FIR filter without decimation: history carried across blocks.
struct Fir {
    /// The taps, last first, so a window of the input dots with them in order.
    reversed: Vec<f32>,
    /// The last `taps - 1` input samples, oldest first.
    history: Vec<Complex<f32>>,
}

impl Fir {
    fn new(taps: Vec<f32>) -> Self {
        Self {
            history: vec![Complex::new(0.0, 0.0); taps.len() - 1],
            reversed: taps.into_iter().rev().collect(),
        }
    }

    fn filter(&mut self, x: &[Complex<f32>]) -> Vec<Complex<f32>> {
        let h = self.history.len();
        let mut ext = Vec::with_capacity(h + x.len());
        ext.extend_from_slice(&self.history);
        ext.extend_from_slice(x);
        let out = ext
            .windows(h + 1)
            .map(|w| {
                let (mut re, mut im) = (0.0f32, 0.0f32);
                for (v, &t) in w.iter().zip(&self.reversed) {
                    re += v.re * t;
                    im += v.im * t;
                }
                Complex::new(re, im)
            })
            .collect();
        self.history.copy_from_slice(&ext[ext.len() - h..]);
        out
    }

    /// Follow `x` without filtering it: the history the next block needs.
    fn skip(&mut self, x: &[Complex<f32>]) {
        let h = self.history.len();
        if x.len() >= h {
            self.history.copy_from_slice(&x[x.len() - h..]);
        } else {
            self.history.rotate_left(x.len());
            self.history[h - x.len()..].copy_from_slice(x);
        }
    }
}

/// One neighbour of a guarded channel: an NCO bringing it to baseband (phase carried
/// across blocks) and the channel filter.
struct Side {
    /// Cycles per sample, the neighbour's offset negated.
    step: f64,
    /// NCO phase in cycles, in `[0, 1)`.
    phase: f64,
    fir: Fir,
}

impl Side {
    /// `x[skip..]` mixed (the NCO at `phase` on `x[0]`), the phase left at the end of
    /// `x`. A phasor stepped by one rotation per sample, restarted from the phase each
    /// block, so its rounding never builds up past a block.
    fn mix(&mut self, x: &[Complex<f32>], skip: usize) -> Vec<Complex<f32>> {
        let at = |cycles: f64| {
            let (s, c) = (2.0 * PI * cycles.rem_euclid(1.0)).sin_cos();
            Complex::new(c, s)
        };
        let (mut z, rot) = (at(self.phase + skip as f64 * self.step), at(self.step));
        let out = x[skip..]
            .iter()
            .map(|&v| {
                let y = v * Complex::new(z.re as f32, z.im as f32);
                z *= rot;
                y
            })
            .collect();
        self.phase = (self.phase + x.len() as f64 * self.step).rem_euclid(1.0);
        out
    }

    /// The neighbour's mean power in block `x`, dBFS.
    fn power_db(&mut self, x: &[Complex<f32>]) -> f32 {
        let mixed = self.mix(x, 0);
        let y = self.fir.filter(&mixed);
        db(y.iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / y.len().max(1) as f64)
    }

    /// Follow block `x` unmeasured: only its tail is mixed, for the filter's history.
    fn skip(&mut self, x: &[Complex<f32>]) {
        let tail = x.len().saturating_sub(self.fir.history.len());
        let mixed = self.mix(x, tail);
        self.fir.skip(&mixed);
    }
}

/// The channel filter of an `nbfm_demod` with `channel_hz`, and its adjacent-channel
/// guard's neighbours when it has `guard_hz`, built for one channel rate.
struct Guard {
    rate_hz: u64,
    centre: Fir,
    sides: Option<[Side; 2]>,
    db: f32,
}

impl Guard {
    /// For `p` (its `channel_hz` set) at `rate_hz`; a band that does not fit the rate
    /// fails.
    fn new(p: &NbfmParams, channel_hz: u64, rate_hz: u64) -> Result<Self, String> {
        let taps = || lowpass_taps(channel_hz as f64 / 2.0, rate_hz as f64, GUARD_TAPS);
        let sides = match p.guard_hz {
            None if channel_hz > rate_hz => {
                return Err(format!(
                    "nbfm_demod params: channel_hz {channel_hz} wider than the channel rate {rate_hz}"
                ));
            }
            None => None,
            Some(g) if channel_hz + 2 * g > rate_hz => return Err(GUARD_NEEDS.into()),
            Some(g) => Some([1.0, -1.0].map(|sign| Side {
                step: -sign * g as f64 / rate_hz as f64,
                phase: 0.0,
                fir: Fir::new(taps()),
            })),
        };
        Ok(Self {
            rate_hz,
            centre: Fir::new(taps()),
            sides,
            db: p.guard_db.unwrap_or(GUARD_DB_DEFAULT) as f32,
        })
    }

    /// The stronger neighbour's power in raw block `x`, dBFS; `None` without a guard.
    fn adjacent_db(&mut self, x: &[Complex<f32>]) -> Option<f32> {
        let sides = self.sides.as_mut()?;
        Some(sides[0].power_db(x).max(sides[1].power_db(x)))
    }

    /// Follow raw block `x` without measuring the neighbours.
    fn skip(&mut self, x: &[Complex<f32>]) {
        for s in self.sides.iter_mut().flatten() {
            s.skip(x);
        }
    }
}

/// How far under `guard_db` a neighbour must fall before a guard holding the gate lets go,
/// dB: a neighbour wavering at the threshold does not flap the gate block by block.
pub const GUARD_HYSTERESIS_DB: f32 = 2.0;

/// Whether the guard holds the gate, a neighbour `excess_db` over the centre, given whether
/// it held it (`was`): it takes hold over `guard_db` and lets go only under
/// `guard_db - GUARD_HYSTERESIS_DB`.
fn guard_holds(was: bool, excess_db: f32, guard_db: f32) -> bool {
    if was {
        excess_db >= guard_db - GUARD_HYSTERESIS_DB
    } else {
        excess_db > guard_db
    }
}

const GUARD_NEEDS: &str =
    "nbfm_demod params: guard_hz needs channel_hz with channel_hz + 2 guard_hz <= the channel rate";

/// Demodulates an NBFM IQ stream to audio at `audio_rate_hz`, behind a
/// [`Squelch`](crate::squelch::Squelch): while it is closed no audio frame is
/// published, and each transition (and the first block) publishes a `squelch`
/// event on the second output.
pub struct NbfmDemodStage {
    spec: StageSpec,
}

impl NbfmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "nbfm_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio, StreamKind::Event],
                params_schema: schemars::schema_for!(NbfmParams),
            },
        }
    }
}

impl Default for NbfmDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for NbfmDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    /// No event output with the squelch off: it would stay silent.
    fn outputs_for(&self, params: &serde_json::Value) -> Vec<StreamKind> {
        squelch_outputs(params)
    }

    async fn run(
        &self,
        params: serde_json::Value,
        inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: NbfmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("nbfm_demod params: {e}")))?;
        if p.deviation_hz == 0 {
            return Err(StageError::Failed(
                "nbfm_demod params: deviation_hz must be >= 1".into(),
            ));
        }
        if p.audio_rate_hz == 0 {
            return Err(StageError::Failed(
                "nbfm_demod params: audio_rate_hz must be >= 1".into(),
            ));
        }
        let mut squelch =
            stage_squelch("nbfm_demod", p.squelch, p.squelch_db).map_err(StageError::Failed)?;
        let (lo, hi) = GUARD_DB_RANGE;
        if let Some(v) = p.guard_db.filter(|v| !(lo..=hi).contains(v)) {
            return Err(StageError::Failed(format!(
                "nbfm_demod params: guard_db {v} out of range {lo}..={hi}"
            )));
        }
        if p.guard_hz.is_some() && p.channel_hz.is_none() {
            return Err(StageError::Failed(GUARD_NEEDS.into()));
        }
        if p.channel_hz == Some(0) {
            return Err(StageError::Failed(
                "nbfm_demod params: channel_hz must be >= 1".into(),
            ));
        }
        let mut voice = Voice::new(&p).map_err(StageError::Failed)?;
        let (out, events) = squelch_streams(&outputs, squelch.is_some())?;
        if squelch.is_none() {
            // No gate this run: clear one an earlier run (before a `graph_put`) latched.
            ctx.note(serde_json::json!({ "squelch": null }));
        }
        let mut prev = Complex::new(0.0f32, 0.0);
        let mut filter = AudioFilter::default();
        let mut guard: Option<Guard> = None;
        // Whether the last block measured with the squelch open had a neighbour over
        // `guard_db`; the gate (open and not guarded) as last published, `None` before the
        // first block.
        let mut guarded = false;
        let mut was: Option<bool> = None;
        let mut was_open = false;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    prev = Complex::new(0.0, 0.0);
                    filter = AudioFilter::default();
                    if let Some(v) = voice.as_mut() {
                        v.reset();
                    }
                    guard = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "nbfm_demod stage expects iq frames".into(),
                ));
            };
            let rate = block.sample_rate_hz.0;
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            if let Some(c) = p.channel_hz
                && guard.as_ref().is_none_or(|g| g.rate_hz != rate)
            {
                guard = Some(Guard::new(&p, c, rate).map_err(StageError::Failed)?);
            }
            // With `channel_hz`, the channel filtered out of the wider cut: what the squelch
            // and the discriminator see.
            let centred;
            let iq: &[Complex<f32>] = match guard.as_mut() {
                Some(g) => {
                    centred = g.centre.filter(&block.samples);
                    &centred
                }
                None => &block.samples,
            };
            let mut published = Vec::new();
            if let (Some(sq), Some(events)) = (squelch.as_mut(), &events) {
                sq.feed(iq, rate);
                // The neighbours are measured while the squelch is open (`guarded` holds the
                // last finding while it is closed), and for an event.
                let mut adjacent = None;
                if sq.is_open() {
                    adjacent = guard.as_mut().and_then(|g| g.adjacent_db(&block.samples));
                    guarded = adjacent.is_some_and(|a| {
                        let db = guard.as_ref().map_or(f32::INFINITY, |g| g.db);
                        guard_holds(guarded, a - sq.power_db(), db)
                    });
                }
                let gate = sq.is_open() && !guarded;
                if was != Some(gate) {
                    if adjacent.is_none() {
                        adjacent = guard.as_mut().and_then(|g| g.adjacent_db(&block.samples));
                    }
                    let event = squelch_event(sq, gate, adjacent, guarded);
                    // Latched in the job's context too: a late subscriber reads the gate there.
                    ctx.note(serde_json::json!({ "squelch": event.clone() }));
                    published.push(Frame::new(
                        events.clone(),
                        prov.clone(),
                        Payload::Event(event),
                    ));
                }
                if adjacent.is_none()
                    && let Some(g) = guard.as_mut()
                {
                    g.skip(&block.samples);
                }
                was = Some(gate);
                if !gate {
                    // Skipped, but the discriminator's reference follows the channel, so a
                    // reopening block differences against the sample just before it.
                    if let Some(&x) = iq
                        .iter()
                        .rev()
                        .find(|x| x.re.is_finite() && x.im.is_finite())
                    {
                        prev = x;
                    }
                    was_open = false;
                    return Ok(published);
                }
                if !std::mem::replace(&mut was_open, true) {
                    // Audio from before the close is not resampled into the reopening block.
                    filter = AudioFilter::default();
                    if let Some(v) = voice.as_mut() {
                        v.reset();
                    }
                }
            }
            let demod = nbfm_demod(iq, rate, p.deviation_hz, &mut prev);
            let mut samples = filter
                .process(&demod, rate, p.audio_rate_hz)
                .map_err(StageError::Failed)?;
            if let Some(v) = voice.as_mut() {
                let (s, span) = v.process(samples, block.t0);
                samples = s;
                prov.span = span;
            }
            if !samples.is_empty() {
                published.push(Frame::new(
                    out.clone(),
                    prov,
                    Payload::Audio {
                        sample_rate_hz: Hz(p.audio_rate_hz),
                        channels: 1,
                        samples: samples.into(),
                    },
                ));
            }
            Ok(published)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use zeta_core::port::IqBlock;
    use zeta_core::types::{Provenance, SourceId, Span, Timestamp};

    use super::*;
    use crate::fixtures::{awgn, awgn_real, burst_to_silence_db, nbfm_modulate, speech_like, tone};

    /// Frequency (Hz) of the largest DFT bin of `x` in `[1, len/2)` and the RMS of `x`.
    fn peak_and_rms(x: &[f32], rate_hz: f64) -> (f64, f64) {
        let n = x.len();
        let mag = |k: usize| {
            let (re, im) = x
                .iter()
                .enumerate()
                .fold((0.0f64, 0.0f64), |(re, im), (i, &v)| {
                    let ph = -2.0 * std::f64::consts::PI * ((k * i) % n) as f64 / n as f64;
                    (re + v as f64 * ph.cos(), im + v as f64 * ph.sin())
                });
            re * re + im * im
        };
        let k = (1..n / 2)
            .max_by(|&a, &b| mag(a).partial_cmp(&mag(b)).unwrap())
            .unwrap();
        let rms = (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / n as f64).sqrt();
        (k as f64 * rate_hz / n as f64, rms)
    }

    #[test]
    fn demod_recovers_tone() {
        let audio = tone(48_000, 9_600, 1_000.0, 1.0);
        let iq = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0);
        let mut prev = Complex::new(1.0, 0.0);
        let out = nbfm_demod(&iq, 48_000, 2_500, &mut prev);
        assert_eq!(out.len(), iq.len());
        let (peak, rms) = peak_and_rms(&out[4_800..], 48_000.0);
        assert_eq!(peak, 1_000.0);
        assert!((rms - 0.707).abs() <= 0.0707, "rms {rms}");
    }

    #[test]
    fn demod_noise_is_bounded_and_finite() {
        // Zero signal plus noise, a zero run and a NaN: every output finite and within ±1.
        let mut iq = vec![Complex::new(0.0f32, 0.0); 4_800];
        awgn(&mut iq, 0.0, 7);
        iq.extend(std::iter::repeat_n(Complex::new(0.0, 0.0), 16));
        iq.push(Complex::new(f32::NAN, 0.0));
        iq.push(Complex::new(1.0, 0.0));
        let mut prev = Complex::new(0.0, 0.0);
        let out = nbfm_demod(&iq, 48_000, 2_500, &mut prev);
        assert_eq!(out.len(), iq.len());
        assert!(out.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        assert!(out[4_801..4_816].iter().all(|&s| s == 0.0));
        // Wideband noise through the discriminator hits the clamp somewhere.
        assert!(out.iter().any(|s| s.abs() > 0.5));
    }

    #[test]
    fn audio_filter_streaming_matches_one_shot_and_rebuilds_on_rate_change() {
        let x = tone(200_000, 4_801, 1_000.0, 1.0);
        let whole = Resampler::new(200_000, 48_000).unwrap().process(&x);
        let mut f = AudioFilter::default();
        let mut parts = Vec::new();
        for chunk in [&x[..1001], &x[1001..1002], &x[1002..]] {
            parts.extend(f.process(chunk, 200_000, 48_000).unwrap());
        }
        assert_eq!(parts, whole);
        // A new rate pair starts from a fresh resampler.
        let y = f.process(&x[..960], 96_000, 48_000).unwrap();
        assert_eq!(
            y,
            Resampler::new(96_000, 48_000).unwrap().process(&x[..960])
        );
        assert!(f.process(&x, 0, 48_000).is_err());
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
    ) -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
        let (tx, rx_out, _rx_ev, h) = spawn_with(params, 8);
        (tx, rx_out, h)
    }

    /// The stage with `cap`-deep input and output channels: its input sender, its audio
    /// and event receivers and its task.
    fn spawn_with(
        params: serde_json::Value,
        cap: usize,
    ) -> (
        broadcast::Sender<Frame>,
        broadcast::Receiver<Frame>,
        broadcast::Receiver<Frame>,
        Run,
    ) {
        let ctx = zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into()));
        spawn_ctx(params, cap, ctx)
    }

    /// [`spawn_with`] in `ctx`.
    fn spawn_ctx(
        params: serde_json::Value,
        cap: usize,
        ctx: zeta_core::stage::StageContext,
    ) -> (
        broadcast::Sender<Frame>,
        broadcast::Receiver<Frame>,
        broadcast::Receiver<Frame>,
        Run,
    ) {
        let bus = Arc::new(StreamBus::new(cap));
        let out = bus.register(StreamKind::Audio, prov());
        let ev = bus.register(StreamKind::Event, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let rx_ev = bus.subscribe(&ev).unwrap();
        let (tx, rx_in) = broadcast::channel(cap);
        let h = tokio::spawn(async move {
            NbfmDemodStage::new()
                .run(
                    params,
                    vec![rx_in],
                    vec![out, ev],
                    bus,
                    CancellationToken::new(),
                    ctx,
                )
                .await
        });
        (tx, rx_out, rx_ev, h)
    }

    fn iq_frame(samples: Vec<Complex<f32>>, rate_hz: u64) -> Frame {
        iq_frame_at(samples, rate_hz, 1_000_000_000)
    }

    fn iq_frame_at(samples: Vec<Complex<f32>>, rate_hz: u64, t0_ns: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(t0_ns),
                sample_rate_hz: Hz(rate_hz),
                center_hz: Hz(100_000_000),
                samples: samples.into(),
            }),
        )
    }

    #[tokio::test]
    async fn nbfm_stage_decimates_to_audio_rate() {
        let (tx, mut rx_out, h) =
            spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}));
        let audio = tone(48_000, 4_800, 1_000.0, 1.0);
        let iq = nbfm_modulate(&audio, 48_000, 96_000, 2_500.0, 0.0);
        assert_eq!(iq.len(), 9_600);
        tx.send(iq_frame(iq, 96_000)).unwrap();
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.kind, StreamKind::Audio);
        let Payload::Audio {
            sample_rate_hz,
            samples,
            ..
        } = &f.payload
        else {
            panic!("expected an audio payload")
        };
        assert_eq!(*sample_rate_hz, Hz(48_000));
        assert_eq!(samples.len(), 4_800);
        let sp = &f.provenance.span;
        assert_eq!(sp.start_ns, Timestamp(1_000_000_000));
        assert_eq!(sp.end_ns.0 - sp.start_ns.0, 100_000_000);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    #[tokio::test]
    async fn nbfm_stage_resamples_a_non_integer_ratio() {
        let (tx, mut rx_out, h) =
            spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}));
        tx.send(iq_frame(vec![Complex::new(1.0, 0.0); 2_000], 200_000))
            .unwrap();
        let f = rx_out.recv().await.unwrap();
        let Payload::Audio {
            sample_rate_hz,
            samples,
            ..
        } = &f.payload
        else {
            panic!("expected an audio payload")
        };
        assert_eq!(*sample_rate_hz, Hz(48_000));
        assert_eq!(samples.len(), 480);
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    /// RMS of the audio the stage with `params` gives for the third 0.1 s block of a
    /// `freq_hz` tone at ±2.5 kHz deviation (the first two settle its filters).
    async fn tone_rms(params: serde_json::Value, freq_hz: f64) -> f64 {
        let (tx, mut rx_out, _h) = spawn(params);
        let audio = tone(48_000, 3 * 4_800, freq_hz, 1.0);
        let mut last = Vec::new();
        for i in 0..3 {
            let iq = nbfm_modulate(
                &audio[i * 4_800..(i + 1) * 4_800],
                48_000,
                96_000,
                2_500.0,
                0.0,
            );
            tx.send(iq_frame_at(
                iq,
                96_000,
                1_000_000_000 + i as u64 * 100_000_000,
            ))
            .unwrap();
            let f = rx_out.recv().await.unwrap();
            let Payload::Audio { samples, .. } = &f.payload else {
                panic!()
            };
            last = samples.to_vec();
        }
        (last.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / last.len() as f64).sqrt()
    }

    #[tokio::test]
    async fn audio_lowpass_passes_voice_and_cuts_a_tone_above_the_cutoff() {
        let p = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "audio_lowpass_hz": 3_400});
        let voice = tone_rms(p.clone(), 1_000.0).await;
        assert!((voice - 0.707).abs() < 0.07, "1 kHz rms {voice}");
        let hiss = tone_rms(p, 5_000.0).await;
        assert!(hiss < 0.05, "5 kHz rms {hiss}");
        // Flat without it.
        let flat = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000});
        let hiss = tone_rms(flat, 5_000.0).await;
        assert!((hiss - 0.707).abs() < 0.07, "flat 5 kHz rms {hiss}");
    }

    #[tokio::test]
    async fn deemphasis_rolls_the_audio_off_at_six_db_per_octave() {
        // A one-pole with tau 530 us: |H| is 0.71 at 300 Hz and 0.10 at 3 kHz.
        let p = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "deemphasis_us": 530});
        let low = tone_rms(p.clone(), 300.0).await;
        let high = tone_rms(p, 3_000.0).await;
        let ratio = high / low;
        assert!(
            (0.10..0.20).contains(&ratio),
            "low {low} high {high} ratio {ratio}"
        );
    }

    #[tokio::test]
    async fn denoise_frames_the_audio_in_rnnoise_frames_and_quietens_hiss() {
        // Speech-like bursts with hiss 10 dB under them, through the channel in 0.1 s
        // blocks: RNNoise lifts the burst-to-silence ratio and frames the output.
        let clean = speech_like(48_000, 6.0);
        let mut noisy = clean.clone();
        awgn_real(&mut noisy, 7.0, 7);
        let iq = nbfm_modulate(&noisy, 48_000, 48_000, 2_500.0, 0.0);
        let run = |denoise: bool| {
            let iq = iq.clone();
            async move {
                let p = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "squelch": false, "denoise": denoise});
                let (tx, mut rx_out, _rx_ev, _h) = spawn_with(p, 128);
                let mut out = Vec::new();
                let mut lens = Vec::new();
                for (i, block) in iq.chunks(4_800).enumerate() {
                    tx.send(iq_frame_at(
                        block.to_vec(),
                        48_000,
                        1_000_000_000 + i as u64 * 100_000_000,
                    ))
                    .unwrap();
                }
                drop(tx);
                while let Ok(f) = rx_out.recv().await {
                    let Payload::Audio { samples, .. } = &f.payload else {
                        panic!()
                    };
                    lens.push(samples.len());
                    out.extend_from_slice(samples);
                }
                (out, lens)
            }
        };
        let (flat, _) = run(false).await;
        let (clean_out, lens) = run(true).await;
        assert!(lens.iter().all(|n| n % 480 == 0), "{lens:?}");
        let (before, after) = (burst_to_silence_db(&flat), burst_to_silence_db(&clean_out));
        assert!(
            after - before >= 6.0,
            "burst/silence {before:.1} dB -> {after:.1} dB"
        );
    }

    #[tokio::test]
    async fn denoised_frames_keep_the_time_of_their_first_sample() {
        // 4 800 samples make ten whole RNNoise frames; then 100 samples are held back and
        // the 380 that complete the frame carry the earlier block's start time.
        let p = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "squelch": false, "denoise": true});
        let (tx, mut rx_out, _h) = spawn(p);
        let audio = tone(48_000, 4_800 + 100 + 380, 1_000.0, 0.5);
        let iq = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0);
        tx.send(iq_frame_at(iq[..4_800].to_vec(), 48_000, 1_000_000_000))
            .unwrap();
        let f = rx_out.recv().await.unwrap();
        assert_eq!(f.provenance.span.start_ns, Timestamp(1_000_000_000));
        tx.send(iq_frame_at(
            iq[4_800..4_900].to_vec(),
            48_000,
            1_100_000_000,
        ))
        .unwrap();
        tx.send(iq_frame_at(iq[4_900..].to_vec(), 48_000, 1_102_083_333))
            .unwrap();
        let f = rx_out.recv().await.unwrap();
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!()
        };
        assert_eq!(samples.len(), 480);
        assert_eq!(f.provenance.span.start_ns, Timestamp(1_100_000_000));
        assert_eq!(f.provenance.span.end_ns, Timestamp(1_110_000_000));
    }

    #[tokio::test]
    async fn nbfm_stage_rejects_zero_audio_rate() {
        let (_tx, _rx, h) = spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 0}));
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => {
                assert_eq!(m, "nbfm_demod params: audio_rate_hz must be >= 1")
            }
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    #[tokio::test]
    async fn nbfm_resets_on_lag() {
        let (tx, mut rx_out, h) =
            spawn(serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}));
        let a = nbfm_modulate(
            &tone(48_000, 4_800, 1_000.0, 1.0),
            48_000,
            96_000,
            2_500.0,
            0.3,
        );
        let b = nbfm_modulate(
            &tone(48_000, 4_800, 700.0, 1.0),
            48_000,
            96_000,
            2_500.0,
            2.0,
        );
        tx.send(iq_frame(a, 96_000)).unwrap();
        rx_out.recv().await.unwrap();
        tx.send(iq_frame(vec![Complex::new(0.0, 1.0); 9_600], 96_000))
            .unwrap();
        for _ in 0..8 {
            tx.send(iq_frame(b.clone(), 96_000)).unwrap();
        }
        let f = rx_out.recv().await.unwrap();
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!("expected an audio payload")
        };
        let mut prev = Complex::new(0.0f32, 0.0);
        let demod = nbfm_demod(&b, 96_000, 2_500, &mut prev);
        let want = AudioFilter::default()
            .process(&demod, 96_000, 48_000)
            .unwrap();
        assert_eq!(samples.len(), want.len());
        for (g, w) in samples.iter().zip(&want) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
    }

    /// 20 ms blocks of `secs` seconds of complex noise at `db` dBFS, at 48 kHz, then
    /// `tail` (blocks of a carrier at `tail_db` dBFS frequency-modulated by a 1 kHz tone).
    fn blocks(secs: f32, db: f32, tail: usize, tail_db: f32) -> Vec<Vec<Complex<f32>>> {
        const BLOCK: usize = 960;
        let n = (secs * 50.0) as usize;
        let mut out: Vec<Vec<Complex<f32>>> = (0..n)
            .map(|i| {
                let mut x = vec![Complex::new(0.0f32, 0.0); BLOCK];
                awgn(&mut x, -db, 100 + i as u64);
                x
            })
            .collect();
        let audio = tone(48_000, BLOCK * tail.max(1), 1_000.0, 0.5);
        let a = 10f32.powf(tail_db / 20.0);
        let iq: Vec<Complex<f32>> = nbfm_modulate(&audio, 48_000, 48_000, 2_500.0, 0.0)
            .into_iter()
            .map(|c| c * a)
            .collect();
        out.extend(iq.chunks(BLOCK).take(tail).map(|c| c.to_vec()));
        out
    }

    /// Feed `blocks` (20 ms apart) to a stage with `params`, end its input and return
    /// the audio frames and the event payloads it published.
    async fn run_blocks(
        params: serde_json::Value,
        blocks: Vec<Vec<Complex<f32>>>,
    ) -> (usize, Vec<serde_json::Value>) {
        let (audio, events) = run_blocks_at(params, blocks, 48_000).await;
        (audio.len(), events)
    }

    /// [`run_blocks`] at `rate_hz`: the audio frames' start times and the events. Every
    /// event is also noted to the job as `{"squelch": event}` (the latched gate state).
    async fn run_blocks_at(
        params: serde_json::Value,
        blocks: Vec<Vec<Complex<f32>>>,
        rate_hz: u64,
    ) -> (Vec<u64>, Vec<serde_json::Value>) {
        let n = blocks.len();
        let params_squelch_off = params["squelch"] == false;
        let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = notes.clone();
        let ctx = zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into()))
            .with_note(Arc::new(move |v| sink.lock().unwrap().push(v)));
        let (tx, mut rx_out, mut rx_ev, h) = spawn_ctx(params, n.max(8) * 2, ctx);
        for (i, b) in blocks.into_iter().enumerate() {
            tx.send(iq_frame_at(
                b,
                rate_hz,
                1_000_000_000 + i as u64 * 20_000_000,
            ))
            .unwrap();
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut audio = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            assert!(matches!(f.payload, Payload::Audio { .. }));
            audio.push(f.provenance.span.start_ns.0);
        }
        let mut events = Vec::new();
        while let Ok(f) = rx_ev.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("expected an event payload")
            };
            events.push(v);
        }
        // With the squelch off, a gate latched by an earlier run is cleared instead.
        let noted: Vec<_> = if params_squelch_off {
            vec![serde_json::json!({ "squelch": null })]
        } else {
            events
                .iter()
                .map(|e| serde_json::json!({ "squelch": e }))
                .collect()
        };
        assert_eq!(*notes.lock().unwrap(), noted);
        (audio, events)
    }

    #[tokio::test]
    async fn closed_squelch_publishes_no_audio_and_a_squelch_event() {
        let p = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000});
        let (audio, events) = run_blocks(p.clone(), blocks(1.0, -60.0, 0, 0.0)).await;
        assert_eq!(audio, 0);
        assert_eq!(events.len(), 1, "{events:?}");
        let e = &events[0];
        assert_eq!(e["protocol"], "squelch");
        assert_eq!(e["open"], false);
        assert!((e["power_db"].as_f64().unwrap() + 60.0).abs() < 1.0, "{e}");
        assert!((e["floor_db"].as_f64().unwrap() + 60.0).abs() < 1.0, "{e}");
        let keys: Vec<_> = e.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), 6, "{e}");
        // No guard: no neighbour measured, never guarded.
        assert_eq!(e["adjacent_db"], serde_json::Value::Null, "{e}");
        assert_eq!(e["guarded"], false, "{e}");

        // Off: audio frames as before, and no squelch events.
        let off =
            serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "squelch": false});
        let (audio, events) = run_blocks(off, blocks(1.0, -60.0, 0, 0.0)).await;
        assert_eq!(audio, 50);
        assert!(events.is_empty(), "{events:?}");

        // A -50 dBFS FM tone after 2 s of noise opens it: audio, and an open event.
        let (audio, events) = run_blocks(p, blocks(2.0, -60.0, 10, -50.0)).await;
        assert!(audio >= 9, "{audio} audio frames");
        let opens: Vec<_> = events.iter().map(|e| e["open"].clone()).collect();
        assert_eq!(opens, [false, true], "{events:?}");
        assert!(
            events[1]["power_db"].as_f64().unwrap() > -51.0,
            "{events:?}"
        );
    }

    #[test]
    fn the_event_output_goes_with_the_squelch() {
        let st = NbfmDemodStage::new();
        assert_eq!(st.spec().outputs, [StreamKind::Audio, StreamKind::Event]);
        let on = serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000});
        assert_eq!(st.outputs_for(&on), [StreamKind::Audio, StreamKind::Event]);
        let off =
            serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "squelch": false});
        assert_eq!(st.outputs_for(&off), [StreamKind::Audio]);
    }

    #[tokio::test]
    async fn a_reopened_squelch_demodulates_from_the_last_closed_block() {
        // Open on an FM tone, closed by 1.2 s of noise, reopened by the tone: the reopening
        // block is demodulated against the sample just before it (not one from before the
        // close) through a fresh audio filter, so no stale-phase spike starts it.
        let mut blocks = blocks(0.0, -60.0, 50, -20.0);
        let quiet = super::tests::blocks(1.2, -60.0, 0, 0.0);
        let last_quiet = *quiet.last().unwrap().last().unwrap();
        blocks.extend(quiet);
        let tone_blocks = super::tests::blocks(0.0, -60.0, 50, -20.0);
        // The second tone starts 90 degrees on: a stale `prev` would clamp to +-1.
        let reopen: Vec<Complex<f32>> = tone_blocks[17]
            .iter()
            .map(|c| c * Complex::new(0.0, 1.0))
            .collect();
        blocks.push(reopen.clone());
        let (tx, mut rx_out, _rx_ev, h) = spawn_with(
            serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000}),
            1_024,
        );
        let n = blocks.len();
        for (i, b) in blocks.into_iter().enumerate() {
            tx.send(iq_frame_at(
                b,
                48_000,
                1_000_000_000 + i as u64 * 20_000_000,
            ))
            .unwrap();
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut last = None;
        while let Ok(f) = rx_out.try_recv() {
            last = Some(f);
        }
        let f = last.expect("audio");
        assert_eq!(
            f.provenance.span.start_ns.0,
            1_000_000_000 + (n as u64 - 1) * 20_000_000,
            "the reopening block's audio is the last frame"
        );
        let Payload::Audio { samples, .. } = &f.payload else {
            panic!("expected audio")
        };
        let mut prev = last_quiet;
        let want = AudioFilter::default()
            .process(
                &nbfm_demod(&reopen, 48_000, 2_500, &mut prev),
                48_000,
                48_000,
            )
            .unwrap();
        assert_eq!(samples.len(), want.len());
        for (g, w) in samples.iter().zip(&want) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
    }

    #[tokio::test]
    async fn squelch_db_out_of_range_fails_the_stage() {
        for v in [-121, 1] {
            let (_tx, _rx, h) = spawn(serde_json::json!({
                "deviation_hz": 2_500, "audio_rate_hz": 48_000, "squelch_db": v
            }));
            match h.await.unwrap() {
                Err(StageError::Failed(m)) => assert_eq!(
                    m,
                    format!("nbfm_demod params: squelch_db {v} out of range -120..=0")
                ),
                r => panic!("expected Failed, got {r:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_fixed_squelch_db_opens_over_it() {
        // -30 dBFS fixed: 1 s of -60 dBFS noise stays closed, a -20 dBFS tone opens.
        let p =
            serde_json::json!({"deviation_hz": 2_500, "audio_rate_hz": 48_000, "squelch_db": -30});
        let (audio, events) = run_blocks(p, blocks(1.0, -60.0, 5, -20.0)).await;
        assert!(audio >= 4, "{audio}");
        let opens: Vec<_> = events.iter().map(|e| e["open"].clone()).collect();
        assert_eq!(opens, [false, true], "{events:?}");
    }

    /// The CB channel rate on a 256 000 Hz bin rate, and its 20 ms block.
    const CB_RATE: u32 = 51_200;
    const CB_BLOCK: usize = 1_024;

    /// `secs` of 20 ms blocks at [`CB_RATE`]: complex noise at -60 dBFS (seeded by `seed`)
    /// plus, for each `(offset_hz, db, deviation_hz)`, a carrier at `db` dBFS `offset_hz`
    /// from the centre, frequency-modulated by a 1 kHz tone.
    fn cb_blocks(secs: f32, signals: &[(f64, f32, f64)], seed: u64) -> Vec<Vec<Complex<f32>>> {
        let len = (secs * 50.0) as usize * CB_BLOCK;
        let mut x = vec![Complex::new(0.0f32, 0.0); len];
        awgn(&mut x, 60.0, seed);
        let audio = tone(CB_RATE, len, 1_000.0, 1.0);
        for &(offset_hz, db, deviation_hz) in signals {
            let a = 10f32.powf(db / 20.0);
            let s = nbfm_modulate(&audio, CB_RATE, CB_RATE, deviation_hz, offset_hz);
            for (v, s) in x.iter_mut().zip(s) {
                *v += s * a;
            }
        }
        x.chunks(CB_BLOCK).map(|c| c.to_vec()).collect()
    }

    /// `nbfm_demod` params with the guard a CB channel's graph carries (`guard_hz`
    /// `Some`), or only its centre filter (`None`: the guard turned off).
    fn cb_params(guard_hz: Option<u64>) -> serde_json::Value {
        serde_json::json!({
            "deviation_hz": 5_000, "audio_rate_hz": 48_000, "squelch_db": null, "squelch": true,
            "guard_hz": guard_hz, "guard_db": 12, "channel_hz": 12_500,
        })
    }

    #[tokio::test]
    async fn a_strong_neighbour_holds_the_gate_closed() {
        // 1 s of noise, then 2 s of a carrier 12.5 kHz up, 40 dB over the noise. Its tail
        // through the centre filter opens the centre's squelch; the guard holds it shut.
        let mut blocks = cb_blocks(1.0, &[], 1);
        blocks.extend(cb_blocks(2.0, &[(12_500.0, -20.0, 5_000.0)], 2));
        let (audio, events) = run_blocks_at(cb_params(Some(12_500)), blocks.clone(), 51_200).await;
        assert!(audio.is_empty(), "{} audio frames", audio.len());
        assert_eq!(events[0]["open"], false, "{events:?}");
        assert!(events.iter().all(|e| e["open"] == false), "{events:?}");
        // The gate never changes: the first block's event only.
        assert_eq!(events.len(), 1, "{events:?}");
        // Without the guard the same channel opens.
        let (unguarded, _) = run_blocks_at(cb_params(None), blocks, 51_200).await;
        assert!(!unguarded.is_empty());
    }

    #[tokio::test]
    async fn a_centre_signal_opens_with_a_quiet_neighbour() {
        let mut blocks = cb_blocks(1.0, &[], 3);
        blocks.extend(cb_blocks(1.0, &[(0.0, -30.0, 2_500.0)], 4));
        let (audio, events) = run_blocks_at(cb_params(Some(12_500)), blocks, 51_200).await;
        assert!(audio.len() >= 45, "{} audio frames", audio.len());
        let opens: Vec<_> = events.iter().map(|e| e["open"].clone()).collect();
        assert_eq!(opens, [false, true], "{events:?}");
        let e = &events[1];
        assert_eq!(e["guarded"], false, "{e}");
        assert!(
            e["adjacent_db"].as_f64().unwrap() < e["power_db"].as_f64().unwrap() - 12.0,
            "{e}"
        );
        assert!(events[0]["adjacent_db"].is_number(), "{events:?}");
    }

    #[tokio::test]
    async fn equal_neighbours_do_not_guard() {
        // Signals on both neighbours and the centre, the centre 3 dB weaker.
        let mut blocks = cb_blocks(1.0, &[], 5);
        let busy = [
            (0.0, -23.0, 2_500.0),
            (12_500.0, -20.0, 5_000.0),
            (-12_500.0, -20.0, 5_000.0),
        ];
        blocks.extend(cb_blocks(1.0, &busy, 6));
        let (audio, events) = run_blocks_at(cb_params(Some(12_500)), blocks, 51_200).await;
        assert!(audio.len() >= 45, "{} audio frames", audio.len());
        assert!(events.iter().all(|e| e["guarded"] == false), "{events:?}");
    }

    #[tokio::test]
    async fn the_guard_only_closes() {
        // 5 s of half-second scenes: every block the guarded stage passes, the same
        // stage without the guard passes too, and the guard closes some it would pass.
        let scenes: [&[(f64, f32, f64)]; 10] = [
            &[],
            &[(0.0, -30.0, 2_500.0)],
            &[(0.0, -30.0, 2_500.0), (12_500.0, -5.0, 5_000.0)],
            &[],
            &[(0.0, -45.0, 2_500.0), (-12_500.0, -20.0, 5_000.0)],
            &[(0.0, -30.0, 2_500.0), (12_500.0, -35.0, 5_000.0)],
            &[(-12_500.0, -15.0, 5_000.0)],
            &[(0.0, -25.0, 2_500.0), (12_500.0, -25.0, 5_000.0)],
            &[],
            &[(12_500.0, -20.0, 5_000.0), (-12_500.0, -20.0, 5_000.0)],
        ];
        let blocks: Vec<_> = scenes
            .iter()
            .enumerate()
            .flat_map(|(i, s)| cb_blocks(0.5, s, 10 + i as u64))
            .collect();
        let (guarded, events) =
            run_blocks_at(cb_params(Some(12_500)), blocks.clone(), 51_200).await;
        let (unguarded, _) = run_blocks_at(cb_params(None), blocks, 51_200).await;
        let open: std::collections::HashSet<_> = unguarded.iter().collect();
        assert!(guarded.iter().all(|t| open.contains(t)));
        assert!(
            guarded.len() < unguarded.len(),
            "{} vs {}",
            guarded.len(),
            unguarded.len()
        );
        assert!(!guarded.is_empty());
        assert!(events.iter().any(|e| e["guarded"] == true), "{events:?}");
    }

    #[test]
    fn the_guard_lets_go_only_under_its_hysteresis() {
        // A neighbour 12 dB over the centre, give or take 1 dB, block by block.
        let mut held = false;
        let mut flips = 0;
        for i in 0..50 {
            let excess = if i % 2 == 0 { 13.0 } else { 11.0 };
            let now = guard_holds(held, excess, 12.0);
            flips += usize::from(now != held);
            held = now;
        }
        assert_eq!((held, flips), (true, 1));
        // It lets go under guard_db - GUARD_HYSTERESIS_DB, and stays off until over guard_db.
        assert!(guard_holds(true, 12.0 - GUARD_HYSTERESIS_DB, 12.0));
        assert!(!guard_holds(true, 12.0 - GUARD_HYSTERESIS_DB - 0.1, 12.0));
        assert!(!guard_holds(false, 11.0, 12.0));
        assert!(!guard_holds(false, 12.0, 12.0));
        assert!(guard_holds(false, 12.1, 12.0));
    }

    #[tokio::test]
    async fn a_neighbour_wavering_at_the_threshold_does_not_flap_the_gate() {
        // A centre carrier and a neighbour about 12 dB over it, alternating 1 dB up and
        // down every 20 ms block: the gate changes at most once after the first event.
        let mut blocks = cb_blocks(0.5, &[], 40);
        for i in 0..50u64 {
            let db = if i % 2 == 0 { -17.0 } else { -19.0 };
            blocks.extend(cb_blocks(
                0.02,
                &[(0.0, -30.0, 2_500.0), (12_500.0, db, 5_000.0)],
                41 + i,
            ));
        }
        let (_, events) = run_blocks_at(cb_params(Some(12_500)), blocks, 51_200).await;
        assert!(events.len() <= 3, "{} events: {events:?}", events.len());
    }

    #[tokio::test]
    async fn guard_params_out_of_range_fail_the_stage() {
        let fails = |p: serde_json::Value, block: bool, want: String| async move {
            let (tx, _rx, h) = spawn(p);
            if block {
                tx.send(iq_frame(vec![Complex::new(1.0, 0.0); CB_BLOCK], 51_200))
                    .unwrap();
            }
            match h.await.unwrap() {
                Err(StageError::Failed(m)) => assert_eq!(m, want),
                r => panic!("expected Failed, got {r:?}"),
            }
        };
        for v in [-1, 61] {
            let mut p = cb_params(Some(12_500));
            p["guard_db"] = v.into();
            fails(
                p,
                false,
                format!("nbfm_demod params: guard_db {v} out of range 0..=60"),
            )
            .await;
        }
        let need = "nbfm_demod params: guard_hz needs channel_hz with channel_hz + 2 guard_hz <= the channel rate";
        let mut p = cb_params(Some(12_500));
        p["channel_hz"] = serde_json::Value::Null;
        fails(p, false, need.into()).await;
        // 12 500 + 2 · 25 000 > 51 200.
        fails(cb_params(Some(25_000)), true, need.into()).await;
    }
}
