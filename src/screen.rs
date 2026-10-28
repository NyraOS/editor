// SPDX-License-Identifier: MIT
//! The screen: how characters show, what goes on each row (the title bar, the text, the status
//! bar and the help lines), and the terminal, which is sent only the rows that changed.

use crate::buffer::{MARK, column_of, raw_byte};
use crate::commands::{Editor, Mode, Prompt, Question};
use crate::syntax::{self, Hue};
use rustix::termios::{self, OptionalActions, Termios};
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

/// The looks of a cell.
pub const PLAIN: u8 = 0;
pub const INVERSE: u8 = 1;
/// Error messages: bold white on red.
pub const ERROR: u8 = 2;
/// The text a search found: black on bright yellow.
pub const SPOT: u8 = 3;

/// One screen cell. Its text is one character with its combining marks; the right half of a
/// wide character has no text. The colours of a syntax (`hue`) show only on a plain cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub text: String,
    pub style: u8,
    pub hue: Hue,
}

impl Cell {
    fn new(c: char, style: u8) -> Self {
        Self {
            text: c.to_string(),
            style,
            hue: Hue::default(),
        }
    }

    fn blank() -> Self {
        Self::new(' ', PLAIN)
    }
}

pub type Row = Vec<Cell>;

/// What the screen shows: its rows, and the cursor (None: hidden).
pub struct Frame {
    pub rows: Vec<Row>,
    pub cursor: Option<(usize, usize)>,
}

/// Control characters show as two cells: `^A`, `^?`, and `^` with a letter for C1 controls.
fn is_control(c: char) -> bool {
    c < ' ' || ('\x7f'..='\u{9f}').contains(&c)
}

fn control_letter(c: char) -> char {
    match c as u32 {
        0x7f => '?',
        0x9f => '=',
        n if n < 0x20 => char::from(n as u8 + 0x40),
        n => char::from((n - 0x20) as u8),
    }
}

/// How many columns `c` takes when it starts in column `col`.
pub fn width_at(c: char, col: usize, tabsize: usize) -> usize {
    if c == '\t' {
        tabsize - col % tabsize
    } else if is_control(c) {
        2
    } else if raw_byte(c).is_some() || is_noncharacter(c) {
        // Private-use characters, also standing for bytes that are not valid UTF-8.
        1
    } else {
        c.width().unwrap_or(1)
    }
}

/// The 66 noncharacters (U+FDD0 to U+FDEF, and the last two of each plane): nano shows each as
/// one blank column.
fn is_noncharacter(c: char) -> bool {
    ('\u{fdd0}'..='\u{fdef}').contains(&c) || (c as u32) & 0xfffe == 0xfffe
}

/// A combining mark or another character that takes no column: it goes with the one before.
pub fn is_zero_width(c: char) -> bool {
    c != '\t' && width_at(c, 0, 1) == 0
}

/// The text of each cell `c` takes, from column `col`; empty for the right half of a wide one.
fn pieces(c: char, col: usize, tabsize: usize) -> Vec<String> {
    let w = width_at(c, col, tabsize);
    if c == '\t' || is_noncharacter(c) {
        vec![" ".into(); w]
    } else if is_control(c) {
        vec!["^".into(), control_letter(c).to_string()]
    } else {
        let mut v = vec![c.to_string()];
        v.resize(w, String::new());
        v
    }
}

/// Writes `text` into `row` from column `col`, as far as it fits. Returns the column after it.
pub fn put(row: &mut [Cell], mut col: usize, text: &str, style: u8) -> usize {
    for c in text.chars() {
        if is_zero_width(c) {
            if let Some(cell) = col.checked_sub(1).and_then(|i| row.get_mut(i)) {
                cell.text.push(c);
            }
            continue;
        }
        let pieces = pieces(c, col, 1);
        if col + pieces.len() > row.len() {
            break;
        }
        for p in pieces {
            row[col] = Cell {
                text: p,
                style,
                hue: Hue::default(),
            };
            col += 1;
        }
    }
    col
}

pub fn text_width(text: &str) -> usize {
    text.chars().map(|c| width_at(c, 0, 1)).sum()
}

/// The cells of the screen columns `start..end` of a text line (`width` cells, the ones after
/// `end - start` blank), with nothing to show that the line goes on; and the column where the
/// reading stopped (past `end` when the line goes on). It reads from the character at byte
/// `from.0`, which begins in column `from.1`, so that a row far into a long line is not found by
/// reading all of it.
pub fn cells_between(
    line: &str,
    from: (usize, usize),
    (start, end): (usize, usize),
    width: usize,
    tabsize: usize,
) -> (Row, usize) {
    let mut cells = vec![Cell::blank(); width];
    let mut col = from.1;
    // The cell where the last character started, for its combining marks.
    let mut last = None;
    for c in line[from.0..].chars() {
        let w = width_at(c, col, tabsize);
        if w == 0 {
            if let Some(i) = last {
                let cell: &mut Cell = &mut cells[i];
                cell.text.push(c);
            }
            continue;
        }
        if col >= end {
            col += w;
            break;
        }
        for (k, p) in pieces(c, col, tabsize).into_iter().enumerate() {
            if (start..end).contains(&(col + k)) {
                cells[col + k - start].text = p;
            }
        }
        last = (start..end).contains(&col).then(|| col - start);
        col += w;
    }
    (cells, col)
}

/// Gives the cells of the screen columns `start..end` of a line the colours of its characters;
/// `hues` has one for each byte of the line (the first ones, see `syntax::paint`), and `from` is
/// as in `cells_between`. Only plain cells are coloured: the markers of a line that goes on past
/// the edge are not.
pub fn colour_cells(
    cells: &mut [Cell],
    line: &str,
    hues: &[Hue],
    from: (usize, usize),
    (start, end): (usize, usize),
    tabsize: usize,
) {
    let mut col = from.1;
    for (i, c) in line[from.0..].char_indices() {
        let Some(&hue) = hues.get(from.0 + i) else {
            break;
        };
        if col >= end {
            break;
        }
        let w = width_at(c, col, tabsize);
        // A character left of `start` (a line scrolled sideways) has no cells to colour.
        let (first, last) = (col.max(start) - start, (col + w).min(end));
        for cell in cells
            .iter_mut()
            .take(last.saturating_sub(start))
            .skip(first)
        {
            if cell.style == PLAIN {
                cell.hue = hue;
            }
        }
        col += w;
    }
}

/// The cells of a text line from screen column `start`, `width` of them. As in nano, a line
/// scrolled to the right starts with `<`, a line that goes on past the edge ends with `>`, and
/// a wide character cut in half by them shows its other half as `]` or `[`.
pub fn line_cells(line: &str, start: usize, width: usize, tabsize: usize) -> Row {
    let end = start + width;
    let (mut cells, col) = cells_between(line, (0, 0), (start, end), width, tabsize);
    if width > 1 && start > 0 {
        cells[0] = Cell::new('<', INVERSE);
        if cells[1].text.is_empty() {
            cells[1] = Cell::new(']', PLAIN);
        }
    }
    if width > 1 && col > end {
        cells[width - 1] = Cell::new('>', INVERSE);
        if cells[width - 2]
            .text
            .chars()
            .next()
            .is_some_and(|c| width_at(c, 0, 1) == 2)
        {
            cells[width - 2] = Cell::new('[', PLAIN);
        }
    }
    cells
}

/// The first screen column shown of a line whose cursor is in column `col`, in a window
/// `width` columns wide: nano scrolls a long line in steps, keeping some text left of the cursor.
pub fn page_start(col: usize, width: usize) -> usize {
    if col == 0 || col + 2 < width {
        0
    } else if width > 8 {
        col - 6 - (col - 6) % (width - 8)
    } else {
        col - width.saturating_sub(2)
    }
}

/// Two help lines from (key, label) pairs, laid out in columns like nano's: the first pair in
/// the top row, the second below it, and so on; as many columns as the width allows.
pub fn help_rows(items: &[(&str, &str)], cols: usize) -> [Row; 2] {
    let mut rows = [vec![Cell::blank(); cols], vec![Cell::blank(); cols]];
    let slots = items.len().div_ceil(2).min((cols / 10 + 4) / 2).max(1);
    let width = cols / slots;
    for (i, (key, label)) in items.iter().take(slots * 2).enumerate() {
        let (slot, row) = (i / 2, i % 2);
        let from = slot * width;
        let to = if slot + 1 == slots {
            cols
        } else {
            from + width
        };
        let cells = &mut rows[row][from..to];
        let after = put(cells, 0, key, INVERSE);
        if after < cells.len() {
            put(cells, after + 1, label, PLAIN);
        }
    }
    rows
}

/// The title bar: the program and version on the left, the file name in the middle and
/// "Modified" on the right, leaving out what does not fit the way nano does.
pub fn title_row(cols: usize, program: &str, name: &str, modified: bool) -> Row {
    let mut row = vec![Cell::new(' ', INVERSE); cols];
    let (p, n) = (text_width(program), text_width(name));
    // The width of "Modified", kept free even when the buffer is not modified.
    let s = 8;
    let state = if modified { "Modified" } else { "" };
    if cols >= p + 3 + n + 1 + s + 2 {
        put(&mut row, 2, program, INVERSE);
        put(
            &mut row,
            p + 3 + (cols - (p + 3) - (n + 1) - (s + 2)) / 2,
            name,
            INVERSE,
        );
        put(&mut row, cols - s - 2, state, INVERSE);
    } else if cols >= 2 + n + 1 + s + 2 {
        put(
            &mut row,
            2 + (cols - 2 - (n + 1) - (s + 2)) / 2,
            name,
            INVERSE,
        );
        put(&mut row, cols - s - 2, state, INVERSE);
    } else if modified {
        if n + 1 + s <= cols {
            put(&mut row, 0, name, INVERSE);
        } else if cols >= 5 + s {
            put(&mut row, 0, &dotted(name, cols - s - 4), INVERSE);
        }
        put(&mut row, cols.saturating_sub(s), state, INVERSE);
    } else if cols >= n + 4 {
        put(&mut row, (cols - n) / 2, name, INVERSE);
    } else if n <= cols {
        put(&mut row, 0, name, INVERSE);
    } else {
        put(&mut row, 0, &dotted(name, cols.saturating_sub(3)), INVERSE);
    }
    row
}

/// "..." and the last `keep` columns of `name`.
fn dotted(name: &str, keep: usize) -> String {
    let mut tail: Vec<char> = Vec::new();
    let mut w = 0;
    for c in name.chars().rev() {
        w += width_at(c, 0, 1);
        if w > keep {
            break;
        }
        tail.push(c);
    }
    tail.reverse();
    format!("...{}", tail.into_iter().collect::<String>())
}

/// A message in the status bar: centered, in brackets; too long for that, from the left. With
/// `dots`, three dots follow it, where nano puts them when more alerts came (only when the
/// message is not wide).
fn message_row(cols: usize, text: &str, style: u8, dots: bool) -> Row {
    let mut row = vec![Cell::blank(); cols];
    let bracketed = format!("[ {text} ]");
    if text_width(&bracketed) <= cols {
        put(
            &mut row,
            (cols - text_width(&bracketed)) / 2,
            &bracketed,
            style,
        );
        let start = (cols - text_width(text)) / 2;
        if dots && start > 4 {
            put(&mut row, cols + 2 - start, "...", style);
        }
    } else {
        put(&mut row, 0, text, style);
    }
    row
}

/// A prompt in the status bar, the whole row inverse; returns it and the cursor column. A label
/// wider than the row less five columns is cut there, before its colon. An
/// answer too long for the row scrolls in steps, as in nano, with `<` just before it and `>` at
/// the end of the row when there is more.
fn prompt_row(cols: usize, prompt: &Prompt) -> (Row, usize) {
    let mut row = vec![Cell::new(' ', INVERSE); cols];
    let label: String = prompt
        .label
        .chars()
        .scan(0, |w, c| {
            *w += width_at(c, 0, 1);
            (*w <= cols.saturating_sub(5)).then_some(c)
        })
        .collect();
    let start = (put(&mut row, 0, &format!("{label}:"), INVERSE) + 1).min(cols);
    let room = cols - start;
    let before: String = prompt.answer.chars().take(prompt.x).collect();
    let x = text_width(&before);
    let step = room.saturating_sub(2).max(1);
    let first = if x == 0 { 0 } else { step * ((x - 1) / step) };
    let (at, cells) = if first > 0 {
        (
            start - 1,
            line_cells(&prompt.answer, first - 1, room + 1, 1),
        )
    } else {
        (start, line_cells(&prompt.answer, 0, room, 1))
    };
    row.splice(at.., cells);
    for c in &mut row {
        c.style = INVERSE;
    }
    (row, start + x - first)
}

/// The keys of a yes-or-no question, with the row and the column (of 16 or half the width) of
/// each; "All" is only in the question of a replacing.
const YES_NO: [(&str, &str, usize, usize); 4] = [
    (" Y", "Yes", 0, 0),
    (" N", "No", 1, 0),
    (" A", "All", 0, 1),
    ("^C", "Cancel", 1, 1),
];

/// The help lines of a yes-or-no question.
fn yes_no_rows(cols: usize, all: bool, cancel: &str) -> [Row; 2] {
    let mut rows = [vec![Cell::blank(); cols], vec![Cell::blank(); cols]];
    let width = (cols / 2).min(16);
    for &(key, label, row, column) in YES_NO.iter().filter(|k| all || k.1 != "All") {
        let from = column * width;
        let to = (from + width).min(cols);
        let cells = &mut rows[row][from.min(cols)..to];
        // Cancel has the key it is bound to; when it has none, it is not shown.
        let key = if label == "Cancel" { cancel } else { key };
        if key.is_empty() {
            continue;
        }
        let after = put(cells, 0, key, INVERSE);
        if after < cells.len() {
            put(cells, after + 1, label, PLAIN);
        }
    }
    rows
}

/// What one row of text shows.
#[derive(Clone, Copy)]
struct Shown {
    line: usize,
    start: usize,
    end: usize,
    /// Where the reading of the line can start: see `cells_between`.
    from: (usize, usize),
    /// Whether the line number goes in front (not before the rows a line wraps into).
    numbered: bool,
}

/// Everything on the screen.
pub fn compose(ed: &Editor) -> Frame {
    let (cols, rows) = (ed.cols, ed.rows);
    let blank = || vec![Cell::blank(); cols];
    let mut screen: Vec<Row> = (0..rows).map(|_| blank()).collect();
    let edit_rows = ed.edit_rows();
    let status = 1 + edit_rows;
    let mut cursor = None;

    if let Mode::Help(top) = ed.mode {
        let mut title = vec![Cell::new(' ', INVERSE); cols];
        let name = crate::commands::HELP_TITLE;
        put(
            &mut title,
            cols.saturating_sub(text_width(name)) / 2,
            name,
            INVERSE,
        );
        screen[0] = title;
        let help = ed.help_lines();
        for (r, line) in help
            .iter()
            .skip(top)
            .take(edit_rows.saturating_sub(1))
            .enumerate()
        {
            screen[1 + r] = line_cells(line, 0, cols, 8);
        }
    } else if matches!(ed.mode, Mode::Ask(Question::Locked(..))) && !ed.started {
        // As nano: the question about a locked file comes before anything of the file.
    } else {
        screen[0] = title_row(cols, &ed.upper_left(), ed.display_name(), ed.buf.modified);
        let margin = ed.margin();
        let width = cols.saturating_sub(margin);
        let digits = margin.saturating_sub(1);
        // What each row shows: a line, from the screen column `start` to `end`, where in the line
        // that is, and whether a line number goes before it.
        let shown: Vec<Shown> = if ed.opts.softwrap {
            ed.visible_spans()
                .iter()
                .map(|s| Shown {
                    line: s.line,
                    start: s.start,
                    end: s.end.min(s.start + width),
                    from: (s.at, s.at_col),
                    numbered: s.start == 0,
                })
                .collect()
        } else {
            (0..edit_rows)
                .map_while(|r| {
                    let line = ed.buf.top + r;
                    (line < ed.buf.lines()).then(|| {
                        let start = if line == ed.buf.line {
                            page_start(ed.buf.column(ed.opts.tabsize), width)
                        } else {
                            0
                        };
                        Shown {
                            line,
                            start,
                            end: start + width,
                            from: (0, 0),
                            numbered: true,
                        }
                    })
                })
                .collect()
        };
        let cursor_row = ed.opts.softwrap.then(|| ed.cursor_span());
        // The colours of the lines shown, all at once because a match may go from one line to the
        // next.
        let (first_line, painted) = match (shown.first(), shown.last()) {
            (Some(a), Some(b)) if !ed.syntax.is_empty() => {
                (a.line, syntax::paint(&ed.syntax, &ed.buf, a.line, b.line))
            }
            _ => (0, Vec::new()),
        };
        let mut cached: Option<(usize, String)> = None;
        for (r, row_shown) in shown.iter().enumerate().take(rows.saturating_sub(1)) {
            let Shown {
                line,
                start,
                end,
                from,
                numbered,
            } = *row_shown;
            if cached.as_ref().is_none_or(|c| c.0 != line) {
                cached = Some((line, ed.buf.line_text(line)));
            }
            let text = &cached.as_ref().expect("just set").1;
            let mut cells = if ed.opts.softwrap {
                cells_between(text, from, (start, end), width, ed.opts.tabsize).0
            } else {
                line_cells(text, start, width, ed.opts.tabsize)
            };
            if let Some(hues) = painted.get(line - first_line) {
                colour_cells(&mut cells, text, hues, from, (start, end), ed.opts.tabsize);
            }
            if let Some((_, from, to)) = ed.spot.filter(|s| s.0 == line) {
                let from = column_of(text, from, ed.opts.tabsize);
                let to = column_of(text, to, ed.opts.tabsize);
                // An empty match is a blank cell that hides the character there, as in nano.
                let empty = from == to;
                for col in from.max(start)..(to + usize::from(empty)).min(end) {
                    if empty {
                        cells[col - start].text = " ".into();
                    }
                    cells[col - start].style = SPOT;
                    cells[col - start].hue = Hue::default();
                }
            }
            // nano takes the selection away while it asks about a match, and gives it back after.
            let asking = matches!(ed.mode, Mode::Ask(Question::Replace(_)));
            if let Some((lo, hi)) = ed.buf.selection().filter(|_| !asking) {
                let first = ed.buf.text.line_to_char(line);
                let len = text.chars().count();
                if lo < first + len && hi > first {
                    let from = column_of(text, lo.saturating_sub(first), ed.opts.tabsize);
                    let to = column_of(text, (hi - first).min(len), ed.opts.tabsize);
                    for col in from.max(start)..to.min(end) {
                        if let Some(cell) = cells.get_mut(col - start) {
                            cell.style = INVERSE;
                            cell.hue = Hue::default();
                        }
                    }
                }
            }
            let row = &mut screen[1 + r];
            if margin > 0 {
                let number = if numbered {
                    format!("{}", line + 1)
                } else {
                    String::new()
                };
                put(row, 0, &format!("{number:>digits$}"), INVERSE);
            }
            row.splice(margin.min(cols).., cells);
            if line == ed.buf.line && cursor_row.is_none_or(|c| c.start == start) {
                let col = ed.buf.column(ed.opts.tabsize);
                cursor = Some((1 + r, margin + col - start));
            }
        }
        if ed.spot.is_some() {
            cursor = None;
        }
    }

    if status < rows {
        match &ed.mode {
            Mode::Prompt(p) => {
                let (row, col) = prompt_row(cols, p);
                screen[status] = row;
                cursor = Some((status, col.min(cols.saturating_sub(1))));
            }
            Mode::Ask(question) => {
                let mut row = vec![Cell::new(' ', INVERSE); cols];
                let room = cols.saturating_sub(1);
                let col = put(&mut row[..room], 0, &question.text(), INVERSE);
                screen[status] = row;
                cursor = (!matches!(question, Question::Replace(_)))
                    .then_some((status, col.min(cols.saturating_sub(1))));
            }
            _ => {
                if let Some(m) = &ed.message {
                    let style = if m.error { ERROR } else { INVERSE };
                    screen[status] = message_row(cols, &m.text, style, m.dots);
                } else if ed.opts.constantshow && matches!(ed.mode, Mode::Edit) {
                    screen[status] = message_row(cols, &ed.location(), INVERSE, false);
                }
            }
        }
    }
    if matches!(ed.mode, Mode::Help(_)) {
        cursor = None;
    }

    // A warning shows alone for a moment.
    if ed.warning.is_some() {
        cursor = None;
    }
    if !ed.opts.nohelp && rows >= 3 && ed.warning.is_none() {
        let bars = match &ed.mode {
            Mode::Ask(question) => {
                let lines = ed.help_lines_of();
                let cancel = lines.first().map_or("", |l| l.0.as_str());
                yes_no_rows(cols, matches!(question, Question::Replace(_)), cancel)
            }
            _ => {
                let lines = ed.help_lines_of();
                let items: Vec<(&str, &str)> = lines.iter().map(|l| (l.0.as_str(), l.1)).collect();
                help_rows(&items, cols)
            }
        };
        let [a, b] = bars;
        screen[rows - 2] = a;
        screen[rows - 1] = b;
    }
    Frame {
        rows: screen,
        cursor,
    }
}

/// The terminal, in raw mode on the alternate screen while the editor runs.
pub struct Terminal {
    saved: Termios,
    /// The rows as the terminal shows them now; empty after a resize or a refresh.
    shown: Vec<Row>,
}

impl Terminal {
    pub fn start() -> io::Result<Self> {
        let saved = termios::tcgetattr(io::stdin())?;
        let mut raw = saved.clone();
        raw.make_raw();
        termios::tcsetattr(io::stdin(), OptionalActions::Flush, &raw)?;
        let mut out = io::stdout().lock();
        out.write_all(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[?2004h")?;
        out.flush()?;
        Ok(Self {
            saved,
            shown: Vec::new(),
        })
    }

    /// The size as (columns, rows).
    pub fn size() -> (usize, usize) {
        match termios::tcgetwinsize(io::stdout()) {
            Ok(w) if w.ws_col > 0 && w.ws_row > 0 => (usize::from(w.ws_col), usize::from(w.ws_row)),
            _ => (80, 24),
        }
    }

    /// Makes the next draw repaint everything.
    pub fn invalidate(&mut self) {
        self.shown.clear();
    }

    /// Shows `frame`, sending only the rows that changed.
    pub fn draw(&mut self, frame: &Frame) -> io::Result<()> {
        let mut out = String::new();
        if self.shown.len() != frame.rows.len()
            || self.shown.first().map(Vec::len) != frame.rows.first().map(Vec::len)
        {
            out.push_str("\x1b[0m\x1b[H\x1b[2J");
            self.shown = frame
                .rows
                .iter()
                .map(|r| vec![Cell::blank(); r.len()])
                .collect();
        }
        out.push_str("\x1b[?25l");
        for (r, row) in frame.rows.iter().enumerate() {
            if self.shown[r] != *row {
                draw_row(&mut out, r, row);
                self.shown[r].clone_from(row);
            }
        }
        if let Some((r, c)) = frame.cursor {
            out.push_str(&format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1));
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(out.as_bytes())?;
        stdout.flush()
    }

    /// Leaves the alternate screen and restores the terminal's settings.
    pub fn stop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?2004l\x1b[?1049l");
        let _ = out.flush();
        let _ = termios::tcsetattr(io::stdin(), OptionalActions::Flush, &self.saved);
    }
}

/// The colour `n` of the 256-colour palette as the parameters of a graphic rendition: `base` is
/// 30 for the foreground and 40 for the background.
fn colour_code(out: &mut String, colour: Option<u8>, base: u8) {
    match colour {
        None => {}
        Some(n) if n < 8 => out.push_str(&format!(";{}", base + n)),
        Some(n) if n < 16 => out.push_str(&format!(";{}", base + 60 + n - 8)),
        Some(n) => out.push_str(&format!(";{};5;{n}", base + 8)),
    }
}

/// What the terminal is told before a cell of this look. The colours of a syntax are numbers
/// that come from the nanorc, so nothing of the text can get into this.
fn sgr(style: u8, hue: Hue) -> String {
    match style {
        INVERSE => "\x1b[0;7m".into(),
        ERROR => "\x1b[0;1;37;41m".into(),
        SPOT => "\x1b[0;30;103m".into(),
        _ => {
            let mut codes = String::from("\x1b[0");
            if hue.bold {
                codes.push_str(";1");
            }
            if hue.italic {
                codes.push_str(";3");
            }
            colour_code(&mut codes, hue.fg, 30);
            colour_code(&mut codes, hue.bg, 40);
            codes.push('m');
            codes
        }
    }
}

fn draw_row(out: &mut String, r: usize, row: &[Cell]) {
    out.push_str(&format!("\x1b[{};1H", r + 1));
    // Blank plain cells at the end are cleared in one go, unless a syntax gave them a colour
    // (a background shows on blanks): clearing would leave them uncoloured.
    let end = row
        .iter()
        .rposition(|c| c.text != " " || c.style != PLAIN || c.hue != Hue::default())
        .map_or(0, |i| i + 1);
    let mut look = None;
    for cell in &row[..end] {
        let hue = if cell.style == PLAIN {
            cell.hue
        } else {
            Hue::default()
        };
        if look != Some((cell.style, hue)) {
            look = Some((cell.style, hue));
            out.push_str(&sgr(cell.style, hue));
        }
        // A byte that is not valid UTF-8 shows as the replacement character, as in nano.
        let mut chars = cell.text.chars().peekable();
        while let Some(c) = chars.next() {
            if raw_byte(c).is_some() && chars.next_if_eq(&MARK).is_some() {
                out.push(char::REPLACEMENT_CHARACTER);
            } else {
                out.push(c);
            }
        }
    }
    out.push_str("\x1b[0m");
    if end < row.len() {
        out.push_str("\x1b[K");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::{Buffer, Format};
    use crate::config::Options;
    use crate::syntax::{self, SyntaxDef};

    fn text(row: &[Cell]) -> String {
        row.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn widths() {
        assert_eq!(width_at('\t', 3, 8), 5);
        assert_eq!(width_at('\x01', 0, 8), 2);
        assert_eq!(width_at('日', 0, 8), 2);
        assert_eq!(width_at('\u{301}', 0, 8), 0);
        assert!(is_zero_width('\u{200b}'));
        assert!(!is_zero_width('a'));
        assert_eq!(control_letter('\x01'), 'A');
        assert_eq!(control_letter('\u{85}'), 'e');
    }

    #[test]
    fn long_lines_are_marked() {
        let line = "0123456789abcdefghij";
        assert_eq!(text(&line_cells(line, 0, 10, 8)), "012345678>");
        assert_eq!(text(&line_cells(line, 8, 10, 8)), "<9abcdefg>");
        assert_eq!(text(&line_cells(line, 16, 10, 8)), "<hij      ");
        assert_eq!(text(&line_cells("12345678日本", 0, 10, 8)), "12345678[>");
        assert_eq!(text(&line_cells("1日本日本", 1, 4, 8)), "<][>");
        assert_eq!(text(&line_cells("1日本日本", 2, 4, 8)), "<本>");
        assert_eq!(text(&line_cells("a\tb", 0, 10, 4)), "a   b     ");
        assert_eq!(text(&line_cells("e\u{301}x", 0, 3, 8)), "e\u{301}x ");
        assert_eq!(text(&line_cells("e\u{301}x", 0, 0, 8)), "");
        assert_eq!(text(&line_cells("ab\u{301}", 1, 1, 8)), "b\u{301}");
    }

    #[test]
    fn page_starts() {
        assert_eq!(page_start(37, 40), 0);
        assert_eq!(page_start(38, 40), 32);
        assert_eq!(page_start(72, 40), 64);
        assert_eq!(page_start(52, 40), 32);
    }

    #[test]
    fn titles() {
        let t = |cols, name, m| {
            text(&title_row(cols, "GNU nano 8.4", name, m))
                .trim_end()
                .to_string()
        };
        assert_eq!(
            t(80, "ab", false),
            format!("  GNU nano 8.4{}ab", " ".repeat(27))
        );
        assert_eq!(t(20, "abcdefghij", true), "abcdefghij  Modified");
        assert_eq!(t(15, "fffffffff", true), "...fff Modified");
        assert_eq!(t(12, "fffffffff", false), "fffffffff");
    }

    #[test]
    fn help_lines() {
        let items = [
            ("^G", "Help"),
            ("^X", "Exit"),
            ("^O", "Write Out"),
            ("^R", "Read File"),
        ];
        let [a, b] = help_rows(&items, 15);
        assert_eq!(text(&a), "^G Help^O Write");
        assert_eq!(text(&b), "^X Exit^R Read ");
    }

    /// `r`, `g`, `b` for a red, green, blue cell, `.` for one with no colour.
    fn colours(cells: &[Cell]) -> String {
        let letter = |c: &Cell| match c.hue.fg {
            Some(1) => 'r',
            Some(2) => 'g',
            Some(4) => 'b',
            _ => '.',
        };
        cells.iter().map(letter).collect()
    }

    fn hues_of(line: &str, letters: &str) -> Vec<Hue> {
        let mut hues = Vec::new();
        for (c, l) in line.chars().zip(letters.chars()) {
            let fg = match l {
                'r' => Some(1),
                'g' => Some(2),
                _ => Some(4),
            };
            hues.extend(std::iter::repeat_n(
                Hue {
                    fg,
                    ..Hue::default()
                },
                c.len_utf8(),
            ));
        }
        hues
    }

    #[test]
    fn colours_follow_tabs_and_wide_characters() {
        // a, a tab of 3 columns (tabsize 4), a wide character, b: one hue for each character.
        let line = "a\t\u{65e5}b";
        let hues = hues_of(line, "rgbr");
        let paint = |from, start, end| {
            let mut cells = vec![Cell::blank(); end - start];
            colour_cells(&mut cells, line, &hues, from, (start, end), 4);
            colours(&cells)
        };
        assert_eq!(paint((0, 0), 0, 8), "rgggbbr.");
        // Scrolled sideways: characters left of the window have no cells.
        assert_eq!(paint((0, 0), 2, 7), "ggbbr");
        // A wide character cut by the window colours the half that is shown.
        assert_eq!(paint((0, 0), 5, 7), "br");
        // A soft-wrapped row starts in the middle of the line.
        assert_eq!(paint((2, 4), 4, 7), "bbr");
        // A wide character that does not fit is not coloured past the edge.
        assert_eq!(paint((0, 0), 0, 5), "rgggb");
    }

    #[test]
    fn marks_and_selection_are_not_coloured() {
        let line = "0123456789abcdefghij";
        let hues = hues_of(line, "rrrrrrrrrrgggggggggg");
        let mut cells = line_cells(line, 4, 10, 8);
        colour_cells(&mut cells, line, &hues, (0, 0), (4, 14), 8);
        // The `<` and `>` are drawn inverse and keep no hue.
        assert_eq!(colours(&cells), ".rrrrrggg.");
    }

    /// Everything the terminal receives for the rows, with the editor's own sequences taken out:
    /// a graphic rendition, a move of the cursor and a clear of the rest of the row.
    fn without_own_sequences(out: &str) -> String {
        let mut rest = out;
        let mut text = String::new();
        while let Some(at) = rest.find('\x1b') {
            text.push_str(&rest[..at]);
            let tail = &rest[at + 1..];
            let body = tail.strip_prefix('[').expect("a sequence that is not ours");
            let end = body
                .find(|c: char| !(c.is_ascii_digit() || c == ';'))
                .unwrap();
            assert!(matches!(&body[end..end + 1], "m" | "H" | "K"), "{out:?}");
            rest = &body[end + 1..];
        }
        text.push_str(rest);
        text
    }

    #[test]
    fn escape_sequences_in_the_file_never_reach_the_terminal() {
        // Rules that match the escape sequences themselves, and everything else: the colours must
        // still come only from the nanorc, and the bytes of the file only as printable text.
        let attack = "\x1b[31mred\x1b]0;title\x07\x1b[2J\u{9b}31m\u{9d}x\x00\x7f \x1b[?1049h\x1b";
        for width in [80, 20, 7] {
            for softwrap in [false, true] {
                let opts = Options {
                    softwrap,
                    ..Options::default()
                };
                let mut ed = Editor::open(opts, width, 12);
                ed.buf = Buffer::new(&format!("{attack}\n{attack}"), Format::Unix);
                let mut defs = Vec::new();
                for rest in [
                    "red \".*\"",
                    "blue start=\"\\]\" end=\"\"",
                    "red start=\"\\[\" end=\"Z\"",
                    "green \"\\[[0-9;]*m\"",
                ] {
                    defs.extend(syntax::parse_rules(rest, false).0);
                }
                let def = SyntaxDef {
                    name: "t".into(),
                    files: Vec::new(),
                    headers: Vec::new(),
                    rules: defs,
                };
                ed.syntax = syntax::compile(&def).0;
                let frame = compose(&ed);
                let mut out = String::new();
                for (r, row) in frame.rows.iter().enumerate() {
                    draw_row(&mut out, r, row);
                }
                // The test would pass for no colours at all, so check that there are some.
                if width == 80 {
                    assert!(out.contains("\x1b[0;31m") && out.contains("\x1b[0;32m"));
                }
                let text = without_own_sequences(&out);
                let bad = |c: char| c < ' ' || ('\x7f'..='\u{9f}').contains(&c);
                assert!(!text.chars().any(bad), "{text:?}");
            }
        }
    }

    #[test]
    fn tiny_screens_work() {
        use crate::config::Options;
        use crate::keys::Key;
        for (cols, rows) in [(1, 1), (1, 2), (2, 3), (5, 4), (12, 5)] {
            let opts = Options {
                linenumbers: true,
                ..Options::default()
            };
            let mut ed = Editor::open(opts, cols, rows);
            let keys = [
                Key::Char('a'),
                Key::Char('日'),
                Key::Ctrl('I'),
                Key::Char('\u{301}'),
                Key::Ctrl('M'),
                Key::Ctrl('O'),
                Key::Char('x'),
                Key::Ctrl('C'),
                Key::Ctrl('G'),
                Key::Ctrl('X'),
                Key::Ctrl('X'),
            ];
            for key in keys {
                ed.key(key);
                let frame = compose(&ed);
                assert_eq!(frame.rows.len(), rows);
                assert!(frame.rows.iter().all(|r| r.len() == cols));
            }
        }
    }
}
