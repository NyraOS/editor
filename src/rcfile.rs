// SPDX-License-Identifier: MIT
//! The nanorc files, read as GNU nano 8.4 reads them. In this order, a later one wins:
//! `/usr/share/nyra-editor/nanorc` (the distribution's), `/etc/nanorc` (the administrator's),
//! then the user's: `~/.nanorc` if it exists, else `$XDG_CONFIG_HOME/nano/nanorc`, else
//! `~/.config/nano/nanorc` (only one of the three, as in nano). The command line wins over all.
//!
//! The home directory is `$HOME`, except for root, which takes it from the password database
//! (as nano does): after `su`, root must not read the user's files. Without NSS: only
//! `/etc/passwd` is looked at.
//!
//! For the tests only, `NYRA_EDITOR_SYSCONFDIR=DIR` replaces the two system files by
//! `DIR/nanorc`, so that the host's files cannot change a result. Only builds with the
//! `test-sysconfdir` feature (or unit tests) read it: a release binary always reads the real system
//! files. Even then it is ignored for root and when
//! the real and effective user differ: an environment that can be inherited (`sudo`, `su -m`) must
//! not choose the administrator's settings of a privileged editor.
//!
//! A line is `set OPTION [ARGUMENT]`, `unset OPTION` or `include "GLOB"` (only in those files, not
//! in an included one: as in nano, those are for syntax files), one of the syntax commands
//! (`syntax`, `header`, `color`, `icolor`, `extendsyntax`, see `syntax`; `comment`, `tabgives`,
//! `linter` and `formatter` are not available yet; `magic` only works with nano's `--magic`, which is
//! not available yet), or
//! `bind` / `unbind` (see `bindings`). A line the editor cannot use is reported on the standard error
//! in nano's words, and the rest is read: the editor starts anyway, with a message in the status
//! bar. An option nano has and the editor does not yet is reported once, in the same way, and the
//! rest is read. An invalid value resets the option to its default, as in nano.
//!
//! The files are not trusted. Deliberate differences from nano, which reports and goes on: a file
//! is read up to `MAX_FILE` bytes (the rest is not read, and a message says so), at most
//! `MAX_FILES` included files are read, at most `MAX_MESSAGES` messages are shown, and each
//! shows at most `MAX_SHOWN` bytes of what is wrong. Only the first two drop settings; after the
//! message limit, every line is still applied. The syntaxes are bounded too: see `MAX_SYNTAXES`,
//! `MAX_MATCHERS`, `MAX_RULES` and `MAX_RULE_COST`.
//!
//! A deliberate difference from nano, which reads a syntax only when a file needs it and reports
//! the mistakes in its `color` lines then: the editor reads them all at the start, checks the
//! regular expressions without compiling them (compiling is left for the syntax picked), and
//! reports every mistake at once.

use crate::bindings;
use crate::colours;
use crate::config::{Options, parse_num};
use crate::syntax::{self, SyntaxDef};
use std::borrow::Cow;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const SYSTEM_FILES: [&str; 2] = ["/usr/share/nyra-editor/nanorc", "/etc/nanorc"];
/// Only for the tests, see the top of this file.
#[cfg(any(test, feature = "test-sysconfdir"))]
const SYSCONFDIR_VARIABLE: &str = "NYRA_EDITOR_SYSCONFDIR";
const MAX_FILE: u64 = 1 << 20;
const MAX_MESSAGES: usize = 50;
const MAX_FILES: usize = 1000;
const MAX_SHOWN: usize = 100;
/// The most changes of key bindings kept: many more than there are keys and menus.
const MAX_BINDS: usize = 10_000;
/// The most files one glob gives.
const MAX_MATCHES: usize = 256;
/// The most syntaxes kept: far more than there are file types.
const MAX_SYNTAXES: usize = 1000;
/// The most regexes kept for choosing a syntax by the name or the first line of a file. Each is
/// compiled at the start (half a millisecond), and takes up to `syntax::SIZE_LIMIT` of memory.
const MAX_MATCHERS: usize = 1000;
/// The most rules one syntax keeps. The rules of the syntax picked are compiled when a file is
/// opened and each is used on every redraw: with 125,000 one-letter rules that cost 0.9 s at the
/// start and 0.25 s for each key. The longest syntax files of nano have about 150 rules.
const MAX_RULES: usize = 2000;
/// The most the rules of all syntaxes may add up to: each rule counts 64 bytes and the length of
/// its expressions. The rules are kept as text until a syntax is picked, so this bounds the memory
/// that a nanorc (or 1000 included files) can take with them.
const MAX_RULE_COST: usize = 8 << 20;

/// The options that work, as they are in the table of the command line.
fn flag<'a>(o: &'a mut Options, name: &str) -> Option<&'a mut bool> {
    Some(match name {
        "afterends" => &mut o.afterends,
        "atblanks" => &mut o.atblanks,
        "autoindent" => &mut o.autoindent,
        "backup" => &mut o.backup,
        "casesensitive" => &mut o.casesensitive,
        "constantshow" => &mut o.constantshow,
        "cutfromcursor" => &mut o.cutfromcursor,
        "linenumbers" => &mut o.linenumbers,
        "mouse" => &mut o.mouse,
        "multibuffer" => &mut o.multibuffer,
        "noconvert" => &mut o.noconvert,
        "nohelp" => &mut o.nohelp,
        "nonewlines" => &mut o.nonewlines,
        "regexp" => &mut o.regexp,
        "saveonexit" => &mut o.saveonexit,
        "smarthome" => &mut o.smarthome,
        "softwrap" => &mut o.softwrap,
        "tabstospaces" => &mut o.tabstospaces,
        "unix" => &mut o.unix,
        "wordbounds" => &mut o.wordbounds,
        "zap" => &mut o.zap,
        _ => return None,
    })
}

/// Options nano has, that change nothing here (or nothing nano would show): accepted.
const IGNORED_FLAGS: &[&str] = &["bookstyle", "locking", "nowrap", "trimblanks"];

/// Options nano has, that the editor does not have yet: reported once, when set.
const LATER_FLAGS: &[&str] = &[
    "allow_insecure_backup",
    "boldtext",
    "breaklonglines",
    "colonparsing",
    "emptyline",
    "historylog",
    "indicator",
    "jumpyscrolling",
    "minibar",
    "positionlog",
    "preserve",
    "quickblank",
    "rawsequences",
    "rebinddelete",
    "showcursor",
    "stateflags",
    "zero",
];

/// The options that take an argument (and cannot be unset).
const ARGUMENT_OPTIONS: &[&str] = &[
    "backupdir",
    "brackets",
    "errorcolor",
    "fill",
    "functioncolor",
    "guidestripe",
    "keycolor",
    "matchbrackets",
    "minicolor",
    "numbercolor",
    "operatingdir",
    "promptcolor",
    "punct",
    "quotestr",
    "scrollercolor",
    "selectedcolor",
    "speller",
    "spotlightcolor",
    "statuscolor",
    "stripecolor",
    "tabsize",
    "titlecolor",
    "whitespace",
    "wordchars",
];

/// Reads the nanorc files into `options`. The second result is the message for the status bar
/// when there were mistakes; the mistakes themselves are already on the standard error.
pub fn load(options: Options) -> (Options, Option<String>) {
    let (env_home, xdg) = environment_dirs(
        is_privileged(),
        std::env::var_os("HOME"),
        env_path("XDG_CONFIG_HOME"),
    );
    let home = home_dir(env_home);
    let mut rc = Reader::new(options, home.clone());
    for file in system_files() {
        rc.read_top(&file);
    }
    if let Some(file) = user_file(home.as_deref(), xdg.as_deref()) {
        rc.read_top(&file);
    }
    let mut stderr = io::stderr();
    for message in &rc.messages {
        let _ = writeln!(stderr, "{message}");
    }
    let alert = rc.first_file.map(|f| format!("Mistakes in '{f}'"));
    (rc.options, alert)
}

/// The system files in the order they are read.
fn system_files() -> Vec<PathBuf> {
    let user = rustix::process::getuid();
    #[cfg(any(test, feature = "test-sysconfdir"))]
    if let Some(dir) = std::env::var_os(SYSCONFDIR_VARIABLE).filter(|v| !v.is_empty()) {
        if !user.is_root() && user == rustix::process::geteuid() {
            return vec![Path::new(&dir).join("nanorc")];
        }
    }
    let _ = user;
    SYSTEM_FILES.iter().map(PathBuf::from).collect()
}

/// Running as root, or with the rights of another user than the one who started the editor.
fn is_privileged() -> bool {
    let user = rustix::process::geteuid();
    user.is_root() || user != rustix::process::getuid()
}

/// `$HOME` and `$XDG_CONFIG_HOME`, unless the editor is privileged: a privileged editor must not
/// take its settings from the environment of whoever started it (`su` can keep that environment).
fn environment_dirs(
    privileged: bool,
    home: Option<OsString>,
    xdg: Option<PathBuf>,
) -> (Option<OsString>, Option<PathBuf>) {
    if privileged {
        (None, None)
    } else {
        (home, xdg)
    }
}

/// `$HOME` (given as `env_home`), or when there is none, the home directory of the effective user.
fn home_dir(env_home: Option<OsString>) -> Option<PathBuf> {
    let home = match env_home {
        Some(home) => home,
        None => {
            let passwd = fs::read_to_string("/etc/passwd").ok()?;
            passwd_home(&passwd, rustix::process::geteuid().as_raw())?.into()
        }
    };
    (!home.is_empty()).then(|| PathBuf::from(home))
}

/// The home directory of `uid` in the text of a password file.
fn passwd_home(passwd: &str, uid: u32) -> Option<&str> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        let (id, home) = (fields.nth(2)?, fields.nth(2)?);
        (id.parse() == Ok(uid)).then_some(home)
    })
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The user's file: `~/.nanorc` if there is one, else the one in the XDG directory.
fn user_file(home: Option<&Path>, xdg: Option<&Path>) -> Option<PathBuf> {
    let plain = home?.join(".nanorc");
    if plain.exists() {
        return Some(plain);
    }
    Some(match xdg {
        Some(dir) => dir.join("nano/nanorc"),
        None => home?.join(".config/nano/nanorc"),
    })
}

/// Where a mistake is, for the message.
#[derive(Clone, Copy)]
struct Spot<'a> {
    file: &'a str,
    line: usize,
}

struct Reader {
    options: Options,
    home: Option<PathBuf>,
    messages: Vec<String>,
    /// The file of the first mistake.
    first_file: Option<String>,
    files_left: usize,
    /// The included files are over `MAX_FILES`, and it has been said.
    too_many_files: bool,
    /// The options and commands already reported as not available yet.
    later: Vec<String>,
    /// How many regexes for choosing a syntax are kept, and the cost of the rules so far.
    matchers: usize,
    rule_cost: usize,
}

/// The syntax whose lines are being read, the last one `syntax` began: nano's "open syntax".
struct Open {
    /// The line of its `syntax` command, for the mistake of having no colour commands.
    line: usize,
    seen_color: bool,
}

impl Reader {
    fn new(options: Options, home: Option<PathBuf>) -> Self {
        Self {
            options,
            home,
            messages: Vec::new(),
            first_file: None,
            files_left: MAX_FILES,
            too_many_files: false,
            later: Vec::new(),
            matchers: 0,
            rule_cost: 0,
        }
    }

    fn error(&mut self, spot: Spot, what: &str) {
        // Past the limit nothing is shown, but the lines are still read and applied.
        if self.messages.len() >= MAX_MESSAGES {
            return;
        }
        self.first_file.get_or_insert_with(|| spot.file.to_string());
        // Line 0: a file as a whole, the message says which.
        let mut message = match spot.line {
            0 => what.to_string(),
            line => format!("Error in {} on line {line}: {what}", spot.file),
        };
        if self.messages.len() + 1 == MAX_MESSAGES {
            message.push_str("\nToo many mistakes: the others are not shown");
        }
        self.messages.push(message);
    }

    /// A file the editor looks for itself: not being there is no mistake.
    fn read_top(&mut self, path: &Path) {
        if matches!(fs::metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound) {
            return;
        }
        let shown = path.display().to_string();
        let whole = Spot {
            file: &shown,
            line: 0,
        };
        match read_limited(&shown) {
            Ok((bytes, cut)) => {
                self.parse(&shown, &bytes, false);
                if cut {
                    self.error(whole, &too_big(&shown));
                }
            }
            Err(what) => self.error(whole, &what),
        }
    }

    /// One file; `included`: it was named by an `include`.
    fn parse(&mut self, file: &str, bytes: &[u8], included: bool) {
        let mut open: Option<Open> = None;
        for (i, raw) in bytes.split(|&b| b == b'\n').enumerate() {
            let spot = Spot { file, line: i + 1 };
            // As in nano, a NUL ends the line, and a CR before the newline is not part of it.
            let raw = raw.split(|&b| b == 0).next().unwrap_or_default();
            let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
            let text = String::from_utf8_lossy(raw);
            let valid = matches!(text, Cow::Borrowed(_));
            let line = text.trim_start_matches(is_blank);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (keyword, rest) = word(line);
            // As in nano, these end the syntax being defined (in an included file they are refused
            // and end nothing).
            if matches!(keyword, "syntax" | "extendsyntax" | "include")
                || (matches!(keyword, "set" | "unset") && !included)
            {
                self.close(spot.file, &mut open);
            }
            let done = match keyword {
                "set" | "unset" | "include" | "extendsyntax" if included => Err(format!(
                    "Command \"{keyword}\" not allowed in included file"
                )),
                "set" | "unset" => self.set_option(spot, keyword == "set", rest, valid),
                "include" => {
                    self.include(spot, rest);
                    Ok(())
                }
                "syntax" => self.begin_syntax(spot, rest, &mut open),
                "extendsyntax" => self.extend_syntax(spot, rest),
                "color" | "icolor" | "header" | "comment" | "linter" | "formatter" | "tabgives" => {
                    match &mut open {
                        Some(o) => {
                            o.seen_color |= matches!(keyword, "color" | "icolor");
                            let index = self.options.syntaxes.len() - 1;
                            self.for_syntax(spot, index, keyword, rest)
                        }
                        None => Err(format!(
                            "A '{keyword}' command requires a preceding 'syntax' command"
                        )),
                    }
                }
                // Only used with nano's --magic, which is not available yet.
                "magic" => Ok(()),
                "bind" | "unbind" => self.bind(keyword == "bind", rest),
                _ => Err(format!("Command \"{}\" not understood", shown(keyword))),
            };
            if let Err(what) = done {
                self.error(spot, &what);
            }
        }
        self.close(file, &mut open);
    }

    /// Ends the syntax being defined, if any: it must have a colour command, as nano wants (the
    /// mistake is said at the line of its `syntax` command).
    fn close(&mut self, file: &str, open: &mut Option<Open>) {
        if let Some(o) = open.take().filter(|o| !o.seen_color) {
            let name = self.options.syntaxes.last().map_or("", |s| s.name.as_str());
            let what = format!("Syntax \"{}\" has no color commands", shown(name));
            self.error(Spot { file, line: o.line }, &what);
        }
    }

    /// `syntax NAME "REGEX"...`: begins a syntax whose file names match the regexes.
    fn begin_syntax(
        &mut self,
        spot: Spot,
        rest: &str,
        open: &mut Option<Open>,
    ) -> Result<(), String> {
        let (token, after) = word(rest);
        if rest.is_empty() || rest == "\"" || rest.starts_with("\"\"") {
            return Err("Missing syntax name".into());
        }
        let quoted = token.starts_with('"');
        if quoted != token.ends_with('"') {
            return Err("Unpaired quote in syntax name".into());
        }
        let name = if quoted {
            &token[1..token.len() - 1]
        } else {
            token
        };
        if name == "none" {
            return Err("The \"none\" syntax is reserved".into());
        }
        if self.options.syntaxes.len() >= MAX_SYNTAXES {
            return Err("Too many syntaxes".into());
        }
        self.options.syntaxes.push(SyntaxDef {
            name: name.to_string(),
            files: Vec::new(),
            headers: Vec::new(),
            rules: Vec::new(),
        });
        *open = Some(Open {
            line: spot.line,
            seen_color: false,
        });
        if after.is_empty() {
            return Ok(());
        }
        if name == "default" {
            return Err("The \"default\" syntax does not accept extensions".into());
        }
        self.matching(spot, self.options.syntaxes.len() - 1, false, after);
        Ok(())
    }

    /// `extendsyntax NAME COMMAND...`: a command for a syntax defined earlier.
    fn extend_syntax(&mut self, spot: Spot, rest: &str) -> Result<(), String> {
        let (name, rest) = word(rest);
        let Some(index) = self.options.syntaxes.iter().rposition(|s| s.name == name) else {
            return Err(format!(
                "Could not find syntax \"{}\" to extend",
                shown(name)
            ));
        };
        let (keyword, rest) = word(rest);
        self.for_syntax(spot, index, keyword, rest)
    }

    /// A command that belongs to the syntax at `index`.
    fn for_syntax(
        &mut self,
        spot: Spot,
        index: usize,
        keyword: &str,
        rest: &str,
    ) -> Result<(), String> {
        match keyword {
            "header" => {
                if self.options.syntaxes[index].name == "default" {
                    return Err("The \"default\" syntax does not accept 'header' regexes".into());
                }
                if rest.is_empty() {
                    return Err("Missing regex string after 'header' command".into());
                }
                self.matching(spot, index, true, rest);
                Ok(())
            }
            "color" | "icolor" => {
                let (rules, error) = syntax::parse_rules(rest, keyword == "icolor");
                for rule in rules {
                    self.rule_cost +=
                        64 + rule.start.len() + rule.end.as_ref().map_or(0, String::len);
                    if self.rule_cost > MAX_RULE_COST
                        || self.options.syntaxes[index].rules.len() >= MAX_RULES
                    {
                        return Err("Too many color rules".into());
                    }
                    self.options.syntaxes[index].rules.push(rule);
                }
                error.map_or(Ok(()), Err)
            }
            // Commands that need more of the editor than there is (the key that comments lines,
            // the linter, the formatter, the tab key's replacement text).
            "comment" | "tabgives" | "linter" | "formatter" => {
                argument_of(keyword, rest)?;
                self.command_not_yet(keyword)
            }
            "magic" => Ok(()),
            _ => Err(format!("Command \"{}\" not understood", shown(keyword))),
        }
    }

    /// The file name regexes (or, with `header`, the first line ones) of the syntax at `index`,
    /// from the text of the command after it. Each mistake is a message of its own.
    fn matching(&mut self, spot: Spot, index: usize, header: bool, rest: &str) {
        let (found, mut errors) =
            syntax::regex_list(rest, MAX_MATCHERS.saturating_sub(self.matchers));
        self.matchers += found.len();
        let s = &mut self.options.syntaxes[index];
        let list = if header { &mut s.headers } else { &mut s.files };
        list.extend(found);
        for e in errors.drain(..) {
            self.error(spot, &e);
        }
    }

    /// A command nano has and the editor does not yet: said once.
    fn command_not_yet(&mut self, name: &str) -> Result<(), String> {
        if self.later.iter().any(|n| n == name) {
            return Ok(());
        }
        self.later.push(name.to_string());
        Err(format!("Option \"{name}\" is not available yet"))
    }

    /// `bind KEY FUNCTION MENU` and `unbind KEY MENU`.
    fn bind(&mut self, bind: bool, rest: &str) -> Result<(), String> {
        if rest.is_empty() {
            return Err("Missing key name".into());
        }
        let (key, rest) = word(rest);
        let (target, rest) = match rest.strip_prefix('"') {
            _ if !bind => (None, rest),
            Some(body) => match body.find('"') {
                Some(end) => (
                    Some(bindings::Target::Text(&body[..end])),
                    body[end + 1..].trim_start_matches(is_blank),
                ),
                None => {
                    return Err(format!("Argument '{}' has an unterminated \"", shown(rest)));
                }
            },
            None => {
                let (function, after) = word(rest);
                let target =
                    (!function.is_empty()).then_some(bindings::Target::Function(shown(function)));
                (target, after)
            }
        };
        let menu = word(rest).0;
        let menu = (!menu.is_empty()).then_some(shown(menu));
        let ops = if bind {
            bindings::bind(shown(key), target, menu)?
        } else {
            bindings::unbind(shown(key), menu)?
        };
        if self.options.binds.len() + ops.len() > MAX_BINDS {
            return Err("Too many key bindings".into());
        }
        self.options.binds.extend(ops);
        Ok(())
    }

    fn set_option(&mut self, spot: Spot, set: bool, rest: &str, valid: bool) -> Result<(), String> {
        if rest.is_empty() {
            return Err("Missing option".into());
        }
        let (name, rest) = word(rest);
        if let Some(flag) = flag(&mut self.options, name) {
            *flag = set;
            return Ok(());
        }
        if IGNORED_FLAGS.contains(&name) {
            return Ok(());
        }
        if LATER_FLAGS.contains(&name) {
            return self.not_yet(set, name);
        }
        if !ARGUMENT_OPTIONS.contains(&name) {
            return Err(format!("Unknown option: {}", shown(name)));
        }
        if !set {
            return Err(format!("Cannot unset option \"{name}\""));
        }
        if rest.is_empty() {
            return Err(format!("Option \"{name}\" requires an argument"));
        }
        let (arg, unterminated) = argument(rest);
        if unterminated {
            self.error(
                spot,
                &format!("Argument '{}' has an unterminated \"", shown(rest)),
            );
        }
        if !valid {
            return Err("Argument is not a valid multibyte string".into());
        }
        let o = &mut self.options;
        match name {
            "tabsize" => {
                // An invalid size gives back the default, not the one set before.
                o.tabsize = Options::default().tabsize;
                o.tabsize = parse_num(arg)
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("Requested tab size \"{}\" is invalid", shown(arg)))?;
            }
            "fill" => {
                parse_num(arg)
                    .ok_or_else(|| format!("Requested fill size \"{}\" is invalid", shown(arg)))?;
            }
            "backupdir" => o.backupdir = Some(PathBuf::from(arg)),
            "wordchars" => o.wordchars = arg.to_string(),
            "speller" => {}
            name if colours::NAMES.contains(&name) => {
                // As in nano, a colour that is not valid leaves the default one.
                let slot = o.colours.setting(name);
                if let Some(slot) = slot {
                    *slot = Some(syntax::parse_hue(arg)?);
                }
            }
            _ => return self.not_yet(set, name),
        }
        Ok(())
    }

    /// An option nano has and the editor does not. Only setting it is a mistake: it is off by
    /// default. Said once.
    fn not_yet(&mut self, set: bool, name: &str) -> Result<(), String> {
        if !set || self.later.iter().any(|n| n == name) {
            return Ok(());
        }
        self.later.push(name.to_string());
        Err(format!("Option \"{name}\" is not available yet"))
    }

    fn include(&mut self, spot: Spot, rest: &str) {
        let (arg, unterminated) = argument(rest);
        if unterminated {
            self.error(
                spot,
                &format!("Argument '{}' has an unterminated \"", shown(rest)),
            );
        }
        let pattern = match (&self.home, arg.strip_prefix('~')) {
            (Some(home), Some(tail)) if tail.is_empty() || tail.starts_with('/') => {
                format!("{}{tail}", home.display())
            }
            _ => arg.to_string(),
        };
        // No match: the pattern is tried as a name, which gives nano's message for it.
        let mut paths = glob(&pattern);
        if paths.is_empty() {
            paths.push(pattern);
        }
        for path in paths {
            if self.files_left == 0 {
                if !self.too_many_files {
                    self.too_many_files = true;
                    self.error(spot, "Too many included files");
                }
                return;
            }
            self.files_left -= 1;
            match read_limited(&path) {
                Ok((bytes, cut)) => {
                    self.parse(&path, &bytes, true);
                    if cut {
                        self.error(spot, &too_big(&path));
                    }
                }
                Err(what) => self.error(spot, &what),
            }
        }
    }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// The first word of `s` (up to a blank), and what follows the blanks after it.
fn word(s: &str) -> (&str, &str) {
    let end = s.find(is_blank).unwrap_or(s.len());
    (&s[..end], s[end..].trim_start_matches(is_blank))
}

/// An argument as nano reads it: a word, or after a double quote everything up to the last
/// double quote of the line (so it may hold blanks and quotes, and what follows that quote is
/// ignored). `true` when the quote is not closed: the rest of the line is then the argument.
fn argument(s: &str) -> (&str, bool) {
    let Some(body) = s.strip_prefix('"') else {
        return (word(s).0, false);
    };
    match body.rfind('"') {
        Some(end) => (&body[..end], false),
        None => (body, true),
    }
}

/// The argument of `comment`, `tabgives`, `linter` or `formatter`: the rest of the line, or what
/// is between the first double quote and the last one. Only the mistakes of nano are looked for.
fn argument_of(keyword: &str, rest: &str) -> Result<(), String> {
    if rest.is_empty() {
        return Err(format!("Missing argument after '{keyword}'"));
    }
    if rest.starts_with('"') && !rest[1..].contains('"') {
        return Err(format!("Argument of '{keyword}' lacks closing \""));
    }
    Ok(())
}

/// `s`, cut short for a message when it is very long (it comes from an untrusted file).
pub fn shown(s: &str) -> &str {
    let mut end = s.len().min(MAX_SHOWN);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn too_big(path: &str) -> String {
    format!(
        "{} is too big: only its first {MAX_FILE} bytes are read",
        shown(path)
    )
}

/// The first `MAX_FILE` bytes of a regular file, cut after a whole line, and whether it was cut;
/// Err is the message, as nano words it.
fn read_limited(path: &str) -> Result<(Vec<u8>, bool), String> {
    let reading = |e: io::Error| {
        // The system's words for the error, as nano prints them.
        let text = e.to_string();
        let text = text.split(" (os error").next().unwrap_or_default();
        format!("Error reading {}: {text}", shown(path))
    };
    // Looked at before opening: opening a FIFO would wait for a writer.
    let kind = fs::metadata(path).map_err(reading)?.file_type();
    if kind.is_dir() {
        return Err(format!("\"{}\" is a directory", shown(path)));
    }
    if !kind.is_file() {
        return Err(format!("\"{}\" is not a regular file", shown(path)));
    }
    let mut bytes = Vec::new();
    let file = fs::File::open(path).map_err(reading)?;
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(reading)?;
    let cut = bytes.len() as u64 > MAX_FILE;
    if cut {
        bytes.truncate(bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1));
    }
    Ok((bytes, cut))
}

/// The names that a pattern with `*`, `?` or `[...]` stands for, in alphabetical order. Empty when
/// the pattern has none of those, or nothing matches. Like the shell, `*` and `?` do not match a
/// dot at the start of a name.
fn glob(pattern: &str) -> Vec<String> {
    if !pattern.contains(['*', '?', '[']) || pattern.len() > 4096 {
        return Vec::new();
    }
    let mut paths = vec![if pattern.starts_with('/') { "/" } else { "" }.to_string()];
    for part in pattern.split('/').filter(|p| !p.is_empty()) {
        let mut next = Vec::new();
        for dir in &paths {
            if !part.contains(['*', '?', '[']) {
                let path = join(dir, part);
                if Path::new(&path).exists() {
                    next.push(path);
                }
                continue;
            }
            let pat: Vec<char> = part.chars().collect();
            let Ok(entries) = fs::read_dir(if dir.is_empty() { "." } else { dir }) else {
                continue;
            };
            for entry in entries.flatten().take(100_000) {
                let name = entry.file_name().to_string_lossy().into_owned();
                let text: Vec<char> = name.chars().collect();
                if (!name.starts_with('.') || part.starts_with('.')) && matches(&pat, &text) {
                    next.push(join(dir, &name));
                }
            }
        }
        next.sort();
        next.truncate(MAX_MATCHES);
        paths = next;
    }
    paths
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether the name fits the pattern, with `*`, `?` and `[abc]`, `[a-z]`, `[!a]` or `[^a]`. The
/// `*` is matched by going back to the last one, so the time is never more than the product of
/// the lengths.
fn matches(pat: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        let step = match pat.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
                continue;
            }
            Some('?') => Some(p + 1),
            Some('[') => match class(pat, p) {
                Some((set, negated, next)) => (in_class(set, name[n]) != negated).then_some(next),
                None => (name[n] == '[').then_some(p + 1),
            },
            Some(&c) => (c == name[n]).then_some(p + 1),
            None => None,
        };
        match (step, star) {
            (Some(next), _) => (p, n) = (next, n + 1),
            (None, Some((star_p, star_n))) => {
                (p, n) = (star_p + 1, star_n + 1);
                star = Some((star_p, star_n + 1));
            }
            (None, None) => return false,
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

/// The bracket expression at `pat[at]`: its characters, whether it is negated, and where the
/// pattern goes on after it. None when it is never closed (the `[` is then an ordinary one).
fn class(pat: &[char], at: usize) -> Option<(&[char], bool, usize)> {
    let negated = matches!(pat.get(at + 1), Some('!' | '^'));
    let start = at + 1 + usize::from(negated);
    // A `]` right after the opening is one of the characters.
    let end = start + 1 + pat.get(start + 1..)?.iter().position(|&c| c == ']')?;
    Some((&pat[start..end], negated, end + 1))
}

fn in_class(set: &[char], c: char) -> bool {
    let mut i = 0;
    while i < set.len() {
        if set.get(i + 1) == Some(&'-') && i + 2 < set.len() {
            if set[i] <= c && c <= set[i + 2] {
                return true;
            }
            i += 3;
        } else {
            if set[i] == c {
                return true;
            }
            i += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colours::Colours;
    use crate::syntax::Hue;

    /// What the text sets, and the messages.
    fn read(text: &str) -> (Options, Vec<String>) {
        let mut rc = Reader::new(Options::default(), Some(PathBuf::from("/home/u")));
        rc.parse("rc", text.as_bytes(), false);
        (rc.options, rc.messages)
    }

    fn mistakes(text: &str) -> Vec<String> {
        read(text).1
    }

    #[test]
    fn the_colours_of_the_interface() {
        let (o, m) = read(
            "set titlecolor bold,white,red\nset statuscolor ,blue\nset keycolor \"lightgreen\"\n\
             set functioncolor #f00\nset numbercolor italic,cyan\nset promptcolor yellow\n\
             set selectedcolor black,white\nset errorcolor normal\nset spotlightcolor magenta,lightcyan\n",
        );
        assert!(m.is_empty(), "{m:?}");
        let c = o.colours;
        let hue = |fg, bg, bold, italic| {
            Some(Hue {
                fg,
                bg,
                bold,
                italic,
            })
        };
        assert_eq!(c.title, hue(Some(7), Some(1), true, false));
        assert_eq!(c.status, hue(None, Some(4), false, false));
        assert_eq!(c.key, hue(Some(10), None, false, false));
        assert_eq!(c.function, hue(Some(196), None, false, false));
        assert_eq!(c.number, hue(Some(6), None, false, true));
        assert_eq!(c.prompt, hue(Some(3), None, false, false));
        assert_eq!(c.selected, hue(Some(0), Some(7), false, false));
        assert_eq!(c.error, hue(None, None, false, false));
        assert_eq!(c.spotlight, hue(Some(5), Some(14), false, false));
        // What is not set is as nano has it.
        assert_eq!(Options::default().colours, Colours::default());
    }

    #[test]
    fn a_colour_that_is_wrong_is_said_and_leaves_the_default() {
        let (o, m) = read(
            "set titlecolor nosuch\nset errorcolor red,\nset statuscolor lightnormal\n\
             set keycolor\nunset functioncolor\nset stripecolor red\n",
        );
        let said = [
            "Error in rc on line 1: Color \"nosuch\" not understood",
            "Error in rc on line 2: Color \"\" not understood",
            "Error in rc on line 3: Color 'normal' takes no prefix",
            "Error in rc on line 4: Option \"keycolor\" requires an argument",
            "Error in rc on line 5: Cannot unset option \"functioncolor\"",
            "Error in rc on line 6: Option \"stripecolor\" is not available yet",
        ];
        assert_eq!(m, said);
        // "red," is a background that is not given: nothing of the line is used.
        assert_eq!(o.colours.error, Colours::default().error);
        assert_eq!(o.colours.title, Colours::default().title);
        assert_eq!(o.colours.status, Colours::default().status);
    }

    #[test]
    fn set_and_unset() {
        let (o, m) = read(
            "# a comment\n\n  set linenumbers  \nset\ttabsize\t4 # not a comment\n\
             set wordchars \"-_ \"\nset nowrap\nunset mouse\nset linenumbers\nunset linenumbers\n",
        );
        assert!(m.is_empty(), "{m:?}");
        assert!(!o.linenumbers);
        assert_eq!((o.tabsize, o.wordchars.as_str()), (4, "-_ "));
    }

    #[test]
    fn quotes_end_at_the_last_quote_of_the_line() {
        assert_eq!(argument("\"a b\" c"), ("a b", false));
        assert_eq!(argument("\"a\" \"b\""), ("a\" \"b", false));
        assert_eq!(argument("\"5\"x"), ("5", false));
        assert_eq!(argument("a\"b c"), ("a\"b", false));
        assert_eq!(argument("\"open"), ("open", true));
        assert_eq!(argument("\""), ("", true));
    }

    #[test]
    fn mistakes_are_nanos_and_reading_goes_on() {
        let m = mistakes(
            "frob\nset\nset bogus\nset tabsize\nset tabsize 0\nset tabsize \"4\nunset tabsize\n\
             set fill x\nset minibar\ncolor red \"x\"\nsyntax\ninclude\nset linenumbers\n",
        );
        let rest = [
            "Command \"frob\" not understood",
            "Missing option",
            "Unknown option: bogus",
            "Option \"tabsize\" requires an argument",
            "Requested tab size \"0\" is invalid",
            "Argument '\"4' has an unterminated \"",
            "Cannot unset option \"tabsize\"",
            "Requested fill size \"x\" is invalid",
            "Option \"minibar\" is not available yet",
            "A 'color' command requires a preceding 'syntax' command",
            "Missing syntax name",
            "Error reading : No such file or directory",
        ];
        let want: Vec<String> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
            .iter()
            .zip(rest)
            .map(|(n, what)| format!("Error in rc on line {n}: {what}"))
            .collect();
        assert_eq!(m, want);
    }

    #[test]
    fn syntax_commands_are_read() {
        let (o, m) = read(
            "syntax \"x\" \"\\.x$\"\nheader \"^#!\"\ncolor red \"a\"\nicolor blue \"b\"\n\
             magic \"text\"\nextendsyntax x color red \"c\"\nbind ^X exit main\n\
             unbind M-x main\nset autoindent\n",
        );
        assert!(m.is_empty(), "{m:?}");
        assert!(o.autoindent);
    }

    #[test]
    fn bind_lines_are_read_in_nanos_words() {
        let (o, m) = read(
            "bind ^X cut main\nbind ^1 cut main\nbind ^X nosuch main\nunbind ^X\n\
             bind ^X \"ab main\nbind ^X \"ab\" main\nunbind F5 all\nbind ^X zap main\n",
        );
        assert_eq!(o.binds.len(), 1 + 1 + 9);
        let want = [
            "Key name ^1 is invalid",
            "Unknown function: nosuch",
            "Must specify a menu (or \"all\") in which to bind/unbind the key",
            "Argument '\"ab main' has an unterminated \"",
            "Function 'zap' is not available in this version yet",
        ];
        let want: Vec<String> = [2, 3, 4, 5, 8]
            .iter()
            .zip(want)
            .map(|(n, what)| format!("Error in rc on line {n}: {what}"))
            .collect();
        assert_eq!(m, want);
    }

    #[test]
    fn thousands_of_bind_lines_are_bounded() {
        let mut rc = Reader::new(Options::default(), None);
        let long = "x".repeat(10_000);
        let text =
            format!("bind {long} cut main\nbind ^X {long} main\nunbind ^X {long}\n").repeat(3000);
        rc.parse("rc", text.as_bytes(), false);
        assert_eq!(rc.messages.len(), 50);
        assert!(rc.messages.iter().all(|m| m.len() < 300));
        let ok = "unbind M-a all\nbind F5 undo main\n".repeat(20_000);
        rc.parse("rc", ok.as_bytes(), false);
        assert!(rc.options.binds.len() <= MAX_BINDS);
    }

    #[test]
    fn hostile_lines() {
        // A NUL ends a line, a CR before the newline is not part of it, bytes that are not UTF-8
        // are no argument, and a very long word is cut in the message.
        let mut rc = Reader::new(Options::default(), None);
        let long = format!("{}\n", "x".repeat(10_000));
        let text = [
            b"set linenumbers\0 junk\r\nset tabsize \"\xff\"\n".as_slice(),
            long.as_bytes(),
        ]
        .concat();
        rc.parse("rc", &text, false);
        assert!(rc.options.linenumbers);
        assert_eq!(rc.messages.len(), 2);
        assert!(rc.messages[0].ends_with("Argument is not a valid multibyte string"));
        assert!(rc.messages[1].len() < 200);
    }

    #[test]
    fn too_many_mistakes_are_not_shown_but_the_reading_goes_on() {
        let text = "frob\n".repeat(1000) + "set linenumbers\n";
        let mut rc = Reader::new(Options::default(), None);
        rc.parse("rc", text.as_bytes(), false);
        assert_eq!(rc.messages.len(), MAX_MESSAGES);
        assert!(rc.options.linenumbers);
    }

    #[test]
    fn an_invalid_value_gives_back_the_default() {
        let (o, m) = read("set tabsize 3\nset tabsize x\n");
        assert_eq!((o.tabsize, m.len()), (8, 1));
        let (o, _) = read("set tabsize 3\nset tabsize \"3\" \"x\"\n");
        assert_eq!(o.tabsize, 8);
        let (o, m) = read("set tabsize 3\nset tabsize 5\n");
        assert_eq!((o.tabsize, m.len()), (5, 0));
    }

    #[test]
    fn search_and_word_options_work() {
        let (o, m) = read("set casesensitive\nset regexp\nset afterends\nunset regexp\n");
        assert!(m.is_empty(), "{m:?}");
        assert!(o.casesensitive && o.afterends && !o.regexp);
    }

    #[test]
    fn options_not_available_yet_are_said_once() {
        let m = mistakes(
            "set showcursor\nset showcursor\nunset minibar\nset stateflags\nset scrollercolor red\n\
             set quotestr \"> \"\nset linenumbers\n",
        );
        let said: Vec<String> = ["showcursor", "stateflags", "scrollercolor", "quotestr"]
            .iter()
            .zip([1, 4, 5, 6])
            .map(|(name, n)| {
                format!("Error in rc on line {n}: Option \"{name}\" is not available yet")
            })
            .collect();
        assert_eq!(m, said);
    }

    #[test]
    fn a_file_over_the_limit_is_cut_after_a_line_and_said() {
        let dir = std::env::temp_dir().join(format!("nyra-rc-big-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.rc");
        let line = format!("# {}\n", "c".repeat(98));
        let text = line.repeat(MAX_FILE as usize / line.len() + 2) + "set linenumbers\n";
        fs::write(&file, text).unwrap();
        let (bytes, cut) = read_limited(file.to_str().unwrap()).unwrap();
        assert!(cut && bytes.len() as u64 <= MAX_FILE && bytes.ends_with(b"\n"));
        let mut rc = Reader::new(Options::default(), None);
        let spot = Spot {
            file: "rc",
            line: 1,
        };
        rc.include(spot, &format!("\"{}\"", file.display()));
        assert_eq!(rc.messages.len(), 1);
        assert!(rc.messages[0].contains("is too big"), "{:?}", rc.messages);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_home_of_a_user_is_in_the_password_file() {
        let passwd = "r:x:0:0:r:/rhome:/sh\nbad\nu:x:1000:1000:U,,,:/uhome:/sh\n";
        assert_eq!(passwd_home(passwd, 0), Some("/rhome"));
        assert_eq!(passwd_home(passwd, 1000), Some("/uhome"));
        assert_eq!(passwd_home(passwd, 7), None);
    }

    #[test]
    fn included_files_may_only_have_syntax_commands() {
        let dir = std::env::temp_dir().join(format!("nyra-rc-{}", std::process::id()));
        fs::create_dir_all(dir.join("d")).unwrap();
        fs::write(
            dir.join("d/a.rc"),
            "set linenumbers\nsyntax x\ncolor red \"a\"\n",
        )
        .unwrap();
        fs::write(dir.join("d/b.rc"), "include \"x\"\n").unwrap();
        fs::write(dir.join("d/.hidden"), "frob\n").unwrap();
        fs::create_dir(dir.join("d/sub.rc")).unwrap();
        let top = dir.display();
        let mut rc = Reader::new(Options::default(), None);
        rc.parse(
            "rc",
            format!("include \"{top}/d/*.rc\"\ninclude \"{top}/d/nope\"\n").as_bytes(),
            false,
        );
        let a = format!("{top}/d/a.rc");
        let b = format!("{top}/d/b.rc");
        let sub = format!("{top}/d/sub.rc");
        assert_eq!(
            rc.messages,
            [
                format!("Error in {a} on line 1: Command \"set\" not allowed in included file"),
                format!("Error in {b} on line 1: Command \"include\" not allowed in included file"),
                format!("Error in rc on line 1: \"{sub}\" is a directory"),
                format!(
                    "Error in rc on line 2: Error reading {top}/d/nope: No such file or directory"
                ),
            ]
        );
        assert_eq!(rc.first_file.as_deref(), Some(a.as_str()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn privileged_editor_ignores_the_environment() {
        let home = Some(OsString::from("/caller"));
        let xdg = Some(PathBuf::from("/caller/xdg"));
        let normal = environment_dirs(false, home.clone(), xdg.clone());
        assert_eq!(normal, (home.clone(), xdg.clone()));
        // Nothing is taken from the environment, so the home comes from the passwd entry and
        // the file is `<that home>/.config/nano/nanorc`, never one under XDG_CONFIG_HOME.
        let (env_home, env_xdg) = environment_dirs(true, home, xdg);
        assert_eq!((env_home, env_xdg.clone()), (None, None));
        let passwd_home = Path::new("/rhome");
        assert_eq!(
            user_file(Some(passwd_home), env_xdg.as_deref()),
            Some(passwd_home.join(".config/nano/nanorc"))
        );
    }

    #[test]
    fn user_file_order() {
        let dir = std::env::temp_dir().join(format!("nyra-rc-user-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (home, xdg) = (dir.as_path(), Some(Path::new("/xdg")));
        assert_eq!(
            user_file(Some(home), xdg),
            Some(Path::new("/xdg/nano/nanorc").into())
        );
        assert_eq!(
            user_file(Some(home), None),
            Some(dir.join(".config/nano/nanorc"))
        );
        fs::write(dir.join(".nanorc"), "").unwrap();
        assert_eq!(user_file(Some(home), xdg), Some(dir.join(".nanorc")));
        assert_eq!(user_file(None, None), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn syntaxes_are_kept_in_order() {
        let (o, m) = read(
            "syntax sh \"\\.sh$\" \"\\.bash$\"\nheader \"^#!\"\ncolor red \"a\" \"b\"\n\
             icolor blue start=\"x\" end=\"y\"\n\
             syntax other\ncolor green \"c\"\nextendsyntax sh color brightred \"d\"\n",
        );
        assert!(m.is_empty(), "{m:?}");
        assert_eq!(o.syntaxes.len(), 2);
        let sh = &o.syntaxes[0];
        assert_eq!(
            (sh.name.as_str(), sh.files.len(), sh.headers.len()),
            ("sh", 2, 1)
        );
        let starts: Vec<&str> = sh.rules.iter().map(|r| r.start.as_str()).collect();
        assert_eq!(starts, ["a", "b", "x", "d"]);
        assert!(sh.rules[2].icase && sh.rules[2].end.as_deref() == Some("y"));
        assert_eq!(o.syntaxes[1].name, "other");
    }

    #[test]
    fn syntax_mistakes_are_nanos() {
        let m = mistakes(
            "syntax\nsyntax none\nsyntax \"x\nsyntax default \"x\"\nsyntax empty\nset linenumbers\n\
             extendsyntax nothere color red \"x\"\nheader x\nsyntax ok \"x\"\ncolor red \"(\"\n\
             color nocolour \"x\"\ncolor red\ncolor red start=\"x\"\n",
        );
        let first = [
            "Missing syntax name",
            "The \"none\" syntax is reserved",
            "Unpaired quote in syntax name",
            "The \"default\" syntax does not accept extensions",
            "Syntax \"default\" has no color commands",
            "Syntax \"empty\" has no color commands",
            "Could not find syntax \"nothere\" to extend",
            "A 'header' command requires a preceding 'syntax' command",
        ];
        assert_eq!(m.len(), 12, "{m:?}");
        for (got, what) in m.iter().zip(first) {
            assert!(got.ends_with(what), "{got}");
        }
        let last = [
            "Bad regex \"(\": Unmatched ( or \\(",
            "Color \"nocolour\" not understood",
            "Missing regex string after 'color' command",
            "\"start=\" requires a corresponding \"end=\"",
        ];
        for (got, what) in m[8..].iter().zip(last) {
            assert!(got.ends_with(what), "{got}");
        }
    }

    #[test]
    fn comment_tabgives_linter_and_formatter_are_said_once() {
        let m = mistakes(
            "syntax x \"x\"\ncolor red \"x\"\ncomment \"#\"\ntabgives \"  \"\nlinter a\nlinter b\n\
             formatter c\ncomment \"//\"\n",
        );
        assert_eq!(m.len(), 4, "{m:?}");
        assert!(m[0].ends_with("Option \"comment\" is not available yet"));
        assert!(m[1].ends_with("Option \"tabgives\" is not available yet"));
    }

    #[test]
    fn syntaxes_are_bounded() {
        let many = "syntax s \"x\"\ncolor red \"x\"\n".repeat(MAX_SYNTAXES + 5);
        let (o, m) = read(&many);
        assert_eq!(o.syntaxes.len(), MAX_SYNTAXES);
        assert!(m.iter().any(|e| e.ends_with("Too many syntaxes")), "{m:?}");
        let rules = "syntax s \"x\"\n".to_string() + &"color red \"x\"\n".repeat(200_000);
        let (o, m) = read(&rules);
        assert_eq!(o.syntaxes[0].rules.len(), MAX_RULES);
        assert!(
            m.iter().any(|e| e.ends_with("Too many color rules")),
            "{m:?}"
        );
    }

    #[test]
    fn the_example_syntax_works() {
        use crate::buffer::{Buffer, Format};
        let (o, m) = read(include_str!("../docs/sh.nanorc"));
        assert!(m.is_empty(), "{m:?}");
        let script = "#!/bin/sh\nif [ \"$x\" ]; then echo 'a'; fi\n";
        let def = syntax::find(&o.syntaxes, "run.sh", "").expect("by name");
        assert_eq!(def.name, "sh");
        assert!(syntax::find(&o.syntaxes, "run", "#!/usr/bin/env bash").is_some());
        assert!(syntax::find(&o.syntaxes, "run", "text").is_none());
        let (rules, errors) = syntax::compile(def);
        assert!(errors.is_empty(), "{errors:?}");
        let buf = Buffer::new(script, Format::Unix);
        let hues = syntax::paint_within(&rules, &buf, 0, 1, std::time::Duration::from_secs(3600));
        let coloured = |line: usize, at: usize| hues[line][at].fg;
        assert!(coloured(0, 0).is_some(), "the comment line");
        assert!(coloured(1, 0).is_some(), "the keyword `if`");
        assert!(
            coloured(1, 5).is_some() || coloured(1, 6).is_some(),
            "the quoted variable"
        );
    }

    #[test]
    fn globs() {
        let m = |p: &str, n: &str| {
            matches(
                &p.chars().collect::<Vec<_>>(),
                &n.chars().collect::<Vec<_>>(),
            )
        };
        assert!(m("*.rc", "a.rc") && m("a?c", "abc") && m("*a*b*", "xaxxbx") && m("*", ""));
        assert!(m("[a-c]x", "bx") && !m("[a-c]x", "dx") && m("[!a]x", "bx") && m("[]]x", "]x"));
        assert!(m("[x", "[x") && !m("*.rc", "a.rcx") && !m("?", ""));
        // No exponential search for a pattern made to be slow.
        let slow = "*a".repeat(30) + "b";
        assert!(!m(&slow, &"a".repeat(200)));
    }
}
