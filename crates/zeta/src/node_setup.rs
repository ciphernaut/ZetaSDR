//! Build the node the CLI drives: stage registry (core plus `zeta-dsp`, `transcribe` on the
//! speech backend, plus `--plugin` stages), sources (SigMF files, KiwiSDRs, other ZetaSDR
//! nodes, VRT senders over UDP, and SoapySDR devices when built with `--cfg soapy`),
//! knowledge (the builtin signatures and the signal catalogue) and classifiers
//! (`zeta_dsp::classifiers`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zeta_core::bursts::BurstPolicy;
use zeta_core::classify::StubClassifier;
use zeta_core::knowledge::StaticKnowledge;
use zeta_core::modes::{Decoder, ModeRegistry, ModeState, ProgramBlock};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Hz, SourceId, SourcePosition};
use zeta_ext::{ExternalDecodeStage, ModesHandle};
use zeta_plugin::host::PluginCommand;
use zeta_source_file::{FileOpts, FileSource};
use zeta_source_kiwi::{KiwiConfig, KiwiSource};
use zeta_source_node::{NodePort, NodeSourceConfig};
use zeta_source_vrt::VrtSourceConfig;
use zeta_source_worker::port::WorkerPort;
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
    registry.register(Arc::new(zeta_ext::SondehubStage::new()));
    registry
}

/// Find the program of every program row, one probe per distinct `programs` list (rows
/// in id order), and record it in `modes` (`set_program`): found, the rows are ready
/// with it; missing, they are unavailable with the report line as their reason. The
/// lookup honours `ZETA_HANDLER_<PROGRAM>` (and `ZETA_DUMP1090` for dump1090), an
/// override naming no executable file being missing. Returns the handler report, one line per
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

/// Whether `--soapy-in-process` was given: set once by `main` before any node opens.
/// A process-wide switch rather than a parameter of [`open`], because it is a diagnosis
/// aid for the whole process and every caller of [`open`] (one-shot, `serve`, `ask`)
/// must honour it alike.
static SOAPY_IN_PROCESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record `--soapy-in-process` for every node this process opens.
pub fn set_soapy_in_process(on: bool) {
    SOAPY_IN_PROCESS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// What a device absent at startup adds when no earlier open cached its capabilities:
/// without them the node cannot list the source, so it is refused rather than attached
/// faulted.
#[cfg(soapy)]
const NEVER_SEEN: &str = "never seen on this host; plug it in once";

/// Open the SoapySDR device `args` as source `id`.
///
/// By default the device runs in a `zeta soapy-worker` child of this process, spawned
/// and supervised by a [`WorkerPort`](zeta_source_worker::port::WorkerPort), so a driver
/// crash faults the source and the worker is respawned instead of the node going down.
/// A device absent at startup attaches faulted (and is polled for) when the source
/// record in `state_dir` holds the capabilities an earlier open cached; never seen, it
/// is refused. `in_process` (`--soapy-in-process`) opens the driver here instead, the
/// path before workers existed, for diagnosis. The worker port is also returned on its
/// own, to be closed before the process exits.
#[cfg(soapy)]
async fn open_soapy(
    id: &str,
    args: &str,
    in_process: bool,
    state_dir: &Path,
) -> Result<(Arc<dyn Port>, Option<Arc<WorkerPort>>), String> {
    use zeta_source_worker::port::{Timings, WorkerConfig};
    if in_process {
        return zeta_source_soapy_driver::open(SourceId(id.to_owned()), args)
            .map(|s| (Arc::new(s) as Arc<dyn Port>, None))
            .map_err(|e| e.to_string());
    }
    let program = worker_program()?;
    let source = SourceId(id.to_owned());
    let cached = zeta_core::node::load_source(&state_dir.join("sources"), &source)
        .and_then(|r| r.capabilities);
    let seen = cached.is_some();
    let cfg = WorkerConfig {
        id: source,
        program,
        args: ["soapy-worker", "--id", id, "--args", args]
            .map(String::from)
            .to_vec(),
        timings: Timings::default(),
    };
    match WorkerPort::open(cfg, cached).await {
        // A hello, or a device not found that was seen before: the supervisor has
        // logged that it is waiting for the device, and the port is faulted until then.
        Ok((port, _attach)) => Ok((port.clone() as Arc<dyn Port>, Some(port))),
        Err(e) if !seen && e.starts_with("device not found: ") => {
            Err(format!("{e} ({NEVER_SEEN})"))
        }
        Err(e) => Err(e),
    }
}

/// The program a worker is spawned from: this process's own image. On Linux that is
/// `/proc/self/exe`, which the kernel resolves to the running image at each exec, so a
/// respawn after the binary was replaced on disk (an upgrade under a running node)
/// still runs the build the node is, never a worker of another protocol. Elsewhere the
/// path the process was started from.
#[cfg(soapy)]
fn worker_program() -> Result<PathBuf, String> {
    if cfg!(target_os = "linux") {
        return Ok(PathBuf::from("/proc/self/exe"));
    }
    std::env::current_exe().map_err(|e| format!("locating zeta: {e}"))
}

/// A build without the driver refuses before spawning anything: the worker would only
/// exit 1 with the same text.
#[cfg(not(soapy))]
async fn open_soapy(
    _id: &str,
    _args: &str,
    _in_process: bool,
    _state_dir: &Path,
) -> Result<(Arc<dyn Port>, Option<Arc<WorkerPort>>), String> {
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

/// The ports of an opened node that must be closed before the process exits: the
/// `--source-node` ports, whose remote jobs are stopped, and the soapy worker ports,
/// whose children are shut down.
#[derive(Default)]
pub struct RemotePorts {
    nodes: Vec<Arc<NodePort>>,
    workers: Vec<Arc<WorkerPort>>,
}

impl RemotePorts {
    /// Stop every remote `passthrough` job and wait for each node to confirm, then shut
    /// every worker down and wait for it to be reaped (each bounded by its shutdown
    /// grace plus a second). Call before the process exits: what a dropped port starts
    /// runs on a task the runtime aborts as it shuts down, so a remote job would be left
    /// running and a worker SIGKILLed with its device mid-stream.
    pub async fn close(&self) {
        for p in &self.nodes {
            p.close().await;
        }
        for w in &self.workers {
            w.close().await;
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
/// device is opened here, in a `zeta soapy-worker` child unless `--soapy-in-process`.
/// `resume` relaunches persisted `Running` jobs (`serve`); without it they are listed as
/// persisted and left alone (one-shot). `speech` is the backend `transcribe`
/// jobs run on. `themes_dir` is the operator's themes (`--themes-dir`; `None` for
/// [`zeta_core::themes::default_operator_dir`]). `modes_dir` is the operator's mode rows (`--modes-dir`; `None` for
/// [`zeta_core::modes::default_operator_dir`]); each broken row is reported on stderr, one
/// line each, then the handler report of the program rows ([`probe_handlers`]); neither
/// fails the node. Errors name the flag, never a Kiwi password or a node secret. The node
/// comes with its [`RemotePorts`].
#[allow(clippy::too_many_arguments)]
pub async fn open(
    state_dir: PathBuf,
    sources: &[SourceSpec],
    source_positions: BTreeMap<SourceId, SourcePosition>,
    plugins: &[PluginSpec],
    resume: bool,
    ring_budget_mb: Option<u64>,
    burst_policy: BurstPolicy,
    modes_dir: Option<PathBuf>,
    themes_dir: Option<PathBuf>,
    speech: Option<Arc<dyn SpeechBackend>>,
) -> Result<(Node, RemotePorts), String> {
    let (registry, modes, modes_dir) = load_modes(&state_dir, plugins, speech, modes_dir).await?;
    let themes = load_themes(themes_dir);
    let (node, remotes) = open_with(
        state_dir,
        registry,
        modes,
        modes_dir,
        sources,
        source_positions,
        resume,
        ring_budget_mb,
        burst_policy,
    )
    .await?;
    node.set_themes(themes);
    Ok((node, remotes))
}

/// The themes (`--themes-dir`, else [`zeta_core::themes::default_operator_dir`]) over the
/// built-in ones; each broken theme is reported on stderr, one line each, and none fails
/// the node.
fn load_themes(themes_dir: Option<PathBuf>) -> Arc<zeta_core::themes::ThemeRegistry> {
    let dir = themes_dir.or_else(zeta_core::themes::default_operator_dir);
    let themes = zeta_core::themes::ThemeRegistry::load(dir.as_deref());
    for line in themes.startup_lines() {
        eprintln!("{line}");
    }
    Arc::new(themes)
}

/// The stage registry (built-ins, `transcribe` on `speech`, each probed `--plugin`) and
/// the mode rows validated against it: `load`, the checked times of `state_dir`, one
/// stderr line per broken row, the handler report ([`probe_handlers`]), and the registry
/// handle `external_decode` reads the rows through filled. Also the operator directory
/// used (`modes_dir`, else the default). Used by [`open`] and by `zeta modes check`,
/// which opens many nodes on one loading.
pub async fn load_modes(
    state_dir: &Path,
    plugins: &[PluginSpec],
    speech: Option<Arc<dyn SpeechBackend>>,
    modes_dir: Option<PathBuf>,
) -> Result<(StageRegistry, Arc<ModeRegistry>, Option<PathBuf>), String> {
    let handle = ModesHandle::default();
    let registry = with_plugins(plugins, speech, &handle, &state_dir.join("run")).await?;
    // The rows validate against every stage (`external_decode` and the plugins
    // included); program rows are probed after the plugins, and the registry is finished
    // before the stages and the node share it.
    let modes_dir = modes_dir.or_else(zeta_core::modes::default_operator_dir);
    let mut modes = ModeRegistry::load(&registry, modes_dir.as_deref());
    modes.load_checked(&checked_path(state_dir));
    for line in modes.startup_lines() {
        eprintln!("{line}");
    }
    for line in probe_handlers(&mut modes).await {
        eprintln!("{line}");
    }
    let modes = Arc::new(modes);
    let _ = handle.set(modes.clone());
    Ok((registry, modes, modes_dir))
}

/// [`open`] on rows already loaded by [`load_modes`] (their registry and the operator
/// directory it used). A node that fails to open closes the ports it had opened (see
/// [`RemotePorts::close`]) before the error is returned.
#[allow(clippy::too_many_arguments)]
pub async fn open_with(
    state_dir: PathBuf,
    registry: StageRegistry,
    modes: Arc<ModeRegistry>,
    modes_dir: Option<PathBuf>,
    sources: &[SourceSpec],
    source_positions: BTreeMap<SourceId, SourcePosition>,
    resume: bool,
    ring_budget_mb: Option<u64>,
    burst_policy: BurstPolicy,
) -> Result<(Node, RemotePorts), String> {
    let mut remotes = RemotePorts::default();
    let opened = open_into(
        state_dir,
        registry,
        modes,
        modes_dir,
        sources,
        source_positions,
        resume,
        ring_budget_mb,
        burst_policy,
        &mut remotes,
    )
    .await;
    match opened {
        Ok(node) => Ok((node, remotes)),
        Err(e) => {
            remotes.close().await;
            Err(e)
        }
    }
}

/// The body of [`open_with`]: each port that must be closed is added to `remotes` as
/// it opens, so a failure later on still closes it.
#[allow(clippy::too_many_arguments)]
async fn open_into(
    state_dir: PathBuf,
    registry: StageRegistry,
    modes: Arc<ModeRegistry>,
    modes_dir: Option<PathBuf>,
    sources: &[SourceSpec],
    source_positions: BTreeMap<SourceId, SourcePosition>,
    resume: bool,
    ring_budget_mb: Option<u64>,
    burst_policy: BurstPolicy,
    remotes: &mut RemotePorts,
) -> Result<Node, String> {
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let mut files = Vec::new();
    let mut ports: Vec<Arc<dyn Port>> = Vec::new();
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
                remotes.nodes.push(p.clone());
                ports.push(p);
            }
            SourceSpec::Vrt { bind, .. } => {
                let cfg = VrtSourceConfig { id, bind: *bind };
                let p = zeta_source_vrt::open(cfg).await.map_err(|e| flag(&e))?;
                ports.push(Arc::new(p));
            }
            SourceSpec::Soapy { args, .. } => {
                let in_process = SOAPY_IN_PROCESS.load(std::sync::atomic::Ordering::Relaxed);
                let (p, worker) = open_soapy(spec.id(), args, in_process, &state_dir)
                    .await
                    .map_err(|e| flag(&e))?;
                remotes.workers.extend(worker);
                ports.push(p);
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
        source_positions,
        burst_policy,
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
    node.add_knowledge(Arc::new(zeta_dsp::catalogue::CatalogueKnowledge::new(
        node.modes(),
    )));
    node.add_classifier(Arc::new(StubClassifier));
    for c in zeta_dsp::classifiers(node.modes()) {
        node.add_classifier(c);
    }
    Ok(node)
}

/// Attach the ACMA register service (`--knowledge-acma`) as a knowledge provider. The
/// service is probed once (the client's 5 s timeout); an unreachable one is logged and
/// the provider is attached anyway, as it logs its own failures. The provider reads the
/// station from `state_dir` on every lookup, so a `station set` takes effect at once.
pub async fn attach_acma(node: &Node, state_dir: &Path, flag: Option<&(String, Option<f64>)>) {
    let Some((url, reach_km)) = flag else {
        return;
    };
    let client = match zeta_ext::mcp_client::McpClient::new(url, Duration::from_secs(5)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("zeta: knowledge acma: {url} unreachable: {e}");
            return;
        }
    };
    if let Err(e) = client.health().await {
        eprintln!("zeta: knowledge acma: {url} unreachable: {e}");
    }
    let dir = state_dir.to_path_buf();
    let station: zeta_ext::acma::StationFn = Arc::new(move || zeta_core::station::load(&dir).0);
    node.add_knowledge(Arc::new(zeta_ext::acma::AcmaKnowledge::new(
        client, *reach_km, station,
    )));
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
            let t = Tuning {
                center_hz: Hz(7_100_000),
                bandwidth_hz: Hz(10_000),
            };
            // A mix has no band: its graph comes from `mix_graph`.
            if kind == JobKind::Mix {
                assert!(
                    matches!(kind.expand(&src, t, Hz(48_000), None, None, None, &modes),
                        Err(OpError::Other { message }) if message.ends_with("use mix_start")),
                    "{kind:?}"
                );
                validate(&zeta_core::ops::mix_graph(None), &registry)
                    .unwrap_or_else(|e| panic!("{kind:?}: {e}"));
                continue;
            }
            // An attachable kind is never started on the radio: its graph reads a job.
            if kind.attachable() {
                assert!(
                    matches!(kind.expand(&src, t, Hz(48_000), None, None, None, &modes),
                        Err(OpError::Other { message }) if message.ends_with("use job_attach")),
                    "{kind:?}"
                );
                let (job, stream) = (
                    zeta_core::types::JobId::new(),
                    zeta_core::types::StreamId::new(),
                );
                let g = kind.expand_attached(&job, &stream, "en", false).unwrap();
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
                        // A band wider than the source is refused, for every kind (the
                        // 12.5 kHz band at a 12 kHz source); and an IQ program row refuses a
                        // band narrower than its own least (`band.min_hz`, else
                        // `band.bandwidth_hz`: VDL2's 25 kHz), at any rate.
                        Err(OpError::OutOfRange { field, min, .. }) if field == "bandwidth_hz" => {
                            let row_least = match &kind {
                                JobKind::Mode(id) => modes
                                    .get(&id.0)
                                    .map(|m| m.row.band.min_hz.unwrap_or(m.row.band.bandwidth_hz)),
                                _ => None,
                            };
                            assert!(
                                (rate == 12_000 && bw == 12_500)
                                    || row_least.is_some_and(|least| bw < least && min == least),
                                "{kind:?} {rate} {bw} {min}"
                            );
                        }
                        // An identify runs no graph: the node drives the source and its
                        // child jobs itself.
                        Ok(g) if kind == JobKind::Identify => {
                            assert!(g.nodes.is_empty() && g.edges.is_empty(), "{g:?}");
                        }
                        // A scan expands to one bare `fft` node; the node writes the mask
                        // (`notch_hz`, `edge_mask`) into it at launch.
                        Ok(g) if kind == JobKind::Scan => {
                            validate(&g, &registry)
                                .unwrap_or_else(|e| panic!("{kind:?} {rate} {bw}: {e}"));
                            assert!(g.edges.is_empty(), "{g:?}");
                            assert_eq!(g.nodes.len(), 1, "{g:?}");
                            let n = &g.nodes[0];
                            assert_eq!(n.kind, "fft", "{g:?}");
                            for k in ["size", "window", "rows_per_s"] {
                                assert!(n.params.get(k).is_some(), "{k} in {g:?}");
                            }
                            for k in ["notch_hz", "edge_mask"] {
                                assert!(n.params.get(k).is_none(), "{k} in {g:?}");
                            }
                        }
                        // A watch: `fft → detect` over the capture, the fft at about one
                        // bin per WATCH_BIN_TARGET_HZ (1500 Hz) of it.
                        Ok(g) if kind == JobKind::Watch => {
                            validate(&g, &registry)
                                .unwrap_or_else(|e| panic!("{kind:?} {rate} {bw}: {e}"));
                            let kinds: Vec<&str> =
                                g.nodes.iter().map(|n| n.kind.as_str()).collect();
                            assert_eq!(kinds, ["fft", "detect"], "{g:?}");
                            let size = param(&g.nodes[0], "size");
                            let want = rate
                                .max(bw)
                                .div_ceil(1_500)
                                .next_power_of_two()
                                .clamp(1024, 8192);
                            assert_eq!(size, want, "{rate} {bw}");
                            assert_eq!(
                                size,
                                u64::from(zeta_core::ops::watch_fft_size(rate.max(bw)))
                            );
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
