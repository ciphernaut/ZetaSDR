//! Wideband (broadcast) FM demodulator stage: phase discriminator, resampling
//! to at most [`WFM_AUDIO_STAGE_RATE_HZ`], de-emphasis, 15 kHz audio low-pass,
//! then resampling to the audio rate.
//!
//! With `stereo: true` the output is two channels (L, R interleaved) for the whole run.
//! The multiplex (MPX) at the stage rate is decoded as receivers do: the 19 kHz pilot,
//! band-passed, locks a PLL whose doubled phase is the suppressed 38 kHz carrier; L-R is
//! the MPX mixed with that carrier and low-passed at 15 kHz, L+R the MPX through the
//! same low-pass (so the two are delay-matched); L = sum + diff, R = sum - diff, each
//! de-emphasised and resampled on its own. While the pilot reads under the threshold
//! (no pilot, or too weak to trust) both channels carry L+R. The reading is noted to the
//! job as `job_context.wfm` (`pilot_db`, `stereo`).

use std::f64::consts::PI;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::nbfm::{AudioFilter, nbfm_demod};
use crate::rds::{Biquad, PILOT_HZ};
use crate::util::{StreamingFir, lowpass_taps};
use crate::{Input, frame_span, run_map};

/// Cutoff of the mono audio low-pass, Hz; above it sit the 19 kHz pilot and
/// the stereo and RDS subcarriers.
pub const WFM_AUDIO_CUTOFF_HZ: f64 = 15_000.0;
/// Taps of the audio low-pass. It runs at no more than
/// [`WFM_AUDIO_STAGE_RATE_HZ`], so the transition band (about
/// `4 × rate / taps`) stays under 8 kHz and the 19 kHz pilot stays out.
pub const WFM_AUDIO_TAPS: usize = 127;
/// Highest rate the de-emphasis and audio low-pass run at, Hz: a faster
/// discriminator output is first resampled to this rate, so the low-pass
/// tap count does not grow with the input rate. 240 kHz still carries the
/// whole MPX band (to 57 kHz RDS) and divides 48 kHz audio exactly.
pub const WFM_AUDIO_STAGE_RATE_HZ: u64 = 240_000;

/// The pilot reading over which a mono decode turns stereo, dB (the locked pilot's power
/// against the MPX's). A pilot sent at the standard 8-10 % of deviation reads -24 dB or
/// more on a clean signal whatever the programme: the multiplex is peak-limited at 100 %
/// deviation, so its power is at most full scale. Noise lowers the reading only as the
/// signal is lost (a synthesised station reads -18 dB at 3 dB SNR over a 240 kHz
/// capture, under -28 dB only at -5 dB), and with no pilot the loop reads the noise in
/// its few-hertz band: about -46 dB on noise alone, -34 dB at its highest. So -30 dB
/// keeps every clean station's pilot with 6 dB to spare and does not take noise for one. It does not drop a merely weak station
/// to mono: that needs a reading of the noise, or blend, neither built.
pub const STEREO_ON_DB: f64 = -30.0;
/// The pilot reading under which a stereo decode falls back to mono, dB: 6 dB under
/// [`STEREO_ON_DB`]. The reading moves with the programme's loudness (the MPX's power,
/// averaged over [`PILOT_AVERAGE_S`]), by 4-5 dB from one phrase to the next, so a
/// marginal pilot switches once each way rather than with every phrase.
pub const STEREO_OFF_DB: f64 = -36.0;
/// Time constant of the pilot reading's averages (the locked pilot's amplitude and the
/// MPX's power), s.
pub const PILOT_AVERAGE_S: f64 = 0.1;
/// Lowest stage rate the stereo decode runs at, Hz: L-R reaches 38 + 15 = 53 kHz, which
/// must sit under the 0.45 of the rate a resampler passes. Below it both channels carry
/// L+R and the reading is `null`.
pub const STEREO_MIN_RATE_HZ: u64 = 120_000;
/// Floor of the reading, dB (no pilot over a silent MPX).
const PILOT_DB_FLOOR: f64 = -120.0;
/// Half-width of the pilot band-pass, Hz, and the pilot PLL's natural frequency (Hz)
/// and damping: as `rds_decode`'s.
const PILOT_BW_HZ: f64 = 100.0;
const PLL_WN_HZ: f64 = 20.0;
const PLL_ZETA: f64 = 0.707;
/// Time constant of the band-passed pilot's power that normalises the PLL's phase
/// detector, s (`rds_decode`'s, at its 240 kHz).
const PLL_LEVEL_S: f64 = 0.042;
/// How often the reading is noted to the job when it has not changed state.
const PILOT_NOTE_EVERY: Duration = Duration::from_secs(5);

/// FM de-emphasis in place: the single-pole IIR `y[n] = a·x[n] + (1−a)·y[n−1]`
/// with `a = 1 − exp(−1/(rate_hz·τ))`, `τ = tau_us` µs. `state` is `y[−1]` on
/// entry and is left holding the last output.
pub fn deemphasis(x: &mut [f32], rate_hz: u64, tau_us: u64, state: &mut f32) {
    let a = (1.0 - (-1.0 / (rate_hz as f64 * tau_us as f64 * 1e-6)).exp()) as f32;
    let mut y = *state;
    for s in x.iter_mut() {
        y = a * *s + (1.0 - a) * y;
        *s = y;
    }
    *state = y;
}

#[derive(Clone, Copy, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct WfmParams {
    /// Peak deviation that maps to ±1.0, Hz (75 000 for broadcast FM).
    pub deviation_hz: u64,
    pub audio_rate_hz: u64,
    /// De-emphasis time constant, µs: 50 (Australia, Europe) or 75 (Americas).
    pub deemphasis_us: u64,
    /// Decode stereo: the output is two channels (L, R interleaved) for the whole run,
    /// both L+R while the 19 kHz pilot reads under the threshold (`job_context.wfm`:
    /// `pilot_db`, `stereo`). `false` (the default): one channel, mono.
    #[serde(default)]
    pub stereo: bool,
}

/// The stereo pilot's reading, noted to the job as `job_context.wfm`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Pilot {
    /// The locked pilot's power against the MPX's, dB, to 0.1 dB; `None` before the
    /// first block and under [`STEREO_MIN_RATE_HZ`].
    pub pilot_db: Option<f64>,
    /// Whether L and R are decoded (else both channels carry L+R).
    pub stereo: bool,
}

/// The pilot PLL and the reading that decides stereo, at one stage rate.
#[derive(Debug)]
struct PilotLock {
    bp: Biquad,
    /// Smoothed power of the band-passed pilot, normalising the phase detector.
    bp_pow: f64,
    level_alpha: f64,
    /// PLL phase (cycles of the pilot, wrapped), frequency (cycles a sample) and the
    /// integrator's correction to it.
    theta: f64,
    freq: f64,
    integ: f64,
    kp: f64,
    ki: f64,
    /// Smoothed `2 p sin(phase)`: the amplitude of the pilot in phase with the lock.
    coherent: f64,
    /// Smoothed power of the MPX.
    mpx_pow: f64,
    alpha: f64,
    /// [`STEREO_ON_DB`] and [`STEREO_OFF_DB`] as power ratios.
    on: f64,
    off: f64,
    stereo: bool,
}

impl PilotLock {
    fn new(rate_hz: u64) -> Self {
        let fs = rate_hz as f64;
        let wn = 2.0 * PI * PLL_WN_HZ / fs;
        let alpha = |tau_s: f64| 1.0 - (-1.0 / (tau_s * fs)).exp();
        Self {
            bp: Biquad::bandpass(PILOT_HZ, PILOT_HZ / (2.0 * PILOT_BW_HZ), fs),
            bp_pow: 0.0,
            level_alpha: alpha(PLL_LEVEL_S),
            theta: 0.0,
            freq: PILOT_HZ / fs,
            integ: 0.0,
            // Loop gains in cycles: phase error e (radians) / 2π.
            kp: 2.0 * PLL_ZETA * wn / (2.0 * PI),
            ki: wn * wn / (2.0 * PI),
            coherent: 0.0,
            mpx_pow: 0.0,
            alpha: alpha(PILOT_AVERAGE_S),
            on: 10f64.powf(STEREO_ON_DB / 10.0),
            off: 10f64.powf(STEREO_OFF_DB / 10.0),
            stereo: false,
        }
    }

    /// One MPX sample `x` through the lock and the reading: the 38 kHz carrier
    /// `sin(2 phase)` for it, in phase with L-R as transmitted.
    fn step(&mut self, x: f64) -> f64 {
        let p = self.bp.run(x);
        self.bp_pow += self.level_alpha * (p * p - self.bp_pow);
        let amp = (2.0 * self.bp_pow).sqrt();
        // NCO estimate sin(2π θ) of the pilot; p · cos ≈ (A/2) sin(phase error).
        let (sn, cs) = (2.0 * PI * self.theta).sin_cos();
        let e = if amp > 1e-9 {
            (2.0 * p * cs / amp).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        self.integ += self.ki * e;
        self.theta = (self.theta + self.freq + self.integ + self.kp * e).rem_euclid(1.0);
        self.coherent += self.alpha * (2.0 * p * sn - self.coherent);
        self.mpx_pow += self.alpha * (x * x - self.mpx_pow);
        // Compared without the log: the pilot's power A²/2 against the threshold's
        // share of the MPX's.
        let pilot_pow = self.pilot_pow();
        let share = if self.stereo { self.off } else { self.on };
        self.stereo = pilot_pow > share * self.mpx_pow;
        2.0 * sn * cs
    }

    /// The pilot's power, `A²/2` of its smoothed in-phase amplitude. Unlocked, that
    /// amplitude wanders about zero, so the power reads the noise in the loop's band.
    fn pilot_pow(&self) -> f64 {
        self.coherent.powi(2) / 2.0
    }

    /// The reading in dB, floored at [`PILOT_DB_FLOOR`].
    fn pilot_db(&self) -> f64 {
        let pilot_pow = self.pilot_pow();
        if self.mpx_pow <= 0.0 || pilot_pow <= 0.0 {
            return PILOT_DB_FLOOR;
        }
        (10.0 * (pilot_pow / self.mpx_pow).log10()).max(PILOT_DB_FLOOR)
    }
}

/// The stereo decode at one stage rate: the pilot lock (none under
/// [`STEREO_MIN_RATE_HZ`]), the L+R and L-R low-passes, and per channel the
/// de-emphasis state and the resampler to the audio rate.
#[derive(Debug)]
struct StereoDecoder {
    rate_hz: u64,
    lock: Option<PilotLock>,
    sum: StreamingFir,
    diff: StreamingFir,
    deemph: [f32; 2],
    audio: [AudioFilter; 2],
}

impl StereoDecoder {
    fn new(rate_hz: u64) -> Self {
        // As the mono low-pass: above 2 × 15 kHz the cutoff stays put.
        let cutoff = WFM_AUDIO_CUTOFF_HZ.min(0.45 * rate_hz as f64);
        let taps = lowpass_taps(cutoff, rate_hz as f64, WFM_AUDIO_TAPS);
        Self {
            rate_hz,
            lock: (rate_hz >= STEREO_MIN_RATE_HZ).then(|| PilotLock::new(rate_hz)),
            sum: StreamingFir::new(taps.clone()),
            diff: StreamingFir::new(taps),
            deemph: [0.0; 2],
            audio: Default::default(),
        }
    }

    /// Decode MPX `x` at the stage rate to interleaved L, R at `p.audio_rate_hz`.
    fn process(&mut self, x: &[f32], p: &WfmParams) -> Result<Vec<f32>, String> {
        let sum = self.sum.process(x);
        let (mut l, mut r) = match &mut self.lock {
            Some(lock) => {
                let mut mixed = Vec::with_capacity(x.len());
                let mut on = Vec::with_capacity(x.len());
                for &s in x {
                    let carrier = lock.step(s as f64);
                    mixed.push((2.0 * s as f64 * carrier) as f32);
                    on.push(lock.stereo);
                }
                let diff = self.diff.process(&mixed);
                let mut l = Vec::with_capacity(x.len());
                let mut r = Vec::with_capacity(x.len());
                for ((&s, &d), &on) in sum.iter().zip(&diff).zip(&on) {
                    let d = if on { d } else { 0.0 };
                    l.push(s + d);
                    r.push(s - d);
                }
                (l, r)
            }
            None => (sum.clone(), sum),
        };
        deemphasis(&mut l, self.rate_hz, p.deemphasis_us, &mut self.deemph[0]);
        deemphasis(&mut r, self.rate_hz, p.deemphasis_us, &mut self.deemph[1]);
        let l = self.audio[0].process(&l, self.rate_hz, p.audio_rate_hz)?;
        let r = self.audio[1].process(&r, self.rate_hz, p.audio_rate_hz)?;
        Ok(l.iter()
            .zip(&r)
            .flat_map(|(&a, &b)| [a.clamp(-1.0, 1.0), b.clamp(-1.0, 1.0)])
            .collect())
    }
}

/// Streaming WFM demodulator: per block, discriminator at `deviation_hz`, a
/// resampler to the stage rate `min(input rate, WFM_AUDIO_STAGE_RATE_HZ)`
/// (identity when equal), de-emphasis and a 15 kHz low-pass at the stage
/// rate, then a resampler to `audio_rate_hz`; output clamped to ±1.0. With
/// `stereo`, the stage-rate MPX goes to the stereo decode instead (see the module
/// docs) and the output is L, R interleaved. State carries across blocks; the filters
/// are rebuilt when the input rate changes.
#[derive(Debug)]
pub struct WfmDemod {
    p: WfmParams,
    prev: Complex<f32>,
    deemph: f32,
    /// Discriminator output (input rate) to the stage rate.
    stage: AudioFilter,
    /// Audio low-pass keyed by the stage rate.
    lpf: Option<(u64, StreamingFir)>,
    lpf_taps: usize,
    audio: AudioFilter,
    /// The stereo decode, built for the stage rate (with `stereo` only).
    stereo: Option<StereoDecoder>,
}

impl WfmDemod {
    /// Checks `p`: `deviation_hz` and `audio_rate_hz` non-zero, `deemphasis_us`
    /// 50 or 75; the error names the parameter.
    pub fn new(p: &WfmParams) -> Result<Self, String> {
        if p.deviation_hz == 0 {
            return Err("wfm_demod params: deviation_hz must be >= 1".into());
        }
        if p.audio_rate_hz == 0 {
            return Err("wfm_demod params: audio_rate_hz must be >= 1".into());
        }
        if !matches!(p.deemphasis_us, 50 | 75) {
            return Err(format!(
                "wfm_demod params: deemphasis_us must be 50 or 75, got {}",
                p.deemphasis_us
            ));
        }
        Ok(Self {
            p: *p,
            prev: Complex::new(0.0, 0.0),
            deemph: 0.0,
            stage: AudioFilter::default(),
            lpf: None,
            lpf_taps: 0,
            audio: AudioFilter::default(),
            stereo: None,
        })
    }

    /// Demodulate one block of `iq` at `rate_hz` to audio at `audio_rate_hz`:
    /// [`channels`](Self::channels) samples a frame, interleaved.
    pub fn process(&mut self, iq: &[Complex<f32>], rate_hz: u64) -> Result<Vec<f32>, String> {
        if rate_hz == 0 {
            return Err("wfm_demod: input sample rate is 0".into());
        }
        let d = nbfm_demod(iq, rate_hz, self.p.deviation_hz, &mut self.prev);
        let stage_hz = rate_hz.min(WFM_AUDIO_STAGE_RATE_HZ);
        let mut x = self.stage.process(&d, rate_hz, stage_hz)?;
        if self.p.stereo {
            if self.stereo.as_ref().is_none_or(|s| s.rate_hz != stage_hz) {
                self.stereo = Some(StereoDecoder::new(stage_hz));
            }
            return match &mut self.stereo {
                Some(s) => s.process(&x, &self.p),
                None => Ok(Vec::new()),
            };
        }
        deemphasis(&mut x, stage_hz, self.p.deemphasis_us, &mut self.deemph);
        if self.lpf.as_ref().is_none_or(|(r, _)| *r != stage_hz) {
            // Above 2 × 15 kHz the cutoff stays put; below, the resampler's
            // own low-pass is what limits the band.
            let cutoff = WFM_AUDIO_CUTOFF_HZ.min(0.45 * stage_hz as f64);
            let taps = lowpass_taps(cutoff, stage_hz as f64, WFM_AUDIO_TAPS);
            self.lpf_taps = taps.len();
            self.lpf = Some((stage_hz, StreamingFir::new(taps)));
        }
        let x = self
            .lpf
            .as_mut()
            .map_or_else(Vec::new, |(_, f)| f.process(&x));
        let mut y = self.audio.process(&x, stage_hz, self.p.audio_rate_hz)?;
        for s in &mut y {
            *s = s.clamp(-1.0, 1.0);
        }
        Ok(y)
    }

    /// Taps in the current audio low-pass (0 before the first block):
    /// [`WFM_AUDIO_TAPS`] whatever the input rate.
    #[doc(hidden)]
    pub fn audio_lpf_taps(&self) -> usize {
        self.lpf_taps
    }

    /// Audio channels in every output: 2 with `stereo`, else 1.
    pub fn channels(&self) -> u16 {
        if self.p.stereo { 2 } else { 1 }
    }

    /// The pilot's reading as of the last block (`None` without `stereo`).
    pub fn pilot(&self) -> Option<Pilot> {
        if !self.p.stereo {
            return None;
        }
        let lock = self.stereo.as_ref().and_then(|s| s.lock.as_ref());
        Some(Pilot {
            pilot_db: lock.map(|l| (l.pilot_db() * 10.0).round() / 10.0),
            stereo: lock.is_some_and(|l| l.stereo),
        })
    }
}

/// Demodulates a wideband FM IQ stream to de-emphasised audio at `audio_rate_hz`:
/// mono, or with `stereo` two channels and the pilot's reading noted to the job as
/// `job_context.wfm` (`pilot_db`, `stereo`) when it turns stereo or mono, and every
/// five seconds otherwise.
pub struct WfmDemodStage {
    spec: StageSpec,
}

impl WfmDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: "wfm_demod",
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(WfmParams),
            },
        }
    }
}

impl Default for WfmDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for WfmDemodStage {
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
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let p: WfmParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("wfm_demod params: {e}")))?;
        let mut demod = WfmDemod::new(&p).map_err(StageError::Failed)?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let channels = demod.channels();
        // The stereo state last noted, and when.
        let mut noted: Option<(bool, Instant)> = None;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod = WfmDemod::new(&p).map_err(StageError::Failed)?;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(
                    "wfm_demod stage expects iq frames".into(),
                ));
            };
            let rate = block.sample_rate_hz.0;
            let samples = demod
                .process(&block.samples, rate)
                .map_err(StageError::Failed)?;
            // The pilot's reading on a change of state, else every PILOT_NOTE_EVERY.
            if let Some(pilot) = demod.pilot() {
                let now = Instant::now();
                if noted.is_none_or(|(stereo, at)| {
                    stereo != pilot.stereo || now.duration_since(at) >= PILOT_NOTE_EVERY
                }) {
                    noted = Some((pilot.stereo, now));
                    ctx.note(serde_json::json!({ "wfm": pilot }));
                }
            }
            if samples.is_empty() {
                return Ok(Vec::new());
            }
            let mut prov = frame.provenance.clone();
            prov.span = frame_span(block.t0, block.samples.len(), rate);
            Ok(vec![Frame::new(
                out.clone(),
                prov,
                Payload::Audio {
                    sample_rate_hz: Hz(p.audio_rate_hz),
                    channels,
                    samples: samples.into(),
                },
            )])
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wfm_stage_rejects_bad_deemphasis_naming_the_param() {
        let bus = Arc::new(StreamBus::new(4));
        let (_tx, rx) = broadcast::channel(4);
        let r = WfmDemodStage::new()
            .run(
                serde_json::json!({"deviation_hz": 75_000, "audio_rate_hz": 48_000, "deemphasis_us": 60}),
                vec![rx],
                vec![StreamId::new()],
                bus,
                CancellationToken::new(),
                zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
            )
            .await;
        match r {
            Err(StageError::Failed(m)) => assert!(m.contains("deemphasis_us"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    /// Run the stage with `params` over `iq` at 240 kHz in 20 ms blocks: its audio
    /// frames and the notes it gave the job.
    async fn run_stage(
        params: serde_json::Value,
        iq: &[Complex<f32>],
    ) -> (Vec<Frame>, Vec<serde_json::Value>) {
        use zeta_core::port::IqBlock;
        use zeta_core::types::{Provenance, SourceId, Span, Timestamp};
        let prov = Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let blocks: Vec<_> = iq.chunks(4_800).collect();
        let bus = Arc::new(StreamBus::new(blocks.len() + 8));
        let out = bus.register(StreamKind::Audio, prov.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(blocks.len() + 8);
        let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = notes.clone();
        let ctx = StageContext::new(SourceId("s".into()))
            .with_note(Arc::new(move |v| sink.lock().unwrap().push(v)));
        for (i, b) in blocks.iter().enumerate() {
            let block = IqBlock {
                t0: Timestamp(1_000_000_000 + i as u64 * 20_000_000),
                sample_rate_hz: Hz(240_000),
                center_hz: Hz(100_000_000),
                samples: b.to_vec().into(),
            };
            tx.send(Frame::new(
                StreamId::new(),
                prov.clone(),
                Payload::Iq(block),
            ))
            .unwrap();
        }
        drop(tx);
        WfmDemodStage::new()
            .run(
                params,
                vec![rx],
                vec![out],
                bus,
                CancellationToken::new(),
                ctx,
            )
            .await
            .unwrap();
        let mut frames = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            frames.push(f);
        }
        let notes = notes.lock().unwrap().clone();
        (frames, notes)
    }

    #[tokio::test]
    async fn a_stereo_stage_sends_two_channel_frames_and_notes_the_pilot() {
        // 1 s of a station whose pilot comes on halfway.
        use crate::synth::{STEREO_PILOT_LEVEL, stereo_mpx, tone, wfm_modulate_composite};
        let left = tone(240_000, 240_000, 1_000.0, 0.4);
        let right = tone(240_000, 240_000, 3_000.0, 0.4);
        let pilot = tone(240_000, 240_000, PILOT_HZ, STEREO_PILOT_LEVEL);
        let mut mpx = stereo_mpx(&left, &right, 240_000, 0.0);
        for (m, p) in mpx.iter_mut().zip(&pilot).skip(120_000) {
            *m += p;
        }
        let iq = wfm_modulate_composite(&mpx, 240_000, 75_000.0);
        let p = serde_json::json!({"deviation_hz": 75_000, "audio_rate_hz": 48_000,
            "deemphasis_us": 50, "stereo": true});
        let (frames, notes) = run_stage(p, &iq).await;
        assert_eq!(frames.len(), 50);
        let mut per_channel = 0;
        for (i, f) in frames.iter().enumerate() {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = &f.payload
            else {
                panic!("expected audio");
            };
            assert_eq!((*sample_rate_hz, *channels), (Hz(48_000), 2));
            assert_eq!(samples.len() % 2, 0);
            per_channel += samples.len() / 2;
            // The frame spans its IQ block, 20 ms, whatever its channel count.
            let start = 1_000_000_000 + i as u64 * 20_000_000;
            assert_eq!(f.provenance.span.start_ns.0, start);
            assert_eq!(f.provenance.span.end_ns.0, start + 20_000_000);
        }
        assert!(per_channel.abs_diff(48_000) <= 2, "{per_channel} a channel");
        // Mono at the first block, stereo once the pilot came; nothing more within 5 s.
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0]["wfm"]["pilot_db"].as_f64().unwrap() < STEREO_OFF_DB);
        assert_eq!(notes[0]["wfm"]["stereo"], false);
        assert_eq!(notes[1]["wfm"]["stereo"], true);
        let db = notes[1]["wfm"]["pilot_db"].as_f64().unwrap();
        assert!(db > STEREO_ON_DB, "{notes:?}");
    }

    #[tokio::test]
    async fn a_mono_stage_sends_one_channel_and_notes_nothing() {
        let audio = crate::synth::tone(48_000, 9_600, 1_000.0, 0.4);
        let iq = crate::synth::wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
        let p = serde_json::json!({"deviation_hz": 75_000, "audio_rate_hz": 48_000,
            "deemphasis_us": 50});
        let (frames, notes) = run_stage(p, &iq).await;
        assert_eq!(frames.len(), 10);
        for f in &frames {
            assert!(matches!(f.payload, Payload::Audio { channels: 1, .. }));
        }
        assert!(notes.is_empty(), "{notes:?}");
    }
}
