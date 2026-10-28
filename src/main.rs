// SPDX-License-Identifier: MIT
//! A nano-compatible terminal editor.

mod bindings;
mod buffer;
mod commands;
mod complete;
mod config;
// Parts of its API are for options still to come (backups: -B, -C).
#[allow(dead_code, unused_imports)]
mod files;
mod keys;
mod rcfile;
mod replace;
mod screen;
mod search;
mod syntax;
mod wrap;

use commands::Editor;
use config::Command;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::OwnedFd;
use rustix::pipe::{PipeFlags, pipe_with};
use screen::Terminal;
use std::collections::VecDeque;
use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = bindings::Bindings::new(opts.modernbindings, &opts.binds).check() {
        eprintln!("{e}");
        return ExitCode::from(1);
    }
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
    let stop = files::watch_signals()?;
    let (cols, rows) = Terminal::size();
    let mut ed = Editor::open(opts, cols, rows);
    let mut term = Terminal::start()?;
    // Restore the terminal if the editor panics, so the shell stays usable.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = io::Write::write_all(&mut io::stdout(), b"\x1b[0m\x1b[?25h\x1b[?2004l\x1b[?1049l");
        hook(info);
    }));
    let result = edit(&mut ed, &mut term, &winch, &stop);
    term.stop();
    // The terminal went away, or SIGHUP or SIGTERM came: the changes go to an emergency copy.
    if result.is_err() || stop.load(Ordering::Relaxed) {
        if let Some(path) = ed.emergency_save() {
            eprintln!("Buffer written to {}", path.display());
        }
    }
    result
}

fn edit(
    ed: &mut Editor,
    term: &mut Terminal,
    winch: &OwnedFd,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut reader = keys::Reader::default();
    let mut buf = [0u8; 4096];
    // The wait for keys ends every second to look at `stop`.
    let second = Timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // Keys read but not handled yet: the screen is drawn when none are left.
    let mut keys = VecDeque::new();
    while !ed.done && !stop.load(Ordering::Relaxed) {
        if let Some(key) = keys.pop_front() {
            if key == keys::Key::Ctrl('L') {
                term.invalidate();
            }
            ed.key(key);
            if ed.warning.is_some() {
                // As nano: a warning stays alone on the screen for a moment.
                term.draw(&screen::compose(ed))?;
                std::thread::sleep(Duration::from_millis(1500));
                ed.warned();
            }
            continue;
        }
        // As nano: what a paste held across reads has come to wears the message out.
        ed.message_worn_by(reader.held_len());
        let mut frame = screen::compose(ed);
        if reader.pasting() {
            frame.cursor = None;
        }
        term.draw(&frame)?;
        let stdin = io::stdin();
        let mut fds = [
            PollFd::new(&stdin, PollFlags::IN),
            PollFd::new(winch, PollFlags::IN),
        ];
        // A paste that goes quiet is ended after 2 s: wake up for that.
        let wait = reader.idle_wait().map_or(second, |left| {
            Timespec::try_from(left).unwrap_or(second).min(second)
        });
        match poll(&mut fds, Some(&wait)) {
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
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            keys.extend(reader.feed(&buf[..n]));
        } else {
            keys.extend(reader.idle());
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
