//! Build the node the CLI drives: stage registry (core plus `zeta-dsp`, `transcribe` on the
//! speech backend, plus `--plugin` stages), sources (SigMF files, KiwiSDRs, other ZetaSDR
//! nodes, VRT senders over UDP, and SoapySDR devices when built with `--cfg soapy`),
//! knowledge (the builtin signatures and the signal catalogue) and classifiers
//! (`zeta_dsp::classifiers`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zeta_core::classify::StubClassifier;
use zeta_core::knowledge::StaticKnowledge;
use zeta_core::modes::{Decoder, ModeRegistry, ModeState, ProgramBlock};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Hz, SourceId};
use zeta_ext::{ExternalDecodeStage, ModesHandle};
use zeta_plugin::host::PluginCommand;
use zeta_source_file::{FileOpts, FileSource};
use zeta_source_kiwi::{KiwiConfig, KiwiSource};
use zeta_source_node::{NodePort, NodeSourceConfig};
use zeta_source_vrt::VrtSourceConfig;
use zeta_speech::{SpeechBackend, TranscribeStage};

use crate::sources::{PluginSpec, SourceSpec};

/// `$XDG_STATE_HOME/zetasdr`, else `$HOME/.local/state/zetasdr`.
pub fn default_state_dir() -> Result<PathBuf, String> {
    let var = |k| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(x) = var("XDG_STATE_HOME") {
        return Ok(x.join("zetasdr"));
    }
    var("HOME")
        .map(|h| h.join(".local/state/zetasdr"))
        .ok_or_else(|| "neither XDG_STATE_HOME nor HOME is set; pass --state-dir".into())
}

/// `modes-checked.json` in `state_dir`: when each program row last passed `zeta modes check`.
pub fn checked_path(state_dir: &Path) -> PathBuf {
    state_dir.join("modes-checked.json")
}

/// Longest a `transcribe` stage waits for the speech backend to answer one utterance.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The stages the CLI node runs. `external_decode` and its `adsb_decode` alias run the
/// mode rows of the registry `modes` will hold (set once the rows are loaded against this
/// registry: see [`open`]), their fifos under `run_dir` (`<state_dir>/run`; `None`, as
/// for a schema listing, fails a fifo row). `transcribe` runs on `speech`; without one it is still
/// registered and its jobs fail saying the node has no speech backend.
pub fn registry(
    speech: Option<Arc<dyn SpeechBackend>>,
    modes: &ModesHandle,
    run_dir: Option<&Path>,
) -> StageRegistry {
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    registry.register(Arc::new(StreamInStage::new()));
    zeta_dsp::register_all(&mut registry);
    registry.register(Arc::new(TranscribeStage::new(speech, TRANSCRIBE_TIMEOUT)));
    let mut external = ExternalDecodeStage::deferred(modes.clone());
    let mut adsb = zeta_ext::AdsbDecodeStage::deferred(modes.clone());
    if let Some(dir) = run_dir {
        external = external.with_run_dir(dir);
        adsb = adsb.with_run_dir(dir);
    }
    registry.register(Arc::new(external));
    registry.register(Arc::new(adsb));
    registry
}

/// Find the program of every program row, one probe per distinct `programs` list (rows
/// in id order), and record it in `modes` (`set_program`): found, the rows are ready
/// with it; missing, they are unavailable with the report line as their reason. The
/// lookup honours `ZETA_HANDLER_<PROGRAM>` (and `ZETA_DUMP1090` for dump1090), an
/// override naming no file being missing. Returns the handler report, one line per
/// list (spec 5.3): `zeta: handlers: <programs[0]>: found <path> (<version>)` or
/// `zeta: handlers: <programs[0]>: missing (<hint>; <url>) — modes: <ids>`, the hint
/// `apt install <debian>`, else `dnf install <fedora>`, else `brew install <brew>`,
/// left out (with its `; `) when the row names no package.
pub async fn probe_handlers(modes: &mut ModeRegistry) -> Vec<String> {
    let mut groups: Vec<(ProgramBlock, Vec<String>)> = Vec::new();
    for m in modes.all().iter().filter(|m| m.state != ModeState::Broken) {
        let Decoder::Program(b) = &m.row.decoder else {
            continue;
        };
        match groups.iter_mut().find(|(g, _)| g.programs == b.programs) {
            Some((_, ids)) => ids.push(m.row.id.0.clone()),
            None => groups.push((b.clone(), vec![m.row.id.0.clone()])),
        }
    }
    let mut lines = Vec::new();
    for (block, ids) in groups {
        let first = block.programs.first().cloned().unwrap_or_default();
        match zeta_ext::proc::find_handler(&block.programs) {
            Some(path) => {
                let version = zeta_ext::proc::handler_version(&path).await;
                let version = if version.is_empty() {
                    "version unknown".to_string()
                } else {
                    version
                };
                lines.push(format!(
                    "zeta: handlers: {first}: found {} ({version})",
                    path.display()
                ));
                for id in &ids {
                    modes.set_program(id, Some(path.clone()), None);
                }
            }
            None => {
                let i = &block.install;
                let hint = [("apt", &i.debian), ("dnf", &i.fedora), ("brew", &i.brew)]
                    .into_iter()
                    .find_map(|(tool, pkg)| pkg.as_ref().map(|p| format!("{tool} install {p}; ")))
                    .unwrap_or_default();
                let reason = format!(
                    "{first}: missing ({hint}{}) — modes: {}",
                    i.url,
                    ids.join(", ")
                );
                lines.push(format!("zeta: handlers: {reason}"));
                for id in &ids {
                    modes.set_program(id, None, Some(reason.clone()));
                }
            }
        }
    }
    lines
}

/// Why a `--source-soapy` or `zeta soapy find` cannot run in this build.
#[cfg(not(soapy))]
pub const NO_SOAPY: &str = "zeta was built without soapy: rebuild with RUSTFLAGS=\"--cfg soapy\" after installing libsoapysdr-dev";

/// `zeta soapy find`: print the attached SoapySDR devices matching `filter` as JSON.
pub fn soapy_find(filter: &str) -> std::process::ExitCode {
    #[cfg(soapy)]
    {
        match zeta_source_soapy_driver::find(filter) {
            Ok(found) => match serde_json::to_string_pretty(&found) {
                Ok(s) => {
                    println!("{s}");
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("zeta: serialising output: {e}");
                    std::process::ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("zeta: soapy find {filter:?}: {e}");
                std::process::ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(soapy))]
    {
        let _ = filter;
        eprintln!("zeta: soapy find: {NO_SOAPY}");
        std::process::ExitCode::FAILURE
    }
}

/// Open the SoapySDR device `args` as source `id`.
#[cfg(soapy)]
fn open_soapy(id: &str, args: &str) -> Result<Arc<dyn Port>, String> {
    zeta_source_soapy_driver::open(SourceId(id.to_owned()), args)
        .map(|s| Arc::new(s) as Arc<dyn Port>)
        .map_err(|e| e.to_string())
}

#[cfg(not(soapy))]
fn open_soapy(_id: &str, _args: &str) -> Result<Arc<dyn Port>, String> {
    Err(NO_SOAPY.into())
}

/// Probe each plugin and register it after the built-in stages. A kind that a built-in
/// stage already has, or a failed probe, is an error naming the flag.
async fn with_plugins(
    plugins: &[PluginSpec],
    speech: Option<Arc<dyn SpeechBackend>>,
    modes: &ModesHandle,
    run_dir: &Path,
) -> Result<StageRegistry, String> {
    let mut reg = registry(speech, modes, Some(run_dir));
    for p in plugins {
        if reg.spec(&p.kind).is_some() {
            return Err(format!(
                "{p}: plugin kind {:?} collides with a built-in stage",
                p.kind
            ));
        }
        let cmd = PluginCommand {
            program: p.program.clone(),
            args: vec![],
        };
        let stage = zeta_plugin::host::probe(&p.kind, cmd)
            .await
            .map_err(|e| format!("{p}: {e}"))?;
        reg.register(Arc::new(stage));
    }
    Ok(reg)
}

/// The `--source-node` ports of an opened node, kept to stop their remote jobs on exit.
#[derive(Default)]
pub struct RemotePorts(Vec<Arc<NodePort>>);

impl RemotePorts {
    /// Stop every remote `passthrough` job and wait for each node to confirm. Call before
    /// the process exits: the stop a dropped port spawns never runs once the runtime is
    /// shutting down.
    pub async fn close(&self) {
        for p in &self.0 {
            p.close().await;
        }
    }
}

/// Default ring budget: a quarter of `MemTotal` capped at 4 GiB, 1 GiB when
/// `/proc/meminfo` cannot be read.
fn default_ring_budget() -> u64 {
    const GIB: u64 = 1 << 30;
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| parse_meminfo(&s))
        .unwrap_or(GIB)
}

/// A quarter of the `MemTotal` line (kB) of `/proc/meminfo`, capped at 4 GiB.
fn parse_meminfo(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some((kb.saturating_mul(1024) / 4).min(4 << 30))
}

/// Open the node on `state_dir` with the given sources and plugin stages. File sources
/// loop in real time and start after the node has subscribed to them (a source with no
/// subscriber stops at once); a KiwiSDR connects on its first tune; a remote node is asked
/// for its source list here (with the secret from `ZETA_NODE_SECRET_<ID>`) and runs a
/// `passthrough` job from each tune; a VRT source binds its UDP address here; a SoapySDR
/// device is opened here. `resume` relaunches persisted `Running` jobs (`serve`); without it they
/// are listed as persisted and left alone (one-shot). `speech` is the backend `transcribe`
/// jobs run on. `modes_dir` is the operator's mode rows (`--modes-dir`; `None` for
/// [`zeta_core::modes::default_operator_dir`]); each broken row is reported on stderr, one
/// line each, then the handler report of the program rows ([`probe_handlers`]); neither
/// fails the node. Errors name the flag, never a Kiwi password or a node secret. The node
/// comes with its [`RemotePorts`].
pub async fn open(
    state_dir: PathBuf,
    sources: &[SourceSpec],
    plugins: &[PluginSpec],
    resume: bool,
    ring_budget_mb: Option<u64>,
    modes_dir: Option<PathBuf>,
    speech: Option<Arc<dyn SpeechBackend>>,
) -> Result<(Node, RemotePorts), String> {
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let handle = ModesHandle::default();
    let registry = with_plugins(plugins, speech, &handle, &state_dir.join("run")).await?;
    // The rows validate against every stage (`external_decode` and the plugins
    // included); program rows are probed after the plugins, and the registry is finished
    // before the stages and the node share it.
    let modes_dir = modes_dir.or_else(zeta_core::modes::default_operator_dir);
    let mut modes = ModeRegistry::load(&registry, modes_dir.as_deref());
    modes.load_checked(&checked_path(&state_dir));
    for line in modes.startup_lines() {
        eprintln!("{line}");
    }
    for line in probe_handlers(&mut modes).await {
        eprintln!("{line}");
    }
    let modes = Arc::new(modes);
    let _ = handle.set(modes.clone());
    let mut files = Vec::new();
    let mut ports: Vec<Arc<dyn Port>> = Vec::new();
    let mut remotes = RemotePorts::default();
    for spec in sources {
        let flag = |e: &dyn std::fmt::Display| format!("{spec}: {e}");
        let id = SourceId(spec.id().to_owned());
        match spec {
            SourceSpec::File {
                path, tune_range, ..
            } => {
                let opts = FileOpts {
                    tune_range: tune_range.map(|(min, max)| (Hz(min), Hz(max))),
                    ..opts
                };
                let f = FileSource::open(id, Path::new(path), opts)
                    .map(Arc::new)
                    .map_err(|e| flag(&e))?;
                ports.push(f.clone());
                files.push(f);
            }
            SourceSpec::Kiwi {
                host,
                port,
                password,
                ..
            } => {
                let cfg =
                    KiwiConfig::new(host.clone(), *port, password.clone()).map_err(|e| flag(&e))?;
                ports.push(Arc::new(KiwiSource::new(id, cfg)));
            }
            SourceSpec::Node {
                url,
                remote_source,
                max_bandwidth_hz,
                ..
            } => {
                let cfg = NodeSourceConfig {
                    id,
                    url: url.clone(),
                    secret: crate::sources::node_secret(spec.id())?,
                    remote_source: SourceId(remote_source.clone()),
                    max_bandwidth_hz: *max_bandwidth_hz,
                };
                let p = Arc::new(NodePort::open(cfg).await.map_err(|e| flag(&e))?);
                remotes.0.push(p.clone());
                ports.push(p);
            }
            SourceSpec::Vrt { bind, .. } => {
                let cfg = VrtSourceConfig { id, bind: *bind };
                let p = zeta_source_vrt::open(cfg).await.map_err(|e| flag(&e))?;
                ports.push(Arc::new(p));
            }
            SourceSpec::Soapy { args, .. } => {
                ports.push(open_soapy(spec.id(), args).map_err(|e| flag(&e))?);
            }
        }
    }
    #[allow(unused_mut)]
    let mut backends = vec![Backend::Cpu];
    #[cfg(feature = "gpu")]
    if zeta_dsp::gpu_available() {
        backends.push(Backend::Gpu);
    }
    let cfg = NodeConfig {
        state_dir,
        ring_seconds: 10.0,
        ring_budget_bytes: ring_budget_mb
            .map_or_else(default_ring_budget, |mb| mb.saturating_mul(1 << 20)),
        backends,
        resume,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        bank_factory: Some(Arc::new(zeta_dsp::bank::PfbBankFactory)),
        modes_dir,
    };
    let node = Node::open_with_modes(cfg, registry, ports, Some(modes))
        .await
        .map_err(|e| e.to_string())?;
    for s in &files {
        s.start();
    }
    node.add_knowledge(Arc::new(StaticKnowledge::builtin()));
    node.add_knowledge(Arc::new(zeta_dsp::catalogue::CatalogueKnowledge));
    node.add_classifier(Arc::new(StubClassifier));
    for c in zeta_dsp::classifiers() {
        node.add_classifier(c);
    }
    Ok((node, remotes))
}

#[cfg(test)]
mod meminfo_tests {
    use super::parse_meminfo;

    #[test]
    fn quarter_of_memtotal_capped_at_4_gib() {
        assert_eq!(
            parse_meminfo("MemTotal:  4194304 kB\nMemFree: 1 kB\n"),
            Some(1 << 30)
        );
        assert_eq!(parse_meminfo("MemTotal: 67108864 kB\n"), Some(4 << 30));
        assert_eq!(parse_meminfo("MemFree: 1 kB\n"), None);
        assert_eq!(parse_meminfo("MemTotal: lots kB\n"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeta_core::graph::validate;
    use zeta_core::ops::{AUDIO_RATE_HZ, JobKind, OpError, schema};
    use zeta_core::port::Tuning;
    use zeta_core::types::Hz;

    fn param(n: &zeta_core::graph::Node, key: &str) -> u64 {
        n.params[key].as_u64().unwrap()
    }

    #[test]
    fn every_job_kind_expands_to_a_valid_graph() {
        let schema = schema();
        // The patched enum: the built-in kinds and the embedded mode rows' ids.
        let kinds: Vec<JobKind> = schema["op"]["$defs"]["JobKind"]["enum"]
            .as_array()
            .expect("JobKind enum in the schema")
            .iter()
            .map(|v| serde_json::from_value(v.clone()).unwrap())
            .collect();
        assert!(kinds.len() >= 5, "kinds read from schema: {kinds:?}");
        for id in [
            "aprs_decode",
            "pocsag_decode",
            "rds_decode",
            "acars_decode",
            "adsb_decode",
        ] {
            assert!(
                kinds.iter().any(|k| k.name() == id),
                "{id} in the schema's kinds: {kinds:?}"
            );
        }
        let src = SourceId("src".into());
        let registry = registry(None, &ModesHandle::default(), None);
        let modes = zeta_core::modes::ModeRegistry::load(&registry, None);
        for kind in kinds {
            // An attachable kind is never started on the radio: its graph reads a job.
            if kind.attachable() {
                let t = Tuning {
                    center_hz: Hz(7_100_000),
                    bandwidth_hz: Hz(10_000),
                };
                assert!(
                    matches!(kind.expand(&src, t, Hz(48_000), None, None, None, &modes),
                        Err(OpError::Other { message }) if message.ends_with("use job_attach")),
                    "{kind:?}"
                );
                let (job, stream) = (
                    zeta_core::types::JobId::new(),
                    zeta_core::types::StreamId::new(),
                );
                let g = kind.expand_attached(&job, &stream, "en").unwrap();
                validate(&g, &registry).unwrap_or_else(|e| panic!("{kind:?} attached: {e}"));
                continue;
            }
            for rate in [
                12_000u64, 48_000, 1_000_000, 2_048_000, 2_400_000, 8_000_000, 10_000_000,
                20_000_000,
            ] {
                for bw in [10_000u64, 12_500] {
                    let tuning = Tuning {
                        center_hz: Hz(7_100_000),
                        bandwidth_hz: Hz(bw),
                    };
                    match kind.expand(&src, tuning, Hz(rate), None, None, None, &modes) {
                        // dump1090 needs a band of 2 MHz or more (unless the band is
                        // already wider than the source, refused as for every kind).
                        Err(OpError::OutOfRange { field, min, .. })
                            if kind == JobKind::AdsbDecode && field == "bandwidth_hz" =>
                        {
                            assert!(
                                min == 2_000_000 || (rate == 12_000 && bw == 12_500),
                                "{rate} {bw} {min}"
                            );
                        }
                        Err(OpError::OutOfRange { field, .. }) if field == "bandwidth_hz" => {
                            assert!(rate == 12_000 && bw == 12_500, "{kind:?} {rate} {bw}");
                        }
                        // A scan runs no graph: the node steps the source itself.
                        Ok(g) if kind == JobKind::Scan => {
                            assert!(g.nodes.is_empty() && g.edges.is_empty(), "{g:?}");
                        }
                        Ok(g) => {
                            validate(&g, &registry)
                                .unwrap_or_else(|e| panic!("{kind:?} {rate} {bw}: {e}"));
                            for n in g.nodes.iter().filter(|n| n.kind == "channel") {
                                let out = param(n, "out_rate_hz");
                                assert_eq!(rate % out, 0, "{kind:?} {rate} {bw}");
                                assert!(param(n, "bandwidth_hz") <= out);
                                for e in g.edges.iter().filter(|e| e.from == n.id) {
                                    let to = g.nodes.iter().find(|m| m.id == e.to).unwrap();
                                    if to.kind == "nbfm_demod" {
                                        assert_eq!(
                                            param(to, "audio_rate_hz"),
                                            AUDIO_RATE_HZ,
                                            "{kind:?} {rate} {bw}"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => panic!("{kind:?} {rate} {bw}: {e}"),
                    }
                }
            }
        }
        // adsb_decode at its own band on the radios' rates: a channel at an exact divisor
        // of the source rate (the adaptor resamples to 2.4 MHz) into the decoder.
        for (rate, bw) in [
            (2_048_000u64, 2_000_000u64),
            (2_400_000, 2_400_000),
            (4_800_000, 2_400_000),
            (8_000_000, 2_400_000),
            (9_600_000, 2_400_000),
            (10_000_000, 2_400_000),
            (20_000_000, 2_400_000),
        ] {
            let tuning = Tuning {
                center_hz: Hz(1_090_000_000),
                bandwidth_hz: Hz(bw),
            };
            let g = JobKind::AdsbDecode
                .expand(&src, tuning, Hz(rate), None, None, None, &modes)
                .unwrap();
            let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
            assert_eq!(kinds, ["channel", "external_decode"], "adsb_decode {rate}");
            let out = param(&g.nodes[0], "out_rate_hz");
            assert_eq!(rate % out, 0, "adsb_decode {rate} {bw}: {out}");
            assert!(out >= bw, "adsb_decode {rate} {bw}: {out}");
            validate(&g, &registry).unwrap_or_else(|e| panic!("adsb_decode {rate}: {e}"));
        }
    }
}
