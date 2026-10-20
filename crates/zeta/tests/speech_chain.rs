//! The source-separation slot, proven with an external audio plugin: an attached
//! transcription job's `denoise` node is replaced by `separate` (the Python stub
//! `examples/python/separate_stub.py`) through `graph get` / `graph put`, and the chain
//! still ends in a `transcript` event.

use std::path::Path;

use serde_json::{Value, json};

mod common;

const NODE_SECRET: &str = "s3cret-long-enough";

fn script(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/python")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("examples/python/{name}: {e}"))
        .display()
        .to_string()
}

fn have_python3() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn json_of(out: &[u8]) -> Value {
    serde_json::from_slice(out).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(out)))
}

#[test]
fn separate_plugin_replaces_denoise_in_an_attached_chain() {
    if !have_python3() {
        eprintln!("python3 not found; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut audio = zeta_dsp::fixtures::speech_like(48_000, 3.0);
    audio.extend(std::iter::repeat_n(0.0, 2 * 48_000));
    let iq = zeta_dsp::fixtures::wfm_modulate(&audio, 48_000, 240_000, 75_000.0, 0.0, true);
    let meta = zeta_dsp::fixtures::write_fixture(dir.path(), "speech", &iq, 240_000, 104_700_000);
    let sock = dir.path().join("voiced.sock");
    let _daemon = zeta_speech::testing::FakeDaemon::replying(&sock, "ok:hello from the daemon\n");
    let src = format!("speech={}", meta.display());
    let plugin = format!("separate={}", script("separate_stub.py"));
    let node = common::ServeProc::start_full(
        dir.path(),
        NODE_SECRET,
        &["--source-file", &src, "--plugin", &plugin],
        &["--stt-socket", sock.to_str().unwrap()],
        &[],
    );
    let remote = |args: &[&str]| {
        json_of(
            &common::zeta_remote(&node.url, NODE_SECRET, args)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        )
    };
    let wfm = remote(&[
        "job",
        "start",
        "--kind",
        "wfm_demod",
        "--source",
        "speech",
        "--center-hz",
        "104700000",
        "--bandwidth-hz",
        "200000",
    ]);
    let wfm = wfm["value"].as_str().expect("job id").to_string();
    let att = remote(&["job", "attach", "--kind", "transcribe", "--job", &wfm]);
    let att = att["value"].as_str().expect("job id").to_string();

    let got = remote(&["graph", "get", "--job", &att]);
    let mut graph = got["value"].clone();
    let nodes = graph["nodes"].as_array_mut().expect("nodes");
    let denoise = nodes
        .iter_mut()
        .find(|n| n["kind"] == "denoise")
        .unwrap_or_else(|| panic!("no denoise node in {got}"));
    denoise["kind"] = json!("separate");
    denoise["params"] = json!({"model": "stub"});
    let file = dir.path().join("graph.json");
    std::fs::write(&file, graph.to_string()).unwrap();
    let mut frames: Vec<Value> = Vec::new();
    for _attempt in 0..3 {
        let put = remote(&[
            "graph",
            "put",
            "--job",
            &att,
            "--file",
            file.to_str().unwrap(),
        ]);
        assert_ne!(put["error"], "invalid", "{put}");

        // Timing matters: the tails must start right after the `graph put`. Started a second
        // later, or in later windows over the looping source, no transcript arrived within 40 s
        // (see the task 8 report), so do not insert work between the put and the tails.
        // The restarted chain also goes quiet now and then (about one run in three), so the
        // put is tried up to three times.
        // Tail the restarted chain's event streams (`vad`'s and `transcribe`'s) at once, then
        // check the stored graph while they wait.
        let streams = remote(&["stream", "list"]);
        let events: Vec<String> = streams["value"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["provenance"]["job"] == att.as_str() && s["kind"] == "event")
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(events.len(), 2, "{streams}");
        let tails: Vec<_> = events
            .into_iter()
            .map(|s| {
                let url = node.url.clone();
                std::thread::spawn(move || {
                    let args = [
                        "stream",
                        "tail",
                        "--stream",
                        &s,
                        "--max-frames",
                        "1",
                        "--timeout-ms",
                        "10000",
                    ];
                    json_of(
                        &common::zeta_remote(&url, NODE_SECRET, &args)
                            .assert()
                            .success()
                            .get_output()
                            .stdout
                            .clone(),
                    )
                })
            })
            .collect();
        let again = remote(&["graph", "get", "--job", &att]);
        let kinds: Vec<&str> = again["value"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|n| n["kind"].as_str())
            .collect();
        assert_eq!(
            kinds,
            ["stream_in", "separate", "vad", "transcribe"],
            "{again}"
        );
        frames = tails
            .into_iter()
            .flat_map(|t| {
                t.join().unwrap()["value"]["frames"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();
        let found = frames
            .iter()
            .any(|f| f["payload"]["protocol"] == "transcript");
        if found {
            break;
        }
    }
    let t = frames
        .iter()
        .find(|f| f["payload"]["protocol"] == "transcript")
        .unwrap_or_else(|| panic!("no transcript in {frames:?}"));
    assert_eq!(t["payload"]["text"], "hello from the daemon", "{t}");
    assert_eq!(t["job"], att.as_str(), "{t}");
}
