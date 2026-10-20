// SPDX-License-Identifier: MIT
//! Replacing: which match comes next, and what each replacement does to the places that matter.

use crate::buffer::{Buffer, Mark};
use crate::search::{self, Hit, Pattern};

/// A replacing that has begun. Places are character indexes in the whole text, kept in step with
/// the replacements.
pub struct Replacing {
    pattern: Pattern,
    with: String,
    backwards: bool,
    /// Where the cursor and the mark were when it began; they are put back at the end.
    cursor: usize,
    mark: Option<Mark>,
    /// The selection, the only place to replace in when there is one.
    region: Option<(usize, usize)>,
    /// Where the next search goes on from.
    from: usize,
    /// Whether the search has gone past the end (or the start) and on from the other end.
    wrapped: bool,
    /// The match asked about, and how many have been replaced.
    hit: Option<Hit>,
    pub count: usize,
    /// Whether there was a match at all.
    pub found: bool,
}

impl Replacing {
    pub fn new(pattern: Pattern, with: String, backwards: bool, buf: &Buffer) -> Self {
        let region = buf.selection();
        let cursor = buf.pos();
        let from = match (region, backwards) {
            (Some((from, _)), false) => from,
            (Some((_, to)), true) => to,
            (None, _) => cursor,
        };
        Self {
            pattern,
            with,
            backwards,
            cursor,
            mark: buf.mark,
            region,
            from,
            wrapped: false,
            hit: None,
            count: 0,
            found: false,
        }
    }

    /// The next match to ask about: forward from the cursor to the end of the text, then from its
    /// start up to the cursor; backwards the other way round. Only the selection when there is
    /// one.
    pub fn next(&mut self, buf: &Buffer) -> Option<Hit> {
        let hit = match (self.region, self.backwards) {
            (Some((_, to)), false) => search::match_after(buf, &self.pattern, self.from, to)
                .filter(|&(at, len)| at + len <= to),
            (Some((from, to)), true) => self.last_inside(buf, from, to),
            (None, false) => self.forward(buf),
            (None, true) => self.backward(buf),
        };
        self.found |= hit.is_some();
        self.hit = hit;
        hit
    }

    fn forward(&mut self, buf: &Buffer) -> Option<Hit> {
        if !self.wrapped {
            if let Some(hit) = search::match_after(buf, &self.pattern, self.from, usize::MAX) {
                return Some(hit);
            }
            self.wrapped = true;
            self.from = 0;
        }
        search::match_after(buf, &self.pattern, self.from, self.cursor)
    }

    fn backward(&mut self, buf: &Buffer) -> Option<Hit> {
        if !self.wrapped {
            if let Some(hit) = search::match_before(buf, &self.pattern, self.from, 0) {
                return Some(hit);
            }
            self.wrapped = true;
            self.from = buf.text.len_chars() + 1;
        }
        search::match_before(buf, &self.pattern, self.from, self.cursor)
    }

    /// The last match before `self.from` that is between `from` and `to`.
    fn last_inside(&mut self, buf: &Buffer, from: usize, to: usize) -> Option<Hit> {
        loop {
            let (at, len) = search::match_before(buf, &self.pattern, self.from, from)?;
            if at + len <= to {
                return Some((at, len));
            }
            self.from = at;
        }
    }

    /// Where the match asked about is: the cursor goes there.
    pub fn hit(&self) -> Option<Hit> {
        self.hit
    }

    /// Leaves the match asked about as it is.
    pub fn skip(&mut self) {
        if let Some((at, _)) = self.hit {
            self.from = if self.backwards { at } else { at + 1 };
        }
    }

    /// Replaces the match asked about.
    pub fn replace(&mut self, buf: &mut Buffer) {
        let Some((at, len)) = self.hit else {
            return;
        };
        let line = buf.text.char_to_line(at);
        let text = buf.line_text(line);
        let with = self
            .pattern
            .replacement(&text, at - buf.text.line_to_char(line), &self.with);
        let new_len = with.chars().count();
        buf.substitute(at, len, &with);
        self.count += 1;
        let shifted = |place: usize| {
            if at < place {
                (place + new_len).saturating_sub(len).max(at)
            } else {
                place
            }
        };
        self.cursor = shifted(self.cursor);
        if let Some(mark) = &mut self.mark {
            mark.at = shifted(mark.at);
        }
        if let Some((_, to)) = &mut self.region {
            *to = shifted(*to);
        }
        // After an empty match, the search goes on one character further: not to find it again.
        self.from = if self.backwards {
            at
        } else {
            at + new_len + usize::from(len == 0)
        };
    }

    /// The end: the cursor and the mark are where they were.
    pub fn finish(&self, buf: &mut Buffer) {
        buf.set_pos(self.cursor.min(buf.text.len_chars()));
        buf.mark = self.mark;
    }
}
