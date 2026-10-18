//! Command line: one subcommand per `Op` variant, plus `serve`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use zeta_core::graph::Graph;
use zeta_core::ops::{JobKind, Op, OpError};
use zeta_core::types::{Hz, JobId, Selection, SourceId, Span, StreamId, Timestamp};

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
    #[arg(long = "source-file", global = true, value_name = "ID=PATH.sigmf-meta", value_parser = parse_source_file)]
    pub source_files: Vec<(String, PathBuf)>,
    /// Machine-readable JSON output (the default when stdout is not a terminal;
    /// no human format exists yet, so output is always JSON)
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Parse the command line, rejecting a `--source-file` id given twice.
    pub fn try_parse_checked() -> Result<Self, clap::Error> {
        use clap::CommandFactory;
        let cli = Self::try_parse()?;
        let mut seen = std::collections::HashSet::new();
        for (id, _) in &cli.source_files {
            if !seen.insert(id) {
                return Err(Self::command().error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("--source-file id {id:?} given more than once"),
                ));
            }
        }
        Ok(cli)
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
    },
    /// Run the node, resuming persisted jobs, until Ctrl-C
    Serve,
}

#[derive(Debug, Subcommand)]
pub enum SourceCmd {
    /// List sources with capabilities, current tuning and holders
    List,
    /// Check a band against a source and tune it if no job holds it
    Ensure(BandArgs),
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
    #[value(name = "aprs_decode")]
    AprsDecode,
    Measure,
}

impl From<Kind> for JobKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Passthrough => JobKind::Passthrough,
            Kind::Spectrum => JobKind::Spectrum,
            Kind::FmDemod => JobKind::FmDemod,
            Kind::AprsDecode => JobKind::AprsDecode,
            Kind::Measure => JobKind::Measure,
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

fn parse_source_file(s: &str) -> Result<(String, PathBuf), String> {
    match s.split_once('=') {
        Some((id, path)) if !id.is_empty() && !path.is_empty() => Ok((id.into(), path.into())),
        _ => Err(format!("expected ID=PATH.sigmf-meta, got {s:?}")),
    }
}

impl Command {
    /// Whether the subcommand changes persisted state or a source's tuning;
    /// these are refused while `zeta serve` owns the state directory.
    pub fn mutates(&self) -> bool {
        matches!(
            self,
            Command::Source(SourceCmd::Ensure(_))
                | Command::Job(JobCmd::Start { .. } | JobCmd::Stop(_))
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
            Command::Source(SourceCmd::Ensure(b)) => Op::SourceEnsure {
                source: SourceId(b.source),
                center_hz: Hz(b.center_hz),
                bandwidth_hz: Hz(b.bandwidth_hz),
            },
            Command::Job(JobCmd::Start { kind, band: b, .. }) => Op::JobStart {
                kind: kind.into(),
                source: SourceId(b.source),
                center_hz: Hz(b.center_hz),
                bandwidth_hz: Hz(b.bandwidth_hz),
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
            Command::AgentsMd { .. } | Command::Serve => return Ok(None),
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
