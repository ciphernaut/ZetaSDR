//! NBFM demodulator stage: phase discriminator, then resampling to the audio rate.

use std::f64::consts::PI;
use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

use crate::resample::Resampler;
use crate::squelch::{
    default_true, squelch_event, squelch_outputs, squelch_streams, stage_squelch,
};
use crate::{Input, frame_span, run_map};

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
}

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
        _ctx: StageContext,
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
        let (out, events) = squelch_streams(&outputs, squelch.is_some())?;
        let mut prev = Complex::new(0.0f32, 0.0);
        let mut filter = AudioFilter::default();
        let mut was_open = false;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    prev = Complex::new(0.0, 0.0);
                    filter = AudioFilter::default();
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
            let mut published = Vec::new();
            if let (Some(sq), Some(events)) = (squelch.as_mut(), &events) {
                if let Some(open) = sq.feed(&block.samples, rate) {
                    published.push(Frame::new(
                        events.clone(),
                        prov.clone(),
                        Payload::Event(squelch_event(sq, open)),
                    ));
                }
                if !sq.is_open() {
                    // Skipped, but the discriminator's reference follows the channel, so a
                    // reopening block differences against the sample just before it.
                    if let Some(&x) = block
                        .samples
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
                }
            }
            let demod = nbfm_demod(&block.samples, rate, p.deviation_hz, &mut prev);
            let samples = filter
                .process(&demod, rate, p.audio_rate_hz)
                .map_err(StageError::Failed)?;
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
    use crate::fixtures::{awgn, nbfm_modulate, tone};

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
                    zeta_core::stage::StageContext::new(zeta_core::types::SourceId("s".into())),
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
        let (tx, mut rx_out, mut rx_ev, h) = spawn_with(params, 1_024);
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
        let mut audio = 0;
        while let Ok(f) = rx_out.try_recv() {
            assert!(matches!(f.payload, Payload::Audio { .. }));
            audio += 1;
        }
        let mut events = Vec::new();
        while let Ok(f) = rx_ev.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("expected an event payload")
            };
            events.push(v);
        }
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
        assert_eq!(keys.len(), 4, "{e}");

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
}
