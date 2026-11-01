//! The source and plugin flags: `--source-file`, `--source-soapy`, `--source-kiwi`,
//! `--source-node`, `--source-vrt` parse to one [`SourceSpec`]; `--plugin` to a
//! [`PluginSpec`].
//!
//! A Kiwi password never appears in a `SourceSpec`'s `Debug` or `Display`, nor in a parse
//! error: everything after the `/` is replaced by `<redacted>`. Without a `/PASSWORD` part
//! the password comes from `ZETA_KIWI_PASSWORD_<ID>` (see [`kiwi_password_env`]), which
//! keeps it out of `ps`.
//!
//! A remote node's secret is never in a `SourceSpec` at all: it is read from
//! `ZETA_NODE_SECRET_<ID>` (see [`node_secret_env`]) when the port is opened.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use zeta_source_node::NODE_DEFAULT_MAX_BANDWIDTH_HZ;

/// One source to attach to the node.
#[derive(Clone, PartialEq, Eq)]
pub enum SourceSpec {
    /// `--source-file ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`; with the range
    /// the recording is played as a radio tunable over `N..=M` Hz.
    File {
        id: String,
        path: PathBuf,
        tune_range: Option<(u64, u64)>,
    },
    /// `--source-soapy ID=ARGS`, `ARGS` a SoapySDR kwargs string (`driver=hackrf,serial=…`).
    Soapy { id: String, args: String },
    /// `--source-kiwi ID=HOST:PORT[/PASSWORD]`
    Kiwi {
        id: String,
        host: String,
        port: u16,
        password: Option<String>,
    },
    /// `--source-node ID=SOURCE@URL[,max_bandwidth_hz=N]`: source `SOURCE` of the node at
    /// `URL`; the secret comes from [`node_secret_env`].
    Node {
        id: String,
        url: String,
        remote_source: String,
        max_bandwidth_hz: u64,
    },
    /// `--source-vrt ID=udp://ADDR:PORT`: VITA 49.2 packets received on the LAN UDP
    /// address `bind`.
    Vrt { id: String, bind: SocketAddr },
}

impl SourceSpec {
    pub fn id(&self) -> &str {
        match self {
            Self::File { id, .. }
            | Self::Soapy { id, .. }
            | Self::Kiwi { id, .. }
            | Self::Node { id, .. }
            | Self::Vrt { id, .. } => id,
        }
    }
}

/// The flag as given, password dropped: `--source-kiwi kiwi0=host:8073`; a node source's
/// cap only when it is not the default.
impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File {
                id,
                path,
                tune_range,
            } => {
                write!(f, "--source-file {id}={}", path.display())?;
                if let Some((min, max)) = tune_range {
                    write!(f, ",tune_min_hz={min},tune_max_hz={max}")?;
                }
                Ok(())
            }
            Self::Soapy { id, args } => write!(f, "--source-soapy {id}={args}"),
            Self::Kiwi { id, host, port, .. } => write!(f, "--source-kiwi {id}={host}:{port}"),
            Self::Node {
                id,
                url,
                remote_source,
                max_bandwidth_hz,
            } => {
                write!(f, "--source-node {id}={remote_source}@{url}")?;
                if *max_bandwidth_hz != NODE_DEFAULT_MAX_BANDWIDTH_HZ {
                    write!(f, ",max_bandwidth_hz={max_bandwidth_hz}")?;
                }
                Ok(())
            }
            Self::Vrt { id, bind } => write!(f, "--source-vrt {id}=udp://{bind}"),
        }
    }
}

impl fmt::Debug for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kiwi { password, .. } => write!(
                f,
                "{self}{}",
                if password.is_some() {
                    "/<redacted>"
                } else {
                    ""
                }
            ),
            _ => write!(f, "{self}"),
        }
    }
}

/// `ID=REST` with both sides non-empty.
fn split_id(s: &str) -> Option<(&str, &str)> {
    s.split_once('=')
        .filter(|(id, rest)| !id.is_empty() && !rest.is_empty())
}

/// A source may not be named like the node's own placeholder source, which a mix's record
/// carries.
fn refuse_reserved(id: &str) -> Result<(), String> {
    if id == zeta_core::node::NODE_SOURCE {
        return Err(format!(
            "source id {id:?} is reserved for the node's own streams"
        ));
    }
    Ok(())
}

/// `ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M]`: the options start at the first
/// `,tune_` (a path may hold commas otherwise); both bounds are required, integers in Hz,
/// `N < M`. Errors name the bad part and show the expected form.
pub fn parse_file(s: &str) -> Result<SourceSpec, String> {
    let bad = |part: String| {
        format!("{part}; expected ID=PATH.sigmf-meta[,tune_min_hz=N,tune_max_hz=M], got {s:?}")
    };
    let (id, rest) = split_id(s).ok_or_else(|| bad("missing ID=".into()))?;
    refuse_reserved(id)?;
    let (path, opts) = match rest.find(",tune_") {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    if path.is_empty() {
        return Err(bad("PATH is empty".into()));
    }
    let mut tune_range = None;
    if let Some(opts) = opts {
        let (mut min, mut max) = (None, None);
        for opt in opts.split(',') {
            let (key, n) = opt
                .split_once('=')
                .ok_or_else(|| bad(format!("unknown option {opt:?}")))?;
            let slot = match key {
                "tune_min_hz" => &mut min,
                "tune_max_hz" => &mut max,
                _ => return Err(bad(format!("unknown option {opt:?}"))),
            };
            *slot = Some(
                n.parse::<u64>()
                    .map_err(|_| bad(format!("{key} {n:?} is not an integer")))?,
            );
        }
        match (min, max) {
            (Some(min), Some(max)) if min < max => tune_range = Some((min, max)),
            (Some(_), Some(_)) => return Err(bad("tune_min_hz must be below tune_max_hz".into())),
            _ => return Err(bad("tune_min_hz and tune_max_hz go together".into())),
        }
    }
    Ok(SourceSpec::File {
        id: id.into(),
        path: path.into(),
        tune_range,
    })
}

/// `ID=ARGS`, split at the first `=`.
pub fn parse_soapy(s: &str) -> Result<SourceSpec, String> {
    let (id, args) = split_id(s).ok_or_else(|| format!("expected ID=ARGS, got {s:?}"))?;
    refuse_reserved(id)?;
    Ok(SourceSpec::Soapy {
        id: id.into(),
        args: args.into(),
    })
}

/// `s` with everything after the first `/` replaced, for error messages.
fn redact_kiwi(s: &str) -> String {
    match s.split_once('/') {
        Some((head, _)) => format!("{head}/<redacted>"),
        None => s.into(),
    }
}

/// `id` upper-cased with every non-alphanumeric character mapped to `_`.
fn env_suffix(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The variable holding source `id`'s Kiwi password: `ZETA_KIWI_PASSWORD_<ID>`, `ID`
/// upper-cased with every non-alphanumeric character mapped to `_`.
pub fn kiwi_password_env(id: &str) -> String {
    format!("ZETA_KIWI_PASSWORD_{}", env_suffix(id))
}

/// `ID=HOST:PORT[/PASSWORD]`; the port is required. Without `/PASSWORD` the password is
/// [`kiwi_password_env`] when set and non-empty; the flag wins when both are given.
/// Errors never contain the password.
pub fn parse_kiwi(s: &str) -> Result<SourceSpec, String> {
    parse_kiwi_with(s, |k| std::env::var(k).ok())
}

/// [`parse_kiwi`] with the environment lookup given.
fn parse_kiwi_with(s: &str, env: impl Fn(&str) -> Option<String>) -> Result<SourceSpec, String> {
    let bad = || format!("expected ID=HOST:PORT[/PASSWORD], got {:?}", redact_kiwi(s));
    let (id, rest) = split_id(s).ok_or_else(bad)?;
    refuse_reserved(id)?;
    let (addr, password) = match rest.split_once('/') {
        Some((a, p)) if !p.is_empty() => (a, Some(p.to_owned())),
        Some(_) => return Err(bad()),
        None => (rest, None),
    };
    let (host, port) = addr.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().map_err(|_| bad())?;
    if host.is_empty() || port == 0 {
        return Err(bad());
    }
    let password = password.or_else(|| env(&kiwi_password_env(id)).filter(|p| !p.is_empty()));
    Ok(SourceSpec::Kiwi {
        id: id.into(),
        host: host.into(),
        port,
        password,
    })
}

/// The variable holding node source `id`'s secret: `ZETA_NODE_SECRET_<ID>`, mapped as
/// [`kiwi_password_env`].
pub fn node_secret_env(id: &str) -> String {
    format!("ZETA_NODE_SECRET_{}", env_suffix(id))
}

/// Node source `id`'s secret from [`node_secret_env`]; unset or empty is an error naming
/// the variable.
pub fn node_secret(id: &str) -> Result<String, String> {
    node_secret_with(id, |k| std::env::var(k).ok())
}

/// [`node_secret`] with the environment lookup given.
fn node_secret_with(id: &str, env: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let var = node_secret_env(id);
    env(&var)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("--source-node {id}: set {var}"))
}

/// Largest `max_bandwidth_hz` a `--source-node` accepts.
const NODE_MAX_BANDWIDTH_HZ: u64 = 20_000_000;

/// `ID=SOURCE@URL[,max_bandwidth_hz=N]`: `URL` is `http://HOST:PORT`, `N` in
/// 1..=20 000 000 (default [`NODE_DEFAULT_MAX_BANDWIDTH_HZ`]). Errors name the bad part
/// and show the expected form.
pub fn parse_node(s: &str) -> Result<SourceSpec, String> {
    let bad =
        |part: String| format!("{part}; expected ID=SOURCE@URL[,max_bandwidth_hz=N], got {s:?}");
    let (id, rest) = split_id(s).ok_or_else(|| bad("missing ID=".into()))?;
    refuse_reserved(id)?;
    let (target, opts) = match rest.split_once(',') {
        Some((t, o)) => (t, Some(o)),
        None => (rest, None),
    };
    let (remote, url) = target
        .split_once('@')
        .ok_or_else(|| bad(format!("{target:?} is not SOURCE@URL")))?;
    if remote.is_empty() {
        return Err(bad("SOURCE is empty".into()));
    }
    let Some(authority) = url.strip_prefix("http://") else {
        return Err(bad(format!("URL {url:?} must start with http://")));
    };
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let ok = authority.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty() && !host.contains('/') && port.parse::<u16>().is_ok_and(|p| p != 0)
    });
    if !ok {
        return Err(bad(format!("URL {url:?} is not http://HOST:PORT")));
    }
    let mut max_bandwidth_hz = NODE_DEFAULT_MAX_BANDWIDTH_HZ;
    for opt in opts.into_iter().flat_map(|o| o.split(',')) {
        match opt.split_once('=') {
            Some(("max_bandwidth_hz", n)) => {
                max_bandwidth_hz = n
                    .parse()
                    .ok()
                    .filter(|n| (1..=NODE_MAX_BANDWIDTH_HZ).contains(n))
                    .ok_or_else(|| {
                        bad(format!(
                            "max_bandwidth_hz {n:?} is not an integer in 1..=20000000"
                        ))
                    })?;
            }
            _ => return Err(bad(format!("unknown option {opt:?}"))),
        }
    }
    Ok(SourceSpec::Node {
        id: id.into(),
        url: url.strip_suffix('/').unwrap_or(url).into(),
        remote_source: remote.into(),
        max_bandwidth_hz,
    })
}

/// `ID=udp://ADDR:PORT`: `ADDR` an IP literal on the LAN (loopback, private, link-local,
/// CGNAT or unspecified, as `serve --listen`), IPv6 in brackets; `PORT` not 0. Errors name
/// the bad part and show the expected form.
pub fn parse_vrt(s: &str) -> Result<SourceSpec, String> {
    let bad = |part: String| format!("{part}; expected ID=udp://ADDR:PORT, got {s:?}");
    let (id, url) = split_id(s).ok_or_else(|| bad("missing ID=".into()))?;
    refuse_reserved(id)?;
    let Some(addr) = url.strip_prefix("udp://") else {
        return Err(bad(format!("{url:?} must start with udp://")));
    };
    let bind: SocketAddr = addr
        .parse()
        .map_err(|_| bad(format!("{addr:?} is not an IP ADDR:PORT")))?;
    if bind.port() == 0 {
        return Err(bad("PORT must not be 0".into()));
    }
    if !zeta_ws::addr::is_lan(bind.ip()) {
        return Err(bad(format!(
            "{} is not a LAN address; ZetaSDR is LAN only",
            bind.ip()
        )));
    }
    Ok(SourceSpec::Vrt {
        id: id.into(),
        bind,
    })
}

/// `--plugin KIND=PATH`: register the program at `PATH` (absolute) as stage kind `KIND`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSpec {
    pub kind: String,
    pub program: PathBuf,
}

impl fmt::Display for PluginSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "--plugin {}={}", self.kind, self.program.display())
    }
}

/// `KIND=PATH`, `KIND` matching `^[a-z][a-z0-9_]*$`.
pub fn parse_plugin(s: &str) -> Result<PluginSpec, String> {
    let (kind, path) = split_id(s).ok_or_else(|| format!("expected KIND=PATH, got {s:?}"))?;
    let mut chars = kind.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(format!("plugin kind {kind:?} must match ^[a-z][a-z0-9_]*$"));
    }
    Ok(PluginSpec {
        kind: kind.into(),
        program: path.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kiwi(id: &str, host: &str, port: u16, pw: Option<&str>) -> SourceSpec {
        SourceSpec::Kiwi {
            id: id.into(),
            host: host.into(),
            port,
            password: pw.map(Into::into),
        }
    }

    #[test]
    fn parse_table() {
        assert_eq!(
            parse_kiwi("kiwi0=kiwisdr.local:8073"),
            Ok(kiwi("kiwi0", "kiwisdr.local", 8073, None))
        );
        assert_eq!(parse_kiwi("k=h:1/pw"), Ok(kiwi("k", "h", 1, Some("pw"))));
        assert_eq!(
            parse_kiwi("k=[::1]:8073"),
            Ok(kiwi("k", "[::1]", 8073, None))
        );
        for bad in [
            "k=h",
            "k=h:",
            "k=h:x",
            "k=:1",
            "=h:1",
            "k=h:1/",
            "k=h:0",
            "k=h:70000",
        ] {
            let e = parse_kiwi(bad).unwrap_err();
            assert!(e.contains("expected ID=HOST:PORT"), "{bad}: {e}");
        }
        assert_eq!(
            parse_soapy("hackrf0=driver=hackrf,serial=ab"),
            Ok(SourceSpec::Soapy {
                id: "hackrf0".into(),
                args: "driver=hackrf,serial=ab".into()
            })
        );
        for e in [
            parse_file("node=a.sigmf-meta").unwrap_err(),
            parse_soapy("node=driver=x").unwrap_err(),
            parse_kiwi("node=h:1").unwrap_err(),
            parse_node("node=s@http://h:1").unwrap_err(),
            parse_vrt("node=udp://127.0.0.1:4991").unwrap_err(),
        ] {
            assert_eq!(
                e,
                "source id \"node\" is reserved for the node's own streams"
            );
        }
        assert!(parse_plugin("node=/x").is_ok());
        assert!(parse_soapy("h=").is_err());
        assert!(parse_soapy("h").is_err());
        assert_eq!(
            parse_file("tone=a=b.sigmf-meta"),
            Ok(SourceSpec::File {
                id: "tone".into(),
                path: "a=b.sigmf-meta".into(),
                tune_range: None,
            })
        );
        assert_eq!(
            parse_file("r=a,b.sigmf-meta,tune_min_hz=5,tune_max_hz=9"),
            Ok(SourceSpec::File {
                id: "r".into(),
                path: "a,b.sigmf-meta".into(),
                tune_range: Some((5, 9)),
            })
        );
        assert_eq!(
            parse_plugin("gain=examples/python/gain.py"),
            Ok(PluginSpec {
                kind: "gain".into(),
                program: "examples/python/gain.py".into()
            })
        );
        assert!(parse_plugin("g_2=x").is_ok());
        for bad in ["Gain=x", "2g=x", "g-x=x", "gain=", "=x", "gain"] {
            assert!(parse_plugin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn kiwi_password_env_name() {
        assert_eq!(kiwi_password_env("kiwi0"), "ZETA_KIWI_PASSWORD_KIWI0");
        assert_eq!(
            kiwi_password_env("my-kiwi.2"),
            "ZETA_KIWI_PASSWORD_MY_KIWI_2"
        );
    }

    #[test]
    fn kiwi_password_from_env_when_flag_has_none() {
        let env = |k: &str| (k == "ZETA_KIWI_PASSWORD_MY_KIWI").then(|| "hunter2".to_owned());
        let s = parse_kiwi_with("my-kiwi=h:1", env).unwrap();
        assert_eq!(s, kiwi("my-kiwi", "h", 1, Some("hunter2")));
        assert_eq!(s.to_string(), "--source-kiwi my-kiwi=h:1");
        assert!(!format!("{s:?}").contains("hunter2"));
        // The flag wins over the environment.
        let s = parse_kiwi_with("my-kiwi=h:1/flagpw", env).unwrap();
        assert_eq!(s, kiwi("my-kiwi", "h", 1, Some("flagpw")));
        // An empty variable is no password.
        let empty = |_: &str| Some(String::new());
        assert_eq!(parse_kiwi_with("k=h:1", empty), Ok(kiwi("k", "h", 1, None)));
    }

    #[test]
    fn password_is_never_shown() {
        let s = parse_kiwi("k=h:1/hunter2").unwrap();
        assert_eq!(s.id(), "k");
        assert_eq!(s.to_string(), "--source-kiwi k=h:1");
        assert_eq!(format!("{s:?}"), "--source-kiwi k=h:1/<redacted>");
        let e = parse_kiwi("k=h/hunter2").unwrap_err();
        assert!(!e.contains("hunter2"), "{e}");
        let e = parse_kiwi("k=h:x/hunter2").unwrap_err();
        assert!(!e.contains("hunter2"), "{e}");
    }

    fn node(id: &str, url: &str, remote: &str, cap: u64) -> SourceSpec {
        SourceSpec::Node {
            id: id.into(),
            url: url.into(),
            remote_source: remote.into(),
            max_bandwidth_hz: cap,
        }
    }

    #[test]
    fn node_spec_parses_and_defaults_cap() {
        let s = parse_node("pi=hack@http://pi.local:7979").unwrap();
        assert_eq!(s, node("pi", "http://pi.local:7979", "hack", 250_000));
        assert_eq!(s.id(), "pi");
        assert_eq!(s.to_string(), "--source-node pi=hack@http://pi.local:7979");
        assert_eq!(format!("{s:?}"), s.to_string());
    }

    #[test]
    fn node_spec_accepts_cap_and_rejects_bad_parts() {
        let s = parse_node("pi=hack@http://10.0.0.5:7979,max_bandwidth_hz=2000000").unwrap();
        assert_eq!(s, node("pi", "http://10.0.0.5:7979", "hack", 2_000_000));
        assert_eq!(
            s.to_string(),
            "--source-node pi=hack@http://10.0.0.5:7979,max_bandwidth_hz=2000000"
        );
        assert!(parse_node("pi=hack@http://h:1,max_bandwidth_hz=20000000").is_ok());
        for (bad, part) in [
            ("pi=http://x", "SOURCE@URL"),
            ("pi", "ID="),
            ("=hack@http://h:1", "ID="),
            ("pi=@http://h:1", "SOURCE"),
            ("pi=hack@", "URL"),
            ("pi=hack@https://h:1", "http://"),
            ("pi=hack@h:1", "http://"),
            ("pi=hack@http://h", "URL"),
            ("pi=hack@http://:1", "URL"),
            ("pi=hack@http://h:0", "URL"),
            ("pi=hack@http://h:x", "URL"),
            ("pi=hack@http://h:1,max_bandwidth_hz=0", "max_bandwidth_hz"),
            (
                "pi=hack@http://h:1,max_bandwidth_hz=20000001",
                "max_bandwidth_hz",
            ),
            ("pi=hack@http://h:1,max_bandwidth_hz=x", "max_bandwidth_hz"),
            ("pi=hack@http://h:1,gain=3", "gain"),
            ("pi=hack@http://h:1,", "option"),
        ] {
            let e = parse_node(bad).unwrap_err();
            assert!(e.contains(part), "{bad}: {e}");
            assert!(
                e.contains("ID=SOURCE@URL[,max_bandwidth_hz=N]"),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn vrt_spec_parses_lan_addresses_only() {
        let s = parse_vrt("v=udp://127.0.0.1:4991").unwrap();
        assert_eq!(
            s,
            SourceSpec::Vrt {
                id: "v".into(),
                bind: "127.0.0.1:4991".parse().unwrap()
            }
        );
        assert_eq!(s.id(), "v");
        assert_eq!(s.to_string(), "--source-vrt v=udp://127.0.0.1:4991");
        for ok in [
            "v=udp://0.0.0.0:4991",
            "v=udp://192.168.1.5:4991",
            "v=udp://10.0.0.1:1",
            "v=udp://169.254.0.9:4991",
            "v=udp://[::1]:4991",
            "v=udp://[fe80::1]:4991",
        ] {
            assert!(parse_vrt(ok).is_ok(), "{ok}");
        }
        for (bad, part) in [
            ("v", "ID="),
            ("=udp://127.0.0.1:1", "ID="),
            ("v=127.0.0.1:1", "udp://"),
            ("v=tcp://127.0.0.1:1", "udp://"),
            ("v=udp://127.0.0.1", "ADDR:PORT"),
            ("v=udp://host.local:1", "ADDR:PORT"),
            ("v=udp://127.0.0.1:x", "ADDR:PORT"),
            ("v=udp://127.0.0.1:0", "PORT"),
            ("v=udp://8.8.8.8:4991", "not a LAN address"),
            ("v=udp://[2001:db8::1]:4991", "not a LAN address"),
        ] {
            let e = parse_vrt(bad).unwrap_err();
            assert!(e.contains(part), "{bad}: {e}");
            assert!(e.contains("ID=udp://ADDR:PORT"), "{bad}: {e}");
        }
    }

    #[test]
    fn node_secret_env_name() {
        assert_eq!(node_secret_env("pi-5"), "ZETA_NODE_SECRET_PI_5");
        assert_eq!(node_secret_env("pi"), "ZETA_NODE_SECRET_PI");
    }

    #[test]
    fn node_secret_from_env_or_usage_error() {
        let env = |k: &str| (k == "ZETA_NODE_SECRET_PI").then(|| "s3cret-long-enough".to_owned());
        assert_eq!(
            node_secret_with("pi", env).as_deref(),
            Ok("s3cret-long-enough")
        );
        let e = node_secret_with("pi", |_| None).unwrap_err();
        assert_eq!(e, "--source-node pi: set ZETA_NODE_SECRET_PI");
        let e = node_secret_with("pi", |_| Some(String::new())).unwrap_err();
        assert!(e.contains("ZETA_NODE_SECRET_PI"), "{e}");
    }
}
