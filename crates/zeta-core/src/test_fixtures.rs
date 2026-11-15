//! Ports and stages for tests, behind the `test-fixtures` feature so the integration tests
//! of this crate and of the crates above it share them.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::port::{
    GainInfo, IqReceiver, LinkMeasure, Port, PortError, PortStats, SWEEP_ROWS_CAPACITY, Starting,
    SweepPlan, SweepRequest, SweepRow, SweepRows, SweepState, Tuning, WorkerInfo,
};
use crate::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use crate::stream::{Frame, Payload, StreamBus};
use crate::types::{Capabilities, Hz, SourceId, Span, StreamId, StreamKind, SweepCaps, Timestamp};

/// The gain range a [`FaultablePort`] reports, a HackRF-like 0..=76 dB.
const GAIN_RANGE_DB: (f64, f64) = (0.0, 76.0);

/// A port that is another port (a `FileSource`, typically) but for a fault the test sets
/// and clears, standing in for a device that is unplugged and found again by a worker.
///
/// While faulted, [`Port::fault`] reports it, `tune` fails with `Io(<fault>)` (a device
/// that cannot be opened) and `current` still answers the inner port's band, as a worker
/// port's cached capture does. [`FaultablePort::heal`] clears the fault and bumps the
/// counter [`Port::healed`] hands out, as a respawned worker does. It also reports a
/// plausible adjustable gain, recording every `set_gain`, and a worker, so the node's
/// recovery of a worker-like source can be tested over a recording, and a worker's start
/// the test sets ([`FaultablePort::set_starting`]), the figures its latest hello measured
/// ([`FaultablePort::set_latest`]), which it caches with their speed, and counts the
/// `remeasure` calls it answers `Ok`. The wrapped port's own `fault` is not consulted:
/// the test owns the fault. One made [`FaultablePort::sweeping`] sweeps for itself (a row
/// every 5 ms, its fault or not, until the rows are dropped); any other forwards a sweep
/// to the inner port.
///
/// It wraps `Arc<dyn Port>` rather than `zeta-source-file`'s `FileSource` because that
/// crate depends on this one; pass the file source, it coerces.
pub struct FaultablePort {
    inner: Arc<dyn Port>,
    fault: Mutex<Option<String>>,
    healed: tokio::sync::watch::Sender<u64>,
    /// Set by [`FaultablePort::heal_held`]: [`Port::heal_settled`] ends the fault.
    held: Mutex<bool>,
    gain: Mutex<Option<f64>>,
    gains: Mutex<Vec<Option<f64>>>,
    /// The worker record's `starting`, set by [`FaultablePort::set_starting`].
    starting: Mutex<Option<Starting>>,
    starting_changed: tokio::sync::watch::Sender<u64>,
    /// The worker record's `latest`, set by [`FaultablePort::set_latest`].
    latest: Mutex<Option<LinkMeasure>>,
    remeasures: Mutex<u32>,
    /// The inner port's record with a `sweep`, on a port made [`FaultablePort::sweeping`].
    caps: Option<Capabilities>,
}

impl FaultablePort {
    pub fn new(inner: Arc<dyn Port>) -> Arc<Self> {
        Self::build(inner, None)
    }

    /// [`FaultablePort::new`] over `inner`, sweeping for itself as `sweep` says.
    pub fn sweeping(inner: Arc<dyn Port>, sweep: SweepCaps) -> Arc<Self> {
        let mut caps = inner.capabilities().clone();
        caps.sweep = Some(sweep);
        Self::build(inner, Some(caps))
    }

    fn build(inner: Arc<dyn Port>, caps: Option<Capabilities>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            fault: Mutex::new(None),
            healed: tokio::sync::watch::channel(0).0,
            held: Mutex::new(false),
            gain: Mutex::new(Some(GAIN_RANGE_DB.0)),
            gains: Mutex::new(Vec::new()),
            starting: Mutex::new(None),
            starting_changed: tokio::sync::watch::channel(0).0,
            latest: Mutex::new(None),
            remeasures: Mutex::new(0),
            caps,
        })
    }

    /// Set (or with `None` clear) the fault the port reports. Clearing does not count as
    /// a heal; see [`FaultablePort::heal`].
    pub fn set_fault(&self, fault: Option<String>) {
        *self.fault.lock().unwrap() = fault;
    }

    /// Clear the fault and bump the healed counter, as a worker that found its device
    /// again does.
    pub fn heal(&self) {
        self.set_fault(None);
        self.healed.send_modify(|n| *n += 1);
    }

    /// Bump the healed counter and keep reporting the fault until [`Port::heal_settled`],
    /// as a worker back quicker than the node's polls does.
    pub fn heal_held(&self) {
        *self.held.lock().unwrap() = true;
        self.healed.send_modify(|n| *n += 1);
    }

    /// Set (or with `None` clear) what the worker record says the worker is doing at
    /// its start, and bump the counter [`Port::starting_changed`] hands out, as a
    /// worker port does at each progress report, its hello and its end.
    pub fn set_starting(&self, s: Option<Starting>) {
        *self.starting.lock().unwrap() = s;
        self.starting_changed.send_modify(|n| *n += 1);
    }

    /// Set (or with `None` clear) the figures the worker record says the latest hello
    /// measured and the speed they were measured at: what [`Port::record_to_cache`]
    /// gives, over the inner port's record, as a worker port's respawn does.
    pub fn set_latest(&self, m: Option<LinkMeasure>) {
        *self.latest.lock().unwrap() = m;
    }

    /// How many times the port was asked to measure its link again.
    pub fn remeasure_calls(&self) -> u32 {
        *self.remeasures.lock().unwrap()
    }

    /// Every `set_gain` the port was asked for, in order.
    pub fn gain_calls(&self) -> Vec<Option<f64>> {
        self.gains.lock().unwrap().clone()
    }

    fn gain_info(&self) -> GainInfo {
        let db = *self.gain.lock().unwrap();
        GainInfo {
            db,
            min_db: GAIN_RANGE_DB.0,
            max_db: GAIN_RANGE_DB.1,
            automatic: db.is_none(),
            automatic_available: true,
        }
    }
}

#[async_trait]
impl Port for FaultablePort {
    fn id(&self) -> &SourceId {
        self.inner.id()
    }
    fn capabilities(&self) -> &Capabilities {
        self.caps.as_ref().unwrap_or(self.inner.capabilities())
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let fault = self.fault.lock().unwrap().clone();
        match fault {
            Some(f) => Err(PortError::Io(f)),
            None => self.inner.tune(t).await,
        }
    }
    fn current(&self) -> Option<Tuning> {
        self.inner.current()
    }
    fn subscribe(&self) -> IqReceiver {
        self.inner.subscribe()
    }
    fn fault(&self) -> Option<String> {
        self.fault.lock().unwrap().clone()
    }
    async fn idle(&self) {
        self.inner.idle().await
    }
    fn gain(&self) -> Option<GainInfo> {
        Some(self.gain_info())
    }
    fn stats(&self) -> Option<PortStats> {
        self.inner.stats()
    }
    async fn set_gain(&self, db: Option<f64>) -> Result<GainInfo, PortError> {
        self.gains.lock().unwrap().push(db);
        if let Some(v) = db
            && !(GAIN_RANGE_DB.0..=GAIN_RANGE_DB.1).contains(&v)
        {
            return Err(PortError::Unsupported(format!(
                "gain {v} dB out of range {}..={} dB",
                GAIN_RANGE_DB.0, GAIN_RANGE_DB.1
            )));
        }
        *self.gain.lock().unwrap() = db;
        Ok(self.gain_info())
    }
    fn healed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.healed.subscribe())
    }
    fn heal_settled(&self) {
        if std::mem::take(&mut *self.held.lock().unwrap()) {
            self.set_fault(None);
        }
    }
    fn worker(&self) -> Option<WorkerInfo> {
        Some(WorkerInfo {
            pid: Some(1),
            restarts: 0,
            since_ns: None,
            transport: None,
            transport_dropped: 0,
            starting: self.starting.lock().unwrap().clone(),
            attachment: None,
            link_measure: self.latest.lock().unwrap().clone(),
        })
    }
    fn record_to_cache(&self) -> (Capabilities, Option<String>) {
        let mut caps = self.capabilities().clone();
        let Some(m) = self.latest.lock().unwrap().clone() else {
            return (caps, None);
        };
        caps.max_bandwidth_hz = m.max_bandwidth_hz;
        caps.depths = m.depths;
        (caps, m.at_speed)
    }
    async fn remeasure(&self) -> Result<(), PortError> {
        *self.remeasures.lock().unwrap() += 1;
        Ok(())
    }
    fn starting_changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.starting_changed.subscribe())
    }
    /// Made [`FaultablePort::sweeping`]: one step of 256 bins over the span, a row
    /// every 5 ms until the rows are dropped. Else the inner port's sweep.
    async fn sweep(&self, req: SweepRequest) -> Result<SweepRows, PortError> {
        if self.caps.is_none() {
            return self.inner.sweep(req).await;
        }
        const BINS: u32 = 256;
        let bin_hz = req.span.bandwidth_hz.0 as f64 / f64::from(BINS);
        let plan = SweepPlan {
            steps: 1,
            step_hz: req.span.bandwidth_hz,
            size: BINS,
            bin_hz,
            dwell_ms: req.dwell_ms,
            shift_hz: Hz(0),
        };
        let (tx, rows) = tokio::sync::mpsc::channel(SWEEP_ROWS_CAPACITY);
        let (state_tx, state) = tokio::sync::watch::channel(SweepState::Running);
        let center_hz = req.span.center_hz;
        tokio::spawn(async move {
            let _state = state_tx;
            for sweep in 0u64.. {
                tokio::select! {
                    _ = tx.closed() => return,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
                }
                let now = crate::node::now();
                let row = SweepRow {
                    sweep,
                    step: 0,
                    center_hz,
                    bin_hz,
                    db: vec![-100.0; BINS as usize],
                    masked: Vec::new(),
                    span: Span {
                        start_ns: Timestamp(now.0.saturating_sub(5_000_000)),
                        end_ns: now,
                    },
                };
                if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = tx.try_send(row) {
                    return;
                }
            }
        });
        Ok(SweepRows {
            plan,
            rows,
            state,
            control: None,
        })
    }
}

/// The stage kind of [`TwoVoiceTestStage`].
pub const TWO_VOICE_TEST_KIND: &str = "two_voice_test";

/// The constant tone [`TwoVoiceTestStage`] publishes on each of its audio streams, Hz: the
/// main stream, `slot1`, `slot2`.
pub const TWO_VOICE_TONES_HZ: [f32; 3] = [500.0, 900.0, 1300.0];

/// The audio rate [`TwoVoiceTestStage`] publishes at, Hz.
pub const TWO_VOICE_RATE_HZ: u64 = 48_000;

/// A stage with two voices, for the node's voices: outputs `[event, audio, audio, audio]`,
/// the main stream (no role) then `slot1` and `slot2`, both voices of their own
/// ([`Stage::voice_roles`]). For each IQ block it reads it publishes an event
/// `{"protocol": "two_voice_test", "audio": <its audio param>}` and, on each audio stream,
/// as long a stretch of its constant tone ([`TWO_VOICE_TONES_HZ`]) at 48 kHz, so a reader
/// can tell which stream it has. Its one param, `audio`, is an audio policy's name
/// (`first_active`, the default, `follow` or `all`); a row passes `"$audio_policy"`.
pub struct TwoVoiceTestStage {
    spec: StageSpec,
}

impl TwoVoiceTestStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: TWO_VOICE_TEST_KIND,
                inputs: vec![StreamKind::Iq],
                outputs: vec![
                    StreamKind::Event,
                    StreamKind::Audio,
                    StreamKind::Audio,
                    StreamKind::Audio,
                ],
                params_schema: schemars::json_schema!({
                    "type": "object",
                    "properties": {
                        "audio": {
                            "type": "string",
                            "enum": ["first_active", "follow", "all"],
                            "default": "first_active"
                        }
                    },
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Default for TwoVoiceTestStage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Stage for TwoVoiceTestStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn output_roles(&self, _params: &serde_json::Value) -> Vec<Option<&'static str>> {
        vec![None, None, Some("slot1"), Some("slot2")]
    }
    fn voice_roles(&self, _params: &serde_json::Value) -> Vec<&'static str> {
        vec!["slot1", "slot2"]
    }
    async fn run(
        &self,
        params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let (Some(rx), [event, audio @ ..]) = (inputs.first_mut(), outputs.as_slice()) else {
            return Err(StageError::Failed("expected 1 input and 4 outputs".into()));
        };
        let policy = params
            .get("audio")
            .cloned()
            .unwrap_or_else(|| "first_active".into());
        // Each stream's tone runs on from block to block.
        let mut n: u64 = 0;
        loop {
            let frame = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => frame,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            };
            let Payload::Iq(block) = &frame.payload else {
                continue;
            };
            let rate = block.sample_rate_hz.0.max(1);
            let len = block.samples.len() as u64 * TWO_VOICE_RATE_HZ / rate;
            bus.publish(Frame::new(
                event.clone(),
                frame.provenance.clone(),
                Payload::Event(serde_json::json!({
                    "protocol": TWO_VOICE_TEST_KIND,
                    "audio": policy,
                })),
            ));
            for (out, hz) in audio.iter().zip(TWO_VOICE_TONES_HZ) {
                let w = 2.0 * std::f64::consts::PI * f64::from(hz) / TWO_VOICE_RATE_HZ as f64;
                let samples: Vec<f32> = (n..n + len)
                    .map(|i| (0.5 * (w * i as f64).sin()) as f32)
                    .collect();
                bus.publish(Frame::new(
                    out.clone(),
                    frame.provenance.clone(),
                    Payload::Audio {
                        sample_rate_hz: Hz(TWO_VOICE_RATE_HZ),
                        channels: 1,
                        samples: Arc::from(samples),
                    },
                ));
            }
            n += len;
        }
    }
}
