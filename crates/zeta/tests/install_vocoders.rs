//! `tools/install-vocoders.sh`: its options, the notice it stops at, that it parses, and its
//! steps after a yes run against fake `git`, `cmake` and `cc`. Nothing here builds a vocoder
//! or downloads anything.

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

/// The commit the script pins, read from the script.
fn pin_commit() -> String {
    let text = std::fs::read_to_string(script()).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("PIN_COMMIT="))
        .expect("PIN_COMMIT in the script")
        .to_string()
}

/// A `zeta-mbe` stand-in that answers each frame of silence with 160 samples.
const FAKE_PROGRAM_OK: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "zeta-mbe fake" ;;
  imbe7200x4400) n=$(wc -c); head -c $((n / 184 * 320)) /dev/zero ;;
  ambe3600x2450) n=$(wc -c); head -c $((n / 96 * 320)) /dev/zero ;;
  *) exit 2 ;;
esac
"#;

/// A `zeta-mbe` stand-in that fails.
const FAKE_PROGRAM_FAILS: &str = "#!/bin/sh\ncat >/dev/null\nexit 1\n";

/// Fake `git` (a clone is an empty tree at the pinned commit), `cmake` (a build leaves an
/// empty `libmbe-neo.a`) and `cc` (writes `program` where `-o` says) in `bin`.
fn fake_build_tools(bin: &Path, program: &str) {
    std::fs::create_dir_all(bin).unwrap();
    write_executable(
        &bin.join("git"),
        &format!(
            r#"#!/bin/sh
last=
for a in "$@"; do last=$a; done
case " $* " in
  *" clone "*) mkdir -p "$last/include/mbelib-neo" ;;
  *" rev-parse "*) echo {} ;;
esac
"#,
            pin_commit()
        ),
    );
    write_executable(
        &bin.join("cmake"),
        "#!/bin/sh\nif [ \"$1\" = --build ]; then mkdir -p \"$2\"; touch \"$2/libmbe-neo.a\"; fi\n",
    );
    let program_file = bin.parent().unwrap().join("fake-zeta-mbe");
    std::fs::write(&program_file, program).unwrap();
    write_executable(
        &bin.join("cc"),
        &format!(
            r#"#!/bin/sh
out=
while [ $# -gt 0 ]; do
  if [ "$1" = -o ]; then out=$2; shift; fi
  shift
done
cp '{}' "$out"
chmod 755 "$out"
"#,
            program_file.display()
        ),
    );
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
    for option in ["--yes", "--src", "--prefix"] {
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
fn a_relative_prefix_prints_an_absolute_handler_path() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fake_build_tools(&bin, FAKE_PROGRAM_OK);
    let (out, text) = run_script(dir.path(), &bin, &["--yes", "--prefix", "rel/prefix"]);
    assert!(out.status.success(), "the script failed: {text}");
    assert!(
        dir.path().join("rel/prefix/bin/zeta-mbe").is_file(),
        "not installed under the relative prefix: {text}"
    );
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("ZETA_HANDLER_ZETA_MBE="))
        .unwrap_or_else(|| panic!("no ZETA_HANDLER_ZETA_MBE line: {text}"));
    let path = Path::new(line.trim_start_matches("ZETA_HANDLER_ZETA_MBE="));
    assert!(path.is_absolute(), "a relative handler path: {line}");
    assert!(path.ends_with("rel/prefix/bin/zeta-mbe"), "{line}");
}

#[test]
fn a_program_that_fails_its_self_test_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    fake_build_tools(&bin, FAKE_PROGRAM_FAILS);
    let prefix = dir.path().join("prefix");
    let (out, text) = run_script(
        dir.path(),
        &bin,
        &["--yes", "--prefix", prefix.to_str().unwrap()],
    );
    assert!(!out.status.success(), "a failing self-test passed: {text}");
    assert!(
        text.contains("self-test imbe7200x4400") && text.contains("nothing installed"),
        "no self-test message: {text}"
    );
    assert!(
        !prefix.join("bin/zeta-mbe").exists(),
        "the failing program was installed: {text}"
    );
}
