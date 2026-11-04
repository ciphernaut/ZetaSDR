//! `capture_in`: the root of a replay chain. It reads a set of captures from the node's
//! [`BurstStore`] and publishes their IQ back to back on its `iq` output, one `segment`
//! event on its `event` output at the start of each.
//!
//! Each capture's blocks carry its own time, `start_ns` plus their offset, contiguous
//! within the capture, so a seam is a jump in time between two frames: downstream stages
//! detect it as the discontinuity it is. Every capture of a set must share the first's
//! sample rate and centre.
//!
//! The samples go out in blocks of [`BLOCK_MS`], paced at the rate (one block per
//! `BLOCK_MS` of wall time) with `realtime`, as fast as the bus takes them without. After
//! the last block of a pass the stage waits `loop_pause_ms`, which lets the chain drain
//! before the job ends, then starts the next pass with `loop`, else ends `Ok(())`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeta_core::bursts::{BurstStore, CaptureRecord};
use zeta_core::capture::{CaptureMeta, read_cf32};
use zeta_core::port::IqBlock;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Provenance, StreamId, StreamKind, Timestamp};

use crate::frame_span;

/// The stage kind, as graphs name it.
pub const CAPTURE_IN_KIND: &str = "capture_in";

/// `loop_pause_ms` by default: the wait after the last block of a pass.
pub const LOOP_PAUSE_MS_DEFAULT: u64 = 500;

/// The least `loop_pause_ms` of an unpaced loop (`loop: true`, `realtime: false`): with
/// no pause it would replay as fast as the bus takes blocks, without end.
pub const LOOP_PAUSE_MS_MIN_UNPACED: u64 = 100;

/// The length of one published block, and the wall time one takes with `realtime`.
pub const BLOCK_MS: u64 = 50;

fn default_true() -> bool {
    true
}

fn default_loop_pause_ms() -> u64 {
    LOOP_PAUSE_MS_DEFAULT
}

/// `capture_in` parameters.
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct CaptureInParams {
    /// The capture ids (`capture_list`) replayed, in this order.
    pub captures: Vec<String>,
    /// Replay the set again after each pass's pause, until cancelled.
    #[serde(rename = "loop", default)]
    pub replay: bool,
    /// Pace the blocks at the captures' rate; unpaced otherwise.
    #[serde(default = "default_true")]
    pub realtime: bool,
    /// The wait after the last block of a pass, in milliseconds; at least
    /// [`LOOP_PAUSE_MS_MIN_UNPACED`] for a loop that is not `realtime`.
    #[serde(default = "default_loop_pause_ms")]
    pub loop_pause_ms: u64,
}

impl CaptureInParams {
    fn check(&self) -> Result<(), String> {
        if self.captures.is_empty() {
            return Err("captures: at least one".into());
        }
        if self.replay && !self.realtime && self.loop_pause_ms < LOOP_PAUSE_MS_MIN_UNPACED {
            return Err(format!(
                "loop_pause_ms: at least {LOOP_PAUSE_MS_MIN_UNPACED} when loop is true and \
                 realtime false"
            ));
        }
        Ok(())
    }
}

/// One capture of the set, read from the store.
pub struct Loaded {
    pub record: CaptureRecord,
    pub meta: CaptureMeta,
    pub samples: Vec<Complex<f32>>,
}

/// Read the captures `ids` from `store`, in order. Every one must exist and share the
/// first's sample rate and centre.
pub fn load_set(store: &BurstStore, ids: &[String]) -> Result<Vec<Loaded>, String> {
    let mut set: Vec<Loaded> = Vec::with_capacity(ids.len());
    for id in ids {
        let record = store
            .get(id)
            .ok_or_else(|| format!("capture {id} not found"))?;
        let (meta, samples) =
            read_cf32(&record.meta_path).map_err(|e| format!("capture {id}: {e}"))?;
        if let Some(first) = set.first() {
            let (f, fid) = (&first.meta, &first.record.id);
            if meta.sample_rate_hz != f.sample_rate_hz {
                return Err(format!(
                    "capture {id}: {} Hz differs from {fid}'s {} Hz",
                    meta.sample_rate_hz.0, f.sample_rate_hz.0
                ));
            }
            if meta.center_hz != f.center_hz {
                return Err(format!(
                    "capture {id}: centre {} Hz differs from {fid}'s centre {} Hz",
                    meta.center_hz.0, f.center_hz.0
                ));
            }
        }
        set.push(Loaded {
            record,
            meta,
            samples,
        });
    }
    Ok(set)
}

/// See the module docs.
pub struct CaptureInStage {
    spec: StageSpec,
}

impl CaptureInStage {
    pub fn new() -> Self {
        Self {
            spec: StageSpec {
                kind: CAPTURE_IN_KIND,
                inputs: vec![],
                outputs: vec![StreamKind::Iq, StreamKind::Event],
                params_schema: schemars::schema_for!(CaptureInParams),
            },
        }
    }
}

impl Default for CaptureInStage {
    fn default() -> Self {
        Self::new()
    }
}

/// `out`'s provenance as registered on `bus`.
fn provenance_of(bus: &StreamBus, out: &StreamId) -> Option<Provenance> {
    bus.list()
        .into_iter()
        .find(|s| &s.id == out)
        .map(|s| s.provenance)
}

/// Wait `ms`, or end with `Cancelled` when `cancel` fires first.
async fn pause(cancel: &CancellationToken, ms: u64) -> Result<(), StageError> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(StageError::Cancelled),
        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(()),
    }
}

#[async_trait]
impl Stage for CaptureInStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }

    fn backend(&self) -> Backend {
        Backend::Cpu
    }

    fn check_params(&self, params: &serde_json::Value) -> Result<(), String> {
        let p: CaptureInParams =
            serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
        p.check()
    }

    async fn run(
        &self,
        params: serde_json::Value,
        _inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        ctx: StageContext,
    ) -> Result<(), StageError> {
        let failed = |m: String| StageError::Failed(format!("{CAPTURE_IN_KIND}: {m}"));
        let p: CaptureInParams =
            serde_json::from_value(params).map_err(|e| failed(format!("params: {e}")))?;
        p.check().map_err(failed)?;
        let [iq_out, ev_out] = outputs.as_slice() else {
            return Err(failed("expected 2 outputs".into()));
        };
        let (Some(iq_base), Some(ev_base)) =
            (provenance_of(&bus, iq_out), provenance_of(&bus, ev_out))
        else {
            return Err(failed("an output stream is not registered".into()));
        };
        let store = ctx
            .bursts
            .clone()
            .ok_or_else(|| failed("the node has no burst store".into()))?;
        let ids = p.captures.clone();
        let set = tokio::task::spawn_blocking(move || load_set(&store, &ids))
            .await
            .map_err(|e| failed(format!("load task: {e}")))?
            .map_err(failed)?;
        let mut pass: u64 = 0;
        loop {
            for (index, cap) in set.iter().enumerate() {
                let start_ns = cap.record.start_ns;
                ctx.note(serde_json::json!({"capture_in": {
                    "pass": pass,
                    "index": index,
                    "captures": set.len(),
                }}));
                let ev = serde_json::json!({
                    "protocol": "segment",
                    "index": index,
                    "capture": cap.record.id,
                    "start_ns": start_ns,
                    "t_ns": start_ns,
                    "summary": format!("segment {index}: {}", cap.record.id),
                });
                let prov = Provenance {
                    span: frame_span(Timestamp(start_ns), 0, 0),
                    ..ev_base.clone()
                };
                bus.publish(Frame::new(ev_out.clone(), prov, Payload::Event(ev)));
                let rate = cap.meta.sample_rate_hz.0;
                let block = ((rate * BLOCK_MS / 1_000) as usize).max(1);
                for (n, chunk) in cap.samples.chunks(block).enumerate() {
                    let offset_ns = ((n * block) as u128 * 1_000_000_000 / rate as u128) as u64;
                    let t0 = Timestamp(start_ns + offset_ns);
                    let prov = Provenance {
                        span: frame_span(t0, chunk.len(), rate),
                        ..iq_base.clone()
                    };
                    let iq = IqBlock {
                        t0,
                        sample_rate_hz: cap.meta.sample_rate_hz,
                        center_hz: cap.meta.center_hz,
                        samples: chunk.to_vec().into(),
                    };
                    bus.publish(Frame::new(iq_out.clone(), prov, Payload::Iq(iq)));
                    if p.realtime {
                        pause(&cancel, BLOCK_MS).await?;
                    } else if cancel.is_cancelled() {
                        return Err(StageError::Cancelled);
                    } else {
                        tokio::task::yield_now().await;
                    }
                }
            }
            pause(&cancel, p.loop_pause_ms).await?;
            if !p.replay {
                return Ok(());
            }
            pass += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use num_complex::Complex;
    use serde_json::Value;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use zeta_core::bursts::{BurstPolicy, BurstStore, NewCapture};
    use zeta_core::stage::{Stage, StageContext, StageError};
    use zeta_core::stream::{Frame, Payload, StreamBus};
    use zeta_core::types::{Hz, JobId, Provenance, SourceId, Span, StreamKind, Timestamp};

    use super::{CaptureInStage, LOOP_PAUSE_MS_DEFAULT};

    const RATE: u64 = 48_000;
    const CENTER: u64 = 144_800_000;
    const T_A: u64 = 1_000_000_000;
    const T_B: u64 = 5_000_000_000;

    /// `ms` of a unit tone 1 kHz above the centre at `rate`.
    fn tone(rate: u64, ms: u64) -> Vec<Complex<f32>> {
        let n = (rate * ms / 1_000) as usize;
        (0..n)
            .map(|i| {
                let ph = 2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / rate as f64;
                Complex::from_polar(1.0, ph as f32)
            })
            .collect()
    }

    fn write(store: &BurstStore, rate: u64, start_ns: u64, ms: u64) -> String {
        let iq = tone(rate, ms);
        let new = NewCapture {
            source: SourceId("s".into()),
            set: JobId("set".into()),
            center_hz: Hz(CENTER),
            bandwidth_hz: Hz(12_500),
            sample_rate_hz: Hz(rate),
            start_ns,
            end_ns: start_ns + ms * 1_000_000,
            features: serde_json::json!({}),
            signal: None,
            job: JobId("set".into()),
            watch: None,
            allocations: serde_json::json!([]),
        };
        store.write(new, &iq).unwrap().id
    }

    /// A store in a tempdir holding captures of `(rate, start_ns, ms)`, and their ids.
    fn store(caps: &[(u64, u64, u64)]) -> (tempfile::TempDir, Arc<BurstStore>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let store = BurstStore::open(dir.path(), BurstPolicy::default()).unwrap();
        let ids = caps
            .iter()
            .map(|&(rate, t, ms)| write(&store, rate, t, ms))
            .collect();
        (dir, store, ids)
    }

    struct Running {
        handle: tokio::task::JoinHandle<Result<(), StageError>>,
        iq: broadcast::Receiver<Frame>,
        ev: broadcast::Receiver<Frame>,
        cancel: CancellationToken,
        started: Instant,
    }

    /// Spawn the stage over `store` with `params`, its `iq` and `event` outputs subscribed.
    fn start(store: Arc<BurstStore>, params: Value) -> Running {
        let prov = Provenance {
            source: SourceId("s".into()),
            job: Some(JobId("job".into())),
            channel: None,
            span: Span {
                start_ns: Timestamp(0),
                end_ns: Timestamp(1),
            },
        };
        let bus = Arc::new(StreamBus::new(256));
        let iq_out = bus.register(StreamKind::Iq, prov.clone());
        let ev_out = bus.register(StreamKind::Event, prov);
        let iq = bus.subscribe(&iq_out).unwrap();
        let ev = bus.subscribe(&ev_out).unwrap();
        let cancel = CancellationToken::new();
        let ctx = StageContext::new(SourceId("s".into())).with_bursts(store);
        let c = cancel.clone();
        let started = Instant::now();
        let handle = tokio::spawn(async move {
            CaptureInStage::new()
                .run(params, vec![], vec![iq_out, ev_out], bus, c, ctx)
                .await
        });
        Running {
            handle,
            iq,
            ev,
            cancel,
            started,
        }
    }

    /// The next frame on `rx`, within 5 s.
    async fn next(rx: &mut broadcast::Receiver<Frame>) -> Frame {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a frame within 5 s")
            .expect("the stream is open")
    }

    fn block(f: &Frame) -> &zeta_core::port::IqBlock {
        match &f.payload {
            Payload::Iq(b) => b,
            _ => panic!("an iq frame"),
        }
    }

    fn event(f: &Frame) -> &Value {
        match &f.payload {
            Payload::Event(v) => v,
            _ => panic!("an event frame"),
        }
    }

    /// Iq blocks until `samples` samples have arrived, each with the instant it was seen.
    async fn blocks(r: &mut Running, samples: usize) -> Vec<(Frame, Instant)> {
        let mut got = Vec::new();
        let mut n = 0;
        while n < samples {
            let f = next(&mut r.iq).await;
            n += block(&f).samples.len();
            got.push((f, Instant::now()));
        }
        got
    }

    #[tokio::test]
    async fn replays_two_captures_back_to_back_with_a_segment_at_each_seam() {
        let (_dir, store, ids) = store(&[(RATE, T_A, 200), (RATE, T_B, 200)]);
        let mut r = start(
            store,
            serde_json::json!({"captures": ids, "realtime": false, "loop": false}),
        );
        let got = blocks(&mut r, 2 * 9_600).await;
        let result = r.handle.await.unwrap();
        let elapsed = r.started.elapsed();
        assert!(matches!(result, Ok(())), "{result:?}");
        assert!(elapsed >= Duration::from_millis(LOOP_PAUSE_MS_DEFAULT));

        let mut segments = Vec::new();
        while let Ok(f) = r.ev.try_recv() {
            segments.push(event(&f).clone());
        }
        assert_eq!(segments.len(), 2, "{segments:?}");
        for (i, (s, t)) in segments.iter().zip([T_A, T_B]).enumerate() {
            assert_eq!(s["protocol"], "segment");
            assert_eq!(s["index"], i);
            assert_eq!(s["capture"], ids[i]);
            assert_eq!(s["start_ns"], t);
            assert_eq!(s["t_ns"], t);
            assert_eq!(s["summary"], format!("segment {i}: {}", ids[i]));
        }

        // Nothing after the second capture's last block.
        assert!(r.iq.try_recv().is_err());
        let total: usize = got.iter().map(|(f, _)| block(f).samples.len()).sum();
        assert_eq!(total, 2 * 9_600);
        // Contiguous within each capture; the seam is a jump to the second's start.
        let mut expect = T_A;
        let mut seen_b = false;
        for (f, _) in &got {
            let b = block(f);
            assert_eq!(b.sample_rate_hz, Hz(RATE));
            assert_eq!(b.center_hz, Hz(CENTER));
            if b.t0.0 == T_B {
                assert_eq!(expect, T_A + 200_000_000, "capture 0 ends at 1.2 s");
                expect = T_B;
                seen_b = true;
            }
            assert_eq!(b.t0.0, expect);
            assert_eq!(f.provenance.span.start_ns, b.t0);
            assert_eq!(f.provenance.job, Some(JobId("job".into())));
            expect = f.provenance.span.end_ns.0;
        }
        assert!(seen_b);
        assert_eq!(expect, T_B + 200_000_000);
    }

    #[tokio::test]
    async fn realtime_paces_at_the_rate() {
        let (_dir, store, ids) = store(&[(RATE, T_A, 200), (RATE, T_B, 200)]);
        let mut r = start(
            store.clone(),
            serde_json::json!({"captures": ids, "realtime": true}),
        );
        let got = blocks(&mut r, 2 * 9_600).await;
        let last = got.last().unwrap().1 - r.started;
        assert!(last >= Duration::from_millis(350), "{last:?}");
        r.cancel.cancel();
        assert!(matches!(
            r.handle.await.unwrap(),
            Err(StageError::Cancelled)
        ));

        let mut r = start(
            store,
            serde_json::json!({"captures": ids, "realtime": false}),
        );
        let got = blocks(&mut r, 2 * 9_600).await;
        let last = got.last().unwrap().1 - r.started;
        assert!(last < Duration::from_millis(100), "{last:?}");
        r.cancel.cancel();
        assert!(matches!(
            r.handle.await.unwrap(),
            Err(StageError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn loop_replays_after_the_pause() {
        let (_dir, store, ids) = store(&[(RATE, T_A, 200), (RATE, T_B, 200)]);
        let mut r = start(
            store,
            serde_json::json!({
                "captures": ids, "realtime": false, "loop": true, "loop_pause_ms": 100
            }),
        );
        assert_eq!(event(&next(&mut r.ev).await)["index"], 0);
        let got = blocks(&mut r, 2 * 9_600).await;
        let last = got.last().unwrap().1;
        assert_eq!(event(&next(&mut r.ev).await)["index"], 1);
        let again = next(&mut r.ev).await;
        let at = Instant::now();
        assert_eq!(event(&again)["index"], 0);
        assert_eq!(event(&again)["capture"], ids[0]);
        assert!(at - last >= Duration::from_millis(90), "{:?}", at - last);
        // The second pass replays the first capture from its start.
        assert_eq!(block(&next(&mut r.iq).await).t0, Timestamp(T_A));
        r.cancel.cancel();
        assert!(matches!(
            r.handle.await.unwrap(),
            Err(StageError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn a_mixed_rate_set_is_refused_naming_the_id() {
        let (_dir, store, ids) = store(&[(RATE, T_A, 200), (96_000, T_B, 200)]);
        let r = start(
            store,
            serde_json::json!({"captures": ids, "realtime": false}),
        );
        let want = format!(
            "capture {}: 96000 Hz differs from {}'s 48000 Hz",
            ids[1], ids[0]
        );
        match r.handle.await.unwrap() {
            Err(StageError::Failed(m)) => assert!(m.contains(&want), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_capture_fails_naming_it() {
        let (_dir, store, _) = store(&[(RATE, T_A, 200)]);
        let r = start(store, serde_json::json!({"captures": ["01NOPE"]}));
        match r.handle.await.unwrap() {
            Err(StageError::Failed(m)) => {
                assert!(m.contains("capture 01NOPE not found"), "{m}");
                assert!(m.starts_with("capture_in: "), "{m}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unpaced_loop_needs_a_pause() {
        let st = CaptureInStage::new();
        assert_eq!(
            st.check_params(&serde_json::json!({
                "captures": ["x"], "loop": true, "realtime": false, "loop_pause_ms": 99
            })),
            Err("loop_pause_ms: at least 100 when loop is true and realtime false".to_string())
        );
        for ok in [
            serde_json::json!({"captures": ["x"], "loop": true, "realtime": false, "loop_pause_ms": 100}),
            serde_json::json!({"captures": ["x"], "loop": true, "realtime": true, "loop_pause_ms": 0}),
            serde_json::json!({"captures": ["x"], "loop": false, "realtime": false, "loop_pause_ms": 0}),
        ] {
            assert!(st.check_params(&ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn empty_captures_are_refused() {
        let st = CaptureInStage::new();
        assert_eq!(
            st.check_params(&serde_json::json!({"captures": []})),
            Err("captures: at least one".to_string())
        );
        assert!(
            st.check_params(&serde_json::json!({"captures": ["x"]}))
                .is_ok()
        );
        assert!(
            st.check_params(&serde_json::json!({"captures": ["x"], "bogus": 1}))
                .is_err()
        );
    }
}
