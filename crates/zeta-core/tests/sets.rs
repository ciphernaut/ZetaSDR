//! Named sets: the set files (built in, then the operator's `--sets-dir`), loaded with
//! the mode registry's rules and listed by `set_list`; `set_start` starts a set's
//! members across radios under one lease decision per source, and `set_stop` stops
//! that instance's jobs.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use zeta_core::job::JobState;
use zeta_core::modes::ModeRegistry;
use zeta_core::modes::handlers::find_handler;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{JobKind, Op, OpError, OpResult, SetInfo};
use zeta_core::ops::{SetExisting, SetRadio, SetRefused, SetStarted};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::sets::{SetRegistry, SetState};
use zeta_core::stage::{
    Backend, PassthroughStage, Readiness, Stage, StageContext, StageError, StageRegistry, StageSpec,
};
use zeta_core::stream::{Frame, Payload, StreamBus};
use zeta_core::stream_in::StreamInStage;
use zeta_core::types::{Capabilities, Claim, Hz, JobId, SampleFormat, SourceId};
use zeta_core::types::{StreamId, StreamKind};

/// A radio that tunes to exactly the band asked for, counting its tunes; with `slow`
/// set a tune takes a second, so a caller's future can be dropped inside it.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
    tunes: AtomicUsize,
    slow: AtomicBool,
}

impl Radio {
    fn new(id: &str) -> Arc<Self> {
        Self::with_max(id, 20_000_000)
    }

    fn tunes(&self) -> usize {
        self.tunes.load(Ordering::SeqCst)
    }

    fn with_max(id: &str, max_bandwidth_hz: u64) -> Arc<Self> {
        Arc::new(Self {
            id: SourceId(id.into()),
            caps: Capabilities {
                tune_min_hz: Claim::Asserted(Hz(1_000_000)),
                tune_max_hz: Claim::Asserted(Hz(6_000_000_000)),
                max_bandwidth_hz: Claim::Asserted(Hz(max_bandwidth_hz)),
                formats: vec![SampleFormat::Cf32],
                channels: 1,
                rx: true,
                tx: false,
                remote: false,
                push: false,
                usable_fraction: Claim::Asserted(1.0),
                depths: vec![],
                sweep: None,
            },
            cur: Mutex::new(None),
            tx: broadcast::channel(1).0,
            tunes: AtomicUsize::new(0),
            slow: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl Port for Radio {
    fn id(&self) -> &SourceId {
        &self.id
    }
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    async fn tune(&self, t: Tuning) -> Result<Tuning, PortError> {
        self.tunes.fetch_add(1, Ordering::SeqCst);
        if self.slow.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        *self.cur.lock().unwrap() = Some(t);
        Ok(t)
    }
    fn current(&self) -> Option<Tuning> {
        *self.cur.lock().unwrap()
    }
    fn subscribe(&self) -> IqReceiver {
        self.tx.subscribe()
    }
}

fn registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Arc::new(PassthroughStage::new()));
    zeta_dsp::register_all(&mut r);
    r
}

fn cfg(dir: &Path, sets_dir: Option<&Path>) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: None,
        sets_dir: sets_dir.map(Path::to_path_buf),
    }
}

async fn open(dir: &Path, sets_dir: Option<&Path>) -> Node {
    let ports: Vec<Arc<dyn Port>> = vec![Radio::with_max("radio0", 20_000_000)];
    Node::open(cfg(dir, sets_dir), registry(), ports)
        .await
        .unwrap()
}

/// Write `json` to `<dir>/<name>`.
fn write_set(dir: &Path, name: &str, json: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), json).unwrap();
}

/// The sets loaded over the embedded ones from `dir`.
fn load(dir: Option<&Path>) -> SetRegistry {
    let stages = registry();
    let modes = ModeRegistry::load(&stages, None);
    SetRegistry::load(&modes, &stages, dir)
}

/// The one-member set `id` whose member is `member` (a JSON object), on role `radio`.
fn one_member(id: &str, member: &str) -> String {
    format!(
        r#"{{"id": "{id}", "name": "{id}", "notes": "", "radios": {{"radio": {{"note": "any"}}}},
            "members": [{member}]}}"#
    )
}

/// A `watch` member over `bandwidth_hz` at 148.5 MHz with `params`.
fn watch_member(bandwidth_hz: u64, params: &str) -> String {
    format!(
        r#"{{"id": "watch", "radio": "radio", "op": "job_start", "kind": "watch",
            "center_hz": 148500000, "bandwidth_hz": {bandwidth_hz}, "deviation_hz": null,
            "sideband": null, "params": {params}}}"#
    )
}

/// The reason the set `id` is broken.
fn broken(sets: &SetRegistry, id: &str) -> String {
    let s = sets.get(id).unwrap_or_else(|| panic!("{id} listed"));
    assert_eq!(s.state, SetState::Broken, "{id}: {s:?}");
    s.reason.clone().expect("a broken set has a reason")
}

#[test]
fn the_builtin_sets_parse_validate_and_are_ready() {
    let sets = load(None);
    assert_eq!(
        sets.ids(),
        ["airband", "airband_vhf", "airband_vhf_and_sos", "pagers_au"]
    );
    for s in sets.all() {
        assert_eq!(s.state, SetState::Ready, "{s:?}");
        assert_eq!(s.reason, None, "{s:?}");
    }
    let airband = sets.get("airband").unwrap();
    assert_eq!(airband.file, "sets/airband.json");
    let roles: Vec<&str> = airband.set.radios.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(roles, ["wide", "second"], "the file's order");
}

/// pagers_au stays centred on 148.5 MHz, and its notes name the steady spur there (the
/// capture centre's DC mask hides it; an offset centre would expose it to the watch).
#[test]
fn pagers_au_stays_on_148_5_mhz_and_names_the_spur() {
    let sets = load(None);
    let pagers = sets.get("pagers_au").unwrap();
    let watch = serde_json::to_value(&pagers.set.members[0]).unwrap();
    assert_eq!(watch["center_hz"], 148_500_000, "{watch}");
    let notes = &pagers.set.notes;
    assert!(
        notes.contains("148.500 MHz") && notes.contains("spur") && notes.contains("b210"),
        "{notes}"
    );
}

/// airband_vhf is airband without its ADS-B member: one role, the same ACARS plan and the
/// same VDL2 job, so a node with one radio starts it with one `set_start` and no `members`.
/// airband_vhf_and_sos is airband_vhf plus the 121.5 MHz guard channel in AM, to listen to.
#[test]
fn airband_vhf_is_airband_without_adsb_and_and_sos_adds_the_guard_channel() {
    let sets = load(None);
    let airband = sets.get("airband").unwrap();
    let vhf = sets.get("airband_vhf").unwrap();
    assert_eq!(vhf.file, "sets/airband_vhf.json");
    let roles: Vec<&str> = vhf.set.radios.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(roles, ["wide"]);
    let ids: Vec<&str> = vhf.set.members.iter().map(|m| m.id()).collect();
    assert_eq!(ids, ["acars", "vdl2"]);
    for m in &vhf.set.members {
        let twin = airband
            .set
            .members
            .iter()
            .find(|a| a.id() == m.id())
            .unwrap();
        assert_eq!(
            serde_json::to_value(m).unwrap(),
            serde_json::to_value(twin).unwrap(),
            "{}",
            m.id()
        );
    }
    assert!(vhf.set.notes.contains("second radio"), "{}", vhf.set.notes);

    let sos = sets.get("airband_vhf_and_sos").unwrap();
    assert_eq!(sos.file, "sets/airband_vhf_and_sos.json");
    let roles: Vec<&str> = sos.set.radios.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(roles, ["wide"]);
    assert_eq!(
        serde_json::to_value(&sos.set.members[..2]).unwrap(),
        serde_json::to_value(&vhf.set.members).unwrap()
    );
    let guard = serde_json::to_value(&sos.set.members[2]).unwrap();
    assert_eq!(guard["id"], "guard", "{guard}");
    assert_eq!(guard["kind"], "am_demod", "{guard}");
    assert_eq!(guard["center_hz"], 121_500_000, "{guard}");
    assert_eq!(guard["bandwidth_hz"], 10_000, "{guard}");
    assert!(sos.set.notes.contains("listen"), "{}", sos.set.notes);
}

#[test]
fn an_operator_set_replaces_a_builtin_and_a_broken_one_is_listed_broken() {
    let dir = tempfile::tempdir().unwrap();
    let sets_dir = dir.path().join("sets");
    let mine = include_str!("../../../sets/airband.json")
        .replace(r#""name": "Airband data links""#, r#""name": "Mine""#);
    write_set(&sets_dir, "airband.json", &mine);
    let bad = r#"{"id": "bad", "name": "Bad", "notes": "", "radios": {"radio": {"note": ""}},
        "members": [{"id": "m", "radio": "nowhere", "op": "job_start", "kind": "measure",
          "center_hz": 100000000, "bandwidth_hz": 10000, "deviation_hz": null, "sideband": null}]}"#;
    write_set(&sets_dir, "bad.json", bad);
    write_set(
        &sets_dir,
        "bad2.json",
        &bad.replace(r#""id": "bad""#, r#""id": "airband""#),
    );

    let sets = load(Some(&sets_dir));
    let airband = sets.get("airband").unwrap();
    assert_eq!(airband.set.name, "Mine");
    assert_eq!(airband.state, SetState::Ready);
    assert_eq!(
        airband.file,
        sets_dir.join("airband.json").display().to_string()
    );
    assert_eq!(
        broken(&sets, "bad"),
        "members[0].radio: unknown role nowhere"
    );
    assert_eq!(
        broken(&sets, "airband@bad2.json"),
        "members[0].radio: unknown role nowhere"
    );
    assert_eq!(
        sets.get("airband").unwrap().set.name,
        "Mine",
        "still in force"
    );
}

#[test]
fn a_set_file_never_carries_replace_or_source() {
    let dir = tempfile::tempdir().unwrap();
    let sets_dir = dir.path().join("sets");
    let member = |extra: &str| {
        format!(
            r#"{{"id": "m", "radio": "radio", "op": "job_start", "kind": "measure",
                "center_hz": 100000000, "bandwidth_hz": 10000, "deviation_hz": null,
                "sideband": null, {extra}}}"#
        )
    };
    write_set(
        &sets_dir,
        "sourced.json",
        &one_member("sourced", &member(r#""source": "b210""#)),
    );
    write_set(
        &sets_dir,
        "replacing.json",
        &one_member("replacing", &member(r#""replace": []"#)),
    );
    let sets = load(Some(&sets_dir));
    assert_eq!(
        broken(&sets, "sourced"),
        "members[0].source: a set file names no source; the request does"
    );
    assert_eq!(
        broken(&sets, "replacing"),
        "members[0].replace: never in a set file"
    );
}

#[test]
fn a_watch_members_params_are_checked_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let sets_dir = dir.path().join("sets");
    write_set(
        &sets_dir,
        "ten.json",
        &one_member(
            "ten",
            &watch_member(
                1_000_000,
                r#"{"modes": ["pocsag_decode"], "hold_ms": "ten"}"#,
            ),
        ),
    );
    write_set(
        &sets_dir,
        "measured.json",
        &one_member(
            "measured",
            r#"{"id": "m", "radio": "radio", "op": "job_start", "kind": "measure",
                "center_hz": 100000000, "bandwidth_hz": 10000, "deviation_hz": null,
                "sideband": null, "params": {"hold_ms": 1}}"#,
        ),
    );
    let sets = load(Some(&sets_dir));
    let ten = broken(&sets, "ten");
    assert!(ten.starts_with("members[0].params.hold_ms: "), "{ten}");
    assert_eq!(
        broken(&sets, "measured"),
        "members[0].params: only a watch member takes params"
    );
}

/// A watch member may name a row this node lacks or cannot run: refused when the set
/// starts, never at load.
#[test]
fn a_watch_member_naming_an_unknown_or_unavailable_row_is_ready_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let sets_dir = dir.path().join("sets");
    write_set(
        &sets_dir,
        "unknown.json",
        &one_member(
            "unknown",
            &watch_member(1_000_000, r#"{"modes": ["no_such_row"]}"#),
        ),
    );
    write_set(
        &sets_dir,
        "lacking.json",
        &one_member(
            "lacking",
            &watch_member(1_000_000, r#"{"modes": ["vdl2_decode"]}"#),
        ),
    );
    let stages = registry();
    let mut modes = ModeRegistry::load(&stages, None);
    modes.set_program("vdl2_decode", None, Some("dumpvdl2 not found".into()));
    assert_eq!(
        modes.get("vdl2_decode").unwrap().state,
        zeta_core::modes::ModeState::Unavailable
    );
    let sets = SetRegistry::load(&modes, &stages, Some(&sets_dir));
    for id in ["unknown", "lacking"] {
        let s = sets.get(id).unwrap();
        assert_eq!(s.state, SetState::Ready, "{id}: {s:?}");
    }
}

/// A watch member's `prefer` would name sources: a set file never does.
#[test]
fn a_watch_member_with_prefer_is_broken_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let sets_dir = dir.path().join("sets");
    write_set(
        &sets_dir,
        "preferring.json",
        &one_member(
            "preferring",
            &watch_member(
                1_000_000,
                r#"{"modes": ["pocsag_decode"], "prefer": ["b210"]}"#,
            ),
        ),
    );
    write_set(
        &sets_dir,
        "empty_prefer.json",
        &one_member(
            "empty_prefer",
            &watch_member(1_000_000, r#"{"modes": ["pocsag_decode"], "prefer": []}"#),
        ),
    );
    let sets = load(Some(&sets_dir));
    assert_eq!(
        broken(&sets, "preferring"),
        "members[0].params.prefer: a set file names no source; the request does"
    );
    assert_eq!(sets.get("empty_prefer").unwrap().state, SetState::Ready);
}

#[test]
fn a_watch_member_narrower_than_its_row_is_broken_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let sets_dir = dir.path().join("sets");
    write_set(
        &sets_dir,
        "narrow.json",
        &one_member(
            "narrow",
            &watch_member(10_000, r#"{"modes": ["pocsag_decode"]}"#),
        ),
    );
    let sets = load(Some(&sets_dir));
    assert_eq!(
        broken(&sets, "narrow"),
        "members[0].params.modes[0]: pocsag_decode is 12500 Hz wide; the watch's band is 10000 Hz"
    );
}

/// `set_list`'s answer.
async fn set_list(node: &Node) -> Vec<SetInfo> {
    match node.apply(Op::SetList {}).await {
        Ok(OpResult::Sets(sets)) => sets,
        other => panic!("expected sets, got {other:?}"),
    }
}

#[tokio::test]
async fn set_list_lists_the_sets_with_their_members_and_no_instance() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), None).await;
    let sets = set_list(&node).await;
    let ids: Vec<&str> = sets.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        ["airband", "airband_vhf", "airband_vhf_and_sos", "pagers_au"]
    );
    let airband = &sets[0];
    assert_eq!(airband.state, SetState::Ready);
    assert_eq!(airband.reason, None);
    assert_eq!(airband.file, "sets/airband.json");
    assert!(airband.radios.contains_key("wide") && airband.radios.contains_key("second"));
    let members: Vec<(&str, &str, &str, &str)> = airband
        .members
        .iter()
        .map(|m| {
            (
                m.id.as_str(),
                m.radio.as_str(),
                m.op.as_str(),
                m.kind.as_str(),
            )
        })
        .collect();
    assert_eq!(
        members,
        [
            ("acars", "wide", "plan_start", "acars_decode"),
            ("vdl2", "wide", "job_start", "vdl2_decode"),
            ("adsb", "second", "job_start", "adsb_decode"),
        ]
    );
    assert!(airband.members.iter().all(|m| !m.summary.is_empty()));
    assert!(sets.iter().all(|s| s.running.is_none()));
    // A read: the JSON names every field.
    let v = serde_json::to_value(&sets[3]).unwrap();
    assert_eq!(v["running"], serde_json::Value::Null, "{v}");
    assert_eq!(v["members"][0]["kind"], "watch", "{v}");
}

#[tokio::test]
async fn a_node_without_a_sets_dir_lists_the_builtins() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), None).await;
    assert_eq!(
        node.sets().ids(),
        ["airband", "airband_vhf", "airband_vhf_and_sos", "pagers_au"]
    );

    // With an operator directory, its sets are listed beside the built-ins.
    let dir2 = tempfile::tempdir().unwrap();
    let sets_dir = dir2.path().join("sets");
    write_set(
        &sets_dir,
        "mine.json",
        &one_member(
            "mine",
            &watch_member(1_000_000, r#"{"modes": ["pocsag_decode"]}"#),
        ),
    );
    let node2 = open(dir2.path(), Some(&sets_dir)).await;
    let ids: Vec<String> = set_list(&node2).await.into_iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        [
            "airband",
            "airband_vhf",
            "airband_vhf_and_sos",
            "mine",
            "pagers_au"
        ]
    );
}

// ---- set_start and set_stop ----

const TWO: &str = "two_radio";

/// A plan member `cb` (`fm_demod` on UHF CB channels 1-3) on role `a`, a `measure`
/// member `m` at `m_hz` and a `watch` member `w` over 300 MHz ± 0.5 MHz on role `b`.
fn two_radio_set(id: &str, m_hz: u64) -> String {
    format!(
        r#"{{"id": "{id}", "name": "{id}", "notes": "",
            "radios": {{"a": {{"note": "UHF CB"}}, "b": {{"note": "300 MHz"}}}},
            "members": [
              {{"id": "cb", "radio": "a", "op": "plan_start", "plan": "uhf_cb_au",
                "channels": [1, 2, 3], "kind": "fm_demod", "deviation_hz": 2500,
                "sideband": null, "attach": null, "language": null}},
              {{"id": "m", "radio": "b", "op": "job_start", "kind": "measure",
                "center_hz": {m_hz}, "bandwidth_hz": 20000, "deviation_hz": null,
                "sideband": null}},
              {{"id": "w", "radio": "b", "op": "job_start", "kind": "watch",
                "center_hz": 300000000, "bandwidth_hz": 1000000, "deviation_hz": null,
                "sideband": null, "params": {{"modes": ["pocsag_decode"]}}}}
            ]}}"#
    )
}

/// The sets directory the set tests use: `two_radio` (`m` at 300.2 MHz, inside the
/// watch's band), `shared` (`m` at 476.5 MHz, beside the CB channels) and `talk` (a
/// one-member set: `cb` with `transcribe` attached).
fn set_files(dir: &Path) -> std::path::PathBuf {
    let sets = dir.join("sets");
    write_set(&sets, "two_radio.json", &two_radio_set(TWO, 300_200_000));
    write_set(&sets, "shared.json", &two_radio_set("shared", 476_500_000));
    write_set(
        &sets,
        "talk.json",
        r#"{"id": "talk", "name": "talk", "notes": "", "radios": {"a": {"note": "UHF CB"}},
            "members": [
              {"id": "cb", "radio": "a", "op": "plan_start", "plan": "uhf_cb_au",
               "channels": [1, 2, 3], "kind": "fm_demod", "deviation_hz": null,
               "sideband": null, "attach": "transcribe", "language": null}]}"#,
    );
    sets
}

async fn open_radios(dir: &Path, radios: &[&Arc<Radio>], r: StageRegistry) -> Node {
    let ports: Vec<Arc<dyn Port>> = radios.iter().map(|r| (*r).clone() as _).collect();
    let sets = set_files(dir);
    let node = Node::open(cfg(dir, Some(&sets)), r, ports).await.unwrap();
    for s in node.sets().all() {
        assert_eq!(s.state, SetState::Ready, "{s:?}");
    }
    node
}

async fn open_two(dir: &Path, a: &Arc<Radio>, b: &Arc<Radio>) -> Node {
    open_radios(dir, &[a, b], registry()).await
}

fn set_op(
    set: &str,
    radios: &[(&str, Option<&str>)],
    members: Option<&[&str]>,
    replace: &[&JobId],
) -> Op {
    Op::SetStart {
        set: set.into(),
        radios: radios
            .iter()
            .map(|(role, s)| (role.to_string(), s.map(|s| SourceId(s.into()))))
            .collect(),
        members: members.map(|m| m.iter().map(|s| s.to_string()).collect()),
        replace: replace.iter().map(|j| (*j).clone()).collect(),
    }
}

/// Role `a` on source `a`, role `b` on source `b`.
const AB: &[(&str, Option<&str>)] = &[("a", Some("a")), ("b", Some("b"))];

/// A `set_start` answer.
struct SetStartParts {
    radios: std::collections::BTreeMap<String, SetRadio>,
    started: Vec<SetStarted>,
    existing: Vec<SetExisting>,
    refused: Vec<SetRefused>,
    warnings: Vec<String>,
}

impl SetStartParts {
    /// `(member, channel)` per started entry.
    fn started_members(&self) -> Vec<(&str, Option<u32>)> {
        self.started
            .iter()
            .map(|s| (s.member.as_str(), s.channel))
            .collect()
    }

    /// The job started for `member` (its first, for a plan member).
    fn job(&self, member: &str) -> JobId {
        self.started
            .iter()
            .find(|s| s.member == member)
            .unwrap_or_else(|| panic!("{member} started"))
            .job
            .clone()
    }
}

async fn started_set(node: &Node, op: Op) -> SetStartParts {
    let want = match &op {
        Op::SetStart { set, .. } => set.clone(),
        op => panic!("not a set_start: {op:?}"),
    };
    match node.apply(op).await {
        Ok(OpResult::SetStart {
            set,
            radios,
            started,
            existing,
            refused,
            warnings,
        }) => {
            assert_eq!(set, want);
            SetStartParts {
                radios,
                started,
                existing,
                refused,
                warnings,
            }
        }
        other => panic!("expected set_start, got {other:?}"),
    }
}

/// `set_stop`'s `(stopped, ended, failed count)`.
async fn stopped_set(node: &Node, set: &str) -> (Vec<JobId>, Vec<JobId>, usize) {
    match node.apply(Op::SetStop { set: set.into() }).await {
        Ok(OpResult::SetStopped {
            set: s,
            stopped,
            ended,
            failed,
        }) => {
            assert_eq!(s, set);
            assert!(failed.is_empty(), "{failed:?}");
            (stopped, ended, failed.len())
        }
        other => panic!("expected set_stopped, got {other:?}"),
    }
}

async fn running_of(node: &Node, set: &str) -> Option<zeta_core::ops::SetRunning> {
    set_list(node)
        .await
        .into_iter()
        .find(|s| s.id == set)
        .unwrap_or_else(|| panic!("{set} listed"))
        .running
}

async fn holders(node: &Node, source: &str) -> Vec<JobId> {
    match node.apply(Op::SourceList {}).await.unwrap() {
        OpResult::Sources(v) => {
            v.into_iter()
                .find(|s| s.id.0 == source)
                .expect("listed")
                .holders
        }
        r => panic!("source_list: {r:?}"),
    }
}

fn running_jobs(node: &Node) -> Vec<JobId> {
    node.job_list()
        .into_iter()
        .filter(|r| r.state == JobState::Running)
        .map(|r| r.id)
        .collect()
}

fn instance_file(dir: &Path, set: &str) -> std::path::PathBuf {
    dir.join("state")
        .join("set-instances")
        .join(format!("{set}.json"))
}

fn band(center_hz: u64, bandwidth_hz: u64) -> Tuning {
    Tuning {
        center_hz: Hz(center_hz),
        bandwidth_hz: Hz(bandwidth_hz),
    }
}

fn job_start(source: &str, kind: JobKind, t: Tuning) -> Op {
    Op::JobStart {
        kind,
        source: SourceId(source.into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic: false,
        replace: vec![],
    }
}

async fn job(node: &Node, op: Op) -> JobId {
    match node.apply(op).await {
        Ok(OpResult::Job(id)) => id,
        other => panic!("expected a job, got {other:?}"),
    }
}

fn other_message(r: Result<OpResult, OpError>) -> String {
    match r {
        Err(OpError::Other { message }) => message,
        other => panic!("expected other, got {other:?}"),
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_set_starts_its_members_under_one_tune_per_radio() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let r = started_set(&node, set_op(TWO, AB, None, &[])).await;
    assert_eq!((a.tunes(), b.tunes()), (1, 1));
    assert_eq!(r.radios["a"].source.0, "a");
    assert_eq!(r.radios["b"].source.0, "b");
    // The CB channels 1-3 (476.425-476.475 MHz, 12.5 kHz each) and the watch's band.
    assert_eq!(r.radios["a"].capture, band(476_450_000, 62_500));
    assert_eq!(r.radios["b"].capture, band(300_000_000, 1_000_000));
    assert_eq!(
        r.started_members(),
        [
            ("cb", Some(1)),
            ("cb", Some(2)),
            ("cb", Some(3)),
            ("m", None),
            ("w", None)
        ]
    );
    assert!(
        r.existing.is_empty() && r.refused.is_empty(),
        "{:?}",
        r.refused
    );
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    // The watch runs with its params on its `detect` node.
    let w = node
        .job_list()
        .into_iter()
        .find(|j| j.id == r.job("w"))
        .unwrap();
    let detect = w.graph.nodes.iter().find(|n| n.kind == "detect").unwrap();
    assert_eq!(
        detect.params,
        serde_json::json!({"modes": ["pocsag_decode"]})
    );

    let running = running_of(&node, TWO).await.expect("an instance");
    let radios: Vec<(&str, &str)> = running
        .radios
        .iter()
        .map(|(r, s)| (r.as_str(), s.0.as_str()))
        .collect();
    assert_eq!(radios, [("a", "a"), ("b", "b")]);
    let jobs: Vec<&JobId> = running.members.iter().map(|m| &m.job).collect();
    let want: Vec<&JobId> = r.started.iter().map(|s| &s.job).collect();
    assert_eq!(jobs, want);
    assert!(
        running.members.iter().all(|m| m.state == "running"),
        "{running:?}"
    );
    assert!(instance_file(dir.path(), TWO).is_file());
}

/// A rigid holder on source `b` outside role `b`'s band: the whole request is the
/// conflict, naming the role, and the source of role `a` is neither held nor tuned.
async fn conflict_on_b(a_id: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new(a_id), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let holder = job(
        &node,
        job_start("b", JobKind::Measure, band(900_000_000, 20_000)),
    )
    .await;
    let r = node
        .apply(set_op(
            TWO,
            &[("a", Some(a_id)), ("b", Some("b"))],
            None,
            &[],
        ))
        .await;
    match r {
        Err(OpError::Conflict {
            source,
            holders: h,
            options,
            ..
        }) => {
            assert_eq!(source.0, "b");
            assert_eq!(h, vec![holder.clone()]);
            assert!(
                options[0].starts_with("radios.b (b) is held by"),
                "{options:?}"
            );
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(a.tunes(), 0, "{a_id} untouched");
    assert!(holders(&node, a_id).await.is_empty());
    assert_eq!(running_jobs(&node), vec![holder]);
    assert!(running_of(&node, TWO).await.is_none());
    assert!(!instance_file(dir.path(), TWO).exists());
}

#[tokio::test]
async fn a_conflict_on_either_radio_starts_nothing_and_names_the_role() {
    // `a` sorts before `b` and `z` after it: the gates are taken in both orders.
    conflict_on_b("a").await;
    conflict_on_b("z").await;
}

#[tokio::test]
async fn two_roles_on_one_source_share_one_capture_or_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let both_on_a: &[(&str, Option<&str>)] = &[("a", Some("a")), ("b", Some("a"))];
    // The CB channels and the measure at 476.5 MHz fit one capture of `a`.
    let r = started_set(&node, set_op("shared", both_on_a, Some(&["cb", "m"]), &[])).await;
    assert_eq!((a.tunes(), b.tunes()), (1, 0));
    assert_eq!(r.started.len(), 4, "{:?}", r.refused);
    assert_eq!(r.radios["a"].source.0, "a");
    assert_eq!(r.radios["b"].source.0, "a");
    assert_eq!(r.radios["a"].capture, r.radios["b"].capture);
    // 476.41875 MHz (ch1's low edge) to 476.51 MHz (the measure's high edge).
    assert_eq!(r.radios["a"].capture, band(476_464_375, 91_250));
    // With the watch at 300 MHz too, the two roles' bands cannot share one capture.
    let m = other_message(node.apply(set_op("shared", both_on_a, None, &[])).await);
    assert!(m.contains("radios.a") && m.contains("radios.b"), "{m}");
    assert_eq!(a.tunes(), 1, "nothing tuned");
    assert_eq!(running_jobs(&node).len(), 4);
}

#[tokio::test]
async fn a_second_start_adds_only_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let first = started_set(&node, set_op(TWO, AB, None, &[])).await;
    assert_eq!(first.started.len(), 5);
    let second = started_set(&node, set_op(TWO, AB, None, &[])).await;
    assert!(second.started.is_empty(), "{:?}", second.started_members());
    assert!(second.refused.is_empty(), "{:?}", second.refused);
    let existing: Vec<(&str, Option<u32>, &JobId)> = second
        .existing
        .iter()
        .map(|e| (e.member.as_str(), e.channel, &e.job))
        .collect();
    let want: Vec<(&str, Option<u32>, &JobId)> = first
        .started
        .iter()
        .map(|s| (s.member.as_str(), s.channel, &s.job))
        .collect();
    assert_eq!(existing, want);
    assert_eq!((a.tunes(), b.tunes()), (1, 1));
    assert_eq!(running_jobs(&node).len(), 5);
    assert_eq!(running_of(&node, TWO).await.unwrap().members.len(), 5);
}

#[tokio::test]
async fn replace_stops_a_named_holder_of_a_source_in_use_only() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let on_a = job(
        &node,
        job_start("a", JobKind::Measure, band(100_000_000, 20_000)),
    )
    .await;
    let on_b = job(
        &node,
        job_start("b", JobKind::Measure, band(900_000_000, 20_000)),
    )
    .await;
    // Only `b` is in use: a holder of `a` is no job of the request's sources.
    let only_b: &[(&str, Option<&str>)] = &[("b", Some("b"))];
    let m = other_message(
        node.apply(set_op(TWO, only_b, Some(&["m", "w"]), &[&on_a]))
            .await,
    );
    assert_eq!(
        m,
        format!("job {} does not hold any source of the request", on_a.0)
    );
    assert_eq!(running_jobs(&node).len(), 2, "nothing stopped");
    let r = started_set(&node, set_op(TWO, only_b, Some(&["m", "w"]), &[&on_b])).await;
    assert_eq!(r.started_members(), [("m", None), ("w", None)]);
    until("the holder ends", || !running_jobs(&node).contains(&on_b)).await;
    assert!(running_jobs(&node).contains(&on_a));
}

#[tokio::test]
async fn a_different_mapping_while_running_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    started_set(&node, set_op(TWO, AB, None, &[])).await;
    let swapped: &[(&str, Option<&str>)] = &[("a", Some("b")), ("b", Some("a"))];
    let m = other_message(node.apply(set_op(TWO, swapped, None, &[])).await);
    assert!(m.contains("set_stop it first"), "{m}");
    assert!(m.starts_with("set two_radio runs on "), "{m}");
    assert_eq!((a.tunes(), b.tunes()), (1, 1));
    assert_eq!(running_jobs(&node).len(), 5);
}

#[tokio::test]
async fn members_narrows_and_unmapped_roles_may_be_omitted() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let r = started_set(&node, set_op(TWO, &[("b", Some("b"))], Some(&["m"]), &[])).await;
    assert_eq!(r.started_members(), [("m", None)]);
    assert_eq!(r.radios.keys().collect::<Vec<_>>(), ["b"]);
    assert_eq!((a.tunes(), b.tunes()), (0, 1));
    assert!(holders(&node, "a").await.is_empty());
    // A role a chosen member uses must be mapped; an unknown member id is named.
    let m = other_message(
        node.apply(set_op(TWO, &[("b", Some("b"))], Some(&["cb"]), &[]))
            .await,
    );
    assert_eq!(m, "radios.a: unmapped");
    match node.apply(set_op(TWO, AB, Some(&["m", "nope"]), &[])).await {
        Err(OpError::Unknown { field, value, .. }) => {
            assert_eq!((field.as_str(), value.as_str()), ("members[1]", "nope"))
        }
        other => panic!("expected unknown, got {other:?}"),
    }
}

/// A stand-in for a stage of the given kind, stream kinds and readiness.
struct Stand {
    spec: StageSpec,
    readiness: Readiness,
}

impl Stand {
    fn new(kind: &'static str, outputs: &[StreamKind], readiness: Readiness) -> Arc<Self> {
        Self::with_inputs(kind, &[StreamKind::Audio], outputs, readiness)
    }

    fn with_inputs(
        kind: &'static str,
        inputs: &[StreamKind],
        outputs: &[StreamKind],
        readiness: Readiness,
    ) -> Arc<Self> {
        Arc::new(Self {
            spec: StageSpec {
                kind,
                inputs: inputs.to_vec(),
                outputs: outputs.to_vec(),
                params_schema: schemars::json_schema!({"type": "object"}),
            },
            readiness,
        })
    }
}

#[async_trait]
impl Stage for Stand {
    fn spec(&self) -> &StageSpec {
        &self.spec
    }
    fn backend(&self) -> Backend {
        Backend::Cpu
    }
    fn readiness(&self) -> Readiness {
        self.readiness.clone()
    }
    async fn run(
        &self,
        _params: serde_json::Value,
        mut inputs: Vec<broadcast::Receiver<Frame>>,
        outputs: Vec<StreamId>,
        bus: Arc<StreamBus>,
        cancel: CancellationToken,
        _ctx: StageContext,
    ) -> Result<(), StageError> {
        let Some(rx) = inputs.first_mut() else {
            return Err(StageError::Failed("expected 1 input".into()));
        };
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(StageError::Cancelled),
                r = rx.recv() => match r {
                    Ok(frame) => {
                        if let (Some(out), Payload::Audio { .. }) = (outputs.first(), &frame.payload)
                            && self.spec.outputs.first() == Some(&StreamKind::Audio)
                        {
                            bus.publish(Frame::new(
                                out.clone(),
                                frame.provenance.clone(),
                                frame.payload.clone(),
                            ));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
            }
        }
    }
}

/// The registry with the attached chain's stand-ins; `transcribe` answers `readiness`.
fn attach_registry(readiness: Readiness) -> StageRegistry {
    let mut r = registry();
    r.register(Arc::new(StreamInStage::new()));
    r.register(Stand::new(
        "denoise",
        &[StreamKind::Audio],
        Readiness::always(),
    ));
    r.register(Stand::new(
        "vad",
        &[StreamKind::Audio, StreamKind::Event],
        Readiness::always(),
    ));
    r.register(Stand::new("transcribe", &[StreamKind::Event], readiness));
    r
}

#[tokio::test]
async fn set_stop_stops_the_instances_jobs_only() {
    let dir = tempfile::tempdir().unwrap();
    let a = Radio::new("a");
    let node = open_radios(dir.path(), &[&a], attach_registry(Readiness::always())).await;
    let r = started_set(&node, set_op("talk", &[("a", Some("a"))], None, &[])).await;
    assert_eq!(r.started.len(), 3, "{:?}", r.refused);
    let attached: Vec<JobId> = r
        .started
        .iter()
        .map(|s| s.attached.clone().expect("attached"))
        .collect();
    let running = running_of(&node, "talk").await.unwrap();
    assert_eq!(
        running.members.len(),
        6,
        "demodulators and their transcribers"
    );
    // A hand-started job inside the set's capture is not the set's.
    let capture = r.radios["a"].capture;
    let hand = job(
        &node,
        job_start("a", JobKind::Measure, band(capture.center_hz.0, 5_000)),
    )
    .await;
    assert_eq!(a.tunes(), 1);

    let (stopped, ended, _) = stopped_set(&node, "talk").await;
    assert!(ended.is_empty(), "{ended:?}");
    assert_eq!(stopped.len(), 6, "{stopped:?}");
    for s in &r.started {
        assert!(stopped.contains(&s.job));
    }
    for j in &attached {
        assert!(stopped.contains(j));
    }
    until("only the hand job runs", || {
        running_jobs(&node) == vec![hand.clone()]
    })
    .await;
    assert!(running_of(&node, "talk").await.is_none());
    assert!(!instance_file(dir.path(), "talk").exists());
    assert_eq!(holders(&node, "a").await, vec![hand]);
}

#[tokio::test]
async fn set_stop_with_ended_members_stops_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let r = started_set(&node, set_op(TWO, AB, None, &[])).await;
    let (m, w) = (r.job("m"), r.job("w"));
    // `m` is stopped by hand; `w` fails: the node reopens without source `b`.
    node.apply(Op::JobStop { job: m.clone() }).await.unwrap();
    until("m ends", || !running_jobs(&node).contains(&m)).await;
    drop(node);
    let a = Radio::new("a");
    let node = open_radios(dir.path(), &[&a], registry()).await;
    let state = |id: &JobId| {
        node.job_list()
            .into_iter()
            .find(|r| &r.id == id)
            .map(|r| r.state)
    };
    until("w fails", || {
        matches!(state(&w), Some(JobState::Failed { .. }))
    })
    .await;
    assert_eq!(state(&m), Some(JobState::Stopped));
    let running = running_of(&node, TWO).await.expect("three jobs still run");
    let ended: Vec<&str> = running
        .members
        .iter()
        .filter(|e| e.state == "ended")
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(ended, ["m", "w"]);

    let (stopped, ended, failed) = stopped_set(&node, TWO).await;
    assert_eq!(ended, vec![m, w]);
    let cb: Vec<JobId> = r
        .started
        .iter()
        .filter(|s| s.member == "cb")
        .map(|s| s.job.clone())
        .collect();
    assert_eq!(stopped, cb);
    assert_eq!(failed, 0);
    assert!(running_of(&node, TWO).await.is_none());
    assert!(!instance_file(dir.path(), TWO).exists());
    until("nothing runs", || running_jobs(&node).is_empty()).await;
    // A set that does not run stops nothing.
    let (stopped, ended, _) = stopped_set(&node, TWO).await;
    assert!(stopped.is_empty() && ended.is_empty());
}

#[tokio::test]
async fn an_instance_whose_jobs_all_ended_reads_not_running_and_the_next_start_drops_it() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let r = started_set(&node, set_op(TWO, &[("b", Some("b"))], Some(&["m"]), &[])).await;
    node.apply(Op::JobStop { job: r.job("m") }).await.unwrap();
    until("m ends", || running_jobs(&node).is_empty()).await;
    // `set_list` is a read: it shows the set as not running and leaves the file.
    assert!(running_of(&node, TWO).await.is_none());
    assert!(instance_file(dir.path(), TWO).is_file());
    assert_eq!(node.set_instances().len(), 1);
    // A new mapping is then a fresh start, not a refusal; the old instance is gone.
    let r = started_set(&node, set_op(TWO, &[("b", Some("a"))], Some(&["m"]), &[])).await;
    assert_eq!(r.started_members(), [("m", None)]);
    let inst = node.set_instances().remove(TWO).expect("the new instance");
    assert_eq!(inst.radios.get("b").map(|s| s.0.as_str()), Some("a"));
    assert_eq!(inst.jobs().collect::<Vec<_>>(), vec![&r.job("m")]);
    let text = std::fs::read_to_string(instance_file(dir.path(), TWO)).unwrap();
    let on_disk: zeta_core::sets::SetInstance = serde_json::from_str(&text).unwrap();
    assert_eq!(on_disk, inst);
}

/// A `set_start` future dropped once its starts are under way (here inside the elastic
/// spectrum's relaunch, the first point it waits at after a tune): every job it started
/// is already in the instance, so `set_stop` stops them all and leaves no holder.
#[tokio::test]
async fn a_set_start_dropped_after_its_starts_leaves_every_job_to_set_stop() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let spectrum = Op::JobStart {
        kind: JobKind::Spectrum,
        source: SourceId("a".into()),
        center_hz: Hz(100_000_000),
        bandwidth_hz: Hz(1_000_000),
        deviation_hz: None,
        sideband: None,
        dwell_ms: None,
        average: None,
        bits: None,
        method: None,
        bin_hz: None,
        elastic: true,
        replace: vec![],
    };
    job(&node, spectrum).await;
    let dropped =
        tokio::time::timeout(Duration::ZERO, node.apply(set_op(TWO, AB, None, &[]))).await;
    assert!(
        dropped.is_err(),
        "the set_start was not dropped: {dropped:?}"
    );
    assert_eq!((a.tunes(), b.tunes()), (2, 1), "both sources tuned");
    let inst = node.set_instances().remove(TWO).expect("the instance");
    let jobs: Vec<JobId> = inst.jobs().cloned().collect();
    assert_eq!(jobs.len(), 5, "{inst:?}");
    assert!(instance_file(dir.path(), TWO).is_file());
    for j in &jobs {
        assert!(running_jobs(&node).contains(j), "{j:?} runs");
    }
    let (stopped, ended, _) = stopped_set(&node, TWO).await;
    assert_eq!(stopped, jobs);
    assert!(ended.is_empty());
    until("the set's jobs end", || {
        let run = running_jobs(&node);
        jobs.iter().all(|j| !run.contains(j))
    })
    .await;
    assert!(holders(&node, "b").await.is_empty());
    assert!(holders(&node, "a").await.is_empty());
    assert!(!instance_file(dir.path(), TWO).exists());
}

#[tokio::test]
async fn a_restart_keeps_the_instance() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let r = started_set(&node, set_op(TWO, AB, None, &[])).await;
    let m = r.job("m");
    node.apply(Op::JobStop { job: m.clone() }).await.unwrap();
    until("m ends", || !running_jobs(&node).contains(&m)).await;
    drop(node);

    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    assert!(
        node.persist_errors().is_empty(),
        "{:?}",
        node.persist_errors()
    );
    until("the set's jobs resume", || running_jobs(&node).len() == 4).await;
    let running = running_of(&node, TWO).await.expect("the instance resumed");
    let states: Vec<(&str, &JobId, &str)> = running
        .members
        .iter()
        .map(|e| (e.id.as_str(), &e.job, e.state.as_str()))
        .collect();
    let want: Vec<(&str, &JobId, &str)> = r
        .started
        .iter()
        .map(|s| {
            let state = if s.job == m { "ended" } else { "running" };
            (s.member.as_str(), &s.job, state)
        })
        .collect();
    assert_eq!(states, want);
    assert_eq!(node.set_instances().len(), 1);
}

#[tokio::test]
async fn an_unreadable_instance_is_a_persist_error_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let a = Radio::new("a");
    let sets = dir.path().join("state").join("set-instances");
    std::fs::create_dir_all(&sets).unwrap();
    std::fs::write(sets.join("two_radio.json"), "nope").unwrap();
    let node = open_radios(dir.path(), &[&a], registry()).await;
    let errors = node.persist_errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("two_radio.json"), "{errors:?}");
    assert!(running_of(&node, TWO).await.is_none());
}

#[tokio::test]
async fn gates_are_taken_in_source_order_and_a_dropped_start_leaves_no_holder() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    b.slow.store(true, Ordering::SeqCst);
    let dropped = tokio::time::timeout(
        Duration::from_millis(100),
        node.apply(set_op(TWO, AB, None, &[])),
    )
    .await;
    assert!(
        dropped.is_err(),
        "the set_start was not dropped: {dropped:?}"
    );
    // `a` sorts first: its tune is done, `b`'s was under way.
    assert_eq!((a.tunes(), b.tunes()), (1, 1), "dropped inside b's tune");
    b.slow.store(false, Ordering::SeqCst);
    assert!(holders(&node, "a").await.is_empty());
    assert!(holders(&node, "b").await.is_empty());
    assert!(running_jobs(&node).is_empty());
    assert!(!instance_file(dir.path(), TWO).exists());
    assert!(running_of(&node, TWO).await.is_none());
    // Both gates were let go: a start on either source neither waits nor conflicts.
    for s in ["a", "b"] {
        let id = tokio::time::timeout(
            Duration::from_secs(5),
            job(
                &node,
                job_start(s, JobKind::Passthrough, band(144_800_000, 12_500)),
            ),
        )
        .await
        .expect("the gate was let go");
        assert_eq!(holders(&node, s).await, vec![id]);
    }
}

#[test]
fn a_truncated_set_result_keeps_its_counts() {
    let started = (0..2000)
        .map(|i| SetStarted {
            member: "cb".into(),
            channel: Some(i),
            center_hz: Hz(476_425_000),
            job: JobId::new(),
            attached: None,
            attach_error: None,
        })
        .collect();
    let warning = "members[0]: transcribe: down; started without attach";
    let r = OpResult::SetStart {
        set: TWO.into(),
        radios: Default::default(),
        started,
        existing: vec![],
        refused: vec![],
        warnings: vec![warning.into()],
    };
    let v = r.summary_json();
    assert_eq!(v["truncated"], true);
    assert_eq!(v["result"], "set_start");
    assert_eq!(
        v["counts"],
        serde_json::json!({"started": 2000, "existing": 0, "refused": 0})
    );
    assert_eq!(v["warnings"], serde_json::json!([warning]));
}

// ---- members: `null` radios, watch members, rows this host lacks, unready attaches ----

/// The `ops.log` entries naming `op`.
fn logged(dir: &Path, op: &str) -> usize {
    std::fs::read_to_string(dir.join("state").join("ops.log"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|l| l["op"]["op"] == op)
        .count()
}

/// The job `id`'s record.
fn record(node: &Node, id: &JobId) -> zeta_core::job::JobRecord {
    node.job_list()
        .into_iter()
        .find(|r| &r.id == id)
        .unwrap_or_else(|| panic!("{id:?} listed"))
}

#[tokio::test]
async fn a_null_radio_picks_the_first_free_source_in_role_order() {
    let dir = tempfile::tempdir().unwrap();
    // `narrow` (1 MHz at once) sorts before `wide` (20 MHz): role `a`, first in the
    // file, takes `narrow` for its CB channels; role `b`'s 1 MHz band then takes `wide`.
    let (narrow, wide) = (Radio::with_max("narrow", 1_000_000), Radio::new("wide"));
    let node = open_two(dir.path(), &narrow, &wide).await;
    let r = started_set(&node, set_op(TWO, &[("a", None), ("b", None)], None, &[])).await;
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(r.radios["a"].source.0, "narrow");
    assert_eq!(r.radios["b"].source.0, "wide");
    assert_eq!(r.started.len(), 5, "{:?}", r.started_members());
    assert_eq!((narrow.tunes(), wide.tunes()), (1, 1));
    let running = running_of(&node, TWO).await.expect("an instance");
    assert_eq!(running.radios["a"].0, "narrow");
    assert_eq!(running.radios["b"].0, "wide");
    // The same mapping named explicitly is the idempotent re-run.
    let picked: &[(&str, Option<&str>)] = &[("a", Some("narrow")), ("b", Some("wide"))];
    let again = started_set(&node, set_op(TWO, picked, None, &[])).await;
    assert!(again.started.is_empty(), "{:?}", again.started_members());
    assert_eq!(again.existing.len(), 5);
    // So is the `null` request again: while the set runs, its roles keep their sources.
    let nulls = started_set(&node, set_op(TWO, &[("a", None), ("b", None)], None, &[])).await;
    assert!(nulls.started.is_empty(), "{:?}", nulls.started_members());
    assert_eq!(nulls.existing.len(), 5);
    assert_eq!(nulls.radios["a"].source.0, "narrow");
    assert_eq!((narrow.tunes(), wide.tunes()), (1, 1));
}

#[tokio::test]
async fn no_free_source_is_a_conflict_naming_the_role() {
    let dir = tempfile::tempdir().unwrap();
    let b = Radio::new("b");
    let node = open_radios(dir.path(), &[&b], registry()).await;
    let holder = job(
        &node,
        job_start("b", JobKind::Measure, band(900_000_000, 20_000)),
    )
    .await;
    let tunes = b.tunes();
    let r = node
        .apply(set_op(TWO, &[("b", None)], Some(&["w"]), &[]))
        .await;
    let Err(OpError::Conflict {
        source,
        holders,
        current,
        options,
    }) = r
    else {
        panic!("expected a conflict, got {r:?}");
    };
    assert_eq!(source.0, "b");
    assert_eq!(holders, vec![holder.clone()]);
    assert_eq!(current, band(900_000_000, 20_000));
    assert_eq!(
        options[0],
        format!(
            "radios.b: no free source covers 300000000 Hz ± 500000 Hz; busy: b (holders [{}])",
            holder.0
        )
    );
    assert_eq!(b.tunes(), tunes, "nothing tuned");
    assert_eq!(running_jobs(&node), vec![holder]);
    assert!(running_of(&node, TWO).await.is_none());
}

/// Pin `source` at `t`.
async fn pin(node: &Node, source: &str, t: Tuning) {
    let op = Op::SourcePin {
        source: SourceId(source.into()),
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
        replace: vec![],
    };
    match node.apply(op).await {
        Ok(OpResult::Tuning(_)) => {}
        other => panic!("pin refused: {other:?}"),
    }
}

/// `source_free` over `t`: the free ids and the busy ids.
async fn free_and_busy(node: &Node, t: Tuning) -> (Vec<String>, Vec<String>) {
    let op = Op::SourceFree {
        center_hz: t.center_hz,
        bandwidth_hz: t.bandwidth_hz,
    };
    match node.apply(op).await {
        Ok(OpResult::SourceFree { free, busy }) => (
            free.into_iter().map(|s| s.0).collect(),
            busy.into_iter().map(|b| b.id.0).collect(),
        ),
        other => panic!("source_free: {other:?}"),
    }
}

/// A pin admits jobs inside it, as `plan_start` inside a pin does: a `null` role picks
/// a radio pinned over its whole band (no tune: the pin's capture holds it), and
/// `source_free` lists it free.
#[tokio::test]
async fn a_null_radio_picks_a_source_pinned_over_its_band() {
    let dir = tempfile::tempdir().unwrap();
    let b = Radio::new("b");
    let node = open_radios(dir.path(), &[&b], registry()).await;
    let watch = band(300_000_000, 1_000_000);
    pin(&node, "b", band(300_500_000, 4_000_000)).await;
    assert_eq!(
        free_and_busy(&node, watch).await,
        (vec!["b".into()], vec![])
    );
    let tunes = b.tunes();
    let r = started_set(&node, set_op(TWO, &[("b", None)], Some(&["w"]), &[])).await;
    assert_eq!(r.radios["b"].source.0, "b");
    assert_eq!(r.started_members(), [("w", None)]);
    assert_eq!(b.tunes(), tunes, "inside the pin: no tune");
    assert_eq!(
        node.pinned(&SourceId("b".into())),
        Some(band(300_500_000, 4_000_000))
    );
}

/// A pin that does not cover the whole band keeps the source busy, for `source_free`
/// and for a `null` role alike.
#[tokio::test]
async fn a_pin_that_does_not_cover_the_band_stays_busy() {
    let dir = tempfile::tempdir().unwrap();
    let b = Radio::new("b");
    let node = open_radios(dir.path(), &[&b], registry()).await;
    let watch = band(300_000_000, 1_000_000);
    // 299.8-300.2 MHz: inside the watch's band, not over it.
    pin(&node, "b", band(300_000_000, 400_000)).await;
    assert_eq!(
        free_and_busy(&node, watch).await,
        (vec![], vec!["b".into()])
    );
    let r = node
        .apply(set_op(TWO, &[("b", None)], Some(&["w"]), &[]))
        .await;
    // A conflict with no holder: the pin, and the way out of it.
    let Err(OpError::Conflict {
        source,
        holders,
        options,
        ..
    }) = r
    else {
        panic!("expected a conflict, got {r:?}");
    };
    assert_eq!(source.0, "b");
    assert!(holders.is_empty());
    assert!(
        options[0].starts_with("radios.b: no free source covers") && options[0].contains("pinned"),
        "{options:?}"
    );
    assert!(
        options.contains(&"unpin source b (source_unpin)".to_string()),
        "{options:?}"
    );
    assert!(running_jobs(&node).is_empty());
}

/// Every covering source busy: the conflict names the first and the holders of all.
#[tokio::test]
async fn a_pick_conflict_names_every_covering_sources_holders() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let on_a = job(
        &node,
        job_start("a", JobKind::Measure, band(900_000_000, 20_000)),
    )
    .await;
    let on_b = job(
        &node,
        job_start("b", JobKind::Measure, band(800_000_000, 20_000)),
    )
    .await;
    let r = node
        .apply(set_op(TWO, &[("b", None)], Some(&["w"]), &[]))
        .await;
    let Err(OpError::Conflict {
        source,
        holders,
        options,
        ..
    }) = r
    else {
        panic!("expected a conflict, got {r:?}");
    };
    assert_eq!(source.0, "a");
    assert_eq!(holders, vec![on_a, on_b]);
    assert!(options[0].starts_with("radios.b: "), "{options:?}");
}

/// A rigid holder inside a covering pin still makes the source busy.
#[tokio::test]
async fn a_held_source_inside_a_covering_pin_stays_busy() {
    let dir = tempfile::tempdir().unwrap();
    let b = Radio::new("b");
    let node = open_radios(dir.path(), &[&b], registry()).await;
    let watch = band(300_000_000, 1_000_000);
    pin(&node, "b", band(300_000_000, 4_000_000)).await;
    let holder = job(
        &node,
        job_start("b", JobKind::Measure, band(301_000_000, 20_000)),
    )
    .await;
    assert_eq!(
        free_and_busy(&node, watch).await,
        (vec![], vec!["b".into()])
    );
    let r = node
        .apply(set_op(TWO, &[("b", None)], Some(&["w"]), &[]))
        .await;
    assert!(r.is_err(), "{r:?}");
    assert_eq!(running_jobs(&node), vec![holder]);
}

#[tokio::test]
async fn a_watch_member_on_a_band_another_watch_holds_is_other_unless_the_policy_matches() {
    let dir = tempfile::tempdir().unwrap();
    let b = Radio::new("b");
    let node = open_radios(dir.path(), &[&b], registry()).await;
    let only_b: &[(&str, Option<&str>)] = &[("b", Some("b"))];
    let hand = job(
        &node,
        job_start("b", JobKind::Watch, band(300_000_000, 1_000_000)),
    )
    .await;
    let m = other_message(node.apply(set_op(TWO, only_b, Some(&["w"]), &[])).await);
    assert_eq!(
        m,
        format!(
            "watch {} holds this band with other policy; stop it or graph_put it",
            hand.0
        )
    );
    assert_eq!(running_jobs(&node), vec![hand.clone()], "nothing started");
    assert!(running_of(&node, TWO).await.is_none());
    // The member's policy with some of the keys it leaves out spelled at their
    // defaults is the same policy: the hand watch is the member.
    let mut graph = record(&node, &hand).graph;
    for n in graph.nodes.iter_mut().filter(|n| n.kind == "detect") {
        n.params = serde_json::json!({
            "modes": ["pocsag_decode"], "max_decoders": 16, "cooldown_ms": 30000,
            "threshold_db": 6, "edge_mask": 0.05
        });
    }
    node.apply(Op::GraphPut {
        job: hand.clone(),
        graph,
    })
    .await
    .unwrap();
    let r = started_set(&node, set_op(TWO, only_b, Some(&["w"]), &[])).await;
    assert!(r.started.is_empty(), "{:?}", r.started_members());
    assert_eq!(r.existing.len(), 1);
    assert_eq!(
        (r.existing[0].member.as_str(), &r.existing[0].job),
        ("w", &hand)
    );
    // The hand watch is not the set's to stop: `set_stop` leaves it.
    stopped_set(&node, TWO).await;
    assert_eq!(running_jobs(&node), vec![hand.clone()]);
    node.apply(Op::JobStop { job: hand }).await.unwrap();
    until("the hand watch ends", || running_jobs(&node).is_empty()).await;

    // Started by the set, a second start finds it and restarts nothing.
    let first = started_set(&node, set_op(TWO, only_b, Some(&["w"]), &[])).await;
    let w = first.job("w");
    let created = record(&node, &w).created_ns;
    let puts = logged(dir.path(), "graph_put");
    let second = started_set(&node, set_op(TWO, only_b, Some(&["w"]), &[])).await;
    assert!(second.started.is_empty(), "{:?}", second.started_members());
    assert_eq!(second.existing.len(), 1);
    assert_eq!(second.existing[0].job, w);
    assert_eq!(record(&node, &w).created_ns, created);
    assert_eq!(logged(dir.path(), "graph_put"), puts, "no restart");
    assert_eq!(running_jobs(&node), vec![w]);
}

#[tokio::test]
async fn a_watch_member_runs_with_its_params() {
    let dir = tempfile::tempdir().unwrap();
    let b = Radio::new("b");
    let node = open_radios(dir.path(), &[&b], registry()).await;
    let r = started_set(&node, set_op(TWO, &[("b", Some("b"))], Some(&["w"]), &[])).await;
    let w = r.job("w");
    let graph = match node.apply(Op::GraphGet { job: w.clone() }).await {
        Ok(OpResult::Graph(g)) => g,
        other => panic!("graph_get: {other:?}"),
    };
    let detect = graph.nodes.iter().find(|n| n.kind == "detect").unwrap();
    assert_eq!(detect.params["modes"], serde_json::json!(["pocsag_decode"]));
    until("the watch runs its loop", || {
        node.job_context(&w)
            .is_some_and(|c| c["watch"]["cap"]["max"] == 16)
    })
    .await;
    // One launch: the params went on the graph before the start, not by a restart.
    assert_eq!(logged(dir.path(), "job_start"), 0);
    assert_eq!(logged(dir.path(), "graph_put"), 0);
    assert_eq!(logged(dir.path(), "set_start"), 1);
    assert_eq!(record(&node, &w).state, JobState::Running);
}

/// A program for a row that must be ready on every host: it reads its input and exits
/// when the input closes.
fn stand_in_program(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join("zeta-test-stand-in");
    std::fs::write(&p, "#!/bin/sh\nexec cat >/dev/null\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[tokio::test]
async fn the_airband_set_starts_what_this_host_has_and_refuses_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    // The operator's rows: `vdl2_decode` on a program every host has, `adsb_decode` on
    // one no host has, so the answer is the same whatever is installed.
    let modes_dir = dir.path().join("modes");
    std::fs::create_dir_all(&modes_dir).unwrap();
    let program = stand_in_program(dir.path());
    let builtin_vdl2 = include_str!("../../../modes/vdl2_decode.json");
    let vdl2 = builtin_vdl2.replace(
        r#""programs": ["dumpvdl2"]"#,
        &format!(r#""programs": ["{}"]"#, program.display()),
    );
    assert_ne!(vdl2, builtin_vdl2);
    std::fs::write(modes_dir.join("vdl2_decode.json"), vdl2).unwrap();
    let builtin_adsb = include_str!("../../../modes/adsb_decode.json");
    let adsb = builtin_adsb.replace(
        r#""programs": ["dump1090", "dump1090-fa", "dump1090-mutability", "readsb"]"#,
        r#""programs": ["zeta-test-missing-decoder"]"#,
    );
    assert_ne!(adsb, builtin_adsb);
    std::fs::write(modes_dir.join("adsb_decode.json"), adsb).unwrap();

    // A stand-in for zeta-ext's program runner, and the rows probed as the CLI probes
    // them at startup (`find_handler`): the stand-in is found, the missing decoder not.
    let mut stages = registry();
    stages.register(Stand::with_inputs(
        "external_decode",
        &[StreamKind::Iq, StreamKind::Audio],
        &[StreamKind::Event],
        Readiness::always(),
    ));
    let mut modes = ModeRegistry::load(&stages, Some(&modes_dir));
    let found = find_handler(&[program.display().to_string()]);
    assert_eq!(found.as_deref(), Some(program.as_path()));
    modes.set_program("vdl2_decode", found, None);
    let missing = "zeta-test-missing-decoder".to_string();
    assert_eq!(find_handler(std::slice::from_ref(&missing)), None);
    modes.set_program(
        "adsb_decode",
        None,
        Some(format!("{missing}: missing — modes: adsb_decode")),
    );
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let ports: Vec<Arc<dyn Port>> = vec![a.clone(), b.clone()];
    let node = Node::open_with_modes(cfg(dir.path(), None), stages, ports, Some(Arc::new(modes)))
        .await
        .unwrap();
    let mapping: &[(&str, Option<&str>)] = &[("wide", Some("a")), ("second", Some("b"))];
    let r = started_set(&node, set_op("airband", mapping, None, &[])).await;
    assert_eq!(
        r.started_members(),
        [
            ("acars", Some(1)),
            ("acars", Some(2)),
            ("acars", Some(3)),
            ("acars", Some(4)),
            ("acars", Some(5)),
            ("acars", Some(6)),
            ("acars", Some(7)),
            ("vdl2", None),
        ],
        "{:?}",
        r.refused
    );
    assert_eq!(r.refused.len(), 1, "{:?}", r.refused);
    assert_eq!(r.refused[0].member, "adsb");
    match &r.refused[0].error {
        OpError::Other { message } => assert!(message.starts_with("unavailable"), "{message}"),
        e => panic!("expected other, got {e:?}"),
    }
    // The second radio's only member was refused: it is neither tuned nor reported.
    assert_eq!(b.tunes(), 0);
    assert_eq!(a.tunes(), 1);
    assert_eq!(r.radios.keys().collect::<Vec<_>>(), ["wide"]);
    assert!(holders(&node, "b").await.is_empty());
    let running = running_of(&node, "airband").await.expect("an instance");
    assert_eq!(running.radios.keys().collect::<Vec<_>>(), ["wide"]);
}

#[tokio::test]
async fn a_member_of_a_kind_this_node_does_not_know_is_refused_and_the_rest_start() {
    let dir = tempfile::tempdir().unwrap();
    write_set(
        &dir.path().join("sets"),
        "odd.json",
        r#"{"id": "odd", "name": "odd", "notes": "", "radios": {"r": {"note": "any"}},
            "members": [
              {"id": "m", "radio": "r", "op": "job_start", "kind": "measure",
               "center_hz": 300000000, "bandwidth_hz": 20000, "deviation_hz": null,
               "sideband": null},
              {"id": "x", "radio": "r", "op": "job_start", "kind": "no_such_decoder",
               "center_hz": 300010000, "bandwidth_hz": 20000, "deviation_hz": null,
               "sideband": null}]}"#,
    );
    let a = Radio::new("a");
    let node = open_radios(dir.path(), &[&a], registry()).await;
    let r = started_set(&node, set_op("odd", &[("r", Some("a"))], None, &[])).await;
    assert_eq!(r.started_members(), [("m", None)]);
    assert_eq!(r.refused.len(), 1, "{:?}", r.refused);
    assert_eq!(r.refused[0].member, "x");
    match &r.refused[0].error {
        OpError::Unknown {
            field,
            value,
            known,
        } => {
            assert_eq!(
                (field.as_str(), value.as_str()),
                ("members[1].kind", "no_such_decoder")
            );
            assert!(known.iter().any(|k| k == "acars_decode"), "{known:?}");
            assert!(known.iter().any(|k| k == "vdl2_decode"), "{known:?}");
        }
        e => panic!("expected unknown, got {e:?}"),
    }
}

/// A watch member whose `modes` names a row this node lacks is refused in the checks,
/// before any gate: its radio, which has no other member, is never tuned.
#[tokio::test]
async fn a_watch_member_naming_a_row_this_node_lacks_is_refused_before_any_tune() {
    let dir = tempfile::tempdir().unwrap();
    write_set(
        &dir.path().join("sets"),
        "lacks.json",
        r#"{"id": "lacks", "name": "lacks", "notes": "",
            "radios": {"r1": {"note": "any"}, "r2": {"note": "any"}},
            "members": [
              {"id": "m", "radio": "r1", "op": "job_start", "kind": "measure",
               "center_hz": 300000000, "bandwidth_hz": 20000, "deviation_hz": null,
               "sideband": null},
              {"id": "w", "radio": "r2", "op": "job_start", "kind": "watch",
               "center_hz": 148500000, "bandwidth_hz": 1000000, "deviation_hz": null,
               "sideband": null, "params": {"modes": ["pocsag_decode", "no_such_row"]}}]}"#,
    );
    let (a, b) = (Radio::new("a"), Radio::new("b"));
    let node = open_two(dir.path(), &a, &b).await;
    let mapping: &[(&str, Option<&str>)] = &[("r1", Some("a")), ("r2", Some("b"))];
    let r = started_set(&node, set_op("lacks", mapping, None, &[])).await;
    assert_eq!(r.started_members(), [("m", None)], "{:?}", r.refused);
    assert_eq!(r.refused.len(), 1, "{:?}", r.refused);
    assert_eq!(r.refused[0].member, "w");
    match &r.refused[0].error {
        OpError::Other { message } => assert_eq!(
            message,
            "members[1].params.modes[1]: unknown mode no_such_row"
        ),
        e => panic!("expected other, got {e:?}"),
    }
    assert_eq!((a.tunes(), b.tunes()), (1, 0));
    assert_eq!(r.radios.keys().collect::<Vec<_>>(), ["r1"]);
    assert!(holders(&node, "b").await.is_empty());
}

#[tokio::test]
async fn an_unready_attach_starts_the_member_without_it_and_warns() {
    let dir = tempfile::tempdir().unwrap();
    let a = Radio::new("a");
    let stages = attach_registry(Readiness::unready("no speech backend"));
    let node = open_radios(dir.path(), &[&a], stages).await;
    let r = started_set(&node, set_op("talk", &[("a", Some("a"))], None, &[])).await;
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert_eq!(
        r.started_members(),
        [("cb", Some(1)), ("cb", Some(2)), ("cb", Some(3))]
    );
    assert!(
        r.started.iter().all(|s| s.attached.is_none()),
        "{:?}",
        r.started
    );
    assert_eq!(
        r.warnings,
        ["members[0]: transcribe: no speech backend; started without attach"]
    );
    assert_eq!(running_jobs(&node).len(), 3);
}
