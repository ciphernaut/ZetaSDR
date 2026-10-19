//! Helpers for tests that run a real `zeta serve --listen` process.
//!
//! Compiled into several test crates, each using a subset.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use zeta_source_file::fixtures::write_tone;

const READY_PREFIX: &str = "zeta: listening on ";
const READY_DEADLINE: Duration = Duration::from_secs(10);

/// A running `zeta serve --listen 127.0.0.1:0`, killed on drop.
pub struct ServeProc {
    pub url: String,
    pub dir: PathBuf,
    child: Child,
}

impl ServeProc {
    pub fn start(dir: &Path, secret: &str) -> ServeProc {
        ServeProc::start_with(dir, secret, &[])
    }

    /// [`ServeProc::start`] with `extra` global flags (e.g. more `--source-file`s).
    pub fn start_with(dir: &Path, secret: &str, extra: &[&str]) -> ServeProc {
        let meta = write_tone(dir);
        let mut child = Command::new(env!("CARGO_BIN_EXE_zeta"))
            .env_remove("ZETA_NODE")
            .env_remove("ZETA_SECRET")
            .arg("--state-dir")
            .arg(dir.join("state"))
            .arg("--source-file")
            .arg(format!("tone={}", meta.display()))
            .args(extra)
            .args(["serve", "--listen", "127.0.0.1:0", "--secret", secret])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn zeta serve");
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        // Forwards every line, then keeps draining so the child never blocks on a full pipe.
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    // Receiver gone: keep reading to drain.
                }
            }
        });
        let deadline = Instant::now() + READY_DEADLINE;
        let mut seen = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(line) => {
                    if let Some(addr) = line.strip_prefix(READY_PREFIX) {
                        return ServeProc {
                            url: format!("http://{}", addr.trim()),
                            dir: dir.to_path_buf(),
                            child,
                        };
                    }
                    seen.push(line);
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("zeta serve not ready in 10 s; stderr so far: {seen:?}");
                }
            }
        }
    }
}

impl Drop for ServeProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `zeta --node <url> --secret <secret> --json <args…>` with the environment cleared of both variables.
pub fn zeta_remote(url: &str, secret: &str, args: &[&str]) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("zeta").unwrap();
    cmd.env_remove("ZETA_NODE")
        .env_remove("ZETA_SECRET")
        .args(["--node", url, "--secret", secret, "--json"])
        .args(args);
    cmd
}
