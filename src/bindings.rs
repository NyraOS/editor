// SPDX-License-Identifier: MIT
//! Key bindings, as nano 8.4 has them: the keys of every menu, `bind` and `unbind` of the nanorc
//! files, and `--modernbindings`. The tables hold what the editor does today; a function nano has
//! and the editor has not can be named in a `bind` of a menu the editor does not have, and is
//! refused in the others (with a message), so that no key ever does nothing without saying so.
//!
//! A key that is pressed is looked up in the list of its menu and becomes the key the editor's
//! code handles for that function (its "canonical" key: `bind ^X cut main` makes ^X act as ^K).
//! The lists are kept with the newest binding first: the help lines show the first key of each
//! function, and a key belongs to one function in a menu, so binding it takes it from the
//! other. A key that is in no list is handled as before.

use crate::keys::{Key, Mods, Named};

/// The menus a nanorc may name (nano's own names), and the letter used for each in `FUNCTIONS`.
const MENUS: [(&str, char); 15] = [
    ("main", 'm'),
    ("search", 's'),
    ("replace", 'r'),
    ("replacewith", 'w'),
    ("yesno", 'y'),
    ("gotoline", 'g'),
    ("writeout", 'o'),
    ("insert", 'i'),
    ("browser", 'b'),
    ("whereisfile", 'f'),
    ("gotodir", 'd'),
    ("help", 'h'),
    ("spell", 'p'),
    ("linter", 'l'),
    ("execute", 'x'),
];

/// The menus the editor has, by the letters of `MENUS`: the index of a letter is its slot.
/// The longest string a key can type.
const MAX_TEXT: usize = 256;

const SLOTS: &str = "msrwygoh";
pub const MAIN: usize = 0;
pub const SEARCH: usize = 1;
pub const REPLACE: usize = 2;
pub const REPLACE_WITH: usize = 3;
pub const YES_NO: usize = 4;
pub const GOTO: usize = 5;
pub const WRITE_OUT: usize = 6;
pub const HELP: usize = 7;

/// Every function nano 8.4 lets a key be bound to, and the menus (letters) where it exists.
const FUNCTIONS: &[(&str, &str)] = &[
    ("help", "msrwgoibplxfd"),
    ("cancel", "srwygoiplxfd"),
    ("exit", "mbh"),
    ("writeout", "m"),
    ("savefile", "m"),
    ("insert", "m"),
    ("whereis", "mbh"),
    ("wherewas", "mbh"),
    ("findprevious", "mbh"),
    ("findnext", "mbh"),
    ("replace", "m"),
    ("cut", "msrwgoibplxfd"),
    ("copy", "m"),
    ("paste", "msrwgoibplxfd"),
    ("zap", "m"),
    ("chopwordleft", "m"),
    ("chopwordright", "m"),
    ("cutrestoffile", "mx"),
    ("mark", "m"),
    ("location", "m"),
    ("wordcount", "m"),
    ("execute", "m"),
    ("speller", "mx"),
    ("formatter", "mx"),
    ("linter", "mx"),
    ("justify", "m"),
    ("fulljustify", "mx"),
    ("indent", "m"),
    ("unindent", "m"),
    ("comment", "m"),
    ("complete", "m"),
    ("left", "msrwgoibplxfd"),
    ("right", "msrwgoibplxfd"),
    ("up", "mbh"),
    ("down", "mbh"),
    ("scrollup", "m"),
    ("scrolldown", "m"),
    ("center", "m"),
    ("cycle", "m"),
    ("prevword", "msrwgoibplxfd"),
    ("nextword", "msrwgoibplxfd"),
    ("home", "msrwgoibplxfd"),
    ("end", "msrwgoibplxfd"),
    ("beginpara", "mg"),
    ("endpara", "mg"),
    ("prevblock", "mb"),
    ("nextblock", "mb"),
    ("toprow", "m"),
    ("bottomrow", "m"),
    ("pageup", "mbhl"),
    ("pagedown", "mbhl"),
    ("firstline", "mgh"),
    ("lastline", "mgh"),
    ("gotoline", "m"),
    ("findbracket", "m"),
    ("anchor", "m"),
    ("prevanchor", "m"),
    ("nextanchor", "m"),
    ("prevbuf", "m"),
    ("nextbuf", "m"),
    ("verbatim", "msrwgoibplxfd"),
    ("tab", "msrwgoibplxfd"),
    ("enter", "msrwgoibplxfd"),
    ("delete", "msrwgoibplxfd"),
    ("backspace", "msrwgoibplxfd"),
    ("recordmacro", "m"),
    ("runmacro", "m"),
    ("undo", "m"),
    ("redo", "m"),
    ("refresh", "msrwygoibhplxfd"),
    ("suspend", "mx"),
    ("casesens", "sr"),
    ("regexp", "sr"),
    ("backwards", "sr"),
    ("older", "srwxf"),
    ("newer", "srwxf"),
    ("flipreplace", "sr"),
    ("flipgoto", "sg"),
    ("flipexecute", "i"),
    ("flippipe", "x"),
    ("flipnewbuffer", "ix"),
    ("flipconvert", "i"),
    ("dosformat", "o"),
    ("macformat", "o"),
    ("append", "o"),
    ("prepend", "o"),
    ("backup", "o"),
    ("discardbuffer", "o"),
    ("browser", "oi"),
    ("gotodir", "b"),
    ("firstfile", "bf"),
    ("lastfile", "bf"),
    ("nohelp", "msrwygoibplxfd"),
    ("zero", "m"),
    ("constantshow", "m"),
    ("softwrap", "m"),
    ("linenumbers", "m"),
    ("whitespacedisplay", "m"),
    ("nosyntax", "m"),
    ("smarthome", "m"),
    ("autoindent", "m"),
    ("cutfromcursor", "m"),
    ("mouse", "m"),
    ("breaklonglines", "m"),
    ("tabstospaces", "m"),
];

/// What a menu shows and has by default: function, label in the help lines (empty: not shown)
/// and keys, the first one is shown. A key that cannot be typed in a nanorc (an arrow, `PgUp`) is
/// only shown.
type Defaults = &'static [(&'static str, &'static str, &'static [&'static str])];

const MAIN_KEYS: Defaults = &[
    ("help", "Help", &["^G", "F1"]),
    ("exit", "Exit", &["^X", "F2"]),
    ("writeout", "Write Out", &["^O", "F3"]),
    ("insert", "Read File", &["^R"]),
    ("whereis", "Where Is", &["^F", "^W", "F6"]),
    ("replace", "Replace", &["^\\", "M-R"]),
    ("cut", "Cut", &["^K", "F9"]),
    ("paste", "Paste", &["^U", "F10"]),
    ("execute", "Execute", &["^T"]),
    ("justify", "Justify", &["^J"]),
    ("location", "Location", &["^C", "F11"]),
    ("gotoline", "Go To Line", &["^/", "M-G"]),
    ("undo", "Undo", &["M-U"]),
    ("redo", "Redo", &["M-E"]),
    ("mark", "Set Mark", &["M-A", "^^"]),
    ("copy", "Copy", &["M-6", "M-^"]),
    ("findbracket", "To Bracket", &["M-]"]),
    ("wherewas", "Where Was", &["^B", "^Q"]),
    ("findprevious", "Previous", &["M-B", "M-Q"]),
    ("findnext", "Next", &["M-F", "M-W"]),
    ("left", "Back", &["◂"]),
    ("right", "Forward", &["▸"]),
    ("prevword", "Prev Word", &["^◂"]),
    ("nextword", "Next Word", &["^▸"]),
    ("home", "Home", &["^A"]),
    ("end", "End", &["^E"]),
    ("up", "Prev Line", &["^P", "▴"]),
    ("down", "Next Line", &["^N", "▾"]),
    ("scrollup", "Scroll Up", &["M-▴"]),
    ("scrolldown", "Scroll Down", &["M-▾"]),
    ("prevblock", "Prev Block", &["^▴"]),
    ("nextblock", "Next Block", &["^▾"]),
    ("beginpara", "Begin of Paragraph", &["M-("]),
    ("endpara", "End of Paragraph", &["M-)"]),
    ("toprow", "Top Row", &["M-Home"]),
    ("bottomrow", "Bottom Row", &["M-End"]),
    ("pageup", "Prev Page", &["^Y", "PgUp"]),
    ("pagedown", "Next Page", &["^V", "PgDn"]),
    ("firstline", "First Line", &["M-\\"]),
    ("lastline", "Last Line", &["M-/"]),
    ("prevbuf", "Prev File", &["M-◂"]),
    ("nextbuf", "Next File", &["M-▸"]),
    ("tab", "Tab", &["^I"]),
    ("enter", "Enter", &["^M"]),
    ("backspace", "Backspace", &["^H", "Bsp"]),
    ("delete", "Delete", &["^D", "Del"]),
    ("chopwordleft", "Chop Left", &["M-Bsp"]),
    ("chopwordright", "Chop Right", &["^Del"]),
    ("cutrestoffile", "Cut Till End", &["M-T"]),
    ("fulljustify", "Full Justify", &["M-J"]),
    ("wordcount", "Word Count", &["M-D"]),
    ("verbatim", "Verbatim", &["M-V"]),
    ("indent", "Indent", &["M-}"]),
    ("unindent", "Unindent", &["M-{"]),
    ("savefile", "", &["^S"]),
    ("complete", "", &["^]"]),
    ("refresh", "", &["^L"]),
];

const HELP_KEYS: Defaults = &[
    ("refresh", "Refresh", &["^L"]),
    ("exit", "Close", &["^X", "F2", "F1", "^G"]),
    ("whereis", "Where Is", &["^F"]),
    ("wherewas", "Where Was", &["^B"]),
    ("findprevious", "Previous", &["M-B"]),
    ("findnext", "Next", &["M-F"]),
    ("up", "Prev Line", &["^P", "▴"]),
    ("down", "Next Line", &["^N", "▾"]),
    ("pageup", "Prev Page", &["^Y", "PgUp"]),
    ("pagedown", "Next Page", &["^V", "PgDn"]),
    ("firstline", "First Line", &["M-\\"]),
    ("lastline", "Last Line", &["M-/"]),
];

/// What every prompt has besides its own keys: the editing of the answer.
const EDITING: Defaults = &[
    ("home", "", &["^A"]),
    ("end", "", &["^E"]),
    ("backspace", "", &["^H"]),
    ("delete", "", &["^D"]),
    ("enter", "", &["^M"]),
];

const SEARCH_KEYS: Defaults = &[
    ("help", "Help", &["^G"]),
    ("cancel", "Cancel", &["^C"]),
    ("casesens", "Case Sens", &["M-C"]),
    ("regexp", "Reg.exp.", &["M-R"]),
    ("backwards", "Backwards", &["M-B"]),
    ("flipreplace", "Replace", &["^R"]),
    ("older", "Older", &["^P", "▴"]),
    ("newer", "Newer", &["^N", "▾"]),
    ("flipgoto", "Go To Line", &["^T"]),
];

const REPLACE_KEYS: Defaults = &[
    ("help", "Help", &["^G"]),
    ("cancel", "Cancel", &["^C"]),
    ("casesens", "Case Sens", &["M-C"]),
    ("regexp", "Reg.exp.", &["M-R"]),
    ("backwards", "Backwards", &["M-B"]),
    ("flipreplace", "No Replace", &["^R"]),
    ("older", "Older", &["^P", "▴"]),
    ("newer", "Newer", &["^N", "▾"]),
];

const REPLACEWITH_KEYS: Defaults = &[
    ("help", "Help", &["^G"]),
    ("cancel", "Cancel", &["^C"]),
    ("older", "Older", &["^P", "▴"]),
    ("newer", "Newer", &["^N", "▾"]),
];

const GOTO_KEYS: Defaults = &[
    ("help", "Help", &["^G"]),
    ("cancel", "Cancel", &["^C"]),
    ("beginpara", "Begin of Paragr.", &["^W"]),
    ("endpara", "End of Paragraph", &["^O"]),
    ("firstline", "First Line", &["^Y"]),
    ("lastline", "Last Line", &["^V"]),
    ("flipgoto", "Go To Text", &["^T"]),
];

const WRITE_KEYS: Defaults = &[
    ("help", "Help", &["^G"]),
    ("cancel", "Cancel", &["^C"]),
    ("dosformat", "DOS Format", &["M-D"]),
    ("macformat", "Mac Format", &["M-M"]),
    ("append", "Append", &["M-A"]),
    ("prepend", "Prepend", &["M-P"]),
    ("backup", "Backup File", &["M-B"]),
    ("browser", "Browse", &["^T"]),
];

const YES_NO_KEYS: Defaults = &[("cancel", "Cancel", &["^C"])];

/// The default lists of the slots, and whether the prompt editing keys come with them.
const DEFAULTS: [(Defaults, bool); 8] = [
    (MAIN_KEYS, false),
    (SEARCH_KEYS, true),
    (REPLACE_KEYS, true),
    (REPLACEWITH_KEYS, true),
    (YES_NO_KEYS, false),
    (GOTO_KEYS, true),
    (WRITE_KEYS, true),
    (HELP_KEYS, false),
];

/// A key as a nanorc names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spec {
    /// `^X`: the ASCII character it is written with, as in `Key::Ctrl`.
    Ctrl(char),
    /// `M-x`: a lowercase letter or another character.
    Meta(char),
    /// `Sh-M-x`: a lowercase letter.
    ShiftMeta(char),
    /// F1 to F24 (F13 and up are Shift and F1 and up).
    F(u8),
    Insert,
    Delete,
}

/// A change of a menu, as a nanorc line asks for it. Kept as it was read, because
/// `--modernbindings` is applied first and the command line is read after the files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    slot: usize,
    key: Spec,
    /// The key as typed in the nanorc, for the help lines.
    label: String,
    function: Option<&'static str>,
    /// For a key bound to a string: the text it types (function is None then).
    text: Option<String>,
}

/// What a `bind` binds the key to.
pub enum Target<'a> {
    Function(&'a str),
    /// A string in quotes: the key types it. Only its first `MAX_TEXT` characters are kept.
    Text(&'a str),
}

/// Parses a key name as nano does: `^X`, `M-X`, `Sh-M-X`, `F1` to `F24`, `Ins`, `Del`.
fn parse_key(name: &str) -> Option<Spec> {
    if name.len() > 12 {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let single = |s: &str| {
        let mut chars = s.chars();
        chars.next().filter(|_| chars.next().is_none())
    };
    if let Some(rest) = name.strip_prefix('^') {
        if lower == "^space" {
            return Some(Spec::Ctrl('@'));
        }
        return Some(Spec::Ctrl(match single(rest)? {
            c @ ('a'..='z' | 'A'..='Z') => c.to_ascii_uppercase(),
            c @ ('@' | '[' | '\\' | ']' | '^' | '_') => c,
            '/' | '-' => '_',
            '`' => '@',
            _ => return None,
        }));
    }
    if let Some(rest) = lower.strip_prefix("sh-m-") {
        let c = single(rest).filter(char::is_ascii_lowercase)?;
        return Some(Spec::ShiftMeta(c));
    }
    if let Some(rest) = lower.strip_prefix("m-") {
        if rest == "space" {
            return Some(Spec::Meta(' '));
        }
        let c = single(rest).filter(char::is_ascii_graphic)?;
        return Some(Spec::Meta(c));
    }
    if let Some(rest) = lower.strip_prefix('f') {
        // As nano reads a number: a sign and digits, and what follows them is not looked at.
        let digits = rest.strip_prefix('+').unwrap_or(rest);
        let end = digits
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(digits.len());
        return match digits[..end].parse() {
            Ok(n @ 1..=24) => Some(Spec::F(n)),
            _ => None,
        };
    }
    match lower.as_str() {
        "ins" => Some(Spec::Insert),
        "del" => Some(Spec::Delete),
        _ => None,
    }
}

/// How the help lines show a key: as it was typed, with its first letter in capitals.
fn label(name: &str) -> String {
    let at = usize::from(name.starts_with('^'));
    let mut text = name.to_string();
    if let Some(c) = text.get_mut(at..at + 1) {
        c.make_ascii_uppercase();
    }
    text
}

/// The name of a key in a message, as nano shows an unbound key.
fn spec_name(spec: Spec) -> String {
    match spec {
        Spec::Ctrl(c) => format!("^{c}"),
        Spec::Meta(' ') => "M-Space".into(),
        Spec::Meta(c) => format!("M-{}", c.to_ascii_uppercase()),
        Spec::ShiftMeta(c) => format!("Sh-M-{}", c.to_ascii_uppercase()),
        Spec::F(n) => format!("F{n}"),
        Spec::Insert => "Ins".into(),
        Spec::Delete => "Del".into(),
    }
}

/// The text of a key for the message of a mistake: nano shows `^a1` as `^A1`.
fn invalid_name(name: &str) -> String {
    label(name)
}

fn menu_letters(menu: &str) -> Result<Vec<char>, String> {
    if menu == "all" {
        return Ok(MENUS.iter().map(|m| m.1).collect());
    }
    MENUS
        .iter()
        .find(|m| m.0 == menu)
        .map(|m| vec![m.1])
        .ok_or_else(|| format!("Unknown menu: {menu}"))
}

/// The ops of a `bind` line, or what to say about it, in nano's words and order. The arguments
/// come as typed and each is cut short by the caller when it is very long.
pub fn bind(key: &str, target: Option<Target>, menu: Option<&str>) -> Result<Vec<Op>, String> {
    let spec =
        parse_key(key).ok_or_else(|| format!("Key name {} is invalid", invalid_name(key)))?;
    let target = target.ok_or("Must specify a function to bind the key to")?;
    let menu = menu.ok_or("Must specify a menu (or \"all\") in which to bind/unbind the key")?;
    let letters = menu_letters(menu)?;
    let name = match target {
        Target::Function(name) => name,
        // Typed in the main menu only; in the others nano takes it as keys of its own.
        Target::Text(text) => {
            let text: String = text.chars().take(MAX_TEXT).collect();
            return Ok(letters
                .into_iter()
                .filter_map(|l| SLOTS.find(l))
                .filter(|&slot| slot == MAIN)
                .map(|slot| Op {
                    slot,
                    key: spec,
                    label: label(key),
                    function: None,
                    text: Some(text.clone()),
                })
                .collect());
        }
    };
    let &(name, exists_in) = FUNCTIONS
        .iter()
        .find(|f| f.0 == name)
        .ok_or_else(|| format!("Unknown function: {name}"))?;
    if menu != "all" && !exists_in.contains(letters[0]) {
        return Err(format!("Function '{name}' does not exist in menu '{menu}'"));
    }
    if spec == Spec::Ctrl('[') {
        return Err("Keystroke ^[ may not be rebound".into());
    }
    let mut ops = Vec::new();
    for letter in letters.into_iter().filter(|l| exists_in.contains(*l)) {
        let Some(slot) = SLOTS.find(letter) else {
            continue;
        };
        if canonical(slot, name).is_none() {
            if menu == "all" {
                continue;
            }
            return Err(format!(
                "Function '{name}' is not available in this version yet"
            ));
        }
        ops.push(Op {
            slot,
            key: spec,
            label: label(key),
            function: Some(name),
            text: None,
        });
    }
    Ok(ops)
}

/// The ops of an `unbind` line, or what to say about it.
pub fn unbind(key: &str, menu: Option<&str>) -> Result<Vec<Op>, String> {
    let spec =
        parse_key(key).ok_or_else(|| format!("Key name {} is invalid", invalid_name(key)))?;
    let menu = menu.ok_or("Must specify a menu (or \"all\") in which to bind/unbind the key")?;
    let letters = menu_letters(menu)?;
    // Nothing is bound to Escape, so there is nothing to remove.
    if spec == Spec::Ctrl('[') {
        return Ok(Vec::new());
    }
    Ok(letters
        .into_iter()
        .filter_map(|l| SLOTS.find(l))
        .map(|slot| Op {
            slot,
            key: spec,
            label: String::new(),
            function: None,
            text: None,
        })
        .collect())
}

fn op(slot: usize, key: &str, function: Option<&'static str>) -> Op {
    Op {
        slot,
        key: parse_key(key).unwrap_or(Spec::Ctrl('@')),
        label: label(key),
        function,
        text: None,
    }
}

/// What `--modernbindings` changes, as binds and unbinds applied to the defaults.
fn modern() -> Vec<Op> {
    let main = [
        ("^X", "cut"),
        ("^Q", "exit"),
        ("^W", "writeout"),
        ("^F", "whereis"),
        ("^B", "wherewas"),
        ("^R", "replace"),
        ("^V", "paste"),
        ("^C", "copy"),
        ("^P", "location"),
        ("^T", "gotoline"),
        ("^Z", "undo"),
        ("^Y", "redo"),
        ("^A", "mark"),
        ("^D", "findprevious"),
        ("^G", "findnext"),
        ("^H", "help"),
        ("^O", "insert"),
        ("^E", "execute"),
    ];
    let mut ops: Vec<Op> = main
        .iter()
        .map(|&(key, function)| op(MAIN, key, Some(function)))
        .collect();
    ops.push(op(MAIN, "^N", None));
    // The help line of the prompts is ^H, and ^A, ^E and ^D are no longer for the answer.
    for slot in [SEARCH, REPLACE, REPLACE_WITH, GOTO, WRITE_OUT] {
        ops.push(op(slot, "^H", Some("help")));
        ops.extend(["^G", "^A", "^E", "^D"].map(|key| op(slot, key, None)));
    }
    ops.extend([
        op(HELP, "^X", None),
        op(HELP, "^Q", Some("exit")),
        op(HELP, "^H", Some("exit")),
        op(HELP, "^D", Some("findprevious")),
        op(HELP, "^G", Some("findnext")),
    ]);
    ops.extend(["^P", "^N", "^Y", "^V"].map(|key| op(HELP, key, None)));
    ops
}

/// The key the editor's code handles for a function in a slot; None for a function the editor
/// does not have.
fn canonical(slot: usize, function: &str) -> Option<Key> {
    let ctrl_arrow = Mods {
        ctrl: true,
        ..Mods::default()
    };
    let plain = |n| Key::Named(n, Mods::default());
    let (keys, editing) = DEFAULTS[slot];
    let known =
        keys.iter().any(|k| k.0 == function) || editing && EDITING.iter().any(|k| k.0 == function);
    if !known {
        return None;
    }
    Some(match (slot, function) {
        (GOTO, "firstline") => Key::Ctrl('Y'),
        (GOTO, "lastline") => Key::Ctrl('V'),
        (_, "help") => Key::Ctrl('G'),
        (_, "cancel") => Key::Ctrl('C'),
        (_, "exit") => Key::Ctrl('X'),
        (_, "writeout") => Key::Ctrl('O'),
        (_, "savefile") => Key::Ctrl('S'),
        (_, "whereis") => Key::Ctrl('F'),
        (_, "wherewas") => Key::Ctrl('B'),
        (_, "findprevious") => Key::Meta('b'),
        (_, "findnext") => Key::Meta('f'),
        (_, "replace") => Key::Ctrl('\\'),
        (_, "cut") => Key::Ctrl('K'),
        (_, "copy") => Key::Meta('6'),
        (_, "paste") => Key::Ctrl('U'),
        (_, "mark") => Key::Meta('a'),
        (_, "location") => Key::Ctrl('C'),
        (_, "complete") => Key::Ctrl(']'),
        (_, "gotoline") => Key::Ctrl('_'),
        (_, "undo") => Key::Meta('u'),
        (_, "redo") => Key::Meta('e'),
        (_, "refresh") => Key::Ctrl('L'),
        (_, "left") => plain(Named::Left),
        (_, "right") => plain(Named::Right),
        (_, "up") => plain(Named::Up),
        (_, "down") => plain(Named::Down),
        (_, "prevword") => Key::Named(Named::Left, ctrl_arrow),
        (_, "nextword") => Key::Named(Named::Right, ctrl_arrow),
        (_, "home") => Key::Ctrl('A'),
        (_, "end") => Key::Ctrl('E'),
        (_, "pageup") => Key::Ctrl('Y'),
        (_, "pagedown") => Key::Ctrl('V'),
        (_, "firstline") => Key::Meta('\\'),
        (_, "lastline") => Key::Meta('/'),
        (_, "tab") => Key::Ctrl('I'),
        (_, "enter") => Key::Ctrl('M'),
        (_, "delete") => Key::Ctrl('D'),
        (_, "backspace") => Key::Ctrl('H'),
        (_, "cutrestoffile") => Key::Meta('t'),
        (_, "casesens") => Key::Meta('c'),
        (_, "regexp") => Key::Meta('r'),
        (_, "backwards") => Key::Meta('b'),
        (_, "flipreplace") => Key::Ctrl('R'),
        (_, "older") => Key::Ctrl('P'),
        (_, "newer") => Key::Ctrl('N'),
        (_, "flipgoto") => Key::Ctrl('T'),
        _ => return None,
    })
}

struct Entry {
    /// None: the key is unbound, or types `text`.
    function: Option<&'static str>,
    text: Option<String>,
    /// None: shown in the help lines but not a key a nanorc can name.
    key: Option<Spec>,
    label: String,
}

/// What a key pressed in a menu comes to.
pub enum Outcome {
    /// The key to handle: the one pressed, or the one the function it is bound to is handled by.
    Key(Key),
    /// The key is bound to nothing: its name for the message.
    Unbound(String),
    /// The key types this text.
    Text(String),
}

pub struct Bindings {
    /// By slot, the newest binding first.
    menus: [Vec<Entry>; 8],
    /// Some key was bound as `Sh-M-x`: from then on Alt+Shift+x is not Alt+x.
    shift_meta: bool,
}

impl Bindings {
    /// The defaults, `--modernbindings` over them if asked, then the nanorc's changes.
    pub fn new(modernbindings: bool, ops: &[Op]) -> Self {
        let menus = DEFAULTS.map(|(keys, editing)| {
            let editing: Defaults = if editing { EDITING } else { &[] };
            keys.iter()
                .chain(editing)
                .flat_map(|(function, _, names)| {
                    names.iter().map(move |name| Entry {
                        function: Some(*function),
                        text: None,
                        key: parse_key(name),
                        label: (*name).to_string(),
                    })
                })
                .collect()
        });
        let mut bindings = Self {
            menus,
            shift_meta: false,
        };
        if modernbindings {
            modern().iter().for_each(|op| bindings.apply(op));
        }
        ops.iter().for_each(|op| bindings.apply(op));
        bindings
    }

    fn apply(&mut self, op: &Op) {
        self.shift_meta |= matches!(op.key, Spec::ShiftMeta(_));
        let list = &mut self.menus[op.slot];
        list.retain(|e| e.key != Some(op.key));
        list.insert(
            0,
            Entry {
                function: op.function,
                text: op.text.clone(),
                key: Some(op.key),
                label: op.label.clone(),
            },
        );
    }

    /// Nano does not start without a way out: the message for when there is none.
    pub fn check(&self) -> Result<(), String> {
        for (slot, name, menu) in [(MAIN, "Exit", "main"), (YES_NO, "Cancel", "yesno")] {
            let function = name.to_ascii_lowercase();
            if !self.menus[slot]
                .iter()
                .any(|e| e.key.is_some() && e.function == Some(function.as_str()))
            {
                return Err(format!(
                    "No key is bound to function '{name}' in menu '{menu}'.  Exiting.\n\n\
                     If needed, use the -I option to adjust your nanorc settings."
                ));
            }
        }
        Ok(())
    }

    /// The key the code of the menu in `slot` handles for the key pressed.
    pub fn translate(&self, slot: usize, key: Key) -> Outcome {
        let spec = match key {
            Key::Ctrl(c) => Spec::Ctrl(c),
            Key::Meta(c) => Spec::Meta(c),
            Key::ShiftMeta(c) if self.shift_meta => Spec::ShiftMeta(c),
            Key::ShiftMeta(c) => return Outcome::Key(Key::Meta(c)),
            Key::Named(Named::F(n @ 1..=12), mods) if !mods.ctrl && !mods.alt => {
                Spec::F(if mods.shift { n + 12 } else { n })
            }
            Key::Named(
                Named::Insert,
                Mods {
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
            ) => Spec::Insert,
            Key::Named(
                Named::Delete,
                Mods {
                    ctrl: false,
                    alt: false,
                    shift: false,
                },
            ) => Spec::Delete,
            other => return Outcome::Key(other),
        };
        match self.menus[slot].iter().find(|e| e.key == Some(spec)) {
            Some(Entry {
                function: Some(function),
                ..
            }) => Outcome::Key(
                canonical(slot, function).unwrap_or(Key::Named(Named::Unknown, Mods::default())),
            ),
            Some(Entry {
                text: Some(text), ..
            }) => Outcome::Text(text.clone()),
            Some(_) => Outcome::Unbound(spec_name(spec)),
            None if matches!(spec, Spec::ShiftMeta(_)) => Outcome::Unbound(spec_name(spec)),
            None => Outcome::Key(key),
        }
    }

    /// The help lines of a menu: the first key of each function that has one, and its label.
    pub fn help_lines(&self, slot: usize) -> Vec<(String, &'static str)> {
        let (keys, _) = DEFAULTS[slot];
        keys.iter()
            .filter(|(_, text, _)| !text.is_empty())
            .filter_map(|&(function, text, _)| {
                let first = self.menus[slot]
                    .iter()
                    .find(|e| e.function == Some(function))?;
                Some((first.label.clone(), text))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(key: &str, function: &str, menu: &str) -> Vec<Op> {
        bind(key, Some(Target::Function(function)), Some(menu)).unwrap()
    }

    #[test]
    fn key_names() {
        for (name, want) in [
            ("^X", Spec::Ctrl('X')),
            ("^x", Spec::Ctrl('X')),
            ("^Space", Spec::Ctrl('@')),
            ("^/", Spec::Ctrl('_')),
            ("^]", Spec::Ctrl(']')),
            ("M-X", Spec::Meta('x')),
            ("m-space", Spec::Meta(' ')),
            ("M-\"", Spec::Meta('"')),
            ("Sh-M-a", Spec::ShiftMeta('a')),
            ("F1", Spec::F(1)),
            ("f24", Spec::F(24)),
            ("F+3", Spec::F(3)),
            ("ins", Spec::Insert),
            ("Del", Spec::Delete),
        ] {
            assert_eq!(parse_key(name), Some(want), "{name}");
        }
        for name in [
            "",
            "^",
            "^1",
            "^?",
            "M-",
            "M-ab",
            "M-é",
            "Sh-M-1",
            "Sh-F1",
            "F0",
            "F25",
            "F-1",
            "Fa",
            "Home",
            "Insx",
            "Esc",
            "M-Del",
            &"^".repeat(5000),
        ] {
            assert_eq!(parse_key(name), None, "{name}");
        }
    }

    #[test]
    fn bind_mistakes_in_nanos_order() {
        let text = |key, target, menu| bind(key, target, menu).unwrap_err();
        let f = |s| Some(Target::Function(s));
        assert_eq!(text("^1", f("cut"), Some("main")), "Key name ^1 is invalid");
        assert_eq!(text("^a1", None, None), "Key name ^A1 is invalid");
        assert_eq!(
            text("^X", None, None),
            "Must specify a function to bind the key to"
        );
        assert_eq!(
            text("^X", f("cut"), None),
            "Must specify a menu (or \"all\") in which to bind/unbind the key"
        );
        assert_eq!(text("^[", f("nosuch"), Some("foo")), "Unknown menu: foo");
        assert_eq!(
            text("^[", f("nosuch"), Some("main")),
            "Unknown function: nosuch"
        );
        assert_eq!(
            text("^[", f("cut"), Some("help")),
            "Function 'cut' does not exist in menu 'help'"
        );
        assert_eq!(
            text("^[", f("cut"), Some("main")),
            "Keystroke ^[ may not be rebound"
        );
        assert!(text("^X", f("zap"), Some("main")).contains("not available"));
        assert_eq!(
            bind("^X", Some(Target::Text("ab")), Some("main")).unwrap()[0]
                .text
                .as_deref(),
            Some("ab")
        );
        assert_eq!(
            unbind("^X", None).unwrap_err(),
            "Must specify a menu (or \"all\") in which to bind/unbind the key"
        );
        assert_eq!(
            unbind("^1", Some("main")).unwrap_err(),
            "Key name ^1 is invalid"
        );
        assert_eq!(unbind("^X", Some("foo")).unwrap_err(), "Unknown menu: foo");
    }

    #[test]
    fn bound_keys_change_the_function_and_the_lines() {
        let main = |b: &Bindings| b.help_lines(MAIN);
        let b = Bindings::new(false, &ops("^X", "cut", "main"));
        assert!(matches!(
            b.translate(MAIN, Key::Ctrl('X')),
            Outcome::Key(Key::Ctrl('K'))
        ));
        // ^X is taken from Exit, which shows its next key.
        assert!(main(&b).contains(&("^X".into(), "Cut")));
        assert!(main(&b).contains(&("F2".into(), "Exit")));
        let mut all = ops("^G", "cut", "all");
        all.extend(unbind("F1", Some("main")).unwrap());
        let b = Bindings::new(false, &all);
        assert!(matches!(
            b.translate(MAIN, Key::Named(Named::F(1), Mods::default())),
            Outcome::Unbound(name) if name == "F1"
        ));
        assert!(matches!(
            b.translate(MAIN, Key::Ctrl('G')),
            Outcome::Key(Key::Ctrl('K'))
        ));
        // Help has no key left: it leaves the lines.
        assert!(!main(&b).iter().any(|l| l.1 == "Help"));
    }

    #[test]
    fn all_menus_and_the_ones_the_editor_has_not() {
        // Cut is in nine of the thirteen menus, the editor has the main and the prompts.
        assert_eq!(ops("^X", "cut", "all").len(), 1);
        assert_eq!(ops("^X", "cancel", "all").len(), 6);
        assert!(ops("^X", "cut", "browser").is_empty());
        assert!(unbind("^X", Some("all")).unwrap().len() == 8);
    }

    #[test]
    fn modern_bindings_come_first() {
        let b = Bindings::new(true, &[]);
        assert!(matches!(
            b.translate(MAIN, Key::Ctrl('Q')),
            Outcome::Key(Key::Ctrl('X'))
        ));
        assert!(matches!(
            b.translate(MAIN, Key::Ctrl('N')),
            Outcome::Unbound(_)
        ));
        // The nanorc has the last word.
        let b = Bindings::new(true, &unbind("^Q", Some("main")).unwrap());
        assert!(matches!(
            b.translate(MAIN, Key::Ctrl('Q')),
            Outcome::Unbound(_)
        ));
        assert!(b.check().is_ok());
    }

    #[test]
    fn nano_wants_a_way_out() {
        let mut ops = unbind("^X", Some("main")).unwrap();
        assert!(Bindings::new(false, &ops).check().is_ok());
        ops.extend(unbind("F2", Some("main")).unwrap());
        let message = Bindings::new(false, &ops).check().unwrap_err();
        assert!(message.starts_with("No key is bound to function 'Exit' in menu 'main'."));
    }

    #[test]
    fn hostile_lists_stay_small() {
        let mut ops = Vec::new();
        for i in 0..20_000 {
            ops.extend(
                unbind(
                    &format!("M-{}", char::from(33 + (i % 90) as u8)),
                    Some("all"),
                )
                .unwrap(),
            );
        }
        let b = Bindings::new(false, &ops);
        assert!(b.menus.iter().all(|m| m.len() < 300));
    }
}
