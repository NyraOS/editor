// SPDX-License-Identifier: MIT
//! One editor running in a pseudo-terminal, with vt100 as the terminal emulator.

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long an editor may take to draw its first screen, or keep drawing without a pause.
const DRAW_LIMIT: Duration = Duration::from_secs(10);
/// How long the first screen must stay still before keys are sent.
const FIRST_SCREEN_PAUSE: Duration = Duration::from_millis(100);
/// How long an editor may take to exit after it closed the terminal.
const EXIT_LIMIT: Duration = Duration::from_secs(3);

pub struct Term {
    master: File,
    child: Child,
    parser: vt100::Parser,
    /// The editor closed the terminal (it exited).
    closed: bool,
    /// The start of a replacement character at the end of the last read, held for the next one.
    held: Vec<u8>,
}

/// U+FFFD, the replacement character, which editors draw for bytes that are not valid UTF-8.
const REPLACEMENT: &[u8] = "\u{fffd}".as_bytes();
/// What the emulator gets instead: vt100 drops U+FFFD when printing, so the screens would not
/// show it. `¿` is one column wide too.
const VISIBLE: &[u8] = "\u{bf}".as_bytes();

/// How the editor ended.
#[derive(Debug, PartialEq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    /// Still running after the steps, and killed.
    Running,
}

impl Term {
    /// Starts `command` in `dir`, with a fixed environment and an empty `home`.
    pub fn spawn(
        command: &[String],
        args: &[String],
        dir: &Path,
        home: &Path,
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let master = openpt(flags)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let slave = ioctl_tiocgptpeer(&master, flags)?;
        tcsetwinsize(&master, winsize(cols, rows))?;
        // setsid(1) gives the editor its own session with the terminal as the controlling one,
        // so it gets SIGWINCH on a resize, as in a real terminal.
        let child = Command::new("setsid")
            .args(["--ctty", "--wait"])
            .args(command)
            .arg("--ignorercfiles")
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", home)
            .env("TERM", "xterm-256color")
            .env("LANG", "C.UTF-8")
            .env("LC_ALL", "C.UTF-8")
            .env("TZ", "UTC")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .spawn()?;
        let mut term = Self {
            master: File::from(master),
            child,
            parser: vt100::Parser::new(nonzero(rows), nonzero(cols), 0),
            closed: false,
            held: Vec::new(),
        };
        term.wait_for_output()?;
        Ok(term)
    }

    /// Sends keys; to an editor that exited, they go nowhere, as in a real terminal.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        match self.master.write_all(bytes) {
            Err(e) if e.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => {
                self.closed = true;
                Ok(())
            }
            other => other,
        }
    }

    /// Whether the editor closed the terminal (it exited).
    pub fn closed(&self) -> bool {
        self.closed
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.parser.set_size(nonzero(rows), nonzero(cols));
        tcsetwinsize(&self.master, winsize(cols, rows))?;
        Ok(())
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Reads the editor's output until it pauses for `quiet`.
    pub fn settle(&mut self, quiet: Duration) -> io::Result<()> {
        let start = Instant::now();
        while self.read(quiet)? {
            if start.elapsed() > DRAW_LIMIT {
                return Err(io::Error::other(format!(
                    "still drawing after {} s",
                    DRAW_LIMIT.as_secs()
                )));
            }
        }
        Ok(())
    }

    /// Waits for the first screen, so keys never reach the terminal before the editor reads it.
    /// A program that fails to start prints its error and exits an instant later; the pause after
    /// the output lets that exit show, or the keys would be echoed by a terminal still open.
    fn wait_for_output(&mut self) -> io::Result<()> {
        if self.read(DRAW_LIMIT)? {
            return self.settle(FIRST_SCREEN_PAUSE);
        }
        if self.closed {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "no output in {} s",
            DRAW_LIMIT.as_secs()
        )))
    }

    /// Reads what arrives within `timeout`; false if nothing did.
    fn read(&mut self, timeout: Duration) -> io::Result<bool> {
        if self.closed {
            return Ok(false);
        }
        let timeout = Timespec::try_from(timeout).map_err(io::Error::other)?;
        let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
        if poll(&mut fds, Some(&timeout))? == 0 {
            // Nothing more came: what was held is not a replacement character.
            let held = std::mem::take(&mut self.held);
            self.parser.process(&held);
            return Ok(false);
        }
        let mut buf = [0; 65536];
        match self.master.read(&mut buf) {
            Ok(n) if n > 0 => {
                let bytes = visible(&mut self.held, &buf[..n]);
                self.parser.process(&bytes);
                Ok(true)
            }
            // EIO: every process closed the terminal.
            Ok(_) | Err(_) => {
                let held = std::mem::take(&mut self.held);
                self.parser.process(&held);
                self.closed = true;
                Ok(false)
            }
        }
    }

    /// The editor's exit status, once its output pauses for `quiet`. An editor that still has
    /// the terminal open then is killed and reported as running.
    pub fn finish(&mut self, quiet: Duration) -> io::Result<Exit> {
        use std::os::unix::process::ExitStatusExt;
        self.settle(quiet)?;
        let start = Instant::now();
        while self.closed && start.elapsed() < EXIT_LIMIT {
            if let Some(status) = self.child.try_wait()? {
                return Ok(match (status.code(), status.signal()) {
                    (Some(code), _) => Exit::Code(code),
                    (_, signal) => Exit::Signal(signal.unwrap_or(0)),
                });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(Exit::Running)
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn nonzero(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

/// The output with each replacement character made visible to the emulator; the start of one at
/// the end is held until the next read shows whether it is one.
fn visible(held: &mut Vec<u8>, data: &[u8]) -> Vec<u8> {
    let mut bytes = std::mem::take(held);
    bytes.extend_from_slice(data);
    let keep = (1..REPLACEMENT.len())
        .rev()
        .find(|&k| bytes.len() >= k && REPLACEMENT.starts_with(&bytes[bytes.len() - k..]))
        .unwrap_or(0);
    *held = bytes.split_off(bytes.len() - keep);
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = &bytes[..];
    while !rest.is_empty() {
        if rest.starts_with(REPLACEMENT) {
            out.extend_from_slice(VISIBLE);
            rest = &rest[REPLACEMENT.len()..];
        } else {
            out.push(rest[0]);
            rest = &rest[1..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::visible;

    #[test]
    fn replacement_characters_are_made_visible_across_reads() {
        let mut held = Vec::new();
        assert_eq!(
            visible(&mut held, "a\u{fffd}b".as_bytes()),
            "a\u{bf}b".as_bytes()
        );
        assert_eq!(visible(&mut held, b"x\xef\xbf"), b"x");
        assert_eq!(visible(&mut held, b"\xbdy"), "\u{bf}y".as_bytes());
        // The start of another character is given back with the next bytes.
        assert_eq!(visible(&mut held, b"\xef"), b"");
        assert_eq!(
            visible(&mut held, &"\u{ff71}".as_bytes()[1..]),
            "\u{ff71}".as_bytes()
        );
    }
}
