//! `zeta registers check [PROVIDER]`: every register query run against the configured
//! service, and `zeta register site PROVIDER SITE_ID`, both against zeta-ext's register
//! stub. Every holder, site, licence and call sign here is made up.

mod common;

use std::path::Path;

use assert_cmd::Command;
use serde_json::{Value, json};
use zeta_ext::testing::acma_stub::{self, Mode};

/// Two transmitters inside the check's default span (400-401 MHz), in
/// `acma_stub::COLUMNS` order; the stub gives them sites `S0` and `S1`.
fn rows() -> Vec<Value> {
    vec![
        json!([
            400_200_000,
            12_500,
            "11K0F3E",
            "T",
            5,
            "W",
            "9100001/1",
            "Example Hill",
            -27.40,
            153.10,
            "Example Water Authority",
            "XX4EX"
        ]),
        json!([
            400_700_000,
            12_500,
            "16K0F1D",
            "T",
            1,
            "W",
            "9100002/1",
            "Example Ridge",
            -27.20,
            153.00,
            "Example Rail Operator",
            null
        ]),
    ]
}

/// `zeta --state-dir <dir>/state <args>` with no remote node, no operator config and no
/// register from the environment.
fn zeta(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("zeta").unwrap();
    cmd.env_remove("ZETA_NODE")
        .env_remove("ZETA_SECRET")
        .env_remove("ZETA_KNOWLEDGE_ACMA")
        .env_remove("ZETA_REGISTERS_DIR")
        .env("XDG_CONFIG_HOME", common::no_xdg_config())
        .arg("--state-dir")
        .arg(dir.join("state"));
    cmd
}

fn run(cmd: &mut Command) -> (String, i32) {
    let out = cmd.output().unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        out.status.code().unwrap(),
    )
}

/// The line of `out` for `<provider>/<id>`.
fn line<'a>(out: &'a str, query: &str) -> &'a str {
    let head = format!("{query}: ");
    out.lines()
        .find(|l| l.starts_with(&head))
        .unwrap_or_else(|| panic!("no line for {query}: {out}"))
}

/// Whether `l` reads `<query>: ok <rows> rows <ms> ms`; the row count.
fn ok_rows(l: &str, query: &str) -> Option<u64> {
    let rest = l.strip_prefix(&format!("{query}: ok "))?;
    let (rows, rest) = rest.split_once(" rows ")?;
    let ms = rest.strip_suffix(" ms")?;
    ms.parse::<u64>().ok()?;
    rows.parse().ok()
}

#[test]
fn every_builtin_query_passes_on_the_stub() {
    let url = acma_stub::serve_on_thread(rows(), Mode::Normal);
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = run(zeta(dir.path()).args(["--knowledge-acma", &url, "registers", "check"]));
    for id in [
        "area_devices",
        "candidates",
        "licence_related",
        "link_far_end",
        "plan_span",
        "site_channels",
    ] {
        let q = format!("acma/{id}");
        assert!(ok_rows(line(&out, &q), &q).is_some(), "{out}");
    }
    assert_eq!(
        ok_rows(line(&out, "acma/candidates"), "acma/candidates"),
        Some(2)
    );
    // The first candidate's licence and site have one device each.
    assert_eq!(
        ok_rows(line(&out, "acma/site_channels"), "acma/site_channels"),
        Some(1)
    );
    let answer = line(&out, "acma");
    let rest = answer.strip_prefix("acma: answer ").expect(answer);
    let (ms, deadline) = rest.split_once(" ms of ").expect(answer);
    ms.parse::<u64>().expect(answer);
    assert_eq!(deadline, "5000 ms", "{answer}");
    assert_eq!(code, 0, "{out}");

    // Named, the provider checks the same; another name is not configured.
    let (named, code) =
        run(zeta(dir.path()).args(["--knowledge-acma", &url, "registers", "check", "acma"]));
    assert_eq!(code, 0, "{named}");
    assert_eq!(named.lines().count(), out.lines().count(), "{named}");
    let (other, code) =
        run(zeta(dir.path()).args(["--knowledge-acma", &url, "registers", "check", "nowhere"]));
    assert_ne!(code, 0, "{other}");
    assert!(other.contains("nowhere"), "{other}");
}

#[test]
fn a_query_the_register_rejects_fails_by_name_and_exits_nonzero() {
    let url = acma_stub::serve_on_thread(rows(), Mode::Normal);
    let dir = tempfile::tempdir().unwrap();
    let registers = dir.path().join("registers");
    std::fs::create_dir_all(registers.join("acma")).unwrap();
    let mine = json!({
        "id": "candidates",
        "provider": "acma",
        "name": "A candidates query naming a column the register lacks",
        "call": "execute_sql",
        "sql": "SELECT d.NO_SUCH_COLUMN AS center_hz FROM device_details d WHERE d.FREQUENCY BETWEEN {lo_hz} AND {hi_hz}",
        "params": [
            {"name": "lo_hz", "kind": "number"},
            {"name": "hi_hz", "kind": "number"}
        ],
        "limit": 10,
        "columns": ["center_hz"]
    });
    std::fs::write(
        registers.join("acma/candidates.json"),
        serde_json::to_string_pretty(&mine).unwrap(),
    )
    .unwrap();
    let (out, code) = run(zeta(dir.path())
        .arg("--registers-dir")
        .arg(&registers)
        .args(["--knowledge-acma", &url, "registers", "check"]));
    let failed = line(&out, "acma/candidates");
    assert!(failed.starts_with("acma/candidates: fail: "), "{out}");
    assert!(failed.contains("NO_SUCH_COLUMN"), "{out}");
    // The queries that need an id from the candidates are skipped, saying so.
    for id in ["licence_related", "site_channels", "link_far_end"] {
        let l = line(&out, &format!("acma/{id}"));
        assert!(l.contains(": skipped: "), "{out}");
    }
    // The rest still answer.
    for id in ["area_devices", "plan_span"] {
        let q = format!("acma/{id}");
        assert!(ok_rows(line(&out, &q), &q).is_some(), "{out}");
    }
    assert!(line(&out, "acma").starts_with("acma: answer "), "{out}");
    assert_ne!(code, 0, "{out}");
}

#[test]
fn an_unreachable_register_is_reported_as_such() {
    // A port nothing listens on: bound, then let go.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let (out, code) = run(zeta(dir.path()).args(["--knowledge-acma", &url, "registers", "check"]));
    let l = line(&out, "acma");
    assert!(l.starts_with("acma: unreachable: "), "{out}");
    assert!(!out.contains("acma/"), "{out}");
    assert_ne!(code, 0, "{out}");
}

#[test]
fn without_a_register_the_check_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = run(zeta(dir.path()).args(["registers", "check"]));
    assert_eq!(out.trim(), "no register configured");
    assert_ne!(code, 0);
}

#[test]
fn register_site_lists_a_sites_channels() {
    let url = acma_stub::serve_on_thread(rows(), Mode::Normal);
    let dir = tempfile::tempdir().unwrap();
    let (out, code) =
        run(zeta(dir.path()).args(["--knowledge-acma", &url, "register", "site", "acma", "S1"]));
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["result"], "register_site", "{v}");
    let a = &v["value"];
    assert_eq!(a["provider"], "acma");
    assert_eq!(a["site_id"], "S1");
    let channels = a["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 1, "{v}");
    assert_eq!(channels[0]["channel"]["center_hz"], 400_700_000);
    assert_eq!(channels[0]["channel"]["role"], "tx");
    assert_eq!(channels[0]["licence_no"], "9100002/1");
    assert_eq!(channels[0]["licensee"], "Example Rail Operator");
}

#[test]
fn register_site_refuses_an_unknown_register_and_a_bad_id() {
    let url = acma_stub::serve_on_thread(rows(), Mode::Normal);
    let dir = tempfile::tempdir().unwrap();
    let (out, code) = run(zeta(dir.path()).args([
        "--knowledge-acma",
        &url,
        "register",
        "site",
        "nowhere",
        "S1",
    ]));
    assert_eq!(code, 2, "{out}");
    let e: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(e["error"], "unknown", "{e}");
    assert_eq!(e["field"], "provider", "{e}");
    assert_eq!(e["known"], json!(["acma"]), "{e}");

    let (out, code) = run(zeta(dir.path()).args([
        "--knowledge-acma",
        &url,
        "register",
        "site",
        "acma",
        "S1' OR 1=1 --",
    ]));
    assert_eq!(code, 2, "{out}");
    let e: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(e["error"], "other", "{e}");
    assert!(
        e["message"].as_str().unwrap().starts_with("site_id: "),
        "{e}"
    );
}
