//! The mode registry: the embedded rows under `modes/`, the operator directory layered
//! over them, and the `modes_list`/`modes_show` ops.

use std::path::Path;

use zeta_core::graph::NodeId;
use zeta_core::modes::{
    Access, Container, Decoder, DecoderKind, Family, Identity, InputStream, ModeId, ModeRegistry,
    ModeState, Role, SubjectWaterfall, parse, policy::HandlerPolicy, template, validate,
};
use zeta_core::node::{Node, NodeConfig};
use zeta_core::ops::{ModeDetail, ModeInfo, Op, OpError, OpResult};
use zeta_core::port::Tuning;
use zeta_core::stage::{Backend, StageRegistry};
use zeta_core::types::{Hz, StreamKind};

/// The stages the embedded graph rows are written against.
fn stages() -> StageRegistry {
    let mut r = StageRegistry::new();
    zeta_dsp::register_all(&mut r);
    r
}

const EMBEDDED: [&str; 32] = [
    "acars_decode",
    "adsb_decode",
    "ais_decode",
    "am_demod",
    "aprs_decode",
    "ble_decode",
    "dfm_decode",
    "dmr_decode",
    "dvbs_ip_decode",
    "dvbt_decode",
    "flex_decode",
    "fm_demod",
    "gsmr_decode",
    "hfdl_decode",
    "lorawan_decode",
    "lte_decode",
    "m17_decode",
    "m20_decode",
    "p25_decode",
    "pocsag_decode",
    "rds_decode",
    "rs41_decode",
    "ssb_demod",
    "tetra_decode",
    "thread_decode",
    "vdl2_decode",
    "video_decode",
    "wfm_demod",
    "wifi_decode",
    "wisun_decode",
    "zigbee_decode",
    "zwave_decode",
];

fn cfg(dir: &Path, modes_dir: &Path) -> NodeConfig {
    NodeConfig {
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        state_dir: dir.join("state"),
        ring_seconds: 1.0,
        ring_budget_bytes: 64 << 20,
        scan_dwell_ms: 20,
        backends: vec![Backend::Cpu],
        resume: true,
        owns_state_dir: true,
        bank_factory: None,
        modes_dir: Some(modes_dir.to_path_buf()),
        handlers_dir: None,
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
    assert_eq!(embedded.len(), EMBEDDED.len());
    for (file, text) in &embedded {
        let row = parse(text).unwrap_or_else(|e| panic!("{file}: {e}"));
        validate(&row, &stages).unwrap_or_else(|e| panic!("{file}: {e}"));
    }
    let reg = ModeRegistry::load(&stages, None);
    assert_eq!(reg.ids(), EMBEDDED);
    for m in reg.all() {
        if !m.has_decoder() {
            // Only a handler runs it, and none has been probed here.
            assert_eq!(m.state, ModeState::Unavailable, "{}", m.row.id.0);
            assert!(
                m.reason
                    .as_deref()
                    .is_some_and(|r| r.starts_with(NO_DECODER_REASON)),
                "{}: {:?}",
                m.row.id.0,
                m.reason
            );
            continue;
        }
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
    let list = async |family, state| match node
        .apply(Op::ModesList {
            family,
            state,
            role: None,
        })
        .await
    {
        Ok(OpResult::Modes { modes }) => modes,
        other => panic!("expected modes, got {other:?}"),
    };
    let all = list(None, None).await;
    assert_eq!(all.len(), EMBEDDED.len() + 1, "{all:?}");
    let aviation: Vec<String> = list(Some(Family::Aviation), None)
        .await
        .into_iter()
        .map(|m| m.id.0)
        .collect();
    assert_eq!(
        aviation,
        ["acars_decode", "adsb_decode", "hfdl_decode", "vdl2_decode"]
    );
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
    assert_eq!(pagers.len(), 2);
    assert_eq!(pagers[0].id.0, "flex_decode");
    assert_eq!(pagers[1].id.0, "pocsag_decode");
    // The wire form: snake-case `state`, `family` and `decoder_kind`, nullable fields `null`.
    let v = serde_json::to_value(&pagers[1]).unwrap();
    assert_eq!(v["state"], "ready");
    assert_eq!(v["family"], "pagers");
    assert_eq!(v["decoder_kind"], "graph");
    assert_eq!(v["file"], "modes/pocsag_decode.json");
    // A row of several ranges lists them; one of one range keeps the `[lo, hi]` form.
    assert_eq!(
        v["band"]["range_hz"],
        serde_json::json!([
            [138_000_000, 175_000_000],
            [450_000_000, 470_000_000],
            [927_340_000, 936_660_000]
        ])
    );
    assert_eq!(v["band"]["min_hz"], serde_json::Value::Null);
    let back: zeta_core::ops::ModeInfo = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(back, pagers[1]);
    let sondes = list(Some(Family::WeatherScience), None).await;
    let one = serde_json::to_value(&sondes[0]).unwrap();
    assert_eq!(
        one["band"]["range_hz"],
        serde_json::json!([400_050_000, 406_000_000])
    );
    let back: zeta_core::ops::ModeInfo = serde_json::from_value(one).unwrap();
    assert_eq!(back, sondes[0]);
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
        source_positions: Default::default(),
        sets_dir: None,
        burst_policy: Default::default(),
        modes_dir: None,
        handlers_dir: None,
        ..cfg(dir.path(), dir.path())
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    assert_eq!(node.modes().ids(), EMBEDDED);
    for m in node.modes().all() {
        // A row with no decoder of its own waits for a probed handler.
        let want = match m.has_decoder() {
            true => ModeState::Ready,
            false => ModeState::Unavailable,
        };
        assert_eq!(m.state, want, "{}", m.row.id.0);
        assert_eq!(m.file, format!("modes/{}.json", m.row.id.0));
    }
}

/// `reload_modes` reads the operator directory again: a row written after `open` is
/// listed, and a program row keeps the program and state its probe gave it.
#[tokio::test]
async fn reload_modes_picks_up_a_new_row_and_keeps_program_state() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    // As the CLI's probe leaves them: one program found, one not.
    let mut probed = (*node.modes()).clone();
    probed.set_program("adsb_decode", Some("/usr/bin/dump1090".into()), None);
    let missing = "no program found (tried rs41mod)".to_string();
    probed.set_program("rs41_decode", None, Some(missing.clone()));
    node.modes_handle().set(std::sync::Arc::new(probed));
    let mut row: serde_json::Value = serde_json::from_str(&renamed_pocsag("Local pagers")).unwrap();
    row["id"] = "local_pagers".into();
    std::fs::write(modes.join("local_pagers.json"), row.to_string()).unwrap();
    assert!(
        node.modes().get("local_pagers").is_none(),
        "not before the reload"
    );
    let lines = node.reload_modes().unwrap();
    assert!(lines.is_empty(), "{lines:?}");
    let OpResult::Modes { modes: listed } = node
        .apply(Op::ModesList {
            family: None,
            state: None,
            role: None,
        })
        .await
        .unwrap()
    else {
        panic!("modes")
    };
    let new = listed.iter().find(|m| m.id.0 == "local_pagers");
    assert_eq!(new.map(|m| m.state), Some(ModeState::Ready), "{listed:?}");
    let reg = node.modes();
    let adsb = reg.get("adsb_decode").unwrap();
    assert_eq!(adsb.state, ModeState::Ready);
    assert_eq!(adsb.program, Some("/usr/bin/dump1090".into()));
    let rs41 = reg.get("rs41_decode").unwrap();
    assert_eq!(rs41.state, ModeState::Unavailable);
    assert_eq!(rs41.program, None);
    assert_eq!(rs41.reason, Some(missing));
    // A broken file's startup line is returned.
    std::fs::write(modes.join("bad.json"), r#"{"id":"bad_row"}"#).unwrap();
    let lines = node.reload_modes().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("zeta: mode bad_row"), "{lines:?}");
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
    // The embedded `fm_demod` row stays in force; the broken file is listed beside it.
    assert_eq!(reg.get("fm_demod").unwrap().state, ModeState::Ready);
    for id in ["fm_demod", "scan", "identify"] {
        let listed = match id {
            "fm_demod" => "fm_demod@fm_demod.json",
            id => id,
        };
        let m = reg.get(listed).unwrap_or_else(|| panic!("{id} listed"));
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
    template::check_band(&m.row, None, tuning, 2_400_000).unwrap();
    let g = template::expand(
        &m.row,
        &m.handlers[0],
        None,
        tuning,
        48_000,
        Default::default(),
    )
    .unwrap();
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
        serde_json::json!({"mode": "audio_sonde", "bandwidth_hz": 12_500})
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
    let Some(Decoder::Program(block)) = &m.row.decoder else {
        panic!("a program row")
    };
    assert_eq!(block.input.stream, InputStream::Audio);
    assert_eq!(block.input.container, Container::Wav);
    assert_eq!(block.programs, ["rs41mod"]);
    assert_eq!(m.row.position.as_ref().map(|p| p.lat.as_str()), Some("lat"));
    assert_eq!(m.row.time.as_deref(), Some("datetime"));
    assert_eq!(
        serde_json::to_value(&m.row.band).unwrap()["range_hz"],
        serde_json::json!([400_050_000, 406_000_000])
    );
    assert!(m.row.band.centers_hz.is_empty());
    assert_eq!(m.row.identify.priority, 1);
    for (id, program) in [("dfm_decode", "dfm09mod"), ("m20_decode", "m20mod")] {
        let m = reg.get(id).expect(id);
        let Some(Decoder::Program(block)) = &m.row.decoder else {
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
            role: None,
        })
        .await
    else {
        panic!("expected modes");
    };
    let info =
        |id: &str| serde_json::to_value(modes.iter().find(|m| m.id.0 == id).unwrap()).unwrap();
    assert_eq!(info("adsb_decode")["subject"]["key"], "icao");
    assert_eq!(info("adsb_decode")["subject"]["waterfall"], "summary");
    // An aircraft heard by ADS-B is the same subject as the one VDL2 names by its address.
    assert_eq!(
        info("adsb_decode")["subject"]["merge"],
        serde_json::json!(["icao"])
    );
    assert_eq!(info("pocsag_decode")["subject"]["key"], "address");
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

/// The rows a sonde band must never try live where their signals are: FLEX on the
/// catalogue's 151.837-153.363 MHz, AIS on its two channels, APRS around its centres. Out of range
/// (on the sonde band) a row is not `in_range` and never fits.
#[test]
fn flex_ais_and_aprs_are_tried_only_where_they_live() {
    let mut reg = ModeRegistry::load(&stages(), None);
    for id in ["flex_decode", "ais_decode", "aprs_decode"] {
        reg.set_program(id, Some("/stand-in".into()), None);
    }
    let fit = |center_hz: u64, id: &str| {
        let scored = zeta_core::modes::score::score_rows(
            reg.weights(),
            &reg,
            center_hz,
            25_000,
            "fsk",
            &|_| 0.0,
            &|_| 0.0,
        );
        let s = scored.iter().find(|s| s.mode.row.id.0 == id).unwrap();
        (s.in_range, s.fits_band)
    };
    for center in [401_776_000, 403_025_000, 404_026_000] {
        for id in ["flex_decode", "ais_decode", "aprs_decode"] {
            assert_eq!(fit(center, id), (false, false), "{id} at {center} Hz");
        }
    }
    for (center, id) in [
        (152_000_000, "flex_decode"),
        (161_975_000, "ais_decode"),
        (162_025_000, "ais_decode"),
        (144_800_000, "aprs_decode"),
        (144_390_000, "aprs_decode"),
        (145_175_000, "aprs_decode"),
    ] {
        assert_eq!(fit(center, id), (true, true), "{id} at {center} Hz");
    }
    assert_eq!(fit(156_800_000, "ais_decode"), (false, false));
    // APRS lives on its centres (144.390, 144.800 and the AU 145.175 MHz), not all of
    // 2 m: an FM or M17 signal elsewhere on the band gets no centre credit from it.
    for center in [146_500_000, 147_000_000, 144_200_000] {
        assert_eq!(
            fit(center, "aprs_decode"),
            (false, false),
            "aprs at {center} Hz"
        );
    }
    let aprs = reg.get("aprs_decode").unwrap();
    assert!(aprs.row.band.centers_hz.contains(&145_175_000));
}

/// Identify tries a row of several ranges (the catalogue's) in any of them and nowhere
/// between: POCSAG on VHF, UHF and 927.3-936.7 MHz, FLEX on VHF and 927.3-936.6 MHz,
/// APRS on 2 m and 70 cm; the aviation rows only on their international bands (the VHF
/// aeronautical band, 1090 MHz).
#[test]
fn rows_with_several_ranges_are_tried_in_each_and_not_between() {
    let mut reg = ModeRegistry::load(&stages(), None);
    for id in ["flex_decode", "acars_decode", "vdl2_decode", "adsb_decode"] {
        reg.set_program(id, Some("/stand-in".into()), None);
    }
    let fit = |center_hz: u64, bandwidth_hz: u64, id: &str| {
        let scored = zeta_core::modes::score::score_rows(
            reg.weights(),
            &reg,
            center_hz,
            bandwidth_hz,
            "fsk",
            &|_| 0.0,
            &|_| 0.0,
        );
        let s = scored.iter().find(|s| s.mode.row.id.0 == id).unwrap();
        (s.in_range, s.fits_band)
    };
    let cases: [(&str, u64, &[u64], &[u64]); 6] = [
        (
            "pocsag_decode",
            12_500,
            &[
                148_637_500,
                160_000_000,
                466_075_000,
                452_000_000,
                931_000_000,
            ],
            &[137_000_000, 300_000_000, 471_000_000, 937_000_000],
        ),
        (
            "flex_decode",
            25_000,
            &[152_000_000, 929_612_500, 931_000_000],
            &[151_000_000, 500_000_000, 937_000_000],
        ),
        (
            "aprs_decode",
            12_500,
            &[144_800_000, 432_500_000],
            &[146_500_000, 300_000_000, 435_000_000],
        ),
        (
            "acars_decode",
            10_000,
            &[131_550_000, 119_000_000, 136_500_000],
            &[117_000_000, 403_025_000],
        ),
        (
            "vdl2_decode",
            25_000,
            &[136_975_000, 125_000_000],
            &[138_000_000, 403_025_000],
        ),
        (
            "adsb_decode",
            2_400_000,
            &[1_090_000_000],
            &[978_000_000, 403_025_000],
        ),
    ];
    for (id, width, inside, outside) in cases {
        for &center in inside {
            assert_eq!(fit(center, width, id), (true, true), "{id} at {center} Hz");
        }
        for &center in outside {
            assert_eq!(
                fit(center, width, id),
                (false, false),
                "{id} at {center} Hz"
            );
        }
    }
}

#[test]
fn only_the_pager_and_digital_voice_rows_are_restricted() {
    let reg = ModeRegistry::load(&stages(), None);
    let restricted: Vec<&str> = reg
        .all()
        .iter()
        .filter(|m| m.row.access == Some(Access::Restricted))
        .map(|m| m.row.id.0.as_str())
        .collect();
    assert_eq!(
        restricted,
        ["dmr_decode", "flex_decode", "p25_decode", "pocsag_decode"]
    );
}

#[test]
fn a_row_with_a_bad_access_word_is_broken() {
    let dir = tempfile::tempdir().unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&renamed_pocsag("Secret pagers")).unwrap();
    v["id"] = "secret_pagers".into();
    v["access"] = "secret".into();
    std::fs::write(dir.path().join("secret_pagers.json"), v.to_string()).unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    let m = reg.get("secret_pagers").expect("listed");
    assert_eq!(m.state, ModeState::Broken);
    assert!(
        m.reason.as_deref().unwrap().starts_with("access:"),
        "{:?}",
        m.reason
    );
}

#[tokio::test]
async fn modes_list_carries_access() {
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
            role: None,
        })
        .await
    else {
        panic!("expected modes");
    };
    let info =
        |id: &str| serde_json::to_value(modes.iter().find(|m| m.id.0 == id).unwrap()).unwrap();
    assert_eq!(info("pocsag_decode")["access"], "restricted");
    assert_eq!(info("aprs_decode")["access"], serde_json::Value::Null);
}

#[test]
fn a_row_with_a_bad_content_word_is_broken() {
    let dir = tempfile::tempdir().unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&renamed_pocsag("Other")).unwrap();
    v["id"] = "other_content".into();
    v["content"] = "other".into();
    std::fs::write(dir.path().join("other_content.json"), v.to_string()).unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    let m = reg.get("other_content").expect("listed");
    assert_eq!(m.state, ModeState::Broken);
    let reason = m.reason.as_deref().unwrap();
    assert!(
        reason.starts_with("content:") && reason.contains("unknown variant"),
        "{reason}"
    );
}

#[tokio::test]
async fn modes_list_and_show_carry_content() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&renamed_pocsag("Carrier")).unwrap();
    v["id"] = "carrier_decode".into();
    v["content"] = "network_traffic".into();
    std::fs::write(modes.join("carrier_decode.json"), v.to_string()).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    let Ok(OpResult::Modes { modes }) = node
        .apply(Op::ModesList {
            family: None,
            state: None,
            role: None,
        })
        .await
    else {
        panic!("expected modes");
    };
    let infos: Vec<serde_json::Value> = modes
        .iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .collect();
    let carrier = infos.iter().find(|m| m["id"] == "carrier_decode").unwrap();
    assert_eq!(carrier["content"], "network_traffic");
    // Every embedded row with a decoder of its own reports null; one with none, the
    // modes only OnAir decodes, says what it carries.
    for m in infos.iter().filter(|m| m["id"] != "carrier_decode") {
        let want = match m["decoder_kind"].is_null() {
            true => serde_json::json!("network_traffic"),
            false => serde_json::Value::Null,
        };
        assert_eq!(m["content"], want, "{}", m["id"]);
    }
    let Ok(OpResult::Mode { mode }) = node
        .apply(Op::ModesShow {
            id: "carrier_decode".into(),
        })
        .await
    else {
        panic!("expected a mode");
    };
    assert_eq!(
        serde_json::to_value(&mode.row).unwrap()["content"],
        "network_traffic"
    );
}

#[test]
fn the_rows_thin_their_tracks_as_planned() {
    let plan = [
        ("adsb_decode", 100, 500.0),
        ("rs41_decode", 500, 100.0),
        ("dfm_decode", 500, 100.0),
        ("m20_decode", 500, 100.0),
        ("aprs_decode", 100, 50.0),
        ("acars_decode", 100, 1000.0),
        ("rds_decode", 100, 0.0),
        ("vdl2_decode", 100, 1000.0),
        ("hfdl_decode", 100, 1000.0),
        ("ais_decode", 200, 50.0),
        ("m17_decode", 100, 50.0),
    ];
    let reg = ModeRegistry::load(&stages(), None);
    for (id, points, min_m) in plan {
        let row = &reg.all().iter().find(|m| m.row.id.0 == id).unwrap().row;
        let sub = row.subject.as_ref().unwrap();
        assert_eq!((sub.track_points, sub.track_min_m), (points, min_m), "{id}");
    }
}

/// AIS runs one decoder per channel and a vessel alternates between the two, so the row
/// merges on `mmsi`: one row per vessel across both jobs, not one per channel.
#[test]
fn ais_merges_a_vessel_across_its_two_channels() {
    let reg = ModeRegistry::load(&stages(), None);
    let row = &reg
        .all()
        .iter()
        .find(|m| m.row.id.0 == "ais_decode")
        .unwrap()
        .row;
    let sub = row.subject.as_ref().unwrap();
    assert_eq!(sub.key, "mmsi");
    assert_eq!(sub.merge, ["mmsi"]);
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
    let plan: [Plan; 12] = [
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
            "pocsag_decode",
            "address",
            "addresses",
            Summary,
            &["kind", "text"],
            None,
            None,
        ),
        (
            "aprs_decode",
            "src",
            "stations",
            Summary,
            &["dst", "info"],
            Some("course_deg"),
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
        (
            "vdl2_decode",
            "icao",
            "aircraft",
            Summary,
            &["addr_type", "reg", "flight", "label", "text"],
            None,
            Some("flight"),
        ),
        (
            "hfdl_decode",
            "icao",
            "aircraft",
            Summary,
            &["flight", "gs", "pdu", "text"],
            None,
            Some("flight"),
        ),
        (
            "ais_decode",
            "mmsi",
            "vessels",
            Summary,
            &["name", "callsign", "sog_kt", "cog_deg", "destination"],
            Some("cog_deg"),
            Some("name"),
        ),
        (
            "m17_decode",
            "src",
            "stations",
            Fold,
            &["dst", "type", "can", "encryption"],
            None,
            None,
        ),
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
}

/// The aircraft data-link rows as plan 30 pins them: dumpvdl2 at 105 kHz beside ACARS,
/// dumphfdl at a Kiwi's 12 kHz on the listed (dial) frequency in kHz.
#[test]
fn the_aircraft_link_rows_are_pinned() {
    let reg = ModeRegistry::load(&stages(), None);
    let row = |id: &str| {
        let m = reg.get(id).unwrap_or_else(|| panic!("{id} listed"));
        assert_eq!(m.state, ModeState::Ready, "{id}: {:?}", m.reason);
        serde_json::to_value(&m.row).unwrap()
    };
    let vdl2 = row("vdl2_decode");
    assert_eq!(vdl2["label"], "VDL2");
    assert_eq!(vdl2["protocol"], "vdl2");
    assert_eq!(vdl2["family"], "aviation");
    assert_eq!(vdl2["band"]["bandwidth_hz"], 25000);
    assert_eq!(vdl2["band"]["centers_hz"][0], 136_975_000);
    assert_eq!(vdl2["subject"]["key"], "icao");
    assert_eq!(vdl2["subject"]["merge"], serde_json::json!(["reg", "icao"]));
    // A ground station's subject carries the last uplink's `reg`: it keeps a row of its own.
    assert_eq!(
        vdl2["subject"]["merge_skip"],
        serde_json::json!({"addr_type": "Ground station"})
    );
    assert_eq!(vdl2["position"]["alt_unit"], "ft");
    assert_eq!(vdl2["decoder"]["input"]["rate_hz"], 105_000);
    assert_eq!(
        vdl2["decoder"]["install"]["debian"],
        serde_json::Value::Null
    );
    assert_eq!(vdl2["identify"]["dwell_ms"], 15000);
    assert_eq!(
        vdl2["decoder"]["output"]["fields"]["reg"],
        serde_json::json!({"pointers": ["/vdl2/avlc/acars/reg"], "trim": "."})
    );
    assert_eq!(
        vdl2["decoder"]["output"]["fields"]["sig_level"],
        "/vdl2/sig_level"
    );

    let hfdl = row("hfdl_decode");
    assert_eq!(hfdl["label"], "HFDL");
    assert_eq!(hfdl["protocol"], "hfdl");
    assert_eq!(hfdl["family"], "aviation");
    assert_eq!(hfdl["band"]["bandwidth_hz"], 6000);
    assert_eq!(hfdl["band"]["centers_hz"], serde_json::json!([]));
    assert_eq!(
        hfdl["band"]["range_hz"],
        serde_json::json!([2_800_000, 22_100_000])
    );
    assert_eq!(hfdl["decoder"]["input"]["rate_hz"], 12000);
    assert!(
        hfdl["decoder"]["args"]
            .as_array()
            .unwrap()
            .contains(&"$center_khz".into()),
        "{hfdl}"
    );
    assert_eq!(
        hfdl["decoder"]["install"]["debian"],
        serde_json::Value::Null
    );
    assert_eq!(hfdl["identify"]["dwell_ms"], 40000);
    assert_eq!(hfdl["subject"]["merge"], serde_json::json!(["reg", "icao"]));
    assert_eq!(hfdl["subject"]["merge_skip"], serde_json::json!({}));
    // A frame from an aircraft whose logon was missed has neither `icao` nor `gs_id`: the
    // summary drops absent fields instead of printing `null`.
    let summary = hfdl["event"]["summary"].as_str().unwrap();
    assert_eq!(summary, "{icao?} {gs_id?} {gs?} {flight?} {pdu?} {text?}");
    let fill = zeta_core::modes::summary::fill;
    assert_eq!(
        fill(summary, &serde_json::json!({"pdu": "Performance data"})),
        "Performance data"
    );
    assert_eq!(
        fill(
            summary,
            &serde_json::json!({"gs_id": 4, "gs": "Example GS"})
        ),
        "4 Example GS"
    );
    let fields = &hfdl["decoder"]["output"]["fields"];
    assert!(fields.get("aircraft_id").is_none(), "{fields}");
    assert_eq!(
        fields["gs_id"],
        serde_json::json!(["/hfdl/spdu/src/id", "/hfdl/lpdu/hfnpdu/gs/id"])
    );
}

/// Only `aprs_decode` declares how its subject matches the register (design 4.6): its
/// `src`, against the `callsign`, the AX.25 SSID stripped.
#[test]
fn aprs_has_an_identity_and_the_other_builtins_none() {
    let reg = ModeRegistry::load(&stages(), None);
    for m in reg.all() {
        let identity = m.row.subject.as_ref().and_then(|s| s.identity.clone());
        if m.row.id.0 == "aprs_decode" {
            assert_eq!(
                identity,
                Some(Identity {
                    field: "src".into(),
                    register: "callsign".into(),
                    strip: Some("ssid".into()),
                })
            );
        } else {
            assert_eq!(identity, None, "{}", m.row.id.0);
        }
    }
}

/// An identity naming a field the event schema lacks, a register field other than
/// `callsign`, or a strip other than `ssid` loads broken, the reason naming the field.
#[test]
fn a_bad_identity_is_broken_naming_the_field() {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("aprs_decode.json"))
        .expect("embedded aprs_decode");
    let stages = stages();
    let cases = [
        ("field", serde_json::json!("nope"), "subject.identity.field"),
        (
            "register",
            serde_json::json!("mmsi"),
            "subject.identity.register",
        ),
        (
            "strip",
            serde_json::json!("prefix"),
            "subject.identity.strip",
        ),
    ];
    for (key, value, field) in cases {
        let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
        v["subject"]["identity"][key] = value;
        let row = parse(&v.to_string()).unwrap();
        let e = validate(&row, &stages).expect_err(field);
        assert_eq!(e.field, field, "{e}");
    }
    // `strip` may be null or left out.
    let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
    v["subject"]["identity"]["strip"] = serde_json::Value::Null;
    validate(&parse(&v.to_string()).unwrap(), &stages).unwrap();
    v["subject"]["identity"]
        .as_object_mut()
        .unwrap()
        .remove("strip");
    validate(&parse(&v.to_string()).unwrap(), &stages).unwrap();
}

/// The embedded `pocsag_decode` row as an operator row `id` naming one handler program
/// that no host has.
fn pocsag_with_a_missing_handler(id: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(&renamed_pocsag("Voice pagers")).unwrap();
    v["id"] = id.into();
    v["decoder"]["handlers"] = serde_json::json!([{
        "programs": ["zeta-no-such-handler"],
        "install": {"debian": "nothing", "url": "https://example.invalid"}
    }]);
    v
}

/// A graph row may name handlers: it validates whole against zeta-dsp's stages.
#[test]
fn a_graph_row_with_handlers_validates() {
    let row = parse(&pocsag_with_a_missing_handler("pocsag_voc").to_string()).unwrap();
    validate(&row, &stages()).unwrap();
    assert_eq!(row.handler_blocks().len(), 1);
}

/// A graph row its probe left unavailable is refused by `job_start` with its reason,
/// as a program row is.
#[tokio::test]
async fn job_start_refuses_an_unavailable_graph_row() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let mut reg = ModeRegistry::load(&stages(), None);
    let reason = "c2dec: missing (apt install codec2; u) — modes: pocsag_decode";
    reg.set_program("pocsag_decode", None, Some(reason.into()));
    // OnAir's handler is missing too, whatever this host has, so that no other handler
    // takes the start the row's own cannot (the node's probe leaves it so).
    reg.set_handler_program(
        "pocsag_decode",
        "onair",
        None,
        Some("onair-rx: missing".into()),
    );
    let node = Node::open_with_modes(
        cfg(dir.path(), &modes),
        stages(),
        vec![],
        Some(std::sync::Arc::new(reg)),
    )
    .await
    .unwrap();
    let e = node
        .apply(Op::JobStart {
            kind: zeta_core::ops::JobKind::PocsagDecode,
            source: zeta_core::types::SourceId("src".into()),
            center_hz: Hz(153_350_000),
            bandwidth_hz: Hz(12_500),
            deviation_hz: None,
            sideband: None,
            dwell_ms: None,
            average: None,
            bits: None,
            method: None,
            bin_hz: None,
            audio_policy: None,
            dwell_scale: None,
            elastic: false,
            replace: vec![],
            handler: None,
            needs: vec![],
            beside: None,
            choose: None,
        })
        .await
        .unwrap_err();
    let OpError::Other { message } = &e else {
        panic!("{e:?}");
    };
    assert!(message.contains("unavailable"), "{message}");
    assert!(message.contains("c2dec: missing"), "{message}");
}

/// A reload carries a graph row's probed handler state over, as a program row's.
#[tokio::test]
async fn reload_modes_keeps_a_graph_rows_handler_state() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let row = pocsag_with_a_missing_handler("pocsag_voc");
    std::fs::write(modes.join("pocsag_voc.json"), row.to_string()).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    let mut probed = (*node.modes()).clone();
    let reason = "zeta-no-such-handler: missing (u) — modes: pocsag_voc".to_string();
    probed.set_program("pocsag_voc", None, Some(reason.clone()));
    node.modes_handle().set(std::sync::Arc::new(probed));
    node.reload_modes().unwrap();
    let reg = node.modes();
    let m = reg.get("pocsag_voc").unwrap();
    assert_eq!(m.state, ModeState::Unavailable);
    assert_eq!(m.reason, Some(reason));
    assert_eq!(reg.get("pocsag_decode").unwrap().state, ModeState::Ready);
}

#[test]
fn every_embedded_row_emits_events_today() {
    let reg = ModeRegistry::load(&stages(), None);
    for m in reg.all() {
        let want: &[StreamKind] = match m.row.id.0.as_str() {
            // M17's, P25's and DMR's speech is ordinary job audio beside their events.
            "m17_decode" | "p25_decode" | "dmr_decode" => &[StreamKind::Event, StreamKind::Audio],
            // Video is its own stream kind, the fields and the lock events beside it.
            "video_decode" => &[StreamKind::Video, StreamKind::Event],
            // The frame stages publish each frame as a packet beside its event.
            "aprs_decode" => &[StreamKind::Event, StreamKind::Packet],
            // dump1090's Beast output is the ADS-B row's packets.
            "adsb_decode" => &[StreamKind::Event, StreamKind::Packet],
            _ if matches!(m.row.decoder, Some(Decoder::Frame(_))) => {
                &[StreamKind::Event, StreamKind::Packet]
            }
            // A demodulator gives audio, and its squelch's events when it has one.
            "fm_demod" | "am_demod" => &[StreamKind::Audio, StreamKind::Event],
            "wfm_demod" | "ssb_demod" => &[StreamKind::Audio],
            // A row with no decoder of its own gives nothing until a handler is ready.
            _ if m.row.decoder.is_none() => &[],
            _ => &[StreamKind::Event],
        };
        assert_eq!(m.outputs, want, "{}", m.row.id.0);
    }
}

#[test]
fn rows_that_cut_frames_list_packet() {
    let reg = ModeRegistry::load(&stages(), None);
    for m in reg.all() {
        let id = m.row.id.0.as_str();
        let lists = m.outputs.contains(&StreamKind::Packet);
        match &m.row.decoder {
            Some(Decoder::Frame(_)) => {
                assert_eq!(m.outputs, [StreamKind::Event, StreamKind::Packet], "{id}");
            }
            // APRS cuts frames in process; ADS-B's program writes them (Beast).
            _ if id == "aprs_decode" || id == "adsb_decode" => {
                assert_eq!(m.outputs, [StreamKind::Event, StreamKind::Packet], "{id}");
            }
            _ => assert!(!lists, "{id} lists packet"),
        }
    }
    for id in ["pocsag_decode", "rds_decode", "video_decode"] {
        assert!(!reg.get(id).unwrap().outputs.contains(&StreamKind::Packet));
    }
}

/// The M17 row runs its stage in-process and Codec 2 through its one handler, `c2dec`,
/// optional as the P25 and DMR rows' `zeta-mbe` is: without it the streams are reported
/// with no audio.
#[test]
fn the_m17_row_names_c2dec() {
    let reg = ModeRegistry::load(&stages(), None);
    let m = reg.get("m17_decode").expect("m17_decode listed");
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    let blocks = m.row.handler_blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].programs, ["c2dec".to_string()]);
    assert!(blocks[0].optional);
    let p25 = reg.get("p25_decode").unwrap().row.handler_blocks();
    assert!(p25[0].optional);
    let v = serde_json::to_value(&m.row).unwrap();
    assert_eq!(v["family"], "amateur_voice");
    assert_eq!(v["band"]["bandwidth_hz"], 12500);
    assert_eq!(v["label"], "M17");
    assert_eq!(v["identify"]["quick"], serde_json::Value::Null);
    assert_eq!(v["catalogue"], serde_json::json!(["m17-rf-protocol"]));
}

#[tokio::test]
async fn modes_list_reports_outputs() {
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
            role: None,
        })
        .await
    else {
        panic!("expected modes");
    };
    let aprs = modes.iter().find(|m| m.id.0 == "aprs_decode").unwrap();
    let v = serde_json::to_value(aprs).unwrap();
    assert_eq!(v["outputs"], serde_json::json!(["event", "packet"]));
}

#[test]
fn outputs_of_a_graph_row_are_its_terminal_stages_kinds() {
    let mut row: serde_json::Value =
        serde_json::from_str(&renamed_pocsag("Pagers and voice")).unwrap();
    row["id"] = "pocsag_voice".into();
    row["decoder"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "fm", "kind": "nbfm_demod",
            "params": {"deviation_hz": 3000, "audio_rate_hz": 48000, "squelch": false}
        }));
    row["decoder"]["edges"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "from": "channel", "to": "fm", "kind": "iq"
        }));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pv.json"), row.to_string()).unwrap();
    let reg = ModeRegistry::load(&stages(), Some(dir.path()));
    let m = reg.get("pocsag_voice").unwrap();
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    assert_eq!(m.outputs, [StreamKind::Event, StreamKind::Audio]);
}

#[test]
fn video_decode_is_embedded_and_validates() {
    let reg = ModeRegistry::load(&stages(), None);
    let m = reg
        .all()
        .iter()
        .find(|m| m.row.id.0 == "video_decode")
        .expect("video_decode is embedded");
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    assert_eq!(format!("{:?}", m.row.family), "Tv");
    assert_eq!(m.row.test.as_ref().unwrap().bandwidth_hz, Some(8_000_000));
}

#[test]
fn a_row_test_without_bandwidth_hz_still_parses() {
    let reg = ModeRegistry::load(&stages(), None);
    let m = reg
        .all()
        .iter()
        .find(|m| m.row.id.0 == "pocsag_decode")
        .unwrap();
    assert_eq!(m.row.test.as_ref().unwrap().bandwidth_hz, None);
}

/// A handler file `id` serving `adsb_decode` with the row's own `input` and `output`
/// (a known-good block), `edit` applied to the whole file.
fn adsb_handler_file(id: &str, edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("adsb_decode.json"))
        .expect("embedded adsb_decode");
    let row: serde_json::Value = serde_json::from_str(text).unwrap();
    let d = &row["decoder"];
    let mut file = serde_json::json!({
        "id": id,
        "name": "Alternative",
        "programs": ["zeta-alt-decoder"],
        "install": {"url": "https://example.invalid/alt"},
        "modes": {
            "adsb_decode": {
                "args": ["--rate", "$rate_hz", "--port", "$port"],
                "input": d["input"],
                "output": d["output"],
            }
        }
    });
    edit(&mut file);
    file.to_string()
}

/// `entry` (a handler file's mode entry) giving pcap on `$packet_port`, which its args
/// name.
fn with_pcap_port(entry: &mut serde_json::Value) {
    entry["args"]
        .as_array_mut()
        .unwrap()
        .extend(["--pcap-port".into(), "$packet_port".into()]);
    entry["packets"] = serde_json::json!({"via": "tcp", "port": "$packet_port", "parser": "pcap"});
}

/// The registry over the embedded rows with `files` (name, contents) in a handlers
/// directory.
fn with_handler_files(files: &[(&str, &str)]) -> ModeRegistry {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    ModeRegistry::load_with(&stages(), None, Some(dir.path()))
}

/// The first handler file of `reg` read from the handlers directory (not embedded, as
/// `handlers/onair.json` is).
fn operator_file(reg: &ModeRegistry) -> &zeta_core::modes::LoadedHandlerFile {
    reg.handler_files()
        .iter()
        .find(|f| !f.file.starts_with("handlers/"))
        .expect("an operator handler file")
}

/// The ids of `mode`'s handlers, the row's own first.
fn handler_ids(reg: &ModeRegistry, mode: &str) -> Vec<String> {
    reg.get(mode)
        .unwrap()
        .handlers
        .iter()
        .map(|h| h.id.clone())
        .collect()
}

#[test]
fn a_handler_file_adds_a_handler_to_each_mode_it_lists() {
    let reg = with_handler_files(&[("alt.json", &adsb_handler_file("alt", |_| {}))]);
    // The row's own first, then the files' by id: alt beside the embedded onair.
    assert_eq!(
        handler_ids(&reg, "adsb_decode"),
        ["dump1090", "alt", "onair"]
    );
    let own = reg.handler("adsb_decode", "dump1090").unwrap();
    assert!(!own.in_process);
    assert_eq!(own.file, "modes/adsb_decode.json");
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert!(!alt.in_process);
    assert_eq!(alt.name, "Alternative");
    assert_eq!(alt.outputs, [StreamKind::Event]);
    assert_eq!(alt.state, ModeState::Ready, "{:?}", alt.reason);
    assert!(alt.file.ends_with("alt.json"), "{}", alt.file);
    let block = reg.program_block("adsb_decode", "alt").unwrap();
    assert_eq!(block.programs, ["zeta-alt-decoder"]);
    assert_eq!(block.install.url, "https://example.invalid/alt");
    assert_eq!(block.args, ["--rate", "$rate_hz", "--port", "$port"]);
    assert_eq!(
        reg.program_block("adsb_decode", "dump1090")
            .unwrap()
            .programs[0],
        "dump1090"
    );

    let with_packets = adsb_handler_file("alt", |f| with_pcap_port(&mut f["modes"]["adsb_decode"]));
    let reg = with_handler_files(&[("alt.json", &with_packets)]);
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.outputs, [StreamKind::Event, StreamKind::Packet]);
    assert!(
        reg.program_block("adsb_decode", "alt")
            .unwrap()
            .packets
            .is_some()
    );

    // A mode no operator file lists has its own handler and the embedded ones; a graph row's own is `builtin`.
    assert_eq!(handler_ids(&reg, "aprs_decode"), ["builtin", "onair"]);
    let builtin = reg.handler("aprs_decode", "builtin").unwrap();
    assert!(builtin.in_process);
    assert_eq!(builtin.state, ModeState::Ready);
    assert_eq!(builtin.outputs, reg.get("aprs_decode").unwrap().outputs);
    assert_eq!(reg.program_block("aprs_decode", "builtin"), None);
    // A graph row with helper programs is `builtin` and in process too, OnAir's after it.
    assert_eq!(handler_ids(&reg, "dmr_decode"), ["builtin", "onair"]);
    assert!(reg.handler("dmr_decode", "builtin").unwrap().in_process);
}

/// `handlers/onair.json` serves its modes, the bus modes and the polled ones: each is a
/// handler of its row, last in its list, whose program is `onair-rx`, at the receiver's
/// input rate; the modes with packets give events and packets, the others events only;
/// none is broken.
#[test]
fn the_embedded_onair_handler_serves_its_modes() {
    let mut reg = ModeRegistry::load(&stages(), None);
    zeta_core::modes::handlers::probe_handler_files(&mut reg);
    let file = reg
        .handler_files()
        .iter()
        .find(|f| f.id == "onair")
        .expect("the embedded onair file");
    assert_eq!(file.broken_entries(), Vec::<String>::new());
    let events = [StreamKind::Event];
    let both = [StreamKind::Event, StreamKind::Packet];
    // (mode id, the streams its OnAir entry gives, its input rate)
    let modes: [(&str, &[StreamKind], u64); 26] = [
        ("pocsag_decode", &events, 1_000_000),
        ("flex_decode", &events, 1_000_000),
        ("acars_decode", &events, 1_000_000),
        ("aprs_decode", &both, 240_000),
        ("ais_decode", &both, 250_000),
        ("vdl2_decode", &events, 250_000),
        ("hfdl_decode", &events, 24_000),
        ("rs41_decode", &events, 2_000_000),
        ("dfm_decode", &events, 2_000_000),
        ("m20_decode", &events, 2_000_000),
        ("rds_decode", &events, 500_000),
        ("dmr_decode", &events, 48_000),
        ("p25_decode", &events, 250_000),
        ("m17_decode", &events, 250_000),
        ("dvbt_decode", &events, 10_000_000),
        // The modes only OnAir decodes: events and each mode's packets.
        ("ble_decode", &both, 4_000_000),
        ("lorawan_decode", &both, 2_000_000),
        ("zigbee_decode", &both, 4_000_000),
        ("thread_decode", &both, 4_000_000),
        ("zwave_decode", &both, 2_000_000),
        ("wifi_decode", &both, 20_000_000),
        ("wisun_decode", &both, 2_000_000),
        ("tetra_decode", &both, 2_000_000),
        ("gsmr_decode", &both, 2_000_000),
        ("lte_decode", &both, 20_000_000),
        ("dvbs_ip_decode", &both, 10_000_000),
    ];
    for (id, outputs, rate) in modes {
        assert_eq!(
            handler_ids(&reg, id).last(),
            Some(&"onair".to_string()),
            "{id}"
        );
        let h = reg.handler(id, "onair").unwrap();
        assert_eq!(h.outputs, outputs, "{id}");
        assert!(!h.in_process, "{id}");
        // Only ever the program, missing or older than the file's 0.4.0 (or its version
        // unread), keeps it from ready.
        if h.state != ModeState::Ready {
            let reason = h.reason.as_deref().unwrap_or_default();
            assert!(onair_program_reason(reason), "{id}: {reason}");
        }
        let block = reg.program_block(id, "onair").unwrap();
        assert_eq!(block.programs, ["onair-rx"], "{id}");
        assert_eq!(block.input.rate_hz, rate, "{id}");
    }
    let aprs = reg.program_block("aprs_decode", "onair").unwrap();
    let packets = aprs.packets.as_ref().expect("onair's packets");
    assert_eq!(
        packets.link_type_numbers().into_iter().collect::<Vec<_>>(),
        [(3, zeta_core::stream::LinkType::Ax25)]
    );
    let ais = reg.program_block("ais_decode", "onair").unwrap();
    let packets = ais.packets.as_ref().expect("onair's AIS packets");
    assert_eq!(
        packets.link_type_numbers().into_iter().collect::<Vec<_>>(),
        [(101, zeta_core::stream::LinkType::RawIp)]
    );
    // OnAir publishes an ACARS message only when its CRC fitted: crc_ok is a constant.
    assert_eq!(
        reg.handler_consts("acars_decode", "onair"),
        std::collections::BTreeMap::from([("crc_ok".to_string(), serde_json::json!(true))])
    );
    // The voice modes' audio is not tapped: DMR and P25 say so, M17's row has no such field.
    for id in ["dmr_decode", "p25_decode"] {
        assert_eq!(
            reg.handler_consts(id, "onair"),
            std::collections::BTreeMap::from([("audio".to_string(), serde_json::json!(false))]),
            "{id}"
        );
    }
    assert!(reg.handler_consts("m17_decode", "onair").is_empty());
    // Analog video missed the listing bar (no one rate locks a full FPV swing and keeps
    // real time on an empty channel): OnAir is not one of its handlers.
    assert_eq!(handler_ids(&reg, "video_decode"), ["builtin"]);
    assert!(reg.handler("video_decode", "onair").is_none());
    assert_eq!(reg.program_block("video_decode", "onair"), None);
}

/// Whether `reason` is one the probe of `onair-rx` gives: missing, older than the
/// embedded file's least version, or its version not read.
fn onair_program_reason(reason: &str) -> bool {
    reason.starts_with("onair-rx: missing")
        || reason.contains(" is older than 0.4.0 (")
        || reason.contains(": its version could not be read (")
}

/// OnAir writes Z-Wave MPDUs as link type 148 (USER1), which the node names for Mode S:
/// the entry remaps it to USER2 (149), so a capture is not dissected as Mode S.
#[test]
fn the_onair_zwave_entry_writes_user2_not_mode_s() {
    let mut reg = ModeRegistry::load(&stages(), None);
    zeta_core::modes::handlers::probe_handler_files(&mut reg);
    let file = reg
        .handler_files()
        .iter()
        .find(|f| f.id == "onair")
        .unwrap();
    assert_eq!(file.broken_entries(), Vec::<String>::new());
    let block = reg.program_block("zwave_decode", "onair").unwrap();
    let packets = block.packets.as_ref().expect("onair's Z-Wave packets");
    assert_eq!(
        packets.link_type_numbers().into_iter().collect::<Vec<_>>(),
        [(148, zeta_core::stream::LinkType::Other(149))]
    );
}

/// OnAir's `dvbt_decode` entry reads the wrapper's status lines (`fixtures/
/// onair-dvbt-status.jsonl`: two lines `onair-rx` wrote on its own generated multiplex,
/// one before lock and one locked with the service list) as the node's `json_lines` stage
/// does: `output.fields` onto the row's fields, `extra` beside them. Each event passes the
/// row's schema; the locked one carries the signalling and `services` names `ZETA TEST`.
#[test]
fn the_onair_dvbt_entry_maps_a_status_line() {
    use zeta_core::modes::FieldPath;
    let mut reg = ModeRegistry::load(&stages(), None);
    zeta_core::modes::handlers::probe_handler_files(&mut reg);
    let block = reg.program_block("dvbt_decode", "onair").unwrap();
    let fields = block.output.fields.expect("the entry's field map");
    let extra = reg.handler_extra("dvbt_decode", "onair");
    assert_eq!(extra.keys().collect::<Vec<_>>(), ["services"]);
    assert!(reg.handler_consts("dvbt_decode", "onair").is_empty());
    // As zeta-ext's `apply_fields`: the first pointer that resolves to a non-null value.
    let map = |paths: &std::collections::BTreeMap<String, FieldPath>, line: &serde_json::Value| {
        paths
            .iter()
            .filter_map(|(k, p)| {
                let v = p
                    .pointers()
                    .iter()
                    .filter_map(|p| line.pointer(p))
                    .find(|v| !v.is_null())?;
                Some((k.clone(), v.clone()))
            })
            .collect::<serde_json::Map<_, _>>()
    };
    let schema = &reg.get("dvbt_decode").unwrap().row.event.schema;
    let validator = jsonschema::validator_for(schema).unwrap();
    let text = include_str!("fixtures/onair-dvbt-status.jsonl");
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|l| {
            let line: serde_json::Value = serde_json::from_str(l).unwrap();
            let mut ev = map(&fields, &line);
            ev.extend(map(&extra, &line));
            serde_json::Value::Object(ev)
        })
        .collect();
    assert_eq!(events.len(), 2);
    for ev in &events {
        assert!(validator.is_valid(ev), "{ev}");
        assert!(ev["locked"].is_boolean(), "{ev}");
    }
    let (searching, locked) = (&events[0], &events[1]);
    assert_eq!(searching["locked"], false);
    assert_eq!(searching["lock"], "searching");
    assert!(searching.get("bandwidth_hz").is_none(), "{searching}");
    assert_eq!(locked["locked"], true);
    assert_eq!(locked["lock"], "locked");
    assert_eq!(locked["bandwidth_hz"], 8_000_000);
    assert_eq!(locked["mode"], "8K");
    assert_eq!(locked["guard"], "1/8");
    assert_eq!(locked["constellation"], "64-QAM");
    assert_eq!(locked["code_rate"], "3/4");
    assert!(locked["snr_db"].is_number(), "{locked}");
    let names: Vec<&str> = locked["services"]
        .as_array()
        .expect("services")
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(names.contains(&"ZETA TEST"), "{locked}");
}

/// `handlers/onair.json` ships in the node: OnAir is `adsb_decode`'s second handler,
/// giving events and packets, ready when `onair-rx` is found and otherwise unavailable
/// with the probe's line; never broken. The row's own handler gives packets too.
#[test]
fn the_embedded_onair_handler_serves_adsb() {
    let mut reg = ModeRegistry::load(&stages(), None);
    let file = reg
        .handler_files()
        .iter()
        .find(|f| f.id == "onair")
        .expect("the embedded onair file");
    assert_eq!(file.file, "handlers/onair.json");
    assert_eq!(file.state, ModeState::Ready, "{:?}", file.reason);
    assert_eq!(file.broken_entries(), Vec::<String>::new());
    // The version it is written for is its least.
    assert_eq!(
        file.handler.as_ref().unwrap().version,
        Some(zeta_core::modes::VersionProbe {
            args: vec!["--version".into()],
            pattern: "^onair-rx (\\d+(?:\\.\\d+)*)".into(),
            min: "0.4.0".into(),
        })
    );
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "onair"]);
    let both = [StreamKind::Event, StreamKind::Packet];
    assert_eq!(
        reg.handler("adsb_decode", "dump1090").unwrap().outputs,
        both
    );
    zeta_core::modes::handlers::probe_handler_files(&mut reg);
    let onair = reg.handler("adsb_decode", "onair").unwrap();
    assert_eq!(onair.name, "OnAir");
    assert!(!onair.in_process);
    assert_eq!(onair.outputs, both);
    let installed = zeta_core::modes::handlers::find_handler(&["onair-rx".into()]);
    match installed {
        // Found, it is ready when it reports 0.4.0 or over.
        Some(path) if onair.state == ModeState::Ready => {
            assert_eq!(onair.program.as_ref(), Some(&path));
            assert!(onair.version.is_some(), "{onair:?}");
        }
        Some(_) => {
            let reason = onair.reason.as_deref().unwrap_or_default();
            assert!(onair_program_reason(reason), "{reason}");
        }
        None => {
            assert_eq!(onair.state, ModeState::Unavailable);
            let reason = onair.reason.as_deref().unwrap_or_default();
            assert!(reason.starts_with("onair-rx: missing"), "{reason}");
        }
    }
    let block = reg.program_block("adsb_decode", "onair").unwrap();
    assert_eq!(block.programs, ["onair-rx"]);
    assert_eq!(block.input.rate_hz, 2_400_000);
    let packets = block.packets.as_ref().expect("onair's packets");
    assert_eq!(
        packets.link_type_numbers().into_iter().collect::<Vec<_>>(),
        [(147, zeta_core::stream::LinkType::ModeS)]
    );
    let extra: Vec<String> = reg
        .handler_extra("adsb_decode", "onair")
        .into_keys()
        .collect();
    assert_eq!(extra, ["df", "level_dbfs", "tc"]);
}

#[test]
fn an_entry_for_an_unknown_mode_breaks_only_that_entry() {
    let text = adsb_handler_file("alt", |f| {
        f["modes"]["no_such_mode"] = f["modes"]["adsb_decode"].clone();
    });
    let reg = with_handler_files(&[("alt.json", &text)]);
    let file = operator_file(&reg);
    assert_eq!(file.state, ModeState::Ready, "{:?}", file.reason);
    assert_eq!(file.broken_entries(), ["no_such_mode: no mode of that id"]);
    assert_eq!(
        handler_ids(&reg, "adsb_decode"),
        ["dump1090", "alt", "onair"]
    );
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{:?}", alt.reason);
}

#[test]
fn an_entry_missing_a_required_event_field_is_broken() {
    let text = adsb_handler_file("alt", |f| {
        f["modes"]["adsb_decode"]["output"]["fields"] =
            serde_json::json!({"callsign": "/callsign"});
    });
    let reg = with_handler_files(&[("alt.json", &text)]);
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Broken);
    assert_eq!(
        alt.reason.as_deref(),
        Some("output.fields: no mapping for icao (the row's subject key)")
    );
    assert_eq!(reg.program_block("adsb_decode", "alt"), None);
    // `fields: null` (the whole parsed object is the event) passes.
    let reg = with_handler_files(&[("alt.json", &adsb_handler_file("alt", |_| {}))]);
    assert_eq!(
        reg.handler("adsb_decode", "alt").unwrap().state,
        ModeState::Ready
    );
}

#[test]
fn an_extra_field_may_not_shadow_a_row_field() {
    let text = adsb_handler_file("alt", |f| {
        f["modes"]["adsb_decode"]["extra"] = serde_json::json!({"callsign": "/x"});
    });
    let reg = with_handler_files(&[("alt.json", &text)]);
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Broken);
    assert_eq!(
        alt.reason.as_deref(),
        Some("extra.callsign: the row's event schema already has it")
    );
    let text = adsb_handler_file("alt", |f| {
        f["modes"]["adsb_decode"]["extra"] = serde_json::json!({"df": "/df"});
    });
    let reg = with_handler_files(&[("alt.json", &text)]);
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{:?}", alt.reason);
}

#[test]
fn an_id_of_builtin_or_of_the_rows_own_program_is_refused() {
    let reg = with_handler_files(&[("b.json", &adsb_handler_file("builtin", |_| {}))]);
    let file = operator_file(&reg);
    assert_eq!(file.state, ModeState::Broken);
    assert!(
        file.reason.as_deref().unwrap().starts_with("id: "),
        "{file:?}"
    );
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "onair"]);

    let reg = with_handler_files(&[("d.json", &adsb_handler_file("dump1090", |_| {}))]);
    let file = operator_file(&reg);
    assert_eq!(file.state, ModeState::Ready, "{file:?}");
    assert_eq!(
        file.broken_entries(),
        ["adsb_decode: id dump1090 is the row's own handler"]
    );
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "onair"]);
    // The row's own handler is untouched.
    assert_eq!(
        reg.program_block("adsb_decode", "dump1090")
            .unwrap()
            .programs[0],
        "dump1090"
    );
}

#[test]
fn an_operator_file_replaces_the_embedded_one_of_its_id() {
    let first = adsb_handler_file("alt", |_| {});
    let second = adsb_handler_file("alt", |f| f["name"] = "Replacement".into());
    let reg = ModeRegistry::load_handler_texts(
        &stages(),
        &[("handlers/alt.json", &first), ("/op/alt.json", &second)],
    );
    assert_eq!(reg.handler_files().len(), 1, "{:?}", reg.handler_files());
    assert_eq!(reg.handler_files()[0].file, "/op/alt.json");
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "alt"]);
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.name, "Replacement");
    assert_eq!(alt.file, "/op/alt.json");
    // A broken operator file leaves the one in force.
    let reg = ModeRegistry::load_handler_texts(
        &stages(),
        &[("handlers/alt.json", &first), ("/op/alt.json", "{")],
    );
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "alt"]);
    assert_eq!(
        reg.handler("adsb_decode", "alt").unwrap().name,
        "Alternative"
    );
}

#[test]
fn a_broken_handler_file_never_fails_the_load() {
    let reg = with_handler_files(&[("bad.json", "{")]);
    assert_eq!(reg.ids(), EMBEDDED);
    // The rows with no decoder of their own are not ready before a handler is probed.
    let own = reg.all().iter().filter(|m| m.has_decoder()).count();
    assert_eq!(reg.ready().count(), own);
    let file = operator_file(&reg);
    assert_eq!(file.state, ModeState::Broken);
    assert!(file.file.ends_with("bad.json"), "{file:?}");
    assert!(file.reason.as_deref().unwrap().contains("EOF"), "{file:?}");
    let lines = reg.startup_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("zeta: handler bad ("), "{lines:?}");
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "onair"]);
}

#[test]
fn a_malformed_entry_breaks_only_itself() {
    let text = adsb_handler_file("alt", |f| {
        let mut bad = f["modes"]["adsb_decode"].clone();
        bad.as_object_mut().unwrap().remove("input");
        f["modes"]["ais_decode"] = bad;
    });
    let reg = with_handler_files(&[("alt.json", &text)]);
    let file = operator_file(&reg);
    assert_eq!(file.state, ModeState::Ready, "{file:?}");
    let broken = file.broken_entries();
    assert_eq!(broken.len(), 1, "{broken:?}");
    assert!(
        broken[0].starts_with("ais_decode: input: missing field `input`"),
        "{broken:?}"
    );
    assert_eq!(
        handler_ids(&reg, "adsb_decode"),
        ["dump1090", "alt", "onair"]
    );
    assert_eq!(
        reg.handler("adsb_decode", "alt").unwrap().state,
        ModeState::Ready
    );
    assert_eq!(handler_ids(&reg, "ais_decode"), ["builtin", "onair"]);
    let lines = reg.startup_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("zeta: handler alt (") && lines[0].contains("): ais_decode: input: "),
        "{lines:?}"
    );
}

/// A handler file written after the node opened is looked up on the reload that reads
/// it: a program no host has makes the handler unavailable, one that exists ready with
/// its path.
#[tokio::test]
async fn a_handler_file_added_before_a_reload_is_probed() {
    let dir = tempfile::tempdir().unwrap();
    let (modes, handlers) = (dir.path().join("modes"), dir.path().join("handlers"));
    std::fs::create_dir_all(&handlers).unwrap();
    let cfg = NodeConfig {
        handlers_dir: Some(handlers.clone()),
        ..cfg(dir.path(), &modes)
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    assert_eq!(
        handler_ids(&node.modes(), "adsb_decode"),
        ["dump1090", "onair"]
    );

    let missing = adsb_handler_file("alt", |f| {
        f["programs"] = serde_json::json!(["zeta-no-such-handler"]);
    });
    std::fs::write(handlers.join("alt.json"), missing).unwrap();
    node.reload_modes().unwrap();
    let reg = node.modes();
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Unavailable, "{alt:?}");
    assert_eq!(
        alt.reason.as_deref(),
        Some("zeta-no-such-handler: missing (https://example.invalid/alt) — modes: adsb_decode")
    );
    assert_eq!(alt.program, None);

    let program = dir.path().join("alt-rx");
    std::fs::write(&program, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let found = adsb_handler_file("alt", |f| {
        f["programs"] = serde_json::json!([program.display().to_string()]);
    });
    std::fs::write(handlers.join("alt.json"), found).unwrap();
    node.reload_modes().unwrap();
    let reg = node.modes();
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{alt:?}");
    assert_eq!(alt.program.as_deref(), Some(program.as_path()));
}

/// A node config on `dir` whose handlers directory holds `alt.json`: an `adsb_decode`
/// handler giving packets beside its events and adding the event field `df`.
fn alt_cfg(dir: &Path) -> NodeConfig {
    let handlers = dir.join("handlers");
    std::fs::create_dir_all(&handlers).unwrap();
    let alt = adsb_handler_file("alt", |f| {
        let e = &mut f["modes"]["adsb_decode"];
        with_pcap_port(e);
        e["extra"] = serde_json::json!({"df": "/df"});
    });
    std::fs::write(handlers.join("alt.json"), alt).unwrap();
    NodeConfig {
        handlers_dir: Some(handlers),
        ..cfg(dir, &events_only_adsb(dir))
    }
}

/// `<dir>/modes` holding `adsb_decode` as the embedded row without its `packets` block
/// (dump1090 giving events only), so the row's own outputs differ from alt's.
fn events_only_adsb(dir: &Path) -> std::path::PathBuf {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("adsb_decode.json"))
        .expect("embedded adsb_decode");
    let mut row: serde_json::Value = serde_json::from_str(text).unwrap();
    let d = row["decoder"].as_object_mut().unwrap();
    d.remove("packets");
    for a in d["args"].as_array_mut().unwrap() {
        if a == "$packet_port" {
            *a = "0".into();
        }
    }
    let modes = dir.join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join("adsb_decode.json"), row.to_string()).unwrap();
    modes
}

/// Set what probing found for `adsb_decode`: the row's own program (`None`: dump1090
/// missing) and alt's (`None`: its program missing). The embedded onair's is missing:
/// the node's own probe at open says so on a host without `onair-rx`, and this keeps
/// the union and the pick below the same on a host that has it.
fn probe_adsb(node: &Node, own: Option<&str>, alt: Option<&str>) {
    let mut reg = (*node.modes()).clone();
    let (mode, missing) = ("adsb_decode", |p: &str| format!("{p}: missing"));
    reg.set_handler_program(mode, "onair", None, Some(missing("onair-rx")));
    match own {
        Some(p) => reg.set_program(mode, Some(p.into()), None),
        None => reg.set_program(mode, None, Some(missing("dump1090"))),
    }
    match alt {
        Some(p) => reg.set_handler_program(mode, "alt", Some(p.into()), None),
        None => reg.set_handler_program(mode, "alt", None, Some(missing("zeta-alt-decoder"))),
    }
    node.modes_handle().set(std::sync::Arc::new(reg));
}

async fn policy_set(node: &Node, mode: Option<&str>, order: &[&str]) -> Result<OpResult, OpError> {
    node.apply(Op::HandlerPolicySet {
        mode: mode.map(|m| ModeId(m.into())),
        order: order.iter().map(|s| s.to_string()).collect(),
    })
    .await
}

async fn policy_get(node: &Node) -> HandlerPolicy {
    match node.apply(Op::HandlerPolicyGet {}).await {
        Ok(OpResult::HandlerPolicy(p)) => p,
        other => panic!("expected the policy, got {other:?}"),
    }
}

/// Every mode as `modes_list` reports it.
async fn listed(node: &Node) -> Vec<ModeInfo> {
    match node
        .apply(Op::ModesList {
            family: None,
            state: None,
            role: None,
        })
        .await
    {
        Ok(OpResult::Modes { modes }) => modes,
        other => panic!("expected modes, got {other:?}"),
    }
}

async fn listed_one(node: &Node, id: &str) -> ModeInfo {
    let all = listed(node).await;
    all.into_iter().find(|m| m.id.0 == id).expect("listed")
}

async fn shown(node: &Node, id: &str) -> ModeDetail {
    match node.apply(Op::ModesShow { id: id.into() }).await {
        Ok(OpResult::Mode { mode }) => *mode,
        other => panic!("expected the mode, got {other:?}"),
    }
}

/// An older node's `modes_list` and `modes_show` answers have no `handlers` or `handler`:
/// they read as no handlers and none picked.
#[tokio::test]
async fn an_older_nodes_mode_answers_without_handlers_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    let mut info = serde_json::to_value(listed_one(&node, "aprs_decode").await).unwrap();
    let mut detail = serde_json::to_value(shown(&node, "aprs_decode").await).unwrap();
    for v in [&mut info, &mut detail] {
        let o = v.as_object_mut().unwrap();
        assert!(o.remove("handlers").is_some() && o.remove("handler").is_some());
    }
    let info: ModeInfo = serde_json::from_value(info).unwrap();
    assert_eq!((info.handlers.len(), info.handler), (0, None));
    let detail: ModeDetail = serde_json::from_value(detail).unwrap();
    assert_eq!((detail.handlers.len(), detail.handler), (0, None));
}

#[tokio::test]
async fn handler_policy_set_persists_and_get_returns_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    assert_eq!(policy_get(&node).await, HandlerPolicy::default());
    let mode_only = HandlerPolicy {
        modes: [("adsb_decode".to_string(), vec!["alt".to_string()])].into(),
        all: vec![],
    };
    let r = policy_set(&node, Some("adsb_decode"), &["alt"]).await;
    assert_eq!(r, Ok(OpResult::HandlerPolicy(mode_only.clone())));
    // The file is written as decided: `{"modes": {...}, "all": [...]}`.
    let file = dir.path().join("state/handler-policy.json");
    let on_disk: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        on_disk,
        serde_json::json!({"modes": {"adsb_decode": ["alt"]}, "all": []})
    );
    drop(node);
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    assert_eq!(policy_get(&node).await, mode_only);

    // The node-wide ranking sits beside the mode's; an empty order clears each.
    let both = HandlerPolicy {
        all: vec!["alt".into(), "dump1090".into()],
        ..mode_only.clone()
    };
    let r = policy_set(&node, None, &["alt", "dump1090"]).await;
    assert_eq!(r, Ok(OpResult::HandlerPolicy(both.clone())));
    let r = policy_set(&node, Some("adsb_decode"), &[]).await;
    let all_only = HandlerPolicy {
        modes: Default::default(),
        all: both.all.clone(),
    };
    assert_eq!(r, Ok(OpResult::HandlerPolicy(all_only.clone())));
    drop(node);
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    assert_eq!(policy_get(&node).await, all_only);
    let r = policy_set(&node, None, &[]).await;
    assert_eq!(r, Ok(OpResult::HandlerPolicy(HandlerPolicy::default())));
    drop(node);

    // A malformed file reads as the empty policy, with one line saying so.
    std::fs::write(&file, "garbage").unwrap();
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    assert_eq!(policy_get(&node).await, HandlerPolicy::default());
    let lines: Vec<_> = node
        .load_errors()
        .iter()
        .filter(|l| l.contains("handler-policy.json"))
        .collect();
    assert_eq!(lines.len(), 1, "{:?}", node.load_errors());
    assert!(
        lines[0].starts_with("unreadable handler-policy.json: "),
        "{lines:?}"
    );
}

#[tokio::test]
async fn the_handler_policy_refuses_a_demodulator_row() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    for id in ["fm_demod", "wfm_demod", "am_demod", "ssb_demod"] {
        let r = policy_set(&node, Some(id), &[]).await;
        assert_eq!(
            r,
            Err(OpError::Other {
                message: format!(
                    "{id} is a demodulator row: handler_policy_set takes decoder rows"
                ),
            })
        );
    }
    // Nothing was written.
    assert_eq!(policy_get(&node).await, HandlerPolicy::default());
}

#[tokio::test]
async fn handler_policy_set_refuses_an_id_no_mode_has() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    let unknown = |field: &str, value: &str, known: Vec<String>| {
        Err(OpError::Unknown {
            field: field.into(),
            value: value.into(),
            known,
        })
    };
    let ids = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // A named mode: its own handlers' ids.
    let r = policy_set(&node, Some("adsb_decode"), &["alt", "nope"]).await;
    assert_eq!(
        r,
        unknown("order", "nope", ids(&["dump1090", "alt", "onair"]))
    );
    // Another mode's handler is not this one's.
    let r = policy_set(&node, Some("aprs_decode"), &["alt"]).await;
    assert_eq!(r, unknown("order", "alt", ids(&["builtin", "onair"])));
    // The node-wide ranking: every handler id of every mode, once each, sorted.
    let mut every: Vec<String> = node
        .modes()
        .all()
        .iter()
        .flat_map(|m| m.handlers.iter().map(|h| h.id.clone()))
        .filter(|id| id != "none")
        .collect();
    every.sort();
    every.dedup();
    for id in ["alt", "builtin", "dump1090"] {
        assert!(every.iter().any(|e| e == id), "{every:?}");
    }
    let r = policy_set(&node, None, &["nope"]).await;
    assert_eq!(r, unknown("order", "nope", every));
    // A mode the node lacks is unknown on `mode`.
    let r = policy_set(&node, Some("nope_decode"), &["alt"]).await;
    assert_eq!(r, unknown("mode", "nope_decode", node.modes().ids()));
    // Nothing was written.
    assert_eq!(policy_get(&node).await, HandlerPolicy::default());
    assert!(!dir.path().join("state/handler-policy.json").exists());
}

#[tokio::test]
async fn modes_list_gives_each_modes_handlers_and_the_policys_pick() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(alt_cfg(dir.path()), stages(), vec![])
        .await
        .unwrap();
    let both = [StreamKind::Event, StreamKind::Packet];

    // The row's program missing, alt found: the mode is ready through alt.
    probe_adsb(&node, None, Some("/opt/alt-rx"));
    let m = listed_one(&node, "adsb_decode").await;
    assert_eq!(m.state, ModeState::Ready);
    assert_eq!(m.handler.as_deref(), Some("alt"));
    assert_eq!(m.program.as_deref(), Some("/opt/alt-rx"));
    assert_eq!(m.reason, None);
    assert_eq!(m.outputs, both);
    let ids: Vec<&str> = m.handlers.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, ["dump1090", "alt", "onair"]);
    let own = &m.handlers[0];
    assert_eq!(own.state, ModeState::Unavailable);
    assert_eq!(own.reason.as_deref(), Some("dump1090: missing"));
    assert_eq!(own.program, None);
    assert!(!own.in_process);
    assert_eq!(own.extra, Vec::<String>::new());
    let alt = &m.handlers[1];
    assert_eq!(alt.name, "Alternative");
    assert_eq!(alt.state, ModeState::Ready);
    assert_eq!(alt.program.as_deref(), Some("/opt/alt-rx"));
    assert_eq!(alt.outputs, both);
    assert_eq!(alt.extra, ["df"]);
    assert!(alt.file.ends_with("alt.json"), "{}", alt.file);
    // The `state` filter reads the reported state.
    let ready = node
        .apply(Op::ModesList {
            family: Some(Family::Aviation),
            state: Some(ModeState::Ready),
            role: None,
        })
        .await;
    let Ok(OpResult::Modes { modes: ready }) = ready else {
        panic!("{ready:?}")
    };
    assert!(ready.iter().any(|r| r.id.0 == "adsb_decode"), "{ready:?}");
    // `modes_show` says the same.
    let s = shown(&node, "adsb_decode").await;
    assert_eq!(
        (&s.state, &s.handler, &s.reason, &s.outputs, &s.handlers),
        (&m.state, &m.handler, &m.reason, &m.outputs, &m.handlers)
    );
    assert_eq!(s.program, Some("/opt/alt-rx".into()));
    // The wire form: `handler` and each handler's nullable fields.
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["handler"], "alt");
    assert_eq!(v["handlers"][0]["program"], serde_json::Value::Null);
    assert_eq!(v["handlers"][1]["extra"], serde_json::json!(["df"]));
    let back: ModeInfo = serde_json::from_value(v).unwrap();
    assert_eq!(back, m);

    // Both found: the row's own leads with no ranking, alt when ranked first; the
    // outputs are both handlers'.
    probe_adsb(&node, Some("/usr/bin/dump1090"), Some("/opt/alt-rx"));
    let m = listed_one(&node, "adsb_decode").await;
    assert_eq!(m.handler.as_deref(), Some("dump1090"));
    assert_eq!(m.program.as_deref(), Some("/usr/bin/dump1090"));
    assert_eq!(m.outputs, both);
    policy_set(&node, Some("adsb_decode"), &["alt"])
        .await
        .unwrap();
    let m = listed_one(&node, "adsb_decode").await;
    assert_eq!(m.handler.as_deref(), Some("alt"));
    assert_eq!(m.program.as_deref(), Some("/opt/alt-rx"));
    assert_eq!(
        shown(&node, "adsb_decode").await.handler.as_deref(),
        Some("alt")
    );

    // None found: the row's own fields.
    probe_adsb(&node, None, None);
    let m = listed_one(&node, "adsb_decode").await;
    assert_eq!(m.state, ModeState::Unavailable);
    assert_eq!(m.handler, None);
    assert_eq!(m.program, None);
    assert_eq!(m.reason.as_deref(), Some("dump1090: missing"));
    // The row's own outputs (this test's row gives events only), not alt's.
    assert_eq!(m.outputs, [StreamKind::Event]);
    assert_eq!(m.handlers[1].state, ModeState::Unavailable);
    assert_eq!(
        m.handlers[1].reason.as_deref(),
        Some("zeta-alt-decoder: missing")
    );
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["handler"], serde_json::Value::Null);
}

/// With no handler beyond each row's own (the embedded OnAir file replaced by an
/// operator file of its id serving no mode), every mode's own fields read as the row's
/// (the pre-handler listing), and its one handler is the row's own (a row with no
/// decoder of its own lists none, and is unavailable).
#[tokio::test]
async fn a_node_with_no_handlers_beyond_the_rows_lists_modes_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    std::fs::write(modes.join("junk.json"), r#"{"id":"junk_mode","bogus":1}"#).unwrap();
    let handlers = dir.path().join("handlers");
    std::fs::create_dir_all(&handlers).unwrap();
    let no_onair = serde_json::json!({
        "id": "onair", "name": "OnAir", "programs": ["onair-rx"],
        "install": {"url": "https://example.invalid/onair"}, "modes": {},
    });
    std::fs::write(handlers.join("onair.json"), no_onair.to_string()).unwrap();
    let cfg = NodeConfig {
        handlers_dir: Some(handlers),
        ..cfg(dir.path(), &modes)
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    // Every kind of answer a probe leaves: found, missing, an optional helper missing,
    // and a check time.
    let mut reg = (*node.modes()).clone();
    reg.set_program("adsb_decode", Some("/usr/bin/dump1090".into()), None);
    reg.set_program("rs41_decode", None, Some("rs41mod: missing".into()));
    reg.set_program("dmr_decode", Some("/usr/bin/zeta-mbe".into()), None);
    reg.set_missing(
        "dmr_decode",
        "zeta-mbe: missing (x; y) — voice silent".into(),
    );
    let checked = dir.path().join("checked.json");
    ModeRegistry::record_checked(&checked, "adsb_decode", 100).unwrap();
    reg.load_checked(&checked);
    node.modes_handle().set(std::sync::Arc::new(reg));

    let reg = node.modes();
    let list = listed(&node).await;
    assert_eq!(list.len(), reg.all().len());
    for info in &list {
        let m = reg.get(&info.id.0).unwrap();
        let id = &info.id.0;
        let before = (
            m.state,
            m.reason.clone(),
            m.program.as_ref().map(|p| p.display().to_string()),
            m.missing.clone(),
            m.outputs.clone(),
            m.checked,
        );
        let now = (
            info.state,
            info.reason.clone(),
            info.program.clone(),
            info.missing.clone(),
            info.outputs.clone(),
            info.checked,
        );
        assert_eq!(now, before, "{id}");
        let s = shown(&node, id).await;
        let shown_now = (
            s.state,
            s.reason.clone(),
            s.program.as_ref().map(|p| p.display().to_string()),
            s.missing.clone(),
            s.outputs.clone(),
            s.checked,
        );
        assert_eq!(shown_now, before, "{id}");
        if !m.has_decoder() {
            // A row with no decoder of its own has no handler of its own, and the file
            // that served it is gone: it lists none and none would run.
            assert!(info.handlers.is_empty(), "{id}");
            assert_eq!(info.handler, None, "{id}");
            assert_eq!(info.state, ModeState::Unavailable, "{id}");
            continue;
        }
        assert_eq!(info.handlers.len(), 1, "{id}");
        assert_eq!(info.handlers[0].id, m.handlers[0].id, "{id}");
        let own = (m.state == ModeState::Ready).then(|| m.handlers[0].id.clone());
        assert_eq!(info.handler, own, "{id}");
    }
    // The probes above are among them.
    let state = |id: &str| list.iter().find(|m| m.id.0 == id).unwrap().state;
    assert_eq!(state("rs41_decode"), ModeState::Unavailable);
    assert_eq!(state("junk_mode"), ModeState::Broken);
}

/// Plan 47: every embedded row with no decoder of its own says what it carries, is
/// checked on a generated fixture under `onair/`, has no quick test, and its notes name
/// where its ranges come from.
#[test]
fn the_embedded_rows_with_no_decoder_carry_content_and_a_fixture_test() {
    let mut seen = Vec::new();
    for (file, text) in ModeRegistry::embedded() {
        let row = parse(text).unwrap();
        if row.decoder.is_some() {
            continue;
        }
        let id = &row.id.0;
        seen.push(id.clone());
        assert_eq!(
            row.content,
            Some(zeta_core::modes::Content::NetworkTraffic),
            "{file}"
        );
        assert!(row.identify.quick.is_none(), "{id}");
        let test = row.test.as_ref().unwrap_or_else(|| panic!("{id}: no test"));
        let v = serde_json::to_value(test).unwrap();
        assert_eq!(v["kind"], "fixture", "{id}");
        let path = v["path"].as_str().unwrap();
        assert!(
            path.starts_with("onair/") && path.ends_with(".sigmf-meta"),
            "{id}: {path}"
        );
        assert!(!v["expect"].as_object().unwrap().is_empty(), "{id}");
        let notes = row.notes.as_deref().unwrap_or_default();
        assert!(
            notes.contains("Ranges and bands:"),
            "{id}: its notes name where its ranges come from"
        );
    }
    for id in [
        "ble_decode",
        "lorawan_decode",
        "zigbee_decode",
        "thread_decode",
        "zwave_decode",
        "wifi_decode",
        "wisun_decode",
        "tetra_decode",
        "gsmr_decode",
        "lte_decode",
        "dvbs_ip_decode",
    ] {
        assert!(
            seen.iter().any(|s| s == id),
            "{id} has no decoder: {seen:?}"
        );
    }
}

/// A program row's own `decoder.output` takes no `const`: only a handler file's entry
/// does, beside its `output`. The key is unknown there, as any other.
#[test]
fn a_row_refuses_constants_of_its_own() {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("adsb_decode.json"))
        .expect("embedded adsb_decode");
    let mut v: serde_json::Value = serde_json::from_str(text).unwrap();
    // Without constants a row serialises as before.
    let row = parse(text).unwrap();
    assert!(
        serde_json::to_value(&row).unwrap()["decoder"]["output"]
            .get("const")
            .is_none()
    );
    v["decoder"]["output"]["const"] = serde_json::json!({"msg": 1});
    let e = parse(&v.to_string()).unwrap_err();
    assert_eq!(e.field, "const", "{e}");
    assert!(e.error.starts_with("unknown field `const`"), "{e}");
}

// Plan 47: a row with no decoder of its own, run only by a handler file's entry.

mod common;
use common::handlers::{self as hf, NO_DECODER_REASON, NODEC};

/// [`hf::NODEC`]'s row as written, `edit` applied, parsed and validated.
fn nodec_row(edit: impl FnOnce(&mut serde_json::Value)) -> Result<(), zeta_core::modes::RowError> {
    let dir = tempfile::tempdir().unwrap();
    let modes = hf::write_nodec_row(dir.path());
    let text = std::fs::read_to_string(modes.join(format!("{NODEC}.json"))).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
    edit(&mut v);
    validate(&parse(&v.to_string())?, &stages())
}

#[tokio::test]
async fn a_row_with_no_decoder_loads_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(hf::nodec_cfg(dir.path(), None), stages(), vec![])
        .await
        .unwrap();
    let reg = node.modes();
    let m = reg.get(NODEC).unwrap();
    assert!(!m.has_decoder());
    assert!(!m.startable());
    assert_eq!(m.state, ModeState::Unavailable);
    // The stand-in own handler, never ready.
    let own = &m.handlers[0];
    assert_eq!(own.id, zeta_core::modes::handler_file::NO_DECODER);
    assert_eq!((own.name.as_str(), own.in_process), ("none", false));
    assert_eq!(own.state, ModeState::Unavailable);
    assert_eq!(
        own.reason.as_deref(),
        Some("the row has no decoder of its own")
    );
    assert!(own.outputs.is_empty());
    assert_eq!(m.handlers.len(), 1);

    let info = listed_one(&node, NODEC).await;
    assert_eq!(info.state, ModeState::Unavailable);
    assert_eq!(info.reason.as_deref(), Some(NO_DECODER_REASON));
    assert_eq!(info.decoder_kind, None);
    assert!(info.handlers.is_empty(), "{:?}", info.handlers);
    assert_eq!(info.handler, None);
    assert!(info.outputs.is_empty());
    assert!(info.voices.is_empty());
    assert_eq!(info.program, None);
    let v = serde_json::to_value(&info).unwrap();
    assert_eq!(v["decoder_kind"], serde_json::Value::Null);
    let s = shown(&node, NODEC).await;
    assert_eq!(s.row.decoder, None);
    assert_eq!(
        (&s.state, &s.reason, &s.handler, &s.handlers, &s.outputs),
        (
            &info.state,
            &info.reason,
            &info.handler,
            &info.handlers,
            &info.outputs
        )
    );
    // The row file writes `"decoder": null`, and it is required.
    let wire = serde_json::to_value(&s.row).unwrap();
    assert_eq!(wire["decoder"], serde_json::Value::Null);
    let e = nodec_row(|v| {
        v.as_object_mut().unwrap().remove("decoder");
    })
    .unwrap_err();
    assert_eq!(e.field, "decoder", "{e}");
}

#[tokio::test]
async fn a_row_with_no_decoder_is_ready_through_a_handler() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(hf::nodec_cfg(dir.path(), Some(true)), stages(), vec![])
        .await
        .unwrap();
    let program = dir.path().join("bin/zeta-alt-decoder");
    let info = listed_one(&node, NODEC).await;
    assert_eq!(info.state, ModeState::Ready, "{:?}", info.reason);
    assert_eq!(info.reason, None);
    assert_eq!(info.decoder_kind, None);
    let ids: Vec<&str> = info.handlers.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, [hf::ALT]);
    assert_eq!(info.handler.as_deref(), Some(hf::ALT));
    assert_eq!(info.outputs, [StreamKind::Event, StreamKind::Packet]);
    assert_eq!(info.outputs, info.handlers[0].outputs);
    assert_eq!(info.program, Some(program.display().to_string()));
    let s = shown(&node, NODEC).await;
    assert_eq!(
        (&s.state, &s.reason, &s.handler, &s.handlers, &s.outputs),
        (
            &info.state,
            &info.reason,
            &info.handler,
            &info.handlers,
            &info.outputs
        )
    );
    assert_eq!(s.program, Some(program));
    assert!(node.modes().get(NODEC).unwrap().startable());
}

/// The reason of a row with no decoder whose handlers are there but not ready names each
/// one and why, after the probe found its program missing (ruling R1).
#[tokio::test]
async fn a_row_with_no_decoder_names_its_handlers_that_are_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    let node = Node::open(hf::nodec_cfg(dir.path(), Some(false)), stages(), vec![])
        .await
        .unwrap();
    let reg = node.modes();
    let alt = reg.handler(NODEC, hf::ALT).unwrap();
    assert_eq!(alt.state, ModeState::Unavailable, "{alt:?}");
    let why = alt.reason.clone().unwrap();
    assert!(
        why.starts_with("/nonexistent/zeta-test/zeta-alt-decoder: missing"),
        "{why}"
    );
    let reason = format!("{NO_DECODER_REASON}; alt: {why}");
    let info = listed_one(&node, NODEC).await;
    assert_eq!(info.state, ModeState::Unavailable);
    assert_eq!(info.reason.as_deref(), Some(reason.as_str()));
    let ids: Vec<&str> = info.handlers.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, [hf::ALT]);
    assert_eq!(shown(&node, NODEC).await.reason, Some(reason.clone()));
    assert_eq!(
        reg.get(NODEC).unwrap().reason.as_deref(),
        Some(reason.as_str())
    );
}

#[test]
fn a_handler_file_named_none_is_broken() {
    let reg = with_handler_files(&[("n.json", &adsb_handler_file("none", |_| {}))]);
    let file = operator_file(&reg);
    assert_eq!(file.state, ModeState::Broken);
    assert_eq!(file.reason.as_deref(), Some("id: none is reserved"));
    assert_eq!(handler_ids(&reg, "adsb_decode"), ["dump1090", "onair"]);
}

#[test]
fn a_row_with_no_decoder_and_a_quick_test_is_broken() {
    assert_eq!(nodec_row(|_| {}), Ok(()));
    let e = nodec_row(|v| v["identify"]["quick"] = "aprs".into()).unwrap_err();
    assert_eq!(
        (e.field.as_str(), e.error.as_str()),
        ("identify.quick", "a row with no decoder has no quick test")
    );
}

#[test]
fn a_row_with_no_decoder_and_a_synth_test_is_broken() {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("adsb_decode.json"))
        .unwrap();
    let adsb: serde_json::Value = serde_json::from_str(text).unwrap();
    let e = nodec_row(|v| v["test"] = adsb["test"].clone()).unwrap_err();
    assert_eq!(
        (e.field.as_str(), e.error.as_str()),
        ("test.kind", "a row with no decoder is checked on a fixture")
    );
    // A fixture test passes.
    let fixture = serde_json::json!({
        "kind": "fixture", "path": "fixtures/nodec.sigmf-meta",
        "expect": {"icao": "7C6B28"}, "within_ms": 5000
    });
    assert_eq!(nodec_row(|v| v["test"] = fixture), Ok(()));
}

#[test]
fn an_entry_for_a_row_with_no_decoder_must_map_every_field() {
    let dir = tempfile::tempdir().unwrap();
    let modes = hf::write_nodec_row(dir.path());
    let handlers = hf::write_nodec_handler(dir.path(), true);
    let path = handlers.join("alt.json");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Every property of the row's schema but the envelope's and `squawk` mapped.
    let fields: serde_json::Map<String, serde_json::Value> = [
        "icao",
        "msg",
        "callsign",
        "altitude_ft",
        "ground_speed_kt",
        "track_deg",
        "lat",
        "lon",
        "vertical_rate_fpm",
        "on_ground",
    ]
    .iter()
    .map(|f| (f.to_string(), format!("/{f}").into()))
    .collect();
    // The file with its entry a JSON-lines one over `fields`, `edit` applied, served for
    // `mode`.
    let load = |mode: &str, edit: &dyn Fn(&mut serde_json::Value)| {
        let mut f = file.clone();
        let mut e = f["modes"][NODEC].take();
        e["output"] = serde_json::json!({"via": "stdout", "parser": "json_lines"});
        e["output"]["fields"] = fields.clone().into();
        e["args"] = serde_json::json!([]);
        e.as_object_mut().unwrap().remove("packets");
        edit(&mut e);
        f["modes"] = serde_json::json!({ mode: e });
        std::fs::write(&path, f.to_string()).unwrap();
        ModeRegistry::load_with(&stages(), Some(&modes), Some(&handlers))
    };
    let reg = load(NODEC, &|_| {});
    let alt = reg.handler(NODEC, hf::ALT).unwrap();
    assert_eq!(alt.state, ModeState::Broken);
    assert_eq!(
        alt.reason.as_deref(),
        Some(
            "output.fields: squawk is not mapped, and the row has no decoder of its own to \
             fill it"
        )
    );
    // Mapped, or given as a constant, it is filled.
    let reg = load(NODEC, &|e| {
        e["output"]["fields"]["squawk"] = "/squawk".into()
    });
    let alt = reg.handler(NODEC, hf::ALT).unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{:?}", alt.reason);
    let reg = load(NODEC, &|e| e["const"] = serde_json::json!({"squawk": null}));
    let alt = reg.handler(NODEC, hf::ALT).unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{:?}", alt.reason);
    // A row with a decoder of its own fills what an entry leaves out.
    let reg = load("adsb_decode", &|_| {});
    let alt = reg.handler("adsb_decode", hf::ALT).unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{:?}", alt.reason);
}

/// The embedded `fm_demod` row as JSON, to adapt.
fn embedded_row(id: &str) -> serde_json::Value {
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with(&format!("{id}.json")))
        .unwrap_or_else(|| panic!("embedded {id}"));
    serde_json::from_str(text).unwrap()
}

/// A registry over an operator directory holding `rows` (file stem, row).
fn registry_with(rows: &[(&str, serde_json::Value)]) -> ModeRegistry {
    let dir = tempfile::tempdir().unwrap();
    for (stem, row) in rows {
        std::fs::write(dir.path().join(format!("{stem}.json")), row.to_string()).unwrap();
    }
    ModeRegistry::load(&stages(), Some(dir.path()))
}

#[test]
fn the_four_demodulator_rows_load_ready_with_role_demodulator() {
    let reg = ModeRegistry::load(&stages(), None);
    for id in zeta_core::modes::DEMOD_KINDS {
        let m = reg.get(id).unwrap_or_else(|| panic!("{id} listed"));
        assert_eq!(m.state, ModeState::Ready, "{id}: {:?}", m.reason);
        assert_eq!(m.role(), Role::Demodulator, "{id}");
        assert_eq!(m.row.decoder_kind(), Some(DecoderKind::Builtin), "{id}");
        assert_eq!(m.voices, ["audio"], "{id}");
        assert!(m.outputs.contains(&StreamKind::Audio), "{id}");
    }
    assert_eq!(reg.get("pocsag_decode").unwrap().role(), Role::Decoder);
}

#[test]
fn a_builtin_row_may_carry_its_own_kind_name_and_no_other() {
    let mut graph: serde_json::Value = serde_json::from_str(&renamed_pocsag("x")).unwrap();
    graph["id"] = "fm_demod".into();
    let mut scan = embedded_row("fm_demod");
    scan["id"] = "scan".into();
    scan["decoder"]["builtin"] = "scan".into();
    let reg = registry_with(&[("a", embedded_row("fm_demod")), ("b", graph), ("c", scan)]);
    assert_eq!(reg.get("fm_demod").unwrap().state, ModeState::Ready);
    let broken = reg.get("fm_demod@b.json").unwrap();
    assert_eq!(broken.state, ModeState::Broken);
    assert_eq!(
        broken.reason.as_deref(),
        Some("id: fm_demod is a built-in job kind")
    );
    let scan = reg.get("scan").unwrap();
    assert_eq!(scan.state, ModeState::Broken);
}

#[test]
fn a_builtin_row_naming_an_unknown_kind_is_broken() {
    let mut row = embedded_row("fm_demod");
    row["id"] = "foo_demod".into();
    row["decoder"]["builtin"] = "foo_demod".into();
    let reg = registry_with(&[("foo", row)]);
    let m = reg.get("foo_demod").unwrap();
    assert_eq!(m.state, ModeState::Broken);
    let reason = m.reason.as_deref().unwrap();
    assert!(
        reason.contains("decoder.builtin") && reason.contains("foo_demod"),
        "{reason}"
    );
}

/// The reason the registry gives for the one operator row `row`, which must load broken.
fn broken_reason(id: &str, row: serde_json::Value) -> String {
    let reg = registry_with(&[("x", row)]);
    let m = reg
        .get(&format!("{id}@x.json"))
        .or_else(|| reg.get(id))
        .unwrap_or_else(|| panic!("{id} listed"));
    assert_eq!(m.state, ModeState::Broken, "{id}");
    m.reason.clone().unwrap()
}

fn variant_row(builtin: &str) -> serde_json::Value {
    let mut row = embedded_row(builtin);
    row["id"] = "my_demod".into();
    row
}

#[test]
fn a_builtin_row_must_prove_nothing() {
    let mut row = variant_row("fm_demod");
    row["identify"]["proof"] = "event".into();
    let reason = broken_reason("my_demod", row);
    assert!(reason.starts_with("identify.proof"), "{reason}");
}

#[test]
fn proof_none_is_for_a_builtin_row_only() {
    let mut row = embedded_row("pocsag_decode");
    row["id"] = "my_pager".into();
    row["identify"]["proof"] = "none".into();
    let reason = broken_reason("my_pager", row);
    assert!(reason.starts_with("identify.proof"), "{reason}");
}

#[test]
fn a_builtin_rows_protocol_follows_its_kind() {
    for (builtin, wrong) in [
        ("fm_demod", "none"),
        ("am_demod", "none"),
        ("wfm_demod", "squelch"),
        ("ssb_demod", "squelch"),
    ] {
        let mut row = variant_row(builtin);
        row["protocol"] = wrong.into();
        let reason = broken_reason("my_demod", row);
        assert!(reason.starts_with("protocol"), "{builtin}: {reason}");
    }
}

#[test]
fn a_row_named_for_one_kind_may_not_name_another() {
    let mut row = embedded_row("wfm_demod");
    row["id"] = "fm_demod".into();
    let reason = broken_reason("fm_demod", row);
    assert_eq!(reason, "id: fm_demod is a built-in job kind");
}

fn rule_reason(rule: serde_json::Value) -> String {
    let mut row = variant_row("fm_demod");
    for (k, v) in rule.as_object().unwrap() {
        row["band"]["width_rule"][k] = v.clone();
    }
    broken_reason("my_demod", row)
}

#[test]
fn a_width_rule_scale_must_be_above_zero() {
    let reason = rule_reason(serde_json::json!({"scale": 0.0}));
    assert!(reason.starts_with("band.width_rule.scale"), "{reason}");
}

#[test]
fn a_width_rule_fixed_width_must_be_above_zero() {
    let reason = rule_reason(serde_json::json!({"fixed_hz": 0}));
    assert!(reason.starts_with("band.width_rule.fixed_hz"), "{reason}");
}

#[test]
fn a_width_rule_floor_may_not_exceed_its_fixed_width() {
    let reason = rule_reason(serde_json::json!({"floor_hz": 7000, "fixed_hz": 6000}));
    assert!(reason.starts_with("band.width_rule.floor_hz"), "{reason}");
}

#[test]
fn a_width_rule_voice_cap_must_be_above_zero() {
    let reason = rule_reason(serde_json::json!({"voice_max_hz": 0}));
    assert!(
        reason.starts_with("band.width_rule.voice_max_hz"),
        "{reason}"
    );
}

#[test]
fn an_absent_width_rule_serialises_as_null() {
    let row = embedded_row("pocsag_decode");
    let parsed = zeta_core::modes::parse(&row.to_string()).unwrap();
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(back["band"]["width_rule"], serde_json::Value::Null);
}

#[test]
fn an_operator_builtin_row_replaces_the_embedded_one() {
    let mut row = embedded_row("fm_demod");
    row["decoder"]["params"]["deviation_hz"] = 2500.into();
    let reg = registry_with(&[("fm_demod", row)]);
    let m = reg.get("fm_demod").unwrap();
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    let Some(Decoder::Builtin(b)) = &m.row.decoder else {
        panic!("a builtin row");
    };
    assert_eq!(b.params["deviation_hz"], 2500);
}

#[test]
fn a_variant_row_names_a_builtin_under_its_own_id() {
    let mut row = embedded_row("fm_demod");
    row["id"] = "fm_narrow".into();
    let reg = registry_with(&[("fm_narrow", row)]);
    let m = reg.get("fm_narrow").unwrap();
    assert_eq!(m.state, ModeState::Ready, "{:?}", m.reason);
    assert_eq!(m.role(), Role::Demodulator);
}

#[tokio::test]
async fn modes_list_filters_by_role() {
    let dir = tempfile::tempdir().unwrap();
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let node = Node::open(cfg(dir.path(), &modes), stages(), vec![])
        .await
        .unwrap();
    let list = async |role| match node
        .apply(Op::ModesList {
            family: None,
            state: None,
            role,
        })
        .await
    {
        Ok(OpResult::Modes { modes }) => modes,
        other => panic!("expected modes, got {other:?}"),
    };
    let ids = |v: Vec<ModeInfo>| v.into_iter().map(|m| m.id.0).collect::<Vec<_>>();
    assert_eq!(
        ids(list(Some(Role::Demodulator)).await),
        ["am_demod", "fm_demod", "ssb_demod", "wfm_demod"]
    );
    let decoders = ids(list(Some(Role::Decoder)).await);
    assert!(
        decoders.iter().all(|i| !i.ends_with("_demod")),
        "{decoders:?}"
    );
    assert_eq!(list(None).await.len(), EMBEDDED.len());
    // A request without `role` reads as null.
    let op: Op = serde_json::from_value(
        serde_json::json!({"op": "modes_list", "family": null, "state": null}),
    )
    .unwrap();
    assert!(matches!(op, Op::ModesList { role: None, .. }));
}

/// A stand-in program `dir/<name>`: a shell script running `body`, written beside it and
/// renamed into place (as an install replaces a program), so it is never run half
/// written.
fn stand_in(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.new"));
    std::fs::write(&tmp, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    path
}

/// The version block of a stand-in `alt-rx`, its least version `min`.
fn alt_version(min: &str) -> serde_json::Value {
    serde_json::json!({"args": ["--version"], "match": "^alt-rx (\\d+(?:\\.\\d+)*)", "min": min})
}

/// The handler file `alt` (serving `adsb_decode`) whose program is `program`, with
/// `version` as its version block (`None`: no block).
fn versioned_alt(program: &Path, version: Option<serde_json::Value>) -> String {
    adsb_handler_file("alt", |f| {
        f["programs"] = serde_json::json!([program.display().to_string()]);
        if let Some(v) = version {
            f["version"] = v;
        }
    })
}

/// The registry with `alt.json` (see [`versioned_alt`]) probed, and the probe's line for
/// it.
fn probed_alt(program: &Path, version: Option<serde_json::Value>) -> (ModeRegistry, String) {
    let mut reg = with_handler_files(&[("alt.json", &versioned_alt(program, version))]);
    let lines = zeta_core::modes::handlers::probe_handler_files(&mut reg);
    let ours: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains(&program.display().to_string()) || l.contains("alt-rx"))
        .collect();
    assert_eq!(ours.len(), 1, "{lines:?}");
    let line = ours[0].clone();
    (reg, line)
}

/// A handler file's program reporting less than the file's `min` is unavailable, the
/// reason naming both versions (and the install URL); so is a program row's whose block
/// names a higher one. Its program is not recorded as runnable.
#[test]
fn a_handler_older_than_its_file_needs_is_unavailable_with_both_versions() {
    let dir = tempfile::tempdir().unwrap();
    let program = stand_in(dir.path(), "alt-rx", "echo 'alt-rx 0.2.0'");
    let (reg, line) = probed_alt(&program, Some(alt_version("0.4.0")));
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    let reason =
        "alt-rx 0.2.0 is older than 0.4.0 (https://example.invalid/alt) — modes: adsb_decode";
    assert_eq!(alt.state, ModeState::Unavailable, "{alt:?}");
    assert_eq!(alt.reason.as_deref(), Some(reason));
    assert_eq!(alt.program, None);
    assert_eq!(alt.version, None);
    assert_eq!(line, format!("zeta: handlers: {reason}"));

    // A program row with a least version is probed on its own, as a handler file is.
    let modes = dir.path().join("modes");
    std::fs::create_dir_all(&modes).unwrap();
    let (_, text) = ModeRegistry::embedded()
        .into_iter()
        .find(|(f, _)| f.ends_with("adsb_decode.json"))
        .unwrap();
    let mut row: serde_json::Value = serde_json::from_str(text).unwrap();
    row["decoder"]["programs"] = serde_json::json!([program.display().to_string()]);
    row["decoder"]["version"] = alt_version("0.3");
    // No package named: the hint is the same on every host.
    row["decoder"]["install"] = serde_json::json!({"url": "https://example.invalid/row"});
    std::fs::write(modes.join("adsb_decode.json"), row.to_string()).unwrap();
    let mut reg = ModeRegistry::load(&stages(), Some(&modes));
    assert_eq!(reg.get("adsb_decode").unwrap().state, ModeState::Ready);
    let lines = zeta_core::modes::handlers::probe_row_versions(&mut reg);
    let reason =
        "alt-rx 0.2.0 is older than 0.3 (https://example.invalid/row) — modes: adsb_decode";
    let m = reg.get("adsb_decode").unwrap();
    assert_eq!(m.state, ModeState::Unavailable);
    assert_eq!(m.reason.as_deref(), Some(reason), "{lines:?}");
    assert_eq!(m.program, None);
    assert_eq!(lines, [format!("zeta: handlers: {reason}")]);
    // Probed, it is not probed again.
    assert!(zeta_core::modes::handlers::probe_row_versions(&mut reg).is_empty());
}

/// At its file's `min` or over it (on stdout or stderr), the handler is ready with its
/// program and the version it reported, which the probe's line names.
#[test]
fn a_handler_at_or_over_its_floor_is_ready_and_its_version_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    for (body, read) in [
        ("echo 'alt-rx 0.4.0'; echo 'built today'", "0.4.0"),
        ("echo 'alt-rx 0.10.1'", "0.10.1"),
        ("echo 'alt-rx 1.0' >&2", "1.0"),
    ] {
        let program = stand_in(dir.path(), "alt-rx", body);
        let (reg, line) = probed_alt(&program, Some(alt_version("0.4.0")));
        let alt = reg.handler("adsb_decode", "alt").unwrap();
        assert_eq!(alt.state, ModeState::Ready, "{body}: {alt:?}");
        assert_eq!(alt.reason, None, "{body}");
        assert_eq!(alt.program.as_deref(), Some(program.as_path()), "{body}");
        assert_eq!(alt.version.as_deref(), Some(read), "{body}");
        let path = program.display();
        assert_eq!(
            line,
            format!("zeta: handlers: {path}: found {path} (handler alt, version {read})"),
            "{body}"
        );
    }
}

/// A program that answers with no version the block reads is unavailable with what
/// happened: an old one that does not know the argument (its usage on stderr, exit 2),
/// one that prints nothing, one whose line does not match.
#[test]
fn a_handler_whose_version_cannot_be_read_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    for (body, what) in [
        (
            "echo 'usage: alt-rx --mode <m>' >&2; exit 2",
            "it exited with status 2".to_string(),
        ),
        ("exit 0", "it printed nothing".to_string()),
        (
            "echo 'alt-rx version unknown'",
            "nothing it printed matched ^alt-rx (\\d+(?:\\.\\d+)*)".to_string(),
        ),
    ] {
        let program = stand_in(dir.path(), "alt-rx", body);
        let (reg, line) = probed_alt(&program, Some(alt_version("0.4.0")));
        let alt = reg.handler("adsb_decode", "alt").unwrap();
        let reason = format!("alt-rx: its version could not be read ({what}) — modes: adsb_decode");
        assert_eq!(alt.state, ModeState::Unavailable, "{body}");
        assert_eq!(alt.reason.as_deref(), Some(reason.as_str()), "{body}");
        assert_eq!(alt.program, None, "{body}");
        assert_eq!(line, format!("zeta: handlers: {reason}"), "{body}");
    }
}

/// A program that never answers is stopped at the timeout: the probe returns within it
/// (and a little), the handler unavailable.
#[test]
fn a_version_probe_that_hangs_is_cut_at_its_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let program = stand_in(dir.path(), "alt-rx", "sleep 30");
    let started = std::time::Instant::now();
    let (reg, _) = probed_alt(&program, Some(alt_version("0.4.0")));
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(3), "{took:?}");
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Unavailable);
    assert_eq!(
        alt.reason.as_deref(),
        Some("alt-rx: its version could not be read (no answer within 2 s) — modes: adsb_decode")
    );
}

/// The read is capped: a program that prints its version and then megabytes is read
/// to the cap, stopped there and ready; one whose version comes after the cap is
/// unavailable. Neither waits for the timeout.
#[test]
fn a_version_probe_reads_at_most_its_cap() {
    let dir = tempfile::tempdir().unwrap();
    let flood = "head -c 4000000 /dev/zero | tr '\\0' x";
    let program = stand_in(
        dir.path(),
        "alt-rx",
        &format!("echo 'alt-rx 0.4.2'; {flood}; echo"),
    );
    let started = std::time::Instant::now();
    let (reg, _) = probed_alt(&program, Some(alt_version("0.4.0")));
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Ready, "{alt:?}");
    assert_eq!(alt.version.as_deref(), Some("0.4.2"));
    let program = stand_in(
        dir.path(),
        "alt-rx",
        &format!("{flood}; echo; echo 'alt-rx 0.4.2'"),
    );
    let (reg, _) = probed_alt(&program, Some(alt_version("0.4.0")));
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Unavailable);
    assert_eq!(
        alt.reason.as_deref(),
        Some(
            "alt-rx: its version could not be read (nothing in the first 4096 bytes it \
             printed matched ^alt-rx (\\d+(?:\\.\\d+)*)) — modes: adsb_decode"
        )
    );
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
}

/// A handler file without a `version` block is looked up and never run: its program
/// (which leaves a mark when run) is found and ready, no version read.
#[test]
fn a_handler_file_without_a_version_block_runs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mark = dir.path().join("ran");
    let program = stand_in(
        dir.path(),
        "alt-rx",
        &format!("touch '{}'; echo 'alt-rx 0.1.0'", mark.display()),
    );
    let (reg, line) = probed_alt(&program, None);
    let alt = reg.handler("adsb_decode", "alt").unwrap();
    assert_eq!(alt.state, ModeState::Ready);
    assert_eq!(alt.program.as_deref(), Some(program.as_path()));
    assert_eq!(alt.version, None);
    let path = program.display();
    assert_eq!(
        line,
        format!("zeta: handlers: {path}: found {path} (handler alt)")
    );
    assert!(!mark.exists(), "the program was run");
}

/// A reload carries the probe of an unchanged program without running it again; the
/// program replaced (another size and time at the same path), the reload probes it
/// again: the newer one is ready.
#[tokio::test]
async fn a_replaced_program_is_probed_again_on_reload() {
    let dir = tempfile::tempdir().unwrap();
    let (modes, handlers) = (dir.path().join("modes"), dir.path().join("handlers"));
    std::fs::create_dir_all(&handlers).unwrap();
    let runs = dir.path().join("runs");
    let counted =
        |version: &str| format!("echo run >> '{}'; echo 'alt-rx {version}'", runs.display());
    let program = stand_in(dir.path(), "alt-rx", &counted("0.2.0"));
    let file = versioned_alt(&program, Some(alt_version("0.4.0")));
    std::fs::write(handlers.join("alt.json"), file).unwrap();
    let cfg = NodeConfig {
        handlers_dir: Some(handlers.clone()),
        ..cfg(dir.path(), &modes)
    };
    let node = Node::open(cfg, stages(), vec![]).await.unwrap();
    let times_run = || {
        std::fs::read_to_string(&runs)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let alt = || node.modes().handler("adsb_decode", "alt").unwrap().clone();
    assert_eq!(alt().state, ModeState::Unavailable, "{:?}", alt());
    assert_eq!(times_run(), 1);

    // Unchanged: carried, not run.
    let lines = node.reload_modes().unwrap();
    assert!(lines.iter().all(|l| !l.contains("alt-rx")), "{lines:?}");
    assert_eq!(alt().state, ModeState::Unavailable);
    assert!(
        alt()
            .reason
            .unwrap()
            .starts_with("alt-rx 0.2.0 is older than 0.4.0"),
        "{:?}",
        alt()
    );
    assert_eq!(times_run(), 1);

    // Replaced by a newer one: probed again, ready.
    stand_in(
        dir.path(),
        "alt-rx",
        &format!("{} # 0.4.1", counted("0.4.1")),
    );
    let lines = node.reload_modes().unwrap();
    let path = program.display();
    assert!(
        lines.contains(&format!(
            "zeta: handlers: {path}: found {path} (handler alt, version 0.4.1)"
        )),
        "{lines:?}"
    );
    assert_eq!(alt().state, ModeState::Ready, "{:?}", alt());
    assert_eq!(alt().version.as_deref(), Some("0.4.1"));
    assert_eq!(times_run(), 2);
}

/// A version block whose `match` is not a regular expression with one group, or whose
/// `min` is not dotted numbers, breaks its handler file at load (the other files and the
/// embedded one load), and a program row's breaks the row.
#[test]
fn a_bad_version_block_breaks_the_file_alone() {
    let program = Path::new("/nonexistent/alt-rx");
    for (edit, field) in [
        (("match", "^alt-rx \\d+"), "version.match"),
        (("match", "^alt-rx (\\d+) (\\d+)"), "version.match"),
        (("match", "^alt-rx (\\d+"), "version.match"),
        (("min", "0.4.x"), "version.min"),
        (("min", ""), "version.min"),
        (("min", "v0.4"), "version.min"),
    ] {
        let mut v = alt_version("0.4.0");
        v[edit.0] = edit.1.into();
        let good = adsb_handler_file("other", |_| {});
        let reg = with_handler_files(&[
            ("alt.json", &versioned_alt(program, Some(v.clone()))),
            ("other.json", &good),
        ]);
        let bad = reg
            .handler_files()
            .iter()
            .find(|f| f.file.ends_with("alt.json"))
            .unwrap();
        assert_eq!(bad.state, ModeState::Broken, "{v}");
        let reason = bad.reason.as_deref().unwrap();
        assert!(reason.starts_with(&format!("{field}: ")), "{v}: {reason}");
        assert_eq!(
            handler_ids(&reg, "adsb_decode"),
            ["dump1090", "onair", "other"],
            "{v}"
        );
        let onair = reg
            .handler_files()
            .iter()
            .find(|f| f.id == "onair")
            .unwrap();
        assert_eq!(onair.state, ModeState::Ready);

        let mut row: serde_json::Value =
            serde_json::from_str(include_str!("../../../modes/adsb_decode.json")).unwrap();
        row["decoder"]["version"] = v.clone();
        let e = parse(&row.to_string()).and_then(|r| validate(&r, &stages()).map(|_| r));
        let e = e.expect_err("a bad block breaks the row");
        assert_eq!(e.field, format!("decoder.{field}"), "{v}: {e}");
    }
    // The good block loads in both.
    let mut row: serde_json::Value =
        serde_json::from_str(include_str!("../../../modes/adsb_decode.json")).unwrap();
    row["decoder"]["version"] = alt_version("0.4.0");
    let r = parse(&row.to_string()).unwrap();
    validate(&r, &stages()).unwrap();
}

/// Versions compare number by number, a missing number read as 0 and what follows the
/// numbers ignored.
#[test]
fn a_dotted_version_compares_by_number() {
    use zeta_core::modes::handlers::version_at_least;
    assert!(version_at_least("0.10.0", "0.4.0"));
    assert!(!version_at_least("0.4.0", "0.10.0"));
    assert!(version_at_least("0.4", "0.4.0"));
    assert!(version_at_least("0.4.0", "0.4"));
    assert!(!version_at_least("0.3.9", "0.4"));
    assert!(version_at_least("0.4.0-rc1", "0.4.0"));
    assert!(!version_at_least("0.4.0-rc1", "0.4.1"));
    assert!(version_at_least("1", "0.99.99"));
    assert!(version_at_least("0.04.0", "0.4.0"));
    assert!(version_at_least(
        "12345678901234567890.1",
        "12345678901234567890"
    ));
    assert!(!version_at_least("v0.4.0", "0.1"));
}
