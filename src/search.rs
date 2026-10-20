// SPDX-License-Identifier: MIT
//! Searching the text, and reading the answer of "go to line".

use crate::buffer::Buffer;
use crate::screen::width_at;

/// What is searched for, and whether upper and lower case differ.
pub struct Pattern {
    chars: Vec<char>,
    case_sensitive: bool,
}

/// One character as the search compares it.
fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

impl Pattern {
    pub fn new(text: &str, case_sensitive: bool) -> Self {
        Self {
            chars: text.chars().map(|c| fold(c, case_sensitive)).collect(),
            case_sensitive,
        }
    }

    fn line(&self, text: String) -> Vec<char> {
        text.chars().map(|c| fold(c, self.case_sensitive)).collect()
    }

    fn matches_at(&self, line: &[char], at: usize) -> bool {
        line.get(at..at + self.chars.len()) == Some(&self.chars[..])
    }

    /// The first match that starts at `from` or later.
    fn first_from(&self, line: &[char], from: usize) -> Option<usize> {
        (from..line.len()).find(|&i| self.matches_at(line, i))
    }

    /// The last match that starts before `before`.
    fn last_before(&self, line: &[char], before: usize) -> Option<usize> {
        (0..before.min(line.len()))
            .rev()
            .find(|&i| self.matches_at(line, i))
    }
}

/// Where a search found the pattern.
pub struct Found {
    pub line: usize,
    pub x: usize,
    /// The search went past the end (or the start) of the text and on from the other end.
    pub wrapped: bool,
}

/// The next match after the cursor (forward) or before it (backwards), wrapping around: a match
/// at the cursor itself is found last, when it is the only one.
pub fn find(buf: &Buffer, pattern: &Pattern, backwards: bool) -> Option<Found> {
    let (here, x) = (buf.line, buf.x);
    let line = |n: usize| pattern.line(buf.line_text(n));
    let lines = buf.lines();
    let found = |line, x, wrapped| Some(Found { line, x, wrapped });
    if backwards {
        for n in (0..=here).rev() {
            let before = if n == here { x } else { usize::MAX };
            if let Some(x) = pattern.last_before(&line(n), before) {
                return found(n, x, false);
            }
        }
        for n in (here..lines).rev() {
            if let Some(x) = pattern.last_before(&line(n), usize::MAX) {
                return found(n, x, true);
            }
        }
    } else {
        for n in here..lines {
            let from = if n == here { x + 1 } else { 0 };
            if let Some(x) = pattern.first_from(&line(n), from) {
                return found(n, x, false);
            }
        }
        for n in 0..=here {
            if let Some(x) = pattern.first_from(&line(n), 0) {
                return found(n, x, true);
            }
        }
    }
    None
}

/// `text` as it is shown in a message or a prompt: cut to `cols` columns, with "..." when it
/// was wider.
pub fn shorten(text: &str, cols: usize) -> String {
    let mut width = 0;
    let mut kept = String::new();
    for c in text.chars() {
        width += width_at(c, 0, 1);
        if width > cols {
            kept.push_str("...");
            break;
        }
        kept.push(c);
    }
    kept
}

/// A number as C's `strtol` reads it, the whole text: blanks, a sign, digits.
fn number(text: &str) -> Option<i64> {
    let text = text.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The answer of "go to line": `LINE`, `LINE,COLUMN` or `,COLUMN` (the separator may also be a
/// point or a colon). Each is None when left out; None when the answer is not like that.
pub fn position(answer: &str) -> Option<(Option<i64>, Option<i64>)> {
    let Some(at) = answer.find([',', '.', ':']) else {
        return Some((Some(number(answer)?), None));
    };
    let (line, column) = (&answer[..at], &answer[at + 1..]);
    let line = if line.is_empty() {
        None
    } else {
        Some(number(line)?)
    };
    Some((line, Some(number(column)?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Format;

    fn buffer(text: &str, line: usize, x: usize) -> Buffer {
        let mut b = Buffer::new(text, Format::Unix);
        b.line = line;
        b.x = x;
        b
    }

    fn at(found: Option<Found>) -> Option<(usize, usize, bool)> {
        found.map(|f| (f.line, f.x, f.wrapped))
    }

    #[test]
    fn forward_starts_after_the_cursor_and_wraps() {
        let b = buffer("aaaa\nxaa\n", 0, 2);
        let p = Pattern::new("aa", false);
        assert_eq!(at(find(&b, &p, false)), Some((1, 1, false)));
        let b = buffer("aaaa\nxaa\n", 1, 1);
        assert_eq!(at(find(&b, &p, false)), Some((0, 0, true)));
        let b = buffer("x\naa\n", 1, 0);
        assert_eq!(at(find(&b, &p, false)), Some((1, 0, true)));
    }

    #[test]
    fn backwards_finds_matches_that_start_before_the_cursor() {
        let b = buffer("aaaa\n", 0, 1);
        let p = Pattern::new("aa", false);
        assert_eq!(at(find(&b, &p, true)), Some((0, 0, false)));
        let b = buffer("aaaa\n", 0, 0);
        assert_eq!(at(find(&b, &p, true)), Some((0, 2, true)));
    }

    #[test]
    fn case_and_missing() {
        let b = buffer("Cat\n", 0, 0);
        assert!(find(&b, &Pattern::new("CAT", false), false).is_some());
        assert!(find(&b, &Pattern::new("cat", true), false).is_none());
        assert!(find(&b, &Pattern::new("dog", false), true).is_none());
    }

    #[test]
    fn shortening() {
        assert_eq!(shorten("abcd", 4), "abcd");
        assert_eq!(shorten("abcde", 4), "abcd...");
        assert_eq!(shorten("日本語", 4), "日本...");
    }

    #[test]
    fn positions() {
        assert_eq!(position("3"), Some((Some(3), None)));
        assert_eq!(position(" +3"), Some((Some(3), None)));
        assert_eq!(position("-2,-3"), Some((Some(-2), Some(-3))));
        assert_eq!(position(",4"), Some((None, Some(4))));
        assert_eq!(position("2:4"), Some((Some(2), Some(4))));
        assert_eq!(position("2.4"), Some((Some(2), Some(4))));
        for bad in [
            "x",
            "1,",
            "1 2",
            "2 ",
            "1,2,3",
            "3;2",
            "- 3",
            "+-3",
            "",
            "99999999999999999999",
        ] {
            assert_eq!(position(bad), None, "{bad:?}");
        }
    }
}
