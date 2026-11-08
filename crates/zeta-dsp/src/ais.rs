//! AIS decode stage: GMSK at 9600 baud from a channel's IQ (the FM discriminator, a DC
//! blocker, a low-pass at [`AIS_LOWPASS_FACTOR`] of the symbol rate, a [`BitSync`] bit
//! clock), NRZI decode, HDLC deframing with the CRC-16/X.25, each frame's bytes
//! bit-reversed into the payload and read by [`crate::ais_msg`] (design 5.2).

use std::sync::Arc;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{StreamId, StreamKind};

use crate::ais_msg::{AisMsg, decode, encode_sentences, hdlc_bytes_to_payload};
use crate::ax25::HdlcDeframer;
use crate::nbfm::nbfm_demod;
use crate::pocsag::BitSync;
use crate::util::{StreamingFir, event, lowpass_taps};
use crate::{Input, run_map, sps_check};

/// The stage's kind and the row's id.
pub const AIS_DECODE_KIND: &str = "ais_decode";
/// AIS's line: GMSK at 9600 baud, 2400 Hz deviation (modulation index 0.5).
pub const AIS_BAUD: u32 = 9600;
pub const AIS_DEVIATION_HZ: u64 = 2400;
/// The discriminator's low-pass cuts off at this fraction of the symbol rate.
pub const AIS_LOWPASS_FACTOR: f64 = 0.6;
/// The low-pass spans this many symbols.
pub const AIS_LOWPASS_SYMBOLS: u64 = 4;
/// The discriminator is scaled for this many times [`AIS_DEVIATION_HZ`], so a carrier
/// off the channel's centre plus the deviation, and the noise, are not clipped (measured
/// without the DC blocker, 4 dB and 500 Hz off: 26 of 40 frames at 1x, 40 of 40 at 2x).
pub const AIS_DISCRIMINATOR_HEADROOM: u64 = 2;
/// A one-pole DC blocker at this corner after the discriminator takes out a carrier's
/// offset within a frame's training; [`BitSync`]'s own 1 Hz high-pass is far too slow for
/// a 26 ms burst (measured with the 2x headroom, 1000 Hz off at 10 dB: 25 of 40 frames
/// without it, 40 of 40 with it; at 100 Hz the six-bit runs the bit stuffing allows start
/// to droop, 36 of 40 on the centre).
pub const AIS_DC_BLOCK_HZ: f64 = 50.0;
/// Channel A (87B) and channel B (88B).
pub const AIS_CHANNEL_A_HZ: u64 = 161_975_000;
pub const AIS_CHANNEL_B_HZ: u64 = 162_025_000;
/// A job's band centred within this of a channel is on that channel.
pub const AIS_CHANNEL_MATCH_HZ: u64 = 12_500;

/// A streaming AIS receiver over one channel's IQ; state carries across
/// [`feed`](Self::feed) calls.
pub struct AisDecoder {
    rate_hz: u64,
    prev: Complex<f32>,
    lowpass: StreamingFir,
    sync: BitSync,
    /// The last line level, for NRZI.
    level: u8,
    deframer: HdlcDeframer,
    /// The DC blocker's coefficient and state (previous input and output).
    dc_a: f32,
    dc_x: f32,
    dc_y: f32,
}

impl AisDecoder {
    /// A receiver for IQ at `rate_hz`; refused under
    /// [`crate::MIN_SAMPLES_PER_SYMBOL`] samples per symbol, naming the rate.
    pub fn new(rate_hz: u64) -> Result<Self, String> {
        sps_check(AIS_DECODE_KIND, rate_hz, AIS_BAUD)?;
        let taps = (AIS_LOWPASS_SYMBOLS * rate_hz / u64::from(AIS_BAUD)) as usize | 1;
        let cutoff = AIS_LOWPASS_FACTOR * f64::from(AIS_BAUD);
        Ok(Self {
            rate_hz,
            prev: Complex::new(0.0, 0.0),
            lowpass: StreamingFir::new(lowpass_taps(cutoff, rate_hz as f64, taps)),
            sync: BitSync::new(rate_hz, AIS_BAUD),
            level: 0,
            deframer: HdlcDeframer::new(),
            dc_a: (-2.0 * std::f64::consts::PI * AIS_DC_BLOCK_HZ / rate_hz as f64).exp() as f32,
            dc_x: 0.0,
            dc_y: 0.0,
        })
    }

    /// The payloads (bits, most significant first) of the CRC-clean frames completed by
    /// `iq`.
    pub fn feed_payloads(&mut self, iq: &[Complex<f32>]) -> Vec<Vec<u8>> {
        let mut audio = nbfm_demod(
            iq,
            self.rate_hz,
            AIS_DEVIATION_HZ * AIS_DISCRIMINATOR_HEADROOM,
            &mut self.prev,
        );
        for x in audio.iter_mut() {
            let y = self.dc_a * (self.dc_y + *x - self.dc_x);
            self.dc_x = *x;
            self.dc_y = y;
            *x = y;
        }
        let smooth = self.lowpass.process(&audio);
        let line = self.sync.feed(&smooth);
        // NRZI: no change of level is a 1, a change a 0.
        let bits: Vec<u8> = line
            .iter()
            .map(|&b| {
                let d = u8::from(b == self.level);
                self.level = b;
                d
            })
            .collect();
        self.deframer
            .feed(&bits)
            .iter()
            .map(|f| hdlc_bytes_to_payload(f))
            .collect()
    }

    /// The messages of the CRC-clean frames completed by `iq`.
    pub fn feed(&mut self, iq: &[Complex<f32>]) -> Vec<AisMsg> {
        self.feed_payloads(iq)
            .iter()
            .filter_map(|p| decode(p))
            .collect()
    }
}

/// Every AIS message in `iq` at `rate_hz` (none under the decoder's lowest rate).
pub fn decode_ais_iq(iq: &[Complex<f32>], rate_hz: u64) -> Vec<AisMsg> {
    AisDecoder::new(rate_hz)
        .map(|mut d| d.feed(iq))
        .unwrap_or_default()
}

/// The AIS channel a job's band `[lo, hi]` is centred on: `'A'`, `'B'` or none.
pub fn channel_of(band: Option<[u64; 2]>) -> Option<char> {
    let [lo, hi] = band?;
    let centre = lo + (hi - lo) / 2;
    [(AIS_CHANNEL_A_HZ, 'A'), (AIS_CHANNEL_B_HZ, 'B')]
        .into_iter()
        .find(|(hz, _)| hz.abs_diff(centre) < AIS_CHANNEL_MATCH_HZ)
        .map(|(_, c)| c)
}

/// `ais_decode` takes no parameters.
#[derive(Default, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct AisParams {}

/// Decodes AIS from a channel's IQ into one Event per CRC-clean message.
pub struct AisDecodeStage {
    spec: StageSpec,
}

impl AisDecodeStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: AIS_DECODE_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![StreamKind::Event],
                params_schema: schemars::schema_for!(AisParams),
            },
        }
    }
}

impl Default for AisDecodeStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for AisDecodeStage {
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
        let _: AisParams = serde_json::from_value(params)
            .map_err(|e| StageError::Failed(format!("{AIS_DECODE_KIND} params: {e}")))?;
        let Some(out) = outputs.first().cloned() else {
            return Err(StageError::Failed("expected 1 output".into()));
        };
        let channel = channel_of(ctx.band);
        let mut dec: Option<AisDecoder> = None;
        run_map(self.spec().kind, inputs, bus, cancel, move |input| {
            let frame = match input {
                Input::Frame(f) => f,
                Input::Lagged { .. } => {
                    dec = None;
                    return Ok(Vec::new());
                }
            };
            let Payload::Iq(block) = &frame.payload else {
                return Err(StageError::Failed(format!(
                    "{AIS_DECODE_KIND} expects iq frames"
                )));
            };
            let rate = block.sample_rate_hz.0;
            let d = match &mut dec {
                Some(d) if d.rate_hz != rate => {
                    return Err(StageError::Failed(format!(
                        "sample_rate_hz changed from {} to {rate}",
                        d.rate_hz
                    )));
                }
                Some(d) => d,
                None => dec.insert(AisDecoder::new(rate).map_err(StageError::Failed)?),
            };
            let mut events = Vec::new();
            for bits in d.feed_payloads(&block.samples) {
                let Some(m) = decode(&bits) else {
                    continue;
                };
                let mut fields = m.fields();
                fields.insert(
                    "channel".into(),
                    serde_json::json!(channel.map(String::from)),
                );
                fields.insert(
                    "nmea".into(),
                    serde_json::json!(encode_sentences(&bits, channel)),
                );
                events.push(Frame::new(
                    out.clone(),
                    frame.provenance.clone(),
                    Payload::Event(event(
                        &ctx,
                        frame.provenance.span.start_ns.0,
                        "ais",
                        serde_json::Value::Object(fields),
                    )),
                ));
            }
            Ok(events)
        })
        .await
    }
}

#[cfg(all(test, feature = "test-fixtures"))]
mod tests {
    use zeta_core::port::IqBlock;
    use zeta_core::types::{Hz, Provenance, SourceId, Span, Timestamp};

    use super::*;
    use crate::synth::{awgn, parse_params, synthesise};

    const RATE: u64 = 48_000;
    /// Channel A's band, as a rigid job on a radio has it.
    const BAND_A: [u64; 2] = [161_962_500, 161_987_500];

    fn prov() -> Provenance {
        Provenance {
            source: SourceId("s".into()),
            job: None,
            channel: None,
            span: Span {
                start_ns: Timestamp(5),
                end_ns: Timestamp(9),
            },
        }
    }

    fn iq_frame(samples: &[Complex<f32>], rate_hz: u64) -> Frame {
        Frame::new(
            StreamId::new(),
            prov(),
            Payload::Iq(IqBlock {
                t0: Timestamp(0),
                sample_rate_hz: Hz(rate_hz),
                center_hz: Hz(AIS_CHANNEL_A_HZ),
                samples: samples.to_vec().into(),
            }),
        )
    }

    /// The `ais` family's capture of the default vessel with `extra` merged over it (the
    /// impairments among them) at [`RATE`].
    fn ais_capture(extra: serde_json::Value) -> Vec<Complex<f32>> {
        let mut p = serde_json::json!({
            "family": "ais", "mmsi": 503000001, "lat": -27.3842, "lon": 153.1175,
            "sog_kt": 12.3, "cog_deg": 45.0, "name": null, "repeat": 1
        });
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        let (spec, imp) = parse_params(p.as_object().unwrap().clone()).unwrap();
        synthesise(&spec, &imp, RATE as u32, AIS_CHANNEL_A_HZ)
    }

    type Run = tokio::task::JoinHandle<Result<(), StageError>>;

    fn mode() -> Arc<zeta_core::modes::ModeRow> {
        let mut stages = zeta_core::stage::StageRegistry::new();
        crate::register_all(&mut stages);
        let loaded = zeta_core::modes::ModeRegistry::load(&stages, None)
            .get(AIS_DECODE_KIND)
            .expect("row")
            .clone();
        Arc::new(loaded.row)
    }

    fn spawn(
        band: Option<[u64; 2]>,
    ) -> (broadcast::Sender<Frame>, broadcast::Receiver<Frame>, Run) {
        let bus = Arc::new(StreamBus::new(64));
        let out = bus.register(StreamKind::Event, prov());
        let rx_out = bus.subscribe(&out).unwrap();
        let (tx, rx_in) = broadcast::channel(256);
        let h = tokio::spawn(async move {
            AisDecodeStage::new()
                .run(
                    serde_json::json!({}),
                    vec![rx_in],
                    vec![out],
                    bus,
                    CancellationToken::new(),
                    StageContext::new(SourceId("s".into()))
                        .with_mode(mode())
                        .with_band(band),
                )
                .await
        });
        (tx, rx_out, h)
    }

    /// Every event the stage emits for `iq`, fed in 4 800-sample blocks.
    async fn events_for(iq: &[Complex<f32>], band: Option<[u64; 2]>) -> Vec<serde_json::Value> {
        let (tx, mut rx_out, h) = spawn(band);
        for chunk in iq.chunks(4_800) {
            tx.send(iq_frame(chunk, RATE)).unwrap();
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut out = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("expected an event")
            };
            out.push(v);
        }
        out
    }

    #[tokio::test]
    async fn the_synthesised_frame_gives_an_event() {
        let iq = ais_capture(serde_json::json!({}));
        let evs = events_for(&iq, Some(BAND_A)).await;
        assert_eq!(evs.len(), 1, "{evs:?}");
        let v = &evs[0];
        assert_eq!(v["protocol"], "ais", "{v}");
        assert_eq!(v["mmsi"], 503_000_001, "{v}");
        assert_eq!(v["msg_type"], 1, "{v}");
        assert_eq!(v["channel"], "A", "{v}");
        let nmea = v["nmea"].as_array().expect("nmea array");
        assert_eq!(nmea.len(), 1, "{v}");
        assert!(
            nmea[0].as_str().unwrap().starts_with("!AIVDM,1,1,,A,"),
            "{v}"
        );
        assert_eq!(v["summary"], "503000001 -27.3842,153.1175 12.3", "{v}");
        assert!(
            (v["position"]["lat"].as_f64().unwrap() + 27.3842).abs() <= 0.0001,
            "{v}"
        );
    }

    #[tokio::test]
    async fn a_type_5_gives_the_name() {
        let iq = ais_capture(serde_json::json!({"name": "ZETA TEST"}));
        let evs = events_for(&iq, Some(BAND_A)).await;
        assert_eq!(evs.len(), 2, "{evs:?}");
        let v = &evs[1];
        assert_eq!(v["msg_type"], 5, "{v}");
        assert_eq!(v["name"], "ZETA TEST", "{v}");
        assert_eq!(v["nmea"].as_array().unwrap().len(), 2, "{v}");
        assert!(
            v["summary"].as_str().unwrap().starts_with("ZETA TEST"),
            "{v}"
        );
    }

    #[tokio::test]
    async fn the_channel_follows_the_band() {
        let iq = ais_capture(serde_json::json!({}));
        let first = |evs: &[serde_json::Value]| evs[0]["nmea"][0].as_str().unwrap().to_string();
        let b = [AIS_CHANNEL_B_HZ - 12_500, AIS_CHANNEL_B_HZ + 12_500];
        let evs = events_for(&iq, Some(b)).await;
        assert_eq!(evs[0]["channel"], "B", "{evs:?}");
        assert!(first(&evs).starts_with("!AIVDM,1,1,,B,"), "{evs:?}");
        let evs = events_for(&iq, None).await;
        assert_eq!(evs[0]["channel"], serde_json::Value::Null, "{evs:?}");
        assert!(first(&evs).starts_with("!AIVDM,1,1,,,"), "{evs:?}");
        let elsewhere = [156_787_500, 156_812_500];
        let evs = events_for(&iq, Some(elsewhere)).await;
        assert_eq!(evs[0]["channel"], serde_json::Value::Null, "{evs:?}");
    }

    #[test]
    fn a_low_rate_is_refused_naming_it() {
        let e = AisDecoder::new(24_000).err().expect("refused");
        assert!(e.contains("24000"), "{e}");
        assert!(AisDecoder::new(48_000).is_ok());
    }

    #[tokio::test]
    async fn a_rate_change_and_a_non_iq_input_fail_the_stage() {
        let z = vec![Complex::new(0.0, 0.0); 480];
        let (tx, _rx, h) = spawn(Some(BAND_A));
        tx.send(iq_frame(&z, 48_000)).unwrap();
        tx.send(iq_frame(&z, 96_000)).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => {
                assert_eq!(m, "sample_rate_hz changed from 48000 to 96000")
            }
            r => panic!("expected Failed, got {r:?}"),
        }
        let (tx, _rx, h) = spawn(Some(BAND_A));
        tx.send(Frame::new(
            StreamId::new(),
            prov(),
            Payload::Event(serde_json::json!({})),
        ))
        .unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert_eq!(m, "ais_decode expects iq frames"),
            r => panic!("expected Failed, got {r:?}"),
        }
        let (tx, _rx, h) = spawn(Some(BAND_A));
        tx.send(iq_frame(&z, 24_000)).unwrap();
        match h.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains("24000"), "{m}"),
            r => panic!("expected Failed, got {r:?}"),
        }
    }

    #[tokio::test]
    async fn a_lag_mid_frame_discards_it() {
        let a = ais_capture(serde_json::json!({"mmsi": 503000002}));
        let b = ais_capture(serde_json::json!({}));
        // Cut frame A inside its payload: half a second of silence, then 10 ms of it.
        let cut = RATE as usize / 2 + 480;
        let (tx, mut rx_out, h) = spawn(Some(BAND_A));
        tx.send(iq_frame(&a[..cut], RATE)).unwrap();
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Capacity 256: overflow it so the stage lags, then the rest of A and all of B.
        let quiet = [Complex::new(0.0, 0.0); 48];
        for _ in 0..300 {
            tx.send(iq_frame(&quiet, RATE)).unwrap();
        }
        for chunk in a[cut..].chunks(4_800).chain(b.chunks(4_800)) {
            tx.send(iq_frame(chunk, RATE)).unwrap();
        }
        drop(tx);
        assert!(matches!(h.await.unwrap(), Ok(())));
        let mut mmsis = Vec::new();
        while let Ok(f) = rx_out.try_recv() {
            let Payload::Event(v) = f.payload else {
                panic!("expected an event")
            };
            mmsis.push(v["mmsi"].as_u64().unwrap());
        }
        assert_eq!(mmsis, [503_000_001], "the cut frame yields nothing");
    }

    #[test]
    fn decode_ais_iq_on_noise_gives_nothing() {
        let mut iq = vec![Complex::new(1.0, 0.0); RATE as usize * 2];
        awgn(&mut iq, -10.0, 7);
        assert!(decode_ais_iq(&iq, RATE).is_empty());
        let mut d = AisDecoder::new(RATE).unwrap();
        assert!(d.feed(&iq).is_empty());
    }

    #[test]
    fn decode_ais_iq_reads_every_repeat() {
        let iq = ais_capture(serde_json::json!({"name": "ZETA TEST", "repeat": 3}));
        let msgs = decode_ais_iq(&iq, RATE);
        assert_eq!(
            msgs.iter().filter(|m| m.msg_type == 1).count(),
            3,
            "{msgs:?}"
        );
        assert_eq!(
            msgs.iter().filter(|m| m.msg_type == 5).count(),
            3,
            "{msgs:?}"
        );
    }

    #[test]
    fn the_stage_survives_snr_10_db_with_offset() {
        let iq = ais_capture(serde_json::json!({"repeat": 3, "snr_db": 10.0, "offset_hz": 500.0}));
        let msgs = decode_ais_iq(&iq, RATE);
        assert!(
            msgs.iter()
                .any(|m| m.msg_type == 1 && m.mmsi == 503_000_001),
            "{msgs:?}"
        );
    }

    #[test]
    fn params_are_strict() {
        assert!(serde_json::from_value::<AisParams>(serde_json::json!({})).is_ok());
        assert!(serde_json::from_value::<AisParams>(serde_json::json!({"x": 1})).is_err());
        assert_eq!(AisDecodeStage::new().spec().kind, AIS_DECODE_KIND);
    }
}
