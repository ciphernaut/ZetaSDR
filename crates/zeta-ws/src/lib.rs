//! WebSocket transport for a ZetaSDR node.

pub mod addr;
pub mod auth;
pub mod http;
#[cfg(feature = "test-server")]
pub mod testing;

pub use http::{Health, Limits, Server, ServerConfig, ServerStats, StatsHandle, bind};
