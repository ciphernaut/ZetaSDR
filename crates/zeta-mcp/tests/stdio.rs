//! `zeta-mcp` as a real child process over stdio, against a node on loopback.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use zeta_client::{op_sections, system_prompt};
use zeta_ws::testing::{TestOpts, TestServer, spawn_with};

const AGENTS_MD: &str = include_str!("../../../AGENTS.md");
const READ_GUARD: Duration = Duration::from_secs(10);

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Mcp {
    fn spawn(url: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zeta-mcp"))
            .args(["--node", url, "--secret", "s3cret"])
            .env_remove("ZETA_NODE")
            .env_remove("ZETA_SECRET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn zeta-mcp");
        let stdin = child.stdin.take().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            stdin,
            lines,
        }
    }

    async fn send(&mut self, msg: Value) {
        let line = format!("{msg}\n");
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    /// Write one request and return the `result` of the response with its id,
    /// skipping notifications; a JSON-RPC error fails the test.
    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let line = tokio::time::timeout(READ_GUARD, self.lines.next_line())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within {READ_GUARD:?}"))
                .unwrap()
                .unwrap_or_else(|| panic!("zeta-mcp closed stdout during {method}"));
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                assert!(v.get("error").is_none(), "{method} failed: {v}");
                return v["result"].clone();
            }
        }
    }

    async fn notify(&mut self, method: &str) {
        self.send(json!({"jsonrpc": "2.0", "method": method})).await;
    }

    async fn init(&mut self) -> Value {
        let r = self
            .request(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "t", "version": "0"}
                }),
            )
            .await;
        self.notify("notifications/initialized").await;
        r
    }

    /// `tools/call`, returning `(isError, parsed text of the one content block)`.
    async fn call(&mut self, id: u64, name: &str, args: Value) -> (bool, Value) {
        let r = self
            .request(id, "tools/call", json!({"name": name, "arguments": args}))
            .await;
        let text = r["content"][0]["text"].as_str().expect("text content");
        (
            r["isError"].as_bool().unwrap_or(false),
            serde_json::from_str(text).unwrap(),
        )
    }

    async fn tool_names(&mut self, id: u64) -> Vec<String> {
        let r = self.request(id, "tools/list", json!({})).await;
        names(&r["tools"])
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn names(tools: &Value) -> Vec<String> {
    tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

async fn server(dir: &Path) -> TestServer {
    spawn_with(
        dir,
        TestOpts {
            agents_md: AGENTS_MD.into(),
            ..Default::default()
        },
    )
    .await
}

fn op_names(schema: &Value) -> Vec<String> {
    schema["op"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["properties"]["op"]["const"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn initialize_lists_tools_from_schema() {
    let dir = tempfile::tempdir().unwrap();
    let ts = server(dir.path()).await;
    let mut mcp = Mcp::spawn(&ts.url);
    let init = mcp.init().await;
    assert_eq!(init["serverInfo"]["name"], "zeta-mcp");
    assert!(init["capabilities"]["tools"].is_object(), "{init}");
    assert!(init["capabilities"]["resources"].is_object(), "{init}");
    assert_eq!(init["instructions"], system_prompt(AGENTS_MD));

    let r = mcp.request(2, "tools/list", json!({})).await;
    assert_eq!(names(&r["tools"]), op_names(&ts.node.schema()));
    for t in r["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{t}");
    }
    let ensure = r["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "source_ensure")
        .unwrap();
    assert_eq!(
        ensure["description"],
        op_sections(AGENTS_MD)["source_ensure"]
    );
    drop(mcp);
    ts.stop().await;
}

#[tokio::test]
async fn tools_call_ok_and_corrective_error() {
    let dir = tempfile::tempdir().unwrap();
    let ts = server(dir.path()).await;
    let mut mcp = Mcp::spawn(&ts.url);
    mcp.init().await;

    let (is_error, v) = mcp.call(2, "source_list", json!({})).await;
    assert!(!is_error, "{v}");
    assert_eq!(v["result"], "sources");

    let args = json!({"source": "tone", "center_hz": 100_000, "bandwidth_hz": 48_000});
    let (is_error, v) = mcp.call(3, "source_ensure", args).await;
    assert!(is_error, "{v}");
    assert_eq!(v["error"], "out_of_range");
    assert_eq!(v["suggested"], 100_000_000);
    drop(mcp);
    ts.stop().await;
}

#[tokio::test]
async fn hallucinated_tool_is_error_listing_known() {
    let dir = tempfile::tempdir().unwrap();
    let ts = server(dir.path()).await;
    let mut mcp = Mcp::spawn(&ts.url);
    mcp.init().await;

    let (is_error, v) = mcp.call(2, "tune", json!({})).await;
    assert!(is_error, "{v}");
    assert_eq!(v["error"], "unknown");
    assert_eq!(v["field"], "tool");
    assert_eq!(v["value"], "tune");
    assert!(
        v["known"]
            .as_array()
            .unwrap()
            .contains(&json!("source_ensure")),
        "{v}"
    );
    drop(mcp);
    ts.stop().await;
}

#[tokio::test]
async fn resources_list_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let ts = server(dir.path()).await;
    let mut mcp = Mcp::spawn(&ts.url);
    mcp.init().await;

    let r = mcp.request(2, "resources/list", json!({})).await;
    let uris: Vec<&str> = r["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["uri"].as_str().unwrap())
        .collect();
    for uri in [
        "zeta://sources",
        "zeta://jobs",
        "zeta://streams",
        "zeta://schema",
        "zeta://agents-md",
    ] {
        assert!(uris.contains(&uri), "{uri} missing from {uris:?}");
    }

    let r = mcp
        .request(3, "resources/read", json!({"uri": "zeta://agents-md"}))
        .await;
    assert_eq!(r["contents"][0]["text"], AGENTS_MD);
    assert_eq!(r["contents"][0]["mimeType"], "text/markdown");

    let r = mcp
        .request(4, "resources/read", json!({"uri": "zeta://jobs"}))
        .await;
    let jobs: Value = serde_json::from_str(r["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(jobs["result"], "jobs");
    drop(mcp);
    ts.stop().await;
}

#[tokio::test]
async fn mcp_survives_node_restart() {
    let dir = tempfile::tempdir().unwrap();
    let ts = server(dir.path()).await;
    let addr = ts.addr;
    let mut mcp = Mcp::spawn(&ts.url);
    let pid = mcp.child.id();
    mcp.init().await;
    let all = op_names(&ts.node.schema());

    let (is_error, v) = mcp.call(2, "source_list", json!({})).await;
    assert!(!is_error, "{v}");

    ts.stop().await;
    let (is_error, v) = mcp.call(3, "source_list", json!({})).await;
    assert!(is_error, "{v}");
    assert_eq!(v["error"], "other");
    assert!(
        v["message"].as_str().unwrap().contains("unreachable"),
        "{v}"
    );
    assert_eq!(mcp.tool_names(4).await, all, "fallback tool list");

    let ts = spawn_with(
        dir.path(),
        TestOpts {
            addr: Some(addr),
            agents_md: AGENTS_MD.into(),
            ..Default::default()
        },
    )
    .await;
    let (is_error, v) = mcp.call(5, "source_list", json!({})).await;
    assert!(!is_error, "{v}");
    assert_eq!(v["result"], "sources");
    assert!(mcp.child.try_wait().unwrap().is_none(), "zeta-mcp exited");
    assert_eq!(mcp.child.id(), pid);
    drop(mcp);
    ts.stop().await;
}
