//! A fake model for the by-hand slice: `cargo run -p zeta-agent --example fake_llm --features test-fake -- 127.0.0.1:8081`.
use serde_json::json;
use zeta_agent::fake::{serve_loop, text_response, tool_call_response};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .expect("usage: fake_llm <ADDR>")
        .parse()
        .expect("ADDR is host:port");
    let chat = vec![
        (200, tool_call_response(&[("source_list", json!({}))])),
        (200, text_response("tone", "stop")),
    ];
    serve_loop(addr, chat).await
}
