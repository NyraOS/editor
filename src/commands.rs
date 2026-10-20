// SPDX-License-Identifier: MIT
//! The editor's state and its commands: what each key does.

use crate::buffer::{self, Buffer, Format, Kind, Mark, next_index, prev_index};
use crate::complete::{self, Completion};
use crate::config::Options;
use crate::files::{self, Expected, Locking, Meta, SaveError, SaveOptions};
use crate::keys::{Key, Mods, Named};
use crate::search::{self, Pattern};
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
    /// Writing under a name another file has; Yes writes over that file only, as it was when
    /// asked.
    Overwrite(String, bool, Meta),
    /// The file changed on disk since it was read or written; Yes writes over it only as it was
    /// when asked.
    Changed(String, bool, Expected),
    /// Another editor holds the lock file.
    Locked(files::LockInfo),
}

impl Question {
    /// The question as nano words it, with the space after it where nano has one.
    pub fn text(&self, name: &str) -> String {
        match self {
            Question::SaveOnExit => "Save modified buffer? ".into(),
            Question::OtherName(..) => "Save file under DIFFERENT NAME? ".into(),
            Question::Overwrite(file, ..) => format!("File \"{file}\" exists; OVERWRITE? "),
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
    /// What the prompt asks, without its colon.
    pub label: String,
    pub answer: String,
    /// The cursor in the answer, in characters.
    pub x: usize,
    /// For a search: the place in the search history that the answer shows (the history's length
    /// for the text typed), and that text while an older search is shown.
    older: usize,
    typed: String,
}

impl Prompt {
    fn new(kind: PromptKind, label: String, answer: String, older: usize) -> Self {
        Self {
            kind,
            label,
            x: answer.chars().count(),
            answer,
            older,
            typed: String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum PromptKind {
    /// The file name to write to; then exit if it was for ^X.
    Write { exit: bool },
    /// The text to search for.
    Search { backwards: bool },
    /// The line and column to go to.
    Goto,
}

/// What the last key did to the cut buffer: the next cut, or copy, of a line adds to it when
/// it is the same.
#[derive(Clone, Copy, PartialEq)]
enum Chain {
    None,
    Cut,
    Copy,
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
    /// The text cut or copied last, and what the last key did to it.
    cut: String,
    chain: Chain,
    /// The completions offered by the last key, when it was ^].
    completion: Option<Completion>,
    /// The searches so far, the oldest first, and whether case matters (kept from search to
    /// search, as in nano).
    history: Vec<String>,
    case_sensitive: bool,
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

const GOTO_MENU: &[(&str, &str)] = &[
    ("^G", "Help"),
    ("^C", "Cancel"),
    ("^W", "Begin of Paragr."),
    ("^O", "End of Paragraph"),
    ("^Y", "First Line"),
    ("^V", "Last Line"),
    ("^T", "Go To Text"),
];

/// The help lines for a mode.
pub fn menu(mode: &Mode) -> &'static [(&'static str, &'static str)] {
    match mode {
        Mode::Edit | Mode::Ask(_) => MAIN_MENU,
        Mode::Help(_) => HELP_MENU,
        Mode::Prompt(p) => match p.kind {
            PromptKind::Write { .. } => WRITE_MENU,
            PromptKind::Search { .. } => SEARCH_MENU,
            PromptKind::Goto => GOTO_MENU,
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

/// A cursor key with Shift selects as it moves: the key without Shift, and whether it had it.
fn unshift(key: Key) -> (Key, bool) {
    use Named::*;
    match key {
        Key::Named(n @ (Up | Down | Left | Right | Home | End | PageUp | PageDown), mods)
            if mods.shift && !mods.alt =>
        {
            (
                Key::Named(
                    n,
                    Mods {
                        shift: false,
                        ..mods
                    },
                ),
                true,
            )
        }
        other => (other, false),
    }
}

/// Whether a key that is not shifted takes a soft mark away: the keys that move the cursor and
/// the ones that put text in.
fn ends_soft_mark(key: &Key) -> bool {
    use Named::*;
    matches!(
        key,
        Key::Char(_)
            | Key::Byte(_)
            | Key::Paste(_)
            | Key::Ctrl('I' | 'M' | 'H' | 'A' | 'E' | 'P' | 'N' | 'Y' | 'V' | ']')
            | Key::Meta('\\' | '/')
            | Key::Named(
                Up | Down | Left | Right | Home | End | PageUp | PageDown | Backspace,
                _
            )
    )
}

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
            chain: Chain::None,
            completion: None,
            history: Vec::new(),
            case_sensitive: false,
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
            ed.goto(Some(line), Some(col));
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

    /// Puts the cursor on a line and a screen column, 1-based; negative numbers count from the end
    /// (of the text, or of the line). What is left out stays: the line, or the column the cursor
    /// keeps when it moves up and down. Out of range is the nearest line or column.
    fn goto(&mut self, line: Option<i64>, col: Option<i64>) {
        let tab = self.opts.tabsize;
        self.spot = None;
        let lines = self.buf.lines() as i64;
        let line = line.unwrap_or(self.buf.line as i64 + 1);
        let line = if line < 0 {
            lines.saturating_add(line) + 1
        } else {
            line
        };
        self.buf.line = (line.clamp(1, lines) - 1) as usize;
        let text = self.buf.line_text(self.buf.line);
        let col = col.unwrap_or(self.buf.want as i64 + 1);
        let col = if col < 0 {
            let width = buffer::column_of(&text, text.chars().count(), tab) as i64;
            (width + 2).saturating_add(col)
        } else {
            col
        };
        let col = col.max(1);
        self.buf.x = buffer::index_at(&text, col as usize - 1, tab);
        self.buf.want = col as usize - 1;
        let rows = self.edit_rows();
        self.buf.top = self.buf.line.saturating_sub(rows / 2);
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
        // Text pasted into a question or a prompt goes in as if it were typed.
        if let (Key::Paste(text), false) = (&key, matches!(self.mode, Mode::Edit)) {
            for c in text.chars() {
                match c {
                    '\n' => {}
                    '\t' => self.key(Key::Ctrl('I')),
                    c => self.key(Key::Char(c)),
                }
            }
            return;
        }
        // nano counts the bytes of a typed character as keys.
        let n = match &key {
            Key::Char(c) => c.len_utf8() as u8,
            Key::Paste(text) => text.len().min(usize::from(u8::MAX)) as u8,
            Key::PasteStart | Key::PasteEnd { .. } => 0,
            _ => 1,
        };
        if let Some(m) = &mut self.message {
            if m.keys_left <= n || self.opts.constantshow {
                self.message = None;
            } else {
                m.keys_left -= n;
            }
        }
        // The match stays marked while the next search is asked for.
        let asks_search = matches!(
            key,
            Key::Ctrl('W' | 'F' | 'Q' | 'B') | Key::Named(Named::F(6), _)
        );
        if !(matches!(self.mode, Mode::Prompt(_)) || asks_search && matches!(self.mode, Mode::Edit))
        {
            self.spot = None;
        }
        let chain = std::mem::replace(&mut self.chain, Chain::None);
        let completion = self.completion.take();
        match &mut self.mode {
            Mode::Edit => self.edit_key(key, chain, completion),
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

    fn edit_key(&mut self, key: Key, chain: Chain, completion: Option<Completion>) {
        use Named::*;
        let tab = self.opts.tabsize;
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        let (key, shifted) = unshift(key);
        if shifted && self.buf.mark.is_none() {
            let at = self.buf.pos();
            self.buf.mark = Some(Mark { at, soft: true });
        }
        if !shifted && self.buf.mark.is_some_and(|m| m.soft) && ends_soft_mark(&key) {
            self.buf.mark = None;
        }
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
            Key::Ctrl('K') | Key::Named(F(9), _) => self.cut(chain),
            Key::Meta('t') => self.cut_to_end(),
            Key::Meta('6' | '^') => self.copy(chain),
            Key::Meta('a') | Key::Ctrl('^') => self.toggle_mark(),
            Key::Ctrl('U') | Key::Named(F(10), _) => self.paste(),
            Key::Ctrl(']') => self.complete_word(completion),
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
            Key::Ctrl('F') | Key::Ctrl('W') | Key::Named(F(6), _) => {
                self.ask_search(false, String::new());
            }
            Key::Ctrl('Q') | Key::Ctrl('B') => self.ask_search(true, String::new()),
            Key::Meta('w') | Key::Meta('f') => self.search(false),
            Key::Meta('q') | Key::Meta('b') => self.search(true),
            Key::Ctrl('_') | Key::Meta('g') => self.ask_goto(String::new()),
            // The main loop draws the whole screen again.
            Key::Ctrl('L') => {}
            Key::PasteStart => {
                self.buf.end_group();
                self.message = None;
            }
            Key::Paste(text) => {
                self.insert_text(&text, Kind::Paste);
                // As nano: a paste taller than the screen puts the cursor's line in the middle.
                if text.matches('\n').count() >= self.edit_rows() {
                    self.buf.top = self.buf.line.saturating_sub(self.edit_rows() / 2);
                }
            }
            Key::PasteEnd { flawed } => {
                self.buf.end_group();
                if flawed {
                    self.error("Flawed paste");
                }
            }
            Key::Meta('u') => self.undo(),
            Key::Meta('e') => self.redo(),
            _ => self.error("Not available in this version yet"),
        }
        self.scroll_to_cursor();
    }

    /// A paste is held across reads, `bytes` of it so far: the message goes when they are as many
    /// as the keys it had left. The paste is counted again when it arrives.
    pub fn message_worn_by(&mut self, bytes: usize) {
        if self
            .message
            .as_ref()
            .is_some_and(|m| usize::from(m.keys_left) <= bytes)
        {
            self.message = None;
        }
    }

    fn type_text(&mut self, s: &str) {
        self.insert_text(s, if s == "\n" { Kind::Enter } else { Kind::Add });
    }

    fn insert_text(&mut self, s: &str, kind: Kind) {
        let last = self.buf.line + 1 == self.buf.lines();
        let at = self.buf.pos();
        self.buf.insert(s);
        self.buf.record(kind, at, String::new(), s.to_string(), at);
        if last && self.magic_line() {
            self.buf.record_final_newline();
        }
        self.buf.keep_column(self.opts.tabsize);
    }

    /// As in nano, the text ends with an empty line (the file with a newline), unless `-L`.
    /// Returns whether it added the newline.
    fn magic_line(&mut self) -> bool {
        let add = !self.opts.nonewlines && !self.buf.ends_with_newline();
        if add {
            let end = self.buf.text.len_chars();
            self.buf.text.insert(end, "\n");
        }
        add
    }

    fn enter(&mut self) {
        self.type_text("\n");
    }

    fn undo(&mut self) {
        match self.buf.undo() {
            Some(kind) => self.say(&format!("Undid {}", kind.name())),
            None => self.error("Nothing to undo"),
        }
        self.center();
        self.buf.keep_column(self.opts.tabsize);
    }

    fn redo(&mut self) {
        match self.buf.redo() {
            Some(kind) => self.say(&format!("Redid {}", kind.name())),
            None => self.error("Nothing to redo"),
        }
        self.center();
        self.buf.keep_column(self.opts.tabsize);
    }

    /// ^] completes the word fragment before the cursor with a word of the text; again in a
    /// row, it takes the next one, and after the last it puts the fragment back.
    fn complete_word(&mut self, completion: Option<Completion>) {
        // There is no `wordchars` option yet: the default is none.
        let wordchars = "";
        let first = completion.is_none();
        let mut completion = match completion {
            Some(c) => {
                // Takes back the last ending: the one edit made, and nothing to redo of it.
                self.buf.undo();
                c
            }
            None => {
                let text = &self.buf.text;
                let cursor = self.buf.pos();
                let start = complete::fragment_start(text, cursor, wordchars);
                if start == cursor {
                    self.error("No word fragment");
                    return;
                }
                Completion::new(String::from(text.slice(start..cursor)), start)
            }
        };
        let Some(ending) = completion.next_ending(&self.buf.text, wordchars) else {
            self.buf.keep_column(self.opts.tabsize);
            self.error(if first {
                "No matches"
            } else {
                "No further matches"
            });
            return;
        };
        // As in nano, a message still shown goes.
        self.message = None;
        self.buf.end_group();
        self.insert_text(&ending, Kind::Add);
        self.completion = Some(completion);
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
            b.erase(Kind::Back, pos - n, pos, pos);
        } else if b.line > 0 {
            let before = b.pos();
            b.line -= 1;
            b.x = b.line_len(b.line);
            self.delete_at(before);
        }
        self.buf.keep_column(self.opts.tabsize);
    }

    /// Deletes the character under the cursor with its combining marks, or at the end of a
    /// line, joins the next line to it.
    fn delete(&mut self) {
        self.delete_at(self.buf.pos());
    }

    /// `delete`, for the cursor that was at `before` (it is moved when Backspace joins lines).
    fn delete_at(&mut self, before: usize) {
        let b = &mut self.buf;
        let len = b.line_len(b.line);
        let pos = b.pos();
        if b.x < len {
            let next = next_index(&b.line_text(b.line), b.x);
            b.erase(Kind::Del, pos, pos + next - b.x, before);
        } else if b.line + 1 < b.lines() {
            // Joining a line with text to the empty last line would lose the final newline.
            let last_empty = b.line + 2 == b.lines() && b.line_len(b.line + 1) == 0;
            if !(last_empty && len > 0 && !self.opts.nonewlines) {
                b.erase(Kind::Join, pos, pos + 1, before);
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

    /// ^K: the selection, or else the line; cutting lines in a row adds them to the cut buffer.
    fn cut(&mut self, chain: Chain) {
        if let Some((from, to)) = self.buf.selection() {
            self.buf.mark = None;
            if from == to {
                return self.say("Nothing was cut");
            }
            self.cut = String::from(self.buf.text.slice(from..to));
            self.erase_cut(from, to);
            return;
        }
        let (from, to) = self.line_range();
        if from == to {
            return self.say("Nothing was cut");
        }
        if chain != Chain::Cut {
            self.cut.clear();
            // Only cuts with nothing pressed between them make one step, as in nano.
            self.buf.end_group();
        }
        self.cut
            .push_str(&String::from(self.buf.text.slice(from..to)));
        let b = &mut self.buf;
        let before = b.pos();
        b.x = 0;
        b.erase(Kind::Cut, from, to, before);
        b.keep_column(self.opts.tabsize);
        self.chain = Chain::Cut;
        self.message = None;
    }

    /// M-T: everything from the cursor to the end of the text.
    fn cut_to_end(&mut self) {
        let (from, to) = (self.buf.pos(), self.buf.text.len_chars());
        if from == to {
            return self.say("Nothing was cut");
        }
        self.cut = String::from(self.buf.text.slice(from..to));
        self.erase_cut(from, to);
    }

    /// Deletes the text between `from` and `to` as one step of undo of its own, with the cursor
    /// left at `from`.
    fn erase_cut(&mut self, from: usize, to: usize) {
        let b = &mut self.buf;
        let before = b.pos();
        b.end_group();
        b.set_pos(from);
        b.erase(Kind::Cut, from, to, before);
        b.end_group();
        b.keep_column(self.opts.tabsize);
        self.message = None;
    }

    /// The first character of the cursor's line, and where the next line starts (the end of the
    /// text on the last line).
    fn line_range(&self) -> (usize, usize) {
        let b = &self.buf;
        let from = b.text.line_to_char(b.line);
        let to = if b.line + 1 < b.lines() {
            b.text.line_to_char(b.line + 1)
        } else {
            b.text.len_chars()
        };
        (from, to)
    }

    /// M-6: copies the selection, or else the line and goes to the next one; copying lines in a
    /// row adds them to the cut buffer.
    fn copy(&mut self, chain: Chain) {
        let mut next = Chain::Copy;
        let (from, to) = match self.buf.selection() {
            Some(range) => {
                next = Chain::None;
                self.buf.mark = None;
                range
            }
            None => self.line_range(),
        };
        if chain != Chain::Copy || next == Chain::None {
            self.cut.clear();
        }
        if from == to {
            return self.say("Copied nothing");
        }
        self.cut
            .push_str(&String::from(self.buf.text.slice(from..to)));
        self.chain = next;
        self.message = None;
        let b = &mut self.buf;
        if next == Chain::Copy {
            if b.line + 1 < b.lines() {
                b.line += 1;
                b.x = 0;
            } else {
                b.x = b.line_len(b.line);
            }
            b.keep_column(self.opts.tabsize);
        }
    }

    /// M-A: sets the mark, or takes it away.
    fn toggle_mark(&mut self) {
        if self.buf.mark.take().is_some() {
            self.say("Mark Unset");
        } else {
            let at = self.buf.pos();
            self.buf.mark = Some(Mark { at, soft: false });
            self.say("Mark Set");
        }
    }

    fn paste(&mut self) {
        if self.cut.is_empty() {
            return self.error("Cutbuffer is empty");
        }
        if self.buf.mark.is_some_and(|m| m.soft) {
            self.buf.mark = None;
        }
        let text = self.cut.clone();
        self.buf.end_group();
        self.insert_text(&text, Kind::Paste);
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
        let (ch, chars) = (b.chars_before(b.pos()), b.chars_before(b.text.len_chars()));
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
                if let Question::Changed(name, exit, _) = question {
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
            (Question::OtherName(name, exit) | Question::Overwrite(name, exit, _), false) => {
                self.ask_name(exit);
                if let Mode::Prompt(p) = &mut self.mode {
                    p.x = name.chars().count();
                    p.answer = name;
                }
            }
            // Another change since the question makes the save fail, and it is asked again.
            (Question::Overwrite(name, exit, meta), true) => {
                self.save(name, Expected::File(meta), exit);
            }
            (Question::Changed(name, exit, expected), true) => self.save(name, expected, exit),
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
        self.spot = None;
        self.say("Cancelled");
    }

    fn write_label(&self) -> String {
        let format = match self.buf.format {
            Format::Unix => "",
            Format::Dos => " [DOS Format]",
            Format::Mac => " [Mac Format]",
        };
        format!("Write to File{format}")
    }

    fn ask_name(&mut self, exit: bool) {
        self.message = None;
        self.mode = Mode::Prompt(Prompt::new(
            PromptKind::Write { exit },
            self.write_label(),
            self.name.clone(),
            0,
        ));
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
                Ok(Some(meta)) => self.mode = Mode::Ask(Question::Overwrite(name, exit, meta)),
                _ => self.save(name, Expected::Absent, exit),
            }
            return;
        }
        match (self.meta, now) {
            (Some(meta), Ok(Some(now))) if now.changed_since(&meta) => {
                self.changed(name, exit, Some(now));
            }
            (Some(meta), Ok(Some(_))) => self.save(name, Expected::File(meta), exit),
            // Gone from the disk, or never there: written as a new file.
            _ => self.save(name, Expected::Absent, exit),
        }
    }

    /// The file at the name is not the one read or written last (`now`; None when there is
    /// none): nano warns for a moment, then asks whether to save anyway. A file of another user
    /// than that one's (or than this editor's, for a new file) is refused: saving over it would
    /// write into whatever that user put there; another name can still be written.
    fn changed(&mut self, name: String, exit: bool, now: Option<Meta>) {
        let owner = self
            .meta
            .map_or_else(|| rustix::process::geteuid().as_raw(), |m| m.uid);
        if now.is_some_and(|now| now.uid != owner) {
            return self.error("File on disk was replaced by another user's file; not saved");
        }
        self.error("File on disk has changed");
        let expected = now.map_or(Expected::Absent, Expected::File);
        self.warning = Some(Question::Changed(name, exit, expected));
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
                self.buf.mark_saved();
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
            Err(SaveError::Changed) => {
                let now = files::stat(Path::new(&name)).ok().flatten();
                match now {
                    // A file appeared under a new name.
                    Some(meta) if name != self.name => {
                        self.mode = Mode::Ask(Question::Overwrite(name, exit, meta));
                    }
                    _ => self.changed(name, exit, now),
                }
            }
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

    /// The text of the last search.
    fn last_search(&self) -> Option<&String> {
        self.history.last()
    }

    /// The prompt for a search, with the answer typed so far.
    fn ask_search(&mut self, backwards: bool, answer: String) {
        self.message = None;
        let mut prompt = Prompt::new(
            PromptKind::Search { backwards },
            String::new(),
            answer,
            self.history.len(),
        );
        prompt.label = self.search_label(backwards);
        self.mode = Mode::Prompt(prompt);
    }

    /// "Search", the toggles that are on, and the last search (cut when long) in brackets.
    fn search_label(&self, backwards: bool) -> String {
        let mut label = "Search".to_string();
        if self.case_sensitive {
            label.push_str(" [Case Sensitive]");
        }
        if backwards {
            label.push_str(" [Backwards]");
        }
        if let Some(last) = self.last_search() {
            label.push_str(&format!(" [{}]", search::shorten(last, self.cols / 3)));
        }
        label
    }

    fn ask_goto(&mut self, answer: String) {
        self.message = None;
        let label = "Enter line number, column number".to_string();
        self.mode = Mode::Prompt(Prompt::new(PromptKind::Goto, label, answer, 0));
    }

    fn prompt_key(&mut self, key: Key) {
        let Mode::Prompt(p) = &mut self.mode else {
            return;
        };
        let len = p.answer.chars().count();
        let byte = |s: &str, x: usize| s.char_indices().nth(x).map_or(s.len(), |(i, _)| i);
        match (key, p.kind) {
            (Key::Char(c), _) => {
                p.answer.insert(byte(&p.answer, p.x), c);
                p.x += 1;
            }
            (Key::Named(Named::Left, _), _) => p.x = p.x.saturating_sub(1),
            (Key::Named(Named::Right, _), _) => p.x = (p.x + 1).min(len),
            (Key::Ctrl('A') | Key::Named(Named::Home, _), _) => p.x = 0,
            (Key::Ctrl('E') | Key::Named(Named::End, _), _) => p.x = len,
            (Key::Ctrl('H') | Key::Named(Named::Backspace, _), _) if p.x > 0 => {
                p.x -= 1;
                p.answer.remove(byte(&p.answer, p.x));
            }
            (Key::Ctrl('D') | Key::Named(Named::Delete, _), _) if p.x < len => {
                p.answer.remove(byte(&p.answer, p.x));
            }
            (Key::Ctrl('C'), _) => self.cancel(),
            (Key::Ctrl('M'), kind) => {
                let answer = std::mem::take(&mut p.answer);
                self.mode = Mode::Edit;
                self.accept(kind, answer);
            }
            (Key::Meta('c'), PromptKind::Search { backwards }) => {
                self.case_sensitive = !self.case_sensitive;
                self.ask_search_again(backwards);
            }
            (Key::Meta('b'), PromptKind::Search { backwards }) => self.ask_search_again(!backwards),
            (Key::Ctrl('P') | Key::Named(Named::Up, _), PromptKind::Search { .. }) => {
                self.older_search();
            }
            (Key::Ctrl('N') | Key::Named(Named::Down, _), PromptKind::Search { .. }) => {
                self.newer_search();
            }
            (Key::Ctrl('T'), PromptKind::Search { .. }) => {
                let answer = std::mem::take(&mut p.answer);
                self.ask_goto(answer);
            }
            (Key::Ctrl('T'), PromptKind::Goto) => {
                let answer = std::mem::take(&mut p.answer);
                self.ask_search(false, answer);
            }
            (Key::Ctrl('Y'), PromptKind::Goto) => {
                self.mode = Mode::Edit;
                self.first_line();
                self.center();
            }
            (Key::Ctrl('V'), PromptKind::Goto) => {
                self.mode = Mode::Edit;
                self.last_line();
                self.center();
            }
            _ => {}
        }
    }

    /// The search prompt again after a toggle, with the same answer and history place.
    fn ask_search_again(&mut self, backwards: bool) {
        let Mode::Prompt(old) = std::mem::replace(&mut self.mode, Mode::Edit) else {
            return;
        };
        let label = self.search_label(backwards);
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Search { backwards },
            label,
            ..old
        });
    }

    /// An older search in the answer; the text typed is kept for coming back to it.
    fn older_search(&mut self) {
        let Mode::Prompt(p) = &mut self.mode else {
            return;
        };
        if p.older == 0 {
            return;
        }
        if p.older == self.history.len() {
            p.typed = std::mem::take(&mut p.answer);
        }
        p.older -= 1;
        p.answer = self.history[p.older].clone();
        p.x = p.answer.chars().count();
    }

    fn newer_search(&mut self) {
        let Mode::Prompt(p) = &mut self.mode else {
            return;
        };
        if p.older == self.history.len() {
            return;
        }
        p.older += 1;
        p.answer = if p.older == self.history.len() {
            std::mem::take(&mut p.typed)
        } else {
            self.history[p.older].clone()
        };
        p.x = p.answer.chars().count();
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
            PromptKind::Search { backwards } => {
                if !answer.is_empty() {
                    self.history.retain(|old| *old != answer);
                    self.history.push(answer);
                } else if self.history.is_empty() {
                    return self.cancel();
                }
                self.search(backwards);
            }
            PromptKind::Goto => {
                if answer.is_empty() {
                    return self.cancel();
                }
                match search::position(&answer) {
                    Some((line, col)) => self.goto(line, col),
                    None => self.error("Invalid line or column number"),
                }
            }
        }
    }

    /// Searches for the last search again.
    fn search(&mut self, backwards: bool) {
        self.message = None;
        self.spot = None;
        let Some(needle) = self.last_search().cloned() else {
            return self.error("No current search pattern");
        };
        let pattern = Pattern::new(&needle, self.case_sensitive);
        let Some(found) = search::find(&self.buf, &pattern, backwards) else {
            let shown = search::shorten(&needle, self.cols / 2);
            return self.error(&format!("\"{shown}\" not found"));
        };
        let start = (self.buf.line, self.buf.x);
        self.buf.line = found.line;
        self.buf.x = found.x;
        self.buf.keep_column(self.opts.tabsize);
        let len = needle
            .chars()
            .count()
            .min(self.buf.line_len(found.line) - found.x);
        // nano shows the selection instead of the match.
        if self.buf.mark.is_none() {
            self.spot = Some((found.line, found.x, found.x + len));
        }
        if (found.line, found.x) == start {
            self.say("This is the only occurrence");
        } else if found.wrapped {
            self.say("Search Wrapped");
        }
        self.center();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("editor-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Puts a new file (a new inode) at `path`, as another program saving it would.
    fn replace(path: &Path, text: &str) {
        let tmp = path.with_extension("new");
        fs::write(&tmp, text).unwrap();
        fs::rename(&tmp, path).unwrap();
    }

    fn open(path: &Path) -> Editor {
        let opts = Options {
            file: Some(path.display().to_string()),
            ..Options::default()
        };
        Editor::open(opts, 80, 24)
    }

    fn asking(ed: &Editor) -> bool {
        matches!(ed.mode, Mode::Ask(_))
    }

    /// The warning, then its question, as the main loop shows them.
    fn after_warning(ed: &mut Editor) {
        assert!(ed.warning.is_some(), "no warning");
        ed.warned();
        assert!(matches!(ed.mode, Mode::Ask(Question::Changed(..))));
    }

    #[test]
    fn yes_writes_only_over_the_changed_file_seen_when_asked() {
        let dir = dir("changed");
        let path = dir.join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let mut ed = open(&path);
        ed.key(Key::Char('x'));
        replace(&path, "two\n");
        ed.key(Key::Ctrl('S'));
        after_warning(&mut ed);
        // Swapped again while the question is shown: Yes does not write, it asks again.
        replace(&path, "three\n");
        ed.key(Key::Char('y'));
        assert_eq!(fs::read_to_string(&path).unwrap(), "three\n");
        after_warning(&mut ed);
        ed.key(Key::Char('y'));
        assert_eq!(fs::read_to_string(&path).unwrap(), "xone\n");
        assert!(!ed.buf.modified);
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn yes_overwrites_only_the_file_seen_when_asked() {
        let dir = dir("overwrite");
        let path = dir.join("other.txt");
        fs::write(&path, "other\n").unwrap();
        let mut ed = Editor::open(Options::default(), 80, 24);
        ed.key(Key::Char('x'));
        ed.key(Key::Ctrl('O'));
        for c in path.display().to_string().chars() {
            ed.key(Key::Char(c));
        }
        ed.key(Key::Ctrl('M'));
        assert!(matches!(ed.mode, Mode::Ask(Question::Overwrite(..))));
        replace(&path, "swapped\n");
        ed.key(Key::Char('y'));
        assert_eq!(fs::read_to_string(&path).unwrap(), "swapped\n");
        assert!(matches!(ed.mode, Mode::Ask(Question::Overwrite(..))));
        ed.key(Key::Char('y'));
        assert_eq!(fs::read_to_string(&path).unwrap(), "x\n");
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_of_another_user_is_not_written_over() {
        let dir = dir("owner");
        let path = dir.join("a.txt");
        fs::write(&path, "one\n").unwrap();
        let mut ed = open(&path);
        ed.key(Key::Char('x'));
        let now = files::stat(&path).unwrap().unwrap();
        // As if the file read had been another user's, now replaced by this user's.
        let mut read = now;
        read.uid = now.uid + 1;
        read.ino = now.ino + 1;
        ed.meta = Some(read);
        ed.key(Key::Ctrl('S'));
        assert!(ed.warning.is_none() && !asking(&ed));
        let message = ed.message.as_ref().unwrap();
        assert!(message.error);
        assert_eq!(
            message.text,
            "File on disk was replaced by another user's file; not saved"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\n");
        // Another name can still be written.
        let other = dir.join("b.txt");
        ed.key(Key::Ctrl('O'));
        for _ in 0..path.display().to_string().chars().count() {
            ed.key(Key::Ctrl('H'));
        }
        for c in other.display().to_string().chars() {
            ed.key(Key::Char(c));
        }
        ed.key(Key::Ctrl('M'));
        ed.key(Key::Char('y'));
        assert_eq!(fs::read_to_string(&other).unwrap(), "xone\n");
        // A change by the same user keeps nano's question.
        replace(&other, "changed\n");
        ed.key(Key::Char('z'));
        ed.key(Key::Ctrl('S'));
        after_warning(&mut ed);
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn typed(ed: &mut Editor, keys: &[Key]) {
        for key in keys {
            ed.key(key.clone());
        }
    }

    fn text(ed: &Editor) -> String {
        String::from(&ed.buf.text)
    }

    fn editor(text: &str) -> Editor {
        let mut ed = Editor::open(Options::default(), 80, 24);
        ed.buf = Buffer::new(text, Format::Unix);
        ed
    }

    #[test]
    fn a_cut_selection_is_pasted_and_undone() {
        let mut ed = editor("one two\n");
        typed(&mut ed, &[Key::Meta('a')]);
        let right = Key::Named(Named::Right, Mods::default());
        typed(
            &mut ed,
            &[right.clone(), right.clone(), right, Key::Ctrl('K')],
        );
        assert_eq!(text(&ed), " two\n");
        assert_eq!(ed.buf.mark, None);
        typed(&mut ed, &[Key::Ctrl('U')]);
        assert_eq!(text(&ed), "one two\n");
        typed(&mut ed, &[Key::Meta('u'), Key::Meta('u')]);
        assert_eq!(text(&ed), "one two\n");
    }

    #[test]
    fn copy_keeps_the_text_and_the_next_copy_adds_a_line() {
        let mut ed = editor("a\nb\nc\n");
        typed(&mut ed, &[Key::Meta('6'), Key::Meta('6')]);
        assert_eq!(ed.cut, "a\nb\n");
        assert_eq!(text(&ed), "a\nb\nc\n");
        assert!(!ed.buf.modified);
    }

    #[test]
    fn shift_selects_and_a_plain_move_unselects() {
        let mut ed = editor("abc\n");
        let shifted = Mods {
            shift: true,
            ..Mods::default()
        };
        typed(&mut ed, &[Key::Named(Named::Right, shifted)]);
        assert_eq!(ed.buf.selection(), Some((0, 1)));
        typed(&mut ed, &[Key::Named(Named::Right, Mods::default())]);
        assert_eq!(ed.buf.mark, None);
    }

    #[test]
    fn a_paste_is_one_step_and_is_not_interpreted() {
        let mut ed = editor("x\n");
        typed(
            &mut ed,
            &[
                Key::PasteStart,
                Key::Paste("a\n\tb".into()),
                Key::PasteEnd { flawed: false },
            ],
        );
        assert_eq!(text(&ed), "a\n\tbx\n");
        typed(&mut ed, &[Key::Meta('u')]);
        assert_eq!(text(&ed), "x\n");
    }
}
