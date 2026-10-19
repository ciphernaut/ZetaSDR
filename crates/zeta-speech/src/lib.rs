//! Speech-to-text backends for a ZetaSDR node: one [`SpeechBackend`] trait, a local daemon on
//! a Unix socket ([`SocketBackend`]) and another node's `POST /stt` ([`NodeBackend`]).

mod backend;
mod node;
mod socket;
#[cfg(feature = "test-fake")]
pub mod testing;
mod wav;

pub use backend::{DEFAULT_LANGUAGE, SpeechBackend, SpeechError, Transcript};
pub use node::NodeBackend;
pub use socket::SocketBackend;
pub use wav::wav_s16_mono;
