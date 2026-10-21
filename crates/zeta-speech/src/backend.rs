//! The backend trait, its transcript and its errors.

use std::time::Duration;

use async_trait::async_trait;

/// The language a request names when it names none.
pub const DEFAULT_LANGUAGE: &str = "en";

/// What a backend heard, in the language it was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub language: String,
    /// How long the call waited for a slot at the backend, ms; 0 for a backend with no queue.
    pub queued_ms: u64,
}

/// Why a transcription failed; the `Display` texts are the reasons a job reports.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SpeechError {
    #[error("language {language:?} is not supported (supported: {})", supported.join(", "))]
    Unsupported {
        language: String,
        supported: Vec<String>,
    },
    #[error("{0}")]
    Unreachable(String),
    #[error("speech daemon: {0}")]
    Daemon(String),
    #[error("the speech daemon did not answer within {} s", .0.as_secs_f64())]
    Timeout(Duration),
    #[error("the speech daemon is busy: no slot within {} s", .0.as_secs_f64())]
    Busy(Duration),
}

/// Something that turns a 16 kHz mono WAV into text.
#[async_trait]
pub trait SpeechBackend: Send + Sync {
    /// The backend and where it lives, e.g. `socket:/run/user/1000/voiced.sock`.
    fn name(&self) -> &str;
    /// The lower-case language codes `transcribe` accepts.
    fn languages(&self) -> &[String];
    async fn transcribe(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError>;
    /// Whether the backend hands its audio to another node, so a request that was itself
    /// forwarded must not reach it (that would be a forwarding loop).
    fn forwards(&self) -> bool {
        false
    }
    /// The longest a call may wait for a slot before it is attempted (on top of the call
    /// timeout); zero for a backend with no queue.
    fn queue_wait(&self) -> Duration {
        Duration::ZERO
    }
}

/// `language` lower-cased and trimmed, or `Unsupported` when `languages` lacks it.
pub(crate) fn check_language(languages: &[String], language: &str) -> Result<String, SpeechError> {
    let language = normalise(language);
    if languages.contains(&language) {
        Ok(language)
    } else {
        Err(SpeechError::Unsupported {
            language,
            supported: languages.to_vec(),
        })
    }
}

pub(crate) fn normalise(language: &str) -> String {
    language.trim().to_ascii_lowercase()
}

/// A comma-separated language list, lower-cased and trimmed; `["en"]` when it names none.
pub(crate) fn parse_languages(list: &str) -> Vec<String> {
    let langs: Vec<String> = list
        .split(',')
        .map(normalise)
        .filter(|l| !l.is_empty())
        .collect();
    if langs.is_empty() {
        default_languages()
    } else {
        langs
    }
}

pub(crate) fn default_languages() -> Vec<String> {
    vec![DEFAULT_LANGUAGE.to_string()]
}
