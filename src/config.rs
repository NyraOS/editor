// SPDX-License-Identifier: MIT
//! The command line, with the options of GNU nano 8.4, over what the nanorc files set (see
//! rcfile.rs). An option the editor cannot honour yet is refused ("not available yet") rather
//! than ignored, unless nano would show the same screen with it.

use crate::colours::Colours;
use crate::syntax::SyntaxDef;
use crate::{bindings, rcfile};
use std::path::PathBuf;

/// What the command line asks for.
#[derive(Debug, PartialEq)]
// Made once at start: boxing the options would only add a step for the reader.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    Edit(Options),
    Version,
    Usage,
    /// `-z`: print the names of the syntaxes and exit. `-z` reads the nanorc files only when `-I`
    /// did not come before it, as in nano, which acts on the options in order.
    ListSyntaxes {
        rcfiles: bool,
    },
    /// The names of the syntaxes of the nanorc files, the last defined first, for `-z`.
    Syntaxes(Vec<String>),
}

/// `+/text`: a search to make when the file is open.
#[derive(Clone, Debug, PartialEq)]
pub struct StartSearch {
    pub text: String,
    pub backwards: bool,
    pub case_sensitive: bool,
    pub regexp: bool,
}

/// A file named on the command line, with the `+` argument before it.
#[derive(Debug, PartialEq)]
pub struct Place {
    pub file: String,
    /// `+LINE,COLUMN`: where the cursor starts (1-based; negative counts from the end).
    pub start: Option<(i64, i64)>,
    /// `+/text` or `+?text`, with modifiers: a search to make instead of going to a place.
    pub search: Option<StartSearch>,
    /// What is wrong with the `+` argument: shown in the status bar, as nano does.
    pub alert: Option<String>,
}

impl Place {
    pub fn new(file: String) -> Self {
        Self {
            file,
            start: None,
            search: None,
            alert: None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Options {
    /// The files to open, one buffer each, in order; none: a new, unnamed buffer.
    pub files: Vec<Place>,
    /// What is wrong with the nanorc files: shown in the status bar once the files are open.
    pub problem: Option<String>,
    /// `-F`: reading a file with ^R makes a new buffer.
    pub multibuffer: bool,
    /// `-I`: the nanorc files are not read.
    pub ignorercfiles: bool,
    /// `-l`: numbers in front of the lines.
    pub linenumbers: bool,
    /// `-S`: long lines wrap at the edge of the screen.
    pub softwrap: bool,
    /// `-a`: with `-S`, wrap after a blank rather than in the middle of a word.
    pub atblanks: bool,
    /// `-c`: the cursor position always in the status bar.
    pub constantshow: bool,
    /// `-x`: no help lines at the bottom.
    pub nohelp: bool,
    /// `-L`: no newline added at the end of the file.
    pub nonewlines: bool,
    /// `-T N`: the width of a tab.
    pub tabsize: usize,
    /// `-A`: Home goes to the first non-blank character first.
    pub smarthome: bool,
    /// `-B`: a backup of the old file at every save.
    pub backup: bool,
    /// `-C DIR`: the backups go there.
    pub backupdir: Option<PathBuf>,
    /// `-E`: a typed Tab makes spaces.
    pub tabstospaces: bool,
    /// `-i`: a new line starts with the blanks of the one before.
    pub autoindent: bool,
    /// `-k`: ^K cuts from the cursor to the end of the line.
    pub cutfromcursor: bool,
    /// `-N`: DOS and Mac line ends are not converted.
    pub noconvert: bool,
    /// `-t`: ^X saves a modified buffer without asking.
    pub saveonexit: bool,
    /// `-u`: the file is saved in Unix format.
    pub unix: bool,
    /// `-Z`: Backspace and Delete erase the selection.
    pub zap: bool,
    /// `-W`: punctuation separates words for the word keys.
    pub wordbounds: bool,
    /// `-X CHARS`: besides letters and digits, these characters are part of words.
    pub wordchars: String,
    /// `-y`: the word keys stop at the ends of words instead of their beginnings.
    pub afterends: bool,
    /// nanorc `set casesensitive`: searches start case sensitive.
    pub casesensitive: bool,
    /// nanorc `set regexp`: searches start with regular expressions.
    pub regexp: bool,
    /// `-m`: the mouse places the cursor, presses the keys of the help lines and scrolls.
    pub mouse: bool,
    /// `-J COLUMN`: a stripe at this column (counted from 1), to judge the width of the text; 0:
    /// none.
    pub guidestripe: usize,
    /// `-q`: a scroll bar in the last column of the text.
    pub indicator: bool,
    /// `-Y NAME`: the syntax to use for every file, `none` for no syntax.
    pub syntax: Option<String>,
    /// The colours of the interface, set by the nanorc files.
    pub colours: Colours,
    /// `-/`: the keys of other editors (^X cuts, ^Q exits, ...), before the nanorc's `bind` lines.
    pub modernbindings: bool,
    /// The nanorc's `bind` and `unbind` lines, in order.
    pub binds: Vec<bindings::Op>,
    /// The syntaxes of the nanorc files, in the order they were defined.
    pub syntaxes: Vec<SyntaxDef>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            problem: None,
            multibuffer: false,
            ignorercfiles: false,
            linenumbers: false,
            softwrap: false,
            atblanks: false,
            constantshow: false,
            nohelp: false,
            nonewlines: false,
            tabsize: 8,
            smarthome: false,
            backup: false,
            backupdir: None,
            tabstospaces: false,
            autoindent: false,
            cutfromcursor: false,
            noconvert: false,
            saveonexit: false,
            unix: false,
            zap: false,
            wordbounds: false,
            wordchars: String::new(),
            afterends: false,
            casesensitive: false,
            regexp: false,
            mouse: false,
            guidestripe: 0,
            syntax: None,
            indicator: false,
            colours: Colours::default(),
            modernbindings: false,
            binds: Vec::new(),
            syntaxes: Vec::new(),
        }
    }
}

/// What the editor does with an option.
#[derive(Clone, Copy, PartialEq)]
enum Support {
    Works,
    /// Accepted; with what the editor has so far, nano would show the same screen.
    Ignored,
    /// Refused, until it is implemented.
    Later,
}
use Support::{Ignored, Later, Works};

struct Opt {
    short: char,
    long: &'static str,
    /// Takes an argument.
    arg: bool,
    support: Support,
}

const fn opt(short: char, long: &'static str, arg: bool, support: Support) -> Opt {
    Opt {
        short,
        long,
        arg,
        support,
    }
}

/// The options of nano 8.4, in nano's order: the order of the possibilities in the message for
/// an abbreviation that is ambiguous.
const OPTIONS: &[Opt] = &[
    opt('a', "atblanks", false, Works),
    opt('i', "autoindent", false, Works),
    opt('y', "afterends", false, Works),
    opt('D', "boldtext", false, Later),
    opt('b', "breaklonglines", false, Later),
    opt('B', "backup", false, Works),
    opt('C', "backupdir", true, Works),
    opt('O', "bookstyle", false, Ignored),
    opt('c', "constantshow", false, Works),
    opt('k', "cutfromcursor", false, Works),
    opt('@', "colonparsing", false, Later),
    opt('e', "emptyline", false, Later),
    opt('r', "fill", true, Ignored),
    opt('J', "guidestripe", true, Works),
    opt('h', "help", false, Works),
    opt('H', "historylog", false, Later),
    opt('I', "ignorercfiles", false, Works),
    opt('q', "indicator", false, Works),
    opt('j', "jumpyscrolling", false, Later),
    opt('l', "linenumbers", false, Works),
    opt('z', "listsyntaxes", false, Works),
    opt('G', "locking", false, Works),
    opt('F', "multibuffer", false, Works),
    opt('m', "mouse", false, Works),
    opt('/', "modernbindings", false, Works),
    opt('_', "minibar", false, Later),
    opt('w', "nowrap", false, Ignored),
    opt('x', "nohelp", false, Works),
    opt('L', "nonewlines", false, Works),
    opt('N', "noconvert", false, Works),
    opt('n', "noread", false, Later),
    opt('o', "operatingdir", true, Later),
    opt('p', "preserve", false, Later),
    opt('P', "positionlog", false, Later),
    opt('Q', "quotestr", true, Later),
    opt('U', "quickblank", false, Later),
    opt('K', "rawsequences", false, Later),
    opt('R', "restricted", false, Later),
    opt('d', "rebinddelete", false, Later),
    opt('f', "rcfile", true, Later),
    opt('Y', "syntax", true, Works),
    opt('g', "showcursor", false, Later),
    opt('s', "speller", true, Ignored),
    opt('t', "saveonexit", false, Works),
    opt('A', "smarthome", false, Works),
    opt('S', "softwrap", false, Works),
    opt('%', "stateflags", false, Later),
    opt('M', "trimblanks", false, Ignored),
    opt('E', "tabstospaces", false, Works),
    opt('T', "tabsize", true, Works),
    opt('u', "unix", false, Works),
    opt('V', "version", false, Works),
    opt('v', "view", false, Later),
    opt('W', "wordbounds", false, Works),
    opt('X', "wordchars", true, Works),
    opt('Z', "zap", false, Works),
    opt('0', "zero", false, Later),
];

/// What the arguments gave so far.
struct Parsing {
    options: Options,
    /// The operands: file names, and the `+` arguments in front of them.
    files: Vec<String>,
    /// The files to open, with their `+` arguments.
    places: Vec<Place>,
    /// The first option or argument that is not available yet.
    later: Option<String>,
}

/// Parses the arguments (without the program name), over the settings of the nanorc files unless
/// `-I` is given. Err is what to print before exiting with status 1.
pub fn parse(args: &[String]) -> Result<Command, String> {
    parse_with(args, |quiet| rcfile::load(Options::default(), quiet))
}

/// `read_rcfiles` gives the settings the arguments are applied to, and the status bar message
/// about the mistakes in the files, if any. The arguments are read once before it (to see `-I`
/// and to fail early on a bad one) and once over its result, so that they win over the files.
fn parse_with(
    args: &[String],
    read_rcfiles: impl FnOnce(bool) -> (Options, Option<String>),
) -> Result<Command, String> {
    let first = parse_over(args, Options::default())?;
    if let Command::ListSyntaxes { rcfiles } = first {
        let names = if rcfiles {
            // nano keeps the mistakes of the files for the screen, which `-z` never starts.
            read_rcfiles(true).0.syntaxes
        } else {
            Vec::new()
        };
        return Ok(Command::Syntaxes(
            names.into_iter().rev().map(|s| s.name).collect(),
        ));
    }
    let Command::Edit(first) = first else {
        return Ok(first);
    };
    if first.ignorercfiles {
        return Ok(Command::Edit(first));
    }
    let (base, mistakes) = read_rcfiles(false);
    match parse_over(args, base)? {
        Command::Edit(mut o) => {
            o.problem = mistakes;
            Ok(Command::Edit(o))
        }
        other => Ok(other),
    }
}

fn parse_over(args: &[String], base: Options) -> Result<Command, String> {
    let mut p = Parsing {
        options: base,
        files: Vec::new(),
        places: Vec::new(),
        later: None,
    };
    let mut args = args.iter();
    let mut only_files = false;
    while let Some(arg) = args.next() {
        if only_files || arg == "-" || !arg.starts_with('-') {
            p.files.push(arg.clone());
        } else if arg == "--" {
            only_files = true;
        } else if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            let found = find_long(name, arg)?;
            let value = match (found.arg, value) {
                (true, Some(v)) => v,
                (true, None) => args.next().map(String::as_str).ok_or_else(|| {
                    usage_error(format!("option '--{}' requires an argument", found.long))
                })?,
                (false, None) => "",
                (false, Some(_)) => {
                    return Err(usage_error(format!(
                        "option '--{}' doesn't allow an argument",
                        found.long
                    )));
                }
            };
            if let Some(done) = apply(&mut p, found, value)? {
                return Ok(done);
            }
        } else {
            // Short options, possibly grouped: -lc, -T4, -T 4.
            let mut chars = arg[1..].chars();
            while let Some(c) = chars.next() {
                let found = OPTIONS
                    .iter()
                    .find(|o| o.short == c)
                    .ok_or_else(|| usage_error(format!("invalid option -- '{c}'")))?;
                let rest: String = if found.arg {
                    chars.by_ref().collect()
                } else {
                    String::new()
                };
                let value = match (found.arg, rest.is_empty()) {
                    (false, _) => "",
                    (true, false) => &rest,
                    (true, true) => args.next().map(String::as_str).ok_or_else(|| {
                        usage_error(format!("option requires an argument -- '{c}'"))
                    })?,
                };
                if let Some(done) = apply(&mut p, found, value)? {
                    return Ok(done);
                }
            }
        }
    }
    place_arguments(&mut p);
    finish(p)
}

/// As in nano, a `+` argument is a place to start only when an argument follows it, and then
/// that argument is a file name, even if it starts with a plus; in last place it is a file name.
/// Each file has the place before it.
fn place_arguments(p: &mut Parsing) {
    let operands = std::mem::take(&mut p.files);
    let mut operands = operands.into_iter().peekable();
    // As in nano, the modifiers (and `?`) of a search stay on for the files after it.
    let mut flags = SearchFlags {
        case_sensitive: p.options.casesensitive,
        regexp: p.options.regexp,
        backwards: false,
    };
    while let Some(arg) = operands.next() {
        match arg.strip_prefix('+') {
            Some(spec) if operands.peek().is_some() => {
                let file = operands.next().unwrap_or_default();
                let mut place = Place::new(file);
                start_spec(&mut place, spec, &mut flags);
                p.places.push(place);
            }
            _ => p.places.push(Place::new(arg)),
        }
    }
}

/// The messages of getopt, as GNU nano prints them (with this program's name).
fn usage_error(what: String) -> String {
    format!("{PROGRAM}: {what}\nType '{PROGRAM} -h' for a list of available options.")
}

const PROGRAM: &str = "editor";

/// The most buffers open at once. Each one holds its text and its undo history, and a lock file
/// with two file descriptors (the usual limit is 1024 per process), and a hang-up writes every
/// modified one to disk; a few hundred keep the memory, the descriptors and that time bounded
/// whatever is typed or put on the command line (`editor *` in a big directory).
pub const MAX_BUFFERS: usize = 256;

/// The option `--name` stands for: an exact name, or the only one that starts with it. `arg` is
/// the whole argument, for the message.
fn find_long(name: &str, arg: &str) -> Result<&'static Opt, String> {
    if let Some(exact) = OPTIONS.iter().find(|o| o.long == name) {
        return Ok(exact);
    }
    let mut starts = OPTIONS.iter().filter(|o| o.long.starts_with(name));
    match (starts.next(), starts.next()) {
        (Some(only), None) if !name.is_empty() => Ok(only),
        (Some(first), Some(second)) if !name.is_empty() => {
            let all: Vec<String> = [first, second]
                .into_iter()
                .chain(starts)
                .map(|o| format!("'--{}'", o.long))
                .collect();
            let all = format!(" {}", all.join(" "));
            Err(usage_error(format!(
                "option '{arg}' is ambiguous; possibilities:{all}"
            )))
        }
        _ => Err(usage_error(format!("unrecognized option '{arg}'"))),
    }
}

/// Handles one option. Some(command) when it ends the parsing (--help, --version).
fn apply(p: &mut Parsing, opt: &Opt, value: &str) -> Result<Option<Command>, String> {
    let o = &mut p.options;
    match opt.support {
        Later => {
            p.later.get_or_insert_with(|| {
                format!(
                    "option -{} (--{}) is not available yet",
                    opt.short, opt.long
                )
            });
            return Ok(None);
        }
        Ignored => {
            if opt.long == "fill" {
                number(value, "Requested fill size")?;
            }
            return Ok(None);
        }
        Works => {}
    }
    match opt.long {
        "version" => return Ok(Some(Command::Version)),
        "listsyntaxes" => {
            return Ok(Some(Command::ListSyntaxes {
                rcfiles: !o.ignorercfiles,
            }));
        }
        "help" => return Ok(Some(Command::Usage)),
        "tabsize" => {
            o.tabsize = parse_num(value)
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("Requested tab size \"{value}\" is invalid"))?;
        }
        "backupdir" => o.backupdir = Some(PathBuf::from(value)),
        "wordchars" => o.wordchars = value.to_string(),
        "nonewlines" => o.nonewlines = true,
        "constantshow" => o.constantshow = true,
        "modernbindings" => o.modernbindings = true,
        "linenumbers" => o.linenumbers = true,
        "softwrap" => o.softwrap = true,
        "atblanks" => o.atblanks = true,
        "multibuffer" => o.multibuffer = true,
        "nohelp" => o.nohelp = true,
        "smarthome" => o.smarthome = true,
        "backup" => o.backup = true,
        "tabstospaces" => o.tabstospaces = true,
        "autoindent" => o.autoindent = true,
        "cutfromcursor" => o.cutfromcursor = true,
        "noconvert" => o.noconvert = true,
        "saveonexit" => o.saveonexit = true,
        "unix" => o.unix = true,
        "zap" => o.zap = true,
        "wordbounds" => o.wordbounds = true,
        "afterends" => o.afterends = true,
        "mouse" => o.mouse = true,
        "syntax" => o.syntax = Some(value.to_string()),
        "guidestripe" => o.guidestripe = guide_column(value)?,
        "indicator" => o.indicator = true,
        "ignorercfiles" => o.ignorercfiles = true,
        // Files are always locked.
        "locking" => {}
        other => unreachable!("option {other} is in the table as working, but not handled"),
    }
    Ok(None)
}

/// The column of the guide stripe, or the message for a bad one.
fn guide_column(value: &str) -> Result<usize, String> {
    parse_num(value)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("Guide column \"{value}\" is invalid"))
}

/// A number option's argument, or the message for a bad one.
fn number(value: &str, what: &str) -> Result<i64, String> {
    parse_num(value)
        .map(i64::from)
        .ok_or_else(|| format!("{what} \"{value}\" is invalid"))
}

/// A number as nano reads it (strtol): blanks, a sign and digits, and it must fit an int.
pub fn parse_num(value: &str) -> Option<i32> {
    value
        .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '\x0b')
        .parse()
        .ok()
}

/// What is left to check once all arguments are read.
fn finish(p: Parsing) -> Result<Command, String> {
    let mut o = p.options;
    if let Some(dir) = &o.backupdir {
        if !dir.is_dir() {
            return Err(format!("Invalid backup directory: {}", dir.display()));
        }
    }
    if let Some(what) = p.later {
        return Err(format!("{PROGRAM}: {what}"));
    }
    if p.places.iter().any(|place| place.file == "-") {
        return Err(format!(
            "{PROGRAM}: reading the text from the standard input is not available yet"
        ));
    }
    if p.places.len() > MAX_BUFFERS {
        return Err(format!(
            "{PROGRAM}: at most {MAX_BUFFERS} files can be opened at once"
        ));
    }
    o.files = p.places;
    Ok(Command::Edit(o))
}

/// How a search starts, which the `+` arguments change and leave changed, as nano does.
struct SearchFlags {
    case_sensitive: bool,
    regexp: bool,
    backwards: bool,
}

/// The argument `+SPEC`: `LINE`, `LINE,COLUMN`, `,COLUMN`, or a search: `/text` forwards, `?text`
/// backwards, after modifiers: `c` and `C` for case sensitive or not, `r` and `R` for a regular
/// expression or not.
fn start_spec(place: &mut Place, spec: &str, flags: &mut SearchFlags) {
    if spec.starts_with(|c: char| c.is_ascii_digit() || matches!(c, ',' | '-')) || spec.is_empty() {
        match position(spec) {
            Some(at) => place.start = Some(at),
            None => place.alert = Some("Invalid line or column number".into()),
        }
        return;
    }
    for (i, c) in spec.char_indices() {
        match c {
            'c' => flags.case_sensitive = true,
            'C' => flags.case_sensitive = false,
            'r' => flags.regexp = true,
            'R' => flags.regexp = false,
            '/' | '?' => {
                flags.backwards |= c == '?';
                let text = spec[i + 1..].to_string();
                if text.is_empty() {
                    place.alert = Some("Empty search string".into());
                } else {
                    place.search = Some(StartSearch {
                        text,
                        backwards: flags.backwards,
                        case_sensitive: flags.case_sensitive,
                        regexp: flags.regexp,
                    });
                }
                return;
            }
            _ => {
                place.alert = Some(format!("Invalid search modifier '{c}'"));
                return;
            }
        }
    }
}

/// `LINE`, `LINE,COLUMN` or `,COLUMN`.
fn position(s: &str) -> Option<(i64, i64)> {
    let (line, col) = s.split_once(',').unwrap_or((s, ""));
    let num = |t: &str, default| {
        if t.is_empty() {
            Some(default)
        } else {
            t.parse().ok()
        }
    };
    Some((num(line, 1)?, num(col, 1)?))
}

/// What `-z` prints: the names, a few to a line, as nano lays them out. Nothing when there are
/// none.
pub fn syntax_list(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut out = String::from("Available syntaxes:\n");
    let mut width = 0;
    for name in names {
        if width > 45 {
            out.push('\n');
            width = 0;
        }
        out.push(' ');
        out.push_str(name);
        width += crate::screen::text_width(name);
    }
    out.push('\n');
    out
}

pub const USAGE: &str = "\
Usage: editor [OPTIONS] [[+LINE[,COLUMN] | +/TEXT | +?TEXT] FILE]...

A text editor that works like GNU nano; it is not GNU nano.
Type ^G inside the editor for the keys. Short options can be joined (-lc, -T4), long ones
shortened while they stay unambiguous; -- ends the options.

Each file is opened in a buffer of its own; M-< and M-> switch between them.

Where to start in the file that follows:
 +LINE[,COLUMN]          Put the cursor there (a negative number counts from the end)
 +/TEXT, +?TEXT          Search forwards, or backwards, for TEXT; before the slash, the
                         modifiers c/C (case sensitive or not), r/R (regular expression or
                         not)

Options:
 -A, --smarthome         Home goes to the first non-blank character first
 -B, --backup            Keep a backup (FILE~) of the old file at every save
 -C, --backupdir=DIR     Put the backups, numbered, in DIR
 -E, --tabstospaces      Make a typed Tab write spaces
 -F, --multibuffer       Read a file with ^R into a new buffer
 -G, --locking           Lock the file while editing it (always on)
 -I, --ignorercfiles     Do not read nanorc files
 -L, --nonewlines        Do not add a newline at the end of the file
 -N, --noconvert         Do not convert DOS and Mac line ends
 -J, --guidestripe=N     Draw a stripe at column N, to judge the width of the text
 -T, --tabsize=N         Make a tab N columns wide (default 8)
 -W, --wordbounds        Let punctuation separate words
 -X, --wordchars=CHARS   Treat these characters as parts of words
 -Y, --syntax=NAME       Use the syntax NAME (or none) for every file
 -Z, --zap               Backspace and Delete erase the selection
 -c, --constantshow      Show the cursor position all the time
 -h, --help              Show this help and exit
 -i, --autoindent        Start a new line with the blanks of the one before
 -k, --cutfromcursor     Cut from the cursor to the end of the line
 -l, --linenumbers       Show line numbers in front of the text
 -q, --indicator         Show a scroll bar in the last column of the text
 -z, --listsyntaxes      List the names of the syntaxes and exit
 -_, --minibar           Show a bar at the bottom instead of the title bar
 -m, --mouse             Use the mouse: click to place the cursor or press a key of the help
                         lines, wheel to scroll
 -t, --saveonexit        Save a modified buffer on exit without asking
 -u, --unix              Save the file in Unix format
 -V, --version           Show the version and exit
 -/, --modernbindings    Use the keys of other editors (^X cuts, ^Q exits, ^V pastes, ...)
 -x, --nohelp            Do not show the two help lines

Accepted, with no effect yet: -M, -O, -a, -r, -s, -w.
Not available yet (refused): -D, -H, -K, -P, -Q, -R, -U, -b, -d, -e, -f, -g, -j,
-n, -o, -p, -v, -@, -%, -0, and - (standard input).
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::SyntaxDef;

    fn run(args: &[&str]) -> Result<Command, String> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_with(&args, |_| (Options::default(), None))
    }

    fn edit(args: &[&str]) -> Options {
        match run(args) {
            Ok(Command::Edit(o)) => o,
            other => panic!("{other:?}"),
        }
    }

    fn err(args: &[&str]) -> String {
        run(args).expect_err("an error")
    }

    /// The names of the files to open.
    fn names(o: &Options) -> Vec<&str> {
        o.files.iter().map(|p| p.file.as_str()).collect()
    }

    #[test]
    fn arguments_win_over_the_nanorc_files() {
        let rc = |_| {
            let base = Options {
                tabsize: 3,
                linenumbers: true,
                ..Options::default()
            };
            (base, Some("Mistakes in 'rc'".to_string()))
        };
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let Ok(Command::Edit(o)) = parse_with(&args(&["-T", "5", "f"]), rc) else {
            panic!("not an edit")
        };
        assert_eq!((o.tabsize, o.linenumbers), (5, true));
        assert_eq!(o.problem.as_deref(), Some("Mistakes in 'rc'"));
        let Ok(Command::Edit(o)) = parse_with(&args(&["+/", "f"]), rc) else {
            panic!("not an edit")
        };
        assert_eq!(o.tabsize, 3);
        assert_eq!(o.files[0].alert.as_deref(), Some("Empty search string"));
        assert_eq!(o.problem.as_deref(), Some("Mistakes in 'rc'"));
        let Ok(Command::Edit(o)) = parse_with(&args(&["-I", "f"]), rc) else {
            panic!("not an edit")
        };
        assert_eq!((o.tabsize, o.linenumbers), (8, false));
    }

    #[test]
    fn numbers_are_read_like_strtol() {
        assert_eq!(parse_num(" +4"), Some(4));
        assert_eq!(parse_num("-4"), Some(-4));
        for bad in ["", "4 ", "0x4", "4x", "99999999999"] {
            assert_eq!(parse_num(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn flags_and_file() {
        let o = edit(&["--ignorercfiles", "-lc", "+3,5", "notes.txt"]);
        assert!(o.linenumbers && o.constantshow && !o.nohelp);
        assert_eq!(o.files[0].start, Some((3, 5)));
        assert_eq!(o.files[0].file, "notes.txt");
        assert_eq!(edit(&["+,7", "f"]).files[0].start, Some((1, 7)));
        assert_eq!(names(&edit(&["-x", "f", "-L"])), ["f"]);
        assert!(edit(&["f", "-L"]).nonewlines);
    }

    #[test]
    fn short_options_joined_and_with_arguments() {
        let o = edit(&["-AEikNtuWZ"]);
        assert!(o.smarthome && o.tabstospaces && o.autoindent && o.cutfromcursor);
        assert!(o.noconvert && o.saveonexit && o.unix && o.wordbounds && o.zap);
        assert_eq!(edit(&["-T4"]).tabsize, 4);
        assert_eq!(edit(&["-T", "2"]).tabsize, 2);
        assert_eq!(edit(&["-lcT3"]).tabsize, 3);
        // As in getopt, what follows the letter is the argument.
        assert_eq!(err(&["-T3c"]), "Requested tab size \"3c\" is invalid");
        assert_eq!(edit(&["-X", "_-"]).wordchars, "_-");
        assert_eq!(edit(&["-X_"]).wordchars, "_");
    }

    #[test]
    fn long_options_exact_abbreviated_and_with_arguments() {
        assert_eq!(edit(&["--tabsize=3"]).tabsize, 3);
        assert_eq!(edit(&["--tabsize", "5"]).tabsize, 5);
        assert!(edit(&["--smart", "--auto", "--cutf"]).smarthome);
        // An exact name wins over longer ones that start with it.
        assert!(edit(&["--backup"]).backup);
        assert_eq!(edit(&["--backup"]).backupdir, None);
        assert_eq!(edit(&["--wordchars="]).wordchars, "");
        assert_eq!(edit(&["-C", "."]).backupdir, Some(PathBuf::from(".")));
    }

    #[test]
    fn double_dash_ends_the_options() {
        assert_eq!(names(&edit(&["--", "-l"])), ["-l"]);
        assert!(!edit(&["--", "-l"]).linenumbers);
        assert_eq!(edit(&["--", "+4", "f"]).files[0].start, Some((4, 1)));
    }

    #[test]
    fn help_and_version_end_the_parsing() {
        assert_eq!(run(&["--version"]), Ok(Command::Version));
        assert_eq!(run(&["-lV", "--bogus"]), Ok(Command::Version));
        assert_eq!(run(&["-h"]), Ok(Command::Usage));
        // As in nano, an option before them that is wrong is still reported first.
        assert!(err(&["--bogus", "-h"]).contains("unrecognized"));
        // One that is only refused for now is not.
        assert_eq!(run(&["-m", "--help"]), Ok(Command::Usage));
    }

    #[test]
    fn errors_are_nanos() {
        assert_eq!(
            err(&["-3"]),
            "editor: invalid option -- '3'\nType 'editor -h' for a list of available options."
        );
        assert!(err(&["--bogus=1"]).starts_with("editor: unrecognized option '--bogus=1'\n"));
        assert!(
            err(&["--backup=x"])
                .starts_with("editor: option '--backup' doesn't allow an argument\n")
        );
        assert!(
            err(&["--tabsize"]).starts_with("editor: option '--tabsize' requires an argument\n")
        );
        assert!(err(&["-lT"]).starts_with("editor: option requires an argument -- 'T'\n"));
        assert!(err(&["--tab=3"]).starts_with(
            "editor: option '--tab=3' is ambiguous; possibilities: '--tabstospaces' '--tabsize'\n"
        ));
        assert!(err(&["--no"]).contains(
            "possibilities: '--nowrap' '--nohelp' '--nonewlines' '--noconvert' '--noread'\n"
        ));
        assert!(err(&["--s"]).contains("'--syntax' '--showcursor' '--speller' '--saveonexit'"));
        assert_eq!(err(&["-T", "x"]), "Requested tab size \"x\" is invalid");
        assert_eq!(err(&["-T0"]), "Requested tab size \"0\" is invalid");
        assert_eq!(err(&["-T", "-3"]), "Requested tab size \"-3\" is invalid");
        assert_eq!(err(&["-r", "x"]), "Requested fill size \"x\" is invalid");
        assert_eq!(err(&["-J", "0"]), "Guide column \"0\" is invalid");
        assert_eq!(
            err(&["-C", "/nonexistent-directory"]),
            "Invalid backup directory: /nonexistent-directory"
        );
    }

    #[test]
    fn fill_takes_any_number_and_is_ignored() {
        assert_eq!(
            edit(&["-r", "-5", "-r0", "-r72", "-M", "-O", "-w", "-s", "x"]),
            Options::default()
        );
    }

    #[test]
    fn what_is_not_available_yet_is_refused_not_ignored() {
        for args in [
            &["-D"][..],
            &["-b"],
            &["-%"],
            &["-_"],
            &["-0"],
            &["-lv"],
            &["--rebinddelete"],
        ] {
            assert!(err(args).ends_with("is not available yet"), "{args:?}");
        }
        assert_eq!(
            err(&["-D"]),
            "editor: option -D (--boldtext) is not available yet"
        );
        assert!(edit(&["-m"]).mouse && edit(&["--mouse"]).mouse && !edit(&[]).mouse);
        // The first one is the one named; a mistake in an option's argument comes before.
        assert!(err(&["-g", "-b"]).contains("--showcursor"));
        assert!(err(&["-b", "-T", "x"]).starts_with("Requested tab size"));
        assert!(edit(&["-/"]).modernbindings);
        assert!(err(&["-"]).contains("standard input"));
    }

    #[test]
    fn the_guide_stripe_the_scroll_bar_and_the_syntax() {
        let o = edit(&["-J", "7", "-q", "-Y", "sh"]);
        assert_eq!((o.guidestripe, o.indicator), (7, true));
        assert_eq!(o.syntax.as_deref(), Some("sh"));
        assert_eq!(edit(&["--guidestripe=3"]).guidestripe, 3);
        for bad in ["0", "-1", "x", ""] {
            assert_eq!(
                err(&["-J", bad]),
                format!("Guide column \"{bad}\" is invalid")
            );
        }
    }

    #[test]
    fn the_syntaxes_are_listed_last_defined_first() {
        let rc = |quiet: bool| {
            assert!(quiet, "-z keeps the mistakes of the files to itself");
            let def = |name: &str| SyntaxDef {
                name: name.into(),
                files: Vec::new(),
                headers: Vec::new(),
                rules: Vec::new(),
            };
            let base = Options {
                syntaxes: vec![def("a"), def("b"), def("a")],
                ..Options::default()
            };
            (base, None)
        };
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let Ok(Command::Syntaxes(names)) = parse_with(&args(&["-z", "--bogus"]), rc) else {
            panic!("not a list")
        };
        assert_eq!(names, ["a", "b", "a"]);
        assert_eq!(syntax_list(&names), "Available syntaxes:\n a b a\n");
        // -I before -z: the files are not read.
        let Ok(Command::Syntaxes(names)) = parse_with(&args(&["-I", "-z"]), rc) else {
            panic!("not a list")
        };
        assert!(names.is_empty() && syntax_list(&names).is_empty());
        // A line is ended once the names on it are wider than 45 columns.
        let long: Vec<String> = ["x".repeat(30), "y".repeat(20), "z".into()].into();
        assert_eq!(
            syntax_list(&long),
            format!("Available syntaxes:\n {} {}\n z\n", long[0], long[1])
        );
    }

    #[test]
    fn every_option_of_the_table_that_works_is_handled() {
        for o in OPTIONS.iter().filter(|o| o.support != Later) {
            let value = if o.long == "backupdir" { "." } else { "1" };
            let mut p = Parsing {
                options: Options::default(),
                files: Vec::new(),
                places: Vec::new(),
                later: None,
            };
            // `apply` panics on an option that is in the table but not handled.
            assert!(apply(&mut p, o, value).is_ok(), "{}", o.long);
        }
        let mut shorts: Vec<char> = OPTIONS.iter().map(|o| o.short).collect();
        shorts.sort_unstable();
        shorts.dedup();
        assert_eq!(shorts.len(), OPTIONS.len());
    }

    #[test]
    fn plus_arguments() {
        let alert = |args: &[&str]| edit(args).files[0].alert.clone();
        assert_eq!(
            alert(&["+3x", "f"]).as_deref(),
            Some("Invalid line or column number")
        );
        assert_eq!(edit(&["+3x", "f"]).files[0].start, None);
        assert_eq!(
            alert(&["+,x", "f"]).as_deref(),
            Some("Invalid line or column number")
        );
        assert_eq!(edit(&["+-2,-3", "f"]).files[0].start, Some((-2, -3)));
        let found = edit(&["+cr?a.c", "f"]).files.remove(0).search;
        let found = found.expect("a search");
        assert_eq!(found.text, "a.c");
        assert!(found.backwards && found.case_sensitive && found.regexp);
        let found = edit(&["+/x/y", "f"]).files.remove(0).search;
        let found = found.expect("a search");
        assert_eq!(found.text, "x/y");
        assert!(!found.backwards && !found.case_sensitive && !found.regexp);
        let found = edit(&["+crCR/x", "f"]).files.remove(0).search;
        assert!(!found.expect("a search").case_sensitive);
        assert_eq!(
            alert(&["+x/y", "f"]).as_deref(),
            Some("Invalid search modifier 'x'")
        );
        assert_eq!(alert(&["+/", "f"]).as_deref(), Some("Empty search string"));
        // A place applies to the next argument, which is a file name.
        let found = edit(&["+/a", "f"]).files.remove(0).search;
        assert_eq!(found.expect("a search").text, "a");
        assert_eq!(names(&edit(&["+3"])), ["+3"]);
        assert_eq!(edit(&["+3"]).files[0].start, None);
        assert_eq!(names(&edit(&["+2", "+4"])), ["+4"]);
        assert_eq!(edit(&["+2", "+4"]).files[0].start, Some((2, 1)));
    }

    #[test]
    fn each_file_has_the_place_before_it() {
        let o = edit(&["a", "+3,4", "b", "c", "+/x", "d", "+5", "e", "+6"]);
        assert_eq!(names(&o), ["a", "b", "c", "d", "e", "+6"]);
        let starts: Vec<_> = o.files.iter().map(|p| p.start).collect();
        assert_eq!(starts, [None, Some((3, 4)), None, None, Some((5, 1)), None]);
        assert_eq!(
            o.files[3].search.as_ref().map(|s| s.text.as_str()),
            Some("x")
        );
        assert!(o.files[2].search.is_none());
    }

    #[test]
    fn the_modifiers_of_a_search_stay_for_the_files_after_it() {
        // As in nano: `c` and `?` are set once and stay set.
        let o = edit(&["+c?x", "a", "+/y", "b"]);
        let (a, b) = (o.files[0].search.as_ref(), o.files[1].search.as_ref());
        let (a, b) = (a.expect("a search"), b.expect("a search"));
        assert!(a.case_sensitive && a.backwards);
        assert!(b.case_sensitive && b.backwards);
    }

    #[test]
    fn several_files_and_multibuffer() {
        assert_eq!(names(&edit(&["a", "b", "c"])), ["a", "b", "c"]);
        assert!(!edit(&["a"]).multibuffer);
        assert!(edit(&["-F", "a"]).multibuffer);
        assert!(edit(&["--multibuffer"]).multibuffer);
        assert!(err(&["a", "-"]).contains("standard input"));
    }

    #[test]
    fn too_many_files_are_refused() {
        let many: Vec<String> = (0..=MAX_BUFFERS).map(|n| format!("f{n}")).collect();
        let args: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(err(&args).contains("at most 256 files"));
        assert_eq!(edit(&args[..MAX_BUFFERS]).files.len(), MAX_BUFFERS);
    }
}
