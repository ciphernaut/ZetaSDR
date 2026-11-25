//! What every external program the node starts gets: a cleared environment with only
//! [`PASS_ENV`] and the operator's `ZETA_PLUGIN_ENV_<NAME>` variables (so the node's
//! credentials, `ZETA_SECRET` among them, never reach it), and on Unix a process group of
//! its own, `SIGKILL`ed whole ([`Group`]) so nothing it started outlives it. A plugin
//! (`zeta-plugin`, which re-exports these), an external decoder (`zeta-ext`) and a handler
//! program's version probe ([`crate::modes::handlers::probe_version`]) all start theirs
//! so.

/// Host variables passed through to a child program unchanged, when set.
pub const PASS_ENV: [&str; 7] = [
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "PYTHONPATH",
];
/// Host variables `ZETA_PLUGIN_ENV_<NAME>` reach the child program as `<NAME>`.
pub const PLUGIN_ENV_PREFIX: &str = "ZETA_PLUGIN_ENV_";

/// The environment a child program gets from the host's `vars`: [`PASS_ENV`] as is,
/// `ZETA_PLUGIN_ENV_<NAME>` as `<NAME>`, and `PYTHONUNBUFFERED=1` (so a Python plugin's
/// frames are not held in its stdout buffer). Nothing else.
pub fn plugin_env(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<_> = vars
        .filter_map(|(k, v)| {
            let name = k.to_str()?;
            if PASS_ENV.contains(&name) {
                Some((k, v))
            } else {
                let rest = name.strip_prefix(PLUGIN_ENV_PREFIX)?;
                (!rest.is_empty()).then(|| (rest.into(), v))
            }
        })
        .collect();
    env.push(("PYTHONUNBUFFERED".into(), "1".into()));
    env
}

/// A child program's process group (it was started as its leader, `process_group(0)`);
/// `SIGKILL`ed whole when dropped, so nothing the program started outlives the run or
/// probe that spawned it. `None` if the program had exited before its id was read.
pub struct Group(pub Option<u32>);

#[cfg(unix)]
mod sys {
    // `killpg(2)`, declared here rather than taking a `libc` dependency for one call.
    unsafe extern "C" {
        fn killpg(pgrp: i32, sig: i32) -> i32;
    }
    /// `SIGKILL` is 9 on every Unix.
    const SIGKILL: i32 = 9;

    /// `SIGKILL` every process in group `pgrp`; a group already gone is not an error.
    pub(super) fn kill_group(pgrp: u32) {
        // A pid fits i32; 0 or less would mean "my own group" or "everything".
        if let Ok(g) = i32::try_from(pgrp)
            && g > 1
        {
            // SAFETY: killpg takes two integers and touches no memory of ours.
            unsafe {
                killpg(g, SIGKILL);
            }
        }
    }
}

impl Group {
    /// `SIGKILL` the whole group (nothing when its id was never read).
    pub fn kill(&self) {
        #[cfg(unix)]
        if let Some(g) = self.0 {
            sys::kill_group(g);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the listed names, the `ZETA_PLUGIN_ENV_` ones renamed, and
    /// `PYTHONUNBUFFERED`: never a credential.
    #[test]
    fn plugin_env_passes_only_the_listed_names() {
        let vars = [
            ("PATH", "/bin"),
            ("ZETA_SECRET", "s"),
            ("ZETA_LLM_KEY", "k"),
            ("ZETA_PLUGIN_ENV_TOKEN", "t"),
            ("ZETA_PLUGIN_ENV_", "x"),
            ("HOME", "/h"),
        ]
        .map(|(k, v)| (k.into(), v.into()));
        let env = plugin_env(vars.into_iter());
        let got: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| {
                let s = |o: &std::ffi::OsString| o.to_string_lossy().into_owned();
                (s(k), s(v))
            })
            .collect();
        let want = [
            ("PATH", "/bin"),
            ("TOKEN", "t"),
            ("HOME", "/h"),
            ("PYTHONUNBUFFERED", "1"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        assert_eq!(got, want);
    }
}
