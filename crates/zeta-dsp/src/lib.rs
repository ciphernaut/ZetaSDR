//! ZetaSDR DSP stages.

pub mod util;

#[cfg(feature = "test-fixtures")]
pub mod fixtures;

pub use util::crc16_x25;

/// Register this crate's stages. Filled in by later tasks.
pub fn register_all(_reg: &mut zeta_core::stage::StageRegistry) {}
