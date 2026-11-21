//! Fixtures the node test targets share (`mod common;`). Each target uses a subset.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::node::NodeConfig;
use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageSpec};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::types::{Hz, StreamId, StreamKind};

/// What a stand-in stage emits for one input frame: (output index, payload) pairs.
pub type MapFn = fn(&Frame) -> Vec<(usize, Payload)>;

/// A stage that maps every frame of its one input through `f`, keeping its provenance.
pub struct MapStage {
    spec: StageSpec,
    f: MapFn,
}

impl MapStage {
    pub fn new(
        kind: &'static str,
        input: StreamKind,
        outputs: &[StreamKind],
        f: MapFn,
    ) -> Arc<Self> {
        Arc::new(Self {
            spec: StageSpec {
                kind,
                inputs: vec![input],
                outputs: outputs.to_vec(),
                params_schema: schemars::json_schema!({"type": "object"}),
            },
            f,
        })
    }
}

#[async_trait]
impl Stage for MapStage {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    async fn run(
        &self,
        _params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let Some(rx) = inputs.first_mut() else {
            return Err(StageError::Failed("expected 1 input".into()));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        for (i, payload) in (self.f)(&frame) {
                            if let Some(out) = outputs.get(i) {
                                bus.publish(Frame::new(out.clone(), frame.provenance.clone(), payload));
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

pub fn audio() -> Payload {
    Payload::Audio {
        sample_rate_hz: Hz(48_000),
        channels: 1,
        samples: Arc::from(vec![0.1f32; 480]),
    }
}

pub fn same(f: &Frame) -> Vec<(usize, Payload)> {
    vec![(0, f.payload.clone())]
}

/// A node config over `dir`'s `state` directory: one second of ring, the CPU backend.
pub fn cfg(dir: &Path, resume: bool) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume,
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: None,
        handlers_dir: None,
    }
}

/// Plan 42's handler fixtures: `adsb_decode` with a handler file `alt` beside the row's
/// own `dump1090` (events only here) and no OnAir, a radio wide enough for it, and a
/// stand-in for zeta-ext's
/// `external_decode` stage (core cannot depend on it).
pub mod handlers {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;

    use zeta_core::modes::ModeRegistry;
    use zeta_core::node::{Node, NodeConfig};
    use zeta_core::ops::{JobKind, Op, OpError, OpResult};
    use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
    use zeta_core::stage::{Backend, Stage, StageContext, StageError, StageRegistry, StageSpec};
    use zeta_core::stream::{Frame, LinkType, Payload, StreamBus};
    use zeta_core::types::{
        Capabilities, Claim, Hz, JobId, Provenance, SampleFormat, SourceId, Span, StreamId,
        StreamKind, Timestamp,
    };

    pub const MODE: &str = "adsb_decode";
    /// The row's own handler: its first program.
    pub const OWN: &str = "dump1090";
    pub const ALT: &str = "alt";
    /// What probing says of alt's program when it is not found.
    pub const ALT_MISSING: &str = "zeta-alt-decoder: missing";
    pub const RADIO: &str = "radio0";

    /// Write `<dir>/handlers/alt.json`: an `adsb_decode` entry running
    /// `<dir>/bin/zeta-alt-decoder` (a stub, so the node finds it) on the row's input and
    /// output, with a pcap `packets` output
    /// when `packets`; the handlers directory.
    pub fn write_alt(dir: &Path, packets: bool) -> PathBuf {
        write_handler(dir, ALT, packets)
    }

    /// [`write_alt`] for a handler file of id `id` (its program `zeta-<id>-decoder`).
    pub fn write_handler(dir: &Path, id: &str, packets: bool) -> PathBuf {
        let (_, text) = ModeRegistry::embedded()
            .into_iter()
            .find(|(f, _)| f.ends_with("adsb_decode.json"))
            .expect("embedded adsb_decode");
        let row: serde_json::Value = serde_json::from_str(text).unwrap();
        let d = &row["decoder"];
        let mut entry = serde_json::json!({
            "args": ["--rate", "$rate_hz", "--port", "$port"],
            "input": d["input"],
            "output": d["output"],
        });
        if packets {
            entry["args"] = serde_json::json!([
                "--rate",
                "$rate_hz",
                "--port",
                "$port",
                "--pcap-port",
                "$packet_port"
            ]);
            entry["packets"] = serde_json::json!({
                "via": "tcp", "port": "$packet_port", "parser": "pcap"
            });
        }
        // The node looks the program up when it opens (never running it: the stand-in
        // stage runs instead), so it must exist for the handler to be ready.
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = bin.join(format!("zeta-{id}-decoder"));
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = serde_json::json!({
            "id": id,
            "name": "Alternative",
            "programs": [program.display().to_string()],
            "install": {"url": "https://example.invalid/alt"},
            "modes": {MODE: entry},
        });
        let handlers = dir.join("handlers");
        std::fs::create_dir_all(&handlers).unwrap();
        std::fs::write(handlers.join(format!("{id}.json")), file.to_string()).unwrap();
        // The embedded OnAir replaced by a file serving no mode: these fixtures' mode has
        // the row's own handler and the files written here, nothing else.
        let no_onair = serde_json::json!({
            "id": "onair", "name": "OnAir", "programs": ["onair-rx"],
            "install": {"url": "https://example.invalid/onair"}, "modes": {},
        });
        std::fs::write(handlers.join("onair.json"), no_onair.to_string()).unwrap();
        handlers
    }

    /// `<dir>/modes` holding `adsb_decode` as the embedded row without its `packets`
    /// block (dump1090 giving events only), so the row's own handler outputs no packets.
    pub fn write_events_only_row(dir: &Path) -> PathBuf {
        let (_, text) = ModeRegistry::embedded()
            .into_iter()
            .find(|(f, _)| f.ends_with("adsb_decode.json"))
            .expect("embedded adsb_decode");
        let mut row: serde_json::Value = serde_json::from_str(text).unwrap();
        let d = row["decoder"].as_object_mut().unwrap();
        d.remove("packets");
        for a in d["args"].as_array_mut().unwrap() {
            if a == "$packet_port" {
                *a = "0".into();
            }
        }
        let modes = dir.join("modes");
        std::fs::create_dir_all(&modes).unwrap();
        std::fs::write(modes.join(format!("{MODE}.json")), row.to_string()).unwrap();
        modes
    }

    /// [`super::cfg`] (resuming) reading the handlers directory [`write_alt`] writes and
    /// the row [`write_events_only_row`] writes.
    pub fn cfg(dir: &Path, packets: bool) -> NodeConfig {
        NodeConfig {
            handlers_dir: Some(write_alt(dir, packets)),
            modes_dir: Some(write_events_only_row(dir)),
            ..super::cfg(dir, true)
        }
    }

    /// The registry a node on `cfg` loads against `stages`, with alt's program missing
    /// (as probing finds it) when `alt_missing`.
    pub fn registry(stages: &StageRegistry, cfg: &NodeConfig, alt_missing: bool) -> ModeRegistry {
        let mut reg = ModeRegistry::load_with(
            stages,
            cfg.modes_dir.as_deref(),
            cfg.handlers_dir.as_deref(),
        );
        if alt_missing {
            reg.set_handler_program(MODE, ALT, None, Some(ALT_MISSING.into()));
        }
        reg
    }

    /// Make alt's program missing on the running `node`, as a probe would.
    pub fn alt_goes_missing(node: &Node) {
        let mut reg = (*node.modes()).clone();
        reg.set_handler_program(MODE, ALT, None, Some(ALT_MISSING.into()));
        node.modes_handle().set(Arc::new(reg));
    }

    /// What probing says of the row's own program when it is not found.
    pub const OWN_MISSING: &str = "dump1090: missing";

    /// Make the row's own program missing in `reg`, as probing finds it.
    pub fn own_missing(reg: &mut ModeRegistry) {
        reg.set_handler_program(MODE, OWN, None, Some(OWN_MISSING.into()));
    }

    /// [`alt_goes_missing`] for the row's own handler.
    pub fn own_goes_missing(node: &Node) {
        let mut reg = (*node.modes()).clone();
        own_missing(&mut reg);
        node.modes_handle().set(Arc::new(reg));
    }

    /// Stands in for `external_decode`: it emits nothing on its events and ends when
    /// cancelled. A job run by the handler `packets_for` names gets a packet output too,
    /// on which it publishes one `user0` packet every 20 ms.
    pub struct External {
        spec: StageSpec,
        packets_for: Option<&'static str>,
    }

    impl External {
        pub fn new(packets_for: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self {
                spec: StageSpec {
                    kind: "external_decode",
                    inputs: vec![StreamKind::Iq, StreamKind::Audio],
                    outputs: vec![StreamKind::Event, StreamKind::Packet],
                    params_schema: schemars::json_schema!({"type": "object"}),
                },
                packets_for,
            })
        }
    }

    #[async_trait]
    impl Stage for External {
        fn spec(&self) -> &StageSpec {
            &self.spec
        }
        fn backend(&self) -> Backend {
            Backend::Cpu
        }
        fn outputs_for(&self, params: &serde_json::Value) -> Vec<StreamKind> {
            let handler = params.get("handler").and_then(|h| h.as_str());
            match self.packets_for.is_some() && handler == self.packets_for {
                true => vec![StreamKind::Event, StreamKind::Packet],
                false => vec![StreamKind::Event],
            }
        }
        async fn run(
            &self,
            _params: serde_json::Value,
            _inputs: Vec<broadcast::Receiver<Frame>>,
            outputs: Vec<StreamId>,
            bus: Arc<StreamBus>,
            cancel: CancellationToken,
            ctx: StageContext,
        ) -> Result<(), StageError> {
            let Some(out) = outputs.get(1).cloned() else {
                cancel.cancelled().await;
                return Err(StageError::Cancelled);
            };
            for n in 1u64.. {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(StageError::Cancelled),
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                }
                let prov = Provenance {
                    source: ctx.source.clone(),
                    job: None,
                    channel: None,
                    span: Span {
                        start_ns: Timestamp(n * 1_000_000),
                        end_ns: Timestamp(n * 1_000_000),
                    },
                };
                let payload = Payload::Packet {
                    link_type: LinkType::User0,
                    data: Arc::from(vec![0x8du8, 0x40, 0x62, 0x1d]),
                };
                bus.publish(Frame::new(out.clone(), prov, payload));
            }
            Ok(())
        }
    }

    /// A stage registry with the real `packet_capture`, `stream_in` and DSP stages and
    /// [`External`] (packets for `packets_for`'s jobs).
    pub fn stages(packets_for: Option<&'static str>) -> StageRegistry {
        let mut r = StageRegistry::new();
        zeta_dsp::register_all(&mut r);
        r.register(Arc::new(zeta_core::stream_in::StreamInStage::new()));
        r.register(Arc::new(
            zeta_core::packet_capture::PacketCaptureStage::new(),
        ));
        r.register(External::new(packets_for));
        r
    }

    /// A radio that tunes to exactly the band asked for, 20 MHz wide at most, counting
    /// its tunes.
    pub struct Radio {
        id: SourceId,
        caps: Capabilities,
        cur: Mutex<Option<Tuning>>,
        tunes: AtomicUsize,
        tx: broadcast::Sender<IqBlock>,
    }

    impl Radio {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                id: SourceId(RADIO.into()),
                caps: Capabilities {
                    tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                    tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                    max_bandwidth_hz: Claim::Asserted(Hz(20_000_000)),
                    formats: vec![SampleFormat::Cf32],
                    channels: 1,
                    rx: true,
                    tx: false,
                    remote: false,
                    push: false,
                    usable_fraction: Claim::Asserted(1.0),
                    depths: vec![],
                    sweep: None,
                },
                cur: Mutex::new(None),
                tunes: AtomicUsize::new(0),
                tx: broadcast::channel(1).0,
            })
        }

        /// How many times the radio has been tuned.
        pub fn tunes(&self) -> usize {
            self.tunes.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Port for Radio {
        fn id(&self) -> &SourceId {
            &self.id
        }
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
            self.tunes.fetch_add(1, Ordering::SeqCst);
            *self.cur.lock().unwrap() = Some(t);
            Ok(t)
        }
        fn current(&self) -> Option<Tuning> {
            *self.cur.lock().unwrap()
        }
        fn subscribe(&self) -> IqReceiver {
            self.tx.subscribe()
        }
    }

    /// A `job_start` of `adsb_decode` on [`RADIO`] at 1090 MHz, 2.4 MHz wide, with
    /// `handler` and `needs`.
    pub fn start_op(handler: Option<&str>, needs: &[StreamKind]) -> Op {
        start_kind_op(
            JobKind::AdsbDecode,
            1_090_000_000,
            2_400_000,
            handler,
            needs,
        )
    }

    /// A `job_start` of `kind` on [`RADIO`] at `center_hz`, `bandwidth_hz` wide, with
    /// `handler` and `needs`.
    pub fn start_kind_op(
        kind: JobKind,
        center_hz: u64,
        bandwidth_hz: u64,
        handler: Option<&str>,
        needs: &[StreamKind],
    ) -> Op {
        let mut op = serde_json::json!({
            "op": "job_start", "kind": kind.name(), "source": RADIO,
            "center_hz": center_hz, "bandwidth_hz": bandwidth_hz,
            "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
        });
        if let Some(h) = handler {
            op["handler"] = h.into();
        }
        if !needs.is_empty() {
            op["needs"] = serde_json::to_value(needs).unwrap();
        }
        serde_json::from_value(op).expect("a job_start op")
    }

    /// The job [`start_op`] with `handler` and `needs` starts.
    pub async fn start(node: &Node, handler: Option<&str>, needs: &[StreamKind]) -> JobId {
        match node.apply(start_op(handler, needs)).await {
            Ok(OpResult::Job(id)) => id,
            r => panic!("adsb_decode start: {r:?}"),
        }
    }

    /// What [`start_op`] with `handler` and `needs` answers.
    pub async fn try_start(
        node: &Node,
        handler: Option<&str>,
        needs: &[StreamKind],
    ) -> Result<OpResult, OpError> {
        node.apply(start_op(handler, needs)).await
    }

    /// A `job_start` with `beside` naming `beside`, `handler` and `needs`, copying kind,
    /// source and band from the job's record (as the CLI and the page fill them), as JSON
    /// for a test to change.
    pub fn beside_op(
        node: &Node,
        beside: &JobId,
        handler: Option<&str>,
        needs: &[&str],
    ) -> serde_json::Value {
        let rec = node
            .job_list()
            .into_iter()
            .find(|r| &r.id == beside)
            .expect("the job beside is listed");
        serde_json::json!({
            "op": "job_start", "kind": rec.kind, "source": rec.source.0,
            "center_hz": rec.tuning.center_hz.0, "bandwidth_hz": rec.tuning.bandwidth_hz.0,
            "deviation_hz": null, "sideband": null, "elastic": false, "replace": [],
            "handler": handler, "needs": needs, "beside": beside.0,
        })
    }

    /// What [`beside_op`] answers.
    pub async fn start_beside(
        node: &Node,
        beside: &JobId,
        handler: Option<&str>,
        needs: &[&str],
    ) -> Result<OpResult, OpError> {
        let op = beside_op(node, beside, handler, needs);
        node.apply(serde_json::from_value(op).expect("a job_start op"))
            .await
    }
}

/// Poll `f` every 20 ms until it holds, panicking with `what` after `within`.
pub async fn until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
