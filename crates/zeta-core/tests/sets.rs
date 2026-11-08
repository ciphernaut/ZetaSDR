//! Named sets: the set files (built in, then the operator's `--sets-dir`), loaded with
//! the mode registry's rules and listed by `set_list`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use zeta_core::modes::ModeRegistry;
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpResult, SetInfo};
use zeta_core::port::{IqBlock, IqReceiver, Port, PortError, Tuning};
use zeta_core::sets::{SetRegistry, SetState};
use zeta_core::stage::{Backend, PassthroughStage, StageRegistry};
use zeta_core::types::{Capabilities, Claim, Hz, SampleFormat, SourceId};

/// A radio that tunes to exactly the band asked for.
struct Radio {
    id: SourceId,
    caps: Capabilities,
    cur: Mutex<Option<Tuning>>,
    tx: broadcast::Sender<IqBlock>,
}

impl Radio {
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
            },
            cur: Mutex::new(None),
            tx: broadcast::channel(1).0,
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
fn the_two_builtin_sets_parse_validate_and_are_ready() {
    let sets = load(None);
    assert_eq!(sets.ids(), ["airband", "pagers_au"]);
    for s in sets.all() {
        assert_eq!(s.state, SetState::Ready, "{s:?}");
        assert_eq!(s.reason, None, "{s:?}");
    }
    let airband = sets.get("airband").unwrap();
    assert_eq!(airband.file, "sets/airband.json");
    let roles: Vec<&str> = airband.set.radios.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(roles, ["wide", "second"], "the file's order");
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
    assert_eq!(ids, ["airband", "pagers_au"]);
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
    let v = serde_json::to_value(&sets[1]).unwrap();
    assert_eq!(v["running"], serde_json::Value::Null, "{v}");
    assert_eq!(v["members"][0]["kind"], "watch", "{v}");
}

#[tokio::test]
async fn a_node_without_a_sets_dir_lists_the_builtins() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), None).await;
    assert_eq!(node.sets().ids(), ["airband", "pagers_au"]);

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
    assert_eq!(ids, ["airband", "mine", "pagers_au"]);
}
