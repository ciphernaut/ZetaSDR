//! The operation set: the one control vocabulary every caller (CLI, MCP, GUI,
//! voice, a small local model) drives the node through. Strict on input
//! (unknown fields rejected, every field required, one unit per field) and
//! corrective on error (errors name the field and range and propose a value).

use std::path::PathBuf;

use num_complex::Complex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use crate::bits::Modulation;
use crate::capture::{CaptureMeta, write_cf32};
use crate::classify::{Band, Classification, Label, Tried};
use crate::graph::{Edge, Graph, GraphError, GraphId, Node as GraphNode, NodeId};
use crate::job::{JobRecord, JobState};
use crate::knowledge::{Allocation, DEFAULT_REGION, Signature, nearest_allocations};
use crate::lease::LeaseError;
use crate::modes::{
    Band as ModeBand, DecoderKind, Family, LoadedMode, ModeId, ModeRegistry, ModeRow, ModeState,
    Subject, is_mode_id, template,
};
use crate::node::{AfterGate, Node, NodeError};
use crate::port::{GainInfo, Port, PortError, PortStats, Tuning, WorkerInfo};
use crate::ring::RingError;
use crate::station::Station;
use crate::stream::{Frame, Payload, StreamInfo};
use crate::types::{
    Baud, Capabilities, ChannelId, Hz, JobId, Selection, SourceId, StreamId, StreamKind, Timestamp,
};

/// A band as `capture_list` narrows by: a capture matches when its own band overlaps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct SignalBand {
    pub center_hz: Hz,
    pub bandwidth_hz: Hz,
}

/// One request to the node.
///
/// Variants without fields are written `Variant {}` so serde enforces
/// `deny_unknown_fields` on them too (it ignores extra keys on internally
/// tagged unit variants); the wire form is just `{"op": "..."}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum Op {
    /// List the sources: id, capabilities, `current` band, `holders` (the
    /// rigid lease holders' job ids), `elastic` (the jobs following the
    /// capture, see `job_start`), `pin` (the band `source_pin` holds the source
    /// at, `null` for none), `fault`, the source's unrecoverable failure (`null` when
    /// healthy; a healed fault stays until recovery has seen the jobs end, normally up to
    /// one poll after the worker's hello, and a `job_start` in that hold is accepted, its
    /// job possibly failing and being relaunched under its id), `bank`, the channel bank shared by the capture's channels
    /// (`bin_hz`, `consumers`, `dropped`, the input blocks it dropped because it could
    /// not keep up; `null` for none), `gain`, the overall receive
    /// gain (`db`, `null` while automatic; `min_db`, `max_db`, `automatic`,
    /// `automatic_available`; `null` for a source without adjustable gain), and
    /// `stats`, the stream's counters since it opened (`measured_rate_hz`, the
    /// rate delivered over the last 10 s, `null` before any; `overflows`,
    /// `timeouts`, `restarts`, streams reopened because their samples ran ahead
    /// of the clock; `null` for a source that keeps none), and `worker`, the child
    /// process the source runs in (`pid`, `restarts`, `since_ns`; `null` for a
    /// source that runs in the node), and `ring`, the IQ history (`seconds`, `null`
    /// before the first block; a `classify` or `iq_read` span older than it fails
    /// `too_old`). Jobs on a faulted source fail with
    /// `source ID: FAULT` as their reason; a successful tune (`source_ensure`,
    /// `job_start`) clears it. A channelised job's `job_context` holds `channel.path`:
    /// `bank` (with `bin_hz`) or `direct` (with `reason`, why not the bank).
    SourceList {},
    /// Check a band against the source and tune it if no job holds it. Takes
    /// no lease: an unheld source is tuned directly (a later `job_start` may
    /// retune it); a held source is never retuned, and the request either
    /// fits the current band (`Tuning`) or is a `Conflict`. Runs under the
    /// lease manager's per-source gate, so it never interleaves with a job's
    /// lease-and-tune. Elastic jobs do not hold the source: a tune relaunches
    /// them on the new capture.
    SourceEnsure {
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        /// Jobs holding the source that may be stopped to make room (`[]` for
        /// none); see `job_start`.
        replace: Vec<JobId>,
    },
    /// Pin the source at a band: a rigid lease held by no job (`source_list`
    /// shows it as `pin`, never among `holders`), granted like a rigid
    /// `job_start`: inside the capture other rigid jobs hold it is leased without
    /// a tune, elsewhere it tunes when only elastic jobs hold the source (they are
    /// relaunched on the new capture), else it is a `conflict`. Replaces any pin
    /// the source had (a conflict keeps the old one). Persisted in the state
    /// directory and restored before any job resumes. The result is the capture.
    SourcePin {
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        /// Jobs holding the source that may be stopped to make room (`[]` for
        /// none); see `job_start`.
        replace: Vec<JobId>,
    },
    /// Release the source's pin (see `source_pin`).
    SourceUnpin {
        source: SourceId,
    },
    /// The sources that could take the band now: `free` lists, in id order, every
    /// source whose `tune_min_hz..=tune_max_hz` holds both edges of the band and whose
    /// `max_bandwidth_hz` its width, with no rigid holder, no pin and no fault (elastic
    /// jobs never make a source busy); `busy` lists every other source that covers the
    /// band with why: `holders`, `elastic`, `pin` and `fault` as `source_list` gives
    /// them. A source that cannot cover the band is in neither list. Takes no lease.
    SourceFree {
        center_hz: Hz,
        bandwidth_hz: Hz,
    },
    /// Set the source's overall receive gain live; the result is the gain after
    /// the change (`source_list` shows it as `gain`). Persisted in the state
    /// directory and re-applied when the node opens.
    SourceSetGain {
        source: SourceId,
        /// The gain in dB, within `source_list`'s `gain.min_db..=max_db`; `null`
        /// turns the driver's automatic gain on (where `gain.automatic_available`).
        #[serde(deserialize_with = "crate::port::nullable_f64")]
        #[schemars(required, schema_with = "nullable_gain_db")]
        gain_db: Option<f64>,
    },
    JobStart {
        kind: JobKind,
        source: SourceId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        /// FM peak deviation in Hz (100..=75000) for `fm_demod` and
        /// `wfm_demod`; `null` is the kind's default (5000 for `fm_demod`,
        /// 75000 for `wfm_demod`). Any other kind must pass `null`.
        #[serde(deserialize_with = "nullable_u64")]
        #[schemars(required, schema_with = "nullable_hz_u64")]
        deviation_hz: Option<u64>,
        /// Sideband for `ssb_demod`: `usb` or `lsb`; `null` is `usb`. Any
        /// other kind must pass `null`.
        #[serde(deserialize_with = "nullable_sideband")]
        #[schemars(required, schema_with = "nullable_sideband_schema")]
        sideband: Option<Sideband>,
        /// `spectrum` only (`false` for any other kind): the job follows the
        /// source's capture instead of holding it. It never conflicts, never
        /// retunes a held source (its `tuning` is the capture it joins), is
        /// relaunched under the same id on every retune, and is stopped by a scan
        /// on its source.
        elastic: bool,
        /// Jobs holding a rigid lease on `source` that may be stopped to make room
        /// (`[]` for none). Under the source's gate each is checked (`unknown`, or
        /// `other` "job <id> does not hold source <src>", nothing stopped) and
        /// stopped as by `job_stop` right before the request, so nothing can take
        /// the radio in between. A conflict with a holder not named leaves them
        /// stopped and names the rest.
        replace: Vec<JobId>,
    },
    /// Attach a job of an attachable `kind` (`transcribe`) to the running job `job`: it
    /// reads that job's audio output stream (the last one in its graph order when it
    /// has several; audio passed between two of its stages does not count) instead of
    /// the radio, holds no lease, copies the job's `source` and `tuning`, and records
    /// it as its `input`. It follows its input through a
    /// `job_retune` or a `graph_put` restart: it is relaunched under its own id on the
    /// input's new stream once the input runs again (`failed` with the reason when it
    /// cannot be). It ends `stopped` when the input job stops or fails for its own
    /// reason. On a node restart it resumes after its input, or is
    /// `failed` with `input job <id> not running` when the input does not run.
    JobAttach {
        kind: JobKind,
        job: JobId,
        /// BCP-47 language code of the speech (`en`, `de`, `pt-BR`); `null` is `en`.
        #[serde(deserialize_with = "nullable_string_value")]
        #[schemars(required, schema_with = "nullable_language")]
        language: Option<String>,
    },
    /// Retune a running job on the radio to a new band under the same id: under the
    /// source's gate its lease is moved to `center_hz`/`bandwidth_hz` (inside the current
    /// capture it is granted without a tune, whether or not another rigid job's band
    /// overlaps it; outside it the source is tuned when no other rigid job or pin holds
    /// it, else it is a `conflict` naming the holders and the job is left as it was), its
    /// graph is expanded again for the new band at the capture's rate and the job
    /// relaunched, `tuning` updated. Applies to a `running` job the node expanded and can
    /// re-expand: the four demodulators, the decoders, `measure`, `passthrough` and the
    /// mode rows. Refused naming the kind for `scan`, `watch`, `identify`, `spectrum`
    /// (elastic or rigid), `mix` (it has no band), a `graph_start` job (it runs on no
    /// radio) and `transcribe` (attached: retune its
    /// input), and for a job whose graph an operator `graph_put` replaced (it launches as stored: put a new
    /// graph or start a new job). A job that is not running is `job <id> is <state>`; an
    /// unknown id is `unknown`. The result is the job's id, as `job_start` answers.
    JobRetune {
        job: JobId,
        center_hz: Hz,
        bandwidth_hz: Hz,
        replace: Vec<JobId>,
    },
    /// Start a mix: a job that sums the audio of every other job with an audio output on
    /// `sources` and plays it as one stream, so whatever is talking is heard. It takes no
    /// lease, tunes nothing and blocks no job (its record's source is `node`, its band 0);
    /// jobs started or stopped later join and leave by themselves within a second. It
    /// outputs `audio` and `event` streams; each change of who is talking publishes
    /// `{"protocol": "mix", "active": [{"job", "source", "center_hz"}], "t_ns", "summary"}`.
    /// Attached jobs, a `watch`'s or `identify`'s own jobs and other mixes are not mixed.
    /// With a mix of the same set of sources already running the result is its id; `null`
    /// matches only `null`, so a list naming every source is another mix. It runs until
    /// stopped, and a node restart resumes it after the jobs it mixes.
    MixStart {
        /// Ids from `source_list`; `null` for every source of the node.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<Vec<SourceId>>")]
        sources: Option<Vec<SourceId>>,
    },
    /// Set the node's station: its position, region and antennas, persisted in
    /// `station.json`. A node-level record, set once; the ACMA provider's distance and
    /// reach, `sink_sondehub`'s defaults and the page's home read it. `antennas` is
    /// `null` for the defaults (a 0-30 MHz dipole and a 30 MHz-6 GHz discone, both 5 m
    /// up). Validated before anything is written: `lat` -90..=90, `lon` -180..=180,
    /// each antenna `min_hz` < `max_hz`, `azimuth_deg` 0..360, `elevation_deg` 0..=90,
    /// ids unique. The result is the station as stored.
    StationSet {
        lat: f64,
        lon: f64,
        alt_m: f64,
        /// A free-text region code such as `AU`.
        region: String,
        /// The antennas; `null` for the defaults.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(
            required,
            schema_with = "crate::types::nullable::<Vec<crate::station::Antenna>>"
        )]
        antennas: Option<Vec<crate::station::Antenna>>,
    },
    /// The station, or `null` when none is set.
    StationGet {},
    /// Forget the station (`station.json` is removed).
    StationClear {},
    /// The captures the node's burst store holds, newest first, each a record (`id`,
    /// `source`, `center_hz`, `bandwidth_hz`, `sample_rate_hz`, `start_ns`, `end_ns`,
    /// `bytes`, `features`, `signal`, `job`, `watch`, `allocations`, `pinned`). `signal`
    /// narrows the list to the captures whose band overlaps that band; `null` lists all.
    CaptureList {
        /// The band to narrow to; `null` for every capture.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<SignalBand>")]
        signal: Option<SignalBand>,
    },
    /// One capture's record and its SigMF meta (`meta`, the `.sigmf-meta` as written).
    /// The record's `meta_path` and `data_path` are the node's own files; a remote
    /// caller fetches them at `GET /captures/<id>.sigmf-meta` and `.sigmf-data`.
    CaptureShow {
        /// An id from `capture_list`.
        capture: String,
    },
    /// Run the classifiers over a capture's whole window (the stored allocations
    /// included) as `classify` does over the ring: `{labels, tried, allocations}`.
    CaptureClassify {
        /// An id from `capture_list`.
        capture: String,
    },
    /// Delete a capture and its files. A pinned capture is refused. Deleting is the
    /// operator's decision: through `ask` it needs `allow.delete_captures`.
    CaptureDelete {
        /// An id from `capture_list`.
        capture: String,
    },
    /// Start `kind` on every channel of the channel plan `plan` (`channel_plans`), or on
    /// the channels numbered in `channels`, each at its channel's `center_hz` with the
    /// plan's `bandwidth_hz`, and attach `attach` to each started job. Under the source's
    /// gate, in this order: everything is checked first (an unknown `plan` or channel is
    /// `unknown`, a channel listed twice, a kind that does not run per channel —
    /// `spectrum`, `scan`, `watch`, `identify`, `mix`, an attachable kind — and a
    /// non-attachable `attach` are `other`; nothing has happened yet); a channel where an
    /// identical rigid job already runs is `existing` and skipped; with `attach`, its stage must be
    /// ready, else the op is refused with the stage's reason; the `replace` jobs are
    /// stopped; then one lease decision for the band bounding the channels to start
    /// (lowest channel edge to highest), checked as a rigid `job_start` of that band
    /// would be: inside the capture no tune, outside it a tune when only elastic jobs
    /// hold the source, else a `conflict` naming the holders and nothing starts; a band
    /// wider than the source is `out_of_range` on `bandwidth_hz`. Each channel is then
    /// started in channel-number order inside that capture: one that fails is listed
    /// under `refused` with its error and the rest go on. With `attach`, each started
    /// job (and each `existing` one without one) gets one attached; a failure is that
    /// channel's `attach_error`, the job kept. The result: `capture`, `started`
    /// (`channel`, `center_hz`, `job`, `attached`, `attach_error`), `existing`
    /// (`channel`, `center_hz`, `job`, `attached`), `refused` (`channel`, `center_hz`,
    /// `error`) and `warnings` (strings to repeat to the operator; empty when there is
    /// nothing to say). The jobs are ordinary jobs; running the same request twice
    /// starts only what is missing.
    PlanStart {
        /// A plan id from `channel_plans`.
        plan: String,
        /// Channel numbers of the plan; `null` for every channel.
        #[serde(deserialize_with = "nullable_u32_list")]
        #[schemars(required, schema_with = "nullable_channels_schema")]
        channels: Option<Vec<u32>>,
        kind: JobKind,
        source: SourceId,
        /// As on `job_start`, passed to every start.
        #[serde(deserialize_with = "nullable_u64")]
        #[schemars(required, schema_with = "nullable_hz_u64")]
        deviation_hz: Option<u64>,
        /// As on `job_start`, passed to every start.
        #[serde(deserialize_with = "nullable_sideband")]
        #[schemars(required, schema_with = "nullable_sideband_schema")]
        sideband: Option<Sideband>,
        /// An attachable kind (`transcribe`) to attach to every started job; `null` for none.
        #[serde(deserialize_with = "nullable_kind")]
        #[schemars(required, schema_with = "nullable_attach_schema")]
        attach: Option<JobKind>,
        /// `job_attach`'s language, `null` for `en`; must be `null` when `attach` is.
        #[serde(deserialize_with = "nullable_string_value")]
        #[schemars(required, schema_with = "nullable_language")]
        language: Option<String>,
        /// As on `job_start`: rigid holders of `source` that may be stopped to make room.
        replace: Vec<JobId>,
    },
    JobList {},
    JobStop {
        job: JobId,
    },
    /// Stop every running job that matches all the filters given, in one operation:
    /// `source`, `kind` and `plan` (`null` for any; all three `null` is every job). A
    /// job's filter on `plan` is its centre lying on one of that plan's channels; a job on
    /// no radio (`graph_start`, `bits_start`, a mix) never matches a `plan`. Elastic
    /// jobs (the page's spectrum) and jobs another job owns (a `watch`'s or an
    /// `identify`'s trial jobs) are never named: they follow the capture or their owner.
    /// Jobs on a radio are stopped first, then those attached to them; an attached job
    /// that ended with its input counts as stopped. The result lists `stopped` (ids) and
    /// `failed` (`job`, `error`; the others go on). No match is an empty `stopped`, not
    /// an error.
    JobsStop {
        /// An id from `source_list`; `null` for every source.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<SourceId>")]
        source: Option<SourceId>,
        /// A job kind; `null` for every kind.
        #[serde(deserialize_with = "nullable_kind")]
        #[schemars(required, schema_with = "nullable_kind_filter_schema")]
        kind: Option<JobKind>,
        /// A plan id from `channel_plans`; `null` for any band.
        #[serde(deserialize_with = "nullable_string_value")]
        #[schemars(required, schema_with = "crate::types::nullable::<String>")]
        plan: Option<String>,
    },
    JobContext {
        job: JobId,
    },
    /// Save a selection under `state_dir/captures`: writes the whole ring
    /// bandwidth over the span as a SigMF pair; the selection band is recorded
    /// as an annotation. The capture is labelled with the ring's centre, and
    /// the selection band must lie inside the ring's band.
    IqRead {
        selection: Selection,
    },
    /// List the streams: id, kind and provenance. One is always the node's
    /// own `ops` event stream (provenance source `node` and no `job`; a mix's
    /// streams have source `node` too, with their `job`): one event per
    /// applied operation with its outcome and caller (`transport`: `local`,
    /// `http`, `ui` or `agent`), logged after the operation completes.
    StreamList {},
    /// Collect frames from a stream until `max_frames` (1..=1000) arrive or
    /// `timeout_ms` (1..=10000) passes, whichever is first; frames published
    /// before the call are not seen. Frames skipped because the caller fell
    /// behind are dropped and counted in the result's `missed`.
    StreamTail {
        stream: StreamId,
        max_frames: u32,
        timeout_ms: u32,
    },
    GraphGet {
        job: JobId,
    },
    /// Replace a job's graph; restarts a running job under the same id with
    /// the new graph.
    GraphPut {
        job: JobId,
        graph: Graph,
    },
    /// Start a job from a graph whose first node has no inputs (`capture_in`,
    /// `stream_in`, `mix`): the record's kind is `graph`, its source `node`, its band
    /// the first `capture_in`'s first capture's (0/0 without one, so the page can place
    /// it), and it takes no lease and tunes nothing. A first node that takes input, and a
    /// node with an unfed input (no edge into it), are refused (`other`, naming the
    /// node); a `capture_in` whose capture is not in the store is `unknown` naming
    /// `captures[<i>]`. It launches the graph as stored (as after a `graph_put`); it is
    /// persisted and resumed like any job, after the jobs on a radio, and a `capture_in`
    /// job whose capture was expired since fails `capture <id> not found`. It has no band
    /// to retune. The result is the job's id, as `job_start` answers.
    GraphStart {
        graph: Graph,
    },
    /// Work a capture's bits: takes every capture of the named capture's `set`, in time
    /// order, and starts `capture_in → resample → <demod> → clock_recover → slicer →
    /// bit_view` from the named capture's feature record, each `null` taken from the
    /// features (`modulation`, one of `fsk`, `psk` and `ook`, from the layer-1 label:
    /// `fsk` and `fm` give `fsk`, `psk` and `ook` themselves; `symbol_rate_baud` and
    /// `order` from the record, `order` 2 when it has none; fsk takes 2 or 4, psk 2, 4
    /// or 8, ook 2). The capture is resampled to about 8 samples per symbol for fsk and ook, and
    /// for psk to the smallest multiple of 4 times the baud that also covers a carrier offset
    /// of 2000 Hz (the coarse estimate is unambiguous only within the rate over twice the
    /// order), moved to a rate the resampler serves exactly. A signal off the capture's
    /// centre whose recorded band reaches past the resampler's passband (its offset plus
    /// half its `bandwidth_hz` over 0.45 times that rate) is shifted to its recorded
    /// `center_hz` first, unless layer 1 labels the capture `noise`: `capture_in → channel [→ resample] → <demod> → ...`, the
    /// `channel` stage at the measured centre with the measured bandwidth × 1.25 (at least
    /// 4 × the baud, at most what the capture holds about that centre: its rate less twice
    /// the shift) and an output rate the capture rate over a whole divisor (the
    /// resampler follows unless that is already the chain rate). 4-fsk and ook take the
    /// `early_late` clock; fsk's demodulator is given the record's measured `tone_spacing_hz` (it tracks
    /// the tones itself when the record has none); `capture_in` loops, paced, with a 500 ms
    /// pause between
    /// passes, and the job runs until stopped. Refused with `other`: a field neither the
    /// request nor the features give (`no symbol rate measured; give symbol_rate_baud`,
    /// `no modulation measured (label <x>); give modulation`), a rate the capture cannot
    /// hold (`symbol_rate_baud <r> needs <n> Hz; the capture holds <rate> Hz`, 4 samples
    /// per symbol at least; a rate of 0 is `symbol_rate_baud: at least 1`), a measured
    /// centre the capture holds under 4 × the baud about (`the measured centre <c> Hz
    /// leaves <room> Hz of the capture <lo>..=<hi> Hz about it, under the <floor> Hz the
    /// chain needs (4 x <baud> Bd)`), and a set whose captures differ in rate or centre
    /// (`capture <id>: <rate> Hz differs from <first>'s <rate> Hz`); an unknown `capture`
    /// is `unknown`. The record's kind is `bits`, its source `node`, its band the
    /// capture's, `job_context.bits` the plan (`captures`, `modulation`,
    /// `symbol_rate_baud`, `order`, `tone_spacing_hz`, `rate_hz`, `center_hz` the centre the
    /// chain runs on and `shift_hz` its offset from the capture's, `bandwidth_hz`,
    /// `padded_bandwidth_hz` the channel band uncut and `clamped` whether it was cut to the
    /// capture, and `channel_rate_hz`, `null` (and `padded_bandwidth_hz` `null`) when the
    /// chain does not shift); the slicer's `bits` events are on
    /// `job_context.slicer.stream` and `bit_view`'s latest hypotheses in
    /// `job_context.bit_view`. Change a running chain with `graph_get`, edit,
    /// `graph_put`. The result is the job's id, as `job_start` answers; one-shot
    /// `--then-tail` tails the `bits` events. The agent's own `bits_start` jobs count as
    /// jobs it started.
    BitsStart {
        /// An id from `capture_list`.
        capture: String,
        /// The demodulator; `null` for the layer-1 label's.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<Modulation>")]
        modulation: Option<Modulation>,
        /// The symbol rate; `null` for the feature record's.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<Baud>")]
        symbol_rate_baud: Option<Baud>,
        /// The modulation order (fsk 2 or 4, psk 2, 4 or 8, ook 2); `null` for the
        /// feature record's, else 2.
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::types::nullable::<u8>")]
        order: Option<u8>,
    },
    /// Band-plan allocations in `region` and known signatures overlapping the
    /// band, from every registered knowledge provider.
    KnowledgeLookup {
        region: String,
        center_hz: Hz,
        bandwidth_hz: Hz,
    },
    /// The channel plans the node knows, each with its channels expanded
    /// and sorted by frequency.
    ChannelPlans {},
    /// The decoder modes the node knows, one row each (`modes/*.json` built in, then the
    /// operator's `--modes-dir`), sorted by id, narrowed to `family` and `state` when
    /// given (`null` for any).
    ModesList {
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<Family>")]
        family: Option<Family>,
        #[serde(deserialize_with = "crate::modes::nullable")]
        #[schemars(required, schema_with = "crate::modes::nullable_enum::<ModeState>")]
        state: Option<ModeState>,
    },
    /// One mode's row as loaded, with its `state` and `reason`.
    ModesShow {
        id: String,
    },
    /// Run every registered classifier over the selection's band and span of
    /// the source ring (no upstream features).
    Classify {
        selection: Selection,
    },
    Schema {},
}

/// Deserialize a required `Option<u64>`: without `#[serde(default)]` a missing
/// key is a "missing field" error, while an explicit `null` is `None`.
fn nullable_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    <Option<u64> as Deserialize>::deserialize(d)
}

/// Schema of a required `Option<u64>`: `["integer","null"]`. (`required` alone
/// would drop the `null`.)
fn nullable_hz_u64(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"],
        "format": "uint64",
        "minimum": 0,
        "description": "FM peak deviation in Hz (100..=75000) for `fm_demod` and `wfm_demod`; `null` is the kind's default (5000 for `fm_demod`, 75000 for `wfm_demod`). Any other kind must pass `null`."
    })
}

/// Deserialize a required `Option<Sideband>` (see [`nullable_u64`]).
fn nullable_sideband<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Sideband>, D::Error> {
    <Option<Sideband> as Deserialize>::deserialize(d)
}

/// Schema of a required `Option<Sideband>`: `["string","null"]`, enum
/// `usb`, `lsb` or `null`.
fn nullable_sideband_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "enum": ["usb", "lsb", null],
        "description": "Sideband for `ssb_demod`: `usb` or `lsb`; `null` is `usb`. Any other kind must pass `null`."
    })
}

/// Deserialize a required `Option<Vec<u32>>` (see [`nullable_u64`]).
fn nullable_u32_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Vec<u32>>, D::Error> {
    <Option<Vec<u32>> as Deserialize>::deserialize(d)
}

/// Schema of `plan_start`'s `channels`: `["array","null"]` of channel numbers.
fn nullable_channels_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["array", "null"],
        "items": {"type": "integer", "format": "uint32", "minimum": 0},
        "description": "Channel numbers of the plan; `null` for every channel."
    })
}

/// Deserialize a required `Option<JobKind>` (see [`nullable_u64`]).
fn nullable_kind<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<JobKind>, D::Error> {
    <Option<JobKind> as Deserialize>::deserialize(d)
}

/// Schema of `jobs_stop`'s `kind`: any job kind's name, or `null`.
fn nullable_kind_filter_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "A job kind; `null` for every kind."
    })
}

/// Schema of `plan_start`'s `attach`: `["string","null"]`, enum the attachable kinds
/// or `null`.
fn nullable_attach_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut kinds: Vec<serde_json::Value> = BUILTIN_KINDS
        .iter()
        .filter(|k| k.attachable())
        .map(|k| k.name().into())
        .collect();
    kinds.push(serde_json::Value::Null);
    schemars::json_schema!({
        "type": ["string", "null"],
        "enum": kinds,
        "description": "An attachable kind (`transcribe`) to attach to every started job; `null` for none."
    })
}

/// An SSB sideband: upper (`usb`) or lower (`lsb`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sideband {
    Usb,
    Lsb,
}

/// Deserialize a required `Option<String>` (see [`nullable_u64`]).
fn nullable_string_value<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    <Option<String> as Deserialize>::deserialize(d)
}

/// Schema of `source_set_gain`'s `gain_db`: `["number","null"]`.
fn nullable_gain_db(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["number", "null"],
        "description": "The gain in dB, within `source_list`'s `gain.min_db..=max_db`; `null` turns the driver's automatic gain on (where `gain.automatic_available`)."
    })
}

/// Schema of `job_attach`'s `language`: `["string","null"]`.
fn nullable_language(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "BCP-47 language code of the speech (`en`, `de`, `pt-BR`); `null` is `en`."
    })
}

/// The language a `job_attach` passing `null` transcribes.
pub const ATTACH_LANGUAGE_DEFAULT: &str = "en";

/// Whether `code` is a BCP-47 tag as `job_attach` takes it:
/// `^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$`.
pub fn is_bcp47(code: &str) -> bool {
    let mut parts = code.split('-');
    let primary = parts.next().unwrap_or("");
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `ssb_demod` audio bandwidth, Hz, used by the `ssb_demod` job kind.
pub const SSB_AUDIO_BANDWIDTH_HZ: u64 = 2_800;

/// A result variant's `value` requires every property, its `#[serde(default)]` ones
/// included: they are always written, and defaulted only for an older node's answer.
fn require_all_value(schema: &mut schemars::Schema) {
    let value = schema
        .get_mut("properties")
        .and_then(|p| p.get_mut("value"))
        .and_then(|v| <&mut schemars::Schema>::try_from(v).ok());
    if let Some(value) = value {
        crate::knowledge::require_all(value);
    }
}

/// Schema of a required `Option<String>`: `["string","null"]`.
fn nullable_string(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "The port's unrecoverable failure (`Port::fault`); `null` when healthy. Jobs on a faulted source fail with it; a successful tune clears it."
    })
}

/// `fm_demod` peak deviation when a job start passes `null`.
pub const FM_DEVIATION_DEFAULT_HZ: u64 = 5_000;
/// `wfm_demod` peak deviation when a job start passes `null` (broadcast FM).
pub const WFM_DEVIATION_DEFAULT_HZ: u64 = 75_000;
/// `wfm_demod` de-emphasis time constant, µs (Australia and Europe).
pub const WFM_DEEMPHASIS_US: u64 = 50;
const FM_DEVIATION_MIN_HZ: u64 = 100;
const FM_DEVIATION_MAX_HZ: u64 = 75_000;

/// Built-in job kinds; each expands to a graph. `fm_demod`, `wfm_demod` and
/// `aprs_decode`, `pocsag_decode`, `rds_decode`, `acars_decode` and `adsb_decode` channelise the job band at the narrowest exact divisor of the source rate
/// that is at least 48 kHz and at least the band; `spectrum` covers the whole
/// capture band. `measure` channelises the job band the same way and
/// measures only that band, reporting its absolute `center_hz`. `scan`
/// time-shares the radio across a band wider than one capture: it expands to
/// one `fft` node whose mask params the node fills in; the node steps the source across the band (see [`scan_steps`]),
/// dwelling [`SCAN_DWELL_MS`] on each step, and emits spectrum rows tagged
/// with each step's `center_hz` while holding the whole band, so every other
/// request on the source is a conflict until the scan is stopped.
/// `identify` also expands to no graph: it holds a rigid lease on its band like a
/// decoder, measures it for [`IDENTIFY_MEASURE_MS`], then tries every plausible mode
/// row as a child job, in score order, publishing one `candidate` event per row and
/// ending with a verdict (see `node::identify`).
/// `watch` holds a rigid lease on its band and expands to `fft → detect` over the
/// capture (the `fft` sized as `spectrum`'s); the node runs a controller beside it that
/// identifies each signal `detect` announces and keeps the proved decoder running
/// (see `node::watch`).
/// `transcribe` is attachable, not started on the radio: `job_attach` runs it on another
/// job's audio (see [`JobKind::expand_attached`]); `job_start` refuses it.
/// `capture` channelises its band as `measure` does and expands to `channel →
/// burst_capture` ([`BURST_CAPTURE_STAGE`]): the stage writes the band's bursts to the
/// node's burst store and returns, so the job ends `stopped` with `job_context.capture`.
///
/// The decoder kinds (`aprs_decode`, `pocsag_decode`, `rds_decode`, `acars_decode`,
/// `adsb_decode`) and [`JobKind::Mode`] expand from their mode rows (see
/// [`crate::modes`]); `adsb_decode`'s row refuses a band under 2 MHz (dump1090 needs most
/// of its 2.4 MHz). On the wire a kind is its name: a built-in one (see [`BUILTIN_KINDS`])
/// or any other id matching `^[a-z][a-z0-9_]*$`, which is a `Mode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobKind {
    Passthrough,
    Spectrum,
    FmDemod,
    WfmDemod,
    AmDemod,
    SsbDemod,
    AprsDecode,
    PocsagDecode,
    RdsDecode,
    AcarsDecode,
    AdsbDecode,
    Measure,
    Scan,
    Identify,
    Watch,
    Transcribe,
    Mix,
    Capture,
    /// A mode row's id that is not a built-in kind's name (an operator's row).
    Mode(ModeId),
}

/// The built-in kinds, in the order `schema` lists them: every kind but [`JobKind::Mode`].
pub static BUILTIN_KINDS: [JobKind; 18] = [
    JobKind::Passthrough,
    JobKind::Spectrum,
    JobKind::FmDemod,
    JobKind::WfmDemod,
    JobKind::AmDemod,
    JobKind::SsbDemod,
    JobKind::AprsDecode,
    JobKind::PocsagDecode,
    JobKind::RdsDecode,
    JobKind::AcarsDecode,
    JobKind::AdsbDecode,
    JobKind::Measure,
    JobKind::Scan,
    JobKind::Identify,
    JobKind::Watch,
    JobKind::Transcribe,
    JobKind::Mix,
    JobKind::Capture,
];

impl Serialize for JobKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for JobKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = std::borrow::Cow::<str>::deserialize(d)?;
        JobKind::from_name(&name).ok_or_else(|| {
            let builtins: Vec<&str> = BUILTIN_KINDS.iter().map(JobKind::name).collect();
            serde::de::Error::custom(format!(
                "unknown job kind {name:?}: expected one of {} or a mode id matching ^[a-z][a-z0-9_]*$",
                builtins.join(", ")
            ))
        })
    }
}

/// The built-in names only; [`schema`] adds the embedded mode rows' ids.
impl JsonSchema for JobKind {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JobKind".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let names: Vec<&str> = BUILTIN_KINDS.iter().map(JobKind::name).collect();
        schemars::json_schema!({ "type": "string", "enum": names })
    }
}

/// The graph of a mix of `sources` (`None` for every source): one `mix` node, nothing
/// muted, no edges. A list is sorted and deduplicated, so a set has one spelling.
pub fn mix_graph(sources: Option<&[SourceId]>) -> Graph {
    let sources = sources.map(|s| normal_sources(s.to_vec()));
    Graph {
        id: GraphId::new(),
        nodes: vec![GraphNode {
            id: NodeId::new(),
            kind: "mix".into(),
            params: serde_json::json!({"sources": sources, "muted": []}),
        }],
        edges: vec![],
    }
}

fn normal_sources(mut sources: Vec<SourceId>) -> Vec<SourceId> {
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    sources.dedup();
    sources
}

/// The set of sources `graph`'s `mix` node names, spelled as [`mix_graph`] spells it:
/// `Some(None)` for every source, `None` for a graph without a readable one.
pub(crate) fn mix_graph_sources(graph: &Graph) -> Option<Option<Vec<SourceId>>> {
    let node = graph.nodes.iter().find(|n| n.kind == "mix")?;
    let sources: Option<Vec<SourceId>> =
        serde_json::from_value(node.params.get("sources")?.clone()).ok()?;
    Some(sources.map(normal_sources))
}

impl JobKind {
    /// The kind's wire name, also recorded as the job's `kind`.
    pub fn name(&self) -> &str {
        match self {
            JobKind::Passthrough => "passthrough",
            JobKind::Spectrum => "spectrum",
            JobKind::FmDemod => "fm_demod",
            JobKind::WfmDemod => "wfm_demod",
            JobKind::AmDemod => "am_demod",
            JobKind::SsbDemod => "ssb_demod",
            JobKind::AprsDecode => "aprs_decode",
            JobKind::PocsagDecode => "pocsag_decode",
            JobKind::RdsDecode => "rds_decode",
            JobKind::AcarsDecode => "acars_decode",
            JobKind::AdsbDecode => "adsb_decode",
            JobKind::Measure => "measure",
            JobKind::Scan => "scan",
            JobKind::Identify => "identify",
            JobKind::Watch => "watch",
            JobKind::Transcribe => "transcribe",
            JobKind::Mix => "mix",
            JobKind::Capture => "capture",
            JobKind::Mode(id) => &id.0,
        }
    }

    /// The id of the mode row the kind expands from: a decoder kind's own name, a
    /// `Mode`'s id; `None` for the other built-in kinds.
    fn mode_id(&self) -> Option<&str> {
        match self {
            JobKind::AprsDecode
            | JobKind::PocsagDecode
            | JobKind::RdsDecode
            | JobKind::AcarsDecode
            | JobKind::AdsbDecode => Some(self.name()),
            JobKind::Mode(id) => Some(&id.0),
            _ => None,
        }
    }

    /// The ready row of `modes` the kind expands from (`None` for a kind without one).
    /// A missing row is `unknown { field: "kind", value: <id>, known }`, `known` the
    /// startable built-in kinds and every row id (ready or not); a row that is not ready
    /// is `other("mode <id> is <state>: <reason>")`.
    pub fn mode_row<'a>(&self, modes: &'a ModeRegistry) -> Result<Option<&'a ModeRow>, OpError> {
        let Some(id) = self.mode_id() else {
            return Ok(None);
        };
        let m = modes.get(id).ok_or_else(|| {
            let mut known: Vec<String> = BUILTIN_KINDS
                .iter()
                .filter(|k| k.startable())
                .map(|k| k.name().to_string())
                .collect();
            for row in modes.ids() {
                if !known.contains(&row) {
                    known.push(row);
                }
            }
            OpError::Unknown {
                field: "kind".into(),
                value: id.to_string(),
                known,
            }
        })?;
        let state = match m.state {
            ModeState::Ready => return Ok(Some(&m.row)),
            ModeState::Broken => "broken",
            ModeState::Unavailable => "unavailable",
        };
        Err(other(format!(
            "mode {id} is {state}: {}",
            m.reason.as_deref().unwrap_or_default()
        )))
    }

    /// Whether `job_retune` may move a running job of the kind to another band: the
    /// kinds whose one band the node expands a graph for. Not a `spectrum` (it shows the
    /// capture), a `scan` (it sweeps), an `identify` or a `watch` (controllers), an
    /// attached `transcribe` (its input is retuned instead) or a `mix` (it has no band).
    pub fn retunable(&self) -> bool {
        match self {
            JobKind::Passthrough
            | JobKind::FmDemod
            | JobKind::WfmDemod
            | JobKind::AmDemod
            | JobKind::SsbDemod
            | JobKind::AprsDecode
            | JobKind::PocsagDecode
            | JobKind::RdsDecode
            | JobKind::AcarsDecode
            | JobKind::AdsbDecode
            | JobKind::Measure
            | JobKind::Mode(_) => true,
            JobKind::Spectrum
            | JobKind::Scan
            | JobKind::Identify
            | JobKind::Watch
            | JobKind::Transcribe
            | JobKind::Mix
            | JobKind::Capture => false,
        }
    }

    /// Whether `job_start` may start the kind `elastic` (following the capture).
    pub fn elastic_capable(&self) -> bool {
        matches!(self, JobKind::Spectrum)
    }

    /// Whether the kind runs on another job's output (`job_attach`) rather than on the
    /// radio (`job_start`).
    pub fn attachable(&self) -> bool {
        matches!(self, JobKind::Transcribe)
    }

    /// Whether `job_start` may start the kind on the radio: not an attachable kind, and
    /// not a `mix` (`mix_start`) or a [`GRAPH_KIND`] or [`BITS_KIND`] job (`graph_start`,
    /// `bits_start`), which have no source or band of their own.
    pub fn startable(&self) -> bool {
        !self.attachable() && !matches!(self, JobKind::Mix) && !self.leaseless_name()
    }

    /// Whether the kind is named [`GRAPH_KIND`] or [`BITS_KIND`]: no built-in kind, so it
    /// would read as a mode row's id.
    fn leaseless_name(&self) -> bool {
        matches!(self, JobKind::Mode(id) if id.0 == GRAPH_KIND || id.0 == BITS_KIND)
    }

    /// Why `job_start` refuses a kind that is not [`startable`](Self::startable).
    fn start_refusal(&self) -> OpError {
        let message = match self {
            JobKind::Mix => "mix takes no band: use mix_start".to_string(),
            k if k.leaseless_name() => {
                format!("{} takes no band: use graph_start / bits_start", k.name())
            }
            k => format!("{} attaches to a job: use job_attach", k.name()),
        };
        OpError::Other { message }
    }

    /// The graph of an attached job of this kind reading `input_stream` (an audio stream)
    /// of `input_job`: `stream_in` then the kind's chain, joined by audio edges. For
    /// `transcribe`: `stream_in -> denoise -> vad -> transcribe {"language"}`, without the
    /// `denoise` when `input_denoised` (the input's demodulator runs RNNoise itself: see
    /// [`graph_denoises`]). A kind that is not [`attachable`](Self::attachable) is
    /// `other("<kind> is not attachable")`.
    pub fn expand_attached(
        &self,
        input_job: &JobId,
        input_stream: &StreamId,
        language: &str,
        input_denoised: bool,
    ) -> Result<Graph, OpError> {
        let chain: Vec<(&str, serde_json::Value)> = match self {
            JobKind::Transcribe => {
                let mut chain = vec![
                    ("vad", serde_json::json!({})),
                    ("transcribe", serde_json::json!({"language": language})),
                ];
                if !input_denoised {
                    chain.insert(0, ("denoise", serde_json::json!({})));
                }
                chain
            }
            _ => {
                return Err(OpError::Other {
                    message: format!("{} is not attachable", self.name()),
                });
            }
        };
        let mut nodes = vec![GraphNode {
            id: NodeId::new(),
            kind: crate::stream_in::STREAM_IN_KIND.into(),
            params: serde_json::json!({
                "job": input_job.0,
                "kind": "audio",
                "stream": input_stream.0,
            }),
        }];
        nodes.extend(chain.into_iter().map(|(kind, params)| GraphNode {
            id: NodeId::new(),
            kind: kind.into(),
            params,
        }));
        let edges = nodes
            .windows(2)
            .map(|w| Edge {
                from: w[0].id.clone(),
                to: w[1].id.clone(),
                kind: StreamKind::Audio,
            })
            .collect();
        Ok(Graph {
            id: GraphId::new(),
            nodes,
            edges,
        })
    }

    /// Whether `graph` denoises its audio itself: a demodulator node with `denoise: true`
    /// (`nbfm_demod`, `am_demod`, `ssb_demod`), or a `denoise` stage.
    pub fn graph_denoises(graph: &Graph) -> bool {
        graph.nodes.iter().any(|n| {
            n.kind == "denoise" || n.params.get("denoise").and_then(|v| v.as_bool()) == Some(true)
        })
    }

    /// The kind recorded as `name` (see [`JobKind::name`]): a built-in kind, else a
    /// `Mode` when `name` is a mode id (`^[a-z][a-z0-9_]*$`), else `None`.
    pub fn from_name(name: &str) -> Option<JobKind> {
        BUILTIN_KINDS
            .iter()
            .find(|k| k.name() == name)
            .cloned()
            .or_else(|| is_mode_id(name).then(|| JobKind::Mode(ModeId(name.into()))))
    }

    /// [`JobKind::expand`] again for a resumed job whose `stored` graph was expanded for
    /// another source rate: its `deviation_hz` and `sideband` are read back from the
    /// stored graph's node params (absent: the kind's default), and a demodulator's
    /// `squelch_db` and `squelch`, its adjacent-channel guard's `guard_hz`, `guard_db` and
    /// `channel_hz`, and a channel's `wide_hz` (as `graph_put` may have set them) are
    /// carried to the node of the same stage kind when `expand` writes the key (absent
    /// from `stored`: the defaults `expand` writes). A `watch`'s `detect` node keeps its
    /// stored params whole.
    pub fn reexpand(
        &self,
        source: &SourceId,
        tuning: Tuning,
        source_rate_hz: Hz,
        bank_bin_rate_hz: Option<u64>,
        stored: &Graph,
        modes: &ModeRegistry,
    ) -> Result<Graph, OpError> {
        self.reexpand_carrying(
            source,
            tuning,
            source_rate_hz,
            bank_bin_rate_hz,
            stored,
            modes,
            &CARRIED_PARAMS,
        )
    }

    /// The `deviation_hz` and `sideband` a graph was expanded with, read back from
    /// its demodulator nodes (`None` for each the graph has no node for).
    pub fn start_params(&self, graph: &Graph) -> (Option<u64>, Option<Sideband>) {
        let param = |key: &str| graph.nodes.iter().find_map(|n| n.params.get(key).cloned());
        (
            param("deviation_hz").and_then(|v| v.as_u64()),
            param("sideband").and_then(|v| serde_json::from_value(v).ok()),
        )
    }

    /// [`JobKind::reexpand`] carrying only the params named in `carried` (a retune
    /// passes [`RETUNE_CARRIED_PARAMS`]); the rest take the values `expand` derives.
    /// `deviation_hz` and `sideband` are read back from `stored` whatever `carried` says.
    #[allow(clippy::too_many_arguments)]
    pub fn reexpand_carrying(
        &self,
        source: &SourceId,
        tuning: Tuning,
        source_rate_hz: Hz,
        bank_bin_rate_hz: Option<u64>,
        stored: &Graph,
        modes: &ModeRegistry,
        carried: &[&str],
    ) -> Result<Graph, OpError> {
        let (deviation_hz, sideband) = self.start_params(stored);
        let deviation_hz = deviation_hz.map(Hz);
        let mut g = self.expand(
            source,
            tuning,
            source_rate_hz,
            bank_bin_rate_hz,
            deviation_hz,
            sideband,
            modes,
        )?;
        for n in &mut g.nodes {
            let Some(old) = stored.nodes.iter().find(|o| o.kind == n.kind) else {
                continue;
            };
            // A watch's `detect` params (thresholds and policy) are the operator's whole.
            if n.kind == DETECT_STAGE {
                n.params = old.params.clone();
                continue;
            }
            // A `capture` job's `burst_capture` params (the watch's policy block, the
            // signal it records) are kept whole but for the band, derived again.
            if n.kind == BURST_CAPTURE_STAGE {
                let band = n.params.get("bandwidth_hz").cloned();
                n.params = old.params.clone();
                if let (Some(band), Some(p)) = (band, n.params.as_object_mut()) {
                    p.insert("bandwidth_hz".into(), band);
                }
                continue;
            }
            for &key in carried {
                if let (Some(v), Some(p)) = (old.params.get(key), n.params.as_object_mut())
                    && p.contains_key(key)
                {
                    p.insert(key.into(), v.clone());
                }
            }
        }
        Ok(g)
    }

    /// Whether the kind's graph has a `channel` node, so depends on the source rate.
    fn channelised(&self) -> bool {
        matches!(
            self,
            JobKind::FmDemod
                | JobKind::WfmDemod
                | JobKind::AmDemod
                | JobKind::SsbDemod
                | JobKind::AprsDecode
                | JobKind::PocsagDecode
                | JobKind::RdsDecode
                | JobKind::AcarsDecode
                | JobKind::AdsbDecode
                | JobKind::Measure
                | JobKind::Capture
                | JobKind::Mode(_)
        )
    }

    /// The kind's graph for `tuning` on a source sampling at `source_rate_hz`
    /// (the port-returned `bandwidth_hz`). Kinds with a `channel` node refuse
    /// an unknown (0) rate and a band wider than [`channel_rate_hz`].
    ///
    /// `bank_bin_rate_hz` is the bin rate of the source's channel bank, if it
    /// has one (twice the bin width). A band no wider than a bin is served by
    /// the bank, so its channel rate is [`channel_rate_hz`] of the bin rate;
    /// otherwise of the source rate.
    ///
    /// A decoder kind or a `Mode` expands from its ready row of `modes` (see
    /// [`JobKind::mode_row`] and [`template::expand`]).
    #[allow(clippy::too_many_arguments)]
    pub fn expand(
        &self,
        _source: &SourceId,
        tuning: Tuning,
        source_rate_hz: Hz,
        bank_bin_rate_hz: Option<u64>,
        deviation_hz: Option<Hz>,
        sideband: Option<Sideband>,
        modes: &ModeRegistry,
    ) -> Result<Graph, OpError> {
        if !self.startable() {
            return Err(self.start_refusal());
        }
        let row = self.mode_row(modes)?;
        let bw = tuning.bandwidth_hz.0;
        let out = match bank_bin_rate_hz {
            Some(bin_rate) if bw <= bin_rate / 2 => channel_rate_hz(bin_rate, bw),
            _ => channel_rate_hz(source_rate_hz.0, bw),
        };
        if self.channelised() {
            if source_rate_hz.0 == 0 {
                return Err(OpError::Other {
                    message: "source rate unknown: tune returned bandwidth_hz 0".into(),
                });
            }
            if tuning.bandwidth_hz.0 > out {
                return Err(OpError::OutOfRange {
                    field: "bandwidth_hz".into(),
                    value: tuning.bandwidth_hz.0,
                    min: 1,
                    max: out,
                    suggested: None,
                });
            }
        }
        if let Some(row) = row {
            template::check_band(row, tuning, source_rate_hz.0)?;
            return template::expand(row, tuning, out);
        }
        Ok(match self {
            JobKind::Transcribe | JobKind::Mix => unreachable!("refused above: not startable"),
            JobKind::AprsDecode
            | JobKind::PocsagDecode
            | JobKind::RdsDecode
            | JobKind::AcarsDecode
            | JobKind::AdsbDecode
            | JobKind::Mode(_) => unreachable!("expanded above from the row"),
            JobKind::Scan => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "fft".into(),
                    params: serde_json::json!({
                        "size": spectrum_size(tuning.bandwidth_hz.0.min(SCAN_STEP_MAX_HZ)),
                        "window": "hann",
                        "rows_per_s": SPECTRUM_ROWS_PER_S,
                    }),
                }],
                edges: vec![],
            },
            JobKind::Identify => Graph {
                id: GraphId::new(),
                nodes: vec![],
                edges: vec![],
            },
            JobKind::Passthrough => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "passthrough".into(),
                    params: serde_json::json!({"kind": "iq"}),
                }],
                edges: vec![],
            },
            JobKind::Measure => {
                let (channel, measure) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: measure.clone(),
                            kind: "measure".into(),
                            params: serde_json::json!({"bandwidth_hz": tuning.bandwidth_hz.0}),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: measure,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::Capture => {
                let (channel, capture) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: capture.clone(),
                            kind: BURST_CAPTURE_STAGE.into(),
                            params: serde_json::json!({"bandwidth_hz": tuning.bandwidth_hz.0}),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: capture,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::Watch => {
                let (fft, detect) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: fft.clone(),
                            kind: "fft".into(),
                            params: serde_json::json!({
                                "size": watch_fft_size(source_rate_hz.0.max(bw)),
                                "window": "hann",
                                "rows_per_s": SPECTRUM_ROWS_PER_S,
                            }),
                        },
                        GraphNode {
                            id: detect.clone(),
                            kind: DETECT_STAGE.into(),
                            params: serde_json::json!({}),
                        },
                    ],
                    edges: vec![Edge {
                        from: fft,
                        to: detect,
                        kind: StreamKind::Event,
                    }],
                }
            }
            JobKind::Spectrum => Graph {
                id: GraphId::new(),
                nodes: vec![GraphNode {
                    id: NodeId::new(),
                    kind: "fft".into(),
                    params: serde_json::json!({
                        "size": spectrum_size(tuning.bandwidth_hz.0),
                        "window": "hann",
                        "rows_per_s": SPECTRUM_ROWS_PER_S,
                    }),
                }],
                edges: vec![],
            },
            JobKind::FmDemod => {
                let (channel, demod) = (NodeId::new(), NodeId::new());
                let mut channel_params = serde_json::json!({
                    "center_hz": tuning.center_hz.0,
                    "bandwidth_hz": bw,
                    "out_rate_hz": out,
                });
                let mut demod_params = serde_json::json!({
                    "deviation_hz": deviation_hz.map_or(FM_DEVIATION_DEFAULT_HZ, |d| d.0),
                    "audio_rate_hz": AUDIO_RATE_HZ,
                    "squelch_db": null,
                    "squelch": true,
                    "audio_lowpass_hz": FM_AUDIO_LOWPASS_HZ,
                    "deemphasis_us": FM_DEEMPHASIS_US,
                    "denoise": false,
                });
                // On a plan channel, with room for both neighbours at the channel rate: the
                // adjacent-channel guard (the channel cut three raster steps wide).
                if let Some((plan, _)) = crate::plans::channel_at(tuning.center_hz.0) {
                    let g = crate::plans::adjacent_hz(plan);
                    if bw + 2 * g <= out {
                        channel_params["wide_hz"] = (bw + 2 * g).into();
                        demod_params["guard_hz"] = g.into();
                        demod_params["guard_db"] = crate::plans::GUARD_DB_DEFAULT.into();
                        demod_params["channel_hz"] = bw.into();
                    }
                }
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: channel_params,
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "nbfm_demod".into(),
                            params: demod_params,
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::WfmDemod => {
                let (channel, demod) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "wfm_demod".into(),
                            params: serde_json::json!({
                                "deviation_hz": deviation_hz.map_or(WFM_DEVIATION_DEFAULT_HZ, |d| d.0),
                                "audio_rate_hz": AUDIO_RATE_HZ,
                                "deemphasis_us": WFM_DEEMPHASIS_US,
                            }),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::AmDemod => {
                let (channel, demod) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "am_demod".into(),
                            params: serde_json::json!({
                                "audio_rate_hz": AUDIO_RATE_HZ,
                                "squelch_db": null,
                                "squelch": true,
                                "denoise": false,
                            }),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
            JobKind::SsbDemod => {
                let (channel, demod) = (NodeId::new(), NodeId::new());
                Graph {
                    id: GraphId::new(),
                    nodes: vec![
                        GraphNode {
                            id: channel.clone(),
                            kind: "channel".into(),
                            params: serde_json::json!({
                                "center_hz": tuning.center_hz.0,
                                "bandwidth_hz": tuning.bandwidth_hz.0,
                                "out_rate_hz": out,
                            }),
                        },
                        GraphNode {
                            id: demod.clone(),
                            kind: "ssb_demod".into(),
                            params: serde_json::json!({
                                "sideband": sideband.unwrap_or(Sideband::Usb),
                                "audio_rate_hz": AUDIO_RATE_HZ,
                                "audio_bandwidth_hz": SSB_AUDIO_BANDWIDTH_HZ,
                                "denoise": false,
                            }),
                        },
                    ],
                    edges: vec![Edge {
                        from: channel,
                        to: demod,
                        kind: StreamKind::Iq,
                    }],
                }
            }
        })
    }
}

/// The audio rate every demodulating job kind outputs; the demod stage
/// resamples from the channel rate to this.
pub const AUDIO_RATE_HZ: u64 = 48_000;

/// Target spectrum resolution: a job's FFT has about one bin per this many Hz of band.
pub const SPECTRUM_BIN_TARGET_HZ: u64 = 6_000;
/// Smallest and largest FFT size a `spectrum` (or `scan`) job uses.
pub const SPECTRUM_SIZE_MIN: u32 = 1024;
pub const SPECTRUM_SIZE_MAX: u32 = 8192;
/// Spectrum rows per second of signal time.
pub const SPECTRUM_ROWS_PER_S: u32 = 20;

/// FFT size for a spectrum of `bandwidth_hz`: the smallest power of two at least
/// `bandwidth_hz / SPECTRUM_BIN_TARGET_HZ`, clamped to
/// `SPECTRUM_SIZE_MIN..=SPECTRUM_SIZE_MAX`.
pub fn spectrum_size(bandwidth_hz: u64) -> u32 {
    let want = bandwidth_hz.div_ceil(SPECTRUM_BIN_TARGET_HZ).max(1);
    want.checked_next_power_of_two()
        .unwrap_or(u64::MAX)
        .clamp(SPECTRUM_SIZE_MIN as u64, SPECTRUM_SIZE_MAX as u64) as u32
}

/// Target resolution of a `watch` job's FFT: finer than a spectrum's, so a ~10 kHz sonde is
/// several bins wide and the detector's centre estimate (the identify child's tuning) is
/// within a bin or so of the signal's.
pub const WATCH_BIN_TARGET_HZ: u64 = 1_500;

/// FFT size for a watch over a capture of `capture_hz`: the smallest power of two at least
/// `capture_hz / WATCH_BIN_TARGET_HZ`, clamped to `SPECTRUM_SIZE_MIN..=SPECTRUM_SIZE_MAX`
/// (4096 bins, about 1.46 kHz each, at 6 MHz).
pub fn watch_fft_size(capture_hz: u64) -> u32 {
    let want = capture_hz.div_ceil(WATCH_BIN_TARGET_HZ).max(1);
    want.checked_next_power_of_two()
        .unwrap_or(u64::MAX)
        .clamp(SPECTRUM_SIZE_MIN as u64, SPECTRUM_SIZE_MAX as u64) as u32
}

/// Widest capture one scan step asks of the source.
pub const SCAN_STEP_MAX_HZ: u64 = 16_000_000;
/// How long a scan waits on each step before reading the ring (the node's
/// default; `NodeConfig::scan_dwell_ms` overrides it).
pub const SCAN_DWELL_MS: u64 = 250;

/// How much of the ring, in ms, an `identify` job measures (layer 1) before it scores
/// the mode rows.
pub const IDENTIFY_MEASURE_MS: u64 = 1_000;

/// Full width of the DC notch a scan masks on local hardware, in Hz: bins within
/// half of this of each capture's centre are masked (the `fft` stage's
/// `notch_hz`). Measured 2026-11-01: the HackRF One's DC artefact is a 3-bin
/// needle at every rate (11.7 kHz over 6 dB at 4096 bins of 16 Msps, 2.9 kHz at
/// 1 Msps), the B210's 5-6 kHz over 6 dB and 9-15 kHz over 3 dB; 50 kHz is four
/// times the widest and the same width as `MEASURE_DC_MASK_HZ` either side.
pub const SCAN_NOTCH_HZ: u64 = 50_000;
/// Fraction of each capture's bins masked at each edge on local hardware.
/// Measured 2026-11-01: the HackRF has no edge roll-off at 1-16 Msps (the outer
/// 5 % within 3 dB of the row median) but a spur on its edge bins (±fs/2, 13-23 dB
/// at 16 Msps); the B210 rolls off over 0.9-3.2 % of the row. 0.02 covers both.
pub const SCAN_EDGE_MASK: f64 = 0.02;
/// Half-width of the DC mask a `measure` job applies, in Hz.
pub const MEASURE_DC_MASK_HZ: u64 = 25_000;

/// Whether the source is a local radio (a SoapySDR device): neither remote nor
/// push. A recording has `push: true`; a Kiwi, a `--source-node` and a VRT
/// sender are `remote`.
pub fn local_hardware(caps: &Capabilities) -> bool {
    !caps.remote && !caps.push
}

/// The scan mask `(notch_hz, edge_mask)` a source gets by default:
/// [`SCAN_NOTCH_HZ`] and [`SCAN_EDGE_MASK`] on local hardware, `(0, 0.0)` else.
pub fn scan_mask_default(caps: &Capabilities) -> (u64, f64) {
    if local_hardware(caps) {
        (SCAN_NOTCH_HZ, SCAN_EDGE_MASK)
    } else {
        (0, 0.0)
    }
}

/// The notch actually applied for a scan step of `step_hz`: `0` when the step
/// is under four notch widths (the notch would eat the capture), else
/// `notch_hz`.
pub fn scan_notch_in_force(step_hz: u64, notch_hz: u64) -> u64 {
    if step_hz < notch_hz.saturating_mul(4) {
        0
    } else {
        notch_hz
    }
}

/// The edge mask actually applied: the configured `edge_mask` when the scan's
/// grid alternates, else `0.0` (a one-step scan has no other sweep to fill its
/// edges in, so masking them would hide signal for good).
pub fn scan_edge_mask_in_force(alternates: bool, edge_mask: f64) -> f64 {
    if alternates { edge_mask } else { 0.0 }
}

/// The two step lists a scan alternates between (see [`scan_grid`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ScanGrid {
    /// Sweeps 0, 2, 4, ...
    pub even: Vec<Tuning>,
    /// Sweeps 1, 3, 5, ...: the even grid shifted down by a quarter step.
    pub odd: Vec<Tuning>,
}

impl ScanGrid {
    /// Whether the odd grid differs from the even one.
    pub fn alternates(&self) -> bool {
        self.odd != self.even
    }

    /// The steps of sweep number `sweep`.
    pub fn steps(&self, sweep: u64) -> &[Tuning] {
        if sweep.is_multiple_of(2) {
            &self.even
        } else {
            &self.odd
        }
    }

    /// How far the odd grid sits below the even one: a quarter step.
    pub fn shift_hz(&self) -> u64 {
        self.even[0].bandwidth_hz.0 / 4
    }
}

/// The grids a scan of `span` takes on a source with `caps`: captures of
/// `w = min(max_bandwidth_hz, SCAN_STEP_MAX_HZ, span width)`. The even grid is
/// centred at `start + w/2 + i·w` for `i < n = ceil(bandwidth_hz / w)`, where
/// `start` is the span's lower edge, so a span narrower than one capture is one
/// step exactly as wide as the span. The odd grid is centred at
/// `start + w/4 + i·w` for `i <= n` (a quarter step lower), dropping a step
/// whose band misses the span, so it has at most one more step. A quarter
/// step, not a half: with rows about as wide as the step, each grid's DC notch
/// and step boundaries then fall at the 25 % / 75 % points of the other grid's
/// rows, clear of its notch and of any edge mask under 0.25, so every
/// frequency is seen clean in one of two sweeps (a half-step shift put each
/// notch on the other grid's step boundary, inside both neighbours' edge
/// masks). A one-step span does not alternate and its odd grid is a copy of
/// the even one. Every centre is clamped to `[tune_min + w/2, tune_max - w/2]`,
/// so no capture reaches outside the source's tune range (when that range is
/// narrower than `w`, to `tune_min + w/2`), and a centre equal to the one
/// before it is dropped.
pub fn scan_grid(span: Tuning, caps: &Capabilities) -> ScanGrid {
    let w = caps
        .max_bandwidth_hz
        .value()
        .0
        .min(SCAN_STEP_MAX_HZ)
        .min(span.bandwidth_hz.0)
        .max(1);
    let n = span.bandwidth_hz.0.div_ceil(w).max(1);
    let start = span.center_hz.0.saturating_sub(span.bandwidth_hz.0 / 2);
    let lo = caps.tune_min_hz.value().0.saturating_add(w / 2);
    let hi = caps.tune_max_hz.value().0.saturating_sub(w / 2).max(lo);
    let build = |centres: &mut dyn Iterator<Item = u64>| -> Vec<Tuning> {
        let mut steps: Vec<Tuning> = Vec::new();
        for c in centres {
            let step = Tuning {
                center_hz: Hz(c.clamp(lo, hi)),
                bandwidth_hz: Hz(w),
            };
            if steps.last() != Some(&step) {
                steps.push(step);
            }
        }
        steps
    };
    let even = build(&mut (0..n).map(|i| start + w / 2 + i * w));
    let end = start.saturating_add(span.bandwidth_hz.0);
    let odd = if n > 1 {
        // A step's unclamped band is `c - w/2 .. c + w/2`; its high edge is
        // always past `start`, so only a last step can miss the span.
        build(
            &mut (0..=n)
                .map(|i| start + w / 4 + i * w)
                .filter(|&c| c.saturating_sub(w / 2) < end),
        )
    } else {
        even.clone()
    };
    ScanGrid { even, odd }
}

/// The steps of a scan's even sweeps: [`scan_grid`]`.even`.
pub fn scan_steps(span: Tuning, caps: &Capabilities) -> Vec<Tuning> {
    scan_grid(span, caps).even
}

/// The lowest channel output rate a job is channelised at.
pub const CHANNEL_RATE_MIN_HZ: u64 = 48_000;

/// Stage kinds that carry the demodulator squelch (`squelch_db`, `squelch` params).
pub const SQUELCH_STAGES: [&str; 2] = ["nbfm_demod", "am_demod"];

/// Fill in the squelch params of every [`SQUELCH_STAGES`] node of `graph` that lacks
/// them (a graph stored before the squelch, or an operator's): `squelch_db: null`, and
/// `squelch: false` when the node's audio feeds another node of the graph (a decoder, as
/// in `aprs_decode`), else `true` (the job's own audio, as in `fm_demod`). Params
/// already there are kept. Whether anything was filled in.
pub fn fill_squelch_params(graph: &mut Graph) -> bool {
    let mut filled = false;
    for n in &mut graph.nodes {
        if !SQUELCH_STAGES.contains(&n.kind.as_str()) {
            continue;
        }
        let feeds = graph
            .edges
            .iter()
            .any(|e| e.from == n.id && e.kind == StreamKind::Audio);
        let Some(p) = n.params.as_object_mut() else {
            continue;
        };
        if !p.contains_key("squelch") {
            p.insert("squelch".into(), (!feeds).into());
            filled = true;
        }
        if !p.contains_key("squelch_db") {
            p.insert("squelch_db".into(), serde_json::Value::Null);
            filled = true;
        }
    }
    filled
}

/// The stage kind of a `watch`'s detector (registered by `zeta-dsp`); its params carry
/// the watch's policy too (`decode_on`, `prefer`, `sondehub`), see `node::watch`.
pub const DETECT_STAGE: &str = "detect";

/// The stage kind of the sonde feed a watch appends to its decoders when its `detect`
/// params carry `sondehub` (registered by `zeta-ext`).
pub const SONDEHUB_STAGE: &str = "sink_sondehub";

/// The stage kind a `capture` job cuts its bursts with (registered by `zeta-dsp`): it
/// writes them to the node's [`crate::bursts::BurstStore`] and ends the job.
pub const BURST_CAPTURE_STAGE: &str = "burst_capture";

/// The stage kind that replays burst captures (registered by `zeta-dsp`): its params'
/// `captures` are ids in the node's [`crate::bursts::BurstStore`].
pub const CAPTURE_IN_STAGE: &str = "capture_in";

/// The stage kind that slices symbols into `bits` events (registered by `zeta-dsp`): it
/// notes the stream it publishes them on as `slicer.stream` in the job's context.
pub const SLICER_STAGE: &str = "slicer";

/// The kind recorded for a job `graph_start` started.
pub const GRAPH_KIND: &str = "graph";

/// The kind recorded for a job `bits_start` started.
pub const BITS_KIND: &str = "bits";

/// The `detect` stage's default `gone_ms`: an announced signal unmatched this long is
/// `gone`. The watch re-adopts a decoder only when its signal reappears within it.
pub const DETECT_GONE_MS_DEFAULT: u64 = 30_000;

/// The params [`JobKind::reexpand`] carries from a stored graph to the node of the same
/// stage kind: what an operator may set with `graph_put` and still have the node's graph.
const CARRIED_PARAMS: [&str; 9] = [
    "squelch_db",
    "squelch",
    "guard_hz",
    "guard_db",
    "channel_hz",
    "wide_hz",
    "denoise",
    "audio_lowpass_hz",
    "deemphasis_us",
];

/// The params a retune ([`JobKind::reexpand_carrying`]) carries from the stored graph:
/// the squelch. The adjacent-channel guard (`guard_hz`, `guard_db`, `channel_hz`, and the
/// channel node's `wide_hz`) is derived again from the channel plan at the new band, so a
/// width preset changes the filter and a move to a neighbouring channel takes that
/// channel's geometry.
pub const RETUNE_CARRIED_PARAMS: [&str; 5] = [
    "squelch_db",
    "squelch",
    "denoise",
    "audio_lowpass_hz",
    "deemphasis_us",
];

/// `fm_demod`'s audio low-pass cutoff, Hz: voice to 3.4 kHz, the FM hiss above it out.
pub const FM_AUDIO_LOWPASS_HZ: u64 = 3_400;
/// `fm_demod`'s de-emphasis time constant, µs (a 6 dB per octave roll-off, as PMR radios
/// apply).
pub const FM_DEEMPHASIS_US: u64 = 530;

/// The narrowest exact divisor of `source_rate_hz` that is at least
/// [`CHANNEL_RATE_MIN_HZ`] and at least `bandwidth_hz`; the source rate itself
/// when it is no wider than that. Integer only: the channel stage decimates by
/// an exact integer factor.
pub fn channel_rate_hz(source_rate_hz: u64, bandwidth_hz: u64) -> u64 {
    let target = bandwidth_hz.max(CHANNEL_RATE_MIN_HZ);
    if target >= source_rate_hz {
        return source_rate_hz;
    }
    (1..=source_rate_hz / target)
        .rev()
        .find(|d| source_rate_hz.is_multiple_of(*d))
        .map_or(source_rate_hz, |d| source_rate_hz / d)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields, transform = require_worker)]
pub struct SourceInfo {
    pub id: SourceId,
    pub capabilities: Capabilities,
    #[schemars(required, schema_with = "crate::types::nullable::<Tuning>")]
    pub current: Option<Tuning>,
    /// The jobs holding a rigid lease (never an elastic job).
    pub holders: Vec<JobId>,
    /// The elastic jobs following the source's capture.
    pub elastic: Vec<JobId>,
    /// The band `source_pin` holds the source at; `null` when unpinned.
    #[schemars(required, schema_with = "crate::types::nullable::<Tuning>")]
    pub pin: Option<Tuning>,
    /// The port's unrecoverable failure (`Port::fault`); `null` when healthy.
    /// Jobs on a faulted source fail with it; a successful tune clears it.
    #[schemars(required, schema_with = "nullable_string")]
    pub fault: Option<String>,
    /// The channel bank the node keeps for the source's capture; `null` when it
    /// keeps none (a narrow capture, or no bank factory).
    #[schemars(required, schema_with = "crate::types::nullable::<BankInfo>")]
    pub bank: Option<BankInfo>,
    /// The overall receive gain (`source_set_gain`); `null` for a source without
    /// adjustable gain (a recording, a KiwiSDR, another node, a VRT stream).
    #[schemars(required, schema_with = "crate::types::nullable::<GainInfo>")]
    pub gain: Option<GainInfo>,
    /// The stream's health counters (`Port::stats`): measured rate, overflows,
    /// read timeouts, stream restarts; `null` for a source that keeps none.
    #[schemars(required, schema_with = "crate::types::nullable::<PortStats>")]
    pub stats: Option<PortStats>,
    /// The child process the source runs in (`Port::worker`): its pid, how many
    /// times it was respawned, when the live one said hello; `null` for a source
    /// that runs in the node. Defaults to `null` when absent, so a plan-20 node can
    /// attach a source of an older node that does not write it (serde already reads a
    /// missing `Option` as `None`; the attribute keeps that if the field ever gains a
    /// `deserialize_with`). The reverse does not hold: an older node refuses the field
    /// as unknown, so both nodes of a federation are upgraded together.
    #[serde(default)]
    #[schemars(schema_with = "crate::types::nullable::<WorkerInfo>")]
    pub worker: Option<WorkerInfo>,
    /// The source's IQ history (`seconds`, the length it was sized to by the
    /// memory budget); `null` before its first block. A `classify` or `iq_read`
    /// span older than this fails `too_old`.
    #[serde(default)]
    #[schemars(required, schema_with = "crate::types::nullable::<RingInfo>")]
    pub ring: Option<RingInfo>,
}

/// Schemars omits a serde-defaulted field from `required`, and `#[schemars(required)]`
/// does not override that; every node of this version writes `worker` and `ring`, so the published
/// schema requires it (as `JobRecord`'s schema does its defaulted fields).
fn require_worker(schema: &mut schemars::Schema) {
    if let Some(req) = schema
        .ensure_object()
        .entry("required")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
    {
        for field in ["worker", "ring"] {
            if !req.iter().any(|r| r == field) {
                req.push(field.into());
            }
        }
    }
}

/// A channel `plan_start` started: its job and, with `attach`, the attached job or why
/// none could be attached.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanStarted {
    pub channel: u32,
    pub center_hz: Hz,
    pub job: JobId,
    /// The job attached to `job`; `null` without `attach` or when it failed.
    #[schemars(required, schema_with = "crate::types::nullable::<JobId>")]
    pub attached: Option<JobId>,
    /// Why the attach failed (the job is kept); `null` otherwise.
    #[schemars(required, schema_with = "nullable_op_error")]
    pub attach_error: Option<OpError>,
}

/// Schema of `plan_start`'s `attach_error`: an [`OpError`] or `null`. (An `OpError`
/// schema has no `type` to add `null` to, so this is an `anyOf`.)
fn nullable_op_error(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let error = g.subschema_for::<OpError>();
    schemars::json_schema!({ "anyOf": [error, { "type": "null" }] })
}

/// A channel `plan_start` skipped because an identical rigid job already runs there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanExisting {
    pub channel: u32,
    pub center_hz: Hz,
    pub job: JobId,
    /// The running job of the `attach` kind reading `job`; `null` for none.
    #[schemars(required, schema_with = "crate::types::nullable::<JobId>")]
    pub attached: Option<JobId>,
}

/// A channel `plan_start` could not start, with the start's error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanRefused {
    pub channel: u32,
    pub center_hz: Hz,
    pub error: OpError,
}

/// A job `jobs_stop` could not stop, with why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct JobStopFailed {
    pub job: JobId,
    pub error: OpError,
}

/// A source of `source_free` that covers the band but cannot take it now, with why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BusySource {
    pub id: SourceId,
    /// The jobs holding a rigid lease.
    pub holders: Vec<JobId>,
    /// The elastic jobs following the capture (never what makes a source busy).
    pub elastic: Vec<JobId>,
    /// The band `source_pin` holds the source at; `null` when unpinned.
    #[schemars(required, schema_with = "crate::types::nullable::<Tuning>")]
    pub pin: Option<Tuning>,
    /// The port's unrecoverable failure; `null` when healthy.
    #[schemars(required, schema_with = "nullable_string")]
    pub fault: Option<String>,
}

/// A source's IQ history, as `source_list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct RingInfo {
    /// The history length the ring was sized to, in seconds.
    pub seconds: f64,
}

/// A source's channel bank: its bin spacing and live subscribers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct BankInfo {
    pub bin_hz: u64,
    pub consumers: usize,
    /// Input blocks the bank dropped at its full queue, cumulative.
    pub dropped: u64,
}

/// One frame of a `stream_tail`, as JSON. `source`, `job`, `channel` and
/// `start_ns`/`end_ns` are the frame's provenance (`job`/`channel` null when
/// absent). `payload` by kind: event, the event value; text, the string;
/// audio, `{"sample_rate_hz", "samples", "peak"}` (count and largest absolute
/// sample); iq and sink, `{"sample_rate_hz", "center_hz", "samples"}`;
/// video, `{"width", "height", "format", "bytes"}` (`format` `rgb8`, `gray8` or `png`;
/// `bytes` the frame's byte count).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct FrameSummary {
    pub stream: StreamId,
    pub kind: StreamKind,
    pub source: SourceId,
    #[schemars(required, schema_with = "crate::types::nullable::<JobId>")]
    pub job: Option<JobId>,
    #[schemars(required, schema_with = "crate::types::nullable::<ChannelId>")]
    pub channel: Option<ChannelId>,
    pub start_ns: Timestamp,
    pub end_ns: Timestamp,
    pub payload: serde_json::Value,
}

impl FrameSummary {
    pub fn from_frame(f: &Frame) -> Self {
        let payload = match &f.payload {
            Payload::Event(v) => v.clone(),
            Payload::Text(t) => serde_json::Value::String(t.clone()),
            Payload::Audio {
                sample_rate_hz,
                channels,
                samples,
            } => serde_json::json!({
                "sample_rate_hz": sample_rate_hz.0,
                "channels": channels,
                "samples": samples.len() / usize::from((*channels).max(1)),
                "peak": samples.iter().fold(0.0f32, |m, s| m.max(s.abs())),
            }),
            Payload::Iq(b) | Payload::Sink(b) => serde_json::json!({
                "sample_rate_hz": b.sample_rate_hz.0,
                "center_hz": b.center_hz.0,
                "samples": b.samples.len(),
            }),
            Payload::Video {
                width,
                height,
                format,
                frame,
            } => serde_json::json!({
                "width": width,
                "height": height,
                "format": format.name(),
                "bytes": frame.len(),
            }),
        };
        Self {
            stream: f.stream.clone(),
            kind: f.kind,
            source: f.provenance.source.clone(),
            job: f.provenance.job.clone(),
            channel: f.provenance.channel.clone(),
            start_ns: f.provenance.span.start_ns,
            end_ns: f.provenance.span.end_ns,
            payload,
        }
    }
}

/// One channel plan as `channel_plans` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanInfo {
    pub id: String,
    pub name: String,
    pub region: String,
    /// The width of one channel.
    pub bandwidth_hz: Hz,
    /// The smallest spacing between two channel centres.
    pub adjacent_hz: Hz,
    /// Sorted by frequency.
    pub channels: Vec<PlanChannel>,
}

/// One channel of a plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct PlanChannel {
    pub number: u32,
    pub center_hz: Hz,
    /// What the channel is reserved for; `null` for an ordinary channel.
    #[schemars(required, schema_with = "nullable_plan_note")]
    #[serde(deserialize_with = "nullable_string_value")]
    pub note: Option<String>,
}

/// Schema of `PlanChannel.note`: `["string","null"]`.
fn nullable_plan_note(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["string", "null"],
        "description": "What the channel is reserved for (`emergency`, `calling`, `data, no voice`, `road (convention)`); `null` for an ordinary channel."
    })
}

impl From<&crate::plans::ChannelPlan> for PlanInfo {
    fn from(p: &crate::plans::ChannelPlan) -> Self {
        let note = |n| {
            p.notes
                .iter()
                .find(|(c, _)| *c == n)
                .map(|(_, t)| (*t).to_string())
        };
        PlanInfo {
            id: p.id.into(),
            name: p.name.into(),
            region: p.region.into(),
            bandwidth_hz: Hz(p.bandwidth_hz),
            adjacent_hz: Hz(crate::plans::adjacent_hz(p)),
            channels: crate::plans::channels(p)
                .into_iter()
                .map(|(number, hz)| PlanChannel {
                    number,
                    center_hz: Hz(hz),
                    note: note(number),
                })
                .collect(),
        }
    }
}

/// One mode as `modes_list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub struct ModeInfo {
    pub id: ModeId,
    pub name: String,
    pub family: Family,
    pub band: ModeBand,
    pub modulation: Vec<String>,
    pub state: ModeState,
    /// Why the row is not ready (a broken row names its file's field); `null` when ready.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub reason: Option<String>,
    pub decoder_kind: DecoderKind,
    /// The file the row came from: `modes/<id>.json` for a built-in row, the path for an
    /// operator row (the file to fix when the row is broken).
    pub file: String,
    /// The program a program row runs; `null` for a graph row or one not yet probed.
    #[schemars(required, schema_with = "crate::types::nullable::<String>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub program: Option<String>,
    /// When `zeta modes check` last passed this program row on this host (UNIX seconds); `null` if never.
    #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub checked: Option<u64>,
    /// What the mode's events are about, its table columns and how the page folds it;
    /// `null` for a mode with none.
    #[schemars(required, schema_with = "crate::types::nullable::<Subject>")]
    #[serde(deserialize_with = "crate::modes::nullable")]
    pub subject: Option<Subject>,
}

impl From<&LoadedMode> for ModeInfo {
    fn from(m: &LoadedMode) -> Self {
        ModeInfo {
            id: m.row.id.clone(),
            name: m.row.name.clone(),
            family: m.row.family,
            band: m.row.band.clone(),
            modulation: m.row.modulation.clone(),
            state: m.state,
            reason: m.reason.clone(),
            decoder_kind: m.row.decoder.kind(),
            file: m.file.clone(),
            program: m.program.as_ref().map(|p| p.display().to_string()),
            checked: m.checked,
            subject: m.row.subject.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[schemars(deny_unknown_fields)]
pub enum OpResult {
    Sources(Vec<SourceInfo>),
    /// `source_free`: the sources that could take the band now, and those that
    /// cover it but are held, pinned or faulted.
    SourceFree {
        free: Vec<SourceId>,
        busy: Vec<BusySource>,
    },
    Tuning(Tuning),
    Job(JobId),
    Jobs(Vec<JobRecord>),
    Context(serde_json::Value),
    /// `path` is the `.sigmf-meta` file; the data file sits beside it.
    /// `center_hz` and `bandwidth_hz` are the band actually written (the
    /// ring's), not the selection's.
    Iq {
        sample_rate_hz: Hz,
        center_hz: Hz,
        bandwidth_hz: Hz,
        samples: usize,
        path: PathBuf,
    },
    Streams(Vec<StreamInfo>),
    /// `missed`: frames skipped during the tail because it fell behind.
    Frames {
        frames: Vec<FrameSummary>,
        missed: u64,
    },
    Graph(Graph),
    Knowledge {
        allocations: Vec<Allocation>,
        signatures: Vec<Signature>,
        /// How far the providers searched: the first provider that applies one.
        /// (A missing `Option` reads as `None`, so an older node's answer loads.)
        #[schemars(
            required,
            schema_with = "crate::types::nullable::<crate::reach::Reach>"
        )]
        reach: Option<crate::reach::Reach>,
    },
    ChannelPlans {
        plans: Vec<PlanInfo>,
    },
    /// `modes_list`: one entry per row, sorted by id.
    Modes {
        modes: Vec<ModeInfo>,
    },
    /// `modes_show`: the row as loaded.
    Mode {
        mode: Box<LoadedMode>,
    },
    /// `classify`: every classifier's labels, the modes decode-to-identify
    /// considered with their score and outcome, and the allocations the knowledge
    /// providers hold for the selection's band in the station's region (the nearest
    /// assignments, then the plan rows: [`nearest_allocations`]).
    #[schemars(transform = require_all_value)]
    Classification {
        labels: Vec<Label>,
        tried: Vec<Tried>,
        /// Always written; an older node's answer without it loads as empty.
        #[serde(default)]
        allocations: Vec<Allocation>,
    },
    /// A source's gain after `source_set_gain`.
    Gain(GainInfo),
    /// `plan_start`: the capture the channels run in, what started, what already ran,
    /// what was refused, and warnings to repeat to the operator.
    PlanStart {
        plan: String,
        source: SourceId,
        capture: Tuning,
        started: Vec<PlanStarted>,
        existing: Vec<PlanExisting>,
        refused: Vec<PlanRefused>,
        warnings: Vec<String>,
    },
    /// `jobs_stop`: the jobs stopped and those that could not be.
    JobsStopped {
        stopped: Vec<JobId>,
        failed: Vec<JobStopFailed>,
    },
    Ok,
    /// `capture_list`: the records, newest first.
    Captures(Vec<crate::bursts::CaptureRecord>),
    /// `capture_show`: the record and its `.sigmf-meta` as JSON.
    Capture {
        record: crate::bursts::CaptureRecord,
        meta: serde_json::Value,
    },
    /// `station_set`, `station_get` and `station_clear`: the station, `null` for none.
    Station(Option<crate::station::Station>),
    Schema(serde_json::Value),
}

impl Op {
    /// The snake_case tag this variant serialises under `op`.
    pub fn name(&self) -> &'static str {
        match self {
            Op::SourceList {} => "source_list",
            Op::SourceEnsure { .. } => "source_ensure",
            Op::SourcePin { .. } => "source_pin",
            Op::SourceUnpin { .. } => "source_unpin",
            Op::SourceFree { .. } => "source_free",
            Op::SourceSetGain { .. } => "source_set_gain",
            Op::JobStart { .. } => "job_start",
            Op::JobAttach { .. } => "job_attach",
            Op::JobRetune { .. } => "job_retune",
            Op::MixStart { .. } => "mix_start",
            Op::StationSet { .. } => "station_set",
            Op::StationGet {} => "station_get",
            Op::StationClear {} => "station_clear",
            Op::CaptureList { .. } => "capture_list",
            Op::CaptureShow { .. } => "capture_show",
            Op::CaptureClassify { .. } => "capture_classify",
            Op::CaptureDelete { .. } => "capture_delete",
            Op::PlanStart { .. } => "plan_start",
            Op::JobList {} => "job_list",
            Op::JobStop { .. } => "job_stop",
            Op::JobsStop { .. } => "jobs_stop",
            Op::JobContext { .. } => "job_context",
            Op::IqRead { .. } => "iq_read",
            Op::StreamList {} => "stream_list",
            Op::StreamTail { .. } => "stream_tail",
            Op::GraphGet { .. } => "graph_get",
            Op::GraphPut { .. } => "graph_put",
            Op::GraphStart { .. } => "graph_start",
            Op::BitsStart { .. } => "bits_start",
            Op::KnowledgeLookup { .. } => "knowledge_lookup",
            Op::ChannelPlans {} => "channel_plans",
            Op::ModesList { .. } => "modes_list",
            Op::ModesShow { .. } => "modes_show",
            Op::Classify { .. } => "classify",
            Op::Schema {} => "schema",
        }
    }

    /// Whether the op only reads the node's state: its result is left out of
    /// `ops.log` when it succeeds (a page polls these several times a second).
    pub fn read_only(&self) -> bool {
        matches!(
            self,
            Op::SourceList {}
                | Op::SourceFree { .. }
                | Op::JobList {}
                | Op::StreamList {}
                | Op::StreamTail { .. }
                | Op::Schema {}
                | Op::JobContext { .. }
                | Op::GraphGet { .. }
                | Op::KnowledgeLookup { .. }
                | Op::ChannelPlans {}
                | Op::ModesList { .. }
                | Op::ModesShow { .. }
                | Op::StationGet {}
                | Op::CaptureList { .. }
                | Op::CaptureShow { .. }
        )
    }

    /// The op as JSON for the `ops` stream: the full value when it fits in
    /// [`OPS_RESULT_LIMIT_BYTES`], otherwise
    /// `{"op": <name>, "truncated": true, "bytes": n}`. A value that fails to
    /// serialise is summarised with `bytes: 0`.
    pub fn summary_json(&self) -> serde_json::Value {
        bounded_json(self, "op", self.name())
    }
}

/// `value` as JSON when its serialisation fits in [`OPS_RESULT_LIMIT_BYTES`],
/// else `{tag: name, "truncated": true, "bytes": n}`; a serde failure gives
/// `bytes: 0`.
fn bounded_json<T: Serialize>(value: &T, tag: &str, name: &str) -> serde_json::Value {
    let summary = |bytes: usize| {
        let mut m = serde_json::Map::new();
        m.insert(tag.into(), name.into());
        m.insert("truncated".into(), true.into());
        m.insert("bytes".into(), bytes.into());
        serde_json::Value::Object(m)
    };
    match serde_json::to_vec(value) {
        Ok(buf) if buf.len() > OPS_RESULT_LIMIT_BYTES => summary(buf.len()),
        Ok(buf) => serde_json::from_slice(&buf).unwrap_or_else(|_| summary(0)),
        Err(_) => summary(0),
    }
}

impl OpResult {
    /// The snake_case tag this variant serialises under `result`.
    pub fn variant_name(&self) -> &'static str {
        match self {
            OpResult::Sources(_) => "sources",
            OpResult::SourceFree { .. } => "source_free",
            OpResult::Tuning(_) => "tuning",
            OpResult::Job(_) => "job",
            OpResult::Jobs(_) => "jobs",
            OpResult::Context(_) => "context",
            OpResult::Iq { .. } => "iq",
            OpResult::Streams(_) => "streams",
            OpResult::Frames { .. } => "frames",
            OpResult::Graph(_) => "graph",
            OpResult::Knowledge { .. } => "knowledge",
            OpResult::ChannelPlans { .. } => "channel_plans",
            OpResult::Modes { .. } => "modes",
            OpResult::Mode { .. } => "mode",
            OpResult::Classification { .. } => "classification",
            OpResult::Gain(_) => "gain",
            OpResult::PlanStart { .. } => "plan_start",
            OpResult::JobsStopped { .. } => "jobs_stopped",
            OpResult::Ok => "ok",
            OpResult::Schema(_) => "schema",
            OpResult::Station(_) => "station",
            OpResult::Captures(_) => "captures",
            OpResult::Capture { .. } => "capture",
        }
    }

    /// The result as JSON for the `ops` stream: the full value when it fits
    /// in [`OPS_RESULT_LIMIT_BYTES`], otherwise
    /// `{"result": <variant>, "truncated": true, "bytes": n}`. A value that
    /// fails to serialise is summarised with `bytes: 0`. A truncated `plan_start` or
    /// `jobs_stop` keeps what its reader needs: `counts`, and `plan_start`'s `warnings`.
    pub fn summary_json(&self) -> serde_json::Value {
        let mut v = bounded_json(self, "result", self.variant_name());
        if v.get("truncated").is_some() {
            match self {
                OpResult::PlanStart {
                    started,
                    existing,
                    refused,
                    warnings,
                    ..
                } => {
                    v["counts"] = serde_json::json!({
                        "started": started.len(),
                        "existing": existing.len(),
                        "refused": refused.len(),
                    });
                    v["warnings"] = serde_json::json!(warnings);
                }
                OpResult::JobsStopped { stopped, failed } => {
                    v["counts"] = serde_json::json!({
                        "stopped": stopped.len(),
                        "failed": failed.len(),
                    });
                }
                _ => {}
            }
        }
        v
    }
}

/// Largest result JSON, in bytes, carried whole on the `ops` stream.
pub const OPS_RESULT_LIMIT_BYTES: usize = 8192;

/// The `ops` stream's event payload for one applied operation:
/// `{"op", "outcome": "ok"|"error", "result"|"error", "elapsed_ms",
/// "transport"}` where `transport` names the caller (see
/// [`crate::node::Node::apply_from`]); see [`Op::summary_json`] and
/// [`OpResult::summary_json`] and [`OpError::summary_json`] for large ops,
/// results and errors.
pub fn ops_payload(
    op: &Op,
    outcome: &Result<OpResult, OpError>,
    elapsed_ms: u64,
    transport: &str,
) -> serde_json::Value {
    let op = op.summary_json();
    let mut v = serde_json::json!({
        "op": op,
        "elapsed_ms": elapsed_ms,
        "transport": transport,
    });
    match outcome {
        Ok(r) => {
            v["outcome"] = "ok".into();
            v["result"] = r.summary_json();
        }
        Err(e) => {
            v["outcome"] = "error".into();
            v["error"] = e.summary_json();
        }
    }
    v
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[schemars(deny_unknown_fields)]
pub enum OpError {
    OutOfRange {
        field: String,
        value: u64,
        min: u64,
        max: u64,
        #[schemars(required, schema_with = "crate::types::nullable::<u64>")]
        suggested: Option<u64>,
    },
    /// The source is held by `holders` at `current`, which the request does
    /// not fit inside.
    Conflict {
        source: SourceId,
        holders: Vec<JobId>,
        current: Tuning,
        options: Vec<String>,
    },
    Unknown {
        field: String,
        value: String,
        known: Vec<String>,
    },
    /// Struct variants, not `Ring(RingError)`: flattening a tagged enum into
    /// this one makes a schema that cannot forbid extra fields.
    Ring {
        detail: RingError,
    },
    Graph {
        detail: GraphError,
    },
    /// A struct variant, not `Other(String)`: an internally tagged enum cannot
    /// carry a bare string.
    Other {
        message: String,
    },
    /// A rigid start identical to the running job `job` (same source, kind, band,
    /// `deviation_hz` and `sideband`): stop it or use it.
    Duplicate {
        job: JobId,
        message: String,
    },
}

impl OpError {
    /// The snake_case tag this variant serialises under `error`.
    pub fn name(&self) -> &'static str {
        match self {
            OpError::OutOfRange { .. } => "out_of_range",
            OpError::Conflict { .. } => "conflict",
            OpError::Unknown { .. } => "unknown",
            OpError::Ring { .. } => "ring",
            OpError::Graph { .. } => "graph",
            OpError::Other { .. } => "other",
            OpError::Duplicate { .. } => "duplicate",
        }
    }

    /// The error as JSON for the `ops` stream: the full value when it fits in
    /// [`OPS_RESULT_LIMIT_BYTES`], otherwise
    /// `{"error": <variant>, "truncated": true, "bytes": n}`. A value that
    /// fails to serialise is summarised with `bytes: 0`.
    pub fn summary_json(&self) -> serde_json::Value {
        bounded_json(self, "error", self.name())
    }
}

// Hand-written: thiserror treats a field named `source` as the error source.
impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::OutOfRange {
                field,
                value,
                min,
                max,
                suggested,
            } => {
                write!(f, "{field} {value} out of range {min}..={max}")?;
                match suggested {
                    Some(s) if s > max => f.write_str(&crate::port::over_cap(Some(*s), *max)),
                    Some(s) => write!(f, "; did you mean {s}?"),
                    None => Ok(()),
                }
            }
            OpError::Conflict {
                source,
                holders,
                current,
                options,
            } => write!(
                f,
                "source {} is held by {:?} at centre {} Hz, bandwidth {} Hz; options: {}",
                source.0,
                holders.iter().map(|j| j.0.as_str()).collect::<Vec<_>>(),
                current.center_hz.0,
                current.bandwidth_hz.0,
                options.join("; ")
            ),
            OpError::Unknown {
                field,
                value,
                known,
            } => {
                write!(f, "unknown {field} {value:?}; known: {}", known.join(", "))
            }
            OpError::Ring { detail } => write!(f, "{detail}"),
            OpError::Graph { detail } => write!(f, "{detail}"),
            OpError::Other { message } | OpError::Duplicate { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for OpError {}

/// Propose the value a caller probably meant when `value` is out of
/// `[min, max]` by whole powers of ten (dropped or extra zeros): multiply when
/// below `min`, divide (only when exact) when above `max`. The SI-prefix
/// scales 10^3 and 10^6 are tried first, then 10^1, 10^2, 10^4, 10^5; the
/// first that lands in range wins. Prefix-first matters for wide ranges: for
/// 101500 in 1 MHz..6 GHz plain smallest-k would give 1015000, not 101500000.
pub fn suggest(value: u64, min: u64, max: u64) -> Option<u64> {
    let in_range = |v: u64| (min..=max).contains(&v);
    [3u32, 6, 1, 2, 4, 5].into_iter().find_map(|k| {
        let p = 10u64.pow(k);
        let v = if value < min {
            value.checked_mul(p)?
        } else if value > max && value.is_multiple_of(p) {
            value / p
        } else {
            return None;
        };
        in_range(v).then_some(v)
    })
}

/// JSON schemas of `Op`, `OpResult` and `OpError` under `op`, `result`,
/// `error`. The `schema` operation answers with [`Node::schema`], which adds
/// the node's stage kinds under `stages`.
///
/// `$defs.JobKind` lists the built-in kinds and the embedded mode rows' ids (read from
/// the row files alone, without a stage registry); an operator row's id is accepted too.
pub fn schema() -> serde_json::Value {
    let mut v = serde_json::json!({
        "op": schemars::schema_for!(Op),
        "result": schemars::schema_for!(OpResult),
        "error": schemars::schema_for!(OpError),
    });
    let mut kinds: Vec<String> = BUILTIN_KINDS.iter().map(|k| k.name().into()).collect();
    for (_, text) in ModeRegistry::embedded() {
        let id = serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|row| row["id"].as_str().map(String::from));
        if let Some(id) = id.filter(|id| !kinds.contains(id)) {
            kinds.push(id);
        }
    }
    let job_kind = serde_json::json!({
        "type": "string",
        "enum": kinds,
        "description": "A built-in job kind or an embedded mode's id, or the id of an operator mode (modes_list)",
    });
    for part in ["op", "result", "error"] {
        if let Some(defs) = v[part]["$defs"].as_object_mut()
            && defs.contains_key("JobKind")
        {
            defs.insert("JobKind".into(), job_kind.clone());
        }
    }
    v
}

fn other(e: impl std::fmt::Display) -> OpError {
    OpError::Other {
        message: e.to_string(),
    }
}

fn range(field: &str, value: u64, min: u64, max: u64) -> Result<(), OpError> {
    if (min..=max).contains(&value) {
        return Ok(());
    }
    Err(OpError::OutOfRange {
        field: field.into(),
        value,
        min,
        max,
        suggested: suggest(value, min, max),
    })
}

fn unknown_source(node: &Node, source: &SourceId) -> OpError {
    let mut known: Vec<String> = node.ports().into_iter().map(|(id, _)| id.0).collect();
    known.sort();
    OpError::Unknown {
        field: "source".into(),
        value: source.0.clone(),
        known,
    }
}

fn unknown_job(node: &Node, job: &JobId) -> OpError {
    let known = node.job_list().into_iter().map(|r| r.id.0).collect();
    OpError::Unknown {
        field: "job".into(),
        value: job.0.clone(),
        known,
    }
}

fn port_error(e: PortError) -> OpError {
    match e {
        PortError::OutOfRange {
            field,
            value,
            min,
            max,
            suggested,
        } => OpError::OutOfRange {
            field: field.into(),
            value,
            min,
            max,
            suggested: suggested.or_else(|| suggest(value, min, max)),
        },
        e => other(e),
    }
}

fn lease_error(e: LeaseError) -> OpError {
    match e {
        LeaseError::Conflict {
            source,
            holders,
            current,
            options,
        } => OpError::Conflict {
            source,
            holders,
            current,
            options,
        },
        LeaseError::Port(p) => port_error(p),
    }
}

fn node_error(node: &Node, e: NodeError) -> OpError {
    match e {
        NodeError::UnknownSource(s) => unknown_source(node, &s),
        NodeError::UnknownJob(j) => unknown_job(node, &j),
        NodeError::Graph(detail) => OpError::Graph { detail },
        NodeError::Lease(l) => lease_error(l),
        // A range error keeps its corrective shape (`field`, the range, `suggested`);
        // anything else reads as the source's failure.
        NodeError::SourcePort(_, p @ PortError::OutOfRange { .. }) => port_error(p),
        e @ NodeError::SourcePort(..) => other(e),
        NodeError::InputNotRunning(j) => OpError::Other {
            message: format!("job {} is not running", j.0),
        },
        NodeError::NoAudioStream(j) => OpError::Other {
            message: format!("job {} has no audio stream", j.0),
        },
        NodeError::Expand(e) => e,
        NodeError::Other(message) => OpError::Other { message },
        e @ (NodeError::Io(_) | NodeError::DuplicateSource(_) | NodeError::Stopped(_)) => other(e),
    }
}

/// The source's port, once `want` is checked against its declared ranges, so
/// the corrective error never depends on the port's own `tune`.
fn checked_port(
    node: &Node,
    source: &SourceId,
    want: &Tuning,
) -> Result<std::sync::Arc<dyn Port>, OpError> {
    checked_port_up_to(node, source, want, |caps| caps.max_bandwidth_hz.value().0)
}

/// [`checked_port`] with the widest band given by `max_band` of the
/// capabilities: a scan's band may be as wide as the tune range plus one capture.
fn checked_port_up_to(
    node: &Node,
    source: &SourceId,
    want: &Tuning,
    max_band: impl Fn(&Capabilities) -> u64,
) -> Result<std::sync::Arc<dyn Port>, OpError> {
    let port = node
        .port(source)
        .ok_or_else(|| unknown_source(node, source))?;
    let caps = port.capabilities();
    range(
        "center_hz",
        want.center_hz.0,
        caps.tune_min_hz.value().0,
        caps.tune_max_hz.value().0,
    )?;
    range("bandwidth_hz", want.bandwidth_hz.0, 1, max_band(caps))?;
    Ok(port)
}

/// The capture band `id` leased on `port`: the port's current band, else the
/// first holder's (as the lease manager takes it), or `None` once `id` holds
/// no lease (the job already ended).
fn leased_band(node: &Node, port: &dyn Port, id: &JobId) -> Option<Tuning> {
    let holders = node.lease_holders(port.id());
    if !holders.iter().any(|(j, _)| j == id) {
        return None;
    }
    port.current().or_else(|| holders.first().map(|(_, t)| *t))
}

/// `job_start` expands against the pre-tune rate and leases under the same
/// hold of the source's gate as the tune, so no other job can lease in between;
/// this stays as a defensive check for a port whose `current()` moves outside
/// the gate (a fault recovery retuning it), when the lease may share a capture
/// at another rate. Re-expand against the leased rate and relaunch the
/// job under the same id (`job_graph_put`), once; if the rate moves again the
/// job is stopped and the start is a `Conflict`. Kinds without a `channel`
/// node do not depend on the rate and are left alone.
async fn reconcile_rate(
    node: &Node,
    port: &dyn Port,
    id: &JobId,
    kind: JobKind,
    tuning: Tuning,
    (deviation_hz, sideband): (Option<Hz>, Option<Sideband>),
    mut expected: Hz,
) -> Result<(), OpError> {
    if !kind.channelised() {
        return Ok(());
    }
    for attempt in 0..2 {
        let Some(leased) = leased_band(node, port, id) else {
            return Ok(());
        };
        if leased.bandwidth_hz == expected {
            return Ok(());
        }
        if attempt == 1 {
            // Best effort: the conflict is the answer whether or not the stop lands.
            let _ = node.job_stop(id).await;
            let holders = node
                .lease_holders(port.id())
                .into_iter()
                .map(|(j, _)| j)
                .filter(|j| j != id)
                .collect();
            return Err(OpError::Conflict {
                source: port.id().clone(),
                holders,
                current: leased,
                options: vec!["retry job_start".into(), "stop one of the holders".into()],
            });
        }
        let graph = match kind.expand(
            port.id(),
            tuning,
            leased.bandwidth_hz,
            node.bank_bin_rate(port.id(), leased),
            deviation_hz,
            sideband,
            &node.modes(),
        ) {
            Ok(g) => g,
            Err(e) => {
                let _ = node.job_stop(id).await;
                return Err(e);
            }
        };
        node.job_graph_set(id, graph, Some(leased.bandwidth_hz))
            .await
            .map_err(|e| node_error(node, e))?;
        expected = leased.bandwidth_hz;
    }
    Ok(())
}

/// `job_start`'s checks of `sideband` (only `ssb_demod` takes one) and `deviation_hz`
/// (only `fm_demod` and `wfm_demod`, within 100..=75000 Hz); the deviation as `Hz`.
fn check_start_params(
    kind: &JobKind,
    deviation_hz: Option<u64>,
    sideband: Option<Sideband>,
) -> Result<Option<Hz>, OpError> {
    if sideband.is_some() && *kind != JobKind::SsbDemod {
        return Err(other(format!(
            "sideband applies only to kind ssb_demod; pass null (kind is {})",
            kind.name()
        )));
    }
    match deviation_hz {
        None => Ok(None),
        Some(_) if !matches!(kind, JobKind::FmDemod | JobKind::WfmDemod) => Err(other(format!(
            "deviation_hz applies only to kinds fm_demod and wfm_demod; pass null (kind is {})",
            kind.name()
        ))),
        Some(d) => {
            range("deviation_hz", d, FM_DEVIATION_MIN_HZ, FM_DEVIATION_MAX_HZ)?;
            Ok(Some(Hz(d)))
        }
    }
}

/// Stop the `replace` holders under `guard` (see [`Node::replace_holders`]);
/// nothing to do without a guard, which [`Node::gate_for_replace`] gives only
/// when `replace` names jobs.
async fn replace_under(
    node: &Node,
    source: &SourceId,
    replace: &[JobId],
    guard: Option<&tokio::sync::OwnedMutexGuard<()>>,
) -> Result<(), NodeError> {
    match guard {
        Some(g) => node.replace_holders(source, replace, g).await,
        None => Ok(()),
    }
}

/// After a request with `replace` failed with `e`, outside the source's gate:
/// the named jobs were stopped without idling the port (their stop ran under
/// the gate), so idle it now if nobody holds it. Nothing was stopped when `e`
/// is the refusal of an id, and then nothing is idled.
async fn idle_after_replace(node: &Node, port: &dyn Port, replace: &[JobId], e: &NodeError) {
    if replace.is_empty() || matches!(e, NodeError::UnknownJob(_) | NodeError::Other(_)) {
        return;
    }
    node.idle_if_unheld(port).await;
}

pub(crate) async fn apply(node: &Node, op: Op) -> Result<OpResult, OpError> {
    match op {
        Op::SourceList {} => {
            let mut v: Vec<SourceInfo> = node
                .ports()
                .into_iter()
                .map(|(id, capabilities)| SourceInfo {
                    current: node.port(&id).and_then(|p| p.current()),
                    holders: node
                        .lease_holders(&id)
                        .into_iter()
                        .map(|(j, _)| j)
                        .collect(),
                    elastic: node.lease_elastic(&id),
                    pin: node.pinned(&id),
                    fault: node.source_fault(&id),
                    bank: node.bank(&id).map(|b| BankInfo {
                        bin_hz: b.bin_hz(),
                        consumers: b.consumers(),
                        dropped: b.dropped(),
                    }),
                    gain: node.port(&id).and_then(|p| p.gain()),
                    stats: node.port(&id).and_then(|p| p.stats()),
                    worker: node.port(&id).and_then(|p| p.worker()),
                    ring: node.ring(&id).map(|r| RingInfo {
                        seconds: r.read().unwrap().capacity_seconds(),
                    }),
                    id,
                    capabilities,
                })
                .collect();
            v.sort_by(|a, b| a.id.0.cmp(&b.id.0));
            Ok(OpResult::Sources(v))
        }
        Op::SourceEnsure {
            source,
            center_hz,
            bandwidth_hz,
            replace,
        } => {
            let want = Tuning {
                center_hz,
                bandwidth_hz,
            };
            let port = checked_port(node, &source, &want)?;
            let guard = node.gate_for_replace(&source, &replace).await;
            let tuned = match replace_under(node, &source, &replace, guard.as_ref()).await {
                Ok(()) => node
                    .tune_if_unheld(port.as_ref(), want, guard.as_ref())
                    .await
                    .map_err(NodeError::Lease),
                Err(e) => Err(e),
            };
            drop(guard);
            let (got, tuned) = match tuned {
                Ok(t) => t,
                Err(e) => {
                    idle_after_replace(node, port.as_ref(), &replace, &e).await;
                    return Err(node_error(node, e));
                }
            };
            // Outside the gate: the relaunched jobs' leases take it.
            if tuned {
                node.relaunch_elastic(&source).await;
            }
            Ok(OpResult::Tuning(got))
        }
        Op::SourcePin {
            source,
            center_hz,
            bandwidth_hz,
            replace,
        } => {
            let want = Tuning {
                center_hz,
                bandwidth_hz,
            };
            checked_port(node, &source, &want)?;
            node.source_pin(&source, want, &replace)
                .await
                .map(OpResult::Tuning)
                .map_err(|e| node_error(node, e))
        }
        Op::SourceFree {
            center_hz,
            bandwidth_hz,
        } => {
            let (free, busy) = node.source_free(center_hz.0, bandwidth_hz.0);
            Ok(OpResult::SourceFree { free, busy })
        }
        Op::SourceUnpin { source } => {
            if node.port(&source).is_none() {
                return Err(unknown_source(node, &source));
            }
            node.source_unpin(&source)
                .await
                .map(|()| OpResult::Ok)
                .map_err(|e| node_error(node, e))
        }
        Op::SourceSetGain { source, gain_db } => {
            if node.port(&source).is_none() {
                return Err(unknown_source(node, &source));
            }
            node.source_set_gain(&source, gain_db)
                .await
                .map(OpResult::Gain)
                .map_err(|e| node_error(node, e))
        }
        Op::JobStart {
            kind,
            source,
            center_hz,
            bandwidth_hz,
            deviation_hz,
            sideband,
            elastic,
            replace,
        } => {
            if !kind.startable() {
                return Err(kind.start_refusal());
            }
            if elastic && !kind.elastic_capable() {
                return Err(OpError::Other {
                    message: format!("{} cannot be elastic", kind.name()),
                });
            }
            let deviation = check_start_params(&kind, deviation_hz, sideband)?;
            // An unknown or unready mode is refused before anything is tuned.
            kind.mode_row(&node.modes())?;
            let tuning = Tuning {
                center_hz,
                bandwidth_hz,
            };
            if kind == JobKind::Scan {
                // No tune here: the scan leases its whole band and steps the
                // source itself; a held source is a conflict at the lease.
                let port = checked_port_up_to(node, &source, &tuning, |caps| {
                    let tune =
                        (caps.tune_max_hz.value().0).saturating_sub(caps.tune_min_hz.value().0);
                    tune.saturating_add(caps.max_bandwidth_hz.value().0)
                })?;
                let graph = kind.expand(&source, tuning, Hz(0), None, None, None, &node.modes())?;
                // With `replace`, the named holders stop and the scan leases under
                // one hold of the gate; the elastic jobs it displaces stop after.
                let guard = node.gate_for_replace(&source, &replace).await;
                let g = guard.as_ref();
                let (started, after) = match replace_under(node, &source, &replace, g).await {
                    Ok(()) => {
                        node.job_start_gated(kind.name(), source, tuning, graph, None, false, g)
                            .await
                    }
                    Err(e) => (Err(e), AfterGate::Nothing),
                };
                drop(guard);
                node.after_gate(after).await;
                return match started {
                    Ok(id) => Ok(OpResult::Job(id)),
                    Err(e) => {
                        idle_after_replace(node, port.as_ref(), &replace, &e).await;
                        Err(node_error(node, e))
                    }
                };
            }
            let port = checked_port(node, &source, &tuning)?;
            // With `replace`, one hold of the source's gate covers stopping the named
            // holders, the tune and the new job's lease: nothing takes the radio in
            // between. The gate is taken always: the duplicate check below must not
            // race a start of the same job.
            let guard = Some(node.gate_guard(&source).await);
            // An identical rigid job already runs: refuse, naming it, before anything
            // is stopped or tuned, so a `replace` on a refused start stops nothing. A
            // match that `replace` names is about to stop, so it is no duplicate.
            if !elastic
                && let Some(job) = node
                    .duplicate_of(
                        &source,
                        kind.name(),
                        tuning,
                        deviation.map(|d| d.0),
                        sideband,
                    )
                    .filter(|j| !replace.contains(j))
            {
                drop(guard);
                return Err(OpError::Duplicate {
                    message: format!(
                        "an identical {} already runs on {} at {}/{} Hz as job {}",
                        kind.name(),
                        source.0,
                        tuning.center_hz.0,
                        tuning.bandwidth_hz.0,
                        job.0
                    ),
                    job,
                });
            }
            if let Err(e) = replace_under(node, &source, &replace, guard.as_ref()).await {
                drop(guard);
                idle_after_replace(node, port.as_ref(), &replace, &e).await;
                return Err(node_error(node, e));
            }
            // The granted capture band: its width is the source rate (Port contract).
            // An elastic job never tunes here: it joins the current capture (or tunes
            // an unheld source to `tuning`) at its lease, which also re-expands its
            // graph for the capture. A rigid tune here relaunches the elastic jobs once
            // the gate is let go (the job's lease, inside the new capture, tunes nothing).
            let (granted, tuned) = match elastic {
                true => (port.current().unwrap_or(tuning), false),
                false => match node
                    .tune_if_unheld(port.as_ref(), tuning, guard.as_ref())
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        drop(guard);
                        // A tune the port refused reads as the source's failure, as
                        // `launch_inner` words it (a conflict stays a conflict).
                        let e = match e {
                            LeaseError::Port(p) => NodeError::SourcePort(source.clone(), p),
                            e => NodeError::Lease(e),
                        };
                        idle_after_replace(node, port.as_ref(), &replace, &e).await;
                        return Err(node_error(node, e));
                    }
                },
            };
            let bin_rate = node.bank_bin_rate(&source, granted);
            let (started, after) = match kind.expand(
                &source,
                tuning,
                granted.bandwidth_hz,
                bin_rate,
                deviation,
                sideband,
                &node.modes(),
            ) {
                Ok(graph) => {
                    let (r, after) = node
                        .job_start_gated(
                            kind.name(),
                            source.clone(),
                            tuning,
                            graph,
                            Some(granted.bandwidth_hz),
                            elastic,
                            guard.as_ref(),
                        )
                        .await;
                    // The tune above moved the capture; the lease, inside it,
                    // tuned nothing more, so the elastic jobs follow from here.
                    let after = match after {
                        AfterGate::Nothing if tuned && r.is_ok() => AfterGate::Relaunch {
                            source: source.clone(),
                            except: None,
                        },
                        after => after,
                    };
                    (r.map_err(|e| node_error(node, e)), after)
                }
                Err(e) => (Err(e), AfterGate::Nothing),
            };
            drop(guard);
            node.after_gate(after).await;
            let id = match started {
                Ok(id) => id,
                Err(e) => {
                    if !replace.is_empty() {
                        node.idle_if_unheld(port.as_ref()).await;
                    }
                    // The tune above moved the capture and no lease followed it.
                    if tuned {
                        node.relaunch_elastic(&source).await;
                    }
                    return Err(e);
                }
            };
            reconcile_rate(
                node,
                port.as_ref(),
                &id,
                kind,
                tuning,
                (deviation, sideband),
                granted.bandwidth_hz,
            )
            .await?;
            Ok(OpResult::Job(id))
        }
        Op::JobAttach {
            kind,
            job,
            language,
        } => job_attach(node, kind, job, language).await,
        Op::JobRetune {
            job,
            center_hz,
            bandwidth_hz,
            replace,
        } => job_retune(node, job, center_hz, bandwidth_hz, replace).await,
        Op::PlanStart {
            plan,
            channels,
            kind,
            source,
            deviation_hz,
            sideband,
            attach,
            language,
            replace,
        } => {
            plan_start(
                node,
                plan,
                channels,
                kind,
                source,
                (deviation_hz, sideband),
                (attach, language),
                replace,
            )
            .await
        }
        Op::StationSet {
            lat,
            lon,
            alt_m,
            region,
            antennas,
        } => {
            let antennas = antennas.unwrap_or_else(|| Station::default_antennas(alt_m));
            let station = Station {
                lat,
                lon,
                alt_m,
                region,
                antennas,
            };
            station.validate().map_err(other)?;
            node.set_station(station)
                .await
                .map(|s| OpResult::Station(Some(s)))
                .map_err(|e| node_error(node, e))
        }
        Op::StationGet {} => Ok(OpResult::Station(node.station())),
        Op::StationClear {} => node
            .clear_station()
            .await
            .map(|()| OpResult::Station(None))
            .map_err(|e| node_error(node, e)),
        Op::CaptureList { signal } => {
            let mut v = node.bursts().list();
            if let Some(b) = signal {
                let (lo, hi) = band_edges(b.center_hz, b.bandwidth_hz);
                v.retain(|r| {
                    let (rlo, rhi) = band_edges(r.center_hz, r.bandwidth_hz);
                    rlo < hi && lo < rhi
                });
            }
            Ok(OpResult::Captures(v))
        }
        Op::CaptureShow { capture } => {
            let record = capture_record(node, &capture)?;
            let meta = std::fs::read(&record.meta_path)
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
                .map_err(|e| other(format!("capture {capture}: {e}")))?;
            Ok(OpResult::Capture { record, meta })
        }
        Op::CaptureClassify { capture } => capture_classify(node, &capture).await,
        Op::CaptureDelete { capture } => {
            capture_record(node, &capture)?;
            node.bursts()
                .delete(&capture)
                .map(|_| OpResult::Ok)
                .map_err(other)
        }
        Op::MixStart { sources } => node
            .mix_start(sources)
            .await
            .map(OpResult::Job)
            .map_err(|e| node_error(node, e)),
        Op::JobList {} => Ok(OpResult::Jobs(node.job_list())),
        Op::JobStop { job } => node
            .job_stop(&job)
            .await
            .map(|()| OpResult::Ok)
            .map_err(|e| node_error(node, e)),
        Op::JobsStop { source, kind, plan } => jobs_stop(node, source, kind, plan).await,
        Op::JobContext { job } => node
            .job_context(&job)
            .map(OpResult::Context)
            .ok_or_else(|| unknown_job(node, &job)),
        Op::IqRead { selection } => iq_read(node, selection),
        Op::StreamList {} => {
            let mut v = node.bus().list();
            v.sort_by(|a, b| a.id.0.cmp(&b.id.0));
            Ok(OpResult::Streams(v))
        }
        Op::StreamTail {
            stream,
            max_frames,
            timeout_ms,
        } => stream_tail(node, stream, max_frames, timeout_ms).await,
        Op::GraphGet { job } => node
            .job_list()
            .into_iter()
            .find(|r| r.id == job)
            .map(|r| OpResult::Graph(r.graph))
            .ok_or_else(|| unknown_job(node, &job)),
        Op::GraphPut { job, graph } => node
            .job_graph_put(&job, graph)
            .await
            .map(|()| OpResult::Ok)
            .map_err(|e| node_error(node, e)),
        Op::GraphStart { graph } => node
            .graph_start(graph)
            .await
            .map(OpResult::Job)
            .map_err(|e| node_error(node, e)),
        Op::BitsStart {
            capture,
            modulation,
            symbol_rate_baud,
            order,
        } => node
            .bits_start(&capture, modulation, symbol_rate_baud, order)
            .await
            .map(OpResult::Job)
            .map_err(|e| node_error(node, e)),
        Op::KnowledgeLookup {
            region,
            center_hz,
            bandwidth_hz,
        } => {
            let (mut allocations, mut signatures, mut reach) = (Vec::new(), Vec::new(), None);
            for p in node.knowledge() {
                allocations.extend(p.allocations(&region, center_hz, bandwidth_hz).await);
                signatures.extend(p.signatures(center_hz, bandwidth_hz).await);
                if reach.is_none() {
                    reach = p.reach(&region, center_hz, bandwidth_hz).await;
                }
            }
            Ok(OpResult::Knowledge {
                allocations,
                signatures,
                reach,
            })
        }
        Op::ChannelPlans {} => Ok(OpResult::ChannelPlans {
            plans: crate::plans::PLANS.iter().map(PlanInfo::from).collect(),
        }),
        Op::ModesList { family, state } => Ok(OpResult::Modes {
            modes: node
                .modes()
                .all()
                .iter()
                .filter(|m| family.is_none_or(|f| m.row.family == f))
                .filter(|m| state.is_none_or(|s| m.state == s))
                .map(ModeInfo::from)
                .collect(),
        }),
        Op::ModesShow { id } => {
            let modes = node.modes();
            match modes.get(&id) {
                Some(m) => Ok(OpResult::Mode {
                    mode: Box::new(m.clone()),
                }),
                None => Err(OpError::Unknown {
                    field: "id".into(),
                    value: id,
                    known: modes.ids(),
                }),
            }
        }
        Op::Classify { selection } => classify(node, selection).await,
        Op::Schema {} => Ok(OpResult::Schema(node.schema())),
    }
}

/// `jobs_stop`: stop the running jobs the filters match, those on a radio first (an
/// attached job then usually ends with its input, and stopping it again is a no-op). A
/// record already forgotten from the history has stopped too.
async fn jobs_stop(
    node: &Node,
    source: Option<SourceId>,
    kind: Option<JobKind>,
    plan: Option<String>,
) -> Result<OpResult, OpError> {
    if let Some(s) = &source
        && node.port(s).is_none()
    {
        return Err(unknown_source(node, s));
    }
    let channels = match &plan {
        None => None,
        Some(id) => {
            let Some(p) = crate::plans::plan(id) else {
                return Err(OpError::Unknown {
                    field: "plan".into(),
                    value: id.clone(),
                    known: crate::plans::PLANS
                        .iter()
                        .map(|p| p.id.to_string())
                        .collect(),
                });
            };
            Some(crate::plans::channels(p))
        }
    };
    let on_plan = |hz: u64| {
        channels.as_ref().is_none_or(|c| {
            c.iter()
                .any(|&(_, center)| center.abs_diff(hz) <= crate::plans::CHANNEL_MATCH_HZ)
        })
    };
    let mut matched: Vec<_> = node
        .job_list()
        .into_iter()
        .filter(|r| {
            r.state == JobState::Running
                && !r.elastic
                && r.owner.is_none()
                && source.as_ref().is_none_or(|s| r.source == *s)
                && kind.as_ref().is_none_or(|k| r.kind == k.name())
                && on_plan(r.tuning.center_hz.0)
                // A leaseless job's centre is its capture's, not a channel it holds.
                && (plan.is_none() || !r.leaseless())
        })
        .collect();
    matched.sort_by_key(|r| r.input.is_some());
    let (mut stopped, mut failed) = (Vec::new(), Vec::new());
    for r in matched {
        match node.job_stop(&r.id).await {
            Ok(()) | Err(NodeError::UnknownJob(_)) => stopped.push(r.id),
            Err(e) => failed.push(JobStopFailed {
                job: r.id,
                error: node_error(node, e),
            }),
        }
    }
    Ok(OpResult::JobsStopped { stopped, failed })
}

/// `job_attach`: check the kind and language, then attach to `job`'s audio stream.
async fn job_attach(
    node: &Node,
    kind: JobKind,
    job: JobId,
    language: Option<String>,
) -> Result<OpResult, OpError> {
    if !kind.attachable() {
        return Err(other(format!("{} is not attachable", kind.name())));
    }
    let language = language.unwrap_or_else(|| ATTACH_LANGUAGE_DEFAULT.into());
    if !is_bcp47(&language) {
        return Err(other(format!(
            "language {language:?} is not a BCP-47 code such as en, de or pt-BR; pass null for en"
        )));
    }
    if !node.job_list().iter().any(|r| r.id == job) {
        return Err(unknown_job(node, &job));
    }
    if let Some(r) = node.stage_readiness(kind.name())
        && !r.ready
    {
        return Err(other(format!(
            "{}: {}",
            kind.name(),
            r.reason.unwrap_or_else(|| "not ready".into())
        )));
    }
    attach_one(node, &kind, &job, &language)
        .await
        .map(OpResult::Job)
}

/// The load warning `plan_start` gives when more jobs of the attached `kind` run than
/// its stage has slots (`capacity`).
fn load_warning(n: usize, kind: &JobKind, capacity: u32) -> String {
    let stage = match kind {
        JobKind::Transcribe => "speech backend".to_string(),
        k => format!("{} stage", k.name()),
    };
    format!(
        "{n} {} jobs share the {stage}'s {capacity} slots: with the squelch on most channels are silent, so the slots are rarely contested; a busy daemon drops utterances with reason busy",
        kind.name()
    )
}

/// `plan_start`: validate, find the channels already running, preflight the attached
/// stage, stop `replace`, take one lease decision for the band bounding the channels to
/// start, start each in channel-number order under the same hold of the source's gate,
/// then attach to the started jobs (and to the existing ones without one).
#[allow(clippy::too_many_arguments)]
async fn plan_start(
    node: &Node,
    plan: String,
    channels: Option<Vec<u32>>,
    kind: JobKind,
    source: SourceId,
    (deviation_hz, sideband): (Option<u64>, Option<Sideband>),
    (attach, language): (Option<JobKind>, Option<String>),
    replace: Vec<JobId>,
) -> Result<OpResult, OpError> {
    // 1. Validate: nothing has happened yet.
    let Some(p) = crate::plans::plan(&plan) else {
        return Err(OpError::Unknown {
            field: "plan".into(),
            value: plan,
            known: crate::plans::PLANS.iter().map(|p| p.id.into()).collect(),
        });
    };
    let all = crate::plans::channels(p);
    let mut known: Vec<u32> = all.iter().map(|&(n, _)| n).collect();
    known.sort_unstable();
    let numbers: Vec<u32> = match channels {
        None => known,
        Some(listed) => {
            let mut seen = Vec::with_capacity(listed.len());
            for n in listed {
                if !known.contains(&n) {
                    return Err(OpError::Unknown {
                        field: "channels".into(),
                        value: n.to_string(),
                        known: known.iter().map(u32::to_string).collect(),
                    });
                }
                if seen.contains(&n) {
                    return Err(other(format!("channels: {n} is listed twice")));
                }
                seen.push(n);
            }
            seen.sort_unstable();
            seen
        }
    };
    if !kind.startable()
        || matches!(
            kind,
            JobKind::Spectrum | JobKind::Scan | JobKind::Watch | JobKind::Identify
        )
    {
        return Err(other(format!(
            "{} cannot be started per channel",
            kind.name()
        )));
    }
    let deviation = check_start_params(&kind, deviation_hz, sideband)?;
    if let Some(a) = &attach
        && !a.attachable()
    {
        return Err(other(format!("{} is not attachable", a.name())));
    }
    if language.is_some() && attach.is_none() {
        return Err(other("language needs attach"));
    }
    let language = language.unwrap_or_else(|| ATTACH_LANGUAGE_DEFAULT.into());
    if !is_bcp47(&language) {
        return Err(other(format!(
            "language {language:?} is not a BCP-47 code such as en, de or pt-BR; pass null for en"
        )));
    }
    kind.mode_row(&node.modes())?;
    let port = node
        .port(&source)
        .ok_or_else(|| unknown_source(node, &source))?;
    let channel_band = |n: u32| Tuning {
        center_hz: Hz(all.iter().find(|&&(m, _)| m == n).map_or(0, |&(_, hz)| hz)),
        bandwidth_hz: Hz(p.bandwidth_hz),
    };

    // One hold of the source's gate covers every step that follows up to the last
    // start: no other caller's tune slips between the lease decision and the starts.
    let guard = node.gate_guard(&source).await;
    // 2. The channels an identical rigid job already runs on (one that `replace` names
    // is about to stop, so its channel is started again).
    let mut existing = Vec::new();
    let mut to_start = Vec::new();
    for &n in &numbers {
        let band = channel_band(n);
        let dup = node
            .duplicate_of(&source, kind.name(), band, deviation.map(|d| d.0), sideband)
            .filter(|j| !replace.contains(j));
        match dup {
            Some(job) => existing.push(PlanExisting {
                channel: n,
                center_hz: band.center_hz,
                attached: attach.as_ref().and_then(|a| attached_to(node, &job, a)),
                job,
            }),
            None => to_start.push(n),
        }
    }
    // 3. The attached stage must be ready: refused, with nothing done, otherwise.
    let readiness = attach.as_ref().and_then(|a| node.stage_readiness(a.name()));
    if let (Some(a), Some(r)) = (&attach, &readiness)
        && !r.ready
    {
        return Err(other(format!(
            "{}: {}; start without attach",
            a.name(),
            r.reason.as_deref().unwrap_or("not ready")
        )));
    }
    // The bounding band against the source's ranges before anything is stopped: its
    // width is the corrective `out_of_range`.
    let bounding = crate::plans::bounding(p, &to_start);
    if !to_start.is_empty() {
        checked_port(node, &source, &bounding)?;
    }
    // 4. Replacements, under the gate.
    if let Err(e) = node.replace_holders(&source, &replace, &guard).await {
        drop(guard);
        idle_after_replace(node, port.as_ref(), &replace, &e).await;
        return Err(node_error(node, e));
    }
    let mut started = Vec::new();
    let mut refused = Vec::new();
    let capture = if to_start.is_empty() {
        drop(guard);
        if !replace.is_empty() {
            node.idle_if_unheld(port.as_ref()).await;
        }
        port.current()
            .unwrap_or_else(|| crate::plans::bounding(p, &numbers))
    } else {
        // 5. One lease decision for the bounding band, taken as a rigid start of it
        // would be. The lease is held only until the channels hold theirs inside its
        // capture: a rigid lease on a source no rigid job holds would retune it to that
        // one channel.
        let (hold, granted, tuned) = match node.hold_band(port.as_ref(), bounding, &guard).await {
            Ok(t) => t,
            Err(e) => {
                drop(guard);
                // A tune the port refused reads as the source's failure, as
                // `job_start` words it (a conflict stays a conflict).
                let e = match e {
                    LeaseError::Port(p) => NodeError::SourcePort(source.clone(), p),
                    e => NodeError::Lease(e),
                };
                idle_after_replace(node, port.as_ref(), &replace, &e).await;
                return Err(node_error(node, e));
            }
        };
        // 6. Each channel inside the capture, in channel-number order; a start that
        // fails is that channel's refusal and the rest go on.
        let bin_rate = node.bank_bin_rate(&source, granted);
        let modes = node.modes();
        // The tune above moved the capture: the elastic jobs follow once the gate goes.
        let mut after = match tuned {
            true => AfterGate::Relaunch {
                source: source.clone(),
                except: None,
            },
            false => AfterGate::Nothing,
        };
        for n in to_start {
            let tuning = channel_band(n);
            let r = match kind.expand(
                &source,
                tuning,
                granted.bandwidth_hz,
                bin_rate,
                deviation,
                sideband,
                &modes,
            ) {
                Ok(graph) => {
                    let (r, a) = node
                        .job_start_gated(
                            kind.name(),
                            source.clone(),
                            tuning,
                            graph,
                            Some(granted.bandwidth_hz),
                            false,
                            Some(&guard),
                        )
                        .await;
                    after = match (after, a) {
                        (AfterGate::Nothing, a) => a,
                        (merged, AfterGate::Nothing) => merged,
                        _ => AfterGate::Relaunch {
                            source: source.clone(),
                            except: None,
                        },
                    };
                    r.map_err(|e| node_error(node, e))
                }
                Err(e) => Err(e),
            };
            match r {
                Ok(job) => started.push(PlanStarted {
                    channel: n,
                    center_hz: tuning.center_hz,
                    job,
                    attached: None,
                    attach_error: None,
                }),
                Err(error) => refused.push(PlanRefused {
                    channel: n,
                    center_hz: tuning.center_hz,
                    error,
                }),
            }
        }
        drop(hold);
        drop(guard);
        node.after_gate(after).await;
        if started.is_empty() && !replace.is_empty() {
            node.idle_if_unheld(port.as_ref()).await;
        }
        granted
    };
    // 7. Attach to every started job, and to each existing one without one; the
    // attach needs no gate (an attached job holds no lease).
    let mut warnings = Vec::new();
    if let Some(a) = &attach {
        for s in &mut started {
            match attach_one(node, a, &s.job, &language).await {
                Ok(id) => s.attached = Some(id),
                Err(e) => s.attach_error = Some(e),
            }
        }
        // An existing entry has no error field: one that cannot be attached stays
        // without, says why in a warning, and a second run tries again.
        for e in existing.iter_mut().filter(|e| e.attached.is_none()) {
            match attach_one(node, a, &e.job, &language).await {
                Ok(id) => e.attached = Some(id),
                Err(err) => warnings.push(format!("ch{}: attach failed: {err}", e.channel)),
            }
        }
        if let Some(c) = readiness.and_then(|r| r.capacity) {
            let n = node
                .job_list()
                .into_iter()
                .filter(|r| r.state == JobState::Running && r.kind == a.name())
                .count();
            if n > c as usize {
                warnings.push(load_warning(n, a, c));
            }
        }
    }
    Ok(OpResult::PlanStart {
        plan,
        source,
        capture,
        started,
        existing,
        refused,
        warnings,
    })
}

/// The running job of `kind` attached to `job`, if any.
fn attached_to(node: &Node, job: &JobId, kind: &JobKind) -> Option<JobId> {
    node.job_list()
        .into_iter()
        .find(|r| {
            r.state == JobState::Running && r.input.as_ref() == Some(job) && r.kind == kind.name()
        })
        .map(|r| r.id)
}

/// `job_attach`'s tail: attach a job of `kind` to `job`'s audio stream, the
/// language and kind already checked.
async fn attach_one(
    node: &Node,
    kind: &JobKind,
    job: &JobId,
    language: &str,
) -> Result<JobId, OpError> {
    let Some(input) = node.job_list().into_iter().find(|r| &r.id == job) else {
        return Err(unknown_job(node, job));
    };
    let stream = node
        .job_audio_stream(job)
        .map_err(|e| node_error(node, e))?;
    let graph = kind.expand_attached(
        job,
        &stream,
        language,
        JobKind::graph_denoises(&input.graph),
    )?;
    node.job_attach(kind.name(), job, graph)
        .await
        .map_err(|e| node_error(node, e))
}

/// `job_retune`: refuse what cannot be retuned (an attached job, an operator's graph, a
/// kind the node does not expand or that is not retunable, a job that is not running, a
/// `replace` naming the job itself), check the band against the source's ranges and the
/// kind's row, then move the job's lease and relaunch it under its id.
async fn job_retune(
    node: &Node,
    job: JobId,
    center_hz: Hz,
    bandwidth_hz: Hz,
    replace: Vec<JobId>,
) -> Result<OpResult, OpError> {
    let Some(rec) = node.job_list().into_iter().find(|r| r.id == job) else {
        return Err(unknown_job(node, &job));
    };
    // Before the operator's-graph test: a mute's `graph_put` sets that flag on a mix.
    if rec.kind == JobKind::Mix.name() {
        return Err(other("mix is not retunable"));
    }
    if rec.leaseless() {
        return Err(other(format!(
            "{} has no band: it runs on no radio",
            rec.kind
        )));
    }
    if let Some(input) = &rec.input {
        return Err(other(format!(
            "{} is attached to job {}: retune that job",
            rec.kind, input.0
        )));
    }
    if rec.operator_graph {
        return Err(other(format!(
            "job {} runs an operator graph (graph_put): put a new graph or start a new job",
            job.0
        )));
    }
    let Some(kind) = node.expandable_kind(&rec.kind) else {
        return Err(other(format!(
            "{} is not a kind the node expands: start a new job",
            rec.kind
        )));
    };
    if !kind.retunable() {
        return Err(other(format!("{} is not retunable", kind.name())));
    }
    if rec.state != JobState::Running {
        let state = serde_json::to_value(&rec.state).unwrap_or_default();
        let status = state["status"].as_str().unwrap_or_default();
        return Err(other(format!("job {} is {status}", job.0)));
    }
    if replace.contains(&job) {
        return Err(other(format!("job {} cannot replace itself", job.0)));
    }
    let want = Tuning {
        center_hz,
        bandwidth_hz,
    };
    let port = checked_port(node, &rec.source, &want)?;
    // The row's own width rule (a program row's `min_hz`), before the gate: the
    // expansion would refuse it only after the lease moved and the radio tuned.
    if let Some(row) = kind.mode_row(&node.modes())? {
        let max = port.capabilities().max_bandwidth_hz.value().0;
        template::check_band(row, want, max)?;
    }
    match node.job_retune(&job, &kind, want, &replace).await {
        Ok(()) => Ok(OpResult::Job(job)),
        Err(e) => {
            idle_after_replace(node, port.as_ref(), &replace, &e).await;
            Err(node_error(node, e))
        }
    }
}

/// The ring's sample rate and centre and the selection's samples, once the
/// selection's band is checked to lie within the ring's band.
fn ring_samples(node: &Node, sel: &Selection) -> Result<(Hz, Hz, Vec<Complex<f32>>), OpError> {
    if node.port(&sel.source).is_none() {
        return Err(unknown_source(node, &sel.source));
    }
    let (start_ns, end_ns) = (sel.span.start_ns, sel.span.end_ns);
    if end_ns.0 <= start_ns.0 {
        // Reported even before the ring has data, as the ring itself would.
        return Err(OpError::Ring {
            detail: RingError::BadSpan { start_ns, end_ns },
        });
    }
    let empty = || OpError::Ring {
        detail: RingError::Empty,
    };
    let ring = node.ring(&sel.source).ok_or_else(empty)?;
    let r = ring.read().unwrap();
    let (rate, center) = (r.sample_rate_hz(), r.center_hz().ok_or_else(empty)?);
    check_band(sel, center, rate)?;
    let samples = r
        .read(sel.span.clone())
        .map_err(|detail| OpError::Ring { detail })?;
    Ok((rate, center, samples))
}

/// `[center - bw/2, center + bw/2]` of the selection must lie within the
/// ring's `[center - rate/2, center + rate/2]`, with a bandwidth of at least 1 Hz.
fn check_band(sel: &Selection, center: Hz, rate: Hz) -> Result<(), OpError> {
    range("selection.bandwidth_hz", sel.bandwidth_hz.0, 1, rate.0)?;
    let (half, ring_half) = (sel.bandwidth_hz.0 / 2, rate.0 / 2);
    range(
        "selection.center_hz",
        sel.center_hz.0,
        center.0.saturating_sub(ring_half).saturating_add(half),
        center.0.saturating_add(ring_half).saturating_sub(half),
    )
}

/// `[lo, hi)` in Hz of a band, the low edge clamped at 0.
fn band_edges(center: Hz, bandwidth: Hz) -> (u64, u64) {
    let half = bandwidth.0 / 2;
    (
        center.0.saturating_sub(half),
        center.0.saturating_add(bandwidth.0 - half),
    )
}

/// The store's record `id`, else an `unknown` error naming `capture` and the known ids.
fn capture_record(node: &Node, id: &str) -> Result<crate::bursts::CaptureRecord, OpError> {
    let store = node.bursts();
    store.get(id).ok_or_else(|| OpError::Unknown {
        field: "capture".into(),
        value: id.into(),
        known: store.list().into_iter().map(|r| r.id).collect(),
    })
}

/// `capture_classify`: the classifiers over the capture's samples in the record's band, with the
/// allocations the record kept.
async fn capture_classify(node: &Node, id: &str) -> Result<OpResult, OpError> {
    let rec = capture_record(node, id)?;
    let Classification {
        labels,
        tried,
        allocations,
    } = classify_capture(&node.classifiers(), &rec)
        .await
        .map_err(other)?;
    Ok(OpResult::Classification {
        labels,
        tried,
        allocations,
    })
}

/// The classifiers `set` over the capture `rec`'s samples, whole band, with the
/// allocations the record kept: `capture_classify`, and a hand `capture` job's
/// end-of-run classify of its first window.
pub(crate) async fn classify_capture(
    set: &crate::classify::ClassifierSet,
    rec: &crate::bursts::CaptureRecord,
) -> Result<Classification, String> {
    let path = rec.meta_path.clone();
    let (meta, samples) = tokio::task::spawn_blocking(move || crate::capture::read_cf32(&path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("capture {}: {e}", rec.id))?;
    let allocations: Vec<Allocation> =
        serde_json::from_value(rec.allocations.clone()).unwrap_or_default();
    Ok(set
        .run_all(
            &samples,
            meta.sample_rate_hz,
            meta.center_hz,
            Band {
                offset_hz: rec.center_hz.0 as i64 - meta.center_hz.0 as i64,
                bandwidth_hz: rec.bandwidth_hz,
            },
            None,
            allocations,
        )
        .await)
}

/// Run the classifier set over the selection's band and span of the source ring.
async fn classify(node: &Node, sel: Selection) -> Result<OpResult, OpError> {
    let (rate, center, samples) = ring_samples(node, &sel)?;
    // `check_band` has proved the band lies inside the ring's.
    let band = Band {
        offset_hz: sel.center_hz.0 as i64 - center.0 as i64,
        bandwidth_hz: sel.bandwidth_hz,
    };
    // Asked once per run, before the classifiers: a dead register costs its deadline once.
    let region = node
        .station()
        .map_or_else(|| DEFAULT_REGION.to_string(), |s| s.region);
    let allocations =
        nearest_allocations(&node.knowledge(), &region, sel.center_hz, sel.bandwidth_hz).await;
    let Classification {
        labels,
        tried,
        allocations,
    } = node
        .classifiers()
        .run_all(&samples, rate, center, band, None, allocations)
        .await;
    Ok(OpResult::Classification {
        labels,
        tried,
        allocations,
    })
}

/// Most frames and longest wait one `stream_tail` takes.
const TAIL_MAX_FRAMES: u64 = 1000;
const TAIL_MAX_TIMEOUT_MS: u64 = 10_000;

/// `field` = `value` must be in `1..=max`; no suggestion is offered.
fn limit(field: &str, value: u32, max: u64) -> Result<(), OpError> {
    if (1..=max).contains(&(value as u64)) {
        return Ok(());
    }
    Err(OpError::OutOfRange {
        field: field.into(),
        value: value as u64,
        min: 1,
        max,
        suggested: None,
    })
}

/// Subscribe to `stream` and collect up to `max` frames within `timeout_ms`.
/// A lagged receiver keeps going and counts the skipped frames in `missed`;
/// a closed stream returns what was collected.
async fn stream_tail(
    node: &Node,
    stream: StreamId,
    max: u32,
    timeout_ms: u32,
) -> Result<OpResult, OpError> {
    limit("max_frames", max, TAIL_MAX_FRAMES)?;
    limit("timeout_ms", timeout_ms, TAIL_MAX_TIMEOUT_MS)?;
    let bus = node.bus();
    let Some(mut rx) = bus.subscribe(&stream) else {
        let mut known: Vec<String> = bus.list().into_iter().map(|s| s.id.0).collect();
        known.sort();
        return Err(OpError::Unknown {
            field: "stream".into(),
            value: stream.0,
            known,
        });
    };
    let mut out = Vec::new();
    let mut missed = 0u64;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    while out.len() < max as usize {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => break,
            Ok(Ok(f)) => out.push(FrameSummary::from_frame(&f)),
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => missed += n,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
        }
    }
    Ok(OpResult::Frames {
        frames: out,
        missed,
    })
}

/// Write the selection's span from the source ring as a cf32_le SigMF pair.
fn iq_read(node: &Node, sel: Selection) -> Result<OpResult, OpError> {
    let (rate, center, samples) = ring_samples(node, &sel)?;
    let dir = node.state_dir().join("captures");
    std::fs::create_dir_all(&dir).map_err(other)?;
    let name = ulid::Ulid::generate().to_string();
    let half = sel.bandwidth_hz.0 / 2;
    let meta = CaptureMeta {
        sample_rate_hz: rate,
        center_hz: center,
        start: sel.span.start_ns,
        annotations: vec![serde_json::json!({
            "core:sample_start": 0,
            "core:sample_count": samples.len(),
            "core:freq_lower_edge": sel.center_hz.0.saturating_sub(half),
            "core:freq_upper_edge": sel.center_hz.0.saturating_add(half),
            "core:label": "selection",
        })],
    };
    let path = write_cf32(&dir, &name, &meta, &samples).map_err(other)?;
    // Complex baseband: the written band is as wide as the sample rate.
    Ok(OpResult::Iq {
        sample_rate_hz: rate,
        center_hz: center,
        bandwidth_hz: rate,
        samples: samples.len(),
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggest_scales_by_powers_of_ten() {
        assert_eq!(
            suggest(101_500, 1_000_000, 6_000_000_000),
            Some(101_500_000)
        );
        assert_eq!(
            suggest(100_000, 100_000_000, 100_000_000),
            Some(100_000_000)
        );
        assert_eq!(
            suggest(2_400_000, 24_000_000, 1_766_000_000),
            Some(24_000_000)
        );
        assert_eq!(
            suggest(101_500_000_000, 1_000_000, 6_000_000_000),
            Some(101_500_000)
        );
        assert_eq!(
            suggest(101_500_001_000, 1_000_000, 6_000_000_000),
            Some(101_500_001)
        );
        assert_eq!(suggest(7, 1_000_000_000, 6_000_000_000), None);
        assert_eq!(suggest(7_000_000_001, 1_000_000, 6_000_000_000), None);
    }

    fn caps_16m() -> Capabilities {
        use crate::types::Claim;
        Capabilities {
            tune_min_hz: Claim::Asserted(Hz(1_000_000)),
            tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
            max_bandwidth_hz: Claim::Asserted(Hz(16_000_000)),
            formats: vec![],
            channels: 1,
            rx: true,
            tx: false,
            remote: false,
            push: false,
        }
    }

    fn t(center_hz: u64, bandwidth_hz: u64) -> Tuning {
        Tuning {
            center_hz: Hz(center_hz),
            bandwidth_hz: Hz(bandwidth_hz),
        }
    }

    #[test]
    fn a_sub_band_scan_is_one_step_as_wide_as_the_span() {
        assert_eq!(
            scan_steps(t(100_000_000, 1_000_000), &caps_16m()),
            [t(100_000_000, 1_000_000)]
        );
    }

    #[test]
    fn scan_steps_stay_inside_the_tune_range() {
        // 0..40 MHz: steps of 16 MHz at 8, 24 and 40 MHz (the last overruns),
        // the first moved up to keep its capture above the 1 MHz floor.
        assert_eq!(
            scan_steps(t(20_000_000, 40_000_000), &caps_16m()),
            [
                t(9_000_000, 16_000_000),
                t(24_000_000, 16_000_000),
                t(40_000_000, 16_000_000)
            ]
        );
        // A span reaching past the tune ceiling: the last capture ends at it.
        let steps = scan_steps(t(5_990_000_000, 30_000_000), &caps_16m());
        assert_eq!(steps.last(), Some(&t(5_992_000_000, 16_000_000)));
        // A span at the floor: the first capture starts at it.
        assert_eq!(
            scan_steps(t(2_000_000, 4_000_000), &caps_16m()),
            [t(3_000_000, 4_000_000)]
        );
    }

    fn caps_min(tune_min_hz: u64) -> Capabilities {
        let mut c = caps_16m();
        c.tune_min_hz = crate::types::Claim::Asserted(Hz(tune_min_hz));
        c
    }

    fn caps_flags(remote: bool, push: bool) -> Capabilities {
        let mut c = caps_16m();
        c.remote = remote;
        c.push = push;
        c
    }

    #[test]
    fn scan_grid_odd_sweep_shifts_by_a_quarter_step() {
        let g = scan_grid(t(20_000_000, 40_000_000), &caps_16m());
        assert_eq!(g.even, scan_steps(t(20_000_000, 40_000_000), &caps_16m()));
        assert_eq!(
            g.even,
            [
                t(9_000_000, 16_000_000),
                t(24_000_000, 16_000_000),
                t(40_000_000, 16_000_000)
            ]
        );
        // 4, 20, 36, 52 MHz: the first clamped up to the 1 MHz floor + w/2, the
        // last dropped (its band, 44-60 MHz, misses the 0-40 MHz span).
        assert_eq!(
            g.odd,
            [
                t(9_000_000, 16_000_000),
                t(20_000_000, 16_000_000),
                t(36_000_000, 16_000_000)
            ]
        );
        assert_eq!(g.shift_hz(), 4_000_000);
        assert!(g.odd.len() <= g.even.len() + 1);
        assert!(g.alternates());
        assert_eq!(g.steps(0), &g.even[..]);
        assert_eq!(g.steps(1), &g.odd[..]);
        assert_eq!(g.steps(2), &g.even[..]);
    }

    #[test]
    fn scan_grid_drops_a_clamped_duplicate_at_the_tune_floor() {
        // Tune floor 20 MHz: lo = 28 MHz. Odd 4, 20, 36 -> 28, 28, 36 (52 misses
        // the span); even 8, 24, 40 -> 28, 28, 40.
        let g = scan_grid(t(20_000_000, 40_000_000), &caps_min(20_000_000));
        assert_eq!(
            g.odd,
            [t(28_000_000, 16_000_000), t(36_000_000, 16_000_000)]
        );
        assert_eq!(
            g.even,
            [t(28_000_000, 16_000_000), t(40_000_000, 16_000_000)]
        );
    }

    /// Whether `f` falls on an unmasked bin of one of `steps` (rows as wide as
    /// the step): the notch is `notch_hz` wide about each centre, the edge mask
    /// the outer `edge_mask` of each row.
    fn seen_clean(f: f64, steps: &[Tuning], notch_hz: u64, edge_mask: f64) -> bool {
        steps.iter().any(|s| {
            let c = s.center_hz.0 as f64;
            let w = s.bandwidth_hz.0 as f64;
            let (lo, hi) = (c - w / 2.0, c + w / 2.0);
            let notch = scan_notch_in_force(s.bandwidth_hz.0, notch_hz) as f64;
            f >= lo + edge_mask * w && f < hi - edge_mask * w && (f - c).abs() >= notch / 2.0
        })
    }

    #[test]
    fn scan_grid_sees_every_frequency_clean_in_one_of_two_sweeps() {
        let bin_hz = 16_000_000.0 / 4096.0;
        for (span, caps) in [
            // The 440-488 MHz survey on the HackRF at 16 Msps.
            (t(464_000_000, 48_000_000), caps_16m()),
            // Spans that are not a whole number of steps.
            (t(464_000_000, 40_000_000), caps_16m()),
            (t(464_000_000, 37_000_000), caps_16m()),
            (t(464_000_000, 45_000_000), caps_16m()),
            (t(464_000_000, 20_000_000), caps_16m()),
            (t(1_000_000_000, 100_000_000), caps_16m()),
        ] {
            let g = scan_grid(span, &caps);
            assert!(g.odd.len() <= g.even.len() + 1, "{span:?}");
            let start = (span.center_hz.0 - span.bandwidth_hz.0 / 2) as f64;
            let end = start + span.bandwidth_hz.0 as f64;
            let mut f = start + bin_hz / 2.0;
            while f < end {
                assert!(
                    seen_clean(f, &g.even, SCAN_NOTCH_HZ, SCAN_EDGE_MASK)
                        || seen_clean(f, &g.odd, SCAN_NOTCH_HZ, SCAN_EDGE_MASK),
                    "{span:?}: {f} Hz is masked in both grids"
                );
                f += bin_hz;
            }
        }
        // A one-step span does not alternate: with no mask in force (see
        // `scan_edge_mask_in_force`) it sees everything; the property is moot.
        let g = scan_grid(t(100_000_000, 1_000_000), &caps_16m());
        assert!(!g.alternates());
    }

    #[test]
    fn scan_grid_of_one_step_does_not_alternate() {
        let g = scan_grid(t(100_000_000, 1_000_000), &caps_16m());
        assert_eq!(g.even, [t(100_000_000, 1_000_000)]);
        assert_eq!(g.odd, g.even);
        assert!(!g.alternates());
    }

    #[test]
    fn scan_mask_default_follows_the_capabilities() {
        assert!(local_hardware(&caps_flags(false, false)));
        assert!(!local_hardware(&caps_flags(true, false)));
        assert!(!local_hardware(&caps_flags(false, true)));
        assert_eq!(scan_mask_default(&caps_flags(false, false)), (50_000, 0.02));
        assert_eq!(scan_mask_default(&caps_flags(true, false)), (0, 0.0));
        assert_eq!(scan_mask_default(&caps_flags(false, true)), (0, 0.0));
        assert_eq!(scan_notch_in_force(16_000_000, 300_000), 300_000);
        assert_eq!(scan_notch_in_force(1_200_000, 300_000), 300_000);
        assert_eq!(scan_notch_in_force(1_000_000, 300_000), 0);
        assert_eq!(scan_notch_in_force(48_000, 12_000), 12_000);
        assert_eq!(scan_notch_in_force(16_000_000, u64::MAX), 0);
        assert_eq!(scan_edge_mask_in_force(true, 0.10), 0.10);
        assert_eq!(scan_edge_mask_in_force(false, 0.10), 0.0);
        assert_eq!(scan_edge_mask_in_force(false, 0.0), 0.0);
    }

    #[test]
    fn scan_expands_to_one_fft_node_without_mask_params() {
        // The embedded rows with no stages: Scan and Identify need no row.
        let modes = ModeRegistry::load(&crate::stage::StageRegistry::new(), None);
        let g = JobKind::Scan
            .expand(
                &SourceId("s".into()),
                t(20_000_000, 40_000_000),
                Hz(0),
                None,
                None,
                None,
                &modes,
            )
            .unwrap();
        assert_eq!(g.nodes.len(), 1);
        assert_eq!(g.nodes[0].kind, "fft");
        assert_eq!(
            g.nodes[0].params,
            serde_json::json!({"size": 4096, "window": "hann", "rows_per_s": 20})
        );
        assert!(g.edges.is_empty());
        let id = JobKind::Identify
            .expand(
                &SourceId("s".into()),
                t(100_000_000, 12_500),
                Hz(0),
                None,
                None,
                None,
                &modes,
            )
            .unwrap();
        assert!(id.nodes.is_empty());
    }

    #[test]
    fn wire_shapes_are_tagged() {
        let op = Op::JobStop {
            job: JobId("j".into()),
        };
        assert_eq!(
            serde_json::to_value(&op).unwrap(),
            serde_json::json!({"op": "job_stop", "job": "j"})
        );
        assert_eq!(
            serde_json::to_value(Op::JobList {}).unwrap(),
            serde_json::json!({"op": "job_list"})
        );
        assert_eq!(
            serde_json::to_value(OpResult::Ok).unwrap(),
            serde_json::json!({"result": "ok"})
        );
        let e = OpError::Ring {
            detail: RingError::Empty,
        };
        let want = serde_json::json!({"error": "ring", "detail": {"cause": "empty"}});
        assert_eq!(serde_json::to_value(&e).unwrap(), want);
        let e = OpError::Other {
            message: "m".into(),
        };
        assert_eq!(
            serde_json::from_value::<OpError>(serde_json::to_value(&e).unwrap()).unwrap(),
            e
        );
    }
}
