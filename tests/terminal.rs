// SPDX-License-Identifier: MIT
//! The modes the editor switches on in the terminal (mouse reports, bracketed paste, the
//! alternate screen) are switched off again however it ends, and mouse reports are never typed.
//! The editor runs in a pseudo-terminal; the tests wait for output they expect, never for time.

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A run that is stuck fails after this long; a run that works never waits for it.
const STUCK: Duration = Duration::from_secs(60);

const MOUSE_ON: &[u8] = b"\x1b[?1006;1000h";
const MOUSE_OFF: &[u8] = b"\x1b[?1006;1000l";
const PASTE_ON: &[u8] = b"\x1b[?2004h";
const PASTE_OFF: &[u8] = b"\x1b[?2004l";
const SCREEN_ON: &[u8] = b"\x1b[?1049h";
const SCREEN_OFF: &[u8] = b"\x1b[?1049l";

struct Session {
    master: File,
    child: Child,
    out: Vec<u8>,
    dir: PathBuf,
}

impl Session {
    /// Starts the editor on `t.txt` (with the text `text`) in a directory of its own.
    fn start(name: &str, args: &[&str], text: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "nyra-editor-terminal-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("t.txt"), text).unwrap();
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let master = openpt(flags).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let slave = ioctl_tiocgptpeer(&master, flags).unwrap();
        let size = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        tcsetwinsize(&master, size).unwrap();
        // setsid(1) gives the editor the terminal as its controlling one, as a terminal emulator does.
        let child = Command::new("setsid")
            .args([
                "--ctty",
                env!("CARGO_BIN_EXE_nyra-editor"),
                "--ignorercfiles",
            ])
            .args(args)
            .arg("t.txt")
            .current_dir(&dir)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &dir)
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave))
            .spawn()
            .unwrap();
        let mut session = Self {
            master: File::from(master),
            child,
            out: Vec::new(),
            dir,
        };
        session.wait_for(SCREEN_ON);
        session
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).unwrap();
    }

    /// Reads what the editor writes; false once it has closed the terminal.
    fn read_more(&mut self) -> bool {
        let wait = Timespec::try_from(Duration::from_millis(200)).unwrap();
        let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
        if poll(&mut fds, Some(&wait)).unwrap() == 0 {
            return true;
        }
        let mut buf = [0; 65536];
        match self.master.read(&mut buf) {
            Ok(n) if n > 0 => {
                self.out.extend_from_slice(&buf[..n]);
                true
            }
            // EIO: nothing has the terminal open any more.
            _ => false,
        }
    }

    fn wait_for(&mut self, needle: &[u8]) {
        let start = Instant::now();
        while !contains(&self.out, needle) {
            assert!(
                self.read_more(),
                "the editor ended before it wrote {needle:?}"
            );
            assert!(start.elapsed() < STUCK, "no {needle:?} in the output");
        }
    }

    /// Reads until the editor has gone, and gives all it wrote.
    fn finish(mut self) -> (Vec<u8>, PathBuf) {
        let start = Instant::now();
        while self.read_more() {
            assert!(start.elapsed() < STUCK, "the editor does not end");
        }
        self.child.wait().unwrap();
        (std::mem::take(&mut self.out), self.dir.clone())
    }

    fn signal(&self, signal: Signal) {
        let pid = Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        kill_process(pid, signal).unwrap();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    last(haystack, needle).is_some()
}

fn last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// Every mode that was switched on is off at the end: the last word on each is "off". The editor
/// writes "off" even for a mode it did not switch on, in case an earlier program left it so.
fn assert_restored(out: &[u8]) {
    for (on, off) in [
        (MOUSE_ON, MOUSE_OFF),
        (PASTE_ON, PASTE_OFF),
        (SCREEN_ON, SCREEN_OFF),
    ] {
        let off = last(out, off).unwrap_or_else(|| panic!("{off:?} is not in the output"));
        assert!(last(out, on).is_none_or(|on| on < off), "{on:?} comes last");
    }
}

#[test]
fn the_mouse_is_never_switched_on_unless_asked() {
    let mut session = Session::start("off", &[], "text\n");
    // What a program can write to the terminal is not read as keys: nothing is typed.
    session.send(b"\x1b[<0;5;1M\x1b[<0;5;1m\x1b[<65;5;5M");
    session.send(b"\x18");
    let (out, _) = session.finish();
    assert!(!contains(&out, MOUSE_ON));
    assert!(
        !contains(&out, b"Save modified buffer"),
        "a report was typed"
    );
    assert_restored(&out);
}

#[test]
fn the_terminal_is_restored_on_exit() {
    let mut session = Session::start("exit", &["--mouse"], "text\n");
    session.wait_for(MOUSE_ON);
    session.send(b"\x18");
    let (out, _) = session.finish();
    assert_restored(&out);
}

#[test]
fn the_terminal_is_restored_by_a_signal_and_the_text_is_saved() {
    for (name, signal) in [("hup", Signal::HUP), ("term", Signal::TERM)] {
        let mut session = Session::start(name, &["--mouse"], "text\n");
        session.wait_for(MOUSE_ON);
        session.send(b"x");
        session.wait_for(b"Modified");
        session.signal(signal);
        let (out, dir) = session.finish();
        assert_restored(&out);
        let saved = fs::read_to_string(dir.join("t.txt.save")).unwrap();
        assert_eq!(saved, "xtext\n", "{name}");
        let _ = fs::remove_dir_all(dir);
    }
}

#[test]
fn a_hostile_report_is_not_typed() {
    let mut session = Session::start("hostile", &["--mouse"], "text\n");
    session.wait_for(MOUSE_ON);
    let long = format!("\x1b[<{}M", "9;".repeat(300));
    for report in [
        "\x1b[<-1;5;5M",
        "\x1b[<0;5M",
        "\x1b[<0;5;5;5M",
        "\x1b[<0;99999999;5M",
        "\x1b[M ab",
        "\x1b[<0;5;5\x1b[<0;5;5M",
        long.as_str(),
    ] {
        session.send(report.as_bytes());
    }
    session.send(b"\x18");
    let (out, _) = session.finish();
    assert!(
        !contains(&out, b"Save modified buffer"),
        "a report was typed"
    );
    assert_restored(&out);
}
