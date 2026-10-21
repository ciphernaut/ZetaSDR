//! A speech daemon on a Unix socket: the bring-your-own contract.
//!
//! One command line per connection, answered by one line: `file:<absolute path>` (a 16 kHz
//! mono WAV) is answered `ok:<text>` or `error:<message>`; `languages` is answered
//! `ok:<code>,<code>...` by a daemon that knows more than English. `local-voice`'s `voiced`
//! speaks it; so can any other daemon.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Semaphore;

use crate::backend::{
    SpeechBackend, SpeechError, Transcript, check_language, default_languages, parse_languages,
};

/// Longest daemon answer line accepted.
const MAX_REPLY_BYTES: u64 = 64 * 1024;

/// Longest the construction-time `languages` probe waits (or the call timeout, when that is
/// shorter): every local command builds the backend, so a hung daemon must not stall it
/// for a full call timeout.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Calls the daemon is handed at once; the rest wait for a slot.
pub const STT_CONCURRENCY: usize = 4;
/// Longest a call waits for a slot before it is given up as busy.
pub const STT_QUEUE_WAIT: Duration = Duration::from_secs(30);
/// Pause before the one retry of a connect the daemon answered with EAGAIN or refused.
pub const STT_RETRY_DELAY: Duration = Duration::from_millis(200);

pub struct SocketBackend {
    name: String,
    path: PathBuf,
    staging_dir: PathBuf,
    timeout: Duration,
    languages: Vec<String>,
    slots: Arc<Semaphore>,
    queue_wait: Duration,
}

/// Removes the staged WAV file when dropped, on every path out of `transcribe`.
struct StagedFile(PathBuf);

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl SocketBackend {
    /// The daemon at `path`, handed WAVs staged in `staging_dir`, given `timeout` to answer.
    /// Asks the daemon for its languages once, waiting at most [`PROBE_TIMEOUT`]: `["en"]`
    /// when it lists none, answers `error:`, cannot be reached or does not answer in time
    /// (that last one said on stderr).
    pub async fn new(path: PathBuf, staging_dir: PathBuf, timeout: Duration) -> Self {
        Self::with_limits(path, staging_dir, timeout, STT_CONCURRENCY, STT_QUEUE_WAIT).await
    }

    /// [`SocketBackend::new`] with `concurrency` calls at once and a wait of at most
    /// `queue_wait` for a slot (tests shorten the wait).
    pub async fn with_limits(
        path: PathBuf,
        staging_dir: PathBuf,
        timeout: Duration,
        concurrency: usize,
        queue_wait: Duration,
    ) -> Self {
        // The daemon may run in another directory: it is always handed an absolute path.
        let staging_dir = std::path::absolute(&staging_dir).unwrap_or(staging_dir);
        let probe = timeout.min(PROBE_TIMEOUT);
        let languages = match tokio::time::timeout(probe, exchange(&path, "languages")).await {
            Ok(Ok(line)) => line
                .strip_prefix("ok:")
                .map_or_else(default_languages, parse_languages),
            Ok(Err(_)) => default_languages(),
            Err(_) => {
                eprintln!(
                    "zeta: speech daemon at {}: no answer to languages within {} s; assuming en",
                    path.display(),
                    probe.as_secs_f64()
                );
                default_languages()
            }
        };
        Self {
            name: format!("socket:{}", path.display()),
            path,
            staging_dir,
            timeout,
            languages,
            slots: Arc::new(Semaphore::new(concurrency.max(1))),
            queue_wait,
        }
    }
}

#[async_trait]
impl SpeechBackend for SocketBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn languages(&self) -> &[String] {
        &self.languages
    }

    fn queue_wait(&self) -> Duration {
        self.queue_wait
    }

    async fn transcribe(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError> {
        let language = check_language(&self.languages, language)?;
        let asked = Instant::now();
        let _slot = tokio::time::timeout(self.queue_wait, self.slots.acquire())
            .await
            .map_err(|_| SpeechError::Busy(self.queue_wait))?
            .map_err(|_| SpeechError::Daemon("the speech queue closed".into()))?;
        let queued_ms = asked.elapsed().as_millis() as u64;
        let dir = &self.staging_dir;
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| SpeechError::Daemon(format!("creating {}: {e}", dir.display())))?;
        let staged = StagedFile(dir.join(format!("{}.wav", ulid::Ulid::generate())));
        tokio::fs::write(&staged.0, wav)
            .await
            .map_err(|e| SpeechError::Daemon(format!("writing {}: {e}", staged.0.display())))?;
        let command = format!("file:{}", staged.0.display());
        let line = tokio::time::timeout(self.timeout, exchange(&self.path, &command))
            .await
            .map_err(|_| SpeechError::Timeout(self.timeout))??;
        if let Some(text) = line.strip_prefix("ok:") {
            Ok(Transcript {
                text: text.to_string(),
                language,
                queued_ms,
            })
        } else if let Some(msg) = line.strip_prefix("error:") {
            Err(SpeechError::Daemon(msg.to_string()))
        } else {
            Err(SpeechError::Daemon(format!("unexpected answer {line:?}")))
        }
    }
}

/// Send `command` and a newline to the daemon at `path`; its answer line, without the newline.
async fn exchange(path: &Path, command: &str) -> Result<String, SpeechError> {
    let mut conn = match UnixStream::connect(path).await {
        Err(e) if retryable(&e) => {
            tokio::time::sleep(STT_RETRY_DELAY).await;
            UnixStream::connect(path).await
        }
        r => r,
    }
    .map_err(|e| SpeechError::Unreachable(format!("speech daemon at {}: {e}", path.display())))?;
    conn.write_all(format!("{command}\n").as_bytes())
        .await
        .map_err(|e| SpeechError::Daemon(format!("sending the command: {e}")))?;
    let mut line = String::new();
    match BufReader::new(conn.take(MAX_REPLY_BYTES))
        .read_line(&mut line)
        .await
    {
        Ok(0) => return Err(SpeechError::Daemon("closed the connection".into())),
        Ok(_) if !line.ends_with('\n') && line.len() as u64 >= MAX_REPLY_BYTES => {
            return Err(SpeechError::Daemon("answer too long".into()));
        }
        Ok(_) => {}
        Err(e) => return Err(SpeechError::Daemon(format!("reading the answer: {e}"))),
    }
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// A connect failure worth one more try: the daemon's backlog is full (EAGAIN) or it is
/// between accepting sockets.
fn retryable(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ConnectionRefused
    )
}
