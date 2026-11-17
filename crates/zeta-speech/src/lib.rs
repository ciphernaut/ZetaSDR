//! Speech-to-text backends for a ZetaSDR node: one [`SpeechBackend`] trait, a local daemon on
//! a Unix socket ([`SocketBackend`]) another node's `POST /stt` ([`NodeBackend`]), a Whisper server
//! over HTTP ([`UrlBackend`]), and the `transcribe` stage on them ([`TranscribeStage`]).

mod backend;
mod node;
mod socket;
#[cfg(feature = "test-fake")]
pub mod testing;
mod transcribe;
mod url;
mod wav;

pub use backend::{DEFAULT_LANGUAGE, SpeechBackend, SpeechError, Transcript};
pub use node::NodeBackend;
pub use socket::SocketBackend;
pub use transcribe::{
    Accumulator, NO_BACKEND, NO_BACKEND_REASON, TranscribeParams, TranscribeStage, Utterance,
};
pub use url::UrlBackend;
pub use wav::wav_s16_mono;
