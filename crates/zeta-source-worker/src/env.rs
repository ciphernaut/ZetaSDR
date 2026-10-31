//! The environment a worker is spawned with: the parent's, minus the node's
//! secrets.
//!
//! The worker is our own code and its driver needs the parent's variables
//! (UHD and SoapySDR read `UHD_IMAGES_DIR`, `SOAPY_SDR_PLUGIN_PATH`, `HOME`,
//! `PATH`), so it is not given the cleared environment plugins get. But it
//! never needs the node's secret or the credentials of other sources, and a
//! driver that logs or crashes with its environment in a core dump must not
//! carry them.

use std::ffi::{OsStr, OsString};

/// Variables removed by exact name: the node's secret, the speech node's, and
/// the agent's LLM key.
const SECRET_NAMES: &[&str] = &["ZETA_SECRET", "ZETA_STT_NODE_SECRET", "ZETA_LLM_KEY"];
/// Variables removed by prefix: one per `--source-node` and per
/// `--source-kiwi`.
const SECRET_PREFIXES: &[&str] = &["ZETA_NODE_SECRET_", "ZETA_KIWI_PASSWORD_"];
/// Words that mark any other `ZETA_*` variable as a secret: the lists above
/// name the credentials zeta reads today, and the pattern catches the ones
/// added later before anyone remembers to list them here.
const SECRET_WORDS: &[&str] = &["SECRET", "PASSWORD", "KEY"];

/// The parent's environment minus the secret variables.
pub fn scrubbed() -> Vec<(OsString, OsString)> {
    scrub(std::env::vars_os())
}

/// `vars` minus the secret variables.
pub fn scrub(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    vars.into_iter().filter(|(k, _)| !is_secret(k)).collect()
}

fn is_secret(name: &OsStr) -> bool {
    // A name that is not UTF-8 is none of ours.
    let Some(name) = name.to_str() else {
        return false;
    };
    SECRET_NAMES.contains(&name)
        || SECRET_PREFIXES.iter().any(|p| name.starts_with(p))
        || (name.starts_with("ZETA_") && SECRET_WORDS.iter().any(|w| name.contains(w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_dropped_and_the_rest_kept() {
        let vars = [
            "ZETA_SECRET",
            "ZETA_STT_NODE_SECRET",
            "ZETA_NODE_SECRET_B210",
            "ZETA_KIWI_PASSWORD_K",
            "ZETA_LLM_KEY",
            // Caught by the pattern alone.
            "ZETA_SECRETS_DIR",
            "ZETA_FUTURE_API_KEY",
            "ZETA_ROUTER_PASSWORD",
            "HOME",
            "PATH",
            "UHD_IMAGES_DIR",
            "ZETA_NODE",
            "ZETA_STT_SOCKET",
            // Not ours: the pattern is for `ZETA_*` only.
            "SSH_AUTH_SOCK",
            "GPG_KEY_ID",
        ]
        .map(|k| (OsString::from(k), OsString::from("v")));
        let kept: Vec<String> = scrub(vars)
            .into_iter()
            .map(|(k, _)| k.into_string().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "HOME",
                "PATH",
                "UHD_IMAGES_DIR",
                "ZETA_NODE",
                "ZETA_STT_SOCKET",
                "SSH_AUTH_SOCK",
                "GPG_KEY_ID"
            ]
        );
    }
}
