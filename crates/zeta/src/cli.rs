//! Command line: one subcommand per `Op` variant, plus `serve`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use zeta_core::graph::Graph;
use zeta_core::modes::{Family, ModeState};
use zeta_core::ops::{JobKind, Modulation, Op, OpError, Sideband, SignalBand};
use zeta_core::station::{Antenna, AntennaKind, Station};
use zeta_core::types::{Baud, Hz, JobId, Selection, SourceId, Span, StreamId, Timestamp};

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
    /// Open --source-soapy devices inside this process instead of a supervised worker
    /// child (diagnosis only: a driver crash then ends the node)
    #[arg(long, global = true)]
    pub soapy_in_process: bool,
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
    /// Directory of the operator's mode rows (*.json), read after the built-in ones; a row
    /// with a built-in id replaces it [default: $XDG_CONFIG_HOME/zetasdr/modes or
    /// ~/.config/zetasdr/modes]
    #[arg(long, global = true, value_name = "PATH")]
    pub modes_dir: Option<PathBuf>,
    #[command(flatten)]
    pub speech: SpeechArgs,
    /// Every source flag parsed, file then soapy then kiwi then node then vrt (filled by
    /// `try_parse_checked`).
    #[arg(skip)]
    sources: Vec<SourceSpec>,
    /// Memory budget in MB for one source's IQ ring (default: a quarter of RAM, at most 4 GiB)
    #[arg(long, global = true, env = "ZETA_RING_BUDGET_MB", value_name = "MB")]
    pub ring_budget_mb: Option<u64>,
    /// Keep at most MB megabytes of unpinned burst captures (default 256); the oldest go first
    #[arg(
        long,
        global = true,
        env = "ZETA_BURSTS_KEEP_MB",
        value_name = "MB",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub bursts_keep_mb: Option<u64>,
    /// Drop unpinned burst captures older than DAYS days (default 7)
    #[arg(
        long,
        global = true,
        env = "ZETA_BURSTS_KEEP_DAYS",
        value_name = "DAYS",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub bursts_keep_days: Option<u32>,
    /// Attach the ACMA register service (MCP over HTTP) as a knowledge provider; `reach_km=N`
    /// overrides its reach rule
    #[arg(
        long = "knowledge-acma",
        global = true,
        env = "ZETA_KNOWLEDGE_ACMA",
        value_name = "URL[,reach_km=N]"
    )]
    knowledge_acma_raw: Option<String>,
    /// `--knowledge-acma` parsed (filled by `try_parse_checked`).
    #[arg(skip)]
    pub knowledge_acma: Option<(String, Option<f64>)>,
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
    /// The burst store's policy: the defaults, with `--bursts-keep-mb` and
    /// `--bursts-keep-days` where given. The store is expired at open only when one of
    /// them was given (the caller also turns that on for `serve` and off for a one-shot
    /// under a running serve), so a plain one-shot never applies the default policy to
    /// captures `zeta serve` keeps under its own.
    pub fn burst_policy(&self) -> zeta_core::bursts::BurstPolicy {
        let d = zeta_core::bursts::BurstPolicy::default();
        zeta_core::bursts::BurstPolicy {
            keep_bytes: self
                .bursts_keep_mb
                .map_or(d.keep_bytes, |mb| mb.saturating_mul(1 << 20)),
            keep_days: self.bursts_keep_days.unwrap_or(d.keep_days),
            expire_at_open: self.bursts_keep_mb.is_some() || self.bursts_keep_days.is_some(),
        }
    }

    /// Parse the command line, then check the source and plugin flags: `--source-kiwi`
    /// values are parsed here (so clap never echoes a password), each `--source-node`
    /// needs its `ZETA_NODE_SECRET_<ID>` (unless `--node` rejects the flag anyway), and a
    /// source id or a plugin kind given twice is refused.
    pub fn try_parse_checked() -> Result<Self, clap::Error> {
        Self::try_parse_checked_from(std::env::args_os())
    }

    /// [`Cli::try_parse_checked`] over `args` (the first being the program name).
    pub fn try_parse_checked_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        use clap::{CommandFactory, FromArgMatches};
        let matches = Self::command().try_get_matches_from(args)?;
        let mut cli = Self::from_arg_matches(&matches)?;
        // The node-local flags an environment variable can set: with `--node` the
        // environment's values are ignored and only an explicit command-line flag is
        // refused, so a shell that sources the serving node's settings still talks to it.
        if cli.node.is_some() {
            let from_env =
                |id: &str| matches.value_source(id) != Some(clap::parser::ValueSource::CommandLine);
            if from_env("knowledge_acma_raw") {
                cli.knowledge_acma_raw = None;
            }
            if from_env("bursts_keep_mb") {
                cli.bursts_keep_mb = None;
            }
            if from_env("bursts_keep_days") {
                cli.bursts_keep_days = None;
            }
        }
        cli.checked()
    }

    fn checked(mut self) -> Result<Self, clap::Error> {
        use clap::CommandFactory;
        use clap::error::ErrorKind;
        let err = |kind, msg: String| Err(Self::command().error(kind, msg));
        if self.speech.stt_socket.is_some() && self.speech.stt_node.is_some() {
            return err(ErrorKind::ArgumentConflict, SPEECH_EXCLUSIVE.to_string());
        }
        if let Some(raw) = self.knowledge_acma_raw.take() {
            match sources::parse_knowledge_acma(&raw) {
                Ok(k) => self.knowledge_acma = Some(k),
                Err(e) => return err(ErrorKind::ValueValidation, e),
            }
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

    /// Whether any flag that configures the local node (sources, plugins, state dir, modes
    /// dir, `--knowledge-acma`, `--bursts-keep-*`; with `--node` their environment variables
    /// were already dropped by `try_parse_checked`) was given.
    pub fn configures_local_node(&self) -> bool {
        !self.sources.is_empty()
            || !self.plugins.is_empty()
            || self.state_dir.is_some()
            || self.modes_dir.is_some()
            || self.knowledge_acma.is_some()
            || self.bursts_keep_mb.is_some()
            || self.bursts_keep_days.is_some()
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
    /// The channel plans the node knows (e.g. uhf_cb_au) with their channels
    Plans {
        /// Only this plan
        #[arg(long, value_name = "ID")]
        plan: Option<String>,
    },
    /// Start a kind on every channel of a channel plan
    #[command(subcommand)]
    Plan(PlanCmd),
    /// Listen to many channels as one audio stream
    #[command(subcommand)]
    Mix(MixCmd),
    /// Work a burst capture's bits
    #[command(subcommand)]
    Bits(BitsCmd),
    /// The node's station: position, region and antennas
    #[command(subcommand)]
    Station(StationCmd),
    /// The burst captures the node stored
    #[command(subcommand)]
    Capture(CaptureCmd),
    /// Many jobs at once
    #[command(subcommand)]
    Jobs(JobsCmd),
    /// The decoder modes the node knows
    #[command(subcommand)]
    Modes(ModesCmd),
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
    /// Serve one SoapySDR device over stdin and stdout for the node that spawned it
    /// (the node's own child; not an operator command)
    #[command(hide = true)]
    SoapyWorker(SoapyWorkerArgs),
}

/// `zeta soapy-worker`: the device a node's `--source-soapy` names, served to that node.
#[derive(Debug, Clone, Args)]
pub struct SoapyWorkerArgs {
    /// The source id the device serves as
    #[arg(long)]
    pub id: String,
    /// SoapySDR device arguments, zeta's own gain= and ppm= included
    #[arg(long)]
    pub args: String,
}

#[derive(Debug, Subcommand)]
pub enum JobsCmd {
    /// Stop every running job matching the filters (none: every job), except elastic
    /// jobs and those another job owns
    Stop {
        /// Only this source
        #[arg(long)]
        source: Option<String>,
        /// Only this kind
        #[arg(long, value_parser = parse_job_kind)]
        kind: Option<JobKind>,
        /// Only jobs on a channel of this plan
        #[arg(long, value_name = "ID")]
        plan: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum MixCmd {
    /// Start a mix: one audio stream summing every job with audio output on the sources
    Start {
        /// Only the audio of jobs on these sources (default: every source)
        #[arg(long, value_name = "ID[,ID]", value_delimiter = ',')]
        sources: Option<Vec<String>>,
    },
}

#[derive(Debug, Subcommand)]
pub enum BitsCmd {
    /// Start the replay-demod-clock-slice-view chain over a capture's set, from its
    /// feature record (no radio, no lease)
    Start {
        /// A capture id (from `capture list`)
        #[arg(long, value_name = "ID")]
        capture: String,
        /// The demodulator; absent = the layer-1 label's
        #[arg(long, value_enum)]
        modulation: Option<ModulationArg>,
        /// The symbol rate in baud; absent = the feature record's
        #[arg(long, value_name = "N")]
        symbol_rate_baud: Option<u32>,
        /// The modulation order; absent = the feature record's, else 2
        #[arg(long, value_name = "N")]
        order: Option<u8>,
        /// After starting, tail the slicer's `bits` events, stop the job and print
        /// `[job_result, frames_result, {"stopped": true}]` (one-shot only)
        #[arg(long, value_name = "MAX_FRAMES,TIMEOUT_MS", value_parser = parse_then_tail)]
        then_tail: Option<ThenTail>,
        /// With --then-tail: leave the job running after the tail
        #[arg(long, requires = "then_tail")]
        keep: bool,
    },
}

/// `bits start --modulation`, mirroring `Modulation`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ModulationArg {
    Fsk,
    Psk,
    Ook,
}

impl From<ModulationArg> for Modulation {
    fn from(m: ModulationArg) -> Self {
        match m {
            ModulationArg::Fsk => Modulation::Fsk,
            ModulationArg::Psk => Modulation::Psk,
            ModulationArg::Ook => Modulation::Ook,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum CaptureCmd {
    /// List the captures, newest first; with --center-hz and --bandwidth-hz only those
    /// overlapping that band
    List {
        /// The band's centre, Hz (with --bandwidth-hz)
        #[arg(long, requires = "bandwidth_hz")]
        center_hz: Option<u64>,
        /// The band's width, Hz (with --center-hz)
        #[arg(long, requires = "center_hz")]
        bandwidth_hz: Option<u64>,
    },
    /// Print one capture's record and its SigMF meta
    Show {
        /// A capture id (from `capture list`)
        #[arg(long)]
        capture: String,
    },
    /// Run the classifiers over a capture's window
    Classify {
        /// A capture id (from `capture list`)
        #[arg(long)]
        capture: String,
    },
    /// Delete a capture and its files (a pinned one is refused)
    Delete {
        /// A capture id (from `capture list`)
        #[arg(long)]
        capture: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum StationCmd {
    /// Set the station (persisted); the defaults (a dipole and a discone) always stay, each
    /// --antenna adds one
    Set {
        /// Latitude, degrees (-90..=90)
        #[arg(long, allow_negative_numbers = true)]
        lat: f64,
        /// Longitude, degrees (-180..=180)
        #[arg(long, allow_negative_numbers = true)]
        lon: f64,
        /// Altitude, metres
        #[arg(long, value_name = "M", allow_negative_numbers = true)]
        alt_m: f64,
        /// Region code
        #[arg(long, default_value = "AU")]
        region: String,
        /// An antenna: KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]
        /// (kind: discone, dipole, skyloop, vertical, yagi, dish or other); repeatable
        #[arg(long, value_name = "SPEC", value_parser = parse_antenna)]
        antenna: Vec<Antenna>,
    },
    /// Print the station (null when none)
    Get,
    /// Forget the station
    Clear,
}

/// `KIND=MIN_HZ-MAX_HZ[,height_m=N,azimuth_deg=N,elevation_deg=N]`; the id is the kind
/// (made unique by `number_antennas`), `height_m` defaults to 5.
fn parse_antenna(s: &str) -> Result<Antenna, String> {
    let mut parts = s.split(',');
    let head = parts.next().unwrap_or("");
    let (kind, band) = head
        .split_once('=')
        .ok_or("expected KIND=MIN_HZ-MAX_HZ[,height_m=N,...]")?;
    let kind: AntennaKind = serde_json::from_value(serde_json::Value::from(kind))
        .map_err(|_| format!("unknown antenna kind {kind}"))?;
    let (lo, hi) = band.split_once('-').ok_or("expected MIN_HZ-MAX_HZ")?;
    let hz = |v: &str| v.parse::<u64>().map_err(|e| format!("{v}: {e}"));
    let mut a = Antenna {
        id: kind_word(kind),
        kind,
        min_hz: Hz(hz(lo)?),
        max_hz: Hz(hz(hi)?),
        height_m: 5.0,
        azimuth_deg: None,
        elevation_deg: None,
    };
    for kv in parts {
        let (k, v) = kv
            .split_once('=')
            .ok_or(format!("{kv}: expected key=value"))?;
        let n: f64 = v.parse().map_err(|e| format!("{kv}: {e}"))?;
        match k {
            "height_m" => a.height_m = n,
            "azimuth_deg" => a.azimuth_deg = Some(n),
            "elevation_deg" => a.elevation_deg = Some(n),
            _ => return Err(format!("unknown antenna key {k}")),
        }
    }
    Ok(a)
}

fn kind_word(k: AntennaKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Antenna ids from the kinds: the first of a kind keeps it, later ones get `-2`, `-3`.
fn number_antennas(mut v: Vec<Antenna>) -> Vec<Antenna> {
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for a in &mut v {
        let n = seen.entry(a.id.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            a.id = format!("{}-{}", a.id, n);
        }
    }
    v
}

#[derive(Debug, Subcommand)]
pub enum PlanCmd {
    /// Start a kind on channels of a plan under one lease decision, optionally
    /// attaching a transcriber to each
    Start {
        /// Plan id (from `zeta plans`)
        #[arg(long, value_name = "ID")]
        plan: String,
        /// Channel numbers, e.g. 1,2,5-8; absent = every channel of the plan
        #[arg(long, value_name = "N[,N|N-M]", value_parser = parse_channels)]
        channels: Option<Channels>,
        /// Job kind to start on each channel
        #[arg(long, value_parser = parse_job_kind)]
        kind: JobKind,
        /// Source id
        #[arg(long)]
        source: String,
        /// FM peak deviation in Hz, as on `job start`; absent = the kind's default
        #[arg(long, value_name = "HZ")]
        deviation_hz: Option<u64>,
        /// Sideband for ssb_demod only; absent = usb
        #[arg(long, value_enum)]
        sideband: Option<SidebandArg>,
        /// Attach this kind to every started job
        #[arg(long, value_enum)]
        attach: Option<AttachKind>,
        /// Language of the attached job (BCP-47); absent = en
        #[arg(long)]
        language: Option<String>,
        #[command(flatten)]
        replace: ReplaceArg,
    },
}

/// The channel numbers of `--channels`: sorted, no repeats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channels(Vec<u32>);

/// The widest `N-M` range of `--channels`, so a typo cannot expand to billions of numbers.
const MAX_CHANNEL_RANGE: u32 = 1000;

/// `1,2,5-8` into sorted, deduplicated channel numbers; an empty item or a reversed
/// range is an error.
pub fn parse_channels(s: &str) -> Result<Channels, String> {
    let num = |t: &str| {
        t.trim()
            .parse::<u32>()
            .map_err(|_| format!("channels: `{t}` is not a channel number"))
    };
    let mut out = vec![];
    for item in s.split(',') {
        match item.split_once('-') {
            Some((lo, hi)) => {
                let (lo, hi) = (num(lo)?, num(hi)?);
                if lo > hi {
                    return Err(format!("channels: range {item} is reversed"));
                }
                if hi - lo >= MAX_CHANNEL_RANGE {
                    return Err(format!(
                        "channels: range {item} is wider than {MAX_CHANNEL_RANGE} channels"
                    ));
                }
                out.extend(lo..=hi);
            }
            None => out.push(num(item)?),
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(Channels(out))
}

#[derive(Debug, Subcommand)]
pub enum ModesCmd {
    /// List the modes with their state (ready, broken, unavailable)
    List {
        /// Only this family (e.g. aviation, pagers, amateur_digital)
        #[arg(long, value_name = "F", value_parser = parse_family)]
        family: Option<Family>,
        /// Only this state: ready, broken or unavailable
        #[arg(long, value_name = "S", value_parser = parse_mode_state)]
        state: Option<ModeState>,
    },
    /// Print one mode's row as loaded
    Show {
        /// The mode id (from `modes list`)
        id: String,
    },
    /// Run each mode's test on a node over a synthesised capture (local only)
    Check {
        /// Only this mode (from `modes list`)
        id: Option<String>,
        /// A mode without a test, or whose program is missing, fails instead of being skipped
        #[arg(long)]
        strict: bool,
    },
}

/// `--family`, spelled as the operation set spells it (`amateur_digital`).
fn parse_family(s: &str) -> Result<Family, String> {
    serde_json::from_value(s.into()).map_err(|e| e.to_string())
}

/// `--state`: `ready`, `broken` or `unavailable`.
fn parse_mode_state(s: &str) -> Result<ModeState, String> {
    serde_json::from_value(s.into()).map_err(|e| e.to_string())
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
    /// Let the agent stop jobs (`job_stop`)
    #[arg(long)]
    pub allow_stop: bool,
    /// Let the agent stop the holders a conflict names (a non-empty `replace`)
    #[arg(long)]
    pub allow_replace: bool,
    /// Let the agent delete captures (`capture_delete`)
    #[arg(long)]
    pub allow_delete: bool,
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
    /// List the sources that could take a band now (free) and those held, pinned or
    /// faulted (busy)
    Free {
        /// Centre frequency, Hz
        #[arg(long, value_name = "HZ")]
        center_hz: u64,
        /// Bandwidth, Hz
        #[arg(long, value_name = "HZ")]
        bandwidth_hz: u64,
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
    /// Start a job of a built-in kind or a mode on a source
    Start {
        /// Job kind: a built-in kind (passthrough, spectrum, fm_demod, wfm_demod,
        /// am_demod, ssb_demod, measure, scan, or a decoder) or a mode id (`modes list`)
        #[arg(long, value_name = "KIND", value_parser = parse_job_kind)]
        kind: JobKind,
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
        /// After starting, tail the job's audio output (else its first event) stream, stop
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
    /// Move a running job to another band under the same id (its attached jobs follow)
    Retune {
        /// The running job to move (an id from `job list`)
        #[arg(long, value_name = "ID")]
        job: String,
        /// New centre frequency in Hz
        #[arg(long, value_name = "HZ")]
        center_hz: u64,
        /// New bandwidth in Hz
        #[arg(long, value_name = "HZ")]
        bandwidth_hz: u64,
        #[command(flatten)]
        replace: ReplaceArg,
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
    /// Start a job from a JSON graph whose first node takes no input (capture_in,
    /// stream_in, mix): no radio, no lease
    Start {
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

/// `job start --kind`: a built-in kind's name, or any mode id (`^[a-z][a-z0-9_]*$`; the
/// node says when it has no such row).
fn parse_job_kind(s: &str) -> Result<JobKind, String> {
    JobKind::from_name(s).ok_or_else(|| {
        let builtins: Vec<&str> = zeta_core::ops::BUILTIN_KINDS
            .iter()
            .filter(|k| k.startable())
            .map(JobKind::name)
            .collect();
        format!(
            "expected a built-in kind ({}) or a mode id matching ^[a-z][a-z0-9_]*$",
            builtins.join(", ")
        )
    })
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
            ) | Command::Job(
                JobCmd::Start { .. }
                    | JobCmd::Attach { .. }
                    | JobCmd::Retune { .. }
                    | JobCmd::Stop(_)
            ) | Command::Graph(GraphCmd::Put { .. } | GraphCmd::Start { .. })
                | Command::Plan(PlanCmd::Start { .. })
                | Command::Mix(MixCmd::Start { .. })
                | Command::Bits(BitsCmd::Start { .. })
                | Command::Station(StationCmd::Set { .. } | StationCmd::Clear)
                | Command::Capture(CaptureCmd::Delete { .. })
                | Command::Jobs(JobsCmd::Stop { .. })
        )
    }

    /// `plans --plan ID`: the plan the printed result is narrowed to.
    pub fn plan_filter(&self) -> Option<String> {
        match self {
            Command::Plans { plan } => plan.clone(),
            _ => None,
        }
    }

    /// `--then-tail` of a `job start` or a `bits start`, if given.
    pub fn then_tail(&self) -> Option<ThenTail> {
        match self {
            Command::Job(JobCmd::Start { then_tail, .. })
            | Command::Bits(BitsCmd::Start { then_tail, .. }) => *then_tail,
            _ => None,
        }
    }

    /// `job start --keep`, `bits start --keep`: leave a `--then-tail` job running.
    pub fn keep(&self) -> bool {
        matches!(
            self,
            Command::Job(JobCmd::Start { keep: true, .. })
                | Command::Bits(BitsCmd::Start { keep: true, .. })
        )
    }

    /// The operation this subcommand applies; `None` for `serve`. Fails only
    /// when `graph put` or `graph start` cannot read or parse its file.
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
            Command::Source(SourceCmd::Free {
                center_hz,
                bandwidth_hz,
            }) => Op::SourceFree {
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(bandwidth_hz),
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
                kind,
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
            Command::Job(JobCmd::Retune {
                job,
                center_hz,
                bandwidth_hz,
                replace,
            }) => Op::JobRetune {
                job: JobId(job),
                center_hz: Hz(center_hz),
                bandwidth_hz: Hz(bandwidth_hz),
                replace: replace.into_ids(),
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
            Command::Graph(GraphCmd::Start { file }) => Op::GraphStart {
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
            Command::Plans { plan } => {
                if let Some(id) = plan
                    && !zeta_core::plans::PLANS.iter().any(|p| p.id == id)
                {
                    let known: Vec<&str> = zeta_core::plans::PLANS.iter().map(|p| p.id).collect();
                    return Err(OpError::Other {
                        message: format!("unknown plan {id} (known: {})", known.join(", ")),
                    });
                }
                Op::ChannelPlans {}
            }
            Command::Jobs(JobsCmd::Stop { source, kind, plan }) => Op::JobsStop {
                source: source.map(SourceId),
                kind,
                plan,
            },
            Command::Plan(PlanCmd::Start {
                plan,
                channels,
                kind,
                source,
                deviation_hz,
                sideband,
                attach,
                language,
                replace,
            }) => Op::PlanStart {
                plan,
                channels: channels.map(|c| c.0),
                kind,
                source: SourceId(source),
                deviation_hz,
                sideband: sideband.map(Into::into),
                attach: attach.map(Into::into),
                language,
                replace: replace.into_ids(),
            },
            Command::Mix(MixCmd::Start { sources }) => Op::MixStart {
                sources: sources.map(|s| s.into_iter().map(SourceId).collect()),
            },
            Command::Bits(BitsCmd::Start {
                capture,
                modulation,
                symbol_rate_baud,
                order,
                ..
            }) => Op::BitsStart {
                capture,
                modulation: modulation.map(Modulation::from),
                symbol_rate_baud: symbol_rate_baud.map(Baud),
                order,
            },
            Command::Station(StationCmd::Set {
                lat,
                lon,
                alt_m,
                region,
                antenna,
            }) => Op::StationSet {
                lat,
                lon,
                alt_m,
                region,
                antennas: (!antenna.is_empty()).then(|| {
                    let mut all = Station::default_antennas(alt_m);
                    all.extend(antenna);
                    number_antennas(all)
                }),
            },
            Command::Capture(CaptureCmd::List {
                center_hz,
                bandwidth_hz,
            }) => Op::CaptureList {
                signal: center_hz.zip(bandwidth_hz).map(|(c, b)| SignalBand {
                    center_hz: Hz(c),
                    bandwidth_hz: Hz(b),
                }),
            },
            Command::Capture(CaptureCmd::Show { capture }) => Op::CaptureShow { capture },
            Command::Capture(CaptureCmd::Classify { capture }) => Op::CaptureClassify { capture },
            Command::Capture(CaptureCmd::Delete { capture }) => Op::CaptureDelete { capture },
            Command::Station(StationCmd::Get) => Op::StationGet {},
            Command::Station(StationCmd::Clear) => Op::StationClear {},
            Command::Modes(ModesCmd::List { family, state }) => Op::ModesList { family, state },
            Command::Modes(ModesCmd::Show { id }) => Op::ModesShow { id },
            Command::Classify(s) => Op::Classify {
                selection: s.into(),
            },
            Command::Schema => Op::Schema {},
            Command::AgentsMd { .. }
            | Command::Serve(_)
            | Command::Ask(_)
            | Command::Soapy(_)
            | Command::SoapyWorker(_)
            | Command::Ops(_)
            | Command::Modes(ModesCmd::Check { .. })
            | Command::Transcripts(_) => {
                return Ok(None);
            }
        }))
    }
}

/// Keep only plan `id` in a `channel_plans` result.
pub fn narrow_plans(result: &mut zeta_core::ops::OpResult, id: Option<&str>) {
    if let (Some(id), zeta_core::ops::OpResult::ChannelPlans { plans }) = (id, result) {
        plans.retain(|p| p.id == id);
    }
}

fn read_graph(path: &std::path::Path) -> Result<Graph, OpError> {
    let other = |e: &dyn std::fmt::Display| OpError::Other {
        message: format!("{}: {e}", path.display()),
    };
    let text = std::fs::read_to_string(path).map_err(|e| other(&e))?;
    serde_json::from_str(&text).map_err(|e| other(&e))
}

#[cfg(test)]
mod channel_tests {
    use super::*;

    #[test]
    fn parse_channels_sorts_dedups_and_refuses_bad_input() {
        assert_eq!(parse_channels("1,2,5-8").unwrap().0, [1, 2, 5, 6, 7, 8]);
        assert_eq!(parse_channels("2,1-2,1").unwrap().0, [1, 2]);
        for bad in ["1,,2", "8-5", "a", "5-", "-5", "1-5000"] {
            let e = parse_channels(bad).expect_err(bad);
            assert!(e.contains("channels"), "{bad}: {e}");
        }
    }
}

#[cfg(test)]
mod cli_graph_tests {
    use super::*;

    #[test]
    fn cli_graph_start_reads_the_graph_from_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("g.json");
        let g = serde_json::json!({
            "id": "g",
            "nodes": [{"id": "src", "kind": "capture_in", "params": {"captures": ["01A"]}}],
            "edges": [],
        });
        std::fs::write(&file, g.to_string()).unwrap();
        let cli = Cli::try_parse_from(["zeta", "graph", "start", "--file", file.to_str().unwrap()])
            .unwrap();
        let want: Graph = serde_json::from_value(g).unwrap();
        match cli.command.into_op().unwrap() {
            Some(Op::GraphStart { graph }) => assert_eq!(graph, want),
            op => panic!("graph start: {op:?}"),
        }
        let missing =
            Cli::try_parse_from(["zeta", "graph", "start", "--file", "/nonexistent/g.json"])
                .unwrap();
        assert!(missing.command.into_op().is_err());
    }
}

#[cfg(test)]
mod cli_bits_tests {
    use super::*;

    #[test]
    fn cli_bits_start_takes_the_nullable_fields_and_the_tail() {
        let cli = Cli::try_parse_from([
            "zeta",
            "bits",
            "start",
            "--capture",
            "X",
            "--modulation",
            "psk",
            "--order",
            "4",
            "--then-tail",
            "2,3000",
            "--keep",
        ])
        .unwrap();
        assert_eq!(
            cli.command.then_tail(),
            Some(ThenTail {
                max_frames: 2,
                timeout_ms: 3000
            })
        );
        assert!(cli.command.keep());
        assert!(cli.command.mutates());
        match cli.command.into_op().unwrap() {
            Some(Op::BitsStart {
                capture,
                modulation,
                symbol_rate_baud,
                order,
            }) => {
                assert_eq!(capture, "X");
                assert_eq!(modulation, Some(Modulation::Psk));
                assert_eq!(symbol_rate_baud, None);
                assert_eq!(order, Some(4));
            }
            op => panic!("bits start: {op:?}"),
        }
    }
}
