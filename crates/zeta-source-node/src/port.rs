//! [`NodePort`]: one source on a remote ZetaSDR node, streamed through a
//! `passthrough` job running there.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use zeta_client::{ClientError, NodeClient, Received, Subscription};
use zeta_core::ops::{JobKind, Op, OpError, OpResult};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::types::{
    Capabilities, Claim, Hz, JobId, SampleFormat, SourceId, StreamKind, Timestamp,
};
use zeta_vrt::IqMeta;

/// Default ceiling on the band requested from a remote node, in Hz.
pub const NODE_DEFAULT_MAX_BANDWIDTH_HZ: u64 = 250_000;
/// How long `tune` waits for the first IQ packet of the remote job's stream.
pub const TUNE_TIMEOUT: Duration = Duration::from_secs(10);
/// Blocks buffered per receiver.
const BROADCAST_CAPACITY: usize = 64;

/// Which remote node and source to use. `Debug` never shows the secret.
#[derive(Clone)]
pub struct NodeSourceConfig {
    /// The local id of this port.
    pub id: SourceId,
    /// `http://host:port` of the remote node.
    pub url: String,
    /// The remote node's bearer secret.
    pub secret: String,
    /// The source on the remote node.
    pub remote_source: SourceId,
    /// Ceiling on the band requested; the advertised maximum is the smaller of
    /// this and the remote source's.
    pub max_bandwidth_hz: u64,
}

impl fmt::Debug for NodeSourceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeSourceConfig")
            .field("id", &self.id)
            .field("url", &self.url)
            .field("secret", &"***")
            .field("remote_source", &self.remote_source)
            .field("max_bandwidth_hz", &self.max_bandwidth_hz)
            .finish()
    }
}

/// State shared with the pump task.
#[derive(Default)]
struct Shared {
    fault: Mutex<Option<String>>,
    current: Mutex<Option<Tuning>>,
}

/// The remote job feeding the port and the task forwarding its stream.
struct Live {
    job: JobId,
    pump: JoinHandle<()>,
}

/// A source on another ZetaSDR node as a push [`Port`].
///
/// Each `tune` starts a `passthrough` job on the remote source (stopping the
/// previous one first) and subscribes to its IQ stream in binary mode. The
/// broadcast lives as long as the port, so receivers taken before a retune or
/// a fault keep receiving afterwards. The pump exits when the remote stream
/// ends (reported through [`Port::fault`]), on the next `tune` or when the
/// port is dropped, which also stop the remote job. It keeps pumping with no
/// receivers, so a later subscriber still gets blocks.
pub struct NodePort {
    id: SourceId,
    caps: Capabilities,
    url: String,
    remote_source: SourceId,
    client: NodeClient,
    tx: broadcast::Sender<IqBlock>,
    shared: Arc<Shared>,
    live: Mutex<Option<Live>>,
    /// Serialises `tune`, which waits on the network.
    gate: tokio::sync::Mutex<()>,
}

/// Open `cfg.remote_source` on the node at `cfg.url`; see [`NodePort::open`].
pub async fn open(cfg: NodeSourceConfig) -> Result<NodePort, PortError> {
    NodePort::open(cfg).await
}

impl NodePort {
    /// One `source_list` round-trip: the remote source must exist. Its
    /// capabilities are copied with `remote` and `push` set, formats `[cf32]`
    /// and `max_bandwidth_hz` capped at `cfg.max_bandwidth_hz`.
    pub async fn open(cfg: NodeSourceConfig) -> Result<NodePort, PortError> {
        let client =
            NodeClient::new(&cfg.url, &cfg.secret).map_err(|e| client_error(&cfg.url, e))?;
        let url = client.url().to_string();
        let sources = match apply(&client, &Op::SourceList {}).await? {
            OpResult::Sources(s) => s,
            other => return Err(unexpected(&url, "source_list", &other)),
        };
        let Some(remote) = sources.iter().find(|s| s.id == cfg.remote_source) else {
            let mut known: Vec<&str> = sources.iter().map(|s| s.id.0.as_str()).collect();
            known.sort_unstable();
            return Err(PortError::Unsupported(format!(
                "remote node {url} has no source {}; known: [{}]",
                cfg.remote_source.0,
                known.join(", ")
            )));
        };
        let remote_max = remote.capabilities.max_bandwidth_hz.value().0;
        let caps = Capabilities {
            max_bandwidth_hz: Claim::Asserted(Hz(remote_max.min(cfg.max_bandwidth_hz))),
            formats: vec![SampleFormat::Cf32],
            remote: true,
            push: true,
            ..remote.capabilities.clone()
        };
        Ok(NodePort {
            id: cfg.id,
            caps,
            url,
            remote_source: cfg.remote_source,
            client,
            tx: broadcast::channel(BROADCAST_CAPACITY).0,
            shared: Arc::new(Shared::default()),
            live: Mutex::new(None),
            gate: tokio::sync::Mutex::new(()),
        })
    }

    fn io(&self, reason: &str) -> PortError {
        PortError::Io(format!("remote node {}: {reason}", self.url))
    }

    /// Stop the pump, then the remote job; a failed `job_stop` is logged.
    async fn stop_live(&self) {
        let live = self.live.lock().unwrap().take();
        let Some(Live { job, pump }) = live else {
            return;
        };
        pump.abort();
        // A cancelled task's JoinError is expected; nothing to report.
        let _ = pump.await;
        stop_job(&self.client, &self.url, job).await;
    }

    /// Start the remote job and its pump; on success the port is live on it.
    async fn start(&self, t: Tuning) -> Result<Tuning, PortError> {
        let op = Op::JobStart {
            kind: JobKind::Passthrough,
            source: self.remote_source.clone(),
            center_hz: t.center_hz,
            bandwidth_hz: t.bandwidth_hz,
            deviation_hz: None,
        };
        let job = match apply(&self.client, &op).await? {
            OpResult::Job(id) => id,
            other => return Err(unexpected(&self.url, "job_start", &other)),
        };
        match self.follow(&job).await {
            Ok((band, pump)) => {
                *self.live.lock().unwrap() = Some(Live { job, pump });
                Ok(band)
            }
            Err(e) => {
                stop_job(&self.client, &self.url, job).await;
                Err(e)
            }
        }
    }

    /// Subscribe to `job`'s IQ stream, spawn the pump and wait for the first
    /// packet's capture band.
    async fn follow(&self, job: &JobId) -> Result<(Tuning, JoinHandle<()>), PortError> {
        let streams = self
            .client
            .streams()
            .await
            .map_err(|e| client_error(&self.url, e))?;
        let Some(stream) = streams
            .into_iter()
            .find(|s| s.kind == StreamKind::Iq && s.provenance.job.as_ref() == Some(job))
        else {
            return Err(self.io(&format!("job {} has no iq stream", job.0)));
        };
        let sub = self
            .client
            .subscribe(&stream.id)
            .await
            .map_err(|e| client_error(&self.url, e))?;
        let (first_tx, first) = oneshot::channel();
        let pump = tokio::spawn(pump(
            sub,
            self.tx.clone(),
            self.shared.clone(),
            self.url.clone(),
            first_tx,
        ));
        let meta = match tokio::time::timeout(TUNE_TIMEOUT, first).await {
            Ok(Ok(meta)) => meta,
            Ok(Err(_)) => {
                // The pump ended before any IQ; it recorded why.
                let why = self.shared.fault.lock().unwrap().clone();
                return Err(PortError::Io(why.unwrap_or_else(|| {
                    format!("remote node {}: stream closed before any iq", self.url)
                })));
            }
            Err(_) => {
                pump.abort();
                // A cancelled task's JoinError is expected; nothing to report.
                let _ = pump.await;
                return Err(self.io(&format!("no iq within {} s", TUNE_TIMEOUT.as_secs())));
            }
        };
        let band = Tuning {
            center_hz: Hz(meta.center_hz),
            bandwidth_hz: Hz(meta.sample_rate_hz),
        };
        Ok((band, pump))
    }
}

impl Drop for NodePort {
    fn drop(&mut self) {
        let Some(Live { job, pump }) = self.live.get_mut().unwrap().take() else {
            return;
        };
        pump.abort();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let client = self.client.clone();
            let url = self.url.clone();
            rt.spawn(async move { stop_job(&client, &url, job).await });
        }
    }
}

#[async_trait]
impl Port for NodePort {
    fn id(&self) -> &SourceId {
        &self.id
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Checks the bandwidth against the cap without touching the network,
    /// stops the previous remote job, starts a `passthrough` job and returns
    /// the band of its first IQ packet (`bandwidth_hz` = its sample rate).
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        let cap = self.caps.max_bandwidth_hz.value().0;
        let bw = t.bandwidth_hz.0;
        if !(1..=cap).contains(&bw) {
            return Err(PortError::OutOfRange {
                field: "bandwidth_hz",
                value: bw,
                min: 1,
                max: cap,
                suggested: Some(cap),
            });
        }
        let _gate = self.gate.lock().await;
        self.stop_live().await;
        *self.shared.current.lock().unwrap() = None;
        let band = self.start(t).await?;
        *self.shared.fault.lock().unwrap() = None;
        *self.shared.current.lock().unwrap() = Some(band);
        Ok(band)
    }

    fn current(&self) -> Option<Tuning> {
        *self.shared.current.lock().unwrap()
    }

    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }

    fn fault(&self) -> Option<String> {
        self.shared.fault.lock().unwrap().clone()
    }
}

/// Forward the subscription's IQ onto `tx`. The first packet's metadata goes
/// to `first`. Ends when the subscription does (setting the fault) or when
/// aborted by a retune or the port's drop; it keeps pumping without
/// receivers so a later subscriber still gets blocks.
async fn pump(
    mut sub: Subscription,
    tx: broadcast::Sender<IqBlock>,
    shared: Arc<Shared>,
    url: String,
    first: oneshot::Sender<IqMeta>,
) {
    let mut first = Some(first);
    while let Some(item) = sub.next().await {
        let (t0_ns, meta, samples) = match item {
            Ok(Received::Iq {
                t0_ns,
                meta,
                samples,
                ..
            }) => (t0_ns, meta, samples),
            // Lost packets show up as a forward jump in the next block's t0.
            Ok(Received::Lagged { .. } | Received::Frame { .. }) => continue,
            Err(e) => {
                eprintln!("remote node {url}: skipping undecodable message: {e}");
                continue;
            }
        };
        if let Some(f) = first.take() {
            // `tune` may have given up waiting; the block still goes out.
            let _ = f.send(meta);
        }
        let block = IqBlock {
            t0: Timestamp(t0_ns),
            sample_rate_hz: Hz(meta.sample_rate_hz),
            center_hz: Hz(meta.center_hz),
            samples: samples.into(),
        };
        // With no receivers the block is dropped; the pump keeps going so a
        // later subscriber gets blocks (it ends on retune, drop or remote close).
        let _ = tx.send(block);
    }
    let reason = sub.close_reason().unwrap_or("ended");
    *shared.fault.lock().unwrap() = Some(format!("remote node {url}: stream closed: {reason}"));
}

/// Best-effort `job_stop`; failures are logged to stderr.
async fn stop_job(client: &NodeClient, url: &str, job: JobId) {
    match client.apply(&Op::JobStop { job: job.clone() }).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!("remote node {url}: job_stop {}: {e}", job.0),
        Err(e) => eprintln!("remote node {url}: job_stop {}: {e}", job.0),
    }
}

/// Apply `op`, mapping a remote refusal to `Unsupported` and a transport
/// failure to `Io`.
async fn apply(client: &NodeClient, op: &Op) -> Result<OpResult, PortError> {
    match client.apply(op).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(op_error(client.url(), &e)),
        Err(e) => Err(client_error(client.url(), e)),
    }
}

fn op_error(url: &str, e: &OpError) -> PortError {
    PortError::Unsupported(format!("remote node {url}: {e}"))
}

/// Client errors never carry the secret; not every variant names the node.
fn client_error(url: &str, e: ClientError) -> PortError {
    PortError::Io(format!("remote node {url}: {e}"))
}

fn unexpected(url: &str, op: &str, r: &OpResult) -> PortError {
    PortError::Io(format!("remote node {url}: unexpected {op} result {r:?}"))
}
