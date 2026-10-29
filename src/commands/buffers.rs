// SPDX-License-Identifier: MIT
//! The buffers: opening the files of the command line one after the other, switching between
//! them, closing them, reading a file into a new buffer or into the text, and the emergency copy
//! of all that were changed.

use super::{Editor, Message, Mode, Prompt, PromptKind, Question, error_text, plural};
use crate::buffer::{self, Buffer, Format, Kind};
use crate::config::{MAX_BUFFERS, Place};
use crate::files::{self, Locking, Meta};
use crate::keys::{Key, Mods, Named};
use crate::syntax::{self, Rule};
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A buffer with what goes with it. The buffer being edited is `Editor::file`; the others are in
/// `Editor::files`, which holds a blank `File` in the place of the one being edited. Switching
/// swaps two whole `File`s, so what a buffer owns can grow without another list to keep in step.
pub struct File {
    pub buf: Buffer,
    /// The file name, empty for a new buffer.
    pub name: String,
    /// The file as read or written last, to notice changes on disk; None for a new file.
    pub(super) meta: Option<Meta>,
    /// The lock file held, and whether it says the buffer is modified.
    pub(super) lock: Option<files::Lock>,
    pub(super) lock_says_modified: bool,
    /// The rules of the syntax picked for the file, empty for none.
    pub syntax: Rc<[Rule]>,
    /// What went wrong when the file was opened while the editor started: shown once, when the
    /// buffer is switched to (as nano does).
    pub(super) read_error: Option<String>,
}

impl File {
    pub(super) fn blank() -> Self {
        Self {
            buf: Buffer::new("", Format::Unix),
            name: String::new(),
            meta: None,
            lock: None,
            lock_says_modified: false,
            syntax: Rc::from(Vec::new()),
            read_error: None,
        }
    }
}

/// Of the message shown so far and the one that came after it, while the files of the command
/// line are opened: nano keeps the more important one (an alert stays and a later alert only
/// adds dots to it; a warning stays against anything but a warning or an alert).
pub(super) fn more_important(old: Option<Message>, new: Option<Message>) -> Option<Message> {
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
pub(super) fn not_opened(name: &str) -> Option<&'static str> {
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
pub(super) fn read_message(lines: usize, format: Format) -> String {
    let mut msg = format!("Read {}", plural(lines, "line"));
    match format {
        Format::Dos => msg.push_str(" (converted from DOS format)"),
        Format::Mac => msg.push_str(" (converted from Mac format)"),
        Format::Unix => {}
    }
    msg
}

impl Editor {
    /// Opens the files of the command line one after the other, each with the place its `+`
    /// argument says. A lock question stops it: `lock_answered` goes on.
    pub(super) fn open_pending(&mut self) {
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
    pub(super) fn finish_start(&mut self) {
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
            self.spot = spot.map(|(_, from, to)| (self.file.buf.line, from, to));
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
    pub(super) fn position(
        &mut self,
        place: &Place,
        mut shown: Option<Message>,
    ) -> Option<Message> {
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
    pub(super) fn open_buffer(&mut self, name: &str) -> bool {
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
    pub(super) fn begin(&mut self, name: &str, lock: Option<files::Lock>) {
        self.new_buffer();
        self.file.name = name.to_string();
        if !name.is_empty() {
            self.read_file();
        }
        // The name goes when the file turns out not to be one to edit: no lock then.
        if let Some(lock) = lock.filter(|_| !self.file.name.is_empty()) {
            let _ = lock.set_modified(self.file.buf.modified);
            self.file.lock_says_modified = self.file.buf.modified;
            self.file.lock = Some(lock);
        }
        self.pick_syntax();
    }

    /// The answer to the question about another editor's lock file: Yes opens the file anyway
    /// and takes the lock over, No leaves it out. Cancelling while starting ends the editor.
    pub(super) fn lock_answered(&mut self, name: &str, yes: bool) {
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
    pub(super) fn new_buffer(&mut self) {
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
        std::mem::swap(&mut self.file, &mut self.files[i]);
    }

    /// Edits the buffer in place `to`.
    pub(super) fn go(&mut self, to: usize) {
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
    pub(super) fn close_buffer(&mut self, to: usize) {
        let old = self.current;
        self.go(to);
        self.files.remove(old);
        if to > old {
            self.current = to - 1;
        }
    }

    /// Closes the buffer being edited and shows the next; the editor is done after the last.
    pub(super) fn close_current(&mut self) {
        if self.files.len() <= 1 {
            self.done = true;
            return;
        }
        self.close_buffer((self.current + 1) % self.files.len());
        self.announce_switch();
    }

    /// M-< and M-> (or M-, and M-.): the previous or the next buffer, the first after the last.
    pub(super) fn switch(&mut self, forward: bool) {
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
    pub(super) fn announce_switch(&mut self) {
        match self.file.read_error.take() {
            Some(text) => self.alert(&text),
            None => self.mention(),
        }
    }

    /// The name of the buffer and its number of lines, as nano says them when switching. While
    /// the editor starts, it is not shown over an error, which is more important.
    pub(super) fn mention(&mut self) {
        if !self.started && self.message.as_ref().is_some_and(|m| m.error) {
            return;
        }
        let name = match self.file.name.rsplit('/').next() {
            Some(tail) if !tail.is_empty() => tail,
            _ => "New Buffer",
        };
        let lines = plural(self.file.buf.file_lines(), "line");
        let format = match self.file.buf.format {
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
    /// opened and when it is saved under another name. A syntax is compiled once, the first time
    /// a buffer needs it: the rules are shared, and so is the message of the first rule that
    /// could not be compiled, which every buffer of that syntax shows when it is picked.
    pub(super) fn pick_syntax(&mut self) {
        let first = syntax::head(&self.file.buf, 0);
        self.file.syntax = Rc::from(Vec::new());
        let syntaxes = &self.opts.syntaxes;
        // `-Y`: a syntax by its name, `none` for none. One that is not defined is said, and the
        // file name decides as without it. Without any syntax there is nothing to say.
        let mut unknown = None;
        let mut named = None;
        if let Some(name) = self.opts.syntax.as_deref().filter(|_| !syntaxes.is_empty()) {
            if name == "none" {
                return;
            }
            named = syntaxes.iter().rev().find(|d| d.name == name);
            unknown = named
                .is_none()
                .then(|| format!("Unknown syntax name: {name}"));
        }
        let found = named.or_else(|| syntax::find(syntaxes, &self.file.name, &first));
        let index = found.and_then(|def| syntaxes.iter().position(|d| std::ptr::eq(d, def)));
        if let Some(text) = unknown {
            self.alert(&text);
        }
        let Some(index) = index else {
            return;
        };
        let syntaxes = &self.opts.syntaxes;
        let known = self.compiled.iter().find(|c| c.0 == index);
        let (rules, error) = match known {
            Some((_, rules, error)) => (Rc::clone(rules), error.clone()),
            None => {
                let (rules, errors) = syntax::compile(&syntaxes[index]);
                let rules: Rc<[Rule]> = rules.into();
                let error = errors.into_iter().next();
                self.compiled
                    .push((index, Rc::clone(&rules), error.clone()));
                (rules, error)
            }
        };
        self.file.syntax = rules;
        if let Some(error) = error {
            self.error(&error);
        }
    }

    pub(super) fn read_file(&mut self) {
        match files::read(Path::new(&self.file.name)) {
            Ok(loaded) => {
                let text = buffer::decode(loaded.bytes);
                let (text, format) = if self.opts.noconvert {
                    (text, Format::Unix)
                } else {
                    buffer::convert_line_ends(text)
                };
                self.file.buf =
                    Buffer::new(&text, if self.opts.unix { Format::Unix } else { format });
                self.file.meta = Some(loaded.meta);
                let msg = read_message(self.file.buf.file_lines(), format);
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
                let name = std::mem::take(&mut self.file.name);
                self.alert(&format!("\"{name}\" {e}"));
            }
            // A file that cannot be read (no permission, say) is not kept as the buffer's name,
            // as in nano: saving would otherwise offer to write over a file never seen.
            Err(e) => {
                let name = std::mem::take(&mut self.file.name);
                self.alert(&format!("Error reading {name}: {}", error_text(&e)));
            }
        }
    }

    /// The text of every modified buffer goes to an emergency copy (the terminal went away, or
    /// SIGHUP or SIGTERM came); returns where each went. Every buffer is written whatever
    /// happens to the others, each as safely as a single one has always been, and all are synced
    /// together at the end (see `files::emergency_save_all`).
    pub fn emergency_save(&mut self) -> Vec<PathBuf> {
        // The buffer being edited goes into its place first, so that all are alike.
        if !self.files.is_empty() {
            self.swap_live(self.current);
        }
        let changed: Vec<&File> = self.files.iter().filter(|f| f.buf.modified).collect();
        let texts: Vec<Vec<u8>> = changed.iter().map(|f| f.buf.to_bytes()).collect();
        let buffers: Vec<(Option<&Path>, &[u8])> = changed
            .iter()
            .zip(&texts)
            .map(|(f, text)| ((!f.name.is_empty()).then(|| Path::new(&f.name)), &text[..]))
            .collect();
        files::emergency_save_all(&buffers)
            .into_iter()
            .flatten()
            .collect()
    }

    /// The keys that have to do with buffers, in the text or in the prompt for a file to read.
    /// Returns whether the key was one of them.
    pub(super) fn buffer_keys(&mut self, key: &Key) -> bool {
        match self.mode {
            Mode::Edit => self.buffer_key(key),
            Mode::Prompt(_) => self.insert_prompt_key(key),
            _ => false,
        }
    }

    /// The keys of the buffers in the text: ^R to read a file, and `M-<` `M->` (or Alt and the
    /// left or right arrow) to switch. Returns whether the key was one of them.
    fn buffer_key(&mut self, key: &Key) -> bool {
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        match key {
            Key::Ctrl('R') => self.ask_insert(String::new(), self.opts.multibuffer),
            Key::Meta('<') => self.switch(false),
            Key::Meta('>') => self.switch(true),
            Key::Named(Named::Left, m) if *m == alt => self.switch(false),
            Key::Named(Named::Right, m) if *m == alt => self.switch(true),
            _ => return false,
        }
        true
    }

    /// The keys that only the prompt for a file to read has. M-F reads into a new buffer, or
    /// not, and keeps what was typed. The file browser, the conversion switch and the command
    /// are not available yet; a key the editor cannot read is ignored, as in nano. Returns
    /// whether the key was one of them.
    fn insert_prompt_key(&mut self, key: &Key) -> bool {
        let Mode::Prompt(p) = &mut self.mode else {
            return false;
        };
        let PromptKind::Insert { new_buffer } = p.kind else {
            return false;
        };
        match key {
            Key::Meta('f') => {
                let answer = std::mem::take(&mut p.answer);
                self.ask_insert(answer, !new_buffer);
            }
            Key::Named(Named::NotAvailable, _) => {
                self.mode = Mode::Edit;
                self.error("Not available in this version yet");
            }
            _ => return false,
        }
        true
    }

    /// The prompt for a file to read into the text, or into a new buffer.
    pub(super) fn ask_insert(&mut self, answer: String, new_buffer: bool) {
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
    pub(super) fn insert_file(&mut self, name: &str, new_buffer: bool) {
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
    pub(super) fn insert_into_text(&mut self, name: &str) {
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
            self.file.buf.end_group();
            self.insert_text(&text, Kind::Insert);
            self.file.buf.end_group();
        }
        self.say(&read_message(lines, format));
        self.scroll_to_cursor();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{asking, dir, editor, text, typed};
    use super::*;
    use crate::config::Options;
    use crate::keys::{Key, Mods, Named};
    use crate::syntax::SyntaxDef;
    use std::fs;

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
        Path::new(&ed.file.name)
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
    fn a_function_not_available_in_the_insert_prompt_is_refused() {
        // The file browser, the command and the conversion switch.
        for key in [Key::Ctrl('T'), Key::Ctrl('X'), Key::Meta('n')] {
            let mut ed = editor("x\n");
            typed(&mut ed, &[Key::Ctrl('R'), key.clone()]);
            assert!(!matches!(ed.mode, Mode::Prompt(..)), "{key:?}");
            let message = ed.message.as_ref().map(|m| m.text.as_str());
            assert_eq!(message, Some("Not available in this version yet"));
            assert_eq!(text(&ed), "x\n");
        }
    }

    #[test]
    fn a_key_the_editor_cannot_read_is_ignored_in_the_insert_prompt() {
        let mut ed = editor("x\n");
        let unknown = Key::Named(Named::Unknown, Mods::default());
        typed(&mut ed, &[Key::Ctrl('R'), Key::Char('a'), unknown]);
        let Mode::Prompt(p) = &ed.mode else {
            panic!("the prompt was closed");
        };
        assert_eq!(p.answer, "a");
        assert!(ed.message.is_none());
    }

    #[test]
    fn buffers_of_one_syntax_share_its_rules() {
        let dir = dir("syntax-shared");
        for n in ["a.rs", "b.rs", "c.txt"] {
            fs::write(dir.join(n), "x\n").unwrap();
        }
        let def = |name: &str, files: &str| SyntaxDef {
            name: name.into(),
            files: syntax::regex_list(files, 10).0,
            headers: Vec::new(),
            rules: syntax::parse_rules("red \"x\"", false).0,
        };
        let opts = Options {
            files: ["a.rs", "b.rs", "c.txt"]
                .iter()
                .map(|n| Place::new(dir.join(n).display().to_string()))
                .collect(),
            syntaxes: vec![def("rust", "\"\\.rs$\""), def("text", "\"\\.txt$\"")],
            ..Options::default()
        };
        let mut ed = Editor::open(opts, 80, 24);
        // Three buffers, two syntaxes: each compiled once, and the same rules in a and b.
        assert_eq!(ed.compiled.len(), 2);
        let rules = |ed: &Editor| Rc::clone(&ed.file.syntax);
        let (a, b) = (rules(&ed), {
            ed.go(1);
            rules(&ed)
        });
        assert!(Rc::ptr_eq(&a, &b) && !a.is_empty());
        ed.go(2);
        assert!(!Rc::ptr_eq(&a, &ed.file.syntax));
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_buffer_keeps_its_own_syntax_lock_and_check_for_changes() {
        let dir = dir("per-buffer");
        for n in ["a.rs", "b.txt"] {
            fs::write(dir.join(n), "x\n").unwrap();
        }
        let opts = Options {
            files: ["a.rs", "b.txt"]
                .iter()
                .map(|n| Place::new(dir.join(n).display().to_string()))
                .collect(),
            syntaxes: vec![SyntaxDef {
                name: "rust".into(),
                files: syntax::regex_list("\"\\.rs$\"", 10).0,
                headers: Vec::new(),
                rules: syntax::parse_rules("red \"x\"", false).0,
            }],
            ..Options::default()
        };
        let mut ed = Editor::open(opts, 80, 24);
        // The syntax goes with the buffer.
        assert!(!ed.file.syntax.is_empty());
        typed(&mut ed, &[Key::Meta('>')]);
        assert!(ed.file.syntax.is_empty());
        // So does the lock file: both are held, and closing a buffer removes only its own.
        assert!(dir.join(".a.rs.swp").exists() && dir.join(".b.txt.swp").exists());
        // And what the buffer knows of the file on disk: b.txt is changed under the editor,
        // and only saving it warns; a.rs is saved as it was read.
        typed(&mut ed, &[Key::Char('y')]);
        replace_file(&dir.join("b.txt"), "other\n");
        typed(&mut ed, &[Key::Meta('<'), Key::Char('y'), Key::Ctrl('S')]);
        assert!(ed.warning.is_none() && !asking(&ed), "a.rs was not changed");
        assert_eq!(fs::read_to_string(dir.join("a.rs")).unwrap(), "yx\n");
        typed(&mut ed, &[Key::Meta('>'), Key::Ctrl('S')]);
        assert!(ed.warning.is_some(), "b.txt was changed on disk");
        drop(ed);
        assert!(!dir.join(".a.rs.swp").exists() && !dir.join(".b.txt.swp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_buffer_has_its_own_modified_flag_in_its_lock_file() {
        let dir = dir("lock-flag");
        for n in ["a.txt", "b.txt"] {
            fs::write(dir.join(n), "x\n").unwrap();
        }
        let mut ed = open_all(&dir, &["a.txt", "b.txt"]);
        // Byte 1007 of a lock file is nano's flag for a modified buffer.
        let flag = |n: &str| fs::read(dir.join(n)).unwrap()[1007];
        typed(&mut ed, &[Key::Char('y')]);
        assert_eq!((flag(".a.txt.swp"), flag(".b.txt.swp")), (0x55, 0));
        // Changing the second buffer is noticed although the first one is changed too.
        typed(&mut ed, &[Key::Meta('>'), Key::Char('y')]);
        assert_eq!((flag(".a.txt.swp"), flag(".b.txt.swp")), (0x55, 0x55));
        typed(&mut ed, &[Key::Meta('u')]);
        assert_eq!((flag(".a.txt.swp"), flag(".b.txt.swp")), (0x55, 0));
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Puts a new file (a new inode) at `path`, as another program saving it would.
    fn replace_file(path: &Path, text: &str) {
        let tmp = path.with_extension("new");
        fs::write(&tmp, text).unwrap();
        fs::rename(&tmp, path).unwrap();
    }

    #[test]
    fn an_unreadable_file_loses_its_name_and_says_why_once() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return; // Root reads any file.
        }
        let dir = dir("unreadable");
        for n in ["a.txt", "u.txt", "v.txt"] {
            fs::write(dir.join(n), "x\n").unwrap();
        }
        for n in ["u.txt", "v.txt"] {
            fs::set_permissions(dir.join(n), fs::Permissions::from_mode(0o000)).unwrap();
        }
        let mut ed = open_all(&dir, &["a.txt", "u.txt", "v.txt"]);
        let said = |ed: &Editor| {
            ed.message
                .as_ref()
                .map(|m| m.text.clone())
                .unwrap_or_default()
        };
        // As in nano, the buffer has no name: it is a new buffer, and saving does not offer
        // the name of a file that was never read. Each says its own error.
        typed(&mut ed, &[Key::Meta('>')]);
        assert_eq!(
            (ed.file.name.as_str(), ed.display_name()),
            ("", "New Buffer")
        );
        assert!(said(&ed).starts_with("Error reading "), "{}", said(&ed));
        assert!(
            said(&ed).ends_with("u.txt: Permission denied"),
            "{}",
            said(&ed)
        );
        assert!(
            !dir.join(".u.txt.swp").exists(),
            "no lock for a buffer with no name"
        );
        typed(&mut ed, &[Key::Meta('>')]);
        assert!(
            said(&ed).ends_with("v.txt: Permission denied"),
            "{}",
            said(&ed)
        );
        // The error is said when the buffer is switched to the first time only.
        typed(&mut ed, &[Key::Meta('>'), Key::Meta('>')]);
        assert_eq!(said(&ed), "New Buffer -- 0 lines");
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_single_unreadable_file_loses_its_name_too() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return;
        }
        let dir = dir("unreadable-one");
        fs::write(dir.join("u.txt"), "U\n").unwrap();
        fs::set_permissions(dir.join("u.txt"), fs::Permissions::from_mode(0o000)).unwrap();
        let ed = open_all(&dir, &["u.txt"]);
        assert_eq!(ed.file.name, "");
        drop(ed);
        fs::remove_dir_all(&dir).unwrap();
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
