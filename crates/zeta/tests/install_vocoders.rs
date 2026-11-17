//! `tools/install-vocoders.sh`: its options, the notice it stops at, that it parses, and its
//! steps after a yes run against a fake `git` whose clone is a stub zeta-mbe checkout, so what
//! the script hands zeta-mbe's `install.sh` is checked without it. Nothing here builds a
//! vocoder or downloads anything.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The script, from the repository root.
fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/install-vocoders.sh")
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Runs the script in `cwd` with `bin` first on `PATH`, stdin closed and its temporary
/// directory under `cwd`.
fn run_script(cwd: &Path, bin: &Path, args: &[&str]) -> (Output, String) {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(script())
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env("TMPDIR", cwd)
        .env_remove("CC")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

/// The zeta-mbe commit the script pins, read from the script.
fn pin_commit() -> String {
    let text = std::fs::read_to_string(script()).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("ZETA_MBE_COMMIT="))
        .expect("ZETA_MBE_COMMIT in the script")
        .to_string()
}

/// A stub zeta-mbe checkout in `dir`: its `install.sh` writes its arguments, one per line, to
/// `record` and exits with `status`.
fn stub_zeta_mbe(dir: &Path, record: &Path, status: i32) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("zeta-mbe.c"), "").unwrap();
    write_executable(
        &dir.join("install.sh"),
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit {status}\n",
            record.display()
        ),
    );
}

/// Fake `git` (a clone is the stub checkout in `stub`, at `commit`, and leaves `clone_marker`),
/// `cmake` and `cc` (present, never needed) in `bin`.
fn fake_tools(bin: &Path, stub: &Path, commit: &str, clone_marker: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    write_executable(
        &bin.join("git"),
        &format!(
            r#"#!/bin/sh
last=
for a in "$@"; do last=$a; done
case " $* " in
  *" clone "*) touch '{marker}'; mkdir -p "$last"; cp -R '{stub}'/. "$last"/ ;;
  *" rev-parse "*) echo {commit} ;;
esac
"#,
            marker = clone_marker.display(),
            stub = stub.display(),
        ),
    );
    write_executable(&bin.join("cmake"), "#!/bin/sh\n");
    write_executable(&bin.join("cc"), "#!/bin/sh\n");
}

/// The arguments the stub's `install.sh` was run with, or `None` if it never ran.
fn recorded(record: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(record).ok()?;
    Some(text.lines().map(str::to_string).collect())
}

/// A scratch directory with the stub checkout and fake tools, the stub exiting with `status`
/// and `git rev-parse` answering `commit`: (dir, bin, record file, clone marker).
fn setup(status: i32, commit: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (bin, stub, record, cloned) = (
        dir.path().join("bin"),
        dir.path().join("stub"),
        dir.path().join("record"),
        dir.path().join("cloned"),
    );
    stub_zeta_mbe(&stub, &record, status);
    fake_tools(&bin, &stub, commit, &cloned);
    (dir, bin, record, cloned)
}

#[test]
fn help_exits_zero_and_names_the_options() {
    let out = Command::new("bash")
        .arg(script())
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "--help failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    for option in ["--yes", "--src", "--zeta-mbe", "--prefix"] {
        assert!(
            text.contains(option),
            "--help does not name {option}: {text}"
        );
    }
}

#[test]
fn without_yes_it_stops_before_downloading() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = dir.path().join("tool-ran");
    // Fakes for every tool the script needs, each leaving the marker if it runs, so the
    // test needs none of them on the host and sees any that runs before a yes.
    for tool in ["git", "cmake", "cc"] {
        write_executable(
            &bin.join(tool),
            &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        );
    }
    let prefix = dir.path().join("prefix");
    let (out, text) = run_script(dir.path(), &bin, &["--prefix", prefix.to_str().unwrap()]);
    assert!(
        !out.status.success(),
        "the script went on without yes: {text}"
    );
    assert!(text.contains("patent"), "no patent notice: {text}");
    assert!(
        text.contains("GPL-2.0-or-later"),
        "no licence notice: {text}"
    );
    assert!(!marker.exists(), "a tool ran before a yes: {text}");
}

#[test]
fn without_home_or_prefix_it_stops_naming_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = dir.path().join("tool-ran");
    // Fakes for every tool the script needs, each leaving the marker if it runs.
    for tool in ["git", "cmake", "cc"] {
        write_executable(
            &bin.join(tool),
            &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        );
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("bash")
        .arg(script())
        .arg("--yes")
        .current_dir(dir.path())
        .env("PATH", path)
        .env("TMPDIR", dir.path())
        .env_remove("HOME")
        .env_remove("CC")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "it went on without HOME: {text}");
    assert!(
        text.contains("--prefix"),
        "the refusal does not name --prefix: {text}"
    );
    assert!(!marker.exists(), "a tool ran: {text}");
}

#[test]
fn the_script_passes_bash_n() {
    let out = Command::new("bash")
        .arg("-n")
        .arg(script())
        .output()
        .unwrap();
    assert!(out.status.success(), "bash -n failed: {out:?}");
}

#[test]
fn after_a_yes_it_clones_the_pinned_tag_and_runs_its_install_script() {
    let (dir, bin, record, cloned) = setup(0, &pin_commit());
    let mbelib = dir.path().join("mbelib-neo");
    std::fs::create_dir(&mbelib).unwrap();
    let (out, text) = run_script(
        dir.path(),
        &bin,
        &["--yes", "--prefix", "rel/prefix", "--src", "mbelib-neo"],
    );
    assert!(out.status.success(), "the script failed: {text}");
    assert!(cloned.exists(), "nothing was cloned: {text}");
    let mbelib = std::fs::canonicalize(&mbelib).unwrap();
    let want = ["--yes", "--prefix", "rel/prefix", "--src"]
        .map(String::from)
        .into_iter()
        .chain([mbelib.display().to_string()])
        .collect::<Vec<_>>();
    assert_eq!(recorded(&record), Some(want), "{text}");
}

#[test]
fn a_tag_at_another_commit_is_refused() {
    let (dir, bin, record, _) = setup(0, &"0".repeat(40));
    let (out, text) = run_script(dir.path(), &bin, &["--yes", "--prefix", "p"]);
    assert!(!out.status.success(), "another commit was built: {text}");
    assert!(text.contains("refusing to build it"), "{text}");
    assert_eq!(recorded(&record), None, "zeta-mbe's script ran: {text}");
}

#[test]
fn with_a_checkout_and_a_source_tree_nothing_is_cloned() {
    let (dir, bin, record, cloned) = setup(0, &pin_commit());
    std::fs::create_dir(dir.path().join("mbelib-neo")).unwrap();
    let args = [
        "--yes",
        "--prefix",
        "p",
        "--src",
        "mbelib-neo",
        "--zeta-mbe",
        "stub",
    ];
    let (out, text) = run_script(dir.path(), &bin, &args);
    assert!(out.status.success(), "the script failed: {text}");
    assert!(!cloned.exists(), "it cloned with a checkout given: {text}");
    assert!(
        recorded(&record).is_some(),
        "zeta-mbe's script never ran: {text}"
    );
}

#[test]
fn a_failing_zeta_mbe_install_fails_the_script() {
    let (dir, bin, _, _) = setup(1, &pin_commit());
    let (out, text) = run_script(dir.path(), &bin, &["--yes", "--prefix", "p"]);
    assert!(!out.status.success(), "a failed install passed: {text}");
    assert!(text.contains("zeta-mbe's install.sh failed"), "{text}");
}
