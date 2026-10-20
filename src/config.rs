// SPDX-License-Identifier: MIT
//! The command line, with the options of GNU nano 8.4 (the nanorc files come later). An option
//! the editor cannot honour yet is refused ("not available yet") rather than ignored, unless
//! nano would show the same screen with it.

use std::path::PathBuf;

/// What the command line asks for.
#[derive(Debug, PartialEq)]
pub enum Command {
    Edit(Options),
    Version,
    Usage,
}

/// `+/text`: a search to make when the file is open.
#[derive(Debug, PartialEq)]
pub struct StartSearch {
    pub text: String,
    pub backwards: bool,
    pub case_sensitive: bool,
    pub regexp: bool,
}

#[derive(Debug, PartialEq)]
pub struct Options {
    /// The file to open; None for a new, unnamed buffer.
    pub file: Option<String>,
    /// `+LINE,COLUMN`: where the cursor starts (1-based; negative counts from the end).
    pub start: Option<(i64, i64)>,
    /// `+/text` or `+?text`, with modifiers: a search to make instead of going to a place.
    pub search: Option<StartSearch>,
    /// What is wrong with the `+` argument: shown in the status bar, as nano does.
    pub start_alert: Option<String>,
    /// `-l`: numbers in front of the lines.
    pub linenumbers: bool,
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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            file: None,
            start: None,
            search: None,
            start_alert: None,
            linenumbers: false,
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
    opt('a', "atblanks", false, Ignored),
    opt('i', "autoindent", false, Works),
    opt('y', "afterends", false, Later),
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
    opt('J', "guidestripe", true, Later),
    opt('h', "help", false, Works),
    opt('H', "historylog", false, Later),
    opt('I', "ignorercfiles", false, Works),
    opt('q', "indicator", false, Later),
    opt('j', "jumpyscrolling", false, Later),
    opt('l', "linenumbers", false, Works),
    opt('z', "listsyntaxes", false, Later),
    opt('G', "locking", false, Works),
    opt('F', "multibuffer", false, Later),
    opt('m', "mouse", false, Later),
    opt('/', "modernbindings", false, Later),
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
    opt('Y', "syntax", true, Later),
    opt('g', "showcursor", false, Later),
    opt('s', "speller", true, Ignored),
    opt('t', "saveonexit", false, Works),
    opt('A', "smarthome", false, Works),
    opt('S', "softwrap", false, Later),
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
    files: Vec<String>,
    /// The first option or argument that is not available yet.
    later: Option<String>,
}

/// Parses the arguments (without the program name). Err is what to print before exiting with
/// status 1.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut p = Parsing {
        options: Options::default(),
        files: Vec::new(),
        later: None,
    };
    let mut args = args.iter();
    let mut only_files = false;
    while let Some(arg) = args.next() {
        if only_files || arg == "-" || !arg.starts_with('-') {
            match arg.strip_prefix('+') {
                Some(spec) => start_spec(&mut p.options, spec),
                None => p.files.push(arg.clone()),
            }
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
    finish(p)
}

/// The messages of getopt, as GNU nano prints them (with this program's name).
fn usage_error(what: String) -> String {
    format!("{PROGRAM}: {what}\nType '{PROGRAM} -h' for a list of available options.")
}

const PROGRAM: &str = "editor";

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
            // Its argument is still checked, as nano does, so a mistake is reported first.
            if opt.long == "guidestripe" && number(value, "Guide column")? <= 0 {
                return Err(format!("Guide column \"{value}\" is invalid"));
            }
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
        "help" => return Ok(Some(Command::Usage)),
        "tabsize" => {
            o.tabsize = value
                .parse::<usize>()
                .ok()
                .filter(|&n| n > 0)
                .ok_or_else(|| format!("Requested tab size \"{value}\" is invalid"))?;
        }
        "backupdir" => o.backupdir = Some(PathBuf::from(value)),
        "wordchars" => o.wordchars = value.to_string(),
        "nonewlines" => o.nonewlines = true,
        "constantshow" => o.constantshow = true,
        "linenumbers" => o.linenumbers = true,
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
        // No nanorc is read yet, so there is nothing to ignore; files are always locked.
        "ignorercfiles" | "locking" => {}
        other => unreachable!("option {other} is in the table as working, but not handled"),
    }
    Ok(None)
}

/// A number option's argument, or the message for a bad one.
fn number(value: &str, what: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|_| format!("{what} \"{value}\" is invalid"))
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
    if p.files.len() > 1 {
        return Err(format!(
            "{PROGRAM}: editing several files is not available yet"
        ));
    }
    o.file = p.files.into_iter().next();
    if o.file.as_deref() == Some("-") {
        return Err(format!(
            "{PROGRAM}: reading the text from the standard input is not available yet"
        ));
    }
    Ok(Command::Edit(o))
}

/// The argument `+SPEC`: `LINE`, `LINE,COLUMN`, `,COLUMN`, or a search: `/text` forwards, `?text`
/// backwards, after modifiers: `c` and `C` for case sensitive or not, `r` and `R` for a regular
/// expression or not.
fn start_spec(o: &mut Options, spec: &str) {
    o.start = None;
    o.search = None;
    o.start_alert = None;
    if spec.starts_with(|c: char| c.is_ascii_digit() || matches!(c, ',' | '-')) || spec.is_empty() {
        match position(spec) {
            Some(at) => o.start = Some(at),
            None => o.start_alert = Some("Invalid line or column number".into()),
        }
        return;
    }
    let mut search = StartSearch {
        text: String::new(),
        backwards: false,
        case_sensitive: false,
        regexp: false,
    };
    for (i, c) in spec.char_indices() {
        match c {
            'c' => search.case_sensitive = true,
            'C' => search.case_sensitive = false,
            'r' => search.regexp = true,
            'R' => search.regexp = false,
            '/' | '?' => {
                search.backwards |= c == '?';
                search.text = spec[i + 1..].to_string();
                if search.text.is_empty() {
                    o.start_alert = Some("Empty search string".into());
                } else {
                    o.search = Some(search);
                }
                return;
            }
            _ => {
                o.start_alert = Some(format!("Invalid search modifier '{c}'"));
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

pub const USAGE: &str = "\
Usage: editor [OPTIONS] [+LINE[,COLUMN] | +/TEXT | +?TEXT] [FILE]

A text editor that works like GNU nano; it is not GNU nano.
Type ^G inside the editor for the keys. Short options can be joined (-lc, -T4), long ones
shortened while they stay unambiguous; -- ends the options.

Where to start:
 +LINE[,COLUMN]          Put the cursor there (a negative number counts from the end)
 +/TEXT, +?TEXT          Search forwards, or backwards, for TEXT; before the slash, the
                         modifiers c/C (case sensitive or not), r/R (regular expression or
                         not)

Options:
 -A, --smarthome         Home goes to the first non-blank character first
 -B, --backup            Keep a backup (FILE~) of the old file at every save
 -C, --backupdir=DIR     Put the backups, numbered, in DIR
 -E, --tabstospaces      Make a typed Tab write spaces
 -G, --locking           Lock the file while editing it (always on)
 -I, --ignorercfiles     Do not read nanorc files
 -L, --nonewlines        Do not add a newline at the end of the file
 -N, --noconvert         Do not convert DOS and Mac line ends
 -T, --tabsize=N         Make a tab N columns wide (default 8)
 -W, --wordbounds        Let punctuation separate words
 -X, --wordchars=CHARS   Treat these characters as parts of words
 -Z, --zap               Backspace and Delete erase the selection
 -c, --constantshow      Show the cursor position all the time
 -h, --help              Show this help and exit
 -i, --autoindent        Start a new line with the blanks of the one before
 -k, --cutfromcursor     Cut from the cursor to the end of the line
 -l, --linenumbers       Show line numbers in front of the text
 -t, --saveonexit        Save a modified buffer on exit without asking
 -u, --unix              Save the file in Unix format
 -V, --version           Show the version and exit
 -x, --nohelp            Do not show the two help lines

Accepted, with no effect yet: -M, -O, -a, -r, -s, -w.
Not available yet (refused): -D, -F, -H, -J, -K, -P, -Q, -R, -S, -U, -Y, -b, -d, -e, -f, -g,
-j, -m, -n, -o, -p, -q, -v, -y, -z, -@, -%, -_, -0, -/, several files, and - (standard input).
";

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Command, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
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

    #[test]
    fn flags_and_file() {
        let o = edit(&["--ignorercfiles", "-lc", "+3,5", "notes.txt"]);
        assert!(o.linenumbers && o.constantshow && !o.nohelp);
        assert_eq!(o.start, Some((3, 5)));
        assert_eq!(o.file.as_deref(), Some("notes.txt"));
        assert_eq!(edit(&["+,7"]).start, Some((1, 7)));
        assert_eq!(edit(&["-x", "f", "-L"]).file.as_deref(), Some("f"));
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
        assert_eq!(edit(&["--", "-l"]).file.as_deref(), Some("-l"));
        assert!(!edit(&["--", "-l"]).linenumbers);
        assert_eq!(edit(&["--", "+4", "f"]).start, Some((4, 1)));
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
            edit(&["-r", "-5", "-r0", "-r72", "-a", "-M", "-O", "-w", "-s", "x"]),
            Options::default()
        );
    }

    #[test]
    fn what_is_not_available_yet_is_refused_not_ignored() {
        for args in [
            &["-m"][..],
            &["--mouse"],
            &["-S"],
            &["-Y", "sh"],
            &["-b"],
            &["-%"],
            &["-_"],
            &["-0"],
            &["-/"],
            &["-lv"],
            &["--rebinddelete"],
        ] {
            assert!(err(args).ends_with("is not available yet"), "{args:?}");
        }
        assert_eq!(
            err(&["-m"]),
            "editor: option -m (--mouse) is not available yet"
        );
        // The first one is the one named; a mistake in an option's argument comes before.
        assert!(err(&["-S", "-m"]).contains("--softwrap"));
        assert!(err(&["-m", "-T", "x"]).starts_with("Requested tab size"));
        assert!(err(&["a", "b"]).contains("several files"));
        assert!(err(&["-"]).contains("standard input"));
    }

    #[test]
    fn every_option_of_the_table_that_works_is_handled() {
        for o in OPTIONS.iter().filter(|o| o.support != Later) {
            let value = if o.long == "backupdir" { "." } else { "1" };
            let mut p = Parsing {
                options: Options::default(),
                files: Vec::new(),
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
        assert_eq!(
            edit(&["+3x", "f"]).start_alert.as_deref(),
            Some("Invalid line or column number")
        );
        assert_eq!(edit(&["+3x", "f"]).start, None);
        assert_eq!(
            edit(&["+,x"]).start_alert.as_deref(),
            Some("Invalid line or column number")
        );
        assert_eq!(edit(&["+-2,-3"]).start, Some((-2, -3)));
        let o = edit(&["+cr?a.c"]);
        let found = o.search.expect("a search");
        assert_eq!(found.text, "a.c");
        assert!(found.backwards && found.case_sensitive && found.regexp);
        let found = edit(&["+/x/y"]).search.expect("a search");
        assert_eq!(found.text, "x/y");
        assert!(!found.backwards && !found.case_sensitive && !found.regexp);
        assert!(!edit(&["+crCR/x"]).search.expect("a search").case_sensitive);
        assert_eq!(
            edit(&["+x/y"]).start_alert.as_deref(),
            Some("Invalid search modifier 'x'")
        );
        assert_eq!(
            edit(&["+/"]).start_alert.as_deref(),
            Some("Empty search string")
        );
        // The last one wins.
        assert_eq!(edit(&["+/a", "+4", "f"]).search, None);
    }
}
