// SPDX-License-Identifier: MIT
//! The editor's state and its commands: what each key does.

use crate::bindings::{self, Bindings, Outcome};
use crate::buffer::{self, Buffer, Format, Kind, Mark, next_index, prev_index};
use crate::complete::{self, Completion};
use crate::config::{MAX_BUFFERS, Options, Place};
use crate::files::{self, Expected, Locking, Meta, SaveError, SaveOptions};
use crate::keys::{Key, Mods, Named};
use crate::replace::Replacing;
use crate::search::{self, Pattern};
use crate::syntax::{self, Rule};
use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

/// How many keys a message stays for, as in nano.
const MESSAGE_KEYS: u8 = 20;

pub struct Message {
    pub text: String,
    pub error: bool,
    /// One of nano's alerts (the worst kind): a later alert while starting only adds dots to it.
    alert: bool,
    /// Dots after the message: more alerts came while the files were opened.
    pub dots: bool,
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
    /// Another editor holds the lock file of this file, which is being opened.
    Locked(String, files::LockInfo),
    /// A replacing, at a match.
    Replace(Box<Replacing>),
}

impl Question {
    /// The question as nano words it, with the space after it where nano has one.
    pub fn text(&self) -> String {
        match self {
            Question::SaveOnExit => "Save modified buffer? ".into(),
            Question::OtherName(..) => "Save file under DIFFERENT NAME? ".into(),
            Question::Overwrite(file, ..) => format!("File \"{file}\" exists; OVERWRITE? "),
            Question::Changed(..) => {
                "File was modified since you opened it; continue saving? ".into()
            }
            Question::Replace(_) => "Replace this instance? ".into(),
            Question::Locked(file, info) => format!(
                "File {file} is being edited by {} (with {}, PID {}); open anyway?",
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
    /// The text to search for, and whether to ask what to replace it with.
    Search { backwards: bool, replace: bool },
    /// What to replace the text searched for with.
    Replacement { backwards: bool },
    /// The line and column to go to.
    Goto,
    /// The file to read into the text, or into a new buffer.
    Insert { new_buffer: bool },
}

/// What the last key did to the cut buffer: the next cut, or copy, of a line adds to it when
/// it is the same.
#[derive(Clone, Copy, PartialEq)]
enum Chain {
    None,
    Cut,
    Copy,
}

/// A buffer that is not being edited now, with what goes with it. While one is edited, its
/// parts are the `Editor`'s own (`buf`, `name`, ...), and its place in `Editor::files` holds a
/// blank `File`: `go` swaps them, so no other code needs to know which buffer is current.
struct File {
    buf: Buffer,
    name: String,
    meta: Option<Meta>,
    lock: Option<files::Lock>,
    lock_says_modified: bool,
    syntax: Vec<Rule>,
    /// What went wrong when the file was opened while the editor started: shown once, when the
    /// buffer is switched to (as nano does).
    read_error: Option<String>,
}

impl File {
    fn blank() -> Self {
        Self {
            buf: Buffer::new("", Format::Unix),
            name: String::new(),
            meta: None,
            lock: None,
            lock_says_modified: false,
            syntax: Vec::new(),
            read_error: None,
        }
    }
}

pub struct Editor {
    pub buf: Buffer,
    /// The file name, empty for a new buffer.
    pub name: String,
    pub opts: Options,
    bindings: Bindings,
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
    /// Whether the text searched for is a regular expression (kept like the case).
    regexp: bool,
    /// What the replacements were so far, the oldest first.
    replace_history: Vec<String>,
    /// What the last search found, highlighted until the next key: line, from, to (characters).
    pub spot: Option<(usize, usize, usize)>,
    /// The file as read or written last, to notice changes on disk; None for a new file.
    meta: Option<Meta>,
    /// The lock file held, and whether it says the buffer is modified.
    lock: Option<files::Lock>,
    lock_says_modified: bool,
    /// A warning shown for a moment, with nothing else, before this question (as nano does).
    pub warning: Option<Question>,
    /// The rules of the syntax picked for the file, empty for none: see `syntax`.
    pub syntax: Vec<Rule>,
    read_error: Option<String>,
    /// All the buffers in order; the one being edited is at `current`, as a blank `File`.
    files: Vec<File>,
    current: usize,
    /// Two buffers were open at some time: the title bar shows `[n/m]` from then on (as nano).
    more_than_one: bool,
    /// The files of the command line that are not opened yet, and the one whose lock question is
    /// being asked.
    pending: VecDeque<Place>,
    placing: Option<Place>,
    /// The start-up is over: the files of the command line are open.
    pub started: bool,
    /// The editor is done: it exits.
    pub done: bool,
}

pub const HELP_TITLE: &str = "Help";

impl Editor {
    /// The help lines for the mode, from the key bindings.
    pub fn help_lines_of(&self) -> Vec<(String, &'static str)> {
        let mut lines = self.bindings.help_lines(self.slot());
        // As in nano, ^X closes a buffer while there are several, and exits with the last.
        if self.files.len() > 1 {
            for line in lines.iter_mut().filter(|l| l.1 == "Exit") {
                line.1 = "Close";
            }
        }
        lines
    }

    /// The menu of the key bindings that a key pressed in this mode belongs to.
    fn slot(&self) -> usize {
        match &self.mode {
            Mode::Edit => bindings::MAIN,
            Mode::Help(_) => bindings::HELP,
            Mode::Ask(_) => bindings::YES_NO,
            Mode::Prompt(p) => match p.kind {
                PromptKind::Search { replace: false, .. } => bindings::SEARCH,
                PromptKind::Search { replace: true, .. } => bindings::REPLACE,
                PromptKind::Replacement { .. } => bindings::REPLACE_WITH,
                PromptKind::Goto => bindings::GOTO,
                PromptKind::Write { .. } => bindings::WRITE_OUT,
                PromptKind::Insert { .. } => bindings::INSERT,
            },
        }
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
    (
        "^R",
        "F5",
        "Read a file into the text (M-F: into a new buffer)",
    ),
    ("M-<", "M-,", "Switch to the previous file buffer"),
    ("M->", "M-.", "Switch to the next file buffer"),
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

/// The history that the answer of a prompt comes from.
fn prompt_history<'a>(
    kind: PromptKind,
    searches: &'a [String],
    replacements: &'a [String],
) -> &'a [String] {
    match kind {
        PromptKind::Replacement { .. } => replacements,
        _ => searches,
    }
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

/// Of the message shown so far and the one that came after it, while the files of the command
/// line are opened: nano keeps the more important one (an alert stays and a later alert only
/// adds dots to it; a warning stays against anything but a warning or an alert).
fn more_important(old: Option<Message>, new: Option<Message>) -> Option<Message> {
    match (old, new) {
        (Some(mut old), Some(new)) if old.alert => {
            old.dots |= new.alert;
            Some(old)
        }
        (Some(old), Some(new)) if old.error && !new.error => Some(old),
        (_, Some(new)) => Some(new),
        (old, None) => old,
    }
}

/// Why nano does not open the path in a buffer, if so: a directory or a device.
fn not_opened(name: &str) -> Option<&'static str> {
    let kind = std::fs::metadata(name).ok()?.file_type();
    if kind.is_dir() {
        Some("is a directory")
    } else if kind.is_char_device() || kind.is_block_device() {
        Some("is a device file")
    } else {
        None
    }
}

/// What reading a file says, with the number of lines and the format it was converted from.
fn read_message(lines: usize, format: Format) -> String {
    let mut msg = format!("Read {}", plural(lines, "line"));
    match format {
        Format::Dos => msg.push_str(" (converted from DOS format)"),
        Format::Mac => msg.push_str(" (converted from Mac format)"),
        Format::Unix => {}
    }
    msg
}

impl Editor {
    /// Opens the files in `opts`, a buffer each, or a new buffer.
    pub fn open(mut opts: Options, cols: usize, rows: usize) -> Self {
        let (case_sensitive, regexp) = (opts.casesensitive, opts.regexp);
        let pending = std::mem::take(&mut opts.files).into();
        let blank = File::blank();
        let mut ed = Self {
            buf: blank.buf,
            name: blank.name,
            bindings: Bindings::new(opts.modernbindings, &opts.binds),
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
            case_sensitive,
            regexp,
            replace_history: Vec::new(),
            spot: None,
            meta: blank.meta,
            lock: blank.lock,
            lock_says_modified: blank.lock_says_modified,
            warning: None,
            syntax: blank.syntax,
            read_error: blank.read_error,
            files: Vec::new(),
            current: 0,
            more_than_one: false,
            pending,
            placing: None,
            started: false,
            done: false,
        };
        ed.open_pending();
        ed
    }

    /// Opens the files of the command line one after the other, each with the place its `+`
    /// argument says. A lock question stops it: `lock_answered` goes on.
    fn open_pending(&mut self) {
        while let Some(place) = self.pending.pop_front() {
            let mut shown = self.message.take();
            // As in nano, what is wrong with the `+` argument is said before the file is opened.
            if let Some(text) = &place.alert {
                self.alert(text);
                shown = more_important(shown, self.message.take());
            }
            let opened = self.open_buffer(&place.file);
            shown = more_important(shown, self.message.take());
            if matches!(self.mode, Mode::Ask(_)) {
                self.message = shown;
                self.placing = Some(place);
                return;
            }
            if opened {
                shown = self.position(&place, shown);
            }
            self.message = shown;
        }
        self.finish_start();
    }

    /// All the files are open: the first is shown, as nano does after the last one.
    fn finish_start(&mut self) {
        if self.files.is_empty() {
            self.begin("", None);
            if self.message.as_ref().is_none_or(|m| !m.error) {
                self.say("For basic help, type Ctrl+G.");
            }
        } else {
            // The match of a search made while starting is still marked, in the first buffer
            // (on its cursor's line), as in nano.
            let spot = self.spot;
            self.go(0);
            self.spot = spot.map(|(_, from, to)| (self.buf.line, from, to));
            if self.more_than_one {
                self.mention();
            }
        }
        if let Some(problem) = self.opts.problem.take() {
            let shown = self.message.take();
            self.alert(&problem);
            self.message = more_important(shown, self.message.take());
        }
        self.started = true;
    }

    /// Where the `+` argument of a file puts the cursor: a line and column, or the first match.
    /// `shown` is the message so far; returns what is shown after.
    fn position(&mut self, place: &Place, mut shown: Option<Message>) -> Option<Message> {
        if let Some((line, col)) = place.start {
            self.goto(Some(line), Some(col));
        }
        if let Some(found) = &place.search {
            self.case_sensitive = found.case_sensitive;
            self.regexp = found.regexp;
            self.history.push(found.text.clone());
            self.message = None;
            self.search(found.backwards);
            match self.message.take() {
                Some(failure) if failure.error => shown = more_important(shown, Some(failure)),
                // As in nano, a match wipes the status bar (the wrapping and the reading of the
                // file are not said), unless an error is on it.
                _ if shown.as_ref().is_some_and(|m| !m.error) => shown = None,
                _ => {}
            }
        }
        shown
    }

    /// Opens `name` in a new buffer after the current one (a blank buffer for no name). Nothing
    /// is opened for a directory or a device, and another editor's lock file is asked about
    /// first: the answer goes on in `lock_answered`. Returns whether a buffer was made.
    ///
    /// A file that is open already is not looked for among the buffers, as in nano: the lock file
    /// of the buffer that has it brings up the same question as another editor's would, and Yes
    /// opens it a second time. That keeps the rule simple and the user in charge; the check for
    /// changes on disk then warns whichever of the two saves second.
    fn open_buffer(&mut self, name: &str) -> bool {
        if let Some(what) = not_opened(name) {
            self.alert(&format!("\"{name}\" {what}"));
            return false;
        }
        if name.is_empty() {
            self.begin(name, None);
            return true;
        }
        match files::lock(Path::new(name), false) {
            Ok(Locking::Busy(info)) => {
                self.mode = Mode::Ask(Question::Locked(name.to_string(), info));
                false
            }
            Ok(Locking::Locked(lock)) => {
                self.begin(name, Some(lock));
                true
            }
            Ok(Locking::Bad(path)) => {
                self.begin(name, None);
                self.alert(&format!("Bad lock file is ignored: {}", path.display()));
                true
            }
            // A directory without write permission: the file is edited without a lock.
            Err(_) => {
                self.begin(name, None);
                true
            }
        }
    }

    /// The buffer for `name`, read from the file and holding its lock.
    fn begin(&mut self, name: &str, lock: Option<files::Lock>) {
        self.new_buffer();
        self.name = name.to_string();
        if !name.is_empty() {
            self.read_file();
        }
        // The name goes when the file turns out not to be one to edit: no lock then.
        if let Some(lock) = lock.filter(|_| !self.name.is_empty()) {
            let _ = lock.set_modified(self.buf.modified);
            self.lock_says_modified = self.buf.modified;
            self.lock = Some(lock);
        }
        self.pick_syntax();
    }

    /// The answer to the question about another editor's lock file: Yes opens the file anyway
    /// and takes the lock over, No leaves it out. Cancelling while starting ends the editor.
    fn lock_answered(&mut self, name: &str, yes: bool) {
        // As in nano, the buffer is made before the lock is looked at, so the title bar shows
        // `[n/m]` from the answer on, even when the answer is No.
        self.more_than_one |= !self.files.is_empty();
        let shown = self.message.take();
        if yes {
            let lock = match files::lock(Path::new(name), true) {
                Ok(Locking::Locked(lock)) => Some(lock),
                _ => None,
            };
            self.begin(name, lock);
        }
        let Some(place) = self.placing.take() else {
            return;
        };
        let opened = self.message.take();
        let shown = more_important(shown, opened);
        self.message = if yes {
            self.position(&place, shown)
        } else {
            shown
        };
        self.open_pending();
    }

    /// Makes a blank buffer after the current one, and makes it the current one.
    fn new_buffer(&mut self) {
        let at = if self.files.is_empty() {
            0
        } else {
            self.swap_live(self.current);
            self.more_than_one = true;
            self.current + 1
        };
        self.files.insert(at, File::blank());
        self.current = at;
    }

    /// Swaps the buffer being edited with the one in place `i`.
    fn swap_live(&mut self, i: usize) {
        let f = &mut self.files[i];
        std::mem::swap(&mut self.buf, &mut f.buf);
        std::mem::swap(&mut self.name, &mut f.name);
        std::mem::swap(&mut self.meta, &mut f.meta);
        std::mem::swap(&mut self.lock, &mut f.lock);
        std::mem::swap(&mut self.lock_says_modified, &mut f.lock_says_modified);
        std::mem::swap(&mut self.syntax, &mut f.syntax);
        std::mem::swap(&mut self.read_error, &mut f.read_error);
    }

    /// Edits the buffer in place `to`.
    fn go(&mut self, to: usize) {
        if to != self.current {
            // The buffer being edited goes to its place, then the other one comes out of its.
            self.swap_live(self.current);
            self.swap_live(to);
            self.current = to;
        }
        self.scroll_to_cursor();
    }

    /// Closes the buffer being edited and shows the one in place `to`. Dropping it removes its
    /// lock file, but not another editor's (see `Lock`).
    fn close_buffer(&mut self, to: usize) {
        let old = self.current;
        self.go(to);
        self.files.remove(old);
        if to > old {
            self.current = to - 1;
        }
    }

    /// Closes the buffer being edited and shows the next; the editor is done after the last.
    fn close_current(&mut self) {
        if self.files.len() <= 1 {
            self.done = true;
            return;
        }
        self.close_buffer((self.current + 1) % self.files.len());
        self.announce_switch();
    }

    /// M-< and M-> (or M-, and M-.): the previous or the next buffer, the first after the last.
    fn switch(&mut self, forward: bool) {
        let n = self.files.len();
        if n <= 1 {
            return self.error("No more open file buffers");
        }
        self.go(if forward {
            (self.current + 1) % n
        } else {
            (self.current + n - 1) % n
        });
        self.announce_switch();
    }

    /// After a switch: what went wrong when the buffer was opened, once, or its name.
    fn announce_switch(&mut self) {
        match self.read_error.take() {
            Some(text) => self.alert(&text),
            None => self.mention(),
        }
    }

    /// The name of the buffer and its number of lines, as nano says them when switching. While
    /// the editor starts, it is not shown over an error, which is more important.
    fn mention(&mut self) {
        if !self.started && self.message.as_ref().is_some_and(|m| m.error) {
            return;
        }
        let name = match self.name.rsplit('/').next() {
            Some(tail) if !tail.is_empty() => tail,
            _ => "New Buffer",
        };
        let lines = plural(self.buf.file_lines(), "line");
        let format = match self.buf.format {
            Format::Unix => "",
            Format::Dos => " (DOS)",
            Format::Mac => " (Mac)",
        };
        let text = format!("{name} -- {lines}{format}");
        self.say(&text);
    }

    /// The left of the title bar: the number of the buffer out of all, once there were several.
    pub fn upper_left(&self) -> String {
        if self.more_than_one {
            format!("[{}/{}]", self.current + 1, self.files.len())
        } else {
            self.program.clone()
        }
    }

    /// Picks the syntax for the file by its name or its first line, as nano does when the file is
    /// opened and when it is saved under another name.
    fn pick_syntax(&mut self) {
        let first = syntax::head(&self.buf, 0);
        self.syntax = Vec::new();
        let Some(def) = syntax::find(&self.opts.syntaxes, &self.name, &first) else {
            return;
        };
        let (rules, errors) = syntax::compile(def);
        self.syntax = rules;
        if let Some(first) = errors.first() {
            self.error(first);
        }
    }

    fn read_file(&mut self) {
        match files::read(Path::new(&self.name)) {
            Ok(loaded) => {
                let text = buffer::decode(loaded.bytes);
                let (text, format) = if self.opts.noconvert {
                    (text, Format::Unix)
                } else {
                    buffer::convert_line_ends(text)
                };
                self.buf = Buffer::new(&text, if self.opts.unix { Format::Unix } else { format });
                self.meta = Some(loaded.meta);
                let msg = read_message(self.buf.file_lines(), format);
                self.magic_line();
                self.say(&msg);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => self.say("New File"),
            // Not a file to edit (a FIFO, say): nano says so and edits a new buffer.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::IsADirectory | io::ErrorKind::InvalidInput
                ) =>
            {
                let name = std::mem::take(&mut self.name);
                self.alert(&format!("\"{name}\" {e}"));
            }
            Err(e) => {
                let text = format!("Error reading {}: {}", self.name, error_text(&e));
                self.alert(&text);
            }
        }
    }

    /// The text of every modified buffer goes to an emergency copy (the terminal went away, or
    /// SIGHUP or SIGTERM came); returns where each went. Every buffer is written whatever
    /// happens to the others, each as safely as a single one has always been.
    pub fn emergency_save(&mut self) -> Vec<PathBuf> {
        // The buffer being edited goes into its place first, so that all are alike.
        if !self.files.is_empty() {
            self.swap_live(self.current);
        }
        let mut written = Vec::new();
        for file in &self.files {
            if !file.buf.modified {
                continue;
            }
            let name = (!file.name.is_empty()).then(|| Path::new(&file.name));
            if let Ok(path) = files::emergency_save(name, &file.buf.to_bytes()) {
                written.push(path);
            }
        }
        written
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
        self.center_cursor();
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
            alert: false,
            dots: false,
            keys_left: MESSAGE_KEYS,
        });
    }

    fn error(&mut self, text: &str) {
        self.message = Some(Message {
            text: text.to_string(),
            error: true,
            alert: false,
            dots: false,
            keys_left: MESSAGE_KEYS,
        });
    }

    /// An error of the worst kind in nano's words: a file that cannot be opened. While the
    /// editor starts, nano keeps it for a file that was not read, to say it again when the
    /// buffer is switched to.
    fn alert(&mut self, text: &str) {
        self.error(text);
        if let Some(m) = &mut self.message {
            m.alert = true;
        }
        let several = self.files.len() > 1;
        if !self.started && several && self.meta.is_none() && self.read_error.is_none() {
            self.read_error = Some(text.to_string());
        }
    }

    pub fn resized(&mut self, cols: usize, rows: usize) {
        self.cols = cols;
        self.rows = rows;
        self.message = None;
        self.scroll_to_cursor();
    }

    /// One key.
    pub fn key(&mut self, key: Key) {
        let key = match self.bindings.translate(self.slot(), key) {
            Outcome::Key(key) => key,
            Outcome::Text(text) => {
                text.chars().for_each(|c| self.key(Key::Char(c)));
                return;
            }
            Outcome::Unbound(name) => {
                if matches!(self.mode, Mode::Edit) {
                    self.error(&format!("Unbound key: {name}"));
                }
                return;
            }
        };
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
            Key::Ctrl('W' | 'F' | 'Q' | 'B' | '\\') | Key::Meta('r') | Key::Named(Named::F(6), _)
        );
        let in_search = matches!(self.mode, Mode::Prompt(_) | Mode::Ask(Question::Replace(_)));
        if !(in_search || asks_search && matches!(self.mode, Mode::Edit)) {
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
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        let alt = Mods {
            alt: true,
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
            Key::Ctrl('I') => self.tab(),
            Key::Ctrl('M') => self.enter(),
            Key::Ctrl('H') | Key::Named(Backspace, _) => self.backspace(),
            Key::Ctrl('D') | Key::Named(Delete, _) => self.delete(),
            Key::Ctrl('P') | Key::Named(Up, _) => self.up(),
            Key::Ctrl('N') | Key::Named(Down, _) => self.down(),
            Key::Named(Left, m) if m == alt => self.switch(false),
            Key::Named(Right, m) if m == alt => self.switch(true),
            Key::Named(Left, m) if m == ctrl => self.word_left(),
            Key::Named(Right, m) if m == ctrl => self.word_right(),
            Key::Named(Left, _) => self.left(),
            Key::Named(Right, _) => self.right(),
            Key::Ctrl('A') | Key::Named(Home, Mods { ctrl: false, .. }) => self.home(),
            Key::Ctrl('E') | Key::Named(End, Mods { ctrl: false, .. }) => self.end(),
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
            Key::Ctrl('R') => self.ask_insert(String::new(), self.opts.multibuffer),
            Key::Meta('<') => self.switch(false),
            Key::Meta('>') => self.switch(true),
            Key::Ctrl('O') | Key::Named(F(3), _) => self.ask_name(false),
            Key::Ctrl('S') => self.save_now(),
            Key::Ctrl('C') | Key::Named(F(11), _) => {
                let loc = self.location();
                self.say(&loc);
            }
            Key::Ctrl('F') | Key::Ctrl('W') | Key::Named(F(6), _) => {
                self.ask_search(false, false, String::new());
            }
            Key::Ctrl('Q') | Key::Ctrl('B') => self.ask_search(true, false, String::new()),
            Key::Ctrl('\\') | Key::Meta('r') => self.ask_search(false, true, String::new()),
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
                    self.center_cursor();
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
        self.insert_text(
            s,
            if s.starts_with('\n') {
                Kind::Enter
            } else {
                Kind::Add
            },
        );
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
        let mut text = String::from("\n");
        if self.opts.autoindent {
            let line = self.buf.line_text(self.buf.line);
            // As in nano, at the end of a line of blanks only, they move to the new line.
            let blanks = line.chars().count();
            if blanks > 0 && self.buf.x == blanks && line.chars().all(|c| matches!(c, ' ' | '\t')) {
                let at = self.buf.pos() - blanks;
                self.buf.substitute(at, blanks, &format!("\n{line}"));
                self.buf.keep_column(self.opts.tabsize);
                return;
            }
            let indent = line.chars().take_while(|c| matches!(c, ' ' | '\t'));
            text.extend(indent.take(self.buf.x));
        }
        self.type_text(&text);
    }

    /// With `-E`, the spaces up to the next tab stop.
    fn tab(&mut self) {
        if self.opts.tabstospaces {
            let tab = self.opts.tabsize;
            let spaces = tab - self.buf.column(tab) % tab;
            self.type_text(&" ".repeat(spaces));
        } else {
            self.type_text("\t");
        }
    }

    /// The start of the line; with `-A`, first the first character that is not a blank, as long
    /// as the cursor is not there already.
    fn home(&mut self) {
        if self.opts.softwrap && self.wrap_home() {
            return;
        }
        let b = &mut self.buf;
        let indent = b
            .line_text(b.line)
            .chars()
            .take_while(|c| matches!(c, ' ' | '\t'))
            .count();
        // On a line of blanks only, nano's Home does not stop at the end of them.
        let blank_line = indent == b.line_len(b.line);
        b.x = if self.opts.smarthome && b.x != indent && !blank_line {
            indent
        } else {
            0
        };
        b.keep_column(self.opts.tabsize);
    }

    /// The end of the line; with soft wrapping, first the end of the row.
    fn end(&mut self) {
        if self.opts.softwrap {
            return self.wrap_end();
        }
        self.buf.x = self.buf.line_len(self.buf.line);
        self.buf.keep_column(self.opts.tabsize);
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
        let wordchars = self.opts.wordchars.clone();
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
                let start = complete::fragment_start(text, cursor, &wordchars);
                if start == cursor {
                    self.error("No word fragment");
                    return;
                }
                Completion::new(String::from(text.slice(start..cursor)), start)
            }
        };
        let Some(ending) = completion.next_ending(&self.buf.text, &wordchars) else {
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
        if self.zap_selection() {
            return;
        }
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
        if self.zap_selection() {
            return;
        }
        self.delete_at(self.buf.pos());
    }

    /// For `-Z`: erases the selection, which does not go to the cut buffer. Returns whether there
    /// was one.
    fn zap_selection(&mut self) -> bool {
        let Some((from, to)) = self.buf.selection().filter(|_| self.opts.zap) else {
            return false;
        };
        self.buf.mark = None;
        if from != to {
            self.erase_cut(from, to, Kind::Zap);
        }
        true
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
        if self.opts.softwrap {
            self.wrap_up();
        } else if self.buf.line > 0 {
            self.buf.goto_line(self.buf.line - 1, self.opts.tabsize);
        }
    }

    fn down(&mut self) {
        if self.opts.softwrap {
            self.wrap_down();
        } else if self.buf.line + 1 < self.buf.lines() {
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

    /// Letters and digits, the `-X` characters, and with `-W` also punctuation.
    fn is_word(&self, c: Option<char>) -> bool {
        c.is_some_and(|c| {
            c.is_alphanumeric()
                || self.opts.wordchars.contains(c)
                || (self.opts.wordbounds && !c.is_whitespace() && !c.is_control())
        })
    }

    /// To the start of the next word, or with `afterends` to the end of this one.
    fn word_right(&mut self) {
        let mut pos = self.buf.pos();
        if self.opts.afterends {
            let mut seen_word = self.is_word(self.char_at(pos));
            while self.char_at(pos).is_some() {
                pos += 1;
                if self.is_word(self.char_at(pos)) {
                    seen_word = true;
                } else if seen_word {
                    break;
                }
            }
        } else {
            while self.is_word(self.char_at(pos)) {
                pos += 1;
            }
            while self.char_at(pos).is_some() && !self.is_word(self.char_at(pos)) {
                pos += 1;
            }
        }
        self.buf.set_pos(pos);
        self.buf.keep_column(self.opts.tabsize);
    }

    /// To the start of this word, or of the one before.
    fn word_left(&mut self) {
        let mut pos = self.buf.pos();
        while pos > 0 && !self.is_word(self.char_at(pos - 1)) {
            pos -= 1;
        }
        while pos > 0 && self.is_word(self.char_at(pos - 1)) {
            pos -= 1;
        }
        self.buf.set_pos(pos);
        self.buf.keep_column(self.opts.tabsize);
    }

    /// A page is the text rows less two, as in nano: two lines stay on the screen.
    pub fn page(&self) -> usize {
        self.edit_rows().saturating_sub(2).max(1)
    }

    fn page_up(&mut self) {
        if self.opts.softwrap {
            return self.wrap_page(false);
        }
        let page = self.page();
        self.buf.top = self.buf.top.saturating_sub(page);
        self.buf
            .goto_line(self.buf.line.saturating_sub(page), self.opts.tabsize);
    }

    /// Down a page, the text with the cursor; at the end, the cursor goes to the last line.
    fn page_down(&mut self) {
        if self.opts.softwrap {
            return self.wrap_page(true);
        }
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
        if self.opts.softwrap {
            return self.wrap_scroll();
        }
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
        if self.opts.softwrap {
            return self.wrap_center(false);
        }
        let rows = self.edit_rows();
        let b = &mut self.buf;
        if b.line < b.top || b.line >= b.top + rows {
            b.top = b.line.saturating_sub(rows / 2);
        }
    }

    /// Puts the cursor's line (with soft wrapping, its row) in the middle of the screen.
    fn center_cursor(&mut self) {
        if self.opts.softwrap {
            return self.wrap_center(true);
        }
        let rows = self.edit_rows();
        self.buf.top = self.buf.line.saturating_sub(rows / 2);
    }

    /// ^K: the selection, or else the line; cutting lines in a row adds them to the cut buffer.
    fn cut(&mut self, chain: Chain) {
        if let Some((from, to)) = self.buf.selection() {
            self.buf.mark = None;
            if from == to {
                return self.say("Nothing was cut");
            }
            self.cut = String::from(self.buf.text.slice(from..to));
            self.erase_cut(from, to, Kind::Cut);
            return;
        }
        let (from, to) = if self.opts.cutfromcursor {
            self.cursor_to_line_end()
        } else {
            self.line_range()
        };
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
        if !self.opts.cutfromcursor {
            b.x = 0;
        }
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
        self.erase_cut(from, to, Kind::Cut);
    }

    /// Deletes the text between `from` and `to` as one step of undo of its own, with the cursor
    /// left at `from`.
    fn erase_cut(&mut self, from: usize, to: usize, kind: Kind) {
        let b = &mut self.buf;
        let before = b.pos();
        b.end_group();
        b.set_pos(from);
        b.erase(kind, from, to, before);
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

    /// For `-k`: from the cursor to the end of the line; at the end of a line, its line break.
    fn cursor_to_line_end(&self) -> (usize, usize) {
        let b = &self.buf;
        let from = b.pos();
        let end = b.text.line_to_char(b.line) + b.line_len(b.line);
        if from < end || b.line + 1 == b.lines() {
            (from, end)
        } else {
            (from, end + 1)
        }
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
        if self.buf.modified && self.opts.saveonexit {
            if self.name.is_empty() {
                // As nano: a warning for a moment, then the usual question.
                self.error("No file name");
                self.warning = Some(Question::SaveOnExit);
            } else {
                self.save_as(self.name.clone(), true);
            }
        } else if self.buf.modified {
            self.mode = Mode::Ask(Question::SaveOnExit);
        } else {
            self.close_current();
        }
    }

    fn ask_key(&mut self, key: Key) {
        let Mode::Ask(question) = std::mem::replace(&mut self.mode, Mode::Edit) else {
            return;
        };
        if let Question::Replace(run) = question {
            return self.replace_key(run, key);
        }
        let yes = match key {
            Key::Char('y' | 'Y') => true,
            Key::Char('n' | 'N') => false,
            // Cancelling the question about a lock file ends the editor while it starts, as in
            // nano; later it is a No.
            Key::Ctrl('C') if matches!(question, Question::Locked(..)) => {
                if let (Question::Locked(name, _), true) = (&question, self.started) {
                    return self.lock_answered(name, false);
                }
                self.done = true;
                return;
            }
            // As nano: cancelling the question about a file changed on disk goes back to its name.
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
            (Question::SaveOnExit, false) => self.close_current(),
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
            (Question::Locked(name, _), yes) => self.lock_answered(&name, yes),
            (Question::Replace(_), _) => {}
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
        let options = SaveOptions {
            backup: self.opts.backup,
            backup_dir: self.opts.backupdir.clone(),
            ..SaveOptions::default()
        };
        match files::save(Path::new(&name), &bytes, expected, &options) {
            Ok(saved) => {
                self.meta = Some(saved.meta);
                self.buf.mark_saved();
                if name != self.name {
                    // The lock goes with the name.
                    self.lock = None;
                    self.name = name;
                    self.pick_syntax();
                    if let Ok(Locking::Locked(lock)) = files::lock(Path::new(&self.name), false) {
                        self.lock = Some(lock);
                        self.lock_says_modified = false;
                    }
                }
                let lines = self.buf.file_lines();
                self.say(&format!("Wrote {}", plural(lines, "line")));
                if exit {
                    self.close_current();
                }
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
    fn ask_search(&mut self, backwards: bool, replace: bool, answer: String) {
        self.message = None;
        let mut prompt = Prompt::new(
            PromptKind::Search { backwards, replace },
            String::new(),
            answer,
            self.history.len(),
        );
        prompt.label = self.search_label(backwards, replace);
        self.mode = Mode::Prompt(prompt);
    }

    /// "Search", the toggles that are on, what it is for, and the last search (cut when long) in
    /// brackets.
    fn search_label(&self, backwards: bool, replace: bool) -> String {
        let mut label = "Search".to_string();
        if self.case_sensitive {
            label.push_str(" [Case Sensitive]");
        }
        if self.regexp {
            label.push_str(" [Regexp]");
        }
        if backwards {
            label.push_str(" [Backwards]");
        }
        if replace {
            label.push_str(" (to replace)");
            if self.buf.mark.is_some() {
                label.push_str(" in selection");
            }
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

    /// The prompt for a file to read into the text, or into a new buffer.
    fn ask_insert(&mut self, answer: String, new_buffer: bool) {
        self.message = None;
        let label = if new_buffer {
            "File to read into new buffer [from ./]"
        } else {
            "File to insert [from ./]"
        };
        let kind = PromptKind::Insert { new_buffer };
        self.mode = Mode::Prompt(Prompt::new(kind, label.into(), answer, 0));
    }

    /// Reads the file `name` into a new buffer, or into the text at the cursor.
    fn insert_file(&mut self, name: &str, new_buffer: bool) {
        match (new_buffer, name.is_empty()) {
            // As nano: no name is a cancel, unless it asks for a new buffer.
            (false, true) => self.cancel(),
            // Every buffer holds a file, a lock and its undo steps, and the title bar and the
            // `.save` copies of a hung-up terminal have to deal with all of them; a few hundred
            // is far more than anyone edits at once.
            (true, _) if self.files.len() >= MAX_BUFFERS => {
                self.error(&format!("At most {MAX_BUFFERS} file buffers can be open"));
            }
            (true, _) => {
                self.open_buffer(name);
            }
            (false, false) => self.insert_into_text(name),
        }
    }

    /// Puts the text of the file `name` in at the cursor, as one step of undo.
    fn insert_into_text(&mut self, name: &str) {
        if let Some(what) = not_opened(name) {
            return self.alert(&format!("\"{name}\" {what}"));
        }
        let loaded = match files::read(Path::new(name)) {
            Ok(loaded) => loaded,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return self.alert(&format!("File \"{name}\" not found"));
            }
            Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                return self.alert(&format!("\"{name}\" {e}"));
            }
            Err(e) => {
                return self.alert(&format!("Error reading {name}: {}", error_text(&e)));
            }
        };
        let text = buffer::decode(loaded.bytes);
        let (text, format) = if self.opts.noconvert {
            (text, Format::Unix)
        } else {
            buffer::convert_line_ends(text)
        };
        let lines = Buffer::new(&text, format).file_lines();
        // An empty file changes nothing: the buffer is not marked as modified.
        if !text.is_empty() {
            self.buf.end_group();
            self.insert_text(&text, Kind::Insert);
            self.buf.end_group();
        }
        self.say(&read_message(lines, format));
        self.scroll_to_cursor();
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
            (Key::Meta('c'), PromptKind::Search { backwards, replace }) => {
                self.case_sensitive = !self.case_sensitive;
                self.ask_search_again(backwards, replace);
            }
            (Key::Meta('r'), PromptKind::Search { backwards, replace }) => {
                self.regexp = !self.regexp;
                self.ask_search_again(backwards, replace);
            }
            (Key::Meta('b'), PromptKind::Search { backwards, replace }) => {
                self.ask_search_again(!backwards, replace);
            }
            (Key::Ctrl('R'), PromptKind::Search { backwards, replace }) => {
                self.ask_search_again(backwards, !replace);
            }
            (
                Key::Ctrl('P') | Key::Named(Named::Up, _),
                PromptKind::Search { .. } | PromptKind::Replacement { .. },
            ) => self.older_search(),
            (
                Key::Ctrl('N') | Key::Named(Named::Down, _),
                PromptKind::Search { .. } | PromptKind::Replacement { .. },
            ) => self.newer_search(),
            (Key::Ctrl('T'), PromptKind::Search { replace: false, .. }) => {
                let answer = std::mem::take(&mut p.answer);
                self.ask_goto(answer);
            }
            // The two that read a file (M-F: into a new buffer, or not) keep what was typed.
            (Key::Meta('f'), PromptKind::Insert { new_buffer }) => {
                let answer = std::mem::take(&mut p.answer);
                self.ask_insert(answer, !new_buffer);
            }
            // The file browser, the conversion switch and the command are not available yet.
            (Key::Named(Named::Unknown, _), PromptKind::Insert { .. }) => {
                self.mode = Mode::Edit;
                self.error("Not available in this version yet");
            }
            (Key::Ctrl('T'), PromptKind::Goto) => {
                let answer = std::mem::take(&mut p.answer);
                self.ask_search(false, false, answer);
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
    fn ask_search_again(&mut self, backwards: bool, replace: bool) {
        let Mode::Prompt(old) = std::mem::replace(&mut self.mode, Mode::Edit) else {
            return;
        };
        let label = self.search_label(backwards, replace);
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Search { backwards, replace },
            label,
            ..old
        });
    }

    /// An older search in the answer; the text typed is kept for coming back to it.
    fn older_search(&mut self) {
        let Mode::Prompt(p) = &mut self.mode else {
            return;
        };
        let history = prompt_history(p.kind, &self.history, &self.replace_history);
        if p.older == 0 {
            return;
        }
        if p.older == history.len() {
            p.typed = std::mem::take(&mut p.answer);
        }
        p.older -= 1;
        p.answer = history[p.older].clone();
        p.x = p.answer.chars().count();
    }

    fn newer_search(&mut self) {
        let Mode::Prompt(p) = &mut self.mode else {
            return;
        };
        let history = prompt_history(p.kind, &self.history, &self.replace_history);
        if p.older == history.len() {
            return;
        }
        p.older += 1;
        p.answer = if p.older == history.len() {
            std::mem::take(&mut p.typed)
        } else {
            history[p.older].clone()
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
            PromptKind::Search { backwards, replace } => {
                if !answer.is_empty() {
                    self.history.retain(|old| *old != answer);
                    self.history.push(answer);
                } else if self.history.is_empty() {
                    return self.cancel();
                }
                if replace {
                    self.ask_replacement(backwards);
                } else {
                    self.search(backwards);
                }
            }
            PromptKind::Replacement { backwards } => {
                if !answer.is_empty() {
                    self.replace_history.retain(|old| *old != answer);
                    self.replace_history.push(answer.clone());
                }
                self.start_replacing(backwards, answer);
            }
            PromptKind::Insert { new_buffer } => self.insert_file(&answer, new_buffer),
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
        let pattern = match Pattern::new(&needle, self.case_sensitive, self.regexp) {
            Ok(pattern) => pattern,
            Err(why) => return self.error(&format!("Bad regex \"{needle}\": {why}")),
        };
        let Some(found) = search::find(&self.buf, &pattern, backwards) else {
            let shown = search::shorten(&needle, self.cols / 2);
            return self.error(&format!("\"{shown}\" not found"));
        };
        let start = (self.buf.line, self.buf.x);
        self.buf.line = found.line;
        self.buf.x = found.x;
        self.buf.keep_column(self.opts.tabsize);
        // nano shows the selection instead of the match.
        if self.buf.mark.is_none() {
            self.spot = Some((found.line, found.x, found.x + found.len));
        }
        if (found.line, found.x) == start {
            self.say("This is the only occurrence");
        } else if found.wrapped {
            self.say("Search Wrapped");
        }
        self.center();
    }

    /// The prompt for what to replace the last search with.
    fn ask_replacement(&mut self, backwards: bool) {
        let needle = self.last_search().cloned().unwrap_or_default();
        // The unsupported back-reference is reported where nano would ask what to do with a match.
        if let Err(why) = Pattern::new(&needle, self.case_sensitive, self.regexp) {
            if why != search::BACKREFERENCE {
                return self.error(&format!("Bad regex \"{needle}\": {why}"));
            }
        }
        self.mode = Mode::Prompt(Prompt::new(
            PromptKind::Replacement { backwards },
            "Replace with".into(),
            String::new(),
            self.replace_history.len(),
        ));
    }

    fn start_replacing(&mut self, backwards: bool, with: String) {
        self.message = None;
        self.spot = None;
        let needle = self.last_search().cloned().unwrap_or_default();
        let pattern = match Pattern::new(&needle, self.case_sensitive, self.regexp) {
            Ok(pattern) => pattern,
            Err(why) => return self.error(&format!("Bad regex \"{needle}\": {why}")),
        };
        let run = Replacing::new(pattern, with, backwards, &self.buf);
        self.replace_next(Box::new(run));
    }

    /// Asks about the next match, or ends when there is none.
    fn replace_next(&mut self, mut run: Box<Replacing>) {
        let Some((at, len)) = run.next(&self.buf) else {
            return self.end_replacing(&run);
        };
        self.buf.set_pos(at);
        self.buf.keep_column(self.opts.tabsize);
        self.spot = Some((self.buf.line, self.buf.x, self.buf.x + len));
        self.center();
        self.mode = Mode::Ask(Question::Replace(run));
    }

    fn replace_key(&mut self, mut run: Box<Replacing>, key: Key) {
        match key {
            Key::Char('y' | 'Y') => run.replace(&mut self.buf),
            Key::Char('n' | 'N') => run.skip(),
            Key::Char('a' | 'A') => {
                run.replace_all(&mut self.buf);
                return self.end_replacing(&run);
            }
            Key::Ctrl('C') => return self.end_replacing(&run),
            _ => {
                self.mode = Mode::Ask(Question::Replace(run));
                return;
            }
        }
        self.replace_next(run);
    }

    fn end_replacing(&mut self, run: &Replacing) {
        self.spot = None;
        self.mode = Mode::Edit;
        run.finish(&mut self.buf);
        self.buf.keep_column(self.opts.tabsize);
        self.center();
        if run.found {
            let n = run.count;
            self.say(&format!("Replaced {}", plural(n, "occurrence")));
        } else {
            let needle = self.last_search().cloned().unwrap_or_default();
            let shown = search::shorten(&needle, self.cols / 2);
            self.error(&format!("\"{shown}\" not found"));
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
            files: vec![Place::new(path.display().to_string())],
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
    fn afterends_stops_the_word_keys_at_the_end_of_words() {
        let ctrl_right = Key::Named(
            Named::Right,
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
        for (afterends, stops) in [(false, [4, 8, 9]), (true, [3, 7, 9])] {
            let opts = Options {
                afterends,
                ..Options::default()
            };
            let mut ed = Editor::open(opts, 80, 24);
            ed.buf = Buffer::new("one two\nx", Format::Unix);
            let mut at = Vec::new();
            for _ in 0..3 {
                typed(&mut ed, std::slice::from_ref(&ctrl_right));
                at.push(ed.buf.pos());
            }
            assert_eq!(at, stops, "afterends {afterends}");
        }
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

    /// Opens the files of `names` in `dir` as the command line does.
    fn open_all(dir: &Path, names: &[&str]) -> Editor {
        let files = names
            .iter()
            .map(|n| Place::new(dir.join(n).display().to_string()))
            .collect();
        let opts = Options {
            files,
            ..Options::default()
        };
        Editor::open(opts, 80, 24)
    }

    fn named(ed: &Editor) -> String {
        Path::new(&ed.name)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn buffers_switch_in_order_and_close_one_by_one() {
        let dir = dir("several");
        for (n, t) in [("a.txt", "A\n"), ("b.txt", "B\n"), ("c.txt", "C\n")] {
            fs::write(dir.join(n), t).unwrap();
        }
        let mut ed = open_all(&dir, &["a.txt", "b.txt", "c.txt"]);
        assert_eq!(
            (named(&ed), ed.upper_left()),
            ("a.txt".into(), "[1/3]".into())
        );
        typed(&mut ed, &[Key::Meta('>'), Key::Meta('>')]);
        assert_eq!(
            (named(&ed), text(&ed)),
            ("c.txt".into(), "A\n".replace('A', "C"))
        );
        // The last buffer goes round to the first.
        typed(&mut ed, &[Key::Meta('>')]);
        assert_eq!(named(&ed), "a.txt");
        typed(&mut ed, &[Key::Meta('<')]);
        assert_eq!(named(&ed), "c.txt");
        // Closing a buffer goes on to the next one, and the editor ends after the last.
        typed(&mut ed, &[Key::Ctrl('X')]);
        assert_eq!(
            (named(&ed), ed.upper_left()),
            ("a.txt".into(), "[1/2]".into())
        );
        typed(&mut ed, &[Key::Ctrl('X'), Key::Ctrl('X')]);
        assert!(ed.done);
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_a_modified_buffer_asks_and_cancel_keeps_it() {
        let dir = dir("close-ask");
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        fs::write(dir.join("b.txt"), "B\n").unwrap();
        let mut ed = open_all(&dir, &["a.txt", "b.txt"]);
        typed(&mut ed, &[Key::Char('x'), Key::Ctrl('X')]);
        assert!(asking(&ed));
        typed(&mut ed, &[Key::Ctrl('C')]);
        assert!(!asking(&ed) && !ed.done);
        assert_eq!(named(&ed), "a.txt");
        // No closes it without writing, and the other buffer is the one shown.
        typed(&mut ed, &[Key::Ctrl('X'), Key::Char('n')]);
        assert_eq!(named(&ed), "b.txt");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "A\n");
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_buffer_has_its_own_undo_and_its_own_text() {
        let dir = dir("undo");
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        fs::write(dir.join("b.txt"), "B\n").unwrap();
        let mut ed = open_all(&dir, &["a.txt", "b.txt"]);
        typed(&mut ed, &[Key::Char('x'), Key::Meta('>')]);
        // Nothing was done in the second buffer, so there is nothing to undo there.
        typed(&mut ed, &[Key::Meta('u')]);
        assert_eq!(text(&ed), "B\n");
        typed(&mut ed, &[Key::Meta('<'), Key::Meta('u')]);
        assert_eq!(text(&ed), "A\n");
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_same_file_twice_asks_about_its_own_lock() {
        let dir = dir("twice");
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        // nano does not look for a file it has open already: the second time the lock file of
        // the first buffer is found and the user is asked, and Yes opens a second buffer.
        let mut ed = open_all(&dir, &["a.txt", "a.txt"]);
        assert!(matches!(ed.mode, Mode::Ask(Question::Locked(..))));
        typed(&mut ed, &[Key::Char('n')]);
        assert_eq!(ed.files.len(), 1);
        assert!(!ed.done);
        drop(ed);
        let mut ed = open_all(&dir, &["a.txt", "a.txt"]);
        typed(&mut ed, &[Key::Char('y')]);
        assert_eq!(ed.files.len(), 2);
        // Closing one of them must not take away the lock file of the other.
        typed(&mut ed, &[Key::Ctrl('X')]);
        assert!(dir.join(".a.txt.swp").exists());
        drop(ed);
        assert!(!dir.join(".a.txt.swp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_or_a_device_is_not_opened() {
        let dir = dir("not-opened");
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let ed = open_all(&dir, &["a.txt", "sub"]);
        assert_eq!(ed.files.len(), 1);
        assert!(!ed.more_than_one);
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_insert_prompt_reads_into_a_new_buffer_with_the_toggle() {
        let dir = dir("insert-new");
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        fs::write(dir.join("b.txt"), "B\n").unwrap();
        let mut ed = open_all(&dir, &["a.txt"]);
        typed(&mut ed, &[Key::Ctrl('R'), Key::Meta('f')]);
        for c in dir.join("b.txt").display().to_string().chars() {
            ed.key(Key::Char(c));
        }
        ed.key(Key::Ctrl('M'));
        assert_eq!(
            (ed.files.len(), named(&ed), text(&ed)),
            (2, "b.txt".into(), "B\n".into())
        );
        // Without the toggle the file goes in at the cursor of the current buffer.
        typed(&mut ed, &[Key::Meta('<'), Key::Ctrl('R')]);
        for c in dir.join("b.txt").display().to_string().chars() {
            ed.key(Key::Char(c));
        }
        ed.key(Key::Ctrl('M'));
        assert_eq!((ed.files.len(), text(&ed)), (2, "B\nA\n".into()));
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_key_with_no_function_in_the_insert_prompt_is_refused() {
        let mut ed = editor("x\n");
        typed(&mut ed, &[Key::Ctrl('R')]);
        assert!(matches!(ed.mode, Mode::Prompt(..)));
        // ^T would open the file browser, which is not available yet.
        typed(&mut ed, &[Key::Ctrl('T')]);
        assert!(!matches!(ed.mode, Mode::Prompt(..)));
        assert_eq!(text(&ed), "x\n");
    }

    #[test]
    fn a_new_buffer_is_refused_at_the_cap() {
        let dir = dir("cap");
        fs::write(dir.join("a.txt"), "A\n").unwrap();
        let mut ed = open_all(&dir, &["a.txt"]);
        // Blank buffers are enough to reach the cap.
        for _ in 1..MAX_BUFFERS {
            ed.insert_file("", true);
        }
        assert_eq!(ed.files.len(), MAX_BUFFERS);
        ed.insert_file("", true);
        assert_eq!(ed.files.len(), MAX_BUFFERS);
        assert!(ed.message.as_ref().is_some_and(|m| m.error));
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_alert_stays_and_later_alerts_only_add_dots() {
        let mut ed = editor("x\n");
        let mut made = |kind: u8| {
            match kind {
                0 => ed.say("plain"),
                1 => ed.error("error"),
                _ => ed.alert("alert"),
            }
            ed.message.take()
        };
        let kept = more_important(made(1), made(0)).unwrap();
        assert_eq!(kept.text, "error");
        let kept = more_important(made(0), made(2)).unwrap();
        assert_eq!(kept.text, "alert");
        let kept = more_important(made(2), made(2)).unwrap();
        assert_eq!((kept.text.as_str(), kept.dots), ("alert", true));
    }
}
