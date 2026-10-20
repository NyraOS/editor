// SPDX-License-Identifier: MIT
//! The command line. Only the options the editor implements so far; the rest of nano's options,
//! and the nanorc files, come later.

/// What the command line asks for.
#[derive(Debug, PartialEq)]
pub enum Command {
    Edit(Options),
    Version,
    Usage,
}

#[derive(Debug, PartialEq)]
pub struct Options {
    /// The file to open; None for a new, unnamed buffer.
    pub file: Option<String>,
    /// `+LINE,COLUMN`: where the cursor starts (1-based; negative counts from the end).
    pub start: Option<(i64, i64)>,
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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            file: None,
            start: None,
            linenumbers: false,
            constantshow: false,
            nohelp: false,
            nonewlines: false,
            tabsize: 8,
        }
    }
}

/// Parses the arguments (without the program name). Err is the message for the user.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut o = Options::default();
    let mut args = args.iter();
    let mut only_files = false;
    while let Some(arg) = args.next() {
        if only_files || arg == "-" || !arg.starts_with('-') {
            if let Some(pos) = arg.strip_prefix('+').filter(|_| !only_files) {
                o.start = Some(
                    position(pos).ok_or_else(|| format!("Invalid line or column number: {pos}"))?,
                );
            } else if o.file.is_none() {
                o.file = Some(arg.clone());
            }
            continue;
        }
        if arg == "--" {
            only_files = true;
            continue;
        }
        // Long options, with an argument after '=' or in the next word.
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "version" => return Ok(Command::Version),
                "help" => return Ok(Command::Usage),
                "tabsize" => {
                    let v = value.or_else(|| args.next().cloned()).unwrap_or_default();
                    o.tabsize = tabsize(&v)?;
                }
                _ => flag(&mut o, name).ok_or_else(|| format!("unrecognized option '--{name}'"))?,
            }
            continue;
        }
        // Short options, possibly grouped: -lc, -T4, -T 4.
        let mut chars = arg[1..].chars();
        while let Some(c) = chars.next() {
            match c {
                'V' => return Ok(Command::Version),
                'h' => return Ok(Command::Usage),
                'T' => {
                    let rest: String = chars.by_ref().collect();
                    let v = if rest.is_empty() {
                        args.next().cloned().unwrap_or_default()
                    } else {
                        rest
                    };
                    o.tabsize = tabsize(&v)?;
                }
                _ => {
                    let name = SHORT
                        .iter()
                        .find(|(s, _)| *s == c)
                        .map(|(_, l)| *l)
                        .ok_or_else(|| format!("invalid option -- '{c}'"))?;
                    flag(&mut o, name).ok_or_else(|| format!("invalid option -- '{c}'"))?;
                }
            }
        }
    }
    Ok(Command::Edit(o))
}

/// Short flags and their long names.
const SHORT: &[(char, &str)] = &[
    ('G', "locking"),
    ('I', "ignorercfiles"),
    ('L', "nonewlines"),
    ('c', "constantshow"),
    ('l', "linenumbers"),
    ('x', "nohelp"),
];

/// Sets a flag by its long name; None if there is no such flag.
fn flag(o: &mut Options, name: &str) -> Option<()> {
    match name {
        // No nanorc is read yet, so there is nothing to ignore.
        "ignorercfiles" => {}
        // Files are always locked.
        "locking" => {}
        "nonewlines" => o.nonewlines = true,
        "constantshow" => o.constantshow = true,
        "linenumbers" => o.linenumbers = true,
        "nohelp" => o.nohelp = true,
        _ => return None,
    }
    Some(())
}

fn tabsize(v: &str) -> Result<usize, String> {
    v.parse::<usize>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("Requested tab size \"{v}\" is invalid"))
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
Usage: editor [OPTIONS] [[+LINE[,COLUMN]] FILE]

A text editor that works like GNU nano; it is not GNU nano.
Type ^G inside the editor for the keys.

Options:
 +LINE[,COLUMN]       Start at this line and column
 -T, --tabsize=N      Make a tab N columns wide (default 8)
 -L, --nonewlines     Do not add a newline at the end of the file
 -c, --constantshow   Show the cursor position all the time
 -l, --linenumbers    Show line numbers in front of the text
 -x, --nohelp         Do not show the two help lines
 -G, --locking        Lock the file while editing it (always on)
 -I, --ignorercfiles  Do not read nanorc files
 -h, --help           Show this help and exit
 -V, --version        Show the version and exit
";

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(args: &[&str]) -> Options {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match parse(&args) {
            Ok(Command::Edit(o)) => o,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn flags_and_file() {
        let o = edit(&["--ignorercfiles", "-lc", "+3,5", "notes.txt", "other"]);
        assert!(o.linenumbers && o.constantshow && !o.nohelp);
        assert_eq!(o.start, Some((3, 5)));
        assert_eq!(o.file.as_deref(), Some("notes.txt"));
        assert_eq!(edit(&["-T4"]).tabsize, 4);
        assert_eq!(edit(&["-T", "2"]).tabsize, 2);
        assert_eq!(edit(&["--tabsize=3"]).tabsize, 3);
        assert_eq!(edit(&["--", "-l"]).file.as_deref(), Some("-l"));
        assert_eq!(edit(&["+,7"]).start, Some((1, 7)));
    }

    #[test]
    fn errors_and_info() {
        let p = |a: &[&str]| parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(p(&["--version"]), Ok(Command::Version));
        assert_eq!(p(&["-h"]), Ok(Command::Usage));
        assert!(p(&["-q"]).is_err());
        assert!(p(&["--bogus"]).is_err());
        assert!(p(&["-T", "0"]).is_err());
        assert!(p(&["+x"]).is_err());
    }
}
