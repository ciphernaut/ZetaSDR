//! ZetaSDR core runtime.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod bank;
pub mod capture;
pub mod classify;
pub mod graph;
pub mod job;
pub mod knowledge;
pub mod lease;
pub mod node;
pub mod ops;
pub mod port;
pub mod ring;
pub mod stage;
pub mod stream;
pub mod types;
