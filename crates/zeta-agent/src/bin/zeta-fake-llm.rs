//! A request-keyed fake model for end-to-end tests (feature `test-fake`):
//! `zeta-fake-llm --listen ADDR:PORT --scripts FILE.json`, the file being
//! `[{"match": "substring", "replies": [<chat response JSON>, ...]}]`. Each chat request is
//! answered from the first script whose `match` the last user message contains, every reply
//! with HTTP 200 (see `FakeLlm::start_by_request`). Prints `listening http://ADDR:PORT` on
//! stdout once bound.
use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::process::ExitCode;
use zeta_agent::fake::{Scripts, serve_by_request};

const USAGE: &str = "usage: zeta-fake-llm --listen ADDR:PORT --scripts FILE.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Script {
    #[serde(rename = "match")]
    key: String,
    replies: Vec<Value>,
}

fn load(path: &str) -> Result<Scripts, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let scripts: Vec<Script> = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    Ok(scripts
        .into_iter()
        .map(|s| (s.key, s.replies.into_iter().map(|r| (200, r)).collect()))
        .collect())
}

fn args() -> Result<(String, String), String> {
    let (mut listen, mut scripts) = (None, None);
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--listen" => &mut listen,
            "--scripts" => &mut scripts,
            _ => return Err(format!("unknown argument {flag}")),
        };
        *slot = Some(it.next().ok_or(format!("{flag} needs a value"))?);
    }
    Ok((
        listen.ok_or("--listen is required")?,
        scripts.ok_or("--scripts is required")?,
    ))
}

#[tokio::main]
async fn main() -> ExitCode {
    let (listen, path) = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("zeta-fake-llm: {e}\n{USAGE}");
            return ExitCode::from(1);
        }
    };
    let scripts = match load(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("zeta-fake-llm: {e}");
            return ExitCode::from(1);
        }
    };
    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("zeta-fake-llm: bind {listen}: {e}");
            return ExitCode::from(1);
        }
    };
    let addr = listener.local_addr().expect("bound address");
    println!("listening http://{addr}");
    let _ = std::io::stdout().flush();
    match serve_by_request(listener, scripts).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zeta-fake-llm: {e}");
            ExitCode::from(1)
        }
    }
}
