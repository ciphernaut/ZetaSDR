//! A speech daemon on a Unix socket: the bring-your-own contract.
//!
//! One command line per connection, answered by one line: `file:<absolute path>` (a 16 kHz
//! mono WAV) is answered `ok:<text>` or `error:<message>`; `languages` is answered
//! `ok:<code>,<code>...` by a daemon that knows more than English. `local-voice`'s `voiced`
//! speaks it; so can any other daemon.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::backend::{
    SpeechBackend, SpeechError, Transcript, check_language, default_languages, parse_languages,
};

/// Longest daemon answer line accepted.
const MAX_REPLY_BYTES: u64 = 64 * 1024;

pub struct SocketBackend {
    name: String,
    path: PathBuf,
    staging_dir: PathBuf,
    timeout: Duration,
    languages: Vec<String>,
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
    /// Asks the daemon for its languages once: `["en"]` when it lists none, answers
    /// `error:` or cannot be reached.
    pub async fn new(path: PathBuf, staging_dir: PathBuf, timeout: Duration) -> Self {
        // The daemon may run in another directory: it is always handed an absolute path.
        let staging_dir = std::path::absolute(&staging_dir).unwrap_or(staging_dir);
        let languages = match tokio::time::timeout(timeout, exchange(&path, "languages")).await {
            Ok(Ok(line)) => line
                .strip_prefix("ok:")
                .map_or_else(default_languages, parse_languages),
            _ => default_languages(),
        };
        Self {
            name: format!("socket:{}", path.display()),
            path,
            staging_dir,
            timeout,
            languages,
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

    async fn transcribe(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError> {
        let language = check_language(&self.languages, language)?;
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
    let mut conn = UnixStream::connect(path).await.map_err(|e| {
        SpeechError::Unreachable(format!("speech daemon at {}: {e}", path.display()))
    })?;
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
