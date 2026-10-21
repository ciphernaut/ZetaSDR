//! Command line: one subcommand per `Op` variant, plus `serve`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use zeta_core::graph::Graph;
use zeta_core::ops::{JobKind, Op, OpError, Sideband};
use zeta_core::types::{Hz, JobId, Selection, SourceId, Span, StreamId, Timestamp};

use crate::sources::{self, PluginSpec, SourceSpec};

#[derive(Debug, Parser)]
#[command(
    name = "zeta",
    version,
    about = "ZetaSDR node: apply one operation, or serve until Ctrl-C"
)]
pub struct Cli {
    /// Node state directory [default: $XDG_STATE_HOME/zetasdr or ~/.local/state/zetasdr]
    #[arg(long, global = true, value_name = "PATH")]
    pub state_dir: Option<PathBuf>,
    /// Register a SigMF recording as source <ID> (repeatable)
    #[arg(long = "source-file", global = true, value_name = "ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]", value_parser = sources::parse_file)]
    source_files: Vec<SourceSpec>,
    /// Register a SoapySDR device as source <ID>; ARGS are SoapySDR device arguments,
    /// e.g. driver=hackrf,serial=... (repeatable; needs a build with --cfg soapy)
    #[arg(long = "source-soapy", global = true, value_name = "ID=ARGS", value_parser = sources::parse_soapy)]
    source_soapy: Vec<SourceSpec>,
    /// Register a KiwiSDR as source <ID> (repeatable). The password is never printed,
    /// but like --secret it is visible in `ps`; prefer ZETA_KIWI_PASSWORD_<ID> (ID
    /// upper-cased, other characters as `_`), used when there is no /PASSWORD
    #[arg(
        long = "source-kiwi",
        global = true,
        value_name = "ID=HOST:PORT[/PASSWORD]"
    )]
    source_kiwi: Vec<String>,
    /// Use source SOURCE of the ZetaSDR node at URL (http://HOST:PORT) as source <ID>
    /// (repeatable). Its secret comes from ZETA_NODE_SECRET_<ID> (ID upper-cased, other
    /// characters as `_`) and is never printed. Requests are capped at 250000 Hz wide
    /// unless max_bandwidth_hz=N (1-20000000) widens it
    #[arg(
        long = "source-node",
        global = true,
        value_name = "ID=SOURCE@URL[,max_bandwidth_hz=N]",
        value_parser = sources::parse_node
    )]
    source_node: Vec<SourceSpec>,
    /// Receive VITA 49.2 (VRT) packets on UDP ADDR:PORT as source <ID> (repeatable). ADDR
    /// must be a LAN address (0.0.0.0 listens on every interface); the sender sets the
    /// frequency and rate, and a job's band must lie inside it
    #[arg(
        long = "source-vrt",
        global = true,
        value_name = "ID=udp://ADDR:PORT",
        value_parser = sources::parse_vrt
    )]
    source_vrt: Vec<SourceSpec>,
    /// Register the program at absolute PATH as stage kind KIND (repeatable); probed at startup
    #[arg(long = "plugin", global = true, value_name = "KIND=PATH", value_parser = sources::parse_plugin)]
    pub plugins: Vec<PluginSpec>,
    #[command(flatten)]
    pub speech: SpeechArgs,
    /// Every source flag parsed, file then soapy then kiwi then node then vrt (filled by
    /// `try_parse_checked`).
    #[arg(skip)]
    sources: Vec<SourceSpec>,
    /// Memory budget in MB for one source's IQ ring (default: a quarter of RAM, at most 4 GiB)
    #[arg(long, global = true, env = "ZETA_RING_BUDGET_MB", value_name = "MB")]
    pub ring_budget_mb: Option<u64>,
    /// Machine-readable JSON output (the default when stdout is not a terminal;
    /// no human format exists yet, so output is always JSON)
    #[arg(long, global = true)]
    pub json: bool,
    /// Send the operation to a running `zeta serve` at URL instead of opening a local node
    #[arg(long, global = true, env = "ZETA_NODE", value_name = "URL")]
    pub node: Option<String>,
    /// Shared secret for --node and serve --listen. Visible in `ps` when given on the
    /// command line; prefer the ZETA_SECRET environment variable
    #[arg(
        long,
        global = true,
        env = "ZETA_SECRET",
        hide_env_values = true,
        value_name = "SECRET"
    )]
    pub secret: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Parse the command line, then check the source and plugin flags: `--source-kiwi`
    /// values are parsed here (so clap never echoes a password), each `--source-node`
    /// needs its `ZETA_NODE_SECRET_<ID>` (unless `--node` rejects the flag anyway), and a
    /// source id or a plugin kind given twice is refused.
    pub fn try_parse_checked() -> Result<Self, clap::Error> {
        Self::try_parse()?.checked()
    }

    fn checked(mut self) -> Result<Self, clap::Error> {
        use clap::CommandFactory;
        use clap::error::ErrorKind;
        let err = |kind, msg: String| Err(Self::command().error(kind, msg));
        if self.speech.stt_socket.is_some() && self.speech.stt_node.is_some() {
            return err(ErrorKind::ArgumentConflict, SPEECH_EXCLUSIVE.to_string());
        }
        let mut sources = std::mem::take(&mut self.source_files);
        sources.append(&mut self.source_soapy);
        for raw in std::mem::take(&mut self.source_kiwi) {
            match sources::parse_kiwi(&raw) {
                Ok(s) => sources.push(s),
                Err(e) => return err(ErrorKind::ValueValidation, format!("--source-kiwi: {e}")),
            }
        }
        for spec in std::mem::take(&mut self.source_node) {
            if self.node.is_none()
                && let Err(e) = sources::node_secret(spec.id())
            {
                return err(ErrorKind::MissingRequiredArgument, e);
            }
            sources.push(spec);
        }
        sources.append(&mut self.source_vrt);
        let mut ids = std::collections::HashSet::new();
        for s in &sources {
            if !ids.insert(s.id()) {
                let id = s.id();
                return err(
                    ErrorKind::ArgumentConflict,
                    format!("source id {id:?} given more than once"),
                );
            }
        }
        let mut kinds = std::collections::HashSet::new();
        for p in &self.plugins {
            if !kinds.insert(&p.kind) {
                let kind = &p.kind;
                return err(
                    ErrorKind::ArgumentConflict,
                    format!("plugin kind {kind:?} given more than once"),
                );
            }
        }
        self.sources = sources;
        Ok(self)
    }

    /// The sources to attach: `--source-file`, then `--source-soapy`, then `--source-kiwi`,
    /// then `--source-node`, then `--source-vrt`, each in flag order.
    pub fn sources(&self) -> Vec<SourceSpec> {
        self.sources.clone()
    }

    /// Whether any flag that configures the local node (sources, plugins, state dir) was given.
    pub fn configures_local_node(&self) -> bool {
        !self.sources.is_empty() || !self.plugins.is_empty() || self.state_dir.is_some()
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sources attached to the node
    #[command(subcommand)]
    Source(SourceCmd),
    /// Persistent jobs
    #[command(subcommand)]
    Job(JobCmd),
    /// IQ captures from a source ring
    #[command(subcommand)]
    Iq(IqCmd),
    /// Streams on the node bus
    #[command(subcommand)]
    Stream(StreamCmd),
    /// Job graphs
    #[command(subcommand)]
    Graph(GraphCmd),
    /// Band plans and known signal signatures
    #[command(subcommand)]
    Knowledge(KnowledgeCmd),
    /// Run every classifier over a selection of a source ring
    Classify(SelectionArgs),
    /// JSON schemas of operations, results and errors
    Schema,
    /// Print AGENTS.md rendered from the operation schema; --check compares it to the committed file
    AgentsMd {
        /// Exit 1 if the committed AGENTS.md differs from the rendering
        #[arg(long)]
        check: bool,
        /// Wrap the guide as a Claude Code skill (SKILL.md); with --check compare skills/zetasdr/SKILL.md
        #[arg(long)]
        skill: bool,
    },
    /// The ops log in the state directory (local; not an operation)
    #[command(subcommand)]
    Ops(OpsCmd),
    /// The transcript log in the state directory (local; not an operation)
    #[command(subcommand)]
    Transcripts(TranscriptsCmd),
    /// Run the node, resuming persisted jobs, until Ctrl-C
    Serve(ServeArgs),
    /// Let a local model call the node's operations to satisfy a request
    Ask(AskArgs),
    /// SoapySDR devices attached to this machine (local; not an operation)
    #[command(subcommand)]
    Soapy(SoapyCmd),
}

#[derive(Debug, Subcommand)]
pub enum OpsCmd {
    /// Print the last operations the node applied, one JSON line each, oldest first
    Tail {
        /// How many lines
        #[arg(long, value_name = "N", default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Debug, Subcommand)]
pub enum TranscriptsCmd {
    /// Print the last transcript lines of `transcribe` jobs, one JSON line each, oldest first
    Tail {
        /// How many lines
        #[arg(long, value_name = "N", default_value_t = 50)]
        lines: usize,
        /// Only lines whose `job` or `input` is this job id
        #[arg(long, value_name = "ID")]
        job: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum SoapyCmd {
    /// List attached SoapySDR devices as JSON (the `SoapySDRUtil --find` equivalent)
    Find {
        /// Only devices matching these SoapySDR device arguments, e.g. driver=hackrf
        #[arg(long, value_name = "ARGS", default_value = "")]
        filter: String,
    },
}

const LLM_MAX_TOKENS: u32 = 1024;
const LLM_READY_TIMEOUT_S: u64 = 600;

/// The model behind `zeta ask` and `zeta serve`'s `POST /ask`.
#[derive(Debug, Clone, Args)]
pub struct LlmArgs {
    /// Base URL of an OpenAI-compatible endpoint
    #[arg(long, env = "ZETA_LLM_URL", value_name = "URL")]
    pub llm_url: Option<String>,
    /// Model name sent to the endpoint
    #[arg(long, env = "ZETA_LLM_MODEL", value_name = "NAME")]
    pub llm_model: Option<String>,
    /// API key for the endpoint, if it needs one
    #[arg(long, env = "ZETA_LLM_KEY", hide_env_values = true, value_name = "KEY")]
    pub llm_key: Option<String>,
    /// Token budget per model turn
    #[arg(long, default_value_t = LLM_MAX_TOKENS, value_name = "N")]
    pub llm_max_tokens: u32,
    /// Enable model thinking (off by default)
    #[arg(long)]
    pub llm_thinking: bool,
    /// URL polled until it reports the model healthy before the first request
    #[arg(long, value_name = "URL")]
    pub llm_status_url: Option<String>,
    /// Longest wait for the model to become ready, seconds
    #[arg(long, default_value_t = LLM_READY_TIMEOUT_S, value_name = "SECONDS")]
    pub llm_ready_timeout_s: u64,
}

/// No model, the flags' defaults otherwise.
impl Default for LlmArgs {
    fn default() -> Self {
        Self {
            llm_url: None,
            llm_model: None,
            llm_key: None,
            llm_max_tokens: LLM_MAX_TOKENS,
            llm_thinking: false,
            llm_status_url: None,
            llm_ready_timeout_s: LLM_READY_TIMEOUT_S,
        }
    }
}

#[derive(Debug, Clone, Args)]
#[command(mut_arg("llm_url", |a| a.required(true)))]
#[command(mut_arg("llm_model", |a| a.required(true)))]
pub struct AskArgs {
    /// The request; omit it or pass "-" to read it from stdin
    pub request: Option<String>,
    #[command(flatten)]
    pub llm: LlmArgs,
    /// Most model turns before giving up
    #[arg(long, default_value_t = 8, value_name = "N")]
    pub max_turns: u32,
}

/// The usage error for `--stt-socket` with `--stt-node`.
pub const SPEECH_EXCLUSIVE: &str = "--stt-socket and --stt-node are exclusive";

/// The speech backend: a local daemon or another node.
#[derive(Debug, Clone, Default, Args)]
pub struct SpeechArgs {
    /// Unix socket of the speech daemon (`local-voice`, or any daemon speaking its
    /// `file:`/`languages` protocol) behind `POST /stt` [default:
    /// $XDG_RUNTIME_DIR/voiced.sock when it exists]
    #[arg(long, global = true, env = "ZETA_STT_SOCKET", value_name = "PATH")]
    pub stt_socket: Option<PathBuf>,
    /// Transcribe with the speech backend of the ZetaSDR node at URL (http://HOST:PORT)
    /// instead of a local daemon. Its secret comes from ZETA_STT_NODE_SECRET and is never
    /// printed. Excludes --stt-socket
    #[arg(long, global = true, env = "ZETA_STT_NODE", value_name = "URL")]
    pub stt_node: Option<String>,
}

#[derive(Debug, Clone, Default, Args)]
pub struct ServeArgs {
    /// Also serve the HTTP/WebSocket API on this address (a LAN ip:port, or a bare port on
    /// 127.0.0.1); requires --secret or ZETA_SECRET
    #[arg(long, value_name = "[ADDR:]PORT")]
    pub listen: Option<String>,
    #[command(flatten)]
    pub llm: LlmArgs,
}

#[derive(Debug, Subcommand)]
pub enum SourceCmd {
    /// List sources with capabilities, current tuning and holders
    List,
    /// Check a band against a source and tune it if no job holds it
    Ensure {
        #[command(flatten)]
        band: BandArgs,
        #[command(flatten)]
        replace: ReplaceArg,
    },
    /// Hold a source at a band without a job (persisted; restored on restart)
    Pin {
        #[command(flatten)]
        band: BandArgs,
        #[command(flatten)]
        replace: ReplaceArg,
    },
    /// Release a source's pin
    Unpin {
        /// Source id
        #[arg(long)]
        source: String,
    },
    /// Set a source's overall receive gain (persisted; re-applied on restart)
    Gain {
        /// Source id
        #[arg(long)]
        source: String,
        /// Gain in dB, within `source list`'s gain.min_db..=max_db
        #[arg(
            long,
            value_name = "DB",
            allow_negative_numbers = true,
            required_unless_present = "auto",
            conflicts_with = "auto"
        )]
        db: Option<f64>,
        /// The driver's automatic gain instead of a value
        #[arg(long)]
        auto: bool,
    },
}

/// `--replace ID[,ID]` on `job start`, `source ensure` and `source pin`.
#[derive(Debug, Args)]
pub struct ReplaceArg {
    /// Jobs holding the source that may be stopped to make room, comma-separated
    /// (ids from `job list`); they are stopped right before the request.
    /// Absent or empty = none
    #[arg(long = "replace", value_name = "ID[,ID]", value_parser = parse_replace)]
    ids: Option<ReplaceIds>,
}

impl ReplaceArg {
    fn into_ids(self) -> Vec<JobId> {
        self.ids.map_or_else(Vec::new, |r| r.0)
    }
}

/// The ids of `--replace`, empty items dropped (so `--replace ''` is none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceIds(Vec<JobId>);

fn parse_replace(s: &str) -> Result<ReplaceIds, String> {
    Ok(ReplaceIds(
        s.split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| JobId(id.to_string()))
            .collect(),
    ))
}

#[derive(Debug, Subcommand)]
pub enum JobCmd {
    /// Start a job of a built-in kind on a source
    Start {
        /// Job kind
        #[arg(long, value_enum)]
        kind: Kind,
        #[command(flatten)]
        band: BandArgs,
        /// FM peak deviation in Hz (100..=75000), fm_demod and wfm_demod only;
        /// absent = the kind's default (5000 for fm_demod, 75000 for wfm_demod)
        #[arg(long, value_name = "HZ")]
        deviation_hz: Option<u64>,
        /// Sideband for ssb_demod only; absent = usb
        #[arg(long, value_enum)]
        sideband: Option<SidebandArg>,
        /// spectrum only: follow the source's capture instead of holding it
        /// (never conflicts; relaunched under the same id on every retune)
        #[arg(long)]
        elastic: bool,
        #[command(flatten)]
        replace: ReplaceArg,
        /// After starting, tail the job's first event (else audio) stream, stop
        /// the job and print `[job_result, frames_result, {"stopped": true}]`;
        /// a job that is not running prints `[job_result, job_record]` and
        /// exits 2 (one-shot only)
        #[arg(long, value_name = "MAX_FRAMES,TIMEOUT_MS", value_parser = parse_then_tail)]
        then_tail: Option<ThenTail>,
        /// With --then-tail: leave the job running after the tail
        /// (prints `{"stopped": false}`)
        #[arg(long, requires = "then_tail")]
        keep: bool,
    },
    /// Attach a processing chain to a running job's output instead of the radio (needs
    /// that job running in the same node: use it against `zeta serve` with --node)
    Attach {
        /// Attached job kind
        #[arg(long, value_enum)]
        kind: AttachKind,
        /// The running job to attach to (an id from `job list`)
        #[arg(long, value_name = "ID")]
        job: String,
        /// BCP-47 language code of the speech (en, de, pt-BR); absent = en
        #[arg(long, value_name = "CODE")]
        language: Option<String>,
    },
    /// List jobs
    List,
    /// Stop a job
    Stop(JobArg),
    /// Show a job's context
    Context(JobArg),
}

#[derive(Debug, Subcommand)]
pub enum IqCmd {
    /// Save a selection of a source ring as a SigMF pair under the state directory
    Read(SelectionArgs),
}

#[derive(Debug, Subcommand)]
pub enum StreamCmd {
    /// List streams
    List,
    /// Collect frames from a stream until MAX_FRAMES arrive or TIMEOUT_MS passes
    Tail {
        /// Stream id from `stream list`
        #[arg(long, value_name = "ID")]
        stream: String,
        /// Most frames to collect
        #[arg(long, value_name = "N")]
        max_frames: u32,
        /// Longest wait, milliseconds
        #[arg(long, value_name = "MS")]
        timeout_ms: u32,
    },
}

/// `--then-tail MAX_FRAMES,TIMEOUT_MS` on `job start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThenTail {
    pub max_frames: u32,
    pub timeout_ms: u32,
}

fn parse_then_tail(s: &str) -> Result<ThenTail, String> {
    let bad = || format!("expected MAX_FRAMES,TIMEOUT_MS, got {s:?}");
    let (n, ms) = s.split_once(',').ok_or_else(bad)?;
    Ok(ThenTail {
        max_frames: n.trim().parse().map_err(|_| bad())?,
        timeout_ms: ms.trim().parse().map_err(|_| bad())?,
    })
}

#[derive(Debug, Subcommand)]
pub enum GraphCmd {
    /// Show a job's graph
    Get(JobArg),
    /// Replace a job's graph from a JSON file
    Put {
        #[command(flatten)]
        job: JobArg,
        /// Graph JSON file
        #[arg(long, value_name = "GRAPH.json")]
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum KnowledgeCmd {
    /// Allocations in a region and signatures overlapping a band
    Lookup {
        /// Band-plan region, e.g. AU
        #[arg(long)]
        region: String,
        /// Centre frequency, Hz
        #[arg(long, value_name = "HZ")]
        center_hz: u64,
        /// Bandwidth, Hz
        #[arg(long, value_name = "HZ")]
        bandwidth_hz: u64,
    },
}

/// Built-in job kinds, mirroring `JobKind`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Kind {
    Passthrough,
    Spectrum,
    // Spelled as on the wire, not clap's default `fm-demod`.
    #[value(name = "fm_demod")]
    FmDemod,
    #[value(name = "wfm_demod")]
    WfmDemod,
    #[value(name = "am_demod")]
    AmDemod,
    #[value(name = "ssb_demod")]
    SsbDemod,
    #[value(name = "aprs_decode")]
    AprsDecode,
    #[value(name = "pocsag_decode")]
    PocsagDecode,
    #[value(name = "rds_decode")]
    RdsDecode,
    #[value(name = "acars_decode")]
    AcarsDecode,
    #[value(name = "adsb_decode")]
    AdsbDecode,
    Measure,
    Scan,
}

impl From<Kind> for JobKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Passthrough => JobKind::Passthrough,
            Kind::Spectrum => JobKind::Spectrum,
            Kind::FmDemod => JobKind::FmDemod,
            Kind::WfmDemod => JobKind::WfmDemod,
            Kind::AmDemod => JobKind::AmDemod,
            Kind::SsbDemod => JobKind::SsbDemod,
            Kind::AprsDecode => JobKind::AprsDecode,
            Kind::PocsagDecode => JobKind::PocsagDecode,
            Kind::RdsDecode => JobKind::RdsDecode,
            Kind::AcarsDecode => JobKind::AcarsDecode,
            Kind::AdsbDecode => JobKind::AdsbDecode,
            Kind::Measure => JobKind::Measure,
            Kind::Scan => JobKind::Scan,
        }
    }
}

/// Attachable job kinds (`JobKind::attachable`), for `job attach`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AttachKind {
    Transcribe,
}

impl From<AttachKind> for JobKind {
    fn from(k: AttachKind) -> Self {
        match k {
            AttachKind::Transcribe => JobKind::Transcribe,
        }
    }
}

/// SSB sidebands, mirroring `Sideband`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SidebandArg {
    Usb,
    Lsb,
}

impl From<SidebandArg> for Sideband {
    fn from(s: SidebandArg) -> Self {
        match s {
            SidebandArg::Usb => Sideband::Usb,
            SidebandArg::Lsb => Sideband::Lsb,
        }
    }
}

#[derive(Debug, Args)]
pub struct BandArgs {
    /// Source id
    #[arg(long)]
    source: String,
    /// Centre frequency, Hz
    #[arg(long, value_name = "HZ")]
    center_hz: u64,
    /// Bandwidth, Hz
    #[arg(long, value_name = "HZ")]
    bandwidth_hz: u64,
}

#[derive(Debug, Args)]
pub struct JobArg {
    /// Job id
    #[arg(long, value_name = "ID")]
    job: String,
}

#[derive(Debug, Args)]
pub struct SelectionArgs {
    /// Source id
    #[arg(long)]
    source: String,
    /// Span start, nanoseconds since the UNIX epoch
    #[arg(long, value_name = "NS")]
    start_ns: u64,
    /// Span end, nanoseconds since the UNIX epoch
    #[arg(long, value_name = "NS")]
    end_ns: u64,
    /// Centre of the selected band, Hz
    #[arg(long, value_name = "HZ")]
    center_hz: u64,
    /// Width of the selected band, Hz
    #[arg(long, value_name = "HZ")]
    bandwidth_hz: u64,
}

impl From<SelectionArgs> for Selection {
    fn from(a: SelectionArgs) -> Self {
        Selection {
            source: SourceId(a.source),
            span: Span {
                start_ns: Timestamp(a.start_ns),
                end_ns: Timestamp(a.end_ns),
            },
            center_hz: Hz(a.center_hz),
            bandwidth_hz: Hz(a.bandwidth_hz),
        }
    }
}

impl Command {
    /// Whether the subcommand changes persisted state or a source's tuning;
    /// these are refused while `zeta serve` owns the state directory.
    pub fn mutates(&self) -> bool {
        matches!(
            self,
            Command::Source(
                SourceCmd::Ensure { .. }
                    | SourceCmd::Pin { .. }
                    | SourceCmd::Unpin { .. }
                    | SourceCmd::Gain { .. }
            ) | Command::Job(JobCmd::Start { .. } | JobCmd::Attach { .. } | JobCmd::Stop(_))
                | Command::Graph(GraphCmd::Put { .. })
        )
    }

    /// `--then-tail` of a `job start`, if given.
    pub fn then_tail(&self) -> Option<ThenTail> {
        match self {
            Command::Job(JobCmd::Start { then_tail, .. }) => *then_tail,
            _ => None,
        }
    }

    /// `job start --keep`: leave a `--then-tail` job running.
    pub fn keep(&self) -> bool {
        matches!(self, Command::Job(JobCmd::Start { keep: true, .. }))
    }

    /// The operation this subcommand applies; `None` for `serve`. Fails only
    /// when `graph put` cannot read or parse its file.
    pub fn into_op(self) -> Result<Option<Op>, OpError> {
        let job = |a: JobArg| JobId(a.job);
        Ok(Some(match self {
            Command::Source(SourceCmd::List) => Op::SourceList {},
            Command::Source(SourceCmd::Ensure { band: b, replace }) => Op::SourceEnsure {
                source: SourceId(b.source),
                center_hz: Hz(b.center_hz),
                bandwidth_hz: Hz(b.bandwidth_hz),
                replace: replace.into_ids(),
            },
            Command::Source(SourceCmd::Pin { band: b, replace }) => Op::SourcePin {
                source: SourceId(b.source),
                center_hz: Hz(b.center_hz),
                bandwidth_hz: Hz(b.bandwidth_hz),
                replace: replace.into_ids(),
            },
            Command::Source(SourceCmd::Unpin { source }) => Op::SourceUnpin {
                source: SourceId(source),
            },
            // clap admits exactly one of `--db` and `--auto`.
            Command::Source(SourceCmd::Gain {
                source,
                db,
                auto: _,
            }) => Op::SourceSetGain {
                source: SourceId(source),
                gain_db: db,
            },
            Command::Job(JobCmd::Start {
                kind,
                band: b,
                deviation_hz,
                sideband,
                elastic,
                replace,
                ..
            }) => Op::JobStart {
                kind: kind.into(),
                source: SourceId(b.source),
                center_hz: Hz(b.center_hz),
                bandwidth_hz: Hz(b.bandwidth_hz),
                deviation_hz,
                sideband: sideband.map(Into::into),
                elastic,
                replace: replace.into_ids(),
            },
            Command::Job(JobCmd::Attach {
                kind,
                job,
                language,
            }) => Op::JobAttach {
                kind: kind.into(),
                job: JobId(job),
                language,
            },
            Command::Job(JobCmd::List) => Op::JobList {},
            Command::Job(JobCmd::Stop(a)) => Op::JobStop { job: job(a) },
            Command::Job(JobCmd::Context(a)) => Op::JobContext { job: job(a) },
            Command::Iq(IqCmd::Read(s)) => Op::IqRead {
                selection: s.into(),
            },
            Command::Stream(StreamCmd::List) => Op::StreamList {},
            Command::Stream(StreamCmd::Tail {
                stream,
                max_frames,
                timeout_ms,
            }) => Op::StreamTail {
                stream: StreamId(stream),
                max_frames,
                timeout_ms,
            },
            Command::Graph(GraphCmd::Get(a)) => Op::GraphGet { job: job(a) },
            Command::Graph(GraphCmd::Put { job: a, file }) => Op::GraphPut {
                job: job(a),
                graph: read_graph(&file)?,
            },
            Command::Knowledge(KnowledgeCmd::Lookup {
                region,
                center_hz,
                bandwidth_hz,
            }) => Op::KnowledgeLookup {
                region,
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(bandwidth_hz),
            },
            Command::Classify(s) => Op::Classify {
                selection: s.into(),
            },
            Command::Schema => Op::Schema {},
            Command::AgentsMd { .. }
            | Command::Serve(_)
            | Command::Ask(_)
            | Command::Soapy(_)
            | Command::Ops(_)
            | Command::Transcripts(_) => {
                return Ok(None);
            }
        }))
    }
}

fn read_graph(path: &std::path::Path) -> Result<Graph, OpError> {
    let other = |e: &dyn std::fmt::Display| OpError::Other {
        message: format!("{}: {e}", path.display()),
    };
    let text = std::fs::read_to_string(path).map_err(|e| other(&e))?;
    serde_json::from_str(&text).map_err(|e| other(&e))
}
