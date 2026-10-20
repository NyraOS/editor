// SPDX-License-Identifier: MIT
use std::process::{Command, Output, Stdio};

fn editor(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nyra-editor"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn version_prints_name_and_version() {
    let out = editor(&["--version"]);
    assert!(out.status.success());
    let expected = format!(
        "Editor {} (nyra-editor), compatible with GNU nano; not GNU nano\n",
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), expected);
}

#[test]
fn help_lists_the_options() {
    let out = editor(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("Usage: editor "), "{text}");
    assert!(text.contains("--linenumbers"), "{text}");
}

#[test]
fn a_bad_option_is_an_error() {
    let out = editor(&["--no-such-option"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("--no-such-option"), "{err}");
}

#[test]
fn editing_needs_a_terminal() {
    let out = editor(&["notes.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}
