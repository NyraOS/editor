// SPDX-License-Identifier: MIT
//! Syntax colouring, as the nanorc files of GNU nano 8.4 define it: `syntax NAME "REGEX"...`,
//! `header`, `color` and `icolor` (also with `start=` and `end=`). No syntax is built in; the
//! definitions come from the nanorc files (see `rcfile`), and the one for the file is picked when
//! it is opened or saved under another name.
//!
//! The nanorc files are the user's data, but the file being edited is not: the work on it is
//! bounded (see `LINE_LIMIT`, `LOOKBACK`, `SIZE_LIMIT` and `TIME_LIMIT`), and the only thing that
//! reaches the terminal from a colour is a number from the nanorc (see `Hue`), never a byte of the
//! file.

use crate::buffer::Buffer;
use crate::rcfile::shown;
use crate::search;
use regex::Regex;
use std::time::{Duration, Instant};

/// How a rule paints: the colours (None: the terminal's own) and the attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hue {
    /// A colour of the 256-colour palette: 0 to 7 the basic ones, 8 to 15 the light ones.
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    pub bold: bool,
    pub italic: bool,
}

/// A compiled regular expression that is compared by its text, so that the options of the editor
/// can still be compared in tests.
#[derive(Clone, Debug)]
pub struct Re(Regex);

impl PartialEq for Re {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

/// One `color` or `icolor` rule, as typed. The expressions are compiled only when the syntax is
/// picked, so that many syntaxes in the nanorc files do not slow the start (compiling takes about
/// half a millisecond for each), as in nano, which reads a syntax only when it is needed.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleDef {
    pub hue: Hue,
    pub icase: bool,
    pub start: String,
    pub end: Option<String>,
}

/// A syntax as the nanorc files define it.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxDef {
    pub name: String,
    /// The regexes of the file name, and of the first line, that choose this syntax.
    pub files: Vec<Re>,
    pub headers: Vec<Re>,
    /// In the order they were given: a later rule paints over an earlier one.
    pub rules: Vec<RuleDef>,
}

/// A rule ready to match.
pub struct Rule {
    hue: Hue,
    start: Regex,
    end: Option<Regex>,
}

/// The longest part of a line that is looked at, in bytes. nano looks for no match that starts
/// past 2000 bytes either. Here the expression is also given only that much of the line: a line
/// of a megabyte (minified code, a data file) costs no more to colour than one of 2000 bytes, and
/// scrolling sideways along it cannot make the screen slow. Beyond it a line has no colour, and
/// `$` matches at the cut.
const LINE_LIMIT: usize = 2000;

/// How many lines above the screen are read to find out whether the first line shown is inside a
/// `start=` ... `end=` match. nano reads the whole file; here the cost of a redraw must not grow
/// with the position in a big file. A match that starts further up than this does not colour the
/// screen.
const LOOKBACK: usize = 1000;

/// The most memory, in bytes, a compiled expression of a rule may take. The time to match grows
/// with the size of the expression: a few repetitions nested in each other make one that needs
/// more than 30 ms for a line of 2000 bytes at 60 KiB. This is a little more than three Unicode
/// classes (`[[:alpha:]]` takes 20 KiB), which the real syntax files do not go over.
const SIZE_LIMIT: usize = 64 << 10;

/// The longest the colouring of a screen may take. Most is checked between two uses of an
/// expression, so a screen takes at most this and the time of one more use (30 ms at most for
/// an expression of `SIZE_LIMIT`, on a line of `LINE_LIMIT`). What is not reached by then is shown
/// without colour; the next redraw tries again. Needed besides the limits above because their
/// product is not small: 50 rows, many rules, and expressions made to be slow.
const TIME_LIMIT: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------------------------
// Reading the arguments of the syntax commands (the lines themselves are read in `rcfile`).

/// The names of colours, with their number in the 256-colour palette, as in nano 8.4. The first
/// eight are the basic colours, which `light` and `bright` may precede; `normal` is the
/// terminal's own colour.
const COLOUR_NAMES: [(&str, Option<u8>); 34] = [
    ("red", Some(1)),
    ("green", Some(2)),
    ("blue", Some(4)),
    ("yellow", Some(3)),
    ("cyan", Some(6)),
    ("magenta", Some(5)),
    ("white", Some(7)),
    ("black", Some(0)),
    ("normal", None),
    ("pink", Some(204)),
    ("purple", Some(163)),
    ("mauve", Some(134)),
    ("lagoon", Some(38)),
    ("mint", Some(48)),
    ("lime", Some(148)),
    ("peach", Some(215)),
    ("orange", Some(208)),
    ("latte", Some(137)),
    ("rosy", Some(175)),
    ("beet", Some(127)),
    ("plum", Some(98)),
    ("sea", Some(32)),
    ("sky", Some(111)),
    ("slate", Some(66)),
    ("teal", Some(35)),
    ("sage", Some(107)),
    ("brown", Some(100)),
    ("ocher", Some(142)),
    ("sand", Some(186)),
    ("tawny", Some(136)),
    ("brick", Some(166)),
    ("crimson", Some(161)),
    ("grey", Some(8)),
    ("gray", Some(8)),
];

/// `#rgb`: the colour of the palette's 6x6x6 cube (or of its grey ramp) nearest to the three
/// hexadecimal digits, by nano's tables.
fn hex_colour(digits: &str) -> Option<u8> {
    const LEVEL: [u8; 16] = [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5];
    const GREY: [u8; 14] = [1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 15, 18, 21, 23];
    let mut n = digits.chars().map(|c| c.to_digit(16));
    let (r, g, b) = (n.next()??, n.next()??, n.next()??);
    if r == g && g == b && 0 < r && r < 15 {
        return Some(232 + GREY[r as usize - 1]);
    }
    let cube = 36 * LEVEL[r as usize] + 6 * LEVEL[g as usize] + LEVEL[b as usize];
    Some(16 + cube)
}

/// One colour name: its number (None for the terminal's own), and whether `light` (the lighter
/// colour) or `bright` (nano's older word, which also means bold) came before it.
fn colour(name: &str) -> Result<(Option<u8>, bool, bool), String> {
    let (rest, light, bright) = match (name.strip_prefix("bright"), name.strip_prefix("light")) {
        (Some(rest), _) if !rest.is_empty() => (rest, true, true),
        (_, Some(rest)) if !rest.is_empty() => (rest, true, false),
        _ => (name, false, false),
    };
    let prefixed = light || bright;
    if rest.len() == 4 && rest.starts_with('#') {
        if prefixed {
            return Err(format!("Color '{rest}' takes no prefix"));
        }
        if let Some(n) = hex_colour(&rest[1..]) {
            return Ok((Some(n), false, false));
        }
    }
    match COLOUR_NAMES.iter().position(|(n, _)| *n == rest) {
        Some(i) if i > 7 && prefixed => Err(format!("Color '{rest}' takes no prefix")),
        Some(i) => Ok((COLOUR_NAMES[i].1, light, bright)),
        None => Err(format!("Color \"{rest}\" not understood")),
    }
}

/// `[bold,][italic,]FOREGROUND[,BACKGROUND]`, with the errors of nano.
pub fn parse_hue(text: &str) -> Result<Hue, String> {
    let mut hue = Hue::default();
    let mut text = text;
    for (word, flag) in [("bold", &mut hue.bold), ("italic", &mut hue.italic)] {
        if let Some(rest) = text.strip_prefix(word) {
            *flag = true;
            text = rest
                .strip_prefix(',')
                .ok_or("An attribute requires a subsequent comma")?;
        }
    }
    let (fg, bg) = match text.split_once(',') {
        Some((fg, bg)) => (fg, Some(bg)),
        None => (text, None),
    };
    if !fg.is_empty() || bg.is_none() {
        let (number, light, bright) = colour(fg)?;
        // The lighter colour is the next eight; the older `bright` only makes the text bold.
        hue.fg = number.map(|n| if light && !bright { n + 8 } else { n });
        hue.bold |= bright;
    }
    if let Some(bg) = bg {
        let (number, light, _) = colour(bg)?;
        hue.bg = number.map(|n| if light { n + 8 } else { n });
    }
    Ok(hue)
}

/// The regular expression that starts `text`, which must start with the double quote that opens
/// it: it ends at a double quote that is followed by a blank or the end of the line. The second
/// result is what follows, without its blanks.
pub fn next_regex(text: &str) -> Result<(&str, &str), String> {
    const FORM: &str = "Regex strings must begin and end with a \" character";
    let body = text.strip_prefix('"').ok_or(FORM)?;
    let end = body
        .char_indices()
        .find(|&(i, c)| c == '"' && body[i + 1..].chars().next().is_none_or(is_blank))
        .map(|(i, _)| i)
        .ok_or(FORM)?;
    if end == 0 {
        return Err("Empty regex string".into());
    }
    Ok((&body[..end], body[end + 1..].trim_start_matches(is_blank)))
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// Checks an expression as `Pattern` would read it; the error is nano's `Bad regex` message.
pub fn check(ere: &str) -> Result<(), String> {
    search::translate(ere)
        .map(drop)
        .map_err(|e| bad_regex(ere, &e))
}

/// nano's message for an expression that cannot be used.
fn bad_regex(ere: &str, why: &str) -> String {
    format!("Bad regex \"{}\": {why}", shown(ere))
}

/// The regexes of an `extension` or `header` list (`rest` starts at the first one), compiled as
/// nano does, without case folding, up to `room` of them. The second result is the messages: a
/// regex that is not valid is left out, one that is not written right ends the list.
pub fn regex_list(rest: &str, room: usize) -> (Vec<Re>, Vec<String>) {
    let (mut found, mut errors) = (Vec::new(), Vec::new());
    let mut rest = rest;
    while !rest.is_empty() {
        if found.len() >= room {
            errors.push("Too many regexes for choosing syntaxes".into());
            break;
        }
        let (text, after) = match next_regex(rest) {
            Ok(split) => split,
            Err(e) => {
                errors.push(e);
                break;
            }
        };
        rest = after;
        match search::compile(text, true, SIZE_LIMIT) {
            Ok(re) => found.push(Re(re)),
            Err(e) => errors.push(bad_regex(text, &e)),
        }
    }
    (found, errors)
}

/// The rules of a `color` or `icolor` command (`rest` is what follows the word), up to the first
/// mistake, and that mistake.
pub fn parse_rules(rest: &str, icase: bool) -> (Vec<RuleDef>, Option<String>) {
    let mut rules = Vec::new();
    let mut parse = || -> Result<(), String> {
        if rest.is_empty() {
            return Err("Missing color name".into());
        }
        let end = rest.find(is_blank).unwrap_or(rest.len());
        let hue = parse_hue(&rest[..end])?;
        let mut rest = rest[end..].trim_start_matches(is_blank);
        if rest.is_empty() {
            return Err("Missing regex string after 'color' command".into());
        }
        while !rest.is_empty() {
            let multi = rest.strip_prefix("start=");
            let (start, after) = next_regex(multi.unwrap_or(rest))?;
            check(start)?;
            rest = after;
            let mut end = None;
            if multi.is_some() {
                let Some(tail) = rest.strip_prefix("end=") else {
                    return Err("\"start=\" requires a corresponding \"end=\"".into());
                };
                let (text, after) = next_regex(tail)?;
                check(text)?;
                (end, rest) = (Some(text.to_string()), after);
            }
            rules.push(RuleDef {
                hue,
                icase,
                start: start.to_string(),
                end,
            });
        }
        Ok(())
    };
    let error = parse().err();
    (rules, error)
}

// ---------------------------------------------------------------------------------------------
// Choosing the syntax of a file.

/// The name of the file as nano matches it: with the directory made absolute and the links
/// followed, or as it was given when that is not possible.
fn full_path(name: &str) -> String {
    let path = std::path::Path::new(name);
    if let Ok(full) = path.canonicalize() {
        return full.to_string_lossy().into_owned();
    }
    // A file that does not exist yet: its directory may.
    if let (Some(dir), Some(base)) = (path.parent(), path.file_name()) {
        if let Ok(dir) = dir.canonicalize() {
            return dir.join(base).to_string_lossy().into_owned();
        }
    }
    name.to_string()
}

/// The syntax for a file: the one whose file name regexes match, else the one whose header
/// regexes match its first line, else `default`. Of several, the one defined last, as in nano, so
/// that a nanorc read later can replace a syntax.
pub fn find<'a>(defs: &'a [SyntaxDef], name: &str, first_line: &str) -> Option<&'a SyntaxDef> {
    let path = full_path(name);
    let newest = defs.iter().rev();
    newest
        .clone()
        .find(|d| d.files.iter().any(|re| re.0.is_match(&path)))
        .or_else(|| {
            newest
                .clone()
                .find(|d| d.headers.iter().any(|re| re.0.is_match(first_line)))
        })
        .or_else(|| newest.clone().find(|d| d.name == "default"))
}

/// The rules of a syntax, compiled; and the messages for the ones that cannot be (too big).
pub fn compile(def: &SyntaxDef) -> (Vec<Rule>, Vec<String>) {
    let (mut rules, mut errors) = (Vec::new(), Vec::new());
    for r in &def.rules {
        let build = |text: &str| {
            search::compile(text, !r.icase, SIZE_LIMIT).map_err(|e| bad_regex(text, &e))
        };
        let end = r.end.as_deref().map(build).transpose();
        match (build(&r.start), end) {
            (Ok(start), Ok(end)) => rules.push(Rule {
                hue: r.hue,
                start,
                end,
            }),
            (Err(e), _) | (_, Err(e)) => errors.push(e),
        }
    }
    (rules, errors)
}

// ---------------------------------------------------------------------------------------------
// Colouring the lines.

/// The first `LINE_LIMIT` bytes of a line, without the line end.
pub fn head(buf: &Buffer, line: usize) -> String {
    let mut s = String::new();
    for c in buf.text.line(line).chars() {
        if c == '\n' || s.len() + c.len_utf8() > LINE_LIMIT {
            break;
        }
        s.push(c);
    }
    s
}

/// The colour of every byte of the lines `first` to `last` (one list per line; the bytes past
/// `LINE_LIMIT` have none), by painting the rules one after the other, as nano does: where
/// several paint the same byte, the last one wins. A rule with `end` is painted from a match of
/// `start` to the next match of `end`, which may be in a later line; with no `end` it goes on to
/// the end of the text, as in nano.
pub fn paint(rules: &[Rule], buf: &Buffer, first: usize, last: usize) -> Vec<Vec<Hue>> {
    paint_within(rules, buf, first, last, TIME_LIMIT)
}

fn paint_within(
    rules: &[Rule],
    buf: &Buffer,
    first: usize,
    last: usize,
    time: Duration,
) -> Vec<Vec<Hue>> {
    let multi = rules.iter().any(|r| r.end.is_some());
    let from = if multi {
        first.saturating_sub(LOOKBACK)
    } else {
        first
    };
    let heads: Vec<String> = (from..=last).map(|line| head(buf, line)).collect();
    let skipped = first - from;
    let mut hues: Vec<Vec<Hue>> = heads[skipped..]
        .iter()
        .map(|h| vec![Hue::default(); h.len()])
        .collect();
    let deadline = Instant::now() + time;
    for rule in rules {
        match &rule.end {
            None => {
                for (h, text) in hues.iter_mut().zip(&heads[skipped..]) {
                    paint_matches(rule, text, h, deadline);
                }
            }
            Some(end) => paint_spans(rule, end, &heads, skipped, &mut hues, deadline),
        }
    }
    hues
}

/// The part of `hues` that `range` covers gets the colour of the rule.
fn fill(hues: &mut [Hue], from: usize, to: usize, hue: Hue) {
    if let Some(part) = hues.get_mut(from..to) {
        part.fill(hue);
    }
}

/// The index of the character after the one at `at`.
fn step(text: &str, at: usize) -> usize {
    at + text[at..].chars().next().map_or(1, char::len_utf8)
}

/// Paints every match of a rule with one expression in a line. Empty matches paint nothing.
fn paint_matches(rule: &Rule, text: &str, hues: &mut [Hue], deadline: Instant) {
    let mut at = 0;
    while at < text.len() && Instant::now() < deadline {
        let Some(m) = rule.start.find_at(text, at) else {
            break;
        };
        if m.is_empty() {
            at = step(text, m.end().max(at));
            continue;
        }
        fill(hues, m.start(), m.end(), rule.hue);
        at = m.end();
    }
}

/// Paints a rule with `start` and `end` over the lines of `heads` (of which the first `skipped`
/// are above the screen and have no colours). Inside a match that goes on from one line to the
/// next, a line is painted whole up to the end match.
fn paint_spans(
    rule: &Rule,
    end: &Regex,
    heads: &[String],
    skipped: usize,
    hues: &mut [Vec<Hue>],
    deadline: Instant,
) {
    let mut open = false;
    for (n, text) in heads.iter().enumerate() {
        if Instant::now() >= deadline {
            return;
        }
        // The lines above the screen are only read, to know whether a match is open.
        let mut nothing = Vec::new();
        let line = match n.checked_sub(skipped) {
            Some(i) => &mut hues[i],
            None => &mut nothing,
        };
        let mut at = 0;
        if open {
            let Some(e) = end.find_at(text, 0) else {
                fill(line, 0, text.len(), rule.hue);
                continue;
            };
            fill(line, 0, e.end(), rule.hue);
            (open, at) = (false, e.end());
        }
        while at <= text.len() {
            let Some(s) = rule.start.find_at(text, at) else {
                break;
            };
            let Some(e) = end.find_at(text, s.end()) else {
                fill(line, s.start(), text.len(), rule.hue);
                open = true;
                break;
            };
            if e.end() > s.start() {
                fill(line, s.start(), e.end(), rule.hue);
            }
            at = e.end();
            // Nothing matched at all: go one character on, or the loop would not end.
            if s.is_empty() && e.is_empty() {
                if at >= text.len() {
                    break;
                }
                at = step(text, at);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Format;

    fn rules_of(text: &str) -> Vec<Rule> {
        let mut defs = Vec::new();
        for line in text.lines() {
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            let (more, error) = parse_rules(rest, word == "icolor");
            assert_eq!(error, None, "{line}");
            defs.extend(more);
        }
        let def = SyntaxDef {
            name: "t".into(),
            files: Vec::new(),
            headers: Vec::new(),
            rules: defs,
        };
        let (rules, errors) = compile(&def);
        assert!(errors.is_empty(), "{errors:?}");
        rules
    }

    fn paint_text(rules: &str, text: &str) -> Vec<String> {
        let buf = Buffer::new(text, Format::Unix);
        let last = buf.lines() - 1;
        let hues = paint(&rules_of(rules), &buf, 0, last);
        hues.iter().map(|line| letters(line)).collect()
    }

    /// One letter for each byte: `r`, `g`, `b` for red, green, blue, `.` for no colour.
    fn letters(hues: &[Hue]) -> String {
        hues.iter()
            .map(|h| match h.fg {
                Some(1) => 'r',
                Some(2) => 'g',
                Some(4) => 'b',
                None => '.',
                Some(_) => '?',
            })
            .collect()
    }

    #[test]
    fn colour_names_and_prefixes() {
        let fg = |s: &str| parse_hue(s).map(|h| (h.fg, h.bg, h.bold, h.italic));
        assert_eq!(fg("red"), Ok((Some(1), None, false, false)));
        assert_eq!(fg("lightred"), Ok((Some(9), None, false, false)));
        // The older word: bold, not a lighter colour.
        assert_eq!(fg("brightred"), Ok((Some(1), None, true, false)));
        assert_eq!(fg("white,blue"), Ok((Some(7), Some(4), false, false)));
        assert_eq!(fg("red,brightblue"), Ok((Some(1), Some(12), false, false)));
        assert_eq!(fg(",green"), Ok((None, Some(2), false, false)));
        assert_eq!(fg("bold,italic,pink"), Ok((Some(204), None, true, true)));
        assert_eq!(fg("normal,normal"), Ok((None, None, false, false)));
        assert_eq!(fg("#f00"), Ok((Some(196), None, false, false)));
        assert_eq!(fg("#888"), Ok((Some(241), None, false, false)));
        assert_eq!(fg("nosuch"), Err("Color \"nosuch\" not understood".into()));
        assert_eq!(
            fg("brightnormal"),
            Err("Color 'normal' takes no prefix".into())
        );
        assert_eq!(fg("lightpink"), Err("Color 'pink' takes no prefix".into()));
        assert_eq!(fg("light#fff"), Err("Color '#fff' takes no prefix".into()));
        assert_eq!(
            fg("bold"),
            Err("An attribute requires a subsequent comma".into())
        );
        assert_eq!(fg("#12"), Err("Color \"#12\" not understood".into()));
    }

    #[test]
    fn regex_strings() {
        assert_eq!(next_regex("\"a b\" \"c\""), Ok(("a b", "\"c\"")));
        // A double quote inside is part of it unless a blank or the end follows.
        assert_eq!(next_regex("\"a\"b\" x"), Ok(("a\"b", "x")));
        assert!(
            next_regex("a")
                .unwrap_err()
                .starts_with("Regex strings must")
        );
        assert!(
            next_regex("\"a")
                .unwrap_err()
                .starts_with("Regex strings must")
        );
        assert_eq!(next_regex("\"\""), Err("Empty regex string".into()));
    }

    #[test]
    fn later_rules_paint_over_earlier_ones() {
        let rules = "color red \"[a-z]+\"\ncolor green \"b+\"\ncolor blue \"c\"";
        assert_eq!(paint_text(rules, "abbc d"), ["rggb.r"]);
        assert_eq!(
            paint_text("color blue \"c\"\ncolor red \"[a-z]+\"", "abbc"),
            ["rrrr"]
        );
    }

    #[test]
    fn icolor_ignores_case_and_color_does_not() {
        assert_eq!(paint_text("color red \"ab\"", "aB ab"), ["...rr"]);
        assert_eq!(paint_text("icolor red \"ab\"", "aB ab"), ["rr.rr"]);
    }

    #[test]
    fn a_match_that_starts_on_one_line_ends_on_a_later_one() {
        let rules = "color red start=\"/\\*\" end=\"\\*/\"";
        let lines = paint_text(rules, "a /* b\nc\nd */ e /* f */ g");
        assert_eq!(lines, ["..rrrr", "r", "rrrr...rrrrrrr.."]);
    }

    #[test]
    fn a_start_without_an_end_goes_to_the_end_of_the_text() {
        let lines = paint_text("color red start=\"<<\" end=\">>\"", "a <<\nb\nc");
        assert_eq!(lines, ["..rr", "r", "r"]);
    }

    #[test]
    fn rules_are_applied_in_order_with_spans_too() {
        let rules = "color red start=\"\\(\" end=\"\\)\"\ncolor green \"b\"";
        assert_eq!(paint_text(rules, "(a\nb)"), ["rr", "gr"]);
    }

    #[test]
    fn empty_matches_paint_nothing_and_end() {
        assert_eq!(paint_text("color red \"x*\"", "abxxc"), ["..rr."]);
        assert_eq!(
            paint_text("color red start=\"^\" end=\"$\"", "ab\ncd"),
            ["rr", "rr"]
        );
    }

    #[test]
    fn colours_are_by_byte_of_the_text() {
        // The screen maps bytes to columns (see `screen`); here, that a wide character is three
        // bytes that all take the colour of the match.
        assert_eq!(
            paint_text("color red \"日\"", "a日\tb"),
            [".rrr..".to_string()]
        );
    }

    #[test]
    fn only_the_start_of_a_long_line_is_coloured() {
        let long = "x".repeat(LINE_LIMIT + 500);
        let lines = paint_text("color red \"x\"", &long);
        assert_eq!(lines[0].len(), LINE_LIMIT);
        assert!(lines[0].chars().all(|c| c == 'r'));
    }

    #[test]
    fn a_match_that_starts_far_above_the_screen_is_not_seen() {
        let mut text = String::from("<<\n");
        text.push_str(&"x\n".repeat(LOOKBACK + 10));
        let buf = Buffer::new(&text, Format::Unix);
        let rules = rules_of("color red start=\"<<\" end=\">>\"");
        let near = buf.lines() - 5;
        // Inside the reach: line 0 is read, and the lines are coloured.
        let hues = paint(&rules, &buf, 5, 6);
        assert_eq!(letters(&hues[0]), "r");
        // Out of the reach.
        let hues = paint(&rules, &buf, near, near + 1);
        assert_eq!(letters(&hues[0]), ".");
    }

    #[test]
    fn a_slow_expression_cannot_hold_the_screen() {
        // An expression that is slow on a long line of the same letter, on thousands of such lines
        // above and below the screen, for rules with and without an end.
        let rules = rules_of(
            "color red \"(a?){100}a{100}\"\ncolor green start=\"a.*z|a\" end=\"never\"\ncolor blue \"a.*z|a\"",
        );
        let text = "a".repeat(LINE_LIMIT) + "\n";
        let buf = Buffer::new(&text.repeat(5000), Format::Unix);
        let began = Instant::now();
        let hues = paint(&rules, &buf, 2500, 2540);
        // The limit is 50 ms, and the slowest use of an expression a few ms; a very slow machine
        // still has a long way to go before this fails.
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(hues.len(), 41);
    }

    #[test]
    fn with_no_time_left_nothing_is_coloured() {
        let buf = Buffer::new("abc\nabc", Format::Unix);
        let rules = rules_of("color red \"abc\"\ncolor green start=\"a\" end=\"c\"");
        let hues = paint_within(&rules, &buf, 0, 1, Duration::ZERO);
        assert!(hues.iter().all(|line| letters(line) == "..."));
        let hues = paint_within(&rules, &buf, 0, 1, Duration::from_secs(5));
        assert!(hues.iter().all(|line| letters(line) == "ggg"));
    }

    #[test]
    fn expressions_over_the_size_limit_are_refused_when_the_syntax_is_picked() {
        let def = SyntaxDef {
            name: "t".into(),
            files: Vec::new(),
            headers: Vec::new(),
            rules: vec![RuleDef {
                hue: Hue::default(),
                icase: false,
                start: "(a?){2000}a{2000}".into(),
                end: None,
            }],
        };
        let (rules, errors) = compile(&def);
        assert!(rules.is_empty());
        assert_eq!(
            errors,
            ["Bad regex \"(a?){2000}a{2000}\": Regular expression too big"]
        );
    }

    #[test]
    fn the_syntax_of_a_file() {
        let (list, errors) = regex_list("\"\\.sh$\" \"\\.bash$\"", 9);
        assert!(errors.is_empty());
        let sh = SyntaxDef {
            name: "sh".into(),
            files: list,
            headers: regex_list("\"^#!.*/sh\"", 9).0,
            rules: Vec::new(),
        };
        let other = SyntaxDef {
            name: "default".into(),
            files: Vec::new(),
            headers: Vec::new(),
            rules: Vec::new(),
        };
        let defs = [sh, other];
        let name = |file: &str, first: &str| find(&defs, file, first).map(|d| d.name.as_str());
        assert_eq!(name("/nowhere/a.sh", ""), Some("sh"));
        assert_eq!(name("a.bash", "x"), Some("sh"));
        assert_eq!(name("script", "#!/bin/sh"), Some("sh"));
        assert_eq!(name("a.txt", "#!/bin/sh -e"), Some("sh"));
        assert_eq!(name("a.txt", "text"), Some("default"));
        assert_eq!(find(&defs[..1], "a.txt", "text").map(|d| &d.name[..]), None);
    }

    #[test]
    fn a_syntax_defined_later_wins() {
        let make = |name: &str| SyntaxDef {
            name: name.into(),
            files: regex_list("\"\\.x$\"", 9).0,
            headers: Vec::new(),
            rules: Vec::new(),
        };
        let defs = [make("first"), make("second")];
        assert_eq!(find(&defs, "a.x", "").map(|d| &d.name[..]), Some("second"));
    }
}
