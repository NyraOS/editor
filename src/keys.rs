// SPDX-License-Identifier: MIT
//! Keys: the bytes the terminal sends, read as keys the way xterm sends them.

use crate::buffer;
use crate::mouse::{self, Mouse, Read, Report};
use std::time::{Duration, Instant};

/// A key, with its modifiers; or a part of a bracketed paste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A typed character.
    Char(char),
    /// A typed byte that is not valid UTF-8.
    Byte(u8),
    /// Ctrl with a key, by the ASCII character it is written with: `^A` is 'A', `^/` is '_'.
    /// Tab is ^I and Enter is ^M, as in nano.
    Ctrl(char),
    /// Alt (Meta) with a character; letters are lowercase.
    Meta(char),
    /// Alt with Shift and a letter, by the lowercase letter.
    ShiftMeta(char),
    /// A named key, with Ctrl, Alt and Shift.
    Named(Named, Mods),
    /// A click or a turn of the wheel. It is never a key to bind and never text: the editor
    /// handles it apart from the keys, and only when it was started with the mouse on.
    Mouse(Mouse),
    /// A bracketed paste begins: what follows, up to `PasteEnd`, is text, not keys.
    PasteStart,
    /// Text of a paste: printable characters, tabs and "\n" for each line break.
    Paste(String),
    /// The paste is over. It is flawed when something that is not text came in it: the paste
    /// stops there, as in nano.
    PasteEnd { flawed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Backspace,
    /// Shift+Tab.
    BackTab,
    F(u8),
    /// A sequence the editor does not know.
    Unknown,
    /// What a key bound to a function the editor does not have yet gives, so that the menu that
    /// has the function can say so. A key the terminal sent that is not understood is `Unknown`,
    /// which nano ignores.
    NotAvailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// From xterm's modifier parameter: 1 + (1 for Shift, 2 for Alt, 4 for Ctrl).
    fn from_param(p: u32) -> Self {
        let bits = p.saturating_sub(1);
        Self {
            shift: bits & 1 != 0,
            alt: bits & 2 != 0,
            ctrl: bits & 4 != 0,
        }
    }
}

pub fn named(n: Named) -> Key {
    Key::Named(n, Mods::default())
}

/// Reads keys from the bytes as they arrive; keeps an incomplete sequence for the next bytes.
#[derive(Default)]
pub struct Reader {
    pending: Vec<u8>,
    /// Inside a bracketed paste, the text so far (nano holds it until the paste ends), and
    /// whether its last byte was a CR (a LF after it is the same line break).
    pasting: bool,
    held: String,
    after_cr: bool,
    /// Whether something that is not text was dropped from the paste.
    flawed: bool,
    last_input: Option<Instant>,
    /// Escape was the last key: the next one comes with Alt, as in nano.
    escape: bool,
    /// The left mouse button is down: a release after it is a click.
    pressed: bool,
    /// Inside a mouse report that was too long to read: what is left of it is dropped.
    dropping: bool,
}

impl Reader {
    /// Whether a paste has begun and not ended: nothing of it shows until it does.
    pub fn pasting(&self) -> bool {
        self.pasting
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        self.last_input = Some(Instant::now());
        let mut keys = Vec::new();
        let mut i = 0;
        while i < self.pending.len() && self.step(&mut i, &mut keys) {}
        self.pending.drain(..i);
        keys
    }

    /// Reads what is at `pending[*i..]` (a key, a report, a piece of a paste), moving `*i` past
    /// it. False when the rest is only the start of something and needs more bytes.
    fn step(&mut self, i: &mut usize, keys: &mut Vec<Key>) -> bool {
        if self.pasting {
            return self.paste(i, keys);
        }
        // The end of a paste that is not there: nothing to do, and nothing to say.
        if self.pending[*i..].starts_with(PASTE_END) {
            *i += PASTE_END.len();
            return true;
        }
        if self.dropping {
            self.drop_rest_of_report(i);
            return true;
        }
        if !self.rejoin_escape(*i) {
            return false;
        }
        // An Escape right before a report is an Escape of its own, not the start of
        // "Escape, Escape and a key" (which would leave the rest of the report to be typed).
        if self.pending[*i..].starts_with(b"\x1b\x1b[<") {
            self.escape = true;
            *i += 1;
            return true;
        }
        if let Some(len) = self.mouse_report(*i, keys) {
            *i += len;
            return len > 0;
        }
        self.read_key(i, keys)
    }

    /// Escape and the start of a report can come in two reads. The Escape was taken for a key of
    /// its own, so the report would be typed: put it back before the report. False: a `[` that
    /// may be the start of one has to wait for the next byte.
    fn rejoin_escape(&mut self, i: usize) -> bool {
        let rest = &self.pending[i..];
        if !self.escape {
            return true;
        }
        if rest.starts_with(b"[<") || rest.starts_with(b"[M") {
            self.pending.insert(i, 0x1b);
        }
        self.pending[i..] != b"["[..]
    }

    /// The rest of a mouse report that was too long, up to its final byte.
    fn drop_rest_of_report(&mut self, i: &mut usize) {
        match self.pending[*i] {
            0x20..=0x3f => *i += 1,
            0x40..=0x7e => {
                *i += 1;
                self.dropping = false;
            }
            _ => self.dropping = false,
        }
    }

    /// One key at `pending[*i..]`. False when it is not all there yet.
    fn read_key(&mut self, i: &mut usize, keys: &mut Vec<Key>) -> bool {
        let Some((key, len)) = parse(&self.pending[*i..]) else {
            return false;
        };
        *i += len;
        match key {
            Some(Key::PasteStart) => {
                self.escape = false;
                self.pasting = true;
                self.after_cr = false;
                self.flawed = false;
            }
            None => self.escape = true,
            Some(key) if self.escape => {
                self.escape = false;
                keys.push(with_alt(key));
            }
            Some(key) => keys.push(key),
        }
        true
    }

    /// A mouse report at `pending[i..]`: how many bytes it took (0: the report is not all there
    /// yet), with the key it makes, if any. None: not a report. Nothing of a report is ever read
    /// as another key: the ones of the old form (`ESC [ M` and three bytes) are dropped whole.
    fn mouse_report(&mut self, i: usize, keys: &mut Vec<Key>) -> Option<usize> {
        let rest = &self.pending[i..];
        if rest.starts_with(b"\x1b[M") || rest.starts_with(b"\x1b[<") {
            // A report is not a key to follow an Escape with.
            self.escape = false;
        }
        if rest.starts_with(b"\x1b[M") {
            return Some(mouse::old_report(rest));
        }
        if !rest.starts_with(b"\x1b[<") {
            return None;
        }
        match mouse::read_sgr(rest) {
            Read::Incomplete => Some(0),
            Read::Drop { len, more } => {
                self.dropping = more;
                Some(len)
            }
            Read::Report(report, len) => {
                match report {
                    Report::Press(..) => self.pressed = true,
                    Report::Release(col, row) if std::mem::take(&mut self.pressed) => {
                        keys.push(Key::Mouse(Mouse::Click { col, row }));
                    }
                    Report::Wheel(wheel) => keys.push(Key::Mouse(wheel)),
                    Report::Release(..) | Report::Other => keys.push(Key::Mouse(Mouse::Other)),
                }
                Some(len)
            }
        }
    }

    /// Reads the text of a paste from `pending[*i..]` as far as it goes. Returns false when the
    /// rest is only the start of something and needs more bytes.
    fn paste(&mut self, i: &mut usize, keys: &mut Vec<Key>) -> bool {
        let mut end = false;
        let mut complete = true;
        while *i < self.pending.len() {
            let b = &self.pending[*i..];
            if b.starts_with(PASTE_END) {
                *i += PASTE_END.len();
                end = true;
                break;
            }
            if PASTE_END.starts_with(b) {
                complete = false;
                break;
            }
            match b[0] {
                b'\r' | b'\n' => {
                    if !(b[0] == b'\n' && self.after_cr) {
                        self.held.push('\n');
                    }
                    self.after_cr = b[0] == b'\r';
                    *i += 1;
                    continue;
                }
                b'\t' | 0x20..=0x7e => {
                    self.held.push(char::from(b[0]));
                    *i += 1;
                }
                0x80.. => match parse_utf8(b) {
                    Some((Some(Key::Char(c)), len)) => {
                        self.held.push(c);
                        *i += len;
                    }
                    Some((_, len)) => {
                        self.held.push_str(&buffer::raw(b[0]));
                        *i += len;
                    }
                    None => {
                        complete = false;
                        break;
                    }
                },
                // Nothing in a paste may run as a key: what is not text is dropped.
                0x1b => match skip_escape(b) {
                    Some(len) => {
                        *i += len;
                        self.flawed = true;
                    }
                    None => {
                        complete = false;
                        break;
                    }
                },
                _ => {
                    *i += 1;
                    self.flawed = true;
                }
            }
            self.after_cr = false;
        }
        if end {
            self.finish_paste(keys);
        }
        complete
    }

    /// The paste is over, by its end marker or because the input stopped.
    fn finish_paste(&mut self, keys: &mut Vec<Key>) {
        let flawed = std::mem::take(&mut self.flawed);
        self.pasting = false;
        keys.push(Key::PasteStart);
        if !self.held.is_empty() {
            keys.push(Key::Paste(std::mem::take(&mut self.held)));
        }
        keys.push(Key::PasteEnd { flawed });
    }

    /// The bytes of the paste held so far.
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// How long until a paste that goes on without input is taken as over; `None` outside one.
    pub fn idle_wait(&self) -> Option<Duration> {
        let since = self.last_input?.elapsed();
        self.pasting.then(|| PASTE_IDLE.saturating_sub(since))
    }

    /// When no input came for `PASTE_IDLE` inside a paste, the paste ends: its end marker was
    /// lost, and the editor must not stay blind. What the paste held is never run as keys.
    pub fn idle(&mut self) -> Vec<Key> {
        let mut keys = Vec::new();
        if self.pasting && self.last_input.is_some_and(|t| t.elapsed() >= PASTE_IDLE) {
            self.pending.clear();
            self.flawed = true;
            self.finish_paste(&mut keys);
        }
        keys
    }
}

/// What ends a bracketed paste.
const PASTE_END: &[u8] = b"\x1b[201~";

/// How long a paste may go without input before it is taken as over.
const PASTE_IDLE: Duration = Duration::from_secs(2);

/// The length of the escape sequence at the start of `b`, which is dropped from a paste; `None`
/// when it is not complete yet.
fn skip_escape(b: &[u8]) -> Option<usize> {
    match b.get(1)? {
        b'[' => {
            // Parameters and intermediate bytes, then the final byte; anything else ends it, so
            // that it never swallows the end marker.
            let n = b[2..].iter().position(|c| !(0x20..=0x3f).contains(c));
            match n {
                Some(n) if (0x40..=0x7e).contains(&b[2 + n]) => Some(n + 3),
                Some(n) => Some(n + 2),
                None => None,
            }
        }
        b'O' => (b.len() >= 3).then_some(3),
        // A lone ESC, before something that is text or the end marker.
        0x1b | 0x80.. => Some(1),
        // ESC and a key: Alt and the key.
        _ => Some(2),
    }
}

fn with_alt(key: Key) -> Key {
    match key {
        Key::Char(c) if c.is_ascii_uppercase() => Key::ShiftMeta(c.to_ascii_lowercase()),
        Key::Char(c) => Key::Meta(c),
        Key::Named(n, mods) => Key::Named(n, Mods { alt: true, ..mods }),
        // Escape, Escape and a key: Ctrl and the key, as in nano.
        other => other,
    }
}

/// The first key in `b` and its length in bytes; the key is None for a lone Escape. None when
/// `b` holds only the start of a key.
fn parse(b: &[u8]) -> Option<(Option<Key>, usize)> {
    let first = b[0];
    let key = |k| Some((Some(k), 1));
    match first {
        0x1b => parse_escape(b),
        b'\r' => key(Key::Ctrl('M')),
        0x7f => key(named(Named::Backspace)),
        0x00 => key(Key::Ctrl('@')),
        0x01..=0x1f => key(Key::Ctrl(char::from(first + 0x40))),
        0x20..=0x7e => key(Key::Char(char::from(first))),
        _ => parse_utf8(b),
    }
}

fn parse_utf8(b: &[u8]) -> Option<(Option<Key>, usize)> {
    let len = match b[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 0,
    };
    if len > 0 && b.len() < len && b[1..].iter().all(|&c| c & 0xc0 == 0x80) {
        return None;
    }
    match std::str::from_utf8(&b[..len.min(b.len())])
        .ok()
        .and_then(|s| s.chars().next())
    {
        Some(c) if len > 0 => Some((Some(Key::Char(c)), len)),
        _ => Some((Some(Key::Byte(b[0])), 1)),
    }
}

fn parse_escape(b: &[u8]) -> Option<(Option<Key>, usize)> {
    match b.get(1) {
        None => Some((None, 1)),
        Some(b'[') => parse_csi(b),
        Some(b'O') => {
            // Escape and O: Alt+O. Another Escape after it starts the next sequence, and is not
            // part of this one.
            let Some(&c) = b.get(2).filter(|&&c| c != 0x1b) else {
                return Some((Some(Key::Meta('o')), 2));
            };
            let n = match c {
                b'A' => Named::Up,
                b'B' => Named::Down,
                b'C' => Named::Right,
                b'D' => Named::Left,
                b'H' => Named::Home,
                b'F' => Named::End,
                b'P'..=b'S' => Named::F(c - b'P' + 1),
                _ => Named::Unknown,
            };
            Some((Some(named(n)), 3))
        }
        // Escape, Escape and a key: Ctrl and the key (nano's way to type it without Ctrl).
        Some(0x1b) => match b.get(2) {
            None => Some((None, 1)),
            // Escape, Escape and "[" may be the start of a report: wait for the next byte.
            Some(b'[') if b.len() == 3 => None,
            Some(&c) if c.is_ascii_alphabetic() || (0x40..=0x5f).contains(&c) => {
                Some((Some(Key::Ctrl(char::from(c.to_ascii_uppercase()))), 3))
            }
            Some(_) => Some((None, 1)),
        },
        Some(_) => {
            let (key, len) = parse(&b[1..])?;
            Some((key.map(with_alt), 1 + len))
        }
    }
}

/// `ESC [ params final`.
fn parse_csi(b: &[u8]) -> Option<(Option<Key>, usize)> {
    // Parameter bytes, then the final byte. Any other byte (an Escape above all) ends the
    // sequence without being part of it: it starts the next one, as a report must not be eaten.
    let end = match b[2..].iter().position(|c| !(0x20..=0x3f).contains(c)) {
        None => return None,
        Some(n) if (0x40..=0x7e).contains(&b[2 + n]) => n + 2,
        Some(n) => return Some((Some(named(Named::Unknown)), n + 2)),
    };
    let params: Vec<u32> = std::str::from_utf8(&b[2..end])
        .unwrap_or("")
        .split(';')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let mods = Mods::from_param(params.get(1).copied().unwrap_or(1));
    let n = match (b[end], params[0]) {
        (b'A', _) => Named::Up,
        (b'B', _) => Named::Down,
        (b'C', _) => Named::Right,
        (b'D', _) => Named::Left,
        (b'H', _) | (b'~', 1 | 7) => Named::Home,
        (b'F', _) | (b'~', 4 | 8) => Named::End,
        (b'Z', _) => Named::BackTab,
        (b'P'..=b'S', _) => Named::F(b[end] - b'P' + 1),
        (b'~', 200) => return Some((Some(Key::PasteStart), end + 1)),
        (b'~', 2) => Named::Insert,
        (b'~', 3) => Named::Delete,
        (b'~', 5) => Named::PageUp,
        (b'~', 6) => Named::PageDown,
        (b'~', n @ 11..=15) => Named::F((n - 10) as u8),
        (b'~', n @ 17..=21) => Named::F((n - 11) as u8),
        (b'~', n @ 23..=24) => Named::F((n - 12) as u8),
        _ => Named::Unknown,
    };
    Some((Some(Key::Named(n, mods)), end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(b: &[u8]) -> Vec<Key> {
        Reader::default().feed(b)
    }

    #[test]
    fn plain_and_control() {
        assert_eq!(
            keys(b"a\r\t\x18\x7f\x1f"),
            [
                Key::Char('a'),
                Key::Ctrl('M'),
                Key::Ctrl('I'),
                Key::Ctrl('X'),
                named(Named::Backspace),
                Key::Ctrl('_'),
            ]
        );
        assert_eq!(keys("é日".as_bytes()), [Key::Char('é'), Key::Char('日')]);
    }

    #[test]
    fn sequences() {
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert_eq!(
            keys(b"\x1bOA\x1b[B\x1b[1;5D\x1b[3~\x1b[6~\x1bOP\x1b[24~"),
            [
                named(Named::Up),
                named(Named::Down),
                Key::Named(Named::Left, ctrl),
                named(Named::Delete),
                named(Named::PageDown),
                named(Named::F(1)),
                named(Named::F(12)),
            ]
        );
        assert_eq!(
            keys(b"\x1bw\x1bW\x1b/"),
            [Key::Meta('w'), Key::ShiftMeta('w'), Key::Meta('/')]
        );
    }

    #[test]
    fn split_input() {
        let mut r = Reader::default();
        assert_eq!(r.feed(b"\x1b"), []);
        assert_eq!(r.feed(b"u"), [Key::Meta('u')]);
        assert_eq!(r.feed(&"é".as_bytes()[..1]), []);
        assert_eq!(r.feed(&"é".as_bytes()[1..]), [Key::Char('é')]);
        assert_eq!(r.feed(b"\x1b[1;"), []);
        assert_eq!(
            r.feed(b"5C"),
            [Key::Named(
                Named::Right,
                Mods {
                    ctrl: true,
                    ..Mods::default()
                }
            )]
        );
        assert_eq!(r.feed(b"\xff"), [Key::Byte(0xff)]);
    }

    #[test]
    fn paste_is_held_until_it_ends() {
        let mut r = Reader::default();
        assert_eq!(r.feed(b"\x1b[200~ab\r\nc\td"), []);
        assert!(r.pasting());
        assert_eq!(r.feed(b"\x1b[20"), []);
        assert_eq!(
            r.feed(b"1~x"),
            [
                Key::PasteStart,
                Key::Paste("ab\nc\td".into()),
                Key::PasteEnd { flawed: false },
                Key::Char('x'),
            ]
        );
        assert!(!r.pasting());
    }

    #[test]
    fn what_is_not_text_is_dropped_from_a_paste_across_reads() {
        let mut r = Reader::default();
        assert_eq!(r.feed(b"\x1b[200~ab\x01\x1bx\x1b[1;5"), []);
        assert_eq!(r.feed(b"C\x1b[200~\x1b\x1bOA\x1b"), []);
        assert_eq!(
            r.feed(b"[201~z"),
            [
                Key::PasteStart,
                Key::Paste("ab".into()),
                Key::PasteEnd { flawed: true },
                Key::Char('z'),
            ]
        );
    }

    #[test]
    fn a_paste_without_its_end_marker_ends_when_the_input_stops() {
        let mut r = Reader::default();
        assert_eq!(r.feed(b"\x1b[200~ab\x0f"), []);
        assert_eq!(r.idle(), []);
        r.last_input = Some(Instant::now() - PASTE_IDLE);
        assert_eq!(
            r.idle(),
            [
                Key::PasteStart,
                Key::Paste("ab".into()),
                Key::PasteEnd { flawed: true },
            ]
        );
        assert_eq!(r.feed(b"\x0f"), [Key::Ctrl('O')]);
    }

    #[test]
    fn an_end_marker_outside_a_paste_is_ignored() {
        assert_eq!(keys(b"a\x1b[201~b"), [Key::Char('a'), Key::Char('b')]);
    }

    fn mouse_keys(b: &[u8]) -> Vec<Key> {
        keys(b)
    }

    #[test]
    fn a_press_and_a_release_are_a_click() {
        assert_eq!(
            mouse_keys(b"\x1b[<0;10;5M\x1b[<0;10;5m"),
            [Key::Mouse(Mouse::Click { col: 9, row: 4 })]
        );
        // The release decides the place, and the bytes may come in any pieces.
        let mut r = Reader::default();
        assert_eq!(r.feed(b"\x1b[<0;3;3M\x1b[<0;"), []);
        assert_eq!(r.feed(b"4;4"), []);
        assert_eq!(r.feed(b"m"), [Key::Mouse(Mouse::Click { col: 3, row: 3 })]);
        // A release with no press before it is no click.
        assert_eq!(mouse_keys(b"\x1b[<0;3;3m"), [Key::Mouse(Mouse::Other)]);
    }

    #[test]
    fn the_wheel_and_the_other_reports() {
        assert_eq!(
            mouse_keys(b"\x1b[<65;7;8M\x1b[<64;7;8M"),
            [
                Key::Mouse(Mouse::Wheel {
                    up: false,
                    col: 6,
                    row: 7
                }),
                Key::Mouse(Mouse::Wheel {
                    up: true,
                    col: 6,
                    row: 7
                }),
            ]
        );
        assert_eq!(
            mouse_keys(b"\x1b[<2;7;8M\x1b[<35;7;8M"),
            [Key::Mouse(Mouse::Other), Key::Mouse(Mouse::Other)]
        );
    }

    #[test]
    fn nothing_of_a_report_is_typed() {
        // Not exact, too long, of the old form: dropped whole, and the text after it is text.
        let long = format!("\x1b[<{}Mz", "1;".repeat(500));
        for report in [
            "\x1b[<-1;5;5Mz",
            "\x1b[<0;5Mz",
            "\x1b[<0;5;5;5Mz",
            "\x1b[<0;0;5Mz",
            "\x1b[<0;123456;5Mz",
            "\x1b[<0;5;5Az",
            "\x1b[M abz",
            long.as_str(),
        ] {
            assert_eq!(
                mouse_keys(report.as_bytes()),
                [Key::Char('z')],
                "{report:.20?}"
            );
        }
        // A sequence cut by another is dropped as far as it went.
        assert_eq!(
            mouse_keys(b"\x1b[<0;5\x1b[Az"),
            [Key::Named(Named::Up, Mods::default()), Key::Char('z')]
        );
    }

    #[test]
    fn a_long_report_is_dropped_across_reads() {
        let mut r = Reader::default();
        assert_eq!(
            r.feed(
                &b"\x1b[<"
                    .iter()
                    .chain(&[b'7'; 100])
                    .copied()
                    .collect::<Vec<u8>>()
            ),
            []
        );
        assert_eq!(r.feed(&[b'7'; 100]), []);
        assert_eq!(r.feed(b"7Mz"), [Key::Char('z')]);
    }

    #[test]
    fn a_report_does_not_make_the_next_key_an_alt_key() {
        assert_eq!(keys(b"\x1b\x1b[<0;1;1Ma"), [Key::Char('a')]);
    }

    const CLICK: &[u8] = b"\x1b[<0;5;5M\x1b[<0;5;5m";

    fn click() -> Key {
        Key::Mouse(Mouse::Click { col: 4, row: 4 })
    }

    #[test]
    fn a_report_after_a_split_escape_is_not_typed() {
        // The Escape in a read of its own, then the report in the next ones, cut anywhere.
        for pieces in [
            &[&b"\x1b"[..], b"[<0;5;5M\x1b[<0;5;5m"][..],
            &[b"\x1b\x1b", b"[<0;5;5M\x1b[<0;5;5m"],
            &[b"\x1b\x1b[", b"<0;5;5M\x1b[<0;5;5m"],
            &[b"\x1b", b"[", b"<0;5;5M\x1b[<0;5;5m"],
            &[b"\x1b", b"[<", b"0;5;5M\x1b[<0;5;5m"],
            &[b"\x1b\x1b", b"[", b"<0;5;5M", b"\x1b[<0;5;5m"],
        ] {
            let mut r = Reader::default();
            let got: Vec<Key> = pieces.iter().flat_map(|p| r.feed(p)).collect();
            assert_eq!(got, [click()], "{pieces:?}");
            // And the next key is a plain one.
            assert_eq!(r.feed(b"a"), [Key::Char('a')], "{pieces:?}");
        }
    }

    #[test]
    fn a_report_after_an_unfinished_sequence_is_not_typed() {
        for start in [
            "\x1b[", "\x1b[1", "\x1b[1;5", "\x1b[3", "\x1b[?", "\x1bO", "\x1b[M a", "\x1b[M",
        ] {
            for split in [false, true] {
                let mut r = Reader::default();
                let mut got = r.feed(start.as_bytes());
                if split {
                    got.extend(r.feed(&CLICK[..3]));
                    got.extend(r.feed(&CLICK[3..]));
                } else {
                    got.extend(r.feed(CLICK));
                }
                assert!(
                    got.iter().all(|k| !matches!(k, Key::Char(_))),
                    "{start:?} {split}: {got:?}"
                );
                assert_eq!(got.last(), Some(&click()), "{start:?} {split}: {got:?}");
            }
        }
    }

    #[test]
    fn an_escape_that_ends_a_sequence_starts_the_next() {
        assert_eq!(
            keys(b"\x1b[1;5\x1bOA"),
            [named(Named::Unknown), named(Named::Up)]
        );
        assert_eq!(keys(b"\x1bO\x1b[A"), [Key::Meta('o'), named(Named::Up)]);
    }
}
