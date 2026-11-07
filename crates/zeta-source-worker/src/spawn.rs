//! Handing a [`Ring`] to a worker child as it is spawned.
//!
//! The ring's memfd is close-on-exec, so a child sees it only where the spawn
//! puts it: [`inherit_ring`] duplicates it onto [`RING_FD`] between fork and
//! exec, and the child is told `--ring-fd 3` by its caller (the supervisor
//! adds the flag itself; the serve tests pass it). Two forms, one for the
//! node's `tokio::process::Command` and one for a `std::process::Command`, as
//! the tests spawn the fake worker; the tokio one hands its inner std command
//! to the other, so both install the same hook.

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;

use crate::ring::Ring;

/// The descriptor a worker child finds the ring on.
pub const RING_FD: RawFd = 3;

/// Make `cmd`'s child inherit `ring` as [`RING_FD`]. The ring must outlive
/// the spawn.
pub fn inherit_ring(cmd: &mut tokio::process::Command, ring: &Ring) {
    inherit_ring_std(cmd.as_std_mut(), ring);
}

/// [`inherit_ring`] for a `std::process::Command`.
pub fn inherit_ring_std(cmd: &mut std::process::Command, ring: &Ring) {
    let fd = ring.fd().as_raw_fd();
    // SAFETY: the hook runs in the forked child before exec, where only
    // async-signal-safe calls are allowed: it makes `dup2` and `fcntl` calls
    // on plain integers and allocates nothing (the error is built from errno
    // only on failure, as std's own pre-exec handling does).
    unsafe {
        cmd.pre_exec(move || onto_ring_fd(fd));
    }
}

/// Put `fd` on [`RING_FD`] without close-on-exec. `dup2` gives the new
/// descriptor a clear close-on-exec flag, but does nothing when the two are
/// already the same descriptor, so that case clears the flag itself.
fn onto_ring_fd(fd: RawFd) -> io::Result<()> {
    if fd == RING_FD {
        // SAFETY: fcntl on a descriptor this process holds; no pointers.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        // SAFETY: as above.
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }
    // SAFETY: dup2 on descriptors this process holds; no pointers.
    if unsafe { libc::dup2(fd, RING_FD) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
