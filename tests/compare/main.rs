// SPDX-License-Identifier: MIT
//! The comparison tests: every scenario in tests/compare/scenarios runs on GNU nano 8.4 and on
//! the editor under test, in the same virtual terminal; the screens must match after every step
//! and the files at the end. See README.md next to this file.

mod compare;
mod scenario;
mod term;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const NANO_VERSION: &str = "GNU nano, version 8.4";

/// GNU nano 8.4: `$NANO`, which must be 8.4, or else `nano` from PATH if it is 8.4.
/// None (the tests are skipped) when `$NANO` is not set and PATH has no nano 8.4.
fn nano() -> Option<Vec<String>> {
    let (path, required) = match std::env::var("NANO") {
        Ok(path) if !path.is_empty() => (path, true),
        _ => ("nano".to_string(), false),
    };
    let version = Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|out| Some(out.lines().next()?.trim().to_string()));
    if version.as_deref() == Some(NANO_VERSION) {
        return Some(vec![program(&path)]);
    }
    let found = version.unwrap_or_else(|| "not found".into());
    assert!(!required, "NANO={path}: want {NANO_VERSION}, found {found}");
    eprintln!(
        "skipped: the comparison tests need {NANO_VERSION} (nano in PATH: {found}); \
         set NANO, see tests/compare/README.md"
    );
    None
}

/// The editor under test: `$COMPARE_EDITOR` (a command, with arguments), or else GNU nano
/// itself while the editor cannot edit yet, which checks the scenarios and the harness.
fn editor(nano: &[String]) -> Vec<String> {
    match std::env::var("COMPARE_EDITOR") {
        Ok(cmd) if !cmd.trim().is_empty() => {
            let mut words = cmd.split_whitespace().map(String::from);
            let first = words.next().map(|p| program(&p));
            first.into_iter().chain(words).collect()
        }
        _ => nano.to_vec(),
    }
}

/// The editor under test must read its system nanorc from `$NYRA_EDITOR_SYSCONFDIR`, which the
/// nanorc scenarios rely on: a build without the `test-sysconfdir` feature reads the real files
/// and would pass them while testing nothing. A bad line in a probe nanorc is reported on the
/// standard error before the editor looks for a terminal, so no terminal is needed here.
fn check_editor_reads_sysconfdir(editor: &[String]) {
    let probe = std::env::temp_dir().join(format!("nyra-editor-probe-{}", std::process::id()));
    fs::create_dir_all(&probe).expect("create the probe directory");
    let rc = probe.join("nanorc");
    fs::write(&rc, "set no-such-option\n").expect("write the probe nanorc");
    let out = Command::new(&editor[0])
        .args(&editor[1..])
        .env("NYRA_EDITOR_SYSCONFDIR", &probe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run the editor under test");
    let _ = fs::remove_dir_all(&probe);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("Error in {}", rc.display())),
        "the editor under test ignores NYRA_EDITOR_SYSCONFDIR (it did not read {}): build it \
         with --features test-sysconfdir. Its stderr:\n{stderr}",
        rc.display()
    );
}

/// Where nano reads its system nanorc, when the tests control it: `$NANO_SYSCONFDIR`, else
/// `etc` next to the `bin` of a nano built by tools/build-nano.sh. The directory must be the one
/// compiled into nano (its name is looked for in the binary), and its nanorc must hold nothing
/// but comments, so that the host's settings cannot change the result. The editor under test is
/// pointed to the same directory (see `Term::spawn`).
fn nano_sysconfdir(nano: &str) -> Result<PathBuf, String> {
    let dir = match std::env::var_os("NANO_SYSCONFDIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ if nano.contains('/') => {
            // `<prefix>/bin/nano` -> `<prefix>/etc`
            let prefix = Path::new(nano).parent().and_then(Path::parent);
            prefix
                .ok_or("NANO has no bin directory to go up from")?
                .join("etc")
        }
        _ => return Err("set NANO to a nano from tools/build-nano.sh or NANO_SYSCONFDIR".into()),
    };
    let dir = fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let compiled = format!("{}/nanorc", dir.display());
    let binary = if nano.contains('/') {
        fs::read(nano)
    } else {
        // A bare name is looked up in PATH, as the shell would.
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = std::env::split_paths(&path).map(|d| d.join(nano));
        fs::read(found.into_iter().find(|p| p.is_file()).unwrap_or_default())
    }
    .map_err(|e| format!("{nano}: {e}"))?;
    if !binary
        .windows(compiled.len())
        .any(|w| w == compiled.as_bytes())
    {
        return Err(format!("nano does not read {compiled}"));
    }
    let text = fs::read_to_string(&compiled).unwrap_or_default();
    if text
        .lines()
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
    {
        return Err(format!("{compiled} has settings"));
    }
    Ok(dir)
}

/// A program path made absolute, as the editors start in their own directories; a bare name
/// (looked up in PATH) stays as it is.
fn program(path: &str) -> String {
    if !path.contains('/') {
        return path.to_string();
    }
    std::path::absolute(path).map_or_else(|_| path.to_string(), |p| p.display().to_string())
}

fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compare/scenarios")
}

/// Every scenario, sorted, as (name, path): the name is the path under scenarios/ without .scn.
fn scenario_files() -> Vec<(String, PathBuf)> {
    let root = scenarios_dir();
    let mut out = Vec::new();
    let mut todo = vec![root.clone()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|e| e == "scn") {
                let name = path.strip_prefix(&root).unwrap().with_extension("");
                out.push((name.display().to_string(), path));
            }
        }
    }
    out.sort();
    out
}

/// Runs one scenario file; the files of a failed run stay under target/tmp/compare. Ok(false):
/// skipped, as it is pending and `skip_pending` is set, or it has nanorc files and `sysconfdir`
/// is None.
fn run_file(
    name: &str,
    path: &Path,
    nano: &[String],
    editor: &[String],
    skip_pending: bool,
    sysconfdir: Option<&Path>,
) -> Result<bool, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let scenario = scenario::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if skip_pending && scenario.pending.is_some() {
        return Ok(false);
    }
    if sysconfdir.is_none() && !scenario.home_files.is_empty() {
        return Ok(false);
    }
    // Named after the scenario file, so that no two scenarios share a directory.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("compare")
        .join(format!("{name}.scn"));
    let _ = fs::remove_dir_all(&dir);
    let mut log = String::new();
    let show = std::env::var_os("COMPARE_SHOW")
        .is_some()
        .then_some(&mut log);
    let result = compare::run(&scenario, nano, editor, &dir, sysconfdir, show);
    print!("{log}");
    if result.is_ok() {
        let _ = fs::remove_dir_all(&dir);
    }
    result
        .map(|()| true)
        .map_err(|e| format!("{e}  (the files are kept in {})\n", dir.display()))
}

#[test]
fn scenarios() {
    let Some(nano) = nano() else { return };
    let editor = editor(&nano);
    let only = std::env::var("COMPARE_ONLY").unwrap_or_default();
    let list: Vec<_> = scenario_files()
        .into_iter()
        .filter(|(name, _)| name.contains(&only))
        .collect();
    assert!(!list.is_empty(), "no scenario name contains {only:?}");
    println!(
        "comparing {} scenarios: nano is {:?}, the editor is {:?}",
        list.len(),
        nano.join(" "),
        editor.join(" ")
    );

    // As root, nano ignores $HOME (it takes the home from the password database), so the
    // scenarios with nanorc files cannot be run in a directory of their own.
    let root = rustix::process::geteuid().is_root();
    let sysconfdir = if root {
        Err("running as root, where nano ignores $HOME".to_string())
    } else {
        nano_sysconfdir(&nano[0])
    };
    // With $NANO set (as in CI) the nanorc scenarios must run, so a missing sysconfdir is a failure.
    if let (Err(why), Some(_), false) = (&sysconfdir, std::env::var_os("NANO"), root) {
        panic!("NANO is set, but its system nanorc cannot be used: {why}");
    }
    if let Err(why) = &sysconfdir {
        eprintln!("skipped: the scenarios with nanorc files ({why}); see tests/compare/README.md");
    }
    let sysconfdir = sysconfdir.ok();
    if editor != nano && !root {
        check_editor_reads_sysconfdir(&editor);
    }
    let with_rc_files = list
        .iter()
        .filter(|(_, path)| {
            let text = fs::read_to_string(path).unwrap_or_default();
            scenario::parse(&text).is_ok_and(|s| !s.home_files.is_empty())
        })
        .count();
    println!("{with_rc_files} of those scenarios have nanorc files");

    // Pending scenarios run only with GNU nano as the editor, which checks them.
    let skip_pending = editor != nano;
    let start = Instant::now();
    let next = AtomicUsize::new(0);
    let skipped = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some((name, path)) = list.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let t = Instant::now();
                    let result = run_file(
                        name,
                        path,
                        &nano,
                        &editor,
                        skip_pending,
                        sysconfdir.as_deref(),
                    );
                    let secs = t.elapsed().as_secs_f32();
                    let status = match result {
                        Ok(true) => "ok  ",
                        Ok(false) => "skip",
                        Err(_) => "FAIL",
                    };
                    println!("{status} {name} ({secs:.1} s)");
                    if result == Ok(false) {
                        skipped.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Err(report) = result {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("FAIL {name}: {report}"));
                    }
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    let skipped = skipped.into_inner();
    println!(
        "{} of {} scenarios passed in {:.1} s{}",
        list.len() - failures.len() - skipped,
        list.len(),
        start.elapsed().as_secs_f32(),
        if skipped > 0 {
            format!(" ({skipped} skipped)")
        } else {
            String::new()
        }
    );
    assert!(
        failures.is_empty(),
        "\n{}\n{} of {} scenarios failed",
        failures.join("\n"),
        failures.len(),
        list.len()
    );
}

/// The harness must catch an editor that differs from GNU nano, and say clearly where. GNU nano
/// with --constantshow plays a wrong editor: it shows the cursor position in the status bar.
/// Its report is printed, as an example of a failure (cargo test -- --nocapture shows it).
#[test]
fn a_wrong_editor_fails_with_a_readable_report() {
    let Some(nano) = nano() else { return };
    let wrong = [nano.clone(), vec!["--constantshow".to_string()]].concat();
    let path = scenarios_dir().join("basic/type-and-save.scn");
    let report = run_file("selftest/wrong-editor", &path, &nano, &wrong, false, None)
        .expect_err("a wrong editor passed");
    println!("Expected failure (the harness's self-test), with nano --constantshow as the editor:");
    println!("{report}");
    assert!(report.starts_with("screens differ after line "), "{report}");
    assert!(report.contains("\n    editor: |"), "{report}");
    let carets = |l: &str| l.starts_with("             ") && l.trim_start().starts_with('^');
    assert!(report.lines().any(carets), "{report}");
}

/// A file rewritten during a scenario must not hide what an editor saved before it: GNU nano with
/// --nonewlines saves without the final newline, and that must show before the rewrite.
#[test]
fn a_rewrite_does_not_hide_a_save() {
    let Some(nano) = nano() else { return };
    let wrong = [nano.clone(), vec!["--nonewlines".to_string()]].concat();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("selftest");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rewrite.scn");
    fs::write(
        &path,
        "# Save, then the file is rewritten.\n\
         args: r.txt\n\
         file r.txt:\n\
         | one\n\
         keys: {Down}x\n\
         keys: {^O}{Enter}\n\
         file r.txt:\n\
         | three\n\
         keys: {^X}\n",
    )
    .unwrap();
    let report = run_file("selftest/rewrite", &path, &nano, &wrong, false, None)
        .expect_err("a wrong save was hidden by the rewrite");
    assert!(report.starts_with("files differ before line 7"), "{report}");
}

/// A reference that cannot run must fail the scenario, not match an editor that cannot run either.
#[test]
fn a_reference_that_cannot_run_fails() {
    let missing = vec!["/nonexistent/nano".to_string()];
    let path = scenarios_dir().join("basic/exit-discard.scn");
    let report = run_file(
        "selftest/missing-nano",
        &path,
        &missing,
        &missing,
        false,
        None,
    )
    .expect_err("a missing nano passed");
    assert!(report.contains("nano could not run"), "{report}");
}

#[test]
fn program_paths_are_made_absolute() {
    assert_eq!(program("nano"), "nano");
    let relative = program("../usr/bin/nano");
    assert!(Path::new(&relative).is_absolute(), "{relative}");
    assert!(relative.ends_with("usr/bin/nano"), "{relative}");
    assert_eq!(program("/usr/bin/nano"), "/usr/bin/nano");
}

#[test]
fn keys_notation() {
    let k = |s: &str| scenario::keys(s).unwrap();
    assert_eq!(k("ab{{c}"), b"ab{c}");
    assert_eq!(k("{Enter}{Tab}{Bsp}{Esc}{Space}"), b"\r\t\x7f\x1b ");
    assert_eq!(
        k("{^X}{^x}{^@}{^]}{^_}{^?}{^/}"),
        b"\x18\x18\x00\x1d\x1f\x7f\x1f"
    );
    assert_eq!(k("{M-u}{M-U}{M-^X}"), b"\x1bu\x1bU\x1b\x18");
    assert_eq!(
        k("{Up}{Down}{Right}{Left}{Home}{End}"),
        b"\x1bOA\x1bOB\x1bOC\x1bOD\x1bOH\x1bOF"
    );
    assert_eq!(
        k("{^Left}{S-Up}{M-Home}{^S-End}"),
        b"\x1b[1;5D\x1b[1;2A\x1b[1;3H\x1b[1;6F"
    );
    assert_eq!(
        k("{Del}{PgDn}{^Del}{F1}{F12}"),
        b"\x1b[3~\x1b[6~\x1b[3;5~\x1bOP\x1b[24~"
    );
    assert_eq!(k("{Down*3}{x*2}"), b"\x1bOB\x1bOB\x1bOBxx");
    assert_eq!(k("é"), "é".as_bytes());
    // The mouse, as xterm's SGR reports: counted from 1, a click is a press and a release.
    assert_eq!(k("{Mouse 65 5 4 M}"), b"\x1b[<65;5;4M");
    assert_eq!(k("{Mouse 0 5 4 m*2}"), b"\x1b[<0;5;4m\x1b[<0;5;4m");
    assert_eq!(k("{Click 12 3}"), b"\x1b[<0;12;3M\x1b[<0;12;3m");
    for bad in [
        "{",
        "{Nope}",
        "{^Ab}",
        "{S-a}",
        "{Down*0}",
        "{Down*x}",
        "{^1}",
        "{}",
        "{Click 1}",
        "{Click 1 2 3}",
        "{Click a 2}",
        "{Mouse 0 1 2}",
        "{Mouse 0 1 2 X}",
    ] {
        assert!(scenario::keys(bad).is_err(), "{bad} should be an error");
    }
}

#[test]
fn scenario_format() {
    let text = "# A comment\n\
                args: -l notes.txt\n\
                size: 100x30\n\
                differ: rows 22-23: the help lines\n\
                differ: row 0 cols 2-9: the name\n\
                pending: the editor cannot do this yet\n\
                file notes.txt:\n\
                | one\\ttab\n\
                |\n\
                | no newline\\\n\
                file empty:\n\
                keys: hi{Enter}\n\
                resize: 90x20\n\
                differ: none\n\
                file notes.txt:\n\
                | new\n\
                pause: 1500\n\
                differ: row 7 col 3: a column\n\
                keys: x\n";
    let s = scenario::parse(text).unwrap();
    let rc = scenario::parse("nanorc: set a\\tb\nnanorc:\nxdgrc: set c\nkeys: x\n").unwrap();
    assert_eq!(rc.home_files[0], (".nanorc", b"set a\tb\n\n".to_vec()));
    assert_eq!(
        rc.home_files[1],
        (".config/nano/nanorc", b"set c\n".to_vec())
    );
    assert_eq!(s.args, ["-l", "notes.txt"]);
    assert_eq!((s.cols, s.rows), (100, 30));
    assert_eq!(
        s.files[0],
        ("notes.txt".into(), b"one\ttab\n\nno newline".to_vec())
    );
    assert_eq!(s.files[1], ("empty".into(), Vec::new()));
    assert_eq!(s.steps[0].action, scenario::Action::Keys(b"hi\r".to_vec()));
    assert_eq!(s.steps[0].line, 12);
    assert_eq!(
        s.steps[1].action,
        scenario::Action::Resize { cols: 90, rows: 20 }
    );
    assert!(s.differ[0].contains(23, 79) && !s.differ[0].contains(21, 0));
    assert!(s.differ[1].contains(0, 9) && !s.differ[1].contains(0, 10));
    // Differences apply from where they are declared, until `differ: none`.
    assert_eq!(s.steps[0].differ, s.differ);
    assert_eq!(s.steps[1].differ, s.differ);
    assert_eq!(
        s.steps[2].action,
        scenario::Action::Write {
            name: "notes.txt".into(),
            bytes: b"new\n".to_vec()
        }
    );
    assert!(s.steps[2].differ.is_empty());
    assert_eq!(s.steps[3].action, scenario::Action::Pause(1500));
    assert!(s.steps[4].differ[0].contains(7, 3) && !s.steps[4].differ[0].contains(7, 4));
    assert_eq!(s.pending.as_deref(), Some("the editor cannot do this yet"));

    for (bad, line) in [
        ("keys: a\nsize: 80\n", "line 2:"),
        ("keys: a\n| stray\n", "line 2:"),
        ("file a:\n|x\nkeys: a\n", "line 2:"),
        ("file ../a:\nkeys: a\n", "line 1:"),
        ("args: a\nargs: b\nkeys: a\n", "line 2:"),
        ("differ: rows 1-2\nkeys: a\n", "line 1:"),
        ("bogus: 1\nkeys: a\n", "line 1:"),
        ("pending:\nkeys: a\n", "line 1:"),
        ("keys: a\npause: 0\n", "line 2:"),
        ("args: x\n", "no steps"),
    ] {
        let err = scenario::parse(bad).unwrap_err();
        assert!(err.starts_with(line), "{bad:?}: {err}");
    }
}
