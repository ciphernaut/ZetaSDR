//! KiwiSDR websocket IQ source for ZetaSDR.

pub mod proto;
mod session;
mod source;
pub mod status;
// Nothing but tests uses the waterfall's plan until the waterfall session
// (plan 36.2 Task 6) does; Task 6 removes this allow.
#[cfg(feature = "test-fake")]
pub mod testing;
#[allow(dead_code)]
mod wf;

pub use source::{KiwiConfig, KiwiSource};
