// SPDX-License-Identifier: MIT
//! Searching the text, and reading the answer of "go to line".

use crate::buffer::Buffer;
use crate::screen::width_at;
use regex::{Regex, RegexBuilder};

/// What is searched for: a text, or a regular expression.
pub struct Pattern {
    chars: Vec<char>,
    case_sensitive: bool,
    regex: Option<Regex>,
}

/// One character as the search compares it.
fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// The byte offset of a character index in `text`.
fn byte_of(text: &str, x: usize) -> usize {
    text.char_indices().nth(x).map_or(text.len(), |(i, _)| i)
}

/// A match in a line: where it starts and how long it is, in characters.
type Span = (usize, usize);

impl Pattern {
    /// The error is what nano shows after `Bad regex "..."`.
    pub fn new(text: &str, case_sensitive: bool, regexp: bool) -> Result<Self, String> {
        let regex = if regexp {
            let ere = translate(text)?;
            let built = RegexBuilder::new(&ere)
                .case_insensitive(!case_sensitive)
                .build();
            Some(built.map_err(|e| match e {
                regex::Error::CompiledTooBig(_) => "Regular expression too big".to_string(),
                _ => "Invalid regular expression".to_string(),
            })?)
        } else {
            None
        };
        Ok(Self {
            chars: text.chars().map(|c| fold(c, case_sensitive)).collect(),
            case_sensitive,
            regex,
        })
    }

    /// The first match that starts at `from` or later.
    pub fn first_from(&self, line: &str, from: usize) -> Option<Span> {
        if let Some(regex) = &self.regex {
            if from > line.chars().count() {
                return None;
            }
            let m = regex.find_at(line, byte_of(line, from))?;
            let at = line[..m.start()].chars().count();
            return Some((at, line[m.start()..m.end()].chars().count()));
        }
        let folded = self.folded(line);
        let len = self.chars.len();
        (from..folded.len())
            .find(|&i| folded.get(i..i + len) == Some(&self.chars[..]))
            .map(|i| (i, len))
    }

    /// The last match that starts before `before`.
    pub fn last_before(&self, line: &str, before: usize) -> Option<Span> {
        if self.regex.is_some() {
            // Each next match is looked for one character after the start of the last one, as
            // the literal search does: the matches may overlap.
            let (mut last, mut from) = (None, 0);
            while let Some(found @ (at, _)) = self.first_from(line, from) {
                if at >= before {
                    break;
                }
                last = Some(found);
                from = at + 1;
            }
            return last;
        }
        let folded = self.folded(line);
        let len = self.chars.len();
        (0..before.min(folded.len()))
            .rev()
            .find(|&i| folded.get(i..i + len) == Some(&self.chars[..]))
            .map(|i| (i, len))
    }

    fn folded(&self, line: &str) -> Vec<char> {
        line.chars().map(|c| fold(c, self.case_sensitive)).collect()
    }

    /// What replaces the match of `len` characters at `at` in `line`: `with`, in which `\1` to
    /// `\9` stand for the groups of a regular expression that has them, as in nano.
    pub fn replacement(&self, line: &str, at: usize, with: &str) -> String {
        let Some(regex) = &self.regex else {
            return with.to_string();
        };
        let Some(groups) = regex.captures_at(line, byte_of(line, at)) else {
            return with.to_string();
        };
        let mut out = String::new();
        let mut chars = with.chars().peekable();
        while let Some(c) = chars.next() {
            let group = match chars.peek() {
                Some(d @ '1'..='9') if c == '\\' => {
                    Some(*d as usize - '0' as usize).filter(|&n| n < groups.len())
                }
                _ => None,
            };
            match group {
                Some(n) => {
                    chars.next();
                    out.push_str(groups.get(n).map_or("", |g| g.as_str()));
                }
                None => out.push(c),
            }
        }
        out
    }
}

/// Where a search found the pattern.
pub struct Found {
    pub line: usize,
    pub x: usize,
    /// How many characters it is.
    pub len: usize,
    /// The search went past the end (or the start) of the text and on from the other end.
    pub wrapped: bool,
}

/// The next match after the cursor (forward) or before it (backwards), wrapping around: a match
/// at the cursor itself is found last, when it is the only one.
pub fn find(buf: &Buffer, pattern: &Pattern, backwards: bool) -> Option<Found> {
    let (here, x) = (buf.line, buf.x);
    let lines = buf.lines();
    let found = |line, (x, len), wrapped| {
        Some(Found {
            line,
            x,
            len,
            wrapped,
        })
    };
    if backwards {
        for n in (0..=here).rev() {
            let before = if n == here { x } else { usize::MAX };
            if let Some(m) = pattern.last_before(&buf.line_text(n), before) {
                return found(n, m, false);
            }
        }
        for n in (here..lines).rev() {
            if let Some(m) = pattern.last_before(&buf.line_text(n), usize::MAX) {
                return found(n, m, true);
            }
        }
    } else {
        for n in here..lines {
            let from = if n == here { x + 1 } else { 0 };
            if let Some(m) = pattern.first_from(&buf.line_text(n), from) {
                return found(n, m, false);
            }
        }
        for n in 0..=here {
            if let Some(m) = pattern.first_from(&buf.line_text(n), 0) {
                return found(n, m, true);
            }
        }
    }
    None
}

/// A match in the text: the character index where it starts, and its length.
pub type Hit = (usize, usize);

/// The first match that starts at `from` or later, and before `limit` (character indexes in the
/// whole text).
pub fn match_after(buf: &Buffer, pattern: &Pattern, from: usize, limit: usize) -> Option<Hit> {
    let first = buf.text.char_to_line(from.min(buf.text.len_chars()));
    for n in (first..buf.lines()).filter(|&n| !after_last_newline(buf, n)) {
        let line_start = buf.text.line_to_char(n);
        let x = if n == first { from - line_start } else { 0 };
        if line_start + x >= limit {
            return None;
        }
        if let Some((at, len)) = pattern.first_from(&buf.line_text(n), x) {
            return (line_start + at < limit).then_some((line_start + at, len));
        }
    }
    None
}

/// The empty line after the final line end: nano does not match anything in it.
fn after_last_newline(buf: &Buffer, n: usize) -> bool {
    n > 0 && n + 1 == buf.lines() && buf.line_len(n) == 0
}

/// The last match that starts before `before` and at `floor` or later.
pub fn match_before(buf: &Buffer, pattern: &Pattern, before: usize, floor: usize) -> Option<Hit> {
    let last = buf.text.char_to_line(before.min(buf.text.len_chars()));
    for n in (0..=last).rev().filter(|&n| !after_last_newline(buf, n)) {
        let line_start = buf.text.line_to_char(n);
        let x = if n == last {
            before - line_start
        } else {
            usize::MAX
        };
        if let Some((at, len)) = pattern.last_before(&buf.line_text(n), x) {
            return (line_start + at >= floor).then_some((line_start + at, len));
        }
        if line_start < floor {
            return None;
        }
    }
    None
}

/// The most a repetition may count in nano's regular expressions (`RE_DUP_MAX`).
const REPEAT_MAX: usize = 0x7fff;

/// The one error nano does not give: its engine takes back-references in a pattern, ours does not.
pub const BACKREFERENCE: &str = "Back-references in the pattern are not supported";

const NO_REPEAT: &str = "Invalid preceding regular expression";
const OPEN_BRACKET: &str = "Unmatched [, [^, [:, [., or [=";

/// The characters of a POSIX class, written for the inside of a bracket expression of the
/// `regex` crate.
fn class(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => r"\p{L}",
        "digit" => "0-9",
        "alnum" => r"\p{L}\p{Nd}",
        "upper" => r"\p{Lu}",
        "lower" => r"\p{Ll}",
        "space" => r"\s",
        "blank" => r"\x20\x09",
        "punct" => r"\p{P}\p{S}",
        "print" => r"\P{C}",
        "graph" => r"[^\p{C}\s]",
        "cntrl" => r"\p{Cc}",
        "xdigit" => "0-9A-Fa-f",
        _ => return None,
    })
}

/// A character inside a bracket expression of the `regex` crate: every ASCII character that is
/// not a letter or a digit as its code, as it can be special in there.
fn class_char(out: &mut String, c: char) {
    if c.is_ascii() && !c.is_ascii_alphanumeric() {
        out.push_str(&format!(r"\x{{{:x}}}", c as u32));
    } else {
        out.push(c);
    }
}

/// The bracket expression that starts at `chars[at]` (the `[`), translated to `out`; returns
/// where it ends.
fn bracket(chars: &[char], at: usize, out: &mut String) -> Result<usize, String> {
    let mut i = at + 1;
    out.push('[');
    if chars.get(i) == Some(&'^') {
        out.push('^');
        i += 1;
    }
    let first = i;
    // The one character of "[.x.]" and "[=x=]", the end of the bracket expression and the
    // character it stands for.
    let single = |i: usize, mark: char| -> Result<(usize, char), String> {
        let rest = &chars[i + 2..];
        let end = rest
            .windows(2)
            .position(|w| w == [mark, ']'])
            .ok_or(OPEN_BRACKET)?;
        match rest[..end] {
            [c] => Ok((i + 2 + end + 2, c)),
            _ => Err("Invalid collation character".to_string()),
        }
    };
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(if i == first {
                "Invalid regular expression"
            } else {
                OPEN_BRACKET
            }
            .to_string());
        };
        if c == ']' && i > first {
            out.push(']');
            return Ok(i + 1);
        }
        let (low, next) = match (c, chars.get(i + 1)) {
            ('[', Some(':')) => {
                let rest = &chars[i + 2..];
                let end = rest
                    .windows(2)
                    .position(|w| w == [':', ']'])
                    .ok_or(OPEN_BRACKET)?;
                let name: String = rest[..end].iter().collect();
                out.push_str(class(&name).ok_or("Invalid character class name")?);
                i += 2 + end + 2;
                continue;
            }
            ('[', Some(&mark @ ('.' | '='))) => {
                let (next, low) = single(i, mark)?;
                (low, next)
            }
            _ => (c, i + 1),
        };
        class_char(out, low);
        i = next;
        if chars.get(i) == Some(&'-') && chars.get(i + 1).is_some_and(|&e| e != ']') {
            let (high, after) = match (chars[i + 1], chars.get(i + 2)) {
                ('[', Some(&mark @ ('.' | '='))) => {
                    let (after, high) = single(i + 1, mark)?;
                    (high, after)
                }
                (e, _) => (e, i + 2),
            };
            if high < low {
                return Err("Invalid range end".to_string());
            }
            out.push('-');
            class_char(out, high);
            i = after;
        }
    }
}

/// The interval that starts at `chars[at]` (the `{`), as the `regex` crate writes it; returns
/// where it ends.
fn interval(chars: &[char], at: usize) -> Result<(String, usize), String> {
    let close = chars[at..]
        .iter()
        .position(|&c| c == '}')
        .ok_or("Unmatched \\{")?;
    let body: String = chars[at + 1..at + close].iter().collect();
    let bad = || "Invalid content of \\{\\}".to_string();
    let number = |s: &str| -> Result<Option<usize>, String> {
        if s.is_empty() {
            Ok(None)
        } else if s.bytes().all(|b| b.is_ascii_digit()) {
            // Longer than any number that is allowed.
            Ok(Some(s.parse().unwrap_or(usize::MAX)))
        } else {
            Err(bad())
        }
    };
    let (min, max) = match body.split_once(',') {
        None => (
            number(&body)?.ok_or_else(bad)?,
            Some(number(&body)?.ok_or_else(bad)?),
        ),
        Some((min, max)) => (number(min)?.unwrap_or(0), number(max)?),
    };
    if max.is_some_and(|max| max < min) {
        return Err(bad());
    }
    if min.max(max.unwrap_or(0)) > REPEAT_MAX {
        return Err("Regular expression too big".to_string());
    }
    let max = max.map_or(String::new(), |m| m.to_string());
    Ok((format!("{{{min},{max}}}"), at + close + 1))
}

/// An extended POSIX regular expression, as GNU nano reads it, written for the `regex` crate;
/// the error is the text nano shows for the same mistake. What cannot be translated is listed in
/// docs/REGEX.md.
fn translate(ere: &str) -> Result<String, String> {
    let chars: Vec<char> = ere.chars().collect();
    let mut out = String::new();
    // Where in `out` the last thing a repetition can follow starts, and whether it is repeated
    // already: `a*?` is a lazy repetition for the `regex` crate, and `(a*)?` in POSIX.
    let mut atom: Option<(usize, bool)> = None;
    let (mut open, mut closed) = (Vec::new(), 0);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let start = out.len();
        match c {
            '\\' => {
                let c = *chars.get(i).ok_or("Trailing backslash")?;
                i += 1;
                match c {
                    'w' | 'W' | 's' | 'S' => {
                        out.push('\\');
                        out.push(c);
                    }
                    '<' | '>' | 'b' | 'B' => {
                        out.push('\\');
                        out.push(c);
                        atom = None;
                        continue;
                    }
                    '`' => out.push_str(r"\A"),
                    '\'' => out.push_str(r"\z"),
                    '1'..='9' if c as usize - '0' as usize > closed => {
                        return Err("Invalid back reference".into());
                    }
                    '1'..='9' => {
                        return Err(BACKREFERENCE.into());
                    }
                    c => out.push_str(&regex::escape(&c.to_string())),
                }
            }
            '(' => {
                if matches!(chars.get(i), Some('?' | '*' | '+')) {
                    return Err(NO_REPEAT.into());
                }
                open.push(start);
                out.push('(');
                atom = None;
                continue;
            }
            ')' => match open.pop() {
                Some(from) => {
                    out.push(')');
                    closed += 1;
                    atom = Some((from, false));
                    continue;
                }
                None => out.push_str(r"\)"),
            },
            '|' => {
                out.push('|');
                atom = None;
                continue;
            }
            '^' | '$' => {
                out.push(c);
                atom = None;
                continue;
            }
            '.' => out.push('.'),
            '[' => i = bracket(&chars, i - 1, &mut out)?,
            '*' | '+' | '?' | '{' => {
                let (from, repeated) = atom.ok_or(NO_REPEAT)?;
                let repeat = if c == '{' {
                    let (repeat, end) = interval(&chars, i - 1)?;
                    i = end;
                    repeat
                } else {
                    c.to_string()
                };
                if repeated {
                    out.insert_str(from, "(?:");
                    out.push(')');
                }
                out.push_str(&repeat);
                atom = Some((from, true));
                continue;
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        atom = Some((start, false));
    }
    if !open.is_empty() {
        return Err("Unmatched ( or \\(".into());
    }
    Ok(out)
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
        let p = Pattern::new("aa", false, false).unwrap();
        assert_eq!(at(find(&b, &p, false)), Some((1, 1, false)));
        let b = buffer("aaaa\nxaa\n", 1, 1);
        assert_eq!(at(find(&b, &p, false)), Some((0, 0, true)));
        let b = buffer("x\naa\n", 1, 0);
        assert_eq!(at(find(&b, &p, false)), Some((1, 0, true)));
    }

    #[test]
    fn backwards_finds_matches_that_start_before_the_cursor() {
        let b = buffer("aaaa\n", 0, 1);
        let p = Pattern::new("aa", false, false).unwrap();
        assert_eq!(at(find(&b, &p, true)), Some((0, 0, false)));
        let b = buffer("aaaa\n", 0, 0);
        assert_eq!(at(find(&b, &p, true)), Some((0, 2, true)));
    }

    #[test]
    fn case_and_missing() {
        let b = buffer("Cat\n", 0, 0);
        assert!(find(&b, &Pattern::new("CAT", false, false).unwrap(), false).is_some());
        assert!(find(&b, &Pattern::new("cat", true, false).unwrap(), false).is_none());
        assert!(find(&b, &Pattern::new("dog", false, false).unwrap(), true).is_none());
    }

    fn first(pattern: &str, text: &str) -> Option<(usize, usize)> {
        Pattern::new(pattern, true, true)
            .unwrap()
            .first_from(text, 0)
    }

    #[test]
    fn extended_expressions_are_translated() {
        assert_eq!(first("[[:alpha:]]+", "12abc3"), Some((2, 3)));
        assert_eq!(first("^a|c$", "bac"), Some((2, 1)));
        assert_eq!(first("a{2,3}", "aaaa"), Some((0, 3)));
        assert_eq!(first(r"\<b", "ab b"), Some((3, 1)));
        assert_eq!(first("a{,2}", "aaa"), Some((0, 2)));
        assert_eq!(first("a)", "a)"), Some((0, 2)));
        assert_eq!(first("[]a]+", "x]a"), Some((1, 2)));
        assert_eq!(first(r"\.", "a.b"), Some((1, 1)));
    }

    #[test]
    fn bad_expressions_give_the_message_of_nano() {
        for (pattern, message) in [
            ("[", "Invalid regular expression"),
            ("[a", "Unmatched [, [^, [:, [., or [="),
            ("(a", "Unmatched ( or \\("),
            ("a{1", "Unmatched \\{"),
            ("*a", "Invalid preceding regular expression"),
            ("a{3,2}", "Invalid content of \\{\\}"),
            ("[[:foo:]]", "Invalid character class name"),
            ("a\\", "Trailing backslash"),
            ("[b-a]", "Invalid range end"),
            ("a{99999}", "Regular expression too big"),
            ("a\\1", "Invalid back reference"),
        ] {
            assert_eq!(
                Pattern::new(pattern, true, true).err().as_deref(),
                Some(message)
            );
        }
    }

    #[test]
    fn replacements_expand_groups() {
        let p = Pattern::new("(a)(b)?", true, true).unwrap();
        assert_eq!(p.replacement("xa", 1, r"[\2\1\3\\]"), r"[a\3\\]");
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
