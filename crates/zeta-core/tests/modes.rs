//! The mode registry: the embedded rows under `modes/`, the operator directory layered
//! over them, and the `modes_list`/`modes_show` ops.

use std::path::Path;

use zeta_core::graph::NodeId;
use zeta_core::modes::{
    Container, Decoder, Family, InputStream, ModeRegistry, ModeState, SubjectWaterfall, parse,
    template, validate,
};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::port::Tuning;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, StreamKind};

/// The stages the embedded graph rows are written against.
fn stages() -> StageRegistry {
    let mut r = StageRegistry::new();
    zeta_dsp::register_all(&mut r);
    r
}

const EMBEDDED: [&str; 8] = [
    "acars_decode",
    "adsb_decode",
    "aprs_decode",
    "dfm_decode",
    "m20_decode",
    "pocsag_decode",
    "rds_decode",
    "rs41_decode",
];

fn cfg(dir: &Path, modes_dir: &Path) -> NodeConfig {
    NodeConfig {
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        bank_factory: None,
        modes_dir: Some(modes_dir.to_path_buf()),
    }
}

/// The embedded `pocsag_decode` row with `name` changed.
fn renamed_pocsag(name: &str) -> String {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("pocsag_decode.json"))
        .expect("embedded pocsag_decode");
    let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
    v["name"] = name.into();
    v.to_string()
}

#[test]
fn embedded_rows_parse_and_validate() {
    let stages = stages();
    let embedded = ModeRegistry::embedded();
    assert_eq!(embedded.len(), 8);
    for (file, text) in &embedded {
        let row = parse(text).unwrap_or_else(|e| panic!("{file}: {e}"));
        validate(&row, &stages).unwrap_or_else(|e| panic!("{file}: {e}"));
    }
    let reg = ModeRegistry::load(&stages, None);
    assert_eq!(reg.ids(), EMBEDDED);
    for m in reg.all() {
        assert_eq!(m.state, ModeState::Ready, "{}: {:?}", m.row.id.0, m.reason);
        assert_eq!(m.reason, None);
    }
    assert!(reg.startup_lines().is_empty());
}

#[test]
fn a_broken_override_leaves_the_embedded_row_in_force() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("pocsag_decode.json"),
        r#"{"id":"pocsag_decode","bogus":1}"#,
    )
    .unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    let m = reg.get("pocsag_decode").expect("pocsag_decode listed");
    assert_eq!(m.state, ModeState::Ready);
    assert_eq!(m.file, "modes/pocsag_decode.json");
    assert_eq!(m.row.name, "POCSAG pagers");
    let broken: Vec<_> = reg
        .all()
        .iter()
        .filter(|m| m.state == ModeState::Broken)
        .collect();
    assert_eq!(broken.len(), 1, "{broken:?}");
    let b = broken[0];
    assert!(b.reason.as_deref().unwrap().contains("bogus"), "{b:?}");
    assert!(b.file.ends_with("pocsag_decode.json"), "{b:?}");
    assert_eq!(b.row.id.0, "pocsag_decode@pocsag_decode.json");
    let lines = reg.startup_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("zeta: mode pocsag_decode") && lines[0].contains("bogus"),
        "{lines:?}"
    );
}

#[test]
fn an_operator_row_replaces_an_embedded_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("my_pocsag.json");
    std::fs::write(&path, renamed_pocsag("Local pagers")).unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    assert_eq!(reg.ids(), EMBEDDED);
    let m = reg.get("pocsag_decode").unwrap();
    assert_eq!(m.state, ModeState::Ready);
    assert_eq!(m.row.name, "Local pagers");
    assert_eq!(m.file, path.display().to_string());
}

#[test]
fn a_missing_operator_dir_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let reg = ModeRegistry::load(&stages(), Some(&dir.path().join("absent")));
    assert_eq!(reg.ids(), EMBEDDED);
}

#[tokio::test]
async fn modes_list_filters_by_family_and_state() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join("junk.json"), r#"{"id":"junk_mode","bogus":1}"#).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    let list = async |family, state| match node.apply(Op::ModesList { family, state }).await {
        Ok(OpResult::Modes { modes }) => modes,
        other => panic!("expected modes, got {other:?}"),
    };
    let all = list(None, None).await;
    assert_eq!(all.len(), 9, "{all:?}");
    let aviation: Vec<String> = list(Some(Family::Aviation), None)
        .await
        .into_iter()
        .map(|m| m.id.0)
        .collect();
    assert_eq!(aviation, ["acars_decode", "adsb_decode"]);
    let broken = list(None, Some(ModeState::Broken)).await;
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].id.0, "junk_mode");
    assert!(broken[0].reason.as_deref().unwrap().contains("bogus"));
    // The file to fix: an operator row's path, a built-in row's `modes/<id>.json`.
    assert_eq!(
        broken[0].file,
        modes.join("junk.json").display().to_string()
    );
    let pagers = list(Some(Family::Pagers), Some(ModeState::Ready)).await;
    assert_eq!(pagers.len(), 1);
    assert_eq!(pagers[0].id.0, "pocsag_decode");
    // The wire form: snake-case `state`, `family` and `decoder_kind`, nullable fields `null`.
    let v = serde_json::to_value(&pagers[0]).unwrap();
    assert_eq!(v["state"], "ready");
    assert_eq!(v["family"], "pagers");
    assert_eq!(v["decoder_kind"], "graph");
    assert_eq!(v["file"], "modes/pocsag_decode.json");
    assert_eq!(v["band"]["range_hz"], serde_json::Value::Null);
    assert_eq!(v["band"]["min_hz"], serde_json::Value::Null);
    let adsb = list(Some(Family::Aviation), Some(ModeState::Ready)).await;
    let v = serde_json::to_value(&adsb[1]).unwrap();
    assert_eq!(v["id"], "adsb_decode");
    assert_eq!(v["decoder_kind"], "program");
    assert_eq!(v["reason"], serde_json::Value::Null);
    assert_eq!(v["program"], serde_json::Value::Null);
    assert_eq!(v["checked"], serde_json::Value::Null);
    // `family` and `state` are required, `null` for no filter.
    let e = serde_json::from_value::<Op>(serde_json::json!({"op": "modes_list"}));
    assert!(e.is_err());
    let ok = serde_json::from_value::<Op>(
        serde_json::json!({"op": "modes_list", "family": null, "state": "ready"}),
    );
    assert!(ok.is_ok(), "{ok:?}");
}

/// `modes_dir: None` reads no operator directory (the CLI resolves the default): the
/// node lists exactly the built-in rows.
#[tokio::test]
async fn a_node_without_a_modes_dir_lists_only_the_embedded_rows() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = NodeConfig {
        modes_dir: None,
        ..cfg(dir.path(), dir.path())
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    assert_eq!(node.modes().ids(), EMBEDDED);
    for m in node.modes().all() {
        assert_eq!(m.state, ModeState::Ready);
        assert_eq!(m.file, format!("modes/{}.json", m.row.id.0));
    }
}

#[tokio::test]
async fn modes_show_unknown_names_the_known_ids() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    match node
        .apply(Op::ModesShow {
            id: "aprs_decode".into(),
        })
        .await
    {
        Ok(OpResult::Mode { mode }) => {
            assert_eq!(mode.row.id.0, "aprs_decode");
            assert_eq!(mode.row.label, "APRS");
            assert_eq!(mode.state, ModeState::Ready);
        }
        other => panic!("expected mode, got {other:?}"),
    }
    let e = node
        .apply(Op::ModesShow { id: "nope".into() })
        .await
        .unwrap_err();
    let OpError::Unknown {
        field,
        value,
        known,
    } = e
    else {
        panic!("expected unknown, got {e:?}");
    };
    assert_eq!(field, "id");
    assert_eq!(value, "nope");
    assert_eq!(known, EMBEDDED);
}

/// A node's own `schema` lists its operator rows in the `kind` enumeration (MCP and
/// `ask` inline `$defs` into each tool's input schema).
#[tokio::test]
async fn node_schema_lists_operator_rows_as_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let mut row: serde_json::Value = serde_json::from_str(&renamed_pocsag("Local")).unwrap();
    row["id"] = "pocsag_local".into();
    std::fs::write(modes.join("pocsag_local.json"), row.to_string()).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    let Ok(OpResult::Schema(v)) = node.apply(Op::Schema {}).await else {
        panic!("expected schema");
    };
    let mut seen = 0;
    for part in ["op", "result", "error"] {
        let Some(def) = v[part]["$defs"].get("JobKind") else {
            continue;
        };
        seen += 1;
        let kinds: Vec<&str> = def["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|k| k.as_str())
            .collect();
        assert!(kinds.contains(&"pocsag_local"), "{part}: {kinds:?}");
        assert!(kinds.contains(&"fm_demod"), "{part}: {kinds:?}");
        let mut dedup = kinds.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), kinds.len(), "{part}: {kinds:?}");
    }
    assert!(seen > 0, "no JobKind in {v}");
}

/// A row named for a built-in job kind other than the five decoders would be listed
/// ready and never started (the name maps to the built-in): it loads broken.
#[test]
fn a_built_in_kind_name_is_refused_as_a_row_id() {
    let dir = tempfile::tempdir().unwrap();
    for id in ["fm_demod", "scan", "identify"] {
        let mut row: serde_json::Value = serde_json::from_str(&renamed_pocsag("x")).unwrap();
        row["id"] = id.into();
        std::fs::write(dir.path().join(format!("{id}.json")), row.to_string()).unwrap();
    }
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    for id in ["fm_demod", "scan", "identify"] {
        let m = reg.get(id).unwrap_or_else(|| panic!("{id} listed"));
        assert_eq!(m.state, ModeState::Broken, "{id}");
        assert_eq!(
            m.reason.as_deref(),
            Some(format!("id: {id} is a built-in job kind").as_str()),
            "{id}"
        );
    }
    // The decoder kinds' own names stay operator-replaceable.
    assert_eq!(reg.get("pocsag_decode").unwrap().state, ModeState::Ready);
}

/// An operator program row reading audio (`adsb_decode`'s row with an audio input and an
/// NBFM demod) expands to `channel → nbfm_demod → external_decode`, the demod's squelch off.
#[test]
fn an_audio_program_row_expands_to_channel_nbfm_external() {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("adsb_decode.json"))
        .expect("embedded adsb_decode");
    let mut row: serde_json::Value = serde_json::from_str(text).unwrap();
    row["id"] = "audio_sonde".into();
    row["band"]["min_hz"] = serde_json::Value::Null;
    row["decoder"]["input"] = serde_json::json!({
        "stream": "audio", "format": "s16", "rate_hz": 48000, "via": "stdin", "container": "wav"
    });
    row["decoder"]["demod"] =
        serde_json::json!({"kind": "nbfm", "deviation_hz": 5000, "audio_rate_hz": 48000});
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("audio_sonde.json"), row.to_string()).unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    let m = reg.get("audio_sonde").expect("audio_sonde listed");
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    let tuning = Tuning {
        center_hz: Hz(402_500_000),
        bandwidth_hz: Hz(12_500),
    };
    // An audio row takes any band its channel can cut.
    template::check_band(&m.row, tuning, 2_400_000).unwrap();
    let g = template::expand(&m.row, tuning, 48_000).unwrap();
    let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["channel", "nbfm_demod", "external_decode"]);
    assert_eq!(
        g.nodes[0].params,
        serde_json::json!({"center_hz": 402_500_000, "bandwidth_hz": 12_500, "out_rate_hz": 48_000})
    );
    assert_eq!(
        g.nodes[1].params,
        serde_json::json!({"deviation_hz": 5000, "audio_rate_hz": 48000, "squelch": false})
    );
    assert_eq!(
        g.nodes[2].params,
        serde_json::json!({"mode": "audio_sonde"})
    );
    let edges: Vec<(&NodeId, &NodeId, StreamKind)> =
        g.edges.iter().map(|e| (&e.from, &e.to, e.kind)).collect();
    assert_eq!(
        edges,
        [
            (&g.nodes[0].id, &g.nodes[1].id, StreamKind::Iq),
            (&g.nodes[1].id, &g.nodes[2].id, StreamKind::Audio),
        ]
    );
}

/// The RS41 row is an audio program row: `rs41mod` hears the NBFM demod's audio, its band
/// a range rather than a raster, its events carrying a position.
#[test]
fn the_rs41_row_is_an_audio_program_row() {
    let reg = ModeRegistry::load(&stages(), None);
    let m = reg.get("rs41_decode").expect("rs41_decode embedded");
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    let Decoder::Program(block) = &m.row.decoder else {
        panic!("a program row")
    };
    assert_eq!(block.input.stream, InputStream::Audio);
    assert_eq!(block.input.container, Container::Wav);
    assert_eq!(block.programs, ["rs41mod"]);
    assert_eq!(m.row.position.as_ref().map(|p| p.lat.as_str()), Some("lat"));
    assert_eq!(m.row.time.as_deref(), Some("datetime"));
    assert_eq!(m.row.band.range_hz, Some([400_050_000, 406_000_000]));
    assert!(m.row.band.centers_hz.is_empty());
    assert_eq!(m.row.identify.priority, 1);
    for (id, program) in [("dfm_decode", "dfm09mod"), ("m20_decode", "m20mod")] {
        let m = reg.get(id).expect(id);
        let Decoder::Program(block) = &m.row.decoder else {
            panic!("{id}: a program row")
        };
        assert_eq!(block.programs, [program]);
        assert_eq!(block.input.stream, InputStream::Audio);
        assert_eq!(m.row.identify.priority, 0);
    }
}

#[tokio::test]
async fn modes_list_and_show_carry_the_subject() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    let Ok(OpResult::Modes { modes }) = node
        .apply(Op::ModesList {
            family: None,
            state: None,
        })
        .await
    else {
        panic!("expected modes");
    };
    let info =
        |id: &str| serde_json::to_value(modes.iter().find(|m| m.id.0 == id).unwrap()).unwrap();
    assert_eq!(info("adsb_decode")["subject"]["key"], "icao");
    assert_eq!(info("adsb_decode")["subject"]["waterfall"], "summary");
    assert_eq!(info("pocsag_decode")["subject"], serde_json::Value::Null);
    let Ok(OpResult::Mode { mode }) = node
        .apply(Op::ModesShow {
            id: "adsb_decode".into(),
        })
        .await
    else {
        panic!("expected mode");
    };
    let v = serde_json::to_value(&mode).unwrap();
    assert_eq!(v["row"]["subject"]["columns"].as_array().unwrap().len(), 4);
    assert_eq!(v["row"]["position"]["alt_unit"], "ft");
    assert_eq!(v["row"]["subject"]["track_points"], 100);
    assert_eq!(v["row"]["subject"]["track_min_m"], 500.0);
}

#[test]
fn the_rows_thin_their_tracks_as_planned() {
    let plan = [
        ("adsb_decode", 100, 500.0),
        ("rs41_decode", 500, 100.0),
        ("dfm_decode", 500, 100.0),
        ("m20_decode", 500, 100.0),
        ("aprs_decode", 100, 50.0),
        ("acars_decode", 100, 0.0),
        ("rds_decode", 100, 0.0),
    ];
    let reg = ModeRegistry::load(&stages(), None);
    for (id, points, min_m) in plan {
        let row = &reg.all().iter().find(|m| m.row.id.0 == id).unwrap().row;
        let sub = row.subject.as_ref().unwrap();
        assert_eq!((sub.track_points, sub.track_min_m), (points, min_m), "{id}");
    }
}

#[test]
fn the_rows_name_the_planned_subjects() {
    type Plan = (
        &'static str,
        &'static str,
        &'static str,
        SubjectWaterfall,
        &'static [&'static str],
        Option<&'static str>,
        Option<&'static str>,
    );
    use SubjectWaterfall::{Fold, Summary};
    let sondes: &[&str] = &["alt", "vel_v"];
    let plan: [Plan; 7] = [
        (
            "adsb_decode",
            "icao",
            "aircraft",
            Summary,
            &["callsign", "altitude_ft", "ground_speed_kt", "track_deg"],
            Some("track_deg"),
            Some("callsign"),
        ),
        (
            "rs41_decode",
            "id",
            "sondes",
            Fold,
            sondes,
            Some("heading"),
            None,
        ),
        (
            "dfm_decode",
            "id",
            "sondes",
            Fold,
            sondes,
            Some("heading"),
            None,
        ),
        (
            "m20_decode",
            "id",
            "sondes",
            Fold,
            sondes,
            Some("heading"),
            None,
        ),
        (
            "aprs_decode",
            "src",
            "stations",
            Summary,
            &["dst", "info"],
            None,
            None,
        ),
        (
            "acars_decode",
            "reg",
            "aircraft",
            Summary,
            &["flight", "label", "text"],
            None,
            Some("flight"),
        ),
        ("rds_decode", "pi", "stations", Fold, &[], None, Some("ps")),
    ];
    let reg = ModeRegistry::load(&stages(), None);
    for (id, key, name, waterfall, columns, heading, label) in plan {
        let sub = reg
            .all()
            .iter()
            .find(|m| m.row.id.0 == id)
            .unwrap()
            .row
            .subject
            .clone()
            .unwrap_or_else(|| panic!("{id}: no subject"));
        assert_eq!(sub.key, key, "{id}");
        assert_eq!(sub.name, name, "{id}");
        assert_eq!(sub.waterfall, waterfall, "{id}");
        let fields: Vec<&str> = sub.columns.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, columns, "{id}");
        assert_eq!(sub.heading.as_deref(), heading, "{id}");
        assert_eq!(sub.label.as_deref(), label, "{id}");
    }
    let pocsag = reg
        .all()
        .iter()
        .find(|m| m.row.id.0 == "pocsag_decode")
        .unwrap();
    assert_eq!(pocsag.row.subject, None);
}
