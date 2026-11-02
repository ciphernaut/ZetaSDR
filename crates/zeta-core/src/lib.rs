//! ZetaSDR core runtime.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod bank;
pub mod capture;
pub mod classify;
pub mod emission;
pub mod graph;
pub mod job;
pub mod knowledge;
pub mod lease;
pub mod modes;
pub mod node;
pub mod ops;
pub mod plans;
pub mod port;
pub mod reach;
pub mod ring;
pub mod stage;
pub mod station;
pub mod stream;
pub mod stream_in;
#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;
pub mod types;
