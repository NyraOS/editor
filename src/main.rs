// SPDX-License-Identifier: MIT
use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("nymo {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    eprintln!("nymo: not usable yet (early development); only --version works");
    ExitCode::from(2)
}
