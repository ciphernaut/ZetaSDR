//! OOK demodulation: a 0..1 envelope decision signal out of on-off keying.

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::fsk::{
    DISCRIMINATOR_LOWPASS_FACTOR, Decision, Levels, MIN_SCALE, WARMUP_DELAYS, decision_lowpass,
    run_decision,
};
use crate::sps_check;
use crate::util::StreamingFir;

/// The stage kind, as graphs name it.
pub const OOK_DEMOD_KIND: &str = "ook_demod";

/// `ook_demod` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct OokDemodParams {
    /// The symbol rate in baud.
    pub symbol_rate_baud: u32,
}

/// The OOK demodulator over one run of contiguous input.
struct Demod {
    rate_hz: u64,
    fir: StreamingFir,
    delay: usize,
    levels: Levels,
}

impl Demod {
    fn new(p: &OokDemodParams, rate_hz: u64) -> Result<Demod, StageError> {
        sps_check(OOK_DEMOD_KIND, rate_hz, p.symbol_rate_baud).map_err(StageError::Failed)?;
        let cutoff = DISCRIMINATOR_LOWPASS_FACTOR * p.symbol_rate_baud as f64;
        let (fir, delay) = decision_lowpass(cutoff, rate_hz, p.symbol_rate_baud);
        Ok(Demod {
            rate_hz,
            fir,
            delay,
            levels: Levels::new(rate_hz, p.symbol_rate_baud, WARMUP_DELAYS * delay),
        })
    }

    /// Scale low-passed envelope samples to 0 (off) .. 1 (on) by the running levels.
    fn scale(&mut self, filtered: Vec<f32>) -> Vec<f32> {
        filtered
            .into_iter()
            .map(|v| {
                let v = v as f64;
                let Some((lo, hi)) = self.levels.push(v) else {
                    return 0.0;
                };
                ((v - lo) / (hi - lo).max(MIN_SCALE)) as f32
            })
            .collect()
    }
}

impl Decision for Demod {
    fn rate_hz(&self) -> u64 {
        self.rate_hz
    }

    fn process(&mut self, x: &[Complex<f32>]) -> Vec<f32> {
        let env: Vec<f32> = x.iter().map(|s| s.norm()).collect();
        let filtered = self.fir.process(&env);
        self.scale(filtered)
    }

    /// Silence for the group delay: after the input the carrier is off.
    fn flush(&mut self) -> Vec<f32> {
        let filtered = self.fir.process(&vec![0.0; self.delay]);
        self.scale(filtered)
    }
}

/// Demodulates on-off keying into a 1-channel decision signal at the input rate: the
/// envelope `|x|`, low-passed at [`DISCRIMINATOR_LOWPASS_FACTOR`] × `symbol_rate_baud`,
/// scaled to 0 (off) .. 1 (on) by the running off and on levels (see
/// [`crate::fsk::Levels`]). An input under [`crate::MIN_SAMPLES_PER_SYMBOL`] samples per
/// symbol fails the stage naming the rate to resample to. A seam ([`crate::is_seam`]), a
/// lag, a rate change and the input's end flush the low-pass (silence for its group
/// delay) and restart the filter and the levels.
pub struct OokDemodStage {
    spec: StageSpec,
}

impl OokDemodStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: OOK_DEMOD_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Audio],
                params_schema: schemars::schema_for!(OokDemodParams),
            },
        }
    }
}

impl Default for OokDemodStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for OokDemodStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: OokDemodParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        if p.symbol_rate_baud == 0 {
            return Err("symbol_rate_baud: at least 1".into());
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
        let p: OokDemodParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{OOK_DEMOD_KIND} params: {e}")))?;
        let [out] = <[StreamId; 1]>::try_from(outputs)
            .map_err(|o| StageError::Failed(format!("expected 1 output, got {}", o.len())))?;
        run_decision(OOK_DEMOD_KIND, inputs, out, bus, cancel, move |rate| {
            Demod::new(&p, rate)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use num_complex::Complex;
    use serde_json::json;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::port::IqBlock;
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, StreamKind, Timestamp};

    use super::OokDemodStage;
    use crate::frame_span;

    const RATE: u64 = 48_000;
    const BAUD: u32 = 2_400;
    const SPS: usize = (RATE / BAUD as u64) as usize;
    /// The decision low-pass's group delay at [`RATE`]: 41 taps.
    const DELAY: usize = 20;
    const T0: u64 = 1_000_000_000;
    /// Symbols per run of on or off in the test signals.
    const RUN: usize = 4;
    /// The on amplitude of the test signals.
    const ON: f32 = 0.3;

    fn prov(t0: u64, n: usize, rate: u64) -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(t0), n, rate),
        }
    }

    /// `symbols` symbols at [`RATE`] alternating in runs of [`RUN`] between `amp` (first)
    /// and silence.
    fn keyed(symbols: usize, amp: f32) -> Vec<Complex<f32>> {
        (0..symbols * SPS)
            .map(|i| {
                let on = (i / SPS / RUN).is_multiple_of(2);
                Complex::new(if on { amp } else { 0.0 }, 0.0)
            })
            .collect()
    }

    /// The stage's output frames' samples over `input` (`(t0, iq, rate)` frames).
    async fn run_frames(
        input: Vec<(u64, Vec<Complex<f32>>, u64)>,
    ) -> Result<Vec<Vec<f32>>, StageError> {
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Audio, prov(0, 0, RATE));
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(input.len().max(1));
        for (t0, iq, rate) in input {
            tx.send(Frame::new(
                zeta_core::types::StreamId::new(),
                prov(t0, iq.len(), rate),
                Payload::Iq(IqBlock {
                    t0: Timestamp(t0),
                    sample_rate_hz: Hz(rate),
                    center_hz: Hz(0),
                    samples: iq.into(),
                }),
            ))
            .unwrap();
        }
        drop(tx);
        OokDemodStage::new()
            .run(
                json!({"symbol_rate_baud": BAUD}),
                vec![rx],
                vec![out],
                bus.clone(),
                CancellationToken::new(),
                StageContext::new(SourceId("s".into())),
            )
            .await?;
        let mut frames = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = f.payload
            else {
                panic!("an audio frame")
            };
            assert_eq!((sample_rate_hz.0, channels), (RATE, 1));
            frames.push(samples.to_vec());
        }
        Ok(frames)
    }

    /// The mean output over input symbol `k` (the low-pass's delay allowed for).
    fn symbol_mean(y: &[f32], k: usize) -> f32 {
        let w = &y[k * SPS + DELAY..(k + 1) * SPS + DELAY];
        w.iter().sum::<f32>() / SPS as f32
    }

    /// The middle symbol of the on run and of the off run starting at symbol `k`.
    fn on_off(y: &[f32], k: usize) -> (f32, f32) {
        (
            symbol_mean(y, k + RUN / 2),
            symbol_mean(y, k + RUN + RUN / 2),
        )
    }

    #[tokio::test]
    async fn on_and_off_map_to_one_and_zero() {
        const SYMBOLS: usize = 64;
        let iq = keyed(SYMBOLS, ON);
        let n = iq.len();
        let y = run_frames(vec![(T0, iq, RATE)]).await.unwrap().concat();
        // The flush lets the last input samples out: one output per input, plus the delay.
        assert_eq!(y.len(), n + DELAY);
        let (on, off) = on_off(&y, SYMBOLS - 2 * RUN);
        assert!((on - 1.0).abs() < 0.1, "on reads {on}");
        assert!(off.abs() < 0.1, "off reads {off}");
    }

    /// Symbols after an impulse within which the on-level reads 1 again (within 0.1): the
    /// release takes back the quarter of the excess the attack let in, at 1/64 a period.
    /// Measured: 96 symbols suffice and 64 do not; the bound keeps a margin.
    const IMPULSE_RECOVERY_SYMBOLS: usize = 128;

    #[tokio::test]
    async fn the_on_level_recovers_from_an_impulse() {
        // 64 keyed symbols, one symbol at 5x the on amplitude, then keying again.
        const BEFORE: usize = 64;
        const AFTER: usize = 320;
        let mut iq = keyed(BEFORE, ON);
        iq.extend(vec![Complex::new(5.0 * ON, 0.0); SPS]);
        // The keying resumes on a whole run boundary after the impulse symbol.
        iq.extend(vec![Complex::new(0.0, 0.0); (RUN - 1) * SPS]);
        iq.extend(keyed(AFTER, ON));
        let y = run_frames(vec![(T0, iq, RATE)]).await.unwrap().concat();
        let resume = BEFORE + RUN;
        let (on, _) = on_off(&y, resume);
        assert!(on < 0.9, "the impulse should lift the on level, reads {on}");
        for k in (resume + IMPULSE_RECOVERY_SYMBOLS..resume + AFTER - 2 * RUN).step_by(2 * RUN) {
            let (on, off) = on_off(&y, k);
            assert!((on - 1.0).abs() < 0.1, "symbol {k}: on reads {on}");
            assert!(off.abs() < 0.1, "symbol {k}: off reads {off}");
        }
    }

    #[tokio::test]
    async fn a_seam_resets_the_levels_and_flushes_the_run() {
        // 32 symbols at amplitude 1, a 1 s gap, then 32 at 0.2: after the seam the levels
        // start again, so the quieter stream reads 1; carried over it would read 0.2.
        const SYMBOLS: usize = 32;
        let (a, b) = (keyed(SYMBOLS, 1.0), keyed(SYMBOLS, 0.2));
        let n = a.len();
        let frames = run_frames(vec![(T0, a, RATE), (T0 + 2_000_000_000, b, RATE)])
            .await
            .unwrap();
        let lens: Vec<usize> = frames.iter().map(Vec::len).collect();
        assert_eq!(lens, [n, DELAY, n, DELAY]);
        let (on, off) = on_off(&frames[2], SYMBOLS - 2 * RUN);
        assert!((on - 1.0).abs() < 0.1, "after the seam on reads {on}");
        assert!(off.abs() < 0.1, "after the seam off reads {off}");
    }

    #[tokio::test]
    async fn too_few_samples_per_symbol_is_refused() {
        let e = run_frames(vec![(T0, vec![Complex::new(1.0, 0.0); 100], 7_200)])
            .await
            .unwrap_err();
        assert!(
            e.to_string().contains(
                "ook_demod: 7200 Hz is 3.0 samples per symbol at 2400 Bd; resample to at least 9600 Hz"
            ),
            "{e}"
        );
    }

    #[test]
    fn params_are_checked_naming_the_field() {
        let s = OokDemodStage::new();
        assert!(s.check_params(&json!({"symbol_rate_baud": 2400})).is_ok());
        assert!(s.check_params(&json!({})).is_err());
        assert!(
            s.check_params(&json!({"symbol_rate_baud": 2400, "order": 2}))
                .is_err()
        );
        assert_eq!(
            s.check_params(&json!({"symbol_rate_baud": 0})).unwrap_err(),
            "symbol_rate_baud: at least 1"
        );
    }
}
