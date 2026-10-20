// SPDX-License-Identifier: MIT
//! Keys: the bytes the terminal sends, read as keys the way xterm sends them.

/// A key, with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// A named key, with Ctrl, Alt and Shift.
    Named(Named, Mods),
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
    /// Escape was the last key: the next one comes with Alt, as in nano.
    escape: bool,
}

impl Reader {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            let Some((key, len)) = parse(&self.pending[i..]) else {
                break;
            };
            i += len;
            match key {
                None => self.escape = true,
                Some(key) if self.escape => {
                    self.escape = false;
                    keys.push(with_alt(key));
                }
                Some(key) => keys.push(key),
            }
        }
        self.pending.drain(..i);
        keys
    }
}

fn with_alt(key: Key) -> Key {
    match key {
        Key::Char(c) => Key::Meta(c.to_ascii_lowercase()),
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
            let Some(&c) = b.get(2) else {
                // Escape and O: Alt+O.
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
    let end = b[2..].iter().position(|c| (0x40..=0x7e).contains(c))? + 2;
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
            [Key::Meta('w'), Key::Meta('w'), Key::Meta('/')]
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
}
