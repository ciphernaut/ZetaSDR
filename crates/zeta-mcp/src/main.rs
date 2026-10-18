//! `zeta-mcp`: serve a running ZetaSDR node to an MCP client over stdio; logs go to stderr only.

use std::process::ExitCode;

use clap::Parser;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use zeta_client::NodeClient;
use zeta_mcp::ZetaMcp;

/// MCP server exposing a running ZetaSDR node over stdio.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// The node's URL, `http://host:port`.
    #[arg(long, env = "ZETA_NODE")]
    node: String,
    /// The node's bearer secret.
    #[arg(long, env = "ZETA_SECRET", hide_env_values = true)]
    secret: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let client = match NodeClient::new(&args.node, &args.secret) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let server = match ZetaMcp::new(client).serve(stdio()).await {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    match server.waiting().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("zeta-mcp: {e}");
    ExitCode::FAILURE
}
