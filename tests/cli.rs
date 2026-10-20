// SPDX-License-Identifier: MIT
use std::process::{Command, Output};

fn editor(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nyra-editor"))
        .args(args)
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
fn anything_else_is_not_usable_yet() {
    let out = editor(&[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}
