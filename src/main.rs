// SPDX-License-Identifier: MIT
//! A nano-compatible terminal editor.

mod buffer;
mod commands;
mod config;
mod files;
mod keys;
mod screen;

use commands::Editor;
use config::Command;
use rustix::event::{PollFd, PollFlags, poll};
use rustix::fd::OwnedFd;
use rustix::pipe::{PipeFlags, pipe_with};
use screen::Terminal;
use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opts = match config::parse(&args) {
        Ok(Command::Edit(opts)) => opts,
        Ok(Command::Version) => {
            println!(
                "Editor {} (nyra-editor), compatible with GNU nano; not GNU nano",
                env!("CARGO_PKG_VERSION")
            );
            return ExitCode::SUCCESS;
        }
        Ok(Command::Usage) => {
            print!("{}", config::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("editor: {e}\nType 'editor --help' for the options.");
            return ExitCode::from(1);
        }
    };
    if !rustix::termios::isatty(io::stdin()) || !rustix::termios::isatty(io::stdout()) {
        eprintln!("editor: the input and the output must be a terminal");
        return ExitCode::from(1);
    }
    match run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("editor: {e}");
            ExitCode::from(1)
        }
    }
}

fn run(opts: config::Options) -> io::Result<()> {
    let winch = winch_pipe()?;
    let (cols, rows) = Terminal::size();
    let mut ed = Editor::open(opts, cols, rows);
    let mut term = Terminal::start()?;
    // Restore the terminal if the editor panics, so the shell stays usable.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = io::Write::write_all(&mut io::stdout(), b"\x1b[0m\x1b[?25h\x1b[?1049l");
        hook(info);
    }));
    let result = edit(&mut ed, &mut term, &winch);
    term.stop();
    result
}

fn edit(ed: &mut Editor, term: &mut Terminal, winch: &OwnedFd) -> io::Result<()> {
    let mut reader = keys::Reader::default();
    let mut buf = [0u8; 4096];
    while !ed.done {
        term.draw(&screen::compose(ed))?;
        let stdin = io::stdin();
        let mut fds = [
            PollFd::new(&stdin, PollFlags::IN),
            PollFd::new(winch, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Err(rustix::io::Errno::INTR) => continue,
            other => other?,
        };
        let (input, resized) = (!fds[0].revents().is_empty(), !fds[1].revents().is_empty());
        if resized {
            while rustix::io::read(winch, &mut buf).is_ok_and(|n| n > 0) {}
            let (cols, rows) = Terminal::size();
            ed.resized(cols, rows);
            term.invalidate();
        }
        if input {
            let n = match rustix::io::read(&stdin, &mut buf) {
                Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => continue,
                other => other?,
            };
            // The terminal is gone (end of input): nothing more can be typed.
            if n == 0 {
                return Ok(());
            }
            for key in reader.feed(&buf[..n]) {
                if key == keys::Key::Ctrl('L') {
                    term.invalidate();
                }
                ed.key(key);
                if ed.done {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// A pipe that gets a byte when the terminal changes size (SIGWINCH).
fn winch_pipe() -> io::Result<OwnedFd> {
    let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGWINCH, write)?;
    Ok(read)
}
