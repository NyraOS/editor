// SPDX-License-Identifier: MIT
use std::process::ExitCode;

// Reading and saving files; the editor calls it once it can open files.
#[allow(dead_code, unused_imports)]
mod files;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!(
            "Editor {} (nyra-editor), compatible with GNU nano; not GNU nano",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }
    eprintln!("editor: not usable yet (early development); only --version works");
    ExitCode::from(2)
}
