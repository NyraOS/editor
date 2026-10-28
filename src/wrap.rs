// SPDX-License-Identifier: MIT
//! Soft wrapping (`-S`): a long line is shown as several screen rows, and the cursor moves,
//! scrolls and pages by rows. A row is a range of screen columns of its line, so that a tab can
//! be split between two rows, as nano shows it.
//!
//! Nothing here looks at more than the lines of the screen and the line of the cursor: the
//! rows of a line are worked out when needed, never kept, and never for the whole text.

use crate::buffer::index_at;
use crate::commands::Editor;
use crate::screen::width_at;

/// How the rows of a line are cut.
#[derive(Clone, Copy)]
pub struct Rules {
    /// The columns of a row.
    pub width: usize,
    pub tabsize: usize,
    /// `-a`: break after a blank rather than in the middle of a word.
    pub atblanks: bool,
}

/// One screen row of a line: its columns `start..end`. The last row of a line has `last` set; it
/// is empty when the line fills its rows exactly, as in nano (the cursor at the end of such a line
/// is on a row of its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub last: bool,
    /// The first character that ends after `start` (a tab may begin before it): its byte index in
    /// the text and the column where it begins.
    pub at: usize,
    pub at_col: usize,
}

/// The rows of one line, one after the other.
pub struct Chunks<'a> {
    line: usize,
    text: &'a str,
    rules: Rules,
    start: usize,
    at: usize,
    at_col: usize,
    done: bool,
}

impl<'a> Chunks<'a> {
    pub fn new(line: usize, text: &'a str, rules: Rules) -> Self {
        Self {
            line,
            text,
            rules: Rules {
                width: rules.width.max(1),
                ..rules
            },
            start: 0,
            at: 0,
            at_col: 0,
            done: false,
        }
    }
}

impl Iterator for Chunks<'_> {
    type Item = Span;

    fn next(&mut self) -> Option<Span> {
        if self.done {
            return None;
        }
        let Rules {
            width,
            tabsize,
            atblanks,
        } = self.rules;
        let limit = self.start + width;
        let mut col = self.at_col;
        let mut last_blank_end = None;
        // Where the row ends, when a character does not fit, and where that character ends.
        let mut cut = None;
        for c in self.text[self.at..].chars() {
            let w = width_at(c, col, tabsize);
            if col + w > limit {
                // Only a tab is split between two rows; a wide or control character moves whole.
                let edge = if c == '\t' { limit } else { col };
                let end = match last_blank_end {
                    _ if !atblanks => edge,
                    _ if c == '\t' && col < limit => limit,
                    found => found.unwrap_or(edge),
                };
                cut = Some((end, col + w));
                break;
            }
            if atblanks && matches!(c, ' ' | '\t') {
                last_blank_end = Some(col + w);
            }
            col += w;
        }
        let end = match cut {
            None if col < limit => {
                self.done = true;
                return Some(Span {
                    line: self.line,
                    start: self.start,
                    end: col,
                    last: true,
                    at: self.at,
                    at_col: self.at_col,
                });
            }
            // The text ends exactly at the edge: an empty row follows.
            None => limit,
            // A one-column screen still shows each character, cut.
            Some((end, over_end)) if end <= self.start => over_end,
            Some((end, _)) => end,
        };
        let span = Span {
            line: self.line,
            start: self.start,
            end,
            last: false,
            at: self.at,
            at_col: self.at_col,
        };
        for c in self.text[self.at..].chars() {
            let w = width_at(c, self.at_col, tabsize);
            if self.at_col + w > end {
                break;
            }
            self.at += c.len_utf8();
            self.at_col += w;
        }
        self.start = end;
        Some(span)
    }
}

impl Editor {
    fn rules(&self) -> Rules {
        Rules {
            width: self.cols.saturating_sub(self.margin()).max(1),
            tabsize: self.opts.tabsize,
            atblanks: self.opts.atblanks,
        }
    }

    fn spans_of(&self, line: usize) -> Vec<Span> {
        Chunks::new(line, &self.file.buf.line_text(line), self.rules()).collect()
    }

    /// The row of the cursor.
    pub fn cursor_span(&self) -> Span {
        let b = &self.file.buf;
        let text = b.line_text(b.line);
        let col = b.column(self.opts.tabsize);
        Chunks::new(b.line, &text, self.rules())
            .find(|s| s.last || col < s.end)
            .expect("the last row of a line has `last` set")
    }

    /// The row at the top of the screen: the one with the column the top remembers.
    fn top_span(&self) -> Span {
        let b = &self.file.buf;
        let line = b.top.min(b.lines() - 1);
        let text = b.line_text(line);
        Chunks::new(line, &text, self.rules())
            .find(|s| s.last || b.top_col < s.end)
            .expect("the last row of a line has `last` set")
    }

    /// Up to `n` rows, from `from` on.
    fn rows_from(&self, from: Span, n: usize) -> Vec<Span> {
        let mut rows = Vec::new();
        let mut line = from.line;
        while rows.len() < n && line < self.file.buf.lines() {
            let spans = self.spans_of(line);
            let skip = if line == from.line {
                spans
                    .iter()
                    .position(|s| s.start == from.start)
                    .unwrap_or(0)
            } else {
                0
            };
            rows.extend(spans.into_iter().skip(skip).take(n - rows.len()));
            line += 1;
        }
        rows
    }

    /// The rows on the screen.
    pub fn visible_spans(&self) -> Vec<Span> {
        self.rows_from(self.top_span(), self.edit_rows())
    }

    /// The row `delta` rows after (before, when negative) `from`, or the first or the last of
    /// the text, and how many rows it is away.
    fn shift(&self, from: Span, delta: isize) -> (Span, usize) {
        let lines = self.file.buf.lines();
        let mut line = from.line;
        let mut spans = self.spans_of(line);
        let mut idx = spans
            .iter()
            .position(|s| s.start == from.start)
            .unwrap_or(0);
        let (mut left, mut moved) = (delta.unsigned_abs(), 0);
        while left > 0 {
            if delta > 0 {
                let below = spans.len() - 1 - idx;
                if left <= below || line + 1 >= lines {
                    let by = left.min(below);
                    (idx, moved, left) = (idx + by, moved + by, 0);
                } else {
                    (left, moved, line, idx) = (left - below - 1, moved + below + 1, line + 1, 0);
                    spans = self.spans_of(line);
                }
            } else if left <= idx || line == 0 {
                let by = left.min(idx);
                (idx, moved, left) = (idx - by, moved + by, 0);
            } else {
                (left, moved, line) = (left - idx - 1, moved + idx + 1, line - 1);
                spans = self.spans_of(line);
                idx = spans.len() - 1;
            }
        }
        (spans[idx], moved)
    }

    fn set_top(&mut self, row: Span) {
        self.file.buf.top = row.line;
        self.file.buf.top_col = row.start;
    }

    /// Scrolls just enough to show the cursor's row.
    pub fn wrap_scroll(&mut self) {
        let (cursor, top) = (self.cursor_span(), self.top_span());
        if (cursor.line, cursor.start) < (top.line, top.start) {
            self.set_top(cursor);
        } else if self.rows_from(top, self.edit_rows()).contains(&cursor) {
            self.set_top(top);
        } else {
            let first = self.shift(cursor, 1 - self.edit_rows() as isize).0;
            self.set_top(first);
        }
    }

    /// Puts the cursor's row in the middle of the screen, if it is not on it (`always`: even if
    /// it is).
    pub fn wrap_center(&mut self, always: bool) {
        let cursor = self.cursor_span();
        let rows = self.edit_rows();
        if always || !self.rows_from(self.top_span(), rows).contains(&cursor) {
            let first = self.shift(cursor, -((rows / 2) as isize)).0;
            self.set_top(first);
        }
    }

    /// The cursor in `row`, in the column it keeps for moving up and down.
    fn place(&mut self, row: Span) {
        let width = self.rules().width;
        let text = self.file.buf.line_text(row.line);
        let col = row.start + self.file.buf.want % width;
        // On a row that goes on, the cursor stays on the row.
        let col = if row.last { col } else { col.min(row.end - 1) };
        self.file.buf.line = row.line;
        self.file.buf.x = index_at(&text, col, self.opts.tabsize);
    }

    pub fn wrap_up(&mut self) {
        let (row, moved) = self.shift(self.cursor_span(), -1);
        if moved > 0 {
            self.place(row);
        }
    }

    pub fn wrap_down(&mut self) {
        let (row, moved) = self.shift(self.cursor_span(), 1);
        if moved > 0 {
            self.place(row);
        }
    }

    /// A page up or down: the screen and the cursor move by as many rows. A page down that does
    /// not have the rows only takes the cursor to the end.
    pub fn wrap_page(&mut self, down: bool) {
        let page = self.page();
        let by = if down {
            page as isize
        } else {
            -(page as isize)
        };
        let (row, moved) = self.shift(self.cursor_span(), by);
        if !down || moved == page {
            let first = self.shift(self.top_span(), by).0;
            self.set_top(first);
        }
        self.place(row);
    }

    /// Home: the start of the row; at the start of the row already, the start of the line.
    /// Returns false when the key is for the line to handle (the first row, or at its start).
    pub fn wrap_home(&mut self) -> bool {
        let row = self.cursor_span();
        let text = self.file.buf.line_text(row.line);
        let start = index_at(&text, row.start, self.opts.tabsize);
        if row.start == 0 || self.file.buf.x == start {
            return false;
        }
        self.file.buf.x = start;
        self.file.buf.keep_column(self.opts.tabsize);
        // Observed in nano 8.4: after Home the wanted column is the left edge of the screen, even
        // when the row starts further in because a wide character went to the next row.
        self.file.buf.want = 0;
        true
    }

    /// End: the last character of the row; there already (or on the last row), the end of the
    /// line.
    pub fn wrap_end(&mut self) {
        let row = self.cursor_span();
        let text = self.file.buf.line_text(row.line);
        let last = index_at(&text, row.end.saturating_sub(1), self.opts.tabsize);
        self.file.buf.x = if row.last || self.file.buf.x == last {
            self.file.buf.line_len(row.line)
        } else {
            last
        };
        self.file.buf.keep_column(self.opts.tabsize);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, width: usize, atblanks: bool) -> Vec<(usize, usize)> {
        let rules = Rules {
            width,
            tabsize: 8,
            atblanks,
        };
        Chunks::new(0, text, rules)
            .map(|s| (s.start, s.end))
            .collect()
    }

    #[test]
    fn short_line_is_one_row() {
        assert_eq!(rows("", 10, false), [(0, 0)]);
        assert_eq!(rows("abc", 10, false), [(0, 3)]);
    }

    #[test]
    fn exact_fit_has_an_empty_row_after_it() {
        assert_eq!(rows("0123456789", 10, false), [(0, 10), (10, 10)]);
        assert_eq!(rows("aaaa bbb", 4, true), [(0, 4), (4, 8), (8, 8)]);
    }

    #[test]
    fn breaks_at_the_edge() {
        assert_eq!(rows("0123456789abc", 5, false), [(0, 5), (5, 10), (10, 13)]);
    }

    #[test]
    fn wide_character_moves_whole() {
        assert_eq!(rows("abc日d", 4, false), [(0, 3), (3, 6)]);
        // Cut into a one-column screen, so that there is no endless loop.
        assert_eq!(rows("日a", 1, false), [(0, 2), (2, 3), (3, 3)]);
    }

    #[test]
    fn control_character_moves_whole() {
        assert_eq!(rows("abc\x01d", 4, false), [(0, 3), (3, 6)]);
    }

    #[test]
    fn tab_is_split_between_rows() {
        // The tab covers columns 3 to 8; the screen cuts it at column 5.
        assert_eq!(rows("abc\tdef", 5, false), [(0, 5), (5, 10), (10, 11)]);
        assert_eq!(rows("abc\tdef", 5, true), [(0, 5), (5, 8), (8, 11)]);
    }

    #[test]
    fn at_blanks_breaks_after_the_last_blank() {
        assert_eq!(rows("aaa bbbbb ccc", 8, true), [(0, 4), (4, 10), (10, 13)]);
        assert_eq!(rows("aaa bbbbb ccc", 8, false), [(0, 8), (8, 13)]);
        assert_eq!(rows("aaaa bbbb cc", 6, true), [(0, 5), (5, 10), (10, 12)]);
    }

    #[test]
    fn at_blanks_without_a_blank_breaks_at_the_edge() {
        assert_eq!(rows("abcdefghij kl", 5, true), [(0, 5), (5, 10), (10, 13)]);
    }

    #[test]
    fn at_blanks_a_blank_at_the_edge_starts_the_next_row() {
        assert_eq!(rows("abcde fg", 5, true), [(0, 5), (5, 8)]);
    }

    #[test]
    fn at_blanks_leading_blanks_make_a_row() {
        assert_eq!(rows("    abcdefgh", 6, true), [(0, 4), (4, 10), (10, 12)]);
    }

    #[test]
    fn rows_cover_the_line_without_gaps() {
        let text = "a\t日 b\x01 cd\tefghi \u{301}jklm ".repeat(7);
        for width in 1..30 {
            for atblanks in [false, true] {
                let r = rows(&text, width, atblanks);
                assert_eq!(r[0].0, 0);
                assert!(r.windows(2).all(|w| w[0].1 == w[1].0 && w[0].0 < w[0].1));
            }
        }
    }

    #[test]
    fn a_very_long_line_is_cut_in_time() {
        let text = "x".repeat(10_000_000);
        assert_eq!(rows(&text, 80, false).len(), 10_000_000 / 80 + 1);
    }
}
