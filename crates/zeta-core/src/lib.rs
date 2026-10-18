//! ZetaSDR core runtime.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod types;
pub mod lease;
pub mod port;
pub mod ring;
pub mod stream;
