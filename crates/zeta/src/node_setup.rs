//! Build the node the CLI drives: stage registry (core plus `zeta-dsp`, plus `--plugin`
//! stages), sources (SigMF files, KiwiSDRs, other ZetaSDR nodes, VRT senders over UDP,
//! and SoapySDR devices when built with `--cfg soapy`), knowledge and classifiers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zeta_core::classify::StubClassifier;
use zeta_core::knowledge::StaticKnowledge;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::SourceId;
use zeta_plugin::host::PluginCommand;
use zeta_source_file::{FileOpts, FileSource};
use zeta_source_kiwi::{KiwiConfig, KiwiSource};
use zeta_source_node::{NodePort, NodeSourceConfig};
use zeta_source_vrt::VrtSourceConfig;

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

/// The stages the CLI node runs.
pub fn registry() -> StageRegistry {
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut registry);
    registry
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
async fn with_plugins(plugins: &[PluginSpec]) -> Result<StageRegistry, String> {
    let mut reg = registry();
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

/// Open the node on `state_dir` with the given sources and plugin stages. File sources
/// loop in real time and start after the node has subscribed to them (a source with no
/// subscriber stops at once); a KiwiSDR connects on its first tune; a remote node is asked
/// for its source list here (with the secret from `ZETA_NODE_SECRET_<ID>`) and runs a
/// `passthrough` job from each tune; a VRT source binds its UDP address here; a SoapySDR
/// device is opened here. `resume` relaunches persisted `Running` jobs (`serve`); without it they
/// are listed as persisted and left alone (one-shot). Errors name the flag, never a
/// Kiwi password or a node secret. The node comes with its [`RemotePorts`].
pub async fn open(
    state_dir: PathBuf,
    sources: &[SourceSpec],
    plugins: &[PluginSpec],
    resume: bool,
) -> Result<(Node, RemotePorts), String> {
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
    };
    let registry = with_plugins(plugins).await?;
    let mut files = Vec::new();
    let mut ports: Vec<Arc<dyn Port>> = Vec::new();
    let mut remotes = RemotePorts::default();
    for spec in sources {
        let flag = |e: &dyn std::fmt::Display| format!("{spec}: {e}");
        let id = SourceId(spec.id().to_owned());
        match spec {
            SourceSpec::File { path, .. } => {
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
        backends,
        resume,
    };
    let node = Node::open(cfg, registry, ports)
        .await
        .map_err(|e| e.to_string())?;
    for s in &files {
        s.start();
    }
    node.add_knowledge(Arc::new(StaticKnowledge::builtin()));
    node.add_classifier(Arc::new(StubClassifier));
    for c in zeta_dsp::classifiers() {
        node.add_classifier(c);
    }
    Ok((node, remotes))
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
        let kinds: Vec<JobKind> = schema["op"]["$defs"]["JobKind"]["enum"]
            .as_array()
            .expect("JobKind enum in the schema")
            .iter()
            .map(|v| serde_json::from_value(v.clone()).unwrap())
            .collect();
        assert!(kinds.len() >= 5, "kinds read from schema: {kinds:?}");
        let src = SourceId("src".into());
        let registry = registry();
        for kind in kinds {
            for rate in [
                12_000u64, 48_000, 1_000_000, 2_048_000, 2_400_000, 8_000_000, 10_000_000,
                20_000_000,
            ] {
                for bw in [10_000u64, 12_500] {
                    let tuning = Tuning {
                        center_hz: Hz(7_100_000),
                        bandwidth_hz: Hz(bw),
                    };
                    match kind.expand(&src, tuning, Hz(rate), None) {
                        Err(OpError::OutOfRange { field, .. }) if field == "bandwidth_hz" => {
                            assert!(rate == 12_000 && bw == 12_500, "{kind:?} {rate} {bw}");
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
    }
}
