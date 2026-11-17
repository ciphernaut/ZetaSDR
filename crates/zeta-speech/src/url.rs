//! A Whisper server over HTTP: `POST` of the utterance as `multipart/form-data` to an
//! OpenAI-compatible `/v1/audio/transcriptions` or whisper.cpp's `/inference`, either of
//! which answers JSON with a `text`.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;

use crate::backend::{
    Slots, SpeechBackend, SpeechError, Transcript, check_language, default_languages, normalise,
};
use crate::socket::{STT_CONCURRENCY, STT_QUEUE_WAIT};

/// Longest part of a server's error body carried in the error.
const BODY_SLICE: usize = 200;

pub struct UrlBackend {
    name: String,
    url: Url,
    key: Option<String>,
    model: Option<String>,
    languages: Vec<String>,
    http: reqwest::Client,
    slots: Slots,
    timeout: Duration,
}

impl UrlBackend {
    /// The server at `url`, used as given. `key` is its bearer, `model` the `model` field
    /// sent when the server needs one, `languages` what `GET /stt/languages` answers (`["en"]`
    /// when none are given: the server's own model list is not asked). Fails only on a bad
    /// URL. A call waits at most [`STT_QUEUE_WAIT`] for a slot and as long again for the
    /// server's answer, upload included.
    pub fn new(
        url: &str,
        key: Option<String>,
        model: Option<String>,
        languages: Vec<String>,
    ) -> Result<Self, String> {
        Self::with_limits(
            url,
            key,
            model,
            languages,
            STT_QUEUE_WAIT,
            STT_CONCURRENCY,
            STT_QUEUE_WAIT,
        )
    }

    /// [`UrlBackend::new`] with the call `timeout`, `concurrency` calls at once and a wait of
    /// at most `queue_wait` for a slot (tests shorten them).
    pub fn with_limits(
        url: &str,
        key: Option<String>,
        model: Option<String>,
        languages: Vec<String>,
        timeout: Duration,
        concurrency: usize,
        queue_wait: Duration,
    ) -> Result<Self, String> {
        let url = Url::parse(url).map_err(|e| format!("{url:?}: {e}"))?;
        let Some(host) = url.host_str() else {
            return Err(format!("{url:?}: no host"));
        };
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("{url:?}: not an http or https URL"));
        }
        // The host only: a URL may carry credentials or a token in its query.
        let name = match url.port() {
            Some(port) => format!("url:{host}:{port}"),
            None => format!("url:{host}"),
        };
        let languages: Vec<String> = languages
            .iter()
            .map(|l| normalise(l))
            .filter(|l| !l.is_empty())
            .collect();
        let languages = if languages.is_empty() {
            default_languages()
        } else {
            languages
        };
        Ok(Self {
            name,
            url,
            key: key.filter(|k| !k.is_empty()),
            model: model.filter(|m| !m.is_empty()),
            languages,
            http: reqwest::Client::new(),
            slots: Slots::new(concurrency, queue_wait),
            timeout,
        })
    }

    /// The error text with the key, should the server echo it, blanked.
    fn redact(&self, text: &str) -> String {
        match &self.key {
            Some(k) => text.replace(k.as_str(), "***"),
            None => text.to_string(),
        }
    }

    async fn post(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError> {
        let boundary = format!("zeta-{}", ulid::Ulid::generate());
        let body = multipart(&boundary, wav, self.model.as_deref(), language);
        let mut req = self
            .http
            .post(self.url.clone())
            .timeout(self.timeout)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
        if let Some(key) = &self.key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(|e| self.transport(e))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| self.transport(e))?;
        let text = String::from_utf8_lossy(&bytes);
        let slice: String = text.chars().take(BODY_SLICE).collect();
        if !status.is_success() {
            return Err(SpeechError::Daemon(
                self.redact(&format!("{} answered {status}: {slice}", self.name)),
            ));
        }
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        match v["text"].as_str() {
            Some(t) => Ok(Transcript {
                text: t.trim().to_string(),
                language: language.to_string(),
                queued_ms: 0,
            }),
            None => Err(SpeechError::Daemon(self.redact(&format!(
                "{} answered {status} with no \"text\": {slice}",
                self.name
            )))),
        }
    }

    fn transport(&self, e: reqwest::Error) -> SpeechError {
        if e.is_timeout() {
            SpeechError::Timeout(self.timeout)
        } else {
            // `without_url`: reqwest's text would otherwise repeat the whole URL.
            SpeechError::Unreachable(self.redact(&format!(
                "speech server {}: {}",
                self.name,
                e.without_url()
            )))
        }
    }
}

/// The form: the WAV as `file`, `response_format=json`, and `model` and `language` when given.
fn multipart(boundary: &str, wav: &[u8], model: Option<&str>, language: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(wav.len() + 512);
    let mut field = |name: &str, value: &str| {
        b.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    field("response_format", "json");
    if let Some(m) = model {
        field("model", m);
    }
    field("language", language);
    b.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"utterance.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(wav);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    b
}

#[async_trait]
impl SpeechBackend for UrlBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn languages(&self) -> &[String] {
        &self.languages
    }

    fn queue_wait(&self) -> Duration {
        self.slots.wait()
    }

    async fn transcribe(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError> {
        let language = check_language(&self.languages, language)?;
        let (_slot, queued_ms) = self.slots.acquire().await?;
        let mut t = self.post(wav, &language).await?;
        t.queued_ms = queued_ms;
        Ok(t)
    }
}
