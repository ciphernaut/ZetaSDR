//! `state_dir/serve.lock`: marks a state directory as owned by a running
//! `zeta serve`, so one-shot commands do not mutate it underneath. The lock
//! holds the server's pid; a lock whose pid is not alive (no `/proc/<pid>`,
//! so Linux only) is stale and ignored.

use std::io::Write;
use std::path::{Path, PathBuf};

const NAME: &str = "serve.lock";

/// The pid of the live `zeta serve` that owns `state_dir`, if any.
pub fn owner(state_dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(state_dir.join(NAME)).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    Path::new(&format!("/proc/{pid}")).exists().then_some(pid)
}

/// The held lock; dropping it removes the file.
pub struct ServeLock(PathBuf);

impl Drop for ServeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Take the lock for this process, replacing a stale one.
pub fn acquire(state_dir: &Path) -> Result<ServeLock, String> {
    std::fs::create_dir_all(state_dir).map_err(|e| format!("{}: {e}", state_dir.display()))?;
    let path = state_dir.join(NAME);
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                let lock = ServeLock(path.clone());
                write!(f, "{}", std::process::id())
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(lock);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Some(pid) = owner(state_dir) {
                    return Err(format!(
                        "zeta serve (pid {pid}) already owns {}",
                        state_dir.display()
                    ));
                }
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Err(format!("{}: could not take the lock", path.display()))
}
