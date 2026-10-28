// SPDX-License-Identifier: MIT
//! The text being edited and the cursor in it.
//!
//! The text is a rope with "\n" between lines, like the file without its line ends converted.
//! When the file ends with a newline, the last line is empty: as in nano, the cursor can go
//! there, and typing on it adds a new empty last line.

use crate::screen::{is_zero_width, width_at};
use ropey::Rope;

/// The line ends of the file, kept when it is saved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Unix,
    Dos,
    Mac,
}

/// A byte of the file that is not valid UTF-8 is kept as two characters, so that it survives
/// editing and saving unchanged: a private-use character for the byte (U+10FE00 + the byte), then
/// `MARK`, a variation selector, which takes no column and goes with it like a combining mark.
/// Each of the two on its own is itself. A real `MARK` right after one of those private-use
/// characters (in a file, or put there by an edit) would read as a byte, so it is kept as its
/// four bytes instead.
pub const RAW_BASE: u32 = 0x10_FE00;
pub const MARK: char = '\u{E01EF}';

/// The two characters that stand for a byte that is not valid UTF-8.
pub fn raw(b: u8) -> String {
    let c = char::from_u32(RAW_BASE + u32::from(b)).unwrap_or(char::REPLACEMENT_CHARACTER);
    [c, MARK].into_iter().collect()
}

/// The byte `c` stands for when a `MARK` follows it.
pub fn raw_byte(c: char) -> Option<u8> {
    (c as u32)
        .checked_sub(RAW_BASE)
        .and_then(|b| u8::try_from(b).ok())
}

/// `MARK` as its four bytes, for a real one that would otherwise read as part of a byte.
fn escaped_mark() -> String {
    MARK.to_string().bytes().map(raw).collect()
}

/// Bytes to text: invalid UTF-8 becomes raw bytes, so that `encode` gives back exactly the same
/// bytes.
pub fn decode(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        // The usual case, without a copy.
        Ok(s) if !s.contains(MARK) => s,
        Ok(s) => decode_slow(s.as_bytes()),
        Err(e) => decode_slow(e.as_bytes()),
    }
}

fn decode_slow(bytes: &[u8]) -> String {
    fn push(out: &mut String, s: &str) {
        for c in s.chars() {
            if c == MARK && out.chars().next_back().and_then(raw_byte).is_some() {
                out.push_str(&escaped_mark());
            } else {
                out.push(c);
            }
        }
    }
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                push(&mut out, s);
                return out;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                push(&mut out, std::str::from_utf8(good).unwrap_or_default());
                let n = e.error_len().unwrap_or(bad.len());
                out.extend(bad[..n].iter().copied().map(raw));
                rest = &bad[n..];
            }
        }
    }
}

/// Text to bytes: the inverse of `decode`.
pub fn encode(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match raw_byte(c) {
            Some(b) if chars.peek() == Some(&MARK) => {
                chars.next();
                out.push(b);
            }
            _ => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    out
}

/// The format of the file's line ends, from the first one, and the text with them made "\n".
pub fn convert_line_ends(text: String) -> (String, Format) {
    let Some(i) = text.find(['\r', '\n']) else {
        return (text, Format::Unix);
    };
    match &text.as_bytes()[i..] {
        [b'\r', b'\n', ..] => (text.replace("\r\n", "\n"), Format::Dos),
        [b'\r', ..] => (text.replace('\r', "\n"), Format::Mac),
        _ => (text, Format::Unix),
    }
}

/// What an edit was, as nano names it in "Undid ..." and "Redid ...".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Add,
    Enter,
    Back,
    Del,
    Join,
    Cut,
    /// `-Z`: the selection erased.
    Zap,
    Paste,
    /// A file read into the text.
    Insert,
    Replace,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Add => "addition",
            Kind::Enter => "line break",
            Kind::Back | Kind::Del => "deletion",
            Kind::Join => "line join",
            Kind::Cut => "cut",
            Kind::Zap => "erasure",
            Kind::Paste => "paste",
            Kind::Insert => "insertion",
            Kind::Replace => "replacement",
        }
    }
}

/// One step of undo: at `at` (a character index), `old` was replaced by `new`. The cursor
/// (character indexes) was at `before` and ended at `after`.
struct Edit {
    kind: Kind,
    at: usize,
    old: String,
    new: String,
    before: usize,
    after: usize,
}

/// The mark that starts a selection, a character index. A soft one comes from selecting with
/// Shift and goes with the next key that moves or types; a hard one (M-A) stays until it is
/// unset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub at: usize,
    pub soft: bool,
}

pub struct Buffer {
    pub text: Rope,
    /// The cursor: its line, and its position in the line in characters.
    pub line: usize,
    pub x: usize,
    /// The screen column the cursor keeps when it moves up and down.
    pub want: usize,
    /// The first line on the screen.
    pub top: usize,
    /// With soft wrapping: the screen column in that line where its first row on the screen
    /// starts.
    pub top_col: usize,
    pub modified: bool,
    pub format: Format,
    pub mark: Option<Mark>,
    /// The edits made, and how many of them are in effect: the rest can be redone.
    edits: Vec<Edit>,
    done: usize,
    /// `done` when the text was saved or read; None when that state can no longer be reached.
    saved: Option<usize>,
    /// Whether the next edit may join the last one (not after an undo, a redo or a save).
    open: bool,
}

impl Buffer {
    pub fn new(text: &str, format: Format) -> Self {
        Self {
            text: Rope::from_str(text),
            line: 0,
            x: 0,
            want: 0,
            top: 0,
            top_col: 0,
            modified: false,
            format,
            mark: None,
            edits: Vec::new(),
            done: 0,
            saved: Some(0),
            open: false,
        }
    }

    pub fn lines(&self) -> usize {
        self.text.len_lines()
    }

    /// A line without its "\n".
    pub fn line_text(&self, line: usize) -> String {
        let mut s = String::from(self.text.line(line));
        if s.ends_with('\n') {
            s.pop();
        }
        s
    }

    /// The number of characters in a line, without its "\n".
    pub fn line_len(&self, line: usize) -> usize {
        let l = self.text.line(line);
        let n = l.len_chars();
        if n > 0 && l.char(n - 1) == '\n' {
            n - 1
        } else {
            n
        }
    }

    /// The selected text as the character indexes where it starts and ends: between the mark and
    /// the cursor, whichever comes first.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let (mark, pos) = (self.mark?.at, self.pos());
        Some((mark.min(pos), mark.max(pos)))
    }

    /// The cursor as a character index in the whole text.
    pub fn pos(&self) -> usize {
        self.text.line_to_char(self.line) + self.x
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.line = self.text.char_to_line(pos);
        self.x = pos - self.text.line_to_char(self.line);
    }

    /// The screen column of the cursor in its line (without horizontal scrolling).
    pub fn column(&self, tabsize: usize) -> usize {
        column_of(&self.line_text(self.line), self.x, tabsize)
    }

    /// Remembers the cursor's column for moving up and down.
    pub fn keep_column(&mut self, tabsize: usize) {
        self.want = self.column(tabsize);
    }

    /// Puts the cursor in `line`, as near the remembered column as the line allows.
    pub fn goto_line(&mut self, line: usize, tabsize: usize) {
        self.line = line.min(self.lines() - 1);
        self.x = index_at(&self.line_text(self.line), self.want, tabsize);
    }

    /// Inserts text at the cursor and moves the cursor after it.
    pub fn insert(&mut self, s: &str) {
        let pos = self.pos();
        self.text.insert(pos, s);
        let end = pos + s.chars().count();
        if let Some(m) = self.mark.as_mut().filter(|m| m.at > pos) {
            m.at += s.chars().count();
        }
        self.seam(end);
        let end = end + self.seam(pos);
        self.set_pos(end);
        self.modified = true;
    }

    /// Deletes the characters from `from` up to `to` (indexes in the whole text).
    pub fn remove(&mut self, from: usize, to: usize) {
        if from < to {
            if let Some(m) = &mut self.mark {
                m.at = if m.at >= to {
                    m.at - (to - from)
                } else {
                    m.at.min(from)
                };
            }
            self.text.remove(from..to);
            self.seam(from);
            self.modified = true;
        }
    }

    /// Deletes the characters from `from` up to `to` and records it for undo. The cursor must
    /// already be where the deletion leaves it; `before` is where it was.
    pub fn erase(&mut self, kind: Kind, from: usize, to: usize, before: usize) {
        let old = String::from(self.text.slice(from..to));
        self.remove(from, to);
        self.record(kind, from, old, String::new(), before);
    }

    /// Replaces `len` characters at `at` with `text`, as a step of undo of its own, and puts the
    /// cursor after the new text.
    pub fn substitute(&mut self, at: usize, len: usize, text: &str) {
        let old = String::from(self.text.slice(at..at + len));
        self.remove(at, at + len);
        self.set_pos(at);
        self.insert(text);
        self.record(Kind::Replace, at, old, text.to_string(), at);
    }

    /// Records an edit that has been made. Typed characters, and deletions with the same key, that
    /// follow one another make one step, as in nano; so do consecutive cuts. The edit ends where
    /// the cursor is.
    pub fn record(&mut self, kind: Kind, at: usize, old: String, new: String, before: usize) {
        let after = self.pos();
        if self.done < self.edits.len() {
            self.edits.truncate(self.done);
            if self.saved.is_some_and(|s| s > self.done) {
                self.saved = None;
            }
        }
        if let Some(last) = self
            .edits
            .last_mut()
            .filter(|l| self.open && l.kind == kind)
        {
            let joined = match kind {
                Kind::Add | Kind::Paste if last.after == at => {
                    // Not at the end of `last.new` when it ends with the final newline.
                    let skip = at - last.at;
                    let i = last
                        .new
                        .char_indices()
                        .nth(skip)
                        .map_or(last.new.len(), |(i, _)| i);
                    last.new.insert_str(i, &new);
                    true
                }
                Kind::Back if last.at == at + old.chars().count() => {
                    last.at = at;
                    last.old.insert_str(0, &old);
                    true
                }
                Kind::Del | Kind::Cut if last.at == at => {
                    last.old.push_str(&old);
                    true
                }
                _ => false,
            };
            if joined {
                last.after = after;
                return;
            }
        }
        self.edits.push(Edit {
            kind,
            at,
            old,
            new,
            before,
            after,
        });
        self.done = self.edits.len();
        self.open = true;
    }

    /// Adds the newline that nano keeps at the end of the text to the last edit, which made
    /// the text end there.
    pub fn record_final_newline(&mut self) {
        if let Some(last) = self.edits.last_mut() {
            last.new.push('\n');
        }
    }

    /// The next edit starts a new undo step.
    pub fn end_group(&mut self) {
        self.open = false;
    }

    /// The text was saved: no edit joins the last one, and undoing back to here is not a change.
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.done);
        self.open = false;
        self.modified = false;
    }

    /// Replaces `len` characters at `at` with `text`, as undo and redo do.
    fn replace(&mut self, at: usize, len: usize, text: &str) {
        self.text.remove(at..at + len);
        self.text.insert(at, text);
        self.seam(at + text.chars().count());
        self.seam(at);
    }

    /// Takes back the last edit in effect; returns its kind, None when there is none.
    pub fn undo(&mut self) -> Option<Kind> {
        let edit = self.edits.get(self.done.checked_sub(1)?)?;
        let (kind, at, before) = (edit.kind, edit.at, edit.before);
        let (new_len, old) = (edit.new.chars().count(), edit.old.clone());
        self.mark = None;
        self.replace(at, new_len, &old);
        self.done -= 1;
        self.finish(before);
        Some(kind)
    }

    /// Makes the last edit taken back again; returns its kind, None when there is none.
    pub fn redo(&mut self) -> Option<Kind> {
        let edit = self.edits.get(self.done)?;
        let (kind, at, after) = (edit.kind, edit.at, edit.after);
        let (old_len, new) = (edit.old.chars().count(), edit.new.clone());
        self.mark = None;
        self.replace(at, old_len, &new);
        self.done += 1;
        // nano puts the cursor back before a replacement, not after it.
        self.finish(if kind == Kind::Replace { at } else { after });
        Some(kind)
    }

    fn finish(&mut self, cursor: usize) {
        self.set_pos(cursor.min(self.text.len_chars()));
        self.open = false;
        self.modified = self.saved != Some(self.done);
    }

    /// Where an edit joined two pieces of text at `at`: a real `MARK` that now follows a
    /// private-use character of the raw-byte range would read as a byte, so it becomes its four
    /// bytes. Returns how many characters were added.
    fn seam(&mut self, at: usize) -> usize {
        let joined = at > 0
            && at < self.text.len_chars()
            && self.text.char(at) == MARK
            && raw_byte(self.text.char(at - 1)).is_some();
        if !joined {
            return 0;
        }
        let escaped = escaped_mark();
        self.text.remove(at..at + 1);
        self.text.insert(at, &escaped);
        escaped.chars().count() - 1
    }

    /// Whether the last line is empty: the text ends with a newline, or is empty.
    pub fn ends_with_newline(&self) -> bool {
        self.line_len(self.lines() - 1) == 0
    }

    /// The bytes to save, with the line ends of the file's format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let text = String::from(&self.text);
        let text = match self.format {
            Format::Unix => text,
            Format::Dos => text.replace('\n', "\r\n"),
            Format::Mac => text.replace('\n', "\r"),
        };
        encode(&text)
    }

    /// The number of characters before `end`, as nano counts them: a byte that is not valid
    /// UTF-8 is one.
    pub fn chars_before(&self, end: usize) -> usize {
        let mut pairs = 0;
        let mut before = None;
        for chunk in self.text.slice(..end).chunks() {
            for (i, _) in chunk.match_indices(MARK) {
                let prev = chunk[..i].chars().next_back().or(before);
                if prev.and_then(raw_byte).is_some() {
                    pairs += 1;
                }
            }
            before = chunk.chars().next_back().or(before);
        }
        end - pairs
    }

    /// The number of lines a file of this text has, as nano counts them in its messages.
    pub fn file_lines(&self) -> usize {
        let n = self.lines();
        if self.ends_with_newline() { n - 1 } else { n }
    }
}

/// The screen column where the character at `index` starts.
pub fn column_of(line: &str, index: usize, tabsize: usize) -> usize {
    line.chars()
        .take(index)
        .fold(0, |col, c| col + width_at(c, col, tabsize))
}

/// The index of the character that covers screen column `col`, or the end of the line. A
/// character with combining marks after it counts as one.
pub fn index_at(line: &str, col: usize, tabsize: usize) -> usize {
    let mut at = 0;
    for (i, c) in line.chars().enumerate() {
        let w = width_at(c, at, tabsize);
        if w > 0 && at + w > col {
            return i;
        }
        at += w;
    }
    line.chars().count()
}

/// The index of the next character after `index`, skipping the combining marks of the one at
/// `index`.
pub fn next_index(line: &str, index: usize) -> usize {
    let mut chars = line.chars().skip(index + 1);
    let mut i = index + 1;
    while chars.next().is_some_and(is_zero_width) {
        i += 1;
    }
    i
}

/// The index of the character before `index`, together with its combining marks.
pub fn prev_index(line: &str, index: usize) -> usize {
    let chars: Vec<char> = line.chars().take(index).collect();
    let mut i = index.saturating_sub(1);
    while i > 0 && is_zero_width(chars[i]) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_utf8_survives() {
        // Invalid bytes, characters of the raw-byte range, MARK, and MARK after one of them.
        let bytes = [
            b"ok \xff\xfe \xc3 x ".as_slice(),
            "\u{10fe41} \u{10ffff} \u{E01EF} \u{10fe42}\u{E01EF} end\n".as_bytes(),
        ]
        .concat();
        let text = decode(bytes.clone());
        assert!(text.contains("\u{10fe41} \u{10ffff} \u{E01EF} "));
        assert_eq!(encode(&text), bytes);
        assert_eq!(encode("\u{10fe42}"), "\u{10fe42}".as_bytes());
        assert_eq!(encode(&raw(0x42)), b"B");
    }

    #[test]
    fn a_byte_counts_as_one_character() {
        let text = decode(b"a\xffb\xc3\xa9\xc3 end".to_vec());
        let b = Buffer::new(&text, Format::Unix);
        let all = b.text.len_chars();
        assert_eq!(b.chars_before(all), 9);
        assert_eq!(b.chars_before(3), 2);
    }

    #[test]
    fn edits_never_make_a_byte() {
        // A raw-range character and a real MARK brought together by deleting what was between.
        let mut b = Buffer::new("\u{10fe42}x\u{E01EF}\n", Format::Unix);
        b.remove(1, 2);
        assert_eq!(
            encode(&String::from(&b.text)),
            "\u{10fe42}\u{E01EF}\n".as_bytes()
        );
        // Or by typing one next to the other.
        let mut b = Buffer::new("\u{10fe42}\n", Format::Unix);
        b.x = 1;
        b.insert("\u{E01EF}");
        assert_eq!(
            encode(&String::from(&b.text)),
            "\u{10fe42}\u{E01EF}\n".as_bytes()
        );
        assert_eq!(b.line_len(0), b.x);
        let mut b = Buffer::new("\u{E01EF}\n", Format::Unix);
        b.insert("\u{10fe42}");
        assert_eq!(
            encode(&String::from(&b.text)),
            "\u{10fe42}\u{E01EF}\n".as_bytes()
        );
        assert_eq!(b.x, 1);
    }

    #[test]
    fn line_ends() {
        assert_eq!(
            convert_line_ends("a\r\nb\r\n".into()),
            ("a\nb\n".into(), Format::Dos)
        );
        assert_eq!(
            convert_line_ends("a\rb\r".into()),
            ("a\nb\n".into(), Format::Mac)
        );
        assert_eq!(
            convert_line_ends("a\nb\r\n".into()),
            ("a\nb\r\n".into(), Format::Unix)
        );
        let mut b = Buffer::new("a\nb\n", Format::Dos);
        assert_eq!(b.to_bytes(), b"a\r\nb\r\n");
        b.format = Format::Mac;
        assert_eq!(b.to_bytes(), b"a\rb\r");
        assert_eq!(b.file_lines(), 2);
    }

    #[test]
    fn columns() {
        let line = "a\tb日e\u{301}x";
        assert_eq!(column_of(line, 2, 8), 8);
        assert_eq!(column_of(line, 4, 8), 11);
        assert_eq!(column_of(line, 6, 8), 12);
        assert_eq!(index_at(line, 5, 8), 1);
        assert_eq!(index_at(line, 10, 8), 3);
        assert_eq!(index_at(line, 11, 8), 4);
        assert_eq!(index_at(line, 99, 8), 7);
        assert_eq!(next_index(line, 4), 6);
        assert_eq!(prev_index(line, 6), 4);
        assert_eq!(prev_index(line, 0), 0);
    }

    #[test]
    fn editing() {
        let mut b = Buffer::new("one\ntwo\n", Format::Unix);
        assert_eq!(b.lines(), 3);
        assert!(b.ends_with_newline());
        b.goto_line(1, 8);
        b.x = 3;
        b.insert("!\nnew");
        assert_eq!(String::from(&b.text), "one\ntwo!\nnew\n");
        assert_eq!((b.line, b.x), (2, 3));
        assert!(b.modified);
        assert_eq!(b.line_len(2), 3);
        assert_eq!(b.line_text(3), "");
    }

    /// Types `s` at the cursor, as the editor does.
    fn type_in(b: &mut Buffer, s: &str) {
        let at = b.pos();
        b.insert(s);
        b.record(Kind::Add, at, String::new(), s.to_string(), at);
    }

    #[test]
    fn typing_in_a_row_is_one_step() {
        let mut b = Buffer::new("one\n", Format::Unix);
        type_in(&mut b, "a");
        type_in(&mut b, "b");
        assert_eq!(b.undo(), Some(Kind::Add));
        assert_eq!((String::from(&b.text), b.pos()), ("one\n".into(), 0));
        assert!(!b.modified);
        assert_eq!(b.undo(), None);
        assert_eq!(b.redo(), Some(Kind::Add));
        assert_eq!((String::from(&b.text), b.pos()), ("abone\n".into(), 2));
        assert_eq!(b.redo(), None);
        // After an undo, typing where the group ended is a new step.
        type_in(&mut b, "c");
        assert_eq!(b.undo(), Some(Kind::Add));
        assert_eq!(String::from(&b.text), "abone\n");
    }

    #[test]
    fn backspaces_and_cuts_each_make_a_step() {
        let mut b = Buffer::new("abcd\nx\ny\n", Format::Unix);
        b.set_pos(3);
        for _ in 0..2 {
            let before = b.pos();
            b.set_pos(before - 1);
            b.erase(Kind::Back, before - 1, before, before);
        }
        assert_eq!(String::from(&b.text), "ad\nx\ny\n");
        b.set_pos(3);
        for _ in 0..2 {
            b.erase(Kind::Cut, 3, 5, 3);
        }
        assert_eq!(String::from(&b.text), "ad\n");
        assert_eq!(b.undo(), Some(Kind::Cut));
        assert_eq!((String::from(&b.text), b.pos()), ("ad\nx\ny\n".into(), 3));
        assert_eq!(b.undo(), Some(Kind::Back));
        assert_eq!((String::from(&b.text), b.pos()), ("abcd\nx\ny\n".into(), 3));
        assert_eq!(b.undo(), None);
    }

    #[test]
    fn a_new_edit_clears_redo_and_modified_follows_the_saved_state() {
        let mut b = Buffer::new("x\n", Format::Unix);
        type_in(&mut b, "a");
        b.mark_saved();
        assert!(!b.modified);
        // A save ends the group.
        type_in(&mut b, "b");
        assert_eq!(b.undo(), Some(Kind::Add));
        assert!(!b.modified);
        assert_eq!(b.undo(), Some(Kind::Add));
        assert!(b.modified);
        assert_eq!(b.redo(), Some(Kind::Add));
        assert!(!b.modified);
        // Back past the save, then a new edit: the saved text is out of reach.
        b.undo();
        type_in(&mut b, "c");
        assert_eq!(b.redo(), None);
        assert!(b.modified);
        b.undo();
        assert!(b.modified);
    }

    #[test]
    fn the_mark_moves_with_the_text() {
        let mut b = Buffer::new("one two\n", Format::Unix);
        b.mark = Some(Mark { at: 4, soft: false });
        b.insert("xx");
        assert_eq!(b.selection(), Some((2, 6)));
        b.set_pos(0);
        b.insert("yy");
        assert_eq!(b.mark.unwrap().at, 8);
        b.remove(0, 3);
        assert_eq!(b.mark.unwrap().at, 5);
    }

    #[test]
    fn undo_takes_the_mark_away() {
        let mut b = Buffer::new("abc\n", Format::Unix);
        let at = b.pos();
        b.insert("x");
        b.record(Kind::Add, at, String::new(), "x".into(), at);
        b.mark = Some(Mark { at: 0, soft: true });
        assert!(b.undo().is_some());
        assert_eq!(b.mark, None);
    }
}
