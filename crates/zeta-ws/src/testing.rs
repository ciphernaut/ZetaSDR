//! A test harness: a node on a temp dir with a looping `tone` source, served on loopback.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use zeta_agent::LlmConfig;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::port::Port;
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::SourceId;
use zeta_source_file::fixtures::write_tone;
use zeta_source_file::{FileOpts, FileSource};
use zeta_speech::SpeechBackend;

use crate::auth::Secret;
use crate::http::{Limits, ServerConfig, StatsHandle, bind};

pub struct TestOpts {
    pub secret: String,
    pub limits: Limits,
    /// `Some` binds exactly this address; `None` binds `127.0.0.1:0`.
    pub addr: Option<SocketAddr>,
    pub agents_md: String,
    /// The model behind `POST /ask`; `None` (the default) answers 503.
    pub llm: Option<LlmConfig>,
    /// The speech backend behind `POST /stt`; `None` (the default) answers 503.
    pub speech: Option<Arc<dyn SpeechBackend>>,
}

impl Default for TestOpts {
    fn default() -> Self {
        Self {
            secret: "s3cret".to_string(),
            limits: Limits::default(),
            addr: None,
            agents_md: "# test guide\n".to_string(),
            llm: None,
            speech: None,
        }
    }
}

pub struct TestServer {
    /// `http://IP:PORT`, no trailing slash.
    pub url: String,
    pub addr: SocketAddr,
    pub node: Arc<Node>,
    pub stats: StatsHandle,
    shutdown: CancellationToken,
    join: JoinHandle<std::io::Result<()>>,
}

impl TestServer {
    /// Cancel the server, wait for it to finish, then drop the node.
    pub async fn stop(self) {
        self.shutdown.cancel();
        self.join
            .await
            .expect("server task panicked")
            .expect("server failed");
        drop(self.node);
    }
}

pub async fn spawn(dir: &Path) -> TestServer {
    spawn_with(dir, TestOpts::default()).await
}

/// Open a node on `dir/state` with a real-time looping `tone` source and serve it.
pub async fn spawn_with(dir: &Path, opts: TestOpts) -> TestServer {
    let meta = write_tone(dir);
    let file_opts = FileOpts {
        realtime: true,
        block_samples: 4800,
        repeat: true,
        tune_range: None,
    };
    let source = Arc::new(
        FileSource::open(SourceId("tone".to_string()), &meta, file_opts).expect("open tone"),
    );
    let mut registry = StageRegistry::new();
    registry.register(Arc::new(PassthroughStage::new()));
    let cfg = NodeConfig {
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 10.0,
        ring_budget_bytes: u64::MAX,
        scan_dwell_ms: zeta_core::ops::SCAN_DWELL_MS,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
    };
    let ports = vec![source.clone() as Arc<dyn Port>];
    let node = Arc::new(Node::open(cfg, registry, ports).await.expect("open node"));
    source.start();

    let listen = opts
        .addr
        .unwrap_or(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    let server = bind(
        node.clone(),
        ServerConfig {
            listen,
            secret: Secret::new(opts.secret).expect("test secret"),
            agents_md: opts.agents_md,
            limits: opts.limits,
            llm: opts.llm,
            speech: opts.speech,
        },
    )
    .await
    .expect("bind test server");
    let addr = server.local_addr();
    let stats = server.stats();
    let shutdown = CancellationToken::new();
    let join = tokio::spawn(server.run(shutdown.clone()));
    TestServer {
        url: format!("http://{addr}"),
        addr,
        node,
        stats,
        shutdown,
        join,
    }
}
