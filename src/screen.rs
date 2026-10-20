// SPDX-License-Identifier: MIT
//! The screen: how characters show, what goes on each row (the title bar, the text, the status
//! bar and the help lines), and the terminal, which is sent only the rows that changed.

use crate::buffer::{MARK, column_of, raw_byte};
use crate::commands::{Editor, Mode, Prompt, Question};
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
/// wide character has no text.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub text: String,
    pub style: u8,
}

impl Cell {
    fn new(c: char, style: u8) -> Self {
        Self {
            text: c.to_string(),
            style,
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
            row[col] = Cell { text: p, style };
            col += 1;
        }
    }
    col
}

pub fn text_width(text: &str) -> usize {
    text.chars().map(|c| width_at(c, 0, 1)).sum()
}

/// The cells of a text line from screen column `start`, `width` of them. As in nano, a line
/// scrolled to the right starts with `<`, a line that goes on past the edge ends with `>`, and
/// a wide character cut in half by them shows its other half as `]` or `[`.
pub fn line_cells(line: &str, start: usize, width: usize, tabsize: usize) -> Row {
    let mut cells = vec![Cell::blank(); width];
    let end = start + width;
    let mut col = 0;
    // The cell where the last character started, for its combining marks.
    let mut last = None;
    for c in line.chars() {
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

/// A message in the status bar: centered, in brackets; too long for that, from the left.
fn message_row(cols: usize, text: &str, style: u8) -> Row {
    let mut row = vec![Cell::blank(); cols];
    let bracketed = format!("[ {text} ]");
    if text_width(&bracketed) <= cols {
        put(
            &mut row,
            (cols - text_width(&bracketed)) / 2,
            &bracketed,
            style,
        );
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

const YES_NO: [(&str, &str); 3] = [(" Y", "Yes"), (" N", "No"), ("^C", "Cancel")];

/// The help lines of a yes-or-no question.
fn yes_no_rows(cols: usize) -> [Row; 2] {
    let mut rows = [vec![Cell::blank(); cols], vec![Cell::blank(); cols]];
    let width = (cols / 2).min(16);
    for (i, (key, label)) in YES_NO.iter().enumerate() {
        let (row, from) = if i == 0 { (0, 0) } else { (1, (i - 1) * width) };
        let to = (from + width).min(cols);
        let cells = &mut rows[row][from.min(cols)..to];
        let after = put(cells, 0, key, INVERSE);
        if after < cells.len() {
            put(cells, after + 1, label, PLAIN);
        }
    }
    rows
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
    } else if matches!(ed.mode, Mode::Ask(Question::Locked(_))) {
        // As nano: the question about a locked file comes before anything of the file.
    } else {
        screen[0] = title_row(cols, &ed.program, ed.display_name(), ed.buf.modified);
        let margin = ed.margin();
        let width = cols.saturating_sub(margin);
        let digits = margin.saturating_sub(1);
        for r in 0..edit_rows.min(rows.saturating_sub(1)) {
            let line = ed.buf.top + r;
            if line >= ed.buf.lines() {
                break;
            }
            let text = ed.buf.line_text(line);
            let start = if line == ed.buf.line {
                page_start(ed.buf.column(ed.opts.tabsize), width)
            } else {
                0
            };
            let mut cells = line_cells(&text, start, width, ed.opts.tabsize);
            if let Some((_, from, to)) = ed.spot.filter(|s| s.0 == line) {
                let from = column_of(&text, from, ed.opts.tabsize);
                let to = column_of(&text, to, ed.opts.tabsize);
                for col in from.max(start)..to.min(start + width) {
                    cells[col - start].style = SPOT;
                }
            }
            let row = &mut screen[1 + r];
            if margin > 0 {
                put(row, 0, &format!("{:>digits$}", line + 1), INVERSE);
            }
            row.splice(margin.min(cols).., cells);
            if line == ed.buf.line {
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
                let col = put(&mut row[..room], 0, &question.text(&ed.name), INVERSE);
                screen[status] = row;
                cursor = Some((status, col.min(cols.saturating_sub(1))));
            }
            _ => {
                if let Some(m) = &ed.message {
                    screen[status] =
                        message_row(cols, &m.text, if m.error { ERROR } else { INVERSE });
                } else if ed.opts.constantshow && matches!(ed.mode, Mode::Edit) {
                    screen[status] = message_row(cols, &ed.location(), INVERSE);
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
            Mode::Ask(_) => yes_no_rows(cols),
            mode => help_rows(crate::commands::menu(mode), cols),
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
        out.write_all(b"\x1b[?1049h\x1b[H\x1b[2J")?;
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
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        let _ = termios::tcsetattr(io::stdin(), OptionalActions::Flush, &self.saved);
    }
}

fn sgr(style: u8) -> &'static str {
    match style {
        INVERSE => "\x1b[0;7m",
        ERROR => "\x1b[0;1;37;41m",
        SPOT => "\x1b[0;30;103m",
        _ => "\x1b[0m",
    }
}

fn draw_row(out: &mut String, r: usize, row: &[Cell]) {
    out.push_str(&format!("\x1b[{};1H", r + 1));
    // Blank plain cells at the end are cleared in one go.
    let end = row
        .iter()
        .rposition(|c| c.text != " " || c.style != PLAIN)
        .map_or(0, |i| i + 1);
    let mut style = u8::MAX;
    for cell in &row[..end] {
        if cell.style != style {
            style = cell.style;
            out.push_str(sgr(style));
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
