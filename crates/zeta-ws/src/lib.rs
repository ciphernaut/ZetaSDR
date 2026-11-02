//! WebSocket transport for a ZetaSDR node.

pub mod addr;
mod ask;
pub mod auth;
mod captures;
pub mod http;
pub mod stream;
mod stt;
#[cfg(feature = "test-server")]
pub mod testing;
mod ui;
pub mod wire;

pub use http::{Health, Limits, Server, ServerConfig, ServerStats, StatsHandle, bind};
pub use wire::{WsFrame, WsMessage, ws_frame};

/// The id of the build this process runs, reported by `GET /health` as `build` so a page can
/// notice that the node was upgraded under it and reload (the crate version alone does not
/// change between builds). `ZETA_BUILD_ID` in the environment wins (tests, operators); else
/// `<version>+<modification time of the executable, UNIX seconds>`, which changes on every
/// relink whatever crate changed, and `<version>+unknown` when the executable cannot be read.
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        if let Ok(id) = std::env::var("ZETA_BUILD_ID")
            && !id.trim().is_empty()
        {
            return id.trim().to_string();
        }
        let built = std::env::current_exe()
            .and_then(|p| p.metadata())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!("{}+{built}", zeta_core::VERSION)
    })
}
