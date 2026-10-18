//! Build the node the CLI drives: stage registry (core plus `zeta-dsp`), file
//! sources, knowledge and classifiers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zeta_core::classify::StubClassifier;
use zeta_core::knowledge::StaticKnowledge;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::SourceId;
use zeta_source_file::{FileOpts, FileSource};

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

/// Open the node on `state_dir` with one looping, real-time `FileSource` per `(id, meta path)`.
/// Sources start after the node has subscribed to them: a source with no
/// subscriber stops at once. `resume` relaunches persisted `Running` jobs
/// (`serve`); without it they are listed as persisted and left alone (one-shot).
pub async fn open(
    state_dir: PathBuf,
    source_files: &[(String, PathBuf)],
    resume: bool,
) -> Result<Node, String> {
    let opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
    };
    let sources = source_files
        .iter()
        .map(|(id, path): &(String, PathBuf)| {
            FileSource::open(SourceId(id.clone()), Path::new(path), opts)
                .map(Arc::new)
                .map_err(|e| format!("--source-file {id}={}: {e}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
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
    let ports = sources.iter().map(|s| s.clone() as Arc<dyn Port>).collect();
    let node = Node::open(cfg, registry(), ports)
        .await
        .map_err(|e| e.to_string())?;
    for s in &sources {
        s.start();
    }
    node.add_knowledge(Arc::new(StaticKnowledge::builtin()));
    node.add_classifier(Arc::new(StubClassifier));
    for c in zeta_dsp::classifiers() {
        node.add_classifier(c);
    }
    Ok(node)
}
