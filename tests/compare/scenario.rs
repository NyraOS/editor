// SPDX-License-Identifier: MIT
//! The scenario format (described in README.md next to this file).

use std::ops::RangeInclusive;

/// The longest `keys:` step, in bytes: a step must fit in the terminal's input buffer, so that
/// writing it never blocks on an editor that stopped reading.
const MAX_STEP_BYTES: usize = 1024;

#[derive(Debug)]
pub struct Scenario {
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub files: Vec<(String, Vec<u8>)>,
    pub steps: Vec<Step>,
    pub differ: Vec<Region>,
}

#[derive(Debug)]
pub struct Step {
    /// The scenario line, for the report.
    pub line: usize,
    pub text: String,
    pub action: Action,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Keys(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// Screen cells that may differ: a deliberate difference from GNU nano.
#[derive(Debug, PartialEq)]
pub struct Region {
    pub rows: RangeInclusive<u16>,
    pub cols: RangeInclusive<u16>,
}

impl Region {
    pub fn contains(&self, row: u16, col: u16) -> bool {
        self.rows.contains(&row) && self.cols.contains(&col)
    }
}

/// Parses a scenario; errors name the line.
pub fn parse(text: &str) -> Result<Scenario, String> {
    let mut s = Scenario {
        args: Vec::new(),
        cols: 80,
        rows: 24,
        files: Vec::new(),
        steps: Vec::new(),
        differ: Vec::new(),
    };
    let (mut seen_args, mut seen_size) = (false, false);
    // Whether the last file can still take content lines.
    let mut in_file = false;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let err = |msg: String| format!("line {n}: {msg}");
        if line.starts_with('|') {
            let content = match line.strip_prefix("| ") {
                Some(c) => c,
                None if line == "|" => "",
                None => return Err(err("a content line starts with \"| \"".into())),
            };
            let Some((_, bytes)) = s.files.last_mut().filter(|_| in_file) else {
                return Err(err("a content line outside a file block".into()));
            };
            // An odd number of backslashes at the end: the last one drops the newline.
            let trailing = content.len() - content.trim_end_matches('\\').len();
            let (content, newline) = match trailing % 2 {
                1 => (&content[..content.len() - 1], false),
                _ => (content, true),
            };
            bytes.extend(unescape(content).map_err(err)?);
            if newline {
                bytes.push(b'\n');
            }
            continue;
        }
        in_file = false;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(err(format!("expected \"name: value\", got {line:?}")));
        };
        let value = value.trim();
        match key {
            "args" if !seen_args => {
                seen_args = true;
                s.args = value.split_whitespace().map(String::from).collect();
            }
            "size" if !seen_size => {
                seen_size = true;
                (s.cols, s.rows) = size(value).map_err(err)?;
            }
            "differ" => s.differ.push(region(value).map_err(err)?),
            "keys" => {
                let bytes = keys(value).map_err(err)?;
                if bytes.is_empty() || bytes.len() > MAX_STEP_BYTES {
                    return Err(err(format!("a step sends 1 to {MAX_STEP_BYTES} bytes")));
                }
                s.steps.push(Step {
                    line: n,
                    text: line.into(),
                    action: Action::Keys(bytes),
                });
            }
            "resize" => {
                let (cols, rows) = size(value).map_err(err)?;
                s.steps.push(Step {
                    line: n,
                    text: line.into(),
                    action: Action::Resize { cols, rows },
                });
            }
            _ => match key.strip_prefix("file ") {
                Some(name) if value.is_empty() => {
                    let name = name.trim();
                    let bad = name.is_empty()
                        || name.starts_with('/')
                        || name
                            .split('/')
                            .any(|p| p.is_empty() || p == "." || p == "..");
                    if bad || s.files.iter().any(|(f, _)| f == name) {
                        return Err(err(format!("bad or repeated file name {name:?}")));
                    }
                    s.files.push((name.into(), Vec::new()));
                    in_file = true;
                }
                _ => return Err(err(format!("unknown or repeated {key:?}"))),
            },
        }
    }
    if s.steps.is_empty() {
        return Err("no steps: add at least one \"keys:\" line".into());
    }
    Ok(s)
}

/// `80x24`: columns by rows.
fn size(value: &str) -> Result<(u16, u16), String> {
    let parsed = value
        .split_once('x')
        .and_then(|(c, r)| Some((c.parse().ok()?, r.parse().ok()?)));
    match parsed {
        Some((c, r)) if (10..=500).contains(&c) && (5..=500).contains(&r) => Ok((c, r)),
        _ => Err(format!(
            "expected a size like 80x24 (10-500 columns, 5-500 rows), got {value:?}"
        )),
    }
}

/// `rows 22-23: reason` or `row 0 cols 2-20: reason`.
fn region(value: &str) -> Result<Region, String> {
    let usage = || format!("expected \"rows A-B [cols C-D]: reason\", got {value:?}");
    let Some((spec, reason)) = value.split_once(':') else {
        return Err(usage());
    };
    let words: Vec<&str> = spec.split_whitespace().collect();
    let parse_range = |name: &str, kind: &str, r: &str| -> Option<RangeInclusive<u16>> {
        if name != kind && name != format!("{kind}s") {
            return None;
        }
        let (a, b) = r.split_once('-').unwrap_or((r, r));
        let (a, b): (u16, u16) = (a.parse().ok()?, b.parse().ok()?);
        (a <= b).then_some(a..=b)
    };
    let (rows, cols) = match words[..] {
        [r, rr] => (parse_range(r, "row", rr), Some(0..=u16::MAX)),
        [r, rr, c, cc] => (parse_range(r, "row", rr), parse_range(c, "col", cc)),
        _ => (None, None),
    };
    match (rows, cols) {
        (Some(rows), Some(cols)) if !reason.trim().is_empty() => Ok(Region { rows, cols }),
        _ => Err(usage()),
    }
}

/// The escapes of file content lines: `\t`, `\r`, `\e`, `\\` and `\xHH`.
fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0; 4];
            out.extend(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('t') => out.push(b'\t'),
            Some('r') => out.push(b'\r'),
            Some('e') => out.push(0x1b),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let byte = u8::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 2)
                    .ok_or(format!("bad escape \\x{hex}"))?;
                out.push(byte);
            }
            other => return Err(format!("bad escape \\{}", other.unwrap_or(' '))),
        }
    }
    Ok(out)
}

/// The bytes a terminal sends for a `keys:` value: text, with keys in braces.
pub fn keys(value: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix("{{") {
            out.push(b'{');
            rest = r;
        } else if c == '{' {
            let end = rest
                .find('}')
                .ok_or(format!("no closing brace in {rest:?}"))?;
            out.extend(key(&rest[1..end])?);
            rest = &rest[end + 1..];
        } else {
            out.extend(&rest.as_bytes()[..c.len_utf8()]);
            rest = &rest[c.len_utf8()..];
        }
    }
    Ok(out)
}

/// One `{...}` key, with an optional `*N` repeat.
fn key(token: &str) -> Result<Vec<u8>, String> {
    let bad = || format!("unknown key {{{token}}}; see tests/compare/README.md");
    let (name, count) = match token.rsplit_once('*') {
        Some((name, n)) if !name.is_empty() => (name, n.parse().map_err(|_| bad())?),
        _ => (token, 1),
    };
    if !(1..=200).contains(&count) {
        return Err(bad());
    }
    // Modifiers: ^ (Ctrl), M- (Alt), S- (Shift), in any order, before the key.
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut name = name;
    loop {
        if let Some(r) = name.strip_prefix('^').filter(|r| !r.is_empty()) {
            (ctrl, name) = (true, r);
        } else if let Some(r) = name.strip_prefix("M-").filter(|r| !r.is_empty()) {
            (alt, name) = (true, r);
        } else if let Some(r) = name.strip_prefix("S-").filter(|r| !r.is_empty()) {
            (shift, name) = (true, r);
        } else {
            break;
        }
    }
    let one = if let Some(bytes) = special(name, ctrl, alt, shift) {
        bytes
    } else {
        let mut chars = name.chars();
        let (Some(c), None, false) = (chars.next(), chars.next(), shift) else {
            return Err(bad());
        };
        let mut bytes = Vec::new();
        if alt {
            bytes.push(0x1b);
        }
        match (ctrl, c.to_ascii_uppercase()) {
            (false, _) => bytes.extend(c.to_string().as_bytes()),
            (true, '?') => bytes.push(0x7f),
            (true, '/') => bytes.push(0x1f),
            (true, u @ '@'..='_') => bytes.push(u as u8 & 0x1f),
            _ => return Err(bad()),
        }
        bytes
    };
    Ok(one.repeat(count))
}

/// The keys with names, as xterm sends them with the keypad in application mode (which
/// ncurses turns on). With modifiers, xterm adds a parameter: 1 + Shift 1 + Alt 2 + Ctrl 4.
fn special(name: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Vec<u8>> {
    let plain = |s: &str| (!ctrl && !alt && !shift).then(|| s.as_bytes().to_vec());
    let m = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    // Keys sent as ESC O x without modifiers and ESC [ 1 ; m x with them.
    let letter = |x: char| {
        Some(if m == 1 {
            format!("\x1bO{x}").into_bytes()
        } else {
            format!("\x1b[1;{m}{x}").into_bytes()
        })
    };
    // Keys sent as ESC [ n ~ and ESC [ n ; m ~.
    let tilde = |n: u8| {
        Some(if m == 1 {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{m}~").into_bytes()
        })
    };
    match name {
        "Enter" => plain("\r"),
        "Tab" => plain("\t"),
        "Esc" => plain("\x1b"),
        "Space" => plain(" "),
        "Bsp" => plain("\x7f"),
        "Up" => letter('A'),
        "Down" => letter('B'),
        "Right" => letter('C'),
        "Left" => letter('D'),
        "Home" => letter('H'),
        "End" => letter('F'),
        "F1" => letter('P'),
        "F2" => letter('Q'),
        "F3" => letter('R'),
        "F4" => letter('S'),
        "Ins" => tilde(2),
        "Del" => tilde(3),
        "PgUp" => tilde(5),
        "PgDn" => tilde(6),
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        _ => None,
    }
}
