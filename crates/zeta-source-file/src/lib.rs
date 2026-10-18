//! File-based IQ source for ZetaSDR.

mod sigmf;

#[cfg(feature = "test-fixtures")]
pub mod fixtures;

pub use sigmf::{FileOpts, FileSource, SigmfMeta, read_meta};
