//! OOK demodulation: a 0..1 envelope decision signal out of on-off keying.

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

use crate::fsk::{
    DISCRIMINATOR_LOWPASS_FACTOR, MIN_SCALE, NORMALISER_SYMBOLS, Periods, decision_lowpass,
};
use crate::util::StreamingFir;
use crate::{Input, is_seam, run_map, sps_check};

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

/// The running off and on levels of the envelope.
struct Levels {
    periods: Periods,
    /// `(lo, hi)` once the first symbol period has closed.
    levels: Option<(f64, f64)>,
}

impl Levels {
    /// Take one envelope sample; the `(lo, hi)` to scale it by, so the output stays in
    /// 0..1. A sample under `lo` or over `hi` takes it at once; at each period's close
    /// its minimum moves `lo` back by `1/NORMALISER_SYMBOLS` when it lies under the
    /// midpoint, and its maximum moves `hi` when it lies over it, so a run of off (or
    /// on) symbols does not drag the other level. The first period after the low-pass's
    /// warm-up seeds both, and until it closes the extremes so far stand in.
    fn of(&mut self, v: f64) -> (f64, f64) {
        let closed = self.periods.push(v);
        match (&mut self.levels, closed) {
            (None, Some(seed)) => self.levels = Some(seed),
            (Some((lo, hi)), closed) => {
                *lo = lo.min(v);
                *hi = hi.max(v);
                if let Some((pmin, pmax)) = closed {
                    let mid = (*lo + *hi) / 2.0;
                    if pmin < mid {
                        *lo += (pmin - *lo) / NORMALISER_SYMBOLS;
                    }
                    if pmax > mid {
                        *hi += (pmax - *hi) / NORMALISER_SYMBOLS;
                    }
                }
            }
            (None, None) => {}
        }
        self.levels.unwrap_or_else(|| self.periods.seen())
    }
}

/// The demodulator over one run of contiguous input.
struct Demod {
    rate_hz: u64,
    fir: StreamingFir,
    levels: Levels,
}

impl Demod {
    fn new(p: &OokDemodParams, rate_hz: u64) -> Result<Demod, StageError> {
        sps_check(OOK_DEMOD_KIND, rate_hz, p.symbol_rate_baud).map_err(StageError::Failed)?;
        let cutoff = DISCRIMINATOR_LOWPASS_FACTOR * p.symbol_rate_baud as f64;
        let (fir, warmup) = decision_lowpass(cutoff, rate_hz, p.symbol_rate_baud);
        Ok(Demod {
            rate_hz,
            fir,
            levels: Levels {
                periods: Periods::new(rate_hz, p.symbol_rate_baud, warmup),
                levels: None,
            },
        })
    }

    fn process(&mut self, x: &[Complex<f32>]) -> Vec<f32> {
        let env: Vec<f32> = x.iter().map(|s| s.norm()).collect();
        self.fir
            .process(&env)
            .into_iter()
            .map(|v| {
                let v = v as f64;
                let (lo, hi) = self.levels.of(v);
                ((v - lo) / (hi - lo).max(MIN_SCALE)) as f32
            })
            .collect()
    }
}

/// Demodulates on-off keying into a 1-channel decision signal at the input rate: the
/// envelope `|x|`, low-passed at [`DISCRIMINATOR_LOWPASS_FACTOR`] × `symbol_rate_baud`,
/// scaled to 0 (off) .. 1 (on) by the running off and on levels (each tracked over
/// [`NORMALISER_SYMBOLS`] symbol periods). An input under
/// [`crate::MIN_SAMPLES_PER_SYMBOL`] samples per symbol fails the stage naming the rate
/// to resample to. A seam ([`is_seam`]), a lag or a rate change restarts the filter and
/// the levels.
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
        let mut demod: Option<Demod> = None;
        let mut prev_end: Option<u64> = None;
        run_map(OOK_DEMOD_KIND, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    demod = None;
                    prev_end = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{OOK_DEMOD_KIND} stage expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            let span = &frame.provenance.span;
            let seam = prev_end.is_some_and(|e| is_seam(e, span.start_ns.0, rate));
            prev_end = Some(span.end_ns.0);
            if seam || demod.as_ref().is_some_and(|d| d.rate_hz != rate) {
                demod = None;
            }
            let d = match &mut demod {
                Some(d) => d,
                None => demod.insert(Demod::new(&p, rate)?),
            };
            let y = d.process(&block.samples);
            if y.is_empty() {
                return Ok(Vec::new());
            }
            Ok(vec![Frame::new(
                out.clone(),
                frame.provenance.clone(),
                Payload::Audio {
                    sample_rate_hz: Hz(rate),
                    channels: 1,
                    samples: y.into(),
                },
            )])
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

    const BAUD: u32 = 2_400;
    const T0: u64 = 1_000_000_000;

    async fn demod(rate: u64, iq: Vec<Complex<f32>>) -> Result<Vec<f32>, StageError> {
        let p = Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: frame_span(Timestamp(T0), iq.len(), rate),
        };
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Audio, p.clone());
        let mut rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx) = broadcast::channel(4);
        tx.send(Frame::new(
            zeta_core::types::StreamId::new(),
            p,
            Payload::Iq(IqBlock {
                t0: Timestamp(T0),
                sample_rate_hz: Hz(rate),
                center_hz: Hz(0),
                samples: iq.into(),
            }),
        ))
        .unwrap();
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
        let mut y = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } = f.payload
            else {
                panic!("an audio frame")
            };
            assert_eq!((sample_rate_hz.0, channels), (rate, 1));
            y.extend(samples.iter().copied());
        }
        Ok(y)
    }

    #[tokio::test]
    async fn on_and_off_map_to_one_and_zero() {
        let rate = 48_000u64;
        let sps = (rate / BAUD as u64) as usize;
        // Alternating runs of 4 symbols on, 4 off, at amplitude 0.3.
        let iq: Vec<Complex<f32>> = (0..64 * sps)
            .map(|i| {
                let on = (i / sps / 4).is_multiple_of(2);
                Complex::new(if on { 0.3 } else { 0.0 }, 0.0)
            })
            .collect();
        let y = demod(rate, iq).await.unwrap();
        assert_eq!(y.len(), 64 * sps);
        // The middle of the run of each kind near the end, clear of the filter's delay.
        let mean = |k: usize| y[k * sps..(k + 1) * sps].iter().sum::<f32>() / sps as f32;
        let (on, off) = (mean(56 + 2), mean(60 + 2));
        assert!((on - 1.0).abs() < 0.1, "on reads {on}");
        assert!(off.abs() < 0.1, "off reads {off}");
    }

    #[tokio::test]
    async fn too_few_samples_per_symbol_is_refused() {
        let e = demod(7_200, vec![Complex::new(1.0, 0.0); 100])
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
