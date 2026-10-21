//! Another ZetaSDR node's speech backend, over its `POST /stt`.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use zeta_client::{ClientError, NodeClient};

use crate::backend::{SpeechBackend, SpeechError, Transcript, default_languages, normalise};
use crate::socket::{PROBE_TIMEOUT, STT_QUEUE_WAIT};

pub struct NodeBackend {
    name: String,
    client: NodeClient,
    languages: Vec<String>,
}

impl NodeBackend {
    /// The node at `url` (`http://HOST:PORT`) with its `secret`. Asks it for its languages
    /// once, waiting at most [`PROBE_TIMEOUT`] (2 s): `["en"]` when it cannot answer then
    /// (reported on stderr). Fails only on a bad URL.
    pub async fn new(url: &str, secret: &str) -> Result<Self, ClientError> {
        let client = NodeClient::new(url, secret)?;
        let probe = tokio::time::timeout(PROBE_TIMEOUT, client.stt_languages()).await;
        let languages = match probe {
            Ok(Ok(langs)) if !langs.is_empty() => langs.iter().map(|l| normalise(l)).collect(),
            Ok(Ok(_)) => default_languages(),
            Ok(Err(e)) => {
                eprintln!("zeta: speech node: {e}; assuming languages [en]");
                default_languages()
            }
            Err(_) => {
                eprintln!(
                    "zeta: speech node {}: no answer to languages within {} s; assuming languages [en]",
                    client.url(),
                    PROBE_TIMEOUT.as_secs()
                );
                default_languages()
            }
        };
        Ok(Self {
            name: format!("node:{}", client.url()),
            client,
            languages,
        })
    }
}

#[async_trait]
impl SpeechBackend for NodeBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn languages(&self) -> &[String] {
        &self.languages
    }

    async fn transcribe(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError> {
        let language = normalise(language);
        match self.client.stt(wav.to_vec(), &language, true).await {
            Ok(text) => Ok(Transcript {
                text,
                language,
                queued_ms: 0,
            }),
            Err(e) => Err(speech_error(e, &language)),
        }
    }

    fn forwards(&self) -> bool {
        true
    }

    /// The peer's own queue: a call there may wait this long for a slot before its daemon
    /// is asked (a peer with a longer wait answers busy only after it, past our bound).
    fn queue_wait(&self) -> Duration {
        STT_QUEUE_WAIT
    }
}

/// The node's refusal as the error its own backend gave, as far as its answer tells.
fn speech_error(e: ClientError, language: &str) -> SpeechError {
    let ClientError::Http { status, body, .. } = &e else {
        return match e {
            ClientError::Unreachable { .. } => SpeechError::Unreachable(e.to_string()),
            _ => SpeechError::Daemon(e.to_string()),
        };
    };
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if *status == 422 && v["error"] == "unsupported" {
        let supported = serde_json::from_value(v["supported"].clone()).unwrap_or_default();
        return SpeechError::Unsupported {
            language: language.to_string(),
            supported,
        };
    }
    if *status == 503 && v["error"] == "busy" {
        // The peer's daemon is full: a drop for the job, as a local busy is.
        let wait = v["wait_ms"]
            .as_u64()
            .map_or(STT_QUEUE_WAIT, Duration::from_millis);
        return SpeechError::Busy(wait);
    }
    let Some(message) = v["message"].as_str() else {
        return SpeechError::Daemon(e.to_string());
    };
    if *status == 503 {
        return SpeechError::Unreachable(message.to_string());
    }
    // The node's own daemon errors already read "speech daemon: ...".
    let message = message.strip_prefix("speech daemon: ").unwrap_or(message);
    SpeechError::Daemon(message.to_string())
}
