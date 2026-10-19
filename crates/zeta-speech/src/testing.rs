//! Test doubles (feature `test-fake`): a fake speech daemon on a Unix socket and a
//! [`FakeBackend`] that answers in process.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::backend::{SpeechBackend, SpeechError, Transcript, check_language, default_languages};

/// One `file:` command the daemon received: the path, and whether that file existed then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedSeen {
    pub path: PathBuf,
    pub existed: bool,
}

type Answer = dyn Fn(&str) -> Option<String> + Send + Sync;

#[derive(Default)]
struct Seen {
    connections: AtomicUsize,
    commands: Mutex<Vec<String>>,
    files: Mutex<Vec<StagedSeen>>,
}

/// A fake daemon speaking the bring-your-own contract on a Unix socket: one command line per
/// connection (`file:<path>` or `languages`), answered by what its answer function returns
/// for that line (without the newline), or never when it returns `None`. Runs on its own
/// threads, so it serves async and blocking tests alike, for as long as the process lives.
pub struct FakeDaemon {
    path: PathBuf,
    seen: Arc<Seen>,
}

impl FakeDaemon {
    /// Serve `path`, answering each command line with `answer(line)` written as is.
    pub fn start(
        path: &Path,
        answer: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self::spawn(path, Arc::new(answer))
    }

    /// Answer every `file:` line with `reply` (written as is: include the newline) and
    /// `languages` with `error:unknown command`, as today's `local-voice` does.
    pub fn replying(path: &Path, reply: &str) -> Self {
        let reply = reply.to_string();
        Self::spawn(
            path,
            Arc::new(move |line: &str| {
                Some(if line == "languages" {
                    "error:unknown command\n".to_string()
                } else {
                    reply.clone()
                })
            }),
        )
    }

    /// [`FakeDaemon::replying`], answering `languages` with `ok:<languages>`.
    pub fn with_languages(path: &Path, reply: &str, languages: &str) -> Self {
        let (reply, languages) = (reply.to_string(), format!("ok:{languages}\n"));
        Self::spawn(
            path,
            Arc::new(move |line: &str| {
                Some(if line == "languages" {
                    languages.clone()
                } else {
                    reply.clone()
                })
            }),
        )
    }

    /// Accept connections and read their command, but never answer.
    pub fn silent(path: &Path) -> Self {
        Self::spawn(path, Arc::new(|_: &str| None))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Connections accepted so far.
    pub fn connections(&self) -> usize {
        self.seen.connections.load(Ordering::SeqCst)
    }

    /// Every command line received, without its newline, in order.
    pub fn commands(&self) -> Vec<String> {
        self.seen.commands.lock().unwrap().clone()
    }

    /// Every `file:` command received, in order.
    pub fn files(&self) -> Vec<StagedSeen> {
        self.seen.files.lock().unwrap().clone()
    }

    fn spawn(path: &Path, answer: Arc<Answer>) -> Self {
        let listener = UnixListener::bind(path).expect("bind fake speech daemon socket");
        let seen = Arc::new(Seen::default());
        let shared = seen.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                shared.connections.fetch_add(1, Ordering::SeqCst);
                let (seen, answer) = (shared.clone(), answer.clone());
                std::thread::spawn(move || serve_one(conn, &seen, &*answer));
            }
        });
        Self {
            path: path.to_path_buf(),
            seen,
        }
    }
}

fn serve_one(mut conn: std::os::unix::net::UnixStream, seen: &Seen, answer: &Answer) {
    let Ok(reader) = conn.try_clone() else { return };
    let mut line = String::new();
    if BufReader::new(reader).read_line(&mut line).is_err() {
        return;
    }
    let line = line.trim_end_matches(['\n', '\r']).to_string();
    if let Some(path) = line.strip_prefix("file:") {
        let path = PathBuf::from(path);
        let existed = path.exists();
        seen.files
            .lock()
            .unwrap()
            .push(StagedSeen { path, existed });
    }
    seen.commands.lock().unwrap().push(line.clone());
    match answer(&line) {
        Some(reply) => {
            let _ = conn.write_all(reply.as_bytes());
        }
        // Hold the connection open, unanswered, until the client gives up.
        None => {
            let _ = std::io::Read::read_to_end(&mut conn, &mut Vec::new());
        }
    }
}

/// An in-process backend: answers `text` in any of its languages (`["en"]` by default),
/// recording every call it accepts.
pub struct FakeBackend {
    text: String,
    languages: Vec<String>,
    calls: Mutex<Vec<(Vec<u8>, String)>>,
    delay: Mutex<Duration>,
    failures: AtomicUsize,
}

impl FakeBackend {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            languages: default_languages(),
            calls: Mutex::new(Vec::new()),
            delay: Mutex::new(Duration::ZERO),
            failures: AtomicUsize::new(0),
        }
    }

    pub fn with_languages(mut self, languages: &[&str]) -> Self {
        self.languages = languages.iter().map(|l| l.to_string()).collect();
        self
    }

    /// Every accepted call: the WAV and the language, in order.
    pub fn calls(&self) -> Vec<(Vec<u8>, String)> {
        self.calls.lock().unwrap().clone()
    }

    /// Wait `delay` before answering each call.
    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    /// Fail the next `n` calls with `Daemon("fake failure")`.
    pub fn fail_next(&self, n: usize) {
        self.failures.store(n, Ordering::SeqCst);
    }
}

#[async_trait]
impl SpeechBackend for FakeBackend {
    fn name(&self) -> &str {
        "fake"
    }

    fn languages(&self) -> &[String] {
        &self.languages
    }

    async fn transcribe(&self, wav: &[u8], language: &str) -> Result<Transcript, SpeechError> {
        let language = check_language(&self.languages, language)?;
        self.calls
            .lock()
            .unwrap()
            .push((wav.to_vec(), language.clone()));
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let failing = self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failing {
            return Err(SpeechError::Daemon("fake failure".to_string()));
        }
        Ok(Transcript {
            text: self.text.clone(),
            language,
        })
    }
}
