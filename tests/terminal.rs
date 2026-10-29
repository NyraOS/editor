// SPDX-License-Identifier: MIT
//! The modes the editor switches on in the terminal (mouse reports, bracketed paste, the
//! alternate screen) are switched off again however it ends, and mouse reports are never typed.
//! The editor runs in a pseudo-terminal; the tests wait for output they expect, never for time.

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::ioctl_fionread;
use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, unlockpt};
use rustix::termios::{LocalModes, Winsize, tcgetattr, tcsetwinsize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::num::NonZeroU16;
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
        Self::start_with(name, args, text, &[])
    }

    /// The same, with environment variables set.
    fn start_with(name: &str, args: &[&str], text: &str, env: &[(&str, &str)]) -> Self {
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
            .envs(env.iter().copied())
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

    /// Sends `first`, waits until the editor has read it, and only then sends `second`: the editor
    /// gets them in two reads, however fast the machine is.
    fn send_split(&mut self, first: &[u8], second: &[u8]) {
        self.send(first);
        // The slave is opened for this check only: the master must see the end of the terminal
        // when the editor closes it, and an open slave here would hide it.
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let slave = ioctl_tiocgptpeer(&self.master, flags).unwrap();
        let start = Instant::now();
        while ioctl_fionread(&slave).unwrap() > 0 {
            assert!(start.elapsed() < STUCK, "the editor does not read");
            std::thread::yield_now();
        }
        drop(slave);
        self.send(second);
    }

    /// Waits until the screen satisfies `ready`, and gives it.
    fn wait_screen(&mut self, ready: impl Fn(&vt100::Screen) -> bool) -> vt100::Screen {
        let start = Instant::now();
        loop {
            let mut parser = vt100::Parser::new(
                NonZeroU16::new(24).unwrap(),
                NonZeroU16::new(80).unwrap(),
                0,
            );
            parser.process(&self.out);
            if ready(parser.screen()) {
                return parser.screen().clone();
            }
            assert!(
                self.read_more(),
                "the editor ended before the screen was right"
            );
            assert!(start.elapsed() < STUCK, "the screen is never right");
        }
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

    /// Reads until the editor has gone.
    fn run_to_end(&mut self) {
        let start = Instant::now();
        while self.read_more() {
            assert!(start.elapsed() < STUCK, "the editor does not end");
        }
        self.child.wait().unwrap();
    }

    /// Reads until the editor has gone, and gives all it wrote.
    fn finish(mut self) -> (Vec<u8>, PathBuf) {
        self.run_to_end();
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
fn m_m_asks_the_terminal_for_the_mouse_and_stops_asking() {
    let mut session = Session::start("toggle", &[], "text\n");
    session.wait_for(b"Read 1 line");
    session.send(b"\x1bm");
    session.wait_for(b"Mouse support enabled");
    session.wait_for(MOUSE_ON);
    session.send(b"\x1bm");
    session.wait_for(b"Mouse support disabled");
    // Written after the first M-M asked for the mouse, so the last word is "off".
    session.send(b"\x18");
    let (out, _) = session.finish();
    let on = last(&out, MOUSE_ON).expect("asked for the mouse");
    assert!(last(&out, MOUSE_OFF).is_some_and(|off| off > on));
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

/// The editor ends at once with `^X` when nothing was typed; typed text would ask to save it.
fn assert_nothing_typed(out: &[u8]) {
    assert!(
        !contains(out, b"Save modified buffer"),
        "a report was typed"
    );
}

#[test]
fn a_report_split_from_what_comes_before_is_not_typed() {
    let report: &[u8] = b"\x1b[<0;5;1M";
    // The first read ends in the middle of a key, and the report follows in the next one; also
    // the report itself cut at its start.
    let splits: [(&[u8], &[u8]); 10] = [
        (b"\x1b", b"[<0;5;1M"),
        (b"\x1b\x1b", b"[<0;5;1M"),
        (b"\x1b\x1b[", b"<0;5;1M"),
        (b"\x1b[", b"<0;5;1M"),
        (b"\x1b[<0;5", b";1M"),
        (b"\x1b[", report),
        (b"\x1b[1", report),
        (b"\x1b[1;5", report),
        (b"\x1b[3", report),
        (b"\x1bO", report),
    ];
    for (n, (first, second)) in splits.into_iter().enumerate() {
        let mut session = Session::start(&format!("split{n}"), &["--mouse"], "text\n");
        session.wait_for(MOUSE_ON);
        session.send_split(first, second);
        session.send(b"\x18");
        let (out, _) = session.finish();
        assert_nothing_typed(&out);
        assert_restored(&out);
    }
}

#[test]
fn a_report_after_the_old_form_is_not_typed() {
    let mut session = Session::start("old", &["--mouse"], "text\n");
    session.wait_for(MOUSE_ON);
    session.send_split(b"\x1b[M a", b"\x1b[<0;5;1M");
    session.send_split(b"\x1b[M", b"\x1b[<0;5;1M");
    session.send(b"\x18");
    let (out, _) = session.finish();
    assert_nothing_typed(&out);
}

#[test]
fn a_report_changes_nothing_when_the_mouse_is_off() {
    let text: String = (1..=60).map(|n| format!("line {n}\n")).collect();
    let mut session = Session::start("deaf", &[], &text);
    // The mark is set, so that a click would also extend a selection.
    session.send(b"\x1bA");
    session.wait_screen(|s| s.contents().contains("Mark Set"));
    let before = session.wait_screen(|_| true);
    let first_row = before.rows(0, 80).nth(1).unwrap();
    let cursor = before.cursor_position();
    // A click, its release and a turn of the wheel, as a program could write them; then ^N, which
    // moves the cursor by one line if the reports changed nothing before it.
    session.send(b"\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<65;5;5M\x0e");
    let after = session.wait_screen(|s| s.cursor_position() != cursor);
    assert_eq!(after.cursor_position(), (cursor.0 + 1, cursor.1));
    assert_eq!(after.rows(0, 80).nth(1).unwrap(), first_row, "it scrolled");
    assert!(!after.contents().contains("Modified"), "it was typed");
    assert!(after.contents().contains("Mark Set"), "the mark changed");
}

#[test]
fn the_terminal_is_restored_when_the_editor_panics() {
    let env = [("NYRA_EDITOR_TEST_PANIC", "1")];
    let mut session = Session::start_with("panic", &["--mouse"], "text\n", &env);
    session.run_to_end();
    assert_restored(&session.out);
    // The message comes after the restore, so that it is readable on the main screen.
    let message = last(&session.out, b"panic asked for").expect("no panic message");
    let screen = last(&session.out, SCREEN_OFF).unwrap();
    assert!(
        screen < message,
        "the message is lost on the alternate screen"
    );
    // Not raw any more: the terminal reads lines and echoes again.
    let modes = tcgetattr(&session.master).unwrap().local_modes;
    assert!(modes.contains(LocalModes::ICANON | LocalModes::ECHO));
}
