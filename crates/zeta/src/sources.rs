//! The source and plugin flags: `--source-file`, `--source-soapy`, `--source-kiwi` parse to
//! one [`SourceSpec`]; `--plugin` to a [`PluginSpec`].
//!
//! A Kiwi password never appears in a `SourceSpec`'s `Debug` or `Display`, nor in a parse
//! error: everything after the `/` is replaced by `<redacted>`.

use std::fmt;
use std::path::PathBuf;

/// One source to attach to the node.
#[derive(Clone, PartialEq, Eq)]
pub enum SourceSpec {
    /// `--source-file ID=PATH.sigmf-meta`
    File { id: String, path: PathBuf },
    /// `--source-soapy ID=ARGS`, `ARGS` a SoapySDR kwargs string (`driver=hackrf,serial=…`).
    Soapy { id: String, args: String },
    /// `--source-kiwi ID=HOST:PORT[/PASSWORD]`
    Kiwi {
        id: String,
        host: String,
        port: u16,
        password: Option<String>,
    },
}

impl SourceSpec {
    pub fn id(&self) -> &str {
        match self {
            Self::File { id, .. } | Self::Soapy { id, .. } | Self::Kiwi { id, .. } => id,
        }
    }
}

/// The flag as given, password dropped: `--source-kiwi kiwi0=host:8073`.
impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File { id, path } => write!(f, "--source-file {id}={}", path.display()),
            Self::Soapy { id, args } => write!(f, "--source-soapy {id}={args}"),
            Self::Kiwi { id, host, port, .. } => write!(f, "--source-kiwi {id}={host}:{port}"),
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

/// `ID=PATH.sigmf-meta`.
pub fn parse_file(s: &str) -> Result<SourceSpec, String> {
    let (id, path) =
        split_id(s).ok_or_else(|| format!("expected ID=PATH.sigmf-meta, got {s:?}"))?;
    Ok(SourceSpec::File {
        id: id.into(),
        path: path.into(),
    })
}

/// `ID=ARGS`, split at the first `=`.
pub fn parse_soapy(s: &str) -> Result<SourceSpec, String> {
    let (id, args) = split_id(s).ok_or_else(|| format!("expected ID=ARGS, got {s:?}"))?;
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

/// `ID=HOST:PORT[/PASSWORD]`; the port is required. Errors never contain the password.
pub fn parse_kiwi(s: &str) -> Result<SourceSpec, String> {
    let bad = || format!("expected ID=HOST:PORT[/PASSWORD], got {:?}", redact_kiwi(s));
    let (id, rest) = split_id(s).ok_or_else(bad)?;
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
    Ok(SourceSpec::Kiwi {
        id: id.into(),
        host: host.into(),
        port,
        password,
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
        assert!(parse_soapy("h=").is_err());
        assert!(parse_soapy("h").is_err());
        assert_eq!(
            parse_file("tone=a=b.sigmf-meta"),
            Ok(SourceSpec::File {
                id: "tone".into(),
                path: "a=b.sigmf-meta".into()
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
}
