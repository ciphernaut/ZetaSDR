//! `zeta agents-md`: render AGENTS.md from the operation schema plus the hand-written field craft.

use std::process::ExitCode;

use serde_json::Value;

const FIELD_CRAFT: &str = include_str!("../../../docs/agents/field-craft.md");

const INTRO: &str = "ZetaSDR is an agentic SDR runtime. You drive it through a small typed operation set. \
The CLI form and the JSON form of every operation are equivalent: `zeta <group> <verb> --flag value` \
and `{\"op\": \"<name>\", ...}` do the same thing.";

const RULES: &[&str] = &[
    "One unit per field: frequencies and bandwidths are integers in hertz in fields ending `_hz`, rates are in baud in fields ending `_baud`, times are integer nanoseconds since the UNIX epoch in fields ending `_ns`, durations are integer milliseconds in fields ending `_ms`.",
    "Every field is required and unknown fields are rejected.",
    "Call operations, do not explain.",
    "Errors carry `error`, the offending `field`, the acceptable range and, when the value looks like dropped zeros, `suggested`. Use it.",
    "Unknown signals are reported as unknown, never guessed.",
    "Ids are never invented: `source` values come from `source list`, `job` values from `job list`.",
];

const RUNNING: &str = "Every command takes these global flags:

- `--state-dir PATH`: where jobs and captures persist (default `$XDG_STATE_HOME/zetasdr`, else `~/.local/state/zetasdr`). Pass the same directory on every invocation.
- `--source-file ID=PATH.sigmf-meta`: attach a SigMF recording as source `ID`.
- `--source-kiwi ID=HOST:PORT[/PASSWORD]`: attach a KiwiSDR receiver (0-30 MHz, at most 12 kHz wide) as source `ID`; the port is required and the password is never printed. Without `/PASSWORD` the password comes from `ZETA_KIWI_PASSWORD_<ID>` (`ID` upper-cased, other characters as `_`), which keeps it out of `ps`; the flag wins when both are given.
- `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: use source `SOURCE` of another ZetaSDR node, `URL` being its `http://HOST:PORT` (a `zeta serve --listen` node), as source `ID`. The node's secret comes from `ZETA_NODE_SECRET_<ID>` (`ID` mapped as for Kiwi passwords) and is never printed; without it the command is a usage error (exit 1). A remote source ships narrowband by default: `bandwidth_hz` is capped at 250 000 Hz (or the remote source's own maximum when smaller) unless `max_bandwidth_hz=N` (1-20 000 000) widens it explicitly. Each tune holds a `passthrough` job on the other node, so its `source list` and `job list` show who is using the source; the job is stopped when the tuning changes or this process exits normally (a killed process leaves it running; stop it there with `job stop`).
- `--source-vrt ID=udp://ADDR:PORT`: receive VITA 49.2 (VRT) packets, one per UDP datagram, on `ADDR:PORT` as source `ID`; `ADDR` must be a LAN address (`0.0.0.0` listens on every interface). The sender sets the frequency and rate: a job's band must lie inside the sender's last context packet (`source list` shows it as `current`; a job started before any context packet waits up to 1.5 s for one), otherwise the job fails with `unsupported` saying where the sender is; retune the sender, not the job. Packets that do not decode are skipped.
- `--source-soapy ID=ARGS`: attach a SoapySDR device as source `ID`, `ARGS` being SoapySDR device arguments such as `driver=hackrf,serial=...`; `zeta soapy find [--filter ARGS]` lists the devices attached to this machine as JSON. Only in a zeta built with `RUSTFLAGS=\"--cfg soapy\"` (needs libsoapysdr-dev); otherwise both exit 1 saying so.
- `--ring-budget-mb N` (env `ZETA_RING_BUDGET_MB`): memory budget for one source's IQ history (default a quarter of RAM, at most 4 GiB). The ring keeps 10 s at narrow rates and shrinks to as little as 1 s at wide ones (a ring holds 8 bytes per sample, so 16 Msps gets 8 s of a 1 GiB budget); `iq read` and `classify` spans older than that fail `too_old`. The size chosen is logged to stderr as `zeta: ring for <source>: <s> s at <rate> Hz (<MB> MB)`.
- `--plugin KIND=PATH`: register the program at absolute `PATH` as stage kind `KIND` (lowercase, not a built-in kind). It is probed at startup; a failed probe exits 1 naming the kind and the reason. A plugin runs with a cleared environment: only `PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM` and `PYTHONPATH` pass through, and each `ZETA_PLUGIN_ENV_<NAME>` variable reaches it as `<NAME>`; nothing else (no `ZETA_SECRET`) does.

Sources and plugins are not persisted: repeat the flags, with the same ids and kinds, on every invocation, `serve` included. A source id may be used once across all the source flags. `source list` shows each source's capability record: `remote` is true for a KiwiSDR, a `--source-node` and a `--source-vrt` source, `push` false for SoapySDR devices, and `fault` names a source that has failed (a full Kiwi, an unplugged device); a job on a faulted source fails with that reason.

There are two ways to run:

- One-shot, `zeta <group> <verb> ...`: opens the node, applies one operation, prints the JSON result and exits (0 on success, 2 with a JSON error). It never resumes jobs: a job started one-shot is persisted as `running` and runs once `zeta serve` starts. `job start --then-tail MAX_FRAMES,TIMEOUT_MS` keeps that one-shot node up after the start, tails the job's first event stream (else its audio stream), then stops the job and prints `[job_result, frames_result, {\"stopped\": true}]`; add `--keep` to leave it running (`{\"stopped\": false}`). If the job is not running (e.g. failed) it prints `[job_result, job_record]`, the record's `state` carrying `status` and `reason`, and exits 2; `stream tail` on its own only sees streams of jobs running in the same process, and `job attach` needs its input job running in the same node, so attach against `zeta serve` (`--node`). A one-shot `iq read` or `classify` reads the ring that the command itself fills from the source, waiting up to 5 s for it to cover the span. One-shot commands, like `zeta serve`, start and probe every `--plugin` program at startup, so with plugins configured even a one-shot `source list` costs one process start per plugin.
- `zeta serve`: resumes persisted jobs and keeps them running until Ctrl-C.
- Against a running node, `zeta --node URL --secret SECRET <group> <verb> ...` (or the `ZETA_NODE` and `ZETA_SECRET` variables): the operation is sent to that node over HTTP and applied there, so jobs started this way keep running; `--state-dir`, the `--source-*` flags and `--plugin` belong to the serving node and are rejected (a usage error, exit 1); `--stt-socket` and `--stt-node` belong to it too but are ignored. `zeta soapy find` always runs locally. `--then-tail` is not supported with `--node` yet (a usage error, exit 1): start the job, then `stream list` and `stream tail` against the node. Start that node with `zeta serve --listen PORT` (binds 127.0.0.1) or `--listen ADDR:PORT` (a LAN address) and a secret. `zeta-mcp --node URL` offers the same operations as MCP tools, and `zeta ask \"REQUEST\"` lets a local model call them.
- `zeta serve --listen PORT --llm-url URL --llm-model NAME` (the other `--llm-*` flags of `zeta ask` apply too) enables `POST /ask` with `{\"request\": \"TEXT\", \"max_turns\": 8}` (`max_turns` 1-32, default 8): the node runs the agent against itself, so every operation it applies appears on the `ops` stream, and answers the transcript (`request`, `answer`, `calls`, `turns`) as `zeta ask` prints it. One run at a time (409 while one is in progress); 503 when serve has no `--llm-url`; a failed run is 502 with the error and the transcript so far; 504 after 300 s.
- `zeta serve --listen PORT --stt-socket PATH` (env `ZETA_STT_SOCKET`; default `$XDG_RUNTIME_DIR/voiced.sock` when that file exists) enables `POST /stt?lang=CODE` (`lang` defaults to `en`): the body is a 16 kHz mono `audio/wav` (at most 4 MiB), the node hands it to the speech daemon at that Unix socket and answers `{\"text\": \"TRANSCRIPT\"}`. The daemon is `local-voice` or any daemon speaking its protocol: one line per connection, `file:<absolute wav path>` answered `ok:<text>` or `error:<message>`, and `languages` answered `ok:en,de,...` (a daemon answering `error:` there is taken to know English only). Instead of a daemon, `--stt-node URL` (env `ZETA_STT_NODE`, secret in `ZETA_STT_NODE_SECRET`, never printed) forwards each request to that node's `POST /stt`; the two flags are exclusive, for every command (exit 1). A forwarded request carries `X-Zeta-Stt-Hop: 1`, and a node that would forward it again answers 508 (`stt forwarding loop`). `GET /stt/languages` answers `{\"backend\": NAME, \"languages\": [CODES]}` (503 without a backend). 415 when the body is not WAV, 413 when too large, 422 `{\"error\": \"unsupported\", \"language\", \"supported\"}` for a language the backend lacks, 502 when the daemon reports an error, 503 when serve has no backend or cannot reach it, 504 after 30 s.

While `zeta serve` runs it owns the state directory (it holds `serve.lock` there). Mutating one-shot commands (`job start`, `job attach`, `job stop`, `graph put`, `source ensure`) are then refused with an `other` error naming the server's pid; read-only commands still work. Use `--node` to change a node that `zeta serve` owns.

Every operation the node applies is appended to `ops.log` in the state directory (rotated at 8 MiB) with its caller's transport; `zeta ops tail --lines N` prints the last N (local only).";

/// Hand-written sentence appended to an operation's section.
fn note(op: &str) -> Option<&'static str> {
    match op {
        "graph_put" => Some(
            "Fetch the current graph with `graph get`, edit it, and put it back; stage kinds and their params are listed under Stage kinds below and by `schema`.",
        ),
        "job_start" => Some(
            "`fm_demod` is narrowband FM voice; `wfm_demod` is wideband broadcast FM (mono, 50 us de-emphasis, 15 kHz audio; give it a band of about 200000 Hz). `am_demod` is AM voice (envelope detector, 5 kHz audio, slow AGC; give it a band of about 10000 Hz). `ssb_demod` is SSB voice (2800 Hz audio, slow AGC; `center_hz` is the suppressed-carrier (dial) frequency, not the middle of the voice band, and the band must cover both sides of it: give it about 6000 Hz). All four output 48 kHz mono audio: an audio frame's payload is `{\"sample_rate_hz\", \"channels\", \"samples\": N, \"peak\"}`, `channels` 1 here, the samples interleaved and `samples` the per-channel count (a frame without `channels` means 1). `pocsag_decode` decodes POCSAG pagers (512, 1200 and 2400 baud detected automatically) from a 12.5 kHz FM channel into one event per message (`address`, `function`, `kind`, `text`). `rds_decode` reads the RDS data of a broadcast FM station (give it the same 200000 Hz band as `wfm_demod`) into events carrying `pi`, `ps` (station name) and `rt` (radio text) as they complete; the first event takes a few seconds. `adsb_decode` decodes Mode S / ADS-B at 1090000000 Hz with `bandwidth_hz` 2400000 (anything under 2000000 is refused) by running dump1090 on the node's host; one event per decoded message (`icao`, `callsign`, `altitude_ft`, `lat`, `lon`, `ground_speed_kt`, `track_deg`), position and identity arriving in separate messages. The job fails with a reason naming dump1090 when the host has none. `acars_decode` decodes ACARS (VHF, AM, 2400 baud MSK) from a 10000 Hz channel on 131550000 Hz (worldwide primary), 131725000 / 131825000 (Europe), 130025000 / 130450000 / 131125000 (North America), 131450000 (Asia) into one event per CRC-clean block (`reg`, `flight`, `label`, `text`). `deviation_hz` is the FM peak deviation for `fm_demod` and `wfm_demod` (100-75000 Hz; `null` means the kind's default: 5000 Hz for `fm_demod`, right for standard voice FM, use 2500 for narrow 12.5 kHz channels; 75000 Hz for `wfm_demod`, right for broadcast FM). Every other kind takes `null`. `sideband` is `usb` or `lsb` for `ssb_demod` (`null` means `usb`; amateur convention is `lsb` below 10 MHz, `usb` above); every other kind takes `null`. On the CLI, omit `--deviation-hz` and `--sideband` for `null`. `scan` sweeps a band wider than one capture: the node steps the source across it in captures of at most 16 MHz, dwells 250 ms on each, and emits `spectrum` rows whose `center_hz` is that step's (frame `channel` `scan/<step index>`), wrapping until stopped. A scan time-shares the radio: while it runs, every other `job_start` and `source_ensure` on the source is a `conflict` naming it, decoders included, and `source_list` shows the step it is on as `current`. Stop the scan to decode. A scan's `job_context` holds `scan.steps` (how many captures one sweep takes), `scan.step_hz` and `scan.dwell_ms`: report step counts from it, never from guesswork. `transcribe` is not started on the radio: `job_start` refuses it; use `job_attach`.",
        ),
        "job_attach" => Some(
            "Attach a processing chain to a running job's output instead of the radio: `transcribe` reads the job's audio (any demodulator, later any digital voice decoder), cleans it (denoise, voice gate) and emits one `transcript` event per utterance (`text`, `language`, `start_ns`, `end_ns`) plus `vad` events as speech starts and stops; it holds no lease and ends when the input job stops. `language` is a BCP-47 code (`null` means `en`; the backend's languages are in `GET /stt/languages`). To transcribe a station: `job_start` a `wfm_demod` on it, then `job_attach` with `kind: transcribe` and that job's id. The attached job fails with a reason naming the speech backend when the node has none or it cannot serve the language.",
        ),
        "job_list" => Some(
            "Stopped and failed jobs are kept as history, newest 20 only (older ones are forgotten with their files); a running job is never dropped. Through `ask` (the agent transport) each record is returned without its `graph`; `graph_get` fetches one.",
        ),
        "knowledge_lookup" => Some(
            "Two providers answer: the built-in seed signatures, and `catalogue`, the 594-signal iNTERCEPT catalogue (Apache-2.0; one signature per frequency range of each signal overlapping the band, `bandwidth_hz` its widest bandwidth, `modulation` the first it lists). A signature's `decoder` names the job kind that proves it: start that kind on the band to decode it; `classify` attempts only the APRS, POCSAG, ACARS and RDS ones as decode-to-identify. Neither has allocations, so an empty `allocations` list means no provider data, not nothing allocated.",
        ),
        "classify" => Some(
            "Layer 1 labels the measured band (`wfm`, `fm`, `am`, `ssb`, `fsk`, `carrier`, `noise` or `unknown`; an `ssb` label's evidence gives the dial frequency). Layer 2 first tries decode-to-identify: `APRS`, `POCSAG` and `ACARS` on a selection narrower than 150000 Hz (100000 Hz when the whole block is measured), `RDS` on a wider one, and labels a protocol only when its CRC/BCH-clean decode passes; ADS-B is never attempted by `classify` (use `adsb_decode`). Layer 2 also adds up to three signal names from the catalogue (evidence `catalogue <id>: ...`), scored 0.4 for the measured centre lying in one of the signal's frequency ranges (required), 0.3 for a matching bandwidth (required for signals wider than 50 kHz) and 0.3 for a matching layer-1 modulation. Layer 3 is an empty slot.",
        ),
        _ => None,
    }
}

/// An operation's example value for a top-level field, when the type's first value
/// would be wrong (only `transcribe` can be attached).
fn example_for(op: &str, field: &str) -> Option<Value> {
    match (op, field) {
        ("job_attach", "kind") => Some("transcribe".into()),
        _ => None,
    }
}

/// Where a field's value comes from, when the type alone does not say.
fn hint(field: &str) -> Option<&'static str> {
    match field {
        "source" => Some("an id from `source list`"),
        "job" => Some("an id from `job list`"),
        "stream" => Some("an id from `stream list`"),
        "region" => Some("free-text region code such as `AU`"),
        _ => None,
    }
}

/// Unit label of a named integer type, stated explicitly.
fn unit(name: &str) -> Option<&'static str> {
    match name {
        "Hz" => Some("Hz"),
        "Baud" => Some("baud"),
        "Timestamp" => Some("ns since epoch"),
        _ => None,
    }
}

/// Render the guide from the node schema (`op`, `result`, `error`, `stages`)
/// and the field-craft text.
pub fn render(schema: &Value, field_craft: &str) -> String {
    let op = &schema["op"];
    let defs = &op["$defs"];
    let mut out = format!("# ZetaSDR agent guide\n\n{INTRO}\n\n## Rules\n\n");
    for r in RULES {
        out.push_str(&format!("- {r}\n"));
    }
    out.push_str(&format!("\n## Running the node\n\n{RUNNING}\n"));
    out.push_str("\n## Operations\n");
    for variant in op["oneOf"].as_array().into_iter().flatten() {
        let name = variant["properties"]["op"]["const"].as_str().unwrap_or("?");
        let fields: Vec<(&str, &Value)> = variant["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|f| *f != "op")
            .map(|f| (f, &variant["properties"][f]))
            .collect();
        out.push_str(&format!("\n### {name}\n\n"));
        if let Some(d) = variant["description"].as_str() {
            out.push_str(&format!("{}\n\n", d.replace('\n', " ")));
        }
        for (f, s) in &fields {
            let h = hint(f).map(|h| format!("; {h}")).unwrap_or_default();
            out.push_str(&format!("- {f}: {}{h}\n", type_of(s, defs)));
        }
        if !fields.is_empty() {
            out.push('\n');
        }
        let mut json = format!("{{\"op\":\"{name}\"");
        for (f, s) in &fields {
            let ex =
                example_for(name, f).map_or_else(|| example_json(f, s, defs), |v| v.to_string());
            json.push_str(&format!(",\"{f}\":{ex}"));
        }
        json.push('}');
        out.push_str(&format!(
            "JSON: `{json}`\n\nCLI: `{}`\n",
            cli_form(name, &fields, defs)
        ));
        if let Some(n) = note(name) {
            out.push_str(&format!("\n{n}\n"));
        }
    }
    out.push_str("\n## Stage kinds\n\nA graph node's `kind` names one of these; its `params` must match the listed fields.\n");
    for st in schema["stages"].as_array().into_iter().flatten() {
        let list = |k: &str| -> String {
            let v: Vec<&str> = st[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            v.join(", ")
        };
        let ps = &st["params_schema"];
        out.push_str(&format!(
            "\n### {}\n\n- inputs: {}\n- outputs: {}\n",
            st["kind"].as_str().unwrap_or("?"),
            list("inputs"),
            list("outputs")
        ));
        for p in required(ps) {
            out.push_str(&format!(
                "- params.{p}: {}\n",
                type_of(&ps["properties"][p], &ps["$defs"])
            ));
        }
    }
    out.push_str("\n## Field craft\n\n");
    out.push_str(field_craft);
    out
}

/// The `$defs` entry a `$ref` points at, with its name.
fn resolve<'a>(s: &'a Value, defs: &'a Value) -> (Option<&'a str>, &'a Value) {
    match s["$ref"].as_str().and_then(|r| r.strip_prefix("#/$defs/")) {
        Some(n) => (Some(n), &defs[n]),
        None => (None, s),
    }
}

fn required(def: &Value) -> Vec<&str> {
    def["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn type_of(s: &Value, defs: &Value) -> String {
    let (name, def) = resolve(s, defs);
    // A nullable scalar (`["integer","null"]`) reads as `integer or null`.
    if let Some(a) = def["type"].as_array() {
        let t: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
        return t.join(" or ");
    }
    let ty = def["type"].as_str().unwrap_or("any");
    match (ty, name) {
        ("integer", Some(n)) => format!("integer ({})", unit(n).unwrap_or(n)),
        ("string", Some(n)) if def["enum"].is_array() => {
            let vals: Vec<&str> = def["enum"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            format!("string ({n}: {})", vals.join("|"))
        }
        ("object", Some(n)) => {
            let parts: Vec<String> = required(def)
                .into_iter()
                .map(|p| {
                    let (_, pd) = resolve(&def["properties"][p], defs);
                    let (item_name, item) = resolve(&pd["items"], defs);
                    if pd["type"] == "object" {
                        let subs = required(pd);
                        let units: Vec<Option<&str>> = subs
                            .iter()
                            .map(|q| resolve(&pd["properties"][*q], defs).0.and_then(unit))
                            .collect();
                        let tail = match units.first() {
                            Some(Some(u)) if units.iter().all(|x| *x == Some(*u)) => {
                                format!(": integer ({u})")
                            }
                            _ => String::new(),
                        };
                        format!("{p}{{{}{tail}}}", subs.join(","))
                    } else if let (Some(n), "object") =
                        (item_name, item["type"].as_str().unwrap_or(""))
                    {
                        format!("{p}[{n}: {}]", required(item).join(", "))
                    } else {
                        p.to_string()
                    }
                })
                .collect();
            format!("object ({n}: {})", parts.join(", "))
        }
        _ => ty.to_string(),
    }
}

fn example(field: &str, s: &Value, defs: &Value) -> Value {
    let (name, def) = resolve(s, defs);
    match def["type"].as_str() {
        _ if name == Some("NodeId") => "<node-id>".into(),
        Some("integer") => match (field, name) {
            ("bandwidth_hz", _) => 12500.into(),
            (_, Some("Hz")) => 144800000u64.into(),
            ("start_ns", _) => 1760000000000000000u64.into(),
            ("end_ns", _) => 1760000001000000000u64.into(),
            ("max_frames", _) => 1.into(),
            ("timeout_ms", _) => 5000.into(),
            _ => 0.into(),
        },
        Some("string") => match def["enum"].as_array().and_then(|a| a.first()) {
            Some(first) => first.clone(),
            None if field == "region" => "AU".into(),
            None if field == "kind" => "passthrough".into(),
            None => "<id>".into(),
        },
        Some("object") => {
            let mut m = serde_json::Map::new();
            for p in required(def) {
                m.insert(p.into(), example(p, &def["properties"][p], defs));
            }
            Value::Object(m)
        }
        Some("array") if field == "nodes" => Value::Array(vec![example("node", &s["items"], defs)]),
        Some("array") => Value::Array(vec![]),
        _ if field == "params" => serde_json::json!({"kind": "iq"}),
        _ => Value::Null,
    }
}

/// Example value as JSON text, object keys in schema (required) order.
fn example_json(field: &str, s: &Value, defs: &Value) -> String {
    let (_, def) = resolve(s, defs);
    if def["type"] != "object" {
        return example(field, s, defs).to_string();
    }
    let parts: Vec<String> = required(def)
        .into_iter()
        .map(|p| format!("\"{p}\":{}", example_json(p, &def["properties"][p], defs)))
        .collect();
    format!("{{{}}}", parts.join(","))
}

/// Flag name and shell-ready value for each leaf the CLI takes.
fn cli_flags(field: &str, s: &Value, defs: &Value, out: &mut Vec<(String, String)>) {
    let (name, def) = resolve(s, defs);
    match (field, name) {
        ("selection", _) | ("span", _) => {
            for p in required(def) {
                if p == "span" {
                    cli_flags(p, &def["properties"][p], defs, out);
                } else {
                    out.push((p.replace('_', "-"), value(p, &def["properties"][p], defs)));
                }
            }
        }
        ("graph", _) => out.push(("file".into(), "graph.json".into())),
        _ => out.push((field.replace('_', "-"), value(field, s, defs))),
    }
}

fn value(field: &str, s: &Value, defs: &Value) -> String {
    match example(field, s, defs) {
        Value::String(t) => t,
        v => v.to_string(),
    }
}

fn cli_form(op: &str, fields: &[(&str, &Value)], defs: &Value) -> String {
    let mut line = String::from("zeta ");
    line.push_str(&match op.split_once('_') {
        Some((g, v)) => format!("{g} {v}"),
        None => op.to_string(),
    });
    let mut flags = vec![];
    for (f, s) in fields {
        // A nullable field is an optional flag; the example omits it.
        if s["type"]
            .as_array()
            .is_some_and(|a| a.contains(&"null".into()))
        {
            continue;
        }
        match example_for(op, f) {
            Some(Value::String(v)) => flags.push((f.replace('_', "-"), v)),
            _ => cli_flags(f, s, defs, &mut flags),
        }
    }
    for (flag, v) in flags {
        line.push_str(&format!(" --{flag} {v}"));
    }
    line
}

/// The full AGENTS.md text for this build.
pub fn rendering() -> String {
    let mut schema = zeta_core::ops::schema();
    schema["stages"] = crate::node_setup::registry(None).describe();
    render(&schema, FIELD_CRAFT)
}

/// The Claude Code skill: front matter naming when to use it, then the full guide.
pub fn render_skill(md: &str) -> String {
    format!(
        "---\nname: zetasdr\ndescription: Use when driving a ZetaSDR node (sources, jobs, IQ captures, classification, knowledge lookup) through the zeta CLI or the zeta-mcp tools.\n---\n\n{md}"
    )
}

/// Print the rendering, or with `check` compare it to the committed file
/// (`AGENTS.md`, or `skills/zetasdr/SKILL.md` with `skill`).
pub fn run(check: bool, skill: bool) -> ExitCode {
    let guide = rendering();
    let (text, path, name, regen) = if skill {
        (
            render_skill(&guide),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/zetasdr/SKILL.md"),
            "skills/zetasdr/SKILL.md",
            "zeta agents-md --skill > skills/zetasdr/SKILL.md",
        )
    } else {
        (
            guide,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../AGENTS.md"),
            "AGENTS.md",
            "zeta agents-md > AGENTS.md",
        )
    };
    if !check {
        print!("{text}");
        return ExitCode::SUCCESS;
    }
    match std::fs::read_to_string(path) {
        Ok(on_disk) if on_disk == text => ExitCode::SUCCESS,
        Ok(on_disk) => {
            let line = text.lines().zip(on_disk.lines()).position(|(a, b)| a != b);
            let at = line.unwrap_or_else(|| text.lines().count().min(on_disk.lines().count())) + 1;
            eprintln!(
                "zeta: {name} differs from the rendering (first difference at line {at}); regenerate with `{regen}`"
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("zeta: reading {path}: {e}");
            ExitCode::FAILURE
        }
    }
}
