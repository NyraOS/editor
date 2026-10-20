// SPDX-License-Identifier: MIT
//! The editor's state and its commands: what each key does.

use crate::buffer::{self, Buffer, Format, next_index, prev_index};
use crate::config::Options;
use crate::files::{self, Expected, Locking, Meta, SaveError, SaveOptions};
use crate::keys::{Key, Mods, Named};
use std::io;
use std::path::{Path, PathBuf};

/// How many keys a message stays for, as in nano.
const MESSAGE_KEYS: u8 = 20;

pub struct Message {
    pub text: String,
    pub error: bool,
    /// The keys still to come before it goes.
    keys_left: u8,
}

pub enum Mode {
    Edit,
    /// The help text, from this line.
    Help(usize),
    Prompt(Prompt),
    /// A yes-or-no question.
    Ask(Question),
}

/// The questions that write a file carry its name and whether to exit after writing it.
pub enum Question {
    /// ^X on a modified buffer.
    SaveOnExit,
    /// Writing under another name than the file's.
    OtherName(String, bool),
    /// Writing under a name another file has.
    Overwrite(String, bool),
    /// The file changed on disk since it was read or written.
    Changed(String, bool),
    /// Another editor holds the lock file.
    Locked(files::LockInfo),
}

impl Question {
    /// The question as nano words it, with the space after it where nano has one.
    pub fn text(&self, name: &str) -> String {
        match self {
            Question::SaveOnExit => "Save modified buffer? ".into(),
            Question::OtherName(..) => "Save file under DIFFERENT NAME? ".into(),
            Question::Overwrite(file, _) => format!("File \"{file}\" exists; OVERWRITE? "),
            Question::Changed(..) => {
                "File was modified since you opened it; continue saving? ".into()
            }
            Question::Locked(info) => format!(
                "File {name} is being edited by {} (with {}, PID {}); open anyway?",
                info.user, info.program, info.pid
            ),
        }
    }
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub answer: String,
    /// The cursor in the answer, in characters.
    pub x: usize,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PromptKind {
    /// The file name to write to; then exit if it was for ^X.
    Write {
        exit: bool,
    },
    Search,
}

pub struct Editor {
    pub buf: Buffer,
    /// The file name, empty for a new buffer.
    pub name: String,
    pub opts: Options,
    /// What the title bar shows on the left.
    pub program: String,
    pub cols: usize,
    pub rows: usize,
    pub mode: Mode,
    pub message: Option<Message>,
    /// The lines cut with ^K, and whether the last key was a ^K (the next one adds to them).
    cut: String,
    cutting: bool,
    last_search: Option<String>,
    /// What the last search found, highlighted until the next key: line, from, to (characters).
    pub spot: Option<(usize, usize, usize)>,
    /// The file as read or written last, to notice changes on disk; None for a new file.
    meta: Option<Meta>,
    /// The lock file held, and whether it says the buffer is modified.
    lock: Option<files::Lock>,
    lock_says_modified: bool,
    /// A warning shown for a moment, with nothing else, before this question (as nano does).
    pub warning: Option<Question>,
    /// The editor is done: it exits.
    pub done: bool,
}

pub const HELP_TITLE: &str = "Help";

/// Keys and labels for the help lines at the bottom, as in nano.
const MAIN_MENU: &[(&str, &str)] = &[
    ("^G", "Help"),
    ("^X", "Exit"),
    ("^O", "Write Out"),
    ("^R", "Read File"),
    ("^F", "Where Is"),
    ("^\\", "Replace"),
    ("^K", "Cut"),
    ("^U", "Paste"),
    ("^T", "Execute"),
    ("^J", "Justify"),
    ("^C", "Location"),
    ("^/", "Go To Line"),
    ("M-U", "Undo"),
    ("M-E", "Redo"),
    ("M-A", "Set Mark"),
    ("M-6", "Copy"),
    ("M-]", "To Bracket"),
    ("^B", "Where Was"),
    ("M-B", "Previous"),
    ("M-F", "Next"),
    ("◂", "Back"),
    ("▸", "Forward"),
    ("^◂", "Prev Word"),
    ("^▸", "Next Word"),
    ("^A", "Home"),
    ("^E", "End"),
    ("^P", "Prev Line"),
    ("^N", "Next Line"),
    ("M-▴", "Scroll Up"),
    ("M-▾", "Scroll Down"),
    ("^▴", "Prev Block"),
    ("^▾", "Next Block"),
    ("M-(", "Begin of Paragraph"),
    ("M-)", "End of Paragraph"),
    ("M-Home", "Top Row"),
    ("M-End", "Bottom Row"),
    ("^Y", "Prev Page"),
    ("^V", "Next Page"),
    ("M-\\", "First Line"),
    ("M-/", "Last Line"),
    ("M-◂", "Prev File"),
    ("M-▸", "Next File"),
    ("^I", "Tab"),
    ("^M", "Enter"),
    ("^H", "Backspace"),
    ("^D", "Delete"),
    ("M-Bsp", "Chop Left"),
    ("^Del", "Chop Right"),
    ("M-T", "Cut Till End"),
    ("M-J", "Full Justify"),
    ("M-D", "Word Count"),
    ("M-V", "Verbatim"),
    ("M-}", "Indent"),
    ("M-{", "Unindent"),
];

const HELP_MENU: &[(&str, &str)] = &[
    ("^L", "Refresh"),
    ("^X", "Close"),
    ("^F", "Where Is"),
    ("^B", "Where Was"),
    ("M-B", "Previous"),
    ("M-F", "Next"),
    ("^P", "Prev Line"),
    ("^N", "Next Line"),
    ("^Y", "Prev Page"),
    ("^V", "Next Page"),
    ("M-\\", "First Line"),
    ("M-/", "Last Line"),
];

const WRITE_MENU: &[(&str, &str)] = &[
    ("^G", "Help"),
    ("^C", "Cancel"),
    ("M-D", "DOS Format"),
    ("M-M", "Mac Format"),
    ("M-A", "Append"),
    ("M-P", "Prepend"),
    ("M-B", "Backup File"),
    ("^T", "Browse"),
];

const SEARCH_MENU: &[(&str, &str)] = &[
    ("^G", "Help"),
    ("^C", "Cancel"),
    ("M-C", "Case Sens"),
    ("M-R", "Reg.exp."),
    ("M-B", "Backwards"),
    ("^R", "Replace"),
    ("^P", "Older"),
    ("^N", "Newer"),
    ("^T", "Go To Line"),
];

/// The help lines for a mode.
pub fn menu(mode: &Mode) -> &'static [(&'static str, &'static str)] {
    match mode {
        Mode::Edit | Mode::Ask(_) => MAIN_MENU,
        Mode::Help(_) => HELP_MENU,
        Mode::Prompt(p) => match p.kind {
            PromptKind::Write { .. } => WRITE_MENU,
            PromptKind::Search => SEARCH_MENU,
        },
    }
}

/// The help text: what the editor is, then the keys. Written for this editor, not nano's.
const HELP_INTRO: &str = "\
This editor works like GNU nano, with the same keys, but it is not GNU nano. The top row shows \
the version, the name of the file and whether you changed it. Below it is the text. The third \
row from the bottom shows messages, and the two rows under it show the most used keys.\n\
\n\
^ means Ctrl: ^X is Ctrl+X. M- means Alt, or Esc and then the key: M-U is Alt+U. These keys \
work in the text:";

const HELP_KEYS: &[(&str, &str, &str)] = &[
    ("^G", "F1", "Show this help"),
    ("^X", "F2", "Exit, asking whether to save the changes"),
    ("^O", "F3", "Write the text to a file"),
    ("^S", "", "Save the file without asking for its name"),
    ("", "", ""),
    ("^F", "^W", "Search forward"),
    ("M-W", "", "Search forward again"),
    ("", "", ""),
    ("^K", "F9", "Cut the line, adding to the lines just cut"),
    ("^U", "F10", "Paste the cut lines at the cursor"),
    ("", "", ""),
    ("^C", "F11", "Show where the cursor is"),
    ("", "", ""),
    ("◂", "", "Go one character back"),
    ("▸", "", "Go one character forward"),
    ("^◂", "", "Go to the previous word"),
    ("^▸", "", "Go to the next word"),
    ("^A", "Home", "Go to the start of the line"),
    ("^E", "End", "Go to the end of the line"),
    ("^P", "Up", "Go to the line above"),
    ("^N", "Down", "Go to the line below"),
    ("^Y", "PgUp", "Go one page up"),
    ("^V", "PgDn", "Go one page down"),
    ("M-\\", "^Home", "Go to the first line"),
    ("M-/", "^End", "Go to the last line"),
    ("", "", ""),
    ("^I", "Tab", "Insert a tab"),
    ("^M", "Enter", "Start a new line"),
    ("^H", "Bsp", "Delete the character before the cursor"),
    ("^D", "Del", "Delete the character under the cursor"),
    ("", "", ""),
    ("^L", "", "Draw the screen again"),
];

fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

/// An error's text without Rust's " (os error N)".
fn error_text(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

impl Editor {
    /// Opens the file in `opts`, or a new buffer.
    pub fn open(opts: Options, cols: usize, rows: usize) -> Self {
        let mut ed = Self {
            buf: Buffer::new("", Format::Unix),
            name: opts.file.clone().unwrap_or_default(),
            opts,
            program: format!("Editor {}", env!("CARGO_PKG_VERSION")),
            cols,
            rows,
            mode: Mode::Edit,
            message: None,
            cut: String::new(),
            cutting: false,
            last_search: None,
            spot: None,
            meta: None,
            lock: None,
            lock_says_modified: false,
            warning: None,
            done: false,
        };
        if ed.name.is_empty() {
            ed.say("For basic help, type Ctrl+G.");
        } else {
            ed.read_file();
            if !ed.name.is_empty() {
                ed.take_lock(false);
            }
        }
        if let Some((line, col)) = ed.opts.start {
            ed.goto(line, col);
        }
        ed
    }

    fn read_file(&mut self) {
        match files::read(Path::new(&self.name)) {
            Ok(loaded) => {
                let (text, format) = buffer::convert_line_ends(buffer::decode(loaded.bytes));
                self.buf = Buffer::new(&text, format);
                self.meta = Some(loaded.meta);
                let mut msg = format!("Read {}", plural(self.buf.file_lines(), "line"));
                self.magic_line();
                match format {
                    Format::Dos => msg.push_str(" (converted from DOS format)"),
                    Format::Mac => msg.push_str(" (converted from Mac format)"),
                    Format::Unix => {}
                }
                self.say(&msg);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.say("New File"),
            // A directory or a device: nano says so and edits a new buffer.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::IsADirectory | io::ErrorKind::InvalidInput
                ) =>
            {
                let name = std::mem::take(&mut self.name);
                self.error(&format!("\"{name}\" {e}"));
            }
            Err(e) => self.error(&format!("Error reading {}: {}", self.name, error_text(&e))),
        }
    }

    /// Locks the file, as nano does with `--locking` (here always). When the lock file cannot
    /// be made (a directory without write permission), the file is edited without one.
    fn take_lock(&mut self, force: bool) {
        match files::lock(Path::new(&self.name), force) {
            Ok(Locking::Locked(lock)) => {
                let _ = lock.set_modified(self.buf.modified);
                self.lock_says_modified = self.buf.modified;
                self.lock = Some(lock);
            }
            Ok(Locking::Busy(info)) => self.mode = Mode::Ask(Question::Locked(info)),
            Ok(Locking::Bad(path)) => {
                self.error(&format!("Bad lock file is ignored: {}", path.display()));
            }
            Err(_) => {}
        }
    }

    /// Writes the emergency copy of a modified buffer (the terminal went away, or SIGHUP or
    /// SIGTERM came); returns where.
    pub fn emergency_save(&mut self) -> Option<PathBuf> {
        if !self.buf.modified {
            return None;
        }
        let name = (!self.name.is_empty()).then(|| Path::new(&self.name));
        files::emergency_save(name, &self.buf.to_bytes()).ok()
    }

    /// `+LINE,COLUMN`, 1-based; negative numbers count from the end.
    fn goto(&mut self, line: i64, col: i64) {
        let from_end = |n: i64, len: usize| {
            if n < 0 {
                (len as i64 + n).max(0) as usize
            } else {
                (n.max(1) - 1) as usize
            }
        };
        self.buf.line = from_end(line, self.buf.lines()).min(self.buf.lines() - 1);
        let len = self.buf.line_len(self.buf.line);
        self.buf.x = from_end(col, len + 1).min(len);
        self.buf.keep_column(self.opts.tabsize);
        self.center();
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "New Buffer"
        } else {
            &self.name
        }
    }

    /// The rows for text: all but the title bar, the status bar and the help lines.
    pub fn edit_rows(&self) -> usize {
        let bars = if self.opts.nohelp { 2 } else { 4 };
        self.rows.saturating_sub(bars).max(1)
    }

    /// The width of the line numbers and the space after them.
    pub fn margin(&self) -> usize {
        if self.opts.linenumbers {
            self.buf.lines().to_string().len().max(2) + 1
        } else {
            0
        }
    }

    fn say(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_string(),
            error: false,
            keys_left: MESSAGE_KEYS,
        });
    }

    fn error(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_string(),
            error: true,
            keys_left: MESSAGE_KEYS,
        });
    }

    pub fn resized(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows = rows;
        self.message = None;
        self.scroll_to_cursor();
    }

    /// One key.
    pub fn key(&mut self, key: Key) {
        // nano counts the bytes of a typed character as keys.
        let n = match key {
            Key::Char(c) => c.len_utf8() as u8,
            _ => 1,
        };
        if let Some(m) = &mut self.message {
            if m.keys_left <= n || self.opts.constantshow {
                self.message = None;
            } else {
                m.keys_left -= n;
            }
        }
        self.spot = None;
        let cutting = std::mem::take(&mut self.cutting);
        match &mut self.mode {
            Mode::Edit => self.edit_key(key, cutting),
            Mode::Help(_) => self.help_key(key),
            Mode::Prompt(_) => self.prompt_key(key),
            Mode::Ask(_) => self.ask_key(key),
        }
        if let Some(lock) = &self.lock {
            if self.buf.modified != self.lock_says_modified {
                let _ = lock.set_modified(self.buf.modified);
                self.lock_says_modified = self.buf.modified;
            }
        }
    }

    fn edit_key(&mut self, key: Key, cutting: bool) {
        use Named::*;
        let tab = self.opts.tabsize;
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        match key {
            Key::Char(c) => self.type_text(&c.to_string()),
            Key::Byte(b) => self.type_text(&buffer::raw(b)),
            Key::Ctrl('I') => self.type_text("\t"),
            Key::Ctrl('M') => self.enter(),
            Key::Ctrl('H') | Key::Named(Backspace, _) => self.backspace(),
            Key::Ctrl('D') | Key::Named(Delete, _) => self.delete(),
            Key::Ctrl('P') | Key::Named(Up, _) => self.up(),
            Key::Ctrl('N') | Key::Named(Down, _) => self.down(),
            Key::Named(Left, m) if m == ctrl => self.word_left(),
            Key::Named(Right, m) if m == ctrl => self.word_right(),
            Key::Named(Left, _) => self.left(),
            Key::Named(Right, _) => self.right(),
            Key::Ctrl('A') | Key::Named(Home, Mods { ctrl: false, .. }) => {
                self.buf.x = 0;
                self.buf.keep_column(tab);
            }
            Key::Ctrl('E') | Key::Named(End, Mods { ctrl: false, .. }) => {
                self.buf.x = self.buf.line_len(self.buf.line);
                self.buf.keep_column(tab);
            }
            Key::Ctrl('Y') | Key::Named(PageUp, _) => self.page_up(),
            Key::Ctrl('V') | Key::Named(PageDown, _) => self.page_down(),
            Key::Meta('\\') | Key::Named(Home, _) => self.first_line(),
            Key::Meta('/') | Key::Named(End, _) => self.last_line(),
            Key::Ctrl('K') | Key::Named(F(9), _) => self.cut_line(cutting),
            Key::Ctrl('U') | Key::Named(F(10), _) => self.paste(),
            Key::Ctrl('G') | Key::Named(F(1), _) => {
                self.message = None;
                self.mode = Mode::Help(0);
            }
            Key::Ctrl('X') | Key::Named(F(2), _) => self.exit(),
            Key::Ctrl('O') | Key::Named(F(3), _) => self.ask_name(false),
            Key::Ctrl('S') => self.save_now(),
            Key::Ctrl('C') | Key::Named(F(11), _) => {
                let loc = self.location();
                self.say(&loc);
            }
            Key::Ctrl('F') | Key::Ctrl('W') | Key::Named(F(6), _) => self.ask_search(),
            Key::Meta('w') => self.search_again(),
            // The main loop draws the whole screen again.
            Key::Ctrl('L') => {}
            _ => self.error("Not available in this version yet"),
        }
        self.scroll_to_cursor();
    }

    fn type_text(&mut self, s: &str) {
        let last = self.buf.line + 1 == self.buf.lines();
        self.buf.insert(s);
        if last {
            self.magic_line();
        }
        self.buf.keep_column(self.opts.tabsize);
    }

    /// As in nano, the text ends with an empty line (the file with a newline), unless `-L`.
    fn magic_line(&mut self) {
        if !self.opts.nonewlines && !self.buf.ends_with_newline() {
            let end = self.buf.text.len_chars();
            self.buf.text.insert(end, "\n");
        }
    }

    fn enter(&mut self) {
        self.type_text("\n");
    }

    /// Deletes the code point before the cursor (as in nano, a combining mark on its own), or
    /// at the start of a line, goes back to the line above and deletes its line end.
    fn backspace(&mut self) {
        let b = &mut self.buf;
        if b.x > 0 {
            let pos = b.pos();
            // A byte that is not valid UTF-8 goes as a whole, with its mark.
            let byte = b.x >= 2
                && b.text.char(pos - 1) == buffer::MARK
                && buffer::raw_byte(b.text.char(pos - 2)).is_some();
            let n = if byte { 2 } else { 1 };
            b.x -= n;
            b.remove(pos - n, pos);
        } else if b.line > 0 {
            b.line -= 1;
            b.x = b.line_len(b.line);
            self.delete();
        }
        self.buf.keep_column(self.opts.tabsize);
    }

    /// Deletes the character under the cursor with its combining marks, or at the end of a
    /// line, joins the next line to it.
    fn delete(&mut self) {
        let b = &mut self.buf;
        let len = b.line_len(b.line);
        let pos = b.pos();
        if b.x < len {
            let next = next_index(&b.line_text(b.line), b.x);
            b.remove(pos, pos + next - b.x);
        } else if b.line + 1 < b.lines() {
            // Joining a line with text to the empty last line would lose the final newline.
            let last_empty = b.line + 2 == b.lines() && b.line_len(b.line + 1) == 0;
            if !(last_empty && len > 0 && !self.opts.nonewlines) {
                b.remove(pos, pos + 1);
            }
        }
    }

    fn up(&mut self) {
        if self.buf.line > 0 {
            self.buf.goto_line(self.buf.line - 1, self.opts.tabsize);
        }
    }

    fn down(&mut self) {
        if self.buf.line + 1 < self.buf.lines() {
            self.buf.goto_line(self.buf.line + 1, self.opts.tabsize);
        }
    }

    fn left(&mut self) {
        let b = &mut self.buf;
        if b.x > 0 {
            b.x = prev_index(&b.line_text(b.line), b.x);
        } else if b.line > 0 {
            b.line -= 1;
            b.x = b.line_len(b.line);
        }
        self.buf.keep_column(self.opts.tabsize);
    }

    fn right(&mut self) {
        let b = &mut self.buf;
        if b.x < b.line_len(b.line) {
            b.x = next_index(&b.line_text(b.line), b.x);
        } else if b.line + 1 < b.lines() {
            b.line += 1;
            b.x = 0;
        }
        self.buf.keep_column(self.opts.tabsize);
    }

    /// The character at a text position, if any.
    fn char_at(&self, pos: usize) -> Option<char> {
        (pos < self.buf.text.len_chars()).then(|| self.buf.text.char(pos))
    }

    fn is_word(c: Option<char>) -> bool {
        c.is_some_and(char::is_alphanumeric)
    }

    /// To the start of the next word.
    fn word_right(&mut self) {
        let mut pos = self.buf.pos();
        while Self::is_word(self.char_at(pos)) {
            pos += 1;
        }
        while self.char_at(pos).is_some() && !Self::is_word(self.char_at(pos)) {
            pos += 1;
        }
        self.buf.set_pos(pos);
        self.buf.keep_column(self.opts.tabsize);
    }

    /// To the start of this word, or of the one before.
    fn word_left(&mut self) {
        let mut pos = self.buf.pos();
        while pos > 0 && !Self::is_word(self.char_at(pos - 1)) {
            pos -= 1;
        }
        while pos > 0 && Self::is_word(self.char_at(pos - 1)) {
            pos -= 1;
        }
        self.buf.set_pos(pos);
        self.buf.keep_column(self.opts.tabsize);
    }

    /// A page is the text rows less two, as in nano: two lines stay on the screen.
    fn page(&self) -> usize {
        self.edit_rows().saturating_sub(2).max(1)
    }

    fn page_up(&mut self) {
        let page = self.page();
        self.buf.top = self.buf.top.saturating_sub(page);
        self.buf
            .goto_line(self.buf.line.saturating_sub(page), self.opts.tabsize);
    }

    /// Down a page, the text with the cursor; at the end, the cursor goes to the last line.
    fn page_down(&mut self) {
        let page = self.page();
        let last = self.buf.lines() - 1;
        if self.buf.line + page > last {
            self.buf.goto_line(last, self.opts.tabsize);
        } else {
            self.buf.top += page;
            self.buf.goto_line(self.buf.line + page, self.opts.tabsize);
        }
    }

    fn first_line(&mut self) {
        self.buf.line = 0;
        self.buf.x = 0;
        self.buf.keep_column(self.opts.tabsize);
    }

    fn last_line(&mut self) {
        self.buf.line = self.buf.lines() - 1;
        self.buf.x = self.buf.line_len(self.buf.line);
        self.buf.keep_column(self.opts.tabsize);
    }

    /// Scrolls just enough to show the cursor's line.
    fn scroll_to_cursor(&mut self) {
        let rows = self.edit_rows();
        let b = &mut self.buf;
        if b.line < b.top {
            b.top = b.line;
        } else if b.line >= b.top + rows {
            b.top = b.line + 1 - rows;
        }
    }

    /// Puts the cursor's line in the middle of the screen if it is not on it.
    fn center(&mut self) {
        let rows = self.edit_rows();
        let b = &mut self.buf;
        if b.line < b.top || b.line >= b.top + rows {
            b.top = b.line.saturating_sub(rows / 2);
        }
    }

    fn cut_line(&mut self, cutting: bool) {
        let b = &mut self.buf;
        let from = b.text.line_to_char(b.line);
        let to = if b.line + 1 < b.lines() {
            b.text.line_to_char(b.line + 1)
        } else {
            b.text.len_chars()
        };
        if from == to {
            return self.say("Nothing was cut");
        }
        if !cutting {
            self.cut.clear();
        }
        self.cut.push_str(&String::from(b.text.slice(from..to)));
        b.remove(from, to);
        b.x = 0;
        b.keep_column(self.opts.tabsize);
        self.cutting = true;
        self.message = None;
    }

    fn paste(&mut self) {
        if self.cut.is_empty() {
            return self.error("Cutbuffer is empty");
        }
        let text = self.cut.clone();
        self.type_text(&text);
        self.message = None;
    }

    /// The cursor's position as ^C shows it.
    pub fn location(&self) -> String {
        let b = &self.buf;
        let tab = self.opts.tabsize;
        let percent = |n: usize, of: usize| (100 * n).checked_div(of).unwrap_or(0);
        let (line, lines) = (b.line + 1, b.lines());
        let col = b.column(tab) + 1;
        let cols = buffer::column_of(&b.line_text(b.line), b.line_len(b.line), tab) + 1;
        let (ch, chars) = (b.pos(), b.text.len_chars());
        let w = |n: usize| n.to_string().len().max(2);
        format!(
            "line {line:>lw$}/{lines} ({:2}%), col {col:>2}/{cols:>2} ({:3}%), char {ch:>cw$}/{chars} ({:2}%)",
            percent(line, lines),
            percent(col, cols),
            percent(ch, chars),
            lw = w(lines),
            cw = w(chars),
        )
    }

    fn exit(&mut self) {
        if self.buf.modified {
            self.mode = Mode::Ask(Question::SaveOnExit);
        } else {
            self.done = true;
        }
    }

    fn ask_key(&mut self, key: Key) {
        let Mode::Ask(question) = std::mem::replace(&mut self.mode, Mode::Edit) else {
            return;
        };
        let yes = match key {
            Key::Char('y' | 'Y') => true,
            Key::Char('n' | 'N') => false,
            // As nano: cancelling the question about a locked file leaves at once, and the one
            // about a file changed on disk goes back to its name.
            Key::Ctrl('C') if matches!(question, Question::Locked(_)) => {
                self.done = true;
                return;
            }
            Key::Ctrl('C') => {
                if let Question::Changed(name, exit) = question {
                    self.ask_name(exit);
                    if let Mode::Prompt(p) = &mut self.mode {
                        p.x = name.chars().count();
                        p.answer = name;
                    }
                    return;
                }
                return self.cancel();
            }
            _ => {
                self.mode = Mode::Ask(question);
                return;
            }
        };
        match (question, yes) {
            (Question::SaveOnExit, true) => self.ask_name(true),
            (Question::SaveOnExit, false) => self.done = true,
            (Question::OtherName(name, exit), true) => self.save_as(name, exit),
            (Question::OtherName(name, exit) | Question::Overwrite(name, exit), false) => {
                self.ask_name(exit);
                if let Mode::Prompt(p) = &mut self.mode {
                    p.x = name.chars().count();
                    p.answer = name;
                }
            }
            (Question::Overwrite(name, exit) | Question::Changed(name, exit), true) => {
                // Over the file as it is now.
                let expected = match files::stat(Path::new(&name)) {
                    Ok(Some(meta)) => Expected::File(meta),
                    _ => Expected::Absent,
                };
                self.save(name, expected, exit);
            }
            (Question::Changed(..), false) => {}
            (Question::Locked(_), true) => self.take_lock(true),
            // As nano: the file is not opened; a new buffer is.
            (Question::Locked(_), false) => {
                self.buf = Buffer::new("", Format::Unix);
                self.name.clear();
                self.meta = None;
                self.say("For basic help, type Ctrl+G.");
            }
        }
    }

    fn cancel(&mut self) {
        self.mode = Mode::Edit;
        self.say("Cancelled");
    }

    fn write_label(&self) -> String {
        let format = match self.buf.format {
            Format::Unix => "",
            Format::Dos => " [DOS Format]",
            Format::Mac => " [Mac Format]",
        };
        format!("Write to File{format}:")
    }

    fn ask_name(&mut self, exit: bool) {
        self.message = None;
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Write { exit },
            label: self.write_label(),
            answer: self.name.clone(),
            x: self.name.chars().count(),
        });
    }

    fn save_now(&mut self) {
        if self.name.is_empty() {
            self.ask_name(false);
        } else {
            self.save_as(self.name.clone(), false);
        }
    }

    /// Writes the text to `name` (then exits, with `exit`), asking first, as nano does, when
    /// another file has that name or the file changed on disk.
    fn save_as(&mut self, name: String, exit: bool) {
        let now = files::stat(Path::new(&name));
        if name != self.name {
            match now {
                Ok(Some(_)) => self.mode = Mode::Ask(Question::Overwrite(name, exit)),
                _ => self.save(name, Expected::Absent, exit),
            }
            return;
        }
        match (self.meta, now) {
            (Some(meta), Ok(Some(now))) if now.changed_since(&meta) => self.changed(name, exit),
            (Some(meta), Ok(Some(_))) => self.save(name, Expected::File(meta), exit),
            // Gone from the disk, or never there: written as a new file.
            _ => self.save(name, Expected::Absent, exit),
        }
    }

    /// The file changed on disk: nano warns for a moment, then asks whether to save anyway.
    fn changed(&mut self, name: String, exit: bool) {
        self.error("File on disk has changed");
        self.warning = Some(Question::Changed(name, exit));
    }

    /// After the moment of a warning: its question.
    pub fn warned(&mut self) {
        if let Some(question) = self.warning.take() {
            self.message = None;
            self.mode = Mode::Ask(question);
        }
    }

    /// Writes the text to `name`, which must still be the `expected` file.
    fn save(&mut self, name: String, expected: Expected, exit: bool) {
        self.magic_line();
        let bytes = self.buf.to_bytes();
        match files::save(Path::new(&name), &bytes, expected, &SaveOptions::default()) {
            Ok(saved) => {
                self.meta = Some(saved.meta);
                self.buf.modified = false;
                if name != self.name {
                    // The lock goes with the name.
                    self.lock = None;
                    self.name = name;
                    if let Ok(Locking::Locked(lock)) = files::lock(Path::new(&self.name), false) {
                        self.lock = Some(lock);
                        self.lock_says_modified = false;
                    }
                }
                let lines = self.buf.file_lines();
                self.say(&format!("Wrote {}", plural(lines, "line")));
                self.done = exit;
            }
            Err(SaveError::Changed) => self.changed(name, exit),
            Err(SaveError::Backup(e)) => {
                self.error(&format!("Error writing backup file: {}", error_text(&e)));
            }
            Err(SaveError::Write { error, copy }) => {
                let mut text = format!("Error writing {name}: {}", error_text(&error));
                if let Some(copy) = copy {
                    text.push_str(&format!(" (the old contents are in {})", copy.display()));
                }
                self.error(&text);
            }
        }
    }

    fn ask_search(&mut self) {
        let label = match &self.last_search {
            Some(s) => format!("Search [{s}]:"),
            None => "Search:".to_string(),
        };
        self.message = None;
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Search,
            label,
            answer: String::new(),
            x: 0,
        });
    }

    fn prompt_key(&mut self, key: Key) {
        let Mode::Prompt(p) = &mut self.mode else {
            return;
        };
        let len = p.answer.chars().count();
        let byte = |s: &str, x: usize| s.char_indices().nth(x).map_or(s.len(), |(i, _)| i);
        match key {
            Key::Char(c) => {
                p.answer.insert(byte(&p.answer, p.x), c);
                p.x += 1;
            }
            Key::Named(Named::Left, _) => p.x = p.x.saturating_sub(1),
            Key::Named(Named::Right, _) => p.x = (p.x + 1).min(len),
            Key::Ctrl('A') | Key::Named(Named::Home, _) => p.x = 0,
            Key::Ctrl('E') | Key::Named(Named::End, _) => p.x = len,
            Key::Ctrl('H') | Key::Named(Named::Backspace, _) if p.x > 0 => {
                p.x -= 1;
                p.answer.remove(byte(&p.answer, p.x));
            }
            Key::Ctrl('D') | Key::Named(Named::Delete, _) if p.x < len => {
                p.answer.remove(byte(&p.answer, p.x));
            }
            Key::Ctrl('C') => self.cancel(),
            Key::Ctrl('M') => {
                let kind = p.kind;
                let answer = std::mem::take(&mut p.answer);
                self.mode = Mode::Edit;
                self.accept(kind, answer);
            }
            _ => {}
        }
    }

    fn accept(&mut self, kind: PromptKind, answer: String) {
        match kind {
            PromptKind::Write { exit } => {
                if answer.is_empty() {
                    return self.cancel();
                }
                if !self.name.is_empty() && answer != self.name {
                    self.mode = Mode::Ask(Question::OtherName(answer, exit));
                    return;
                }
                self.save_as(answer, exit);
            }
            PromptKind::Search => {
                let needle = if answer.is_empty() {
                    match self.last_search.clone() {
                        Some(s) => s,
                        None => return self.cancel(),
                    }
                } else {
                    answer
                };
                self.last_search = Some(needle.clone());
                self.search(&needle);
            }
        }
    }

    fn search_again(&mut self) {
        match self.last_search.clone() {
            Some(s) => self.search(&s),
            None => self.error("No current search pattern"),
        }
    }

    /// Searches forward from the cursor, ignoring case, wrapping around at the end.
    fn search(&mut self, needle: &str) {
        let lower = |s: &str| -> Vec<char> {
            s.chars()
                .map(|c| c.to_lowercase().next().unwrap_or(c))
                .collect()
        };
        let text = lower(&String::from(&self.buf.text));
        let pat = lower(needle);
        let start = self.buf.pos();
        let find = |range: std::ops::Range<usize>| {
            range
                .into_iter()
                .find(|&i| text.get(i..i + pat.len()) == Some(&pat[..]))
        };
        let hit = find(start + 1..text.len())
            .map(|i| (i, false))
            .or_else(|| find(0..start + 1).map(|i| (i, true)));
        match hit {
            None => self.error(&format!("\"{needle}\" not found")),
            Some((i, wrapped)) => {
                self.buf.set_pos(i);
                self.buf.keep_column(self.opts.tabsize);
                let line_start = self.buf.text.line_to_char(self.buf.line);
                let end = (i + pat.len()).min(
                    self.buf.text.line_to_char(self.buf.line) + self.buf.line_len(self.buf.line),
                );
                self.spot = Some((self.buf.line, i - line_start, end - line_start));
                if i == start {
                    self.say("This is the only occurrence");
                } else if wrapped {
                    self.say("Search Wrapped");
                }
                self.center();
            }
        }
    }

    fn help_key(&mut self, key: Key) {
        let Mode::Help(top) = self.mode else { return };
        let rows = self.edit_rows().saturating_sub(1);
        let last = self.help_lines().len().saturating_sub(rows);
        let page = rows.saturating_sub(2).max(1);
        let top = match key {
            Key::Ctrl('X') | Key::Ctrl('G') | Key::Named(Named::F(1) | Named::F(2), _) => {
                self.mode = Mode::Edit;
                return;
            }
            Key::Ctrl('P') | Key::Named(Named::Up, _) => top.saturating_sub(1),
            Key::Ctrl('N') | Key::Named(Named::Down, _) => top + 1,
            Key::Ctrl('Y') | Key::Named(Named::PageUp, _) => top.saturating_sub(page),
            Key::Ctrl('V') | Key::Named(Named::PageDown, _) => top + page,
            Key::Meta('\\') => 0,
            Key::Meta('/') => last,
            _ => top,
        };
        self.mode = Mode::Help(top.min(last));
    }

    /// The help text in lines as wide as the screen.
    pub fn help_lines(&self) -> Vec<String> {
        let mut lines = vec![String::new()];
        let width = self.cols.saturating_sub(1).max(10);
        for para in HELP_INTRO.split('\n') {
            let mut line = String::new();
            for word in para.split(' ').filter(|w| !w.is_empty()) {
                if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                    lines.push(std::mem::take(&mut line));
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
            lines.push(line);
        }
        lines.push(String::new());
        for (key, alt, text) in HELP_KEYS {
            if key.is_empty() && alt.is_empty() {
                lines.push(String::new());
                continue;
            }
            let alt = if alt.is_empty() {
                String::new()
            } else {
                format!("({alt})")
            };
            lines.push(format!("{key:<7}{alt:<10}{text}"));
        }
        lines
    }
}
