// SPDX-License-Identifier: MIT
//! The mouse (`-m`, `set mouse`), as nano 8.4 uses it: a click of the left button in the text
//! puts the cursor there (a second one on the same character sets the mark, or takes it away),
//! a click on an item of the help lines presses its key, and the wheel scrolls the text or the
//! help text. Nothing else does anything: the other buttons, clicks on the title bar, the status
//! bar and the line numbers, and clicks while a question or a prompt is asked in the text.
//!
//! A click never types anything: a press of a key of the help lines goes through `Editor::key`
//! like the key itself, and the rest only moves the cursor or the text.

use super::{Chain, Editor, Mode, Question};
use crate::buffer::index_at;
use crate::keys::Key;
use crate::mouse::Mouse;
use crate::screen::{self, page_start};

/// How many lines one turn of the wheel scrolls, as in nano.
const WHEEL_LINES: usize = 2;

impl Editor {
    /// A click or a turn of the wheel at a place of the screen (columns and rows from 0).
    pub fn mouse(&mut self, mouse: Mouse) {
        // The reports are only asked for with the mouse on, but a program can write them to the
        // terminal too: without it they do nothing. A warning is on the screen alone.
        if !self.opts.mouse || self.warning.is_some() {
            return;
        }
        // Every report is a key to nano, a turn of the wheel in the window three.
        self.wear_message(1);
        let (Mouse::Click { col, row } | Mouse::Wheel { col, row, .. }) = mouse else {
            return;
        };
        // A place outside the screen is not one a terminal reports.
        if col >= self.cols || row >= self.rows {
            return;
        }
        match mouse {
            Mouse::Click { .. } => self.click(col, row),
            Mouse::Wheel { up, .. } => self.wheel(up, row),
            Mouse::Other => {}
        }
    }

    /// The row of the help lines (0 or 1) that a row of the screen is, if they are shown.
    fn help_row(&self, row: usize) -> Option<usize> {
        let shown = !self.opts.nohelp && self.rows >= 3;
        row.checked_sub(self.rows.saturating_sub(2))
            .filter(|_| shown && row < self.rows)
    }

    fn click(&mut self, col: usize, row: usize) {
        if let Some(help) = self.help_row(row) {
            return self.click_help(col, help);
        }
        if matches!(self.mode, Mode::Edit) && (1..=self.edit_rows()).contains(&row) {
            self.click_text(col, row - 1);
        }
    }

    /// A click on the help lines presses the key of the item, in the menu that is shown.
    fn click_help(&mut self, col: usize, help: usize) {
        if let Mode::Ask(question) = &self.mode {
            let all = matches!(question, Question::Replace(_));
            return self.click_question(col, help, all);
        }
        let keys = self.bindings.help_keys(self.slot());
        let (slots, width) = screen::help_slots(keys.len(), self.cols);
        let index = (col / width.max(1)).min(slots - 1) * 2 + help;
        if let Some(Some(key)) = keys.get(index) {
            self.key(key.clone());
        }
    }

    /// The items of a yes-or-no question are in two columns of a fixed width, and only they
    /// answer.
    fn click_question(&mut self, col: usize, help: usize, all: bool) {
        let width = screen::yes_no_width(self.cols);
        if width == 0 || col >= width * 2 {
            return;
        }
        let item = screen::YES_NO
            .iter()
            .filter(|item| all || item.1 != "All")
            .find(|item| item.2 == help && item.3 == col / width);
        let key = match item {
            Some((_, "Cancel", ..)) => self.bindings.help_keys(crate::bindings::YES_NO).pop(),
            Some((letter, ..)) => letter.trim().chars().next().map(|c| Some(Key::Char(c))),
            None => None,
        };
        if let Some(Some(key)) = key {
            self.key(key);
        }
    }

    /// A click in the text: the cursor goes to the character under it, and when it was there
    /// already, the mark is set or taken away.
    fn click_text(&mut self, col: usize, row: usize) {
        let margin = self.margin();
        // The line numbers are not text.
        let Some(want) = col.checked_sub(margin) else {
            return;
        };
        let Some((line, text_col, end)) = self.text_place(row, want) else {
            return;
        };
        let tab = self.opts.tabsize;
        let text = self.file.buf.line_text(line);
        // On a row that goes on, the cursor stays on the row.
        let x = index_at(
            &text,
            end.map_or(text_col, |end| text_col.min(end - 1)),
            tab,
        );
        // Whatever was going to join the next key is over.
        self.chain = Chain::None;
        self.completion = None;
        let b = &mut self.file.buf;
        if (b.line, b.x) == (line, x) {
            return self.toggle_mark();
        }
        b.end_group();
        (b.line, b.x) = (line, x);
        b.keep_column(tab);
        self.scroll_to_cursor();
    }

    /// The line and the screen column of that line that a place of the text window is, and where
    /// its row ends when the row goes on (soft wrapping), as nano works it out. A row far to the right of the cursor's line scrolls by the page of the
    /// cursor's line, which is where the click lands on the clicked line too.
    fn text_place(&self, row: usize, want: usize) -> Option<(usize, usize, Option<usize>)> {
        if self.opts.softwrap {
            let spans = self.visible_spans();
            let span = *spans.get(row).or(spans.last())?;
            return Some((
                span.line,
                span.start + want,
                (!span.last).then_some(span.end),
            ));
        }
        let b = &self.file.buf;
        // Below the last line, the click is on the last line, as in nano.
        let line = (b.top + row).min(b.lines() - 1);
        let width = self.cols.saturating_sub(self.margin());
        let page = page_start(b.column(self.opts.tabsize), width);
        Some((line, page + want, None))
    }

    /// A turn of the wheel over the text, the help text or the status bar scrolls it by
    /// `WHEEL_LINES` (with the help lines hidden, over the rows they were on too). Each turn wears
    /// the message on the status bar down by two keys more than any report does, as in nano.
    fn wheel(&mut self, up: bool, row: usize) {
        let below = if self.opts.nohelp {
            self.rows
        } else {
            self.rows.saturating_sub(2)
        };
        if !(1..below).contains(&row) {
            return;
        }
        self.wear_message(2);
        match self.mode {
            Mode::Help(top) => {
                let rows = self.edit_rows().saturating_sub(1);
                let last = self.help_lines().len().saturating_sub(rows);
                let top = if up {
                    top.saturating_sub(WHEEL_LINES)
                } else {
                    top + WHEEL_LINES
                };
                self.mode = Mode::Help(top.min(last));
            }
            Mode::Edit => self.scroll_text(up),
            _ => {}
        }
    }

    /// Scrolls the text, and takes the cursor along when it would leave the window.
    fn scroll_text(&mut self, up: bool) {
        for _ in 0..WHEEL_LINES {
            self.scroll_one(up);
        }
    }

    fn scroll_one(&mut self, up: bool) {
        let rows = self.edit_rows();
        let tab = self.opts.tabsize;
        if self.opts.softwrap {
            return self.wrap_scroll_one(up);
        }
        let b = &mut self.file.buf;
        if up {
            b.top = b.top.saturating_sub(1);
        } else if b.top + 1 < b.lines() {
            b.top += 1;
        }
        if b.line < b.top {
            b.goto_line(b.top, tab);
        } else if b.line >= b.top + rows {
            b.goto_line(b.top + rows - 1, tab);
        }
    }
}
