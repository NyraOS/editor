// SPDX-License-Identifier: MIT
//! The comparison tests: every scenario in tests/compare/scenarios runs on GNU nano 8.4 and on
//! the editor under test, in the same virtual terminal; the screens must match after every step
//! and the files at the end. See README.md next to this file.

mod compare;
mod scenario;
mod term;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
/// skipped, as it is pending and `skip_pending` is set.
fn run_file(
    name: &str,
    path: &Path,
    nano: &[String],
    editor: &[String],
    skip_pending: bool,
) -> Result<bool, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let scenario = scenario::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if skip_pending && scenario.pending.is_some() {
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
    let result = compare::run(&scenario, nano, editor, &dir, show);
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
                    let result = run_file(name, path, &nano, &editor, skip_pending);
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
            format!(" ({skipped} pending, skipped)")
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
    let report = run_file("selftest/wrong-editor", &path, &nano, &wrong, false)
        .expect_err("a wrong editor passed");
    println!("Expected failure (the harness's self-test), with nano --constantshow as the editor:");
    println!("{report}");
    assert!(report.starts_with("screens differ after line "), "{report}");
    assert!(report.contains("\n    editor: |"), "{report}");
    let carets = |l: &str| l.starts_with("             ") && l.trim_start().starts_with('^');
    assert!(report.lines().any(carets), "{report}");
}

/// A reference that cannot run must fail the scenario, not match an editor that cannot run either.
#[test]
fn a_reference_that_cannot_run_fails() {
    let missing = vec!["/nonexistent/nano".to_string()];
    let path = scenarios_dir().join("basic/exit-discard.scn");
    let report = run_file("selftest/missing-nano", &path, &missing, &missing, false)
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
    for bad in [
        "{", "{Nope}", "{^Ab}", "{S-a}", "{Down*0}", "{Down*x}", "{^1}", "{}",
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
                resize: 90x20\n";
    let s = scenario::parse(text).unwrap();
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
        ("args: x\n", "no steps"),
    ] {
        let err = scenario::parse(bad).unwrap_err();
        assert!(err.starts_with(line), "{bad:?}: {err}");
    }
}
