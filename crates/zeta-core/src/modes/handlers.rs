//! Finding a row's programs (a program row's own, a graph row's helper programs) and a
//! handler file's program on this host: `PATH`, an absolute path, or the
//! `ZETA_HANDLER_<PROGRAM>` override. One lookup, shared by the startup probe, the
//! external-program stage and any stage of a graph row that runs a helper program. The
//! lookup never runs the program; only a block naming the least version its program must
//! report ([`VersionProbe`]) has the probe run the program found, once, to ask it
//! ([`probe_version`]).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use super::{Decoder, Install, ModeRegistry, ModeRow, ModeState, VersionProbe};

/// How long a version probe waits for the program's answer before it stops it.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// The most bytes a version probe reads of the program's stdout, and of its stderr.
pub const VERSION_READ_CAP: usize = 4096;

/// A program as a probe found it: its path, size and modified time. A carried probe of
/// a block with a version floor is kept only while the program found now has the same
/// ([`ModeRegistry::carry_programs`]), so installing another and reloading the modes
/// is enough to probe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramStamp {
    pub path: PathBuf,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl ProgramStamp {
    /// `path`'s stamp; `None` when it cannot be read.
    pub fn of(path: &Path) -> Option<ProgramStamp> {
        let m = std::fs::metadata(path).ok()?;
        Some(ProgramStamp {
            path: path.to_path_buf(),
            len: m.len(),
            modified: m.modified().ok(),
        })
    }

    /// The stamp of the program [`find_handler`] finds for `programs` now; `None` when
    /// none is found.
    pub fn found(programs: &[String]) -> Option<ProgramStamp> {
        find_handler(programs).and_then(|p| ProgramStamp::of(&p))
    }
}

/// What probing one program list found: the program to run (`None` when it is missing
/// or refused), why it is not ready (`None` when it is), the version it reported (with a
/// version block) and the program's stamp (`None` when missing).
struct Probed {
    program: Option<PathBuf>,
    reason: Option<String>,
    version: Option<String>,
    stamp: Option<ProgramStamp>,
    line: String,
}

/// Probe `programs` for `served` (the mode ids that need it, for the report): look it up
/// and, with `version`, ask the program found its version ([`check_version`]). `named`
/// is what the found line adds after the path (`handler <id>`, or nothing).
fn probe_one(
    programs: &[String],
    install: &Install,
    version: Option<&VersionProbe>,
    served: &[String],
    named: Option<&str>,
) -> Probed {
    let first = programs.first().cloned().unwrap_or_default();
    let modes = served.join(", ");
    let Some(path) = find_handler(programs) else {
        let hint = install.hint().map(|h| format!("{h}; ")).unwrap_or_default();
        let reason = format!("{first}: missing ({hint}{}) — modes: {modes}", install.url);
        return Probed {
            program: None,
            line: format!("zeta: handlers: {reason}"),
            reason: Some(reason),
            version: None,
            stamp: None,
        };
    };
    let stamp = ProgramStamp::of(&path);
    let read = match version {
        None => None,
        Some(v) => match check_version(&first, &path, v, install) {
            Ok(read) => Some(read),
            Err(why) => {
                let reason = format!("{why} — modes: {modes}");
                return Probed {
                    program: None,
                    line: format!("zeta: handlers: {reason}"),
                    reason: Some(reason),
                    version: None,
                    stamp,
                };
            }
        },
    };
    let said: Vec<String> = named
        .map(String::from)
        .into_iter()
        .chain(read.as_ref().map(|v| format!("version {v}")))
        .collect();
    let said = match said.is_empty() {
        true => String::new(),
        false => format!(" ({})", said.join(", ")),
    };
    Probed {
        line: format!("zeta: handlers: {first}: found {}{said}", path.display()),
        program: Some(path),
        reason: None,
        version: read,
        stamp,
    }
}

/// Look up the program of each ready handler file whose handlers are not probed yet
/// (`ready` with no program: as loaded, and not carried over a reload by
/// [`ModeRegistry::carry_programs`]), one probe per file, and record it in every such
/// handler ([`ModeRegistry::set_handler_program`], [`ModeRegistry::set_version_read`]):
/// found, they are ready with its path; missing, unavailable with the report line as
/// their reason. A file without a `version` block runs nothing. With one, the program
/// found is run once to ask its version ([`check_version`]): one too old, or one whose
/// version cannot be read, makes them unavailable with that reason. Returns one line per
/// file looked up: `zeta: handlers: <programs[0]>: found <path> (handler <id>)` (`,
/// version <v>` added with a block), or `zeta: handlers: <reason>`, the reason being
/// `<programs[0]>: missing (<hint>; <url>) — modes: <ids>` as for a row's program (the
/// hint left out, with its `; `, when the file names no script or package), or the
/// version's reason then ` — modes: <ids>`.
pub fn probe_handler_files(modes: &mut ModeRegistry) -> Vec<String> {
    let files: Vec<_> = modes
        .handler_files()
        .iter()
        .filter_map(|f| f.handler.as_ref())
        .map(|h| {
            let (id, programs, install) = (h.id.clone(), h.programs.clone(), h.install.clone());
            (id, programs, install, h.version.clone())
        })
        .collect();
    let mut lines = Vec::new();
    for (id, programs, install, version) in files {
        let unprobed = |h: &super::ModeHandler| {
            h.id == id && h.state == ModeState::Ready && h.program.is_none()
        };
        let served: Vec<String> = modes
            .all()
            .iter()
            .filter(|m| m.handlers.iter().skip(1).any(unprobed))
            .map(|m| m.row.id.0.clone())
            .collect();
        if served.is_empty() {
            continue;
        }
        let named = format!("handler {id}");
        let p = probe_one(&programs, &install, version.as_ref(), &served, Some(&named));
        lines.push(p.line);
        for mode in &served {
            modes.set_handler_program(mode, &id, p.program.clone(), p.reason.clone());
            modes.set_version_read(mode, &id, p.version.clone(), p.stamp.clone());
        }
    }
    lines
}

/// The version block of `row`'s own program: a program row's `decoder.version`; `None`
/// for any other row, and a program row without one.
pub fn row_version(row: &ModeRow) -> Option<&VersionProbe> {
    match &row.decoder {
        Some(Decoder::Program(p)) => p.version.as_ref(),
        _ => None,
    }
}

/// Probe each program row with a `version` block not probed yet (`ready` with no
/// program: as loaded, and not carried over a reload), one row at a time, as
/// [`probe_handler_files`] probes a file's program, and record it
/// ([`ModeRegistry::set_program`], [`ModeRegistry::set_version_read`]). The other rows'
/// programs are the CLI's startup probe's (they are looked up, not run, here: never).
/// Returns one line per row probed: `zeta: handlers: <programs[0]>: found <path>
/// (version <v>)`, or `zeta: handlers: <reason>` (` — modes: <id>` ending it).
pub fn probe_row_versions(modes: &mut ModeRegistry) -> Vec<String> {
    let rows: Vec<_> = modes
        .all()
        .iter()
        .filter(|m| m.state == ModeState::Ready && m.program.is_none())
        .filter_map(|m| {
            let Some(Decoder::Program(p)) = &m.row.decoder else {
                return None;
            };
            let v = p.version.clone()?;
            Some((
                m.row.id.0.clone(),
                m.own_handler().to_string(),
                p.clone(),
                v,
            ))
        })
        .collect();
    let mut lines = Vec::new();
    for (id, own, block, v) in rows {
        let p = probe_one(
            &block.programs,
            &block.install,
            Some(&v),
            std::slice::from_ref(&id),
            None,
        );
        lines.push(p.line);
        modes.set_program(&id, p.program, p.reason);
        modes.set_version_read(&id, &own, p.version, p.stamp);
    }
    lines
}

/// Ask the program at `path` its version (`v`) and hold it to `v.min`: `Ok(<version
/// read>)` at or over it, else the reason for people: `<name> <version> is older than
/// <min> (<hint>; <url>)` (the hint left out, with its `; `, when `install` names no
/// script or package), or `<name>: its version could not be read (<what happened>)`.
/// `name` is the block's `programs[0]`, as the found and missing lines name it, whatever
/// path (an override's, say) the program was found at.
pub fn check_version(
    name: &str,
    path: &Path,
    v: &VersionProbe,
    install: &Install,
) -> Result<String, String> {
    let unread = |what: String| format!("{name}: its version could not be read ({what})");
    let read = probe_version(path, v).map_err(unread)?;
    if dotted_numbers(&read).is_none() {
        return Err(unread(format!("it reported {read:?}, not dotted numbers")));
    }
    if !version_at_least(&read, &v.min) {
        let hint = install.hint().map(|h| format!("{h}; ")).unwrap_or_default();
        return Err(format!(
            "{name} {read} is older than {} ({hint}{})",
            v.min, install.url
        ));
    }
    Ok(read)
}

/// How far a reader of a probed program's pipe got.
const READING: u8 = 0;
const AT_END: u8 = 1;
const AT_CAP: u8 = 2;

/// Run `program` once with `v.args` (no shell, stdin closed, the cleared environment of
/// [`crate::child::plugin_env`], its own process group) and read its version: the
/// first group of `v.pattern`'s first match in its stdout, else its stderr. At most
/// [`VERSION_READ_CAP`] bytes of each are read: a program that writes more is stopped
/// there and only what was read is matched. `Err` says what happened: it could not be
/// run, gave no answer within [`VERSION_TIMEOUT`] (it is then killed), exited non-zero or
/// by a signal, printed nothing, or printed nothing the pattern matches.
pub fn probe_version(program: &Path, v: &VersionProbe) -> Result<String, String> {
    probe_version_within(program, v, VERSION_TIMEOUT)
}

fn probe_version_within(
    program: &Path,
    v: &VersionProbe,
    timeout: Duration,
) -> Result<String, String> {
    use std::process::{Command, Stdio};
    let re = regex::Regex::new(&v.pattern)
        .map_err(|e| format!("its match is not a regular expression: {e}"))?;
    let mut cmd = Command::new(program);
    cmd.args(&v.args)
        .env_clear()
        .envs(crate::child::plugin_env(std::env::vars_os()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("it could not be run: {e}"))?;
    // The group id is the leader's pid (`process_group(0)`). Killed whole on every way
    // out, so nothing the program started outlives the probe or holds its pipes.
    let mut group = crate::child::Group(Some(child.id()));
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    // Each pipe read on a thread of its own, what it read kept where the probe can take
    // it whenever it stops waiting: a program that leaves a child of its own holding the
    // pipe open never holds the probe past its timeout (and the group kill ends that
    // child, so its reader ends too).
    let readers = pipes.map(|pipe| {
        let got = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(AtomicU8::new(AT_END));
        if let Some(pipe) = pipe {
            state.store(READING, Ordering::Release);
            let (got, state) = (got.clone(), state.clone());
            std::thread::spawn(move || {
                let end = read_capped(pipe, &got);
                state.store(end, Ordering::Release);
            });
        }
        (got, state)
    });
    let deadline = Instant::now() + timeout;
    let mut status = None;
    let cut = loop {
        if status.is_none() {
            status = child.try_wait().ok().flatten();
        }
        let states = readers.each_ref().map(|(_, s)| s.load(Ordering::Acquire));
        if states.contains(&AT_CAP) {
            break true;
        }
        if status.is_some() && states.iter().all(|s| *s == AT_END) {
            break false;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The whole group, before a leader still running is reaped (its id is the group's
    // until then), and once only: the id is let go so the drop does not send it again.
    group.kill();
    group.0 = None;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Stopped at the cap: what it read is all there is, and how it ended says nothing.
    if !cut {
        let Some(status) = status else {
            return Err(format!("no answer within {} s", timeout.as_secs_f64()));
        };
        if !status.success() {
            return Err(exit_text(status));
        }
    }
    let texts = readers.map(|(got, _)| {
        let got = got.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&got).into_owned()
    });
    if let Some(version) = texts
        .iter()
        .find_map(|t| Some(re.captures(t)?.get(1)?.as_str().to_string()))
    {
        return Ok(version);
    }
    Err(match (cut, texts.iter().all(|t| t.trim().is_empty())) {
        (true, _) => format!(
            "nothing in the first {VERSION_READ_CAP} bytes it printed matched {}",
            v.pattern
        ),
        (false, true) => "it printed nothing".into(),
        (false, false) => format!("nothing it printed matched {}", v.pattern),
    })
}

/// Read `pipe` into `into` until its end or [`VERSION_READ_CAP`] bytes: [`AT_END`] or
/// [`AT_CAP`]. A read error is the end.
fn read_capped(mut pipe: impl Read, into: &Mutex<Vec<u8>>) -> u8 {
    let mut buf = [0u8; 1024];
    loop {
        let room = VERSION_READ_CAP - into.lock().unwrap_or_else(|e| e.into_inner()).len();
        if room == 0 {
            return AT_CAP;
        }
        let want = room.min(buf.len());
        match pipe.read(&mut buf[..want]) {
            Ok(0) => return AT_END,
            Ok(n) => into
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return AT_END,
        }
    }
}

/// How a probed program ended, for people: `it exited with status <n>`, or `it was
/// killed by signal <n>`.
fn exit_text(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("it exited with status {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("it was killed by signal {signal}");
        }
    }
    format!("it ended: {status}")
}

/// The dotted numbers `text` starts with (`0.4.0` of `0.4.0-rc1`), each without its
/// leading zeros, and the rest of `text`; `None` when it does not start with a digit or
/// a dot follows no digit.
pub fn dotted_numbers(text: &str) -> Option<(Vec<&str>, &str)> {
    let mut numbers = Vec::new();
    let mut rest = text;
    loop {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let n = rest[..digits].trim_start_matches('0');
        numbers.push(if n.is_empty() { "0" } else { n });
        rest = &rest[digits..];
        match rest.strip_prefix('.') {
            Some(after) if after.starts_with(|c: char| c.is_ascii_digit()) => rest = after,
            _ => return Some((numbers, rest)),
        }
    }
}

/// Whether the version `read` is at least `min`, both dotted numbers compared number by
/// number (`0.10.0` over `0.4.0`), a missing number read as 0 (`0.4` is `0.4.0`). What
/// follows the numbers is ignored (`0.4.0-rc1` reads `0.4.0`). A `read` that does not
/// start with a number is never at least anything; a `min` that does not is no floor.
pub fn version_at_least(read: &str, min: &str) -> bool {
    let Some((read, _)) = dotted_numbers(read) else {
        return false;
    };
    let Some((min, _)) = dotted_numbers(min) else {
        return true;
    };
    for i in 0..read.len().max(min.len()) {
        let a = read.get(i).copied().unwrap_or("0");
        let b = min.get(i).copied().unwrap_or("0");
        // Without leading zeros, the longer number is the larger.
        match a.len().cmp(&b.len()).then_with(|| a.cmp(b)) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    true
}

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

/// The package managers an install hint can name, in the order the hint prefers them.
pub const PACKAGE_MANAGERS: [&str; 5] = ["apt", "dnf", "brew", "snap", "flatpak"];

/// Which of [`PACKAGE_MANAGERS`] are executables in a `path` directory, in that order.
pub fn package_managers_on(path: Option<OsString>) -> Vec<&'static str> {
    PACKAGE_MANAGERS
        .into_iter()
        .filter(|tool| find_program(&[(*tool).to_string()], None, path.clone()).is_some())
        .collect()
}

/// The package managers this host has on `PATH`, probed once per process.
pub fn host_package_managers() -> &'static [&'static str] {
    static HOST: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    HOST.get_or_init(|| package_managers_on(std::env::var_os("PATH")))
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
    let blocks = row.handler_blocks();
    let programs = blocks
        .get(index)
        .ok_or_else(|| format!("{id}: no handler {index}"))?
        .programs;
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
