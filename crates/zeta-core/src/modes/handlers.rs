//! Finding a row's handler programs on this host: `PATH`, an absolute path, or the
//! `ZETA_HANDLER_<PROGRAM>` override. One lookup, shared by the startup probe, the
//! external-program stage and any stage of a graph row that runs a handler.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::ModeRow;

/// The variables that name a row's program instead of a `PATH` search:
/// `ZETA_HANDLER_<PROGRAM>` for `programs[0]` (upper-cased, every other character
/// `_`), then `ZETA_DUMP1090` when that program is `dump1090` (the older name).
pub fn handler_vars(programs: &[String]) -> Vec<String> {
    let Some(first) = programs.first() else {
        return vec![];
    };
    let name: String = first
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let mut vars = vec![format!("ZETA_HANDLER_{name}")];
    if first == "dump1090" {
        vars.push("ZETA_DUMP1090".into());
    }
    vars
}

/// The program a row runs, from this process's environment: see [`find_program`].
pub fn find_handler(programs: &[String]) -> Option<PathBuf> {
    let over = handler_vars(programs)
        .iter()
        .find_map(|v| std::env::var_os(v).filter(|x| !x.is_empty()));
    find_program(programs, over, std::env::var_os("PATH"))
}

/// The program to run: the override `over` when set (it must be an absolute path to an
/// executable file, else there is none: a set override is never passed over for
/// `PATH`), else the first of `programs` that is an absolute path to an executable or
/// an executable in a `path` directory.
pub fn find_program(
    programs: &[String],
    over: Option<OsString>,
    path: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(p) = over.map(PathBuf::from) {
        return (p.is_absolute() && is_executable(&p)).then_some(p);
    }
    programs.iter().find_map(|name| {
        let p = Path::new(name);
        if p.is_absolute() {
            return is_executable(p).then(|| p.to_path_buf());
        }
        if name.contains('/') {
            return None;
        }
        std::env::split_paths(path.as_deref()?)
            .map(|dir| dir.join(name))
            .find(|c| is_executable(c))
    })
}

/// Whether `p` is a file this process may execute (on Unix, any execute bit set).
pub fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        m.is_file()
    }
}

/// The `index`th handler of `row` (a program row's program is its handler 0), found
/// with [`find_handler`]: `Err("<id>: handler <programs[0]> not found (tried <programs
/// joined>)")` when it is not on this host, `Err("<id>: no handler <index>")` when the
/// row has no such handler.
pub fn resolve(row: &ModeRow, index: usize) -> Result<PathBuf, String> {
    let id = &row.id.0;
    let blocks = row.decoder.handler_blocks();
    let (programs, _) = blocks
        .get(index)
        .ok_or_else(|| format!("{id}: no handler {index}"))?;
    find_handler(programs).ok_or_else(|| {
        format!(
            "{id}: handler {} not found (tried {})",
            programs.first().map_or("", String::as_str),
            programs.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_program_honours_override_then_absolute_then_path_order() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let make = |p: &Path| {
            std::fs::write(p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        };
        make(&b.join("readsb"));
        make(&b.join("dump1090-fa"));
        // A non-executable dump1090 earlier in PATH is passed over.
        std::fs::write(a.join("dump1090"), "").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        let progs = names(&["dump1090", "dump1090-fa", "readsb"]);
        assert_eq!(
            find_program(&progs, None, Some(path.clone())),
            Some(b.join("dump1090-fa"))
        );
        let over = b.join("readsb");
        assert_eq!(
            find_program(&progs, Some(over.clone().into()), Some(path.clone())),
            Some(over.clone())
        );
        // A set override that is relative or missing is no program, never PATH's.
        assert_eq!(
            find_program(&progs, Some("readsb".into()), Some(path.clone())),
            None
        );
        assert_eq!(
            find_program(&progs, Some(a.join("nope").into()), Some(path.clone())),
            None
        );
        // An absolute name in the row is used as is.
        let abs = names(&[over.to_str().unwrap()]);
        assert_eq!(find_program(&abs, None, None), Some(over));
        assert_eq!(
            find_program(&progs, None, Some("/nonexistent".into())),
            None
        );
        assert_eq!(find_program(&progs, None, None), None);
    }

    /// An override naming a file that is not executable is missing, as a `PATH`
    /// candidate would be (and is never passed over for `PATH`).
    #[cfg(unix)]
    #[test]
    fn a_non_executable_override_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("dump1090");
        std::fs::write(&plain, "#!/bin/sh\n").unwrap();
        let progs = names(&["dump1090"]);
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(
            find_program(&progs, Some(plain.clone().into()), Some(path)),
            None
        );
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            find_program(&progs, Some(plain.clone().into()), None),
            Some(plain)
        );
    }

    #[test]
    fn handler_vars_map_the_first_program() {
        assert_eq!(
            handler_vars(&names(&["dump1090", "readsb"])),
            ["ZETA_HANDLER_DUMP1090", "ZETA_DUMP1090"]
        );
        assert_eq!(
            handler_vars(&names(&["multimon-ng"])),
            ["ZETA_HANDLER_MULTIMON_NG"]
        );
        assert!(handler_vars(&[]).is_empty());
    }

    /// A handler no host has is named with what was tried; an index past the row's
    /// handlers is named as such. (No `PATH` or `ZETA_HANDLER_*` is set here: the
    /// program's name is one nothing installs.)
    #[test]
    fn resolve_names_the_missing_handler() {
        let mut v: serde_json::Value =
            serde_json::from_str(include_str!("../../../../modes/pocsag_decode.json")).unwrap();
        v["decoder"]["handlers"] = serde_json::json!([{
            "programs": ["zeta-no-such-handler"],
            "install": {"url": "https://example.invalid"}
        }]);
        let row = super::super::parse(&v.to_string()).unwrap();
        assert_eq!(
            resolve(&row, 0).unwrap_err(),
            "pocsag_decode: handler zeta-no-such-handler not found (tried zeta-no-such-handler)"
        );
        assert_eq!(resolve(&row, 1).unwrap_err(), "pocsag_decode: no handler 1");
    }
}
