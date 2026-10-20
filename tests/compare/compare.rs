// SPDX-License-Identifier: MIT
//! Runs a scenario on GNU nano and on the editor under test, and reports the differences.

use crate::scenario::{Action, Region, Scenario};
use crate::term::{Exit, Term};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

/// The pause that ends a step: the editor is done drawing.
const QUIET: Duration = Duration::from_millis(40);
/// The longer pause tried before a difference is reported, in case an editor was only slow.
const QUIET_SURE: Duration = Duration::from_millis(400);

const NAMES: [&str; 2] = ["nano", "editor"];

/// Runs `scenario` with `nano` and `editor` (commands with their arguments) in fresh
/// directories under `dir`. Err is the report of the first difference. With `show`, nano's
/// screen after every step and its files at the end go to `log`, to check a new scenario.
pub fn run(
    scenario: &Scenario,
    nano: &[String],
    editor: &[String],
    dir: &Path,
    show: Option<&mut String>,
) -> Result<(), String> {
    let mut show = show;
    let io_err = |e: io::Error| format!("error: {e}");
    let mut terms = Vec::new();
    for (name, command) in NAMES.iter().zip([nano, editor]) {
        let work = dir.join(name).join("work");
        let home = dir.join(name).join("home");
        fs::create_dir_all(&home).map_err(io_err)?;
        fs::create_dir_all(&work).map_err(io_err)?;
        for (file, bytes) in &scenario.files {
            let path = work.join(file);
            fs::create_dir_all(path.parent().unwrap_or(&work)).map_err(io_err)?;
            fs::write(path, bytes).map_err(io_err)?;
        }
        for (file, bytes) in &scenario.home_files {
            let path = home.join(file);
            fs::create_dir_all(path.parent().unwrap_or(&home)).map_err(io_err)?;
            fs::write(path, bytes).map_err(io_err)?;
        }
        let term = Term::spawn(
            command,
            &scenario.args,
            &work,
            &home,
            !scenario.home_files.is_empty(),
            scenario.cols,
            scenario.rows,
        )
        .map_err(|e| format!("{name} ({}): {e}", command.join(" ")))?;
        terms.push(term);
    }

    let mut what = "at the start".to_string();
    let mut differ = &scenario.differ;
    compare_screens(&mut terms, differ, &what)?;
    if let Some(log) = show.as_deref_mut() {
        log_screen(log, &what, terms[0].screen());
    }
    for step in &scenario.steps {
        what = format!("after line {}, {}", step.line, step.text);
        differ = &step.differ;
        // What a rewrite replaces is compared first, so that it cannot hide what was saved.
        if let Action::Write { .. } = step.action {
            let mut files = NAMES.map(|name| list_files(&dir.join(name).join("work")));
            drop_editor_locks(&mut files);
            if let Some(r) = diff_files(&files[0], &files[1]) {
                return Err(format!(
                    "files differ before line {}, {}\n{r}",
                    step.line, step.text
                ));
            }
        }
        for (name, term) in NAMES.iter().zip(&mut terms) {
            match &step.action {
                Action::Keys(bytes) => term.send(bytes),
                Action::Resize { cols, rows } => term.resize(*cols, *rows),
                Action::Write { name: file, bytes } => {
                    fs::write(dir.join(name).join("work").join(file), bytes)
                }
                // Both editors wait together, below.
                Action::Pause(_) => Ok(()),
            }
            .map_err(io_err)?;
        }
        if let Action::Pause(ms) = step.action {
            std::thread::sleep(Duration::from_millis(ms));
        }
        compare_screens(&mut terms, differ, &what)?;
        if let Some(log) = show.as_deref_mut() {
            log_screen(log, &what, terms[0].screen());
        }
    }

    let mut exits = Vec::new();
    for (name, term) in NAMES.iter().zip(&mut terms) {
        exits.push(
            term.finish(QUIET_SURE)
                .map_err(|e| format!("{name}: {e}"))?,
        );
    }
    let end = "at the end";
    // 126 and 127: the shell's codes for a command that could not be executed (from setsid).
    if let Exit::Code(code @ (126 | 127)) = exits[0] {
        return Err(format!(
            "nano could not run (exit status {code}): {}\n  check NANO",
            nano.join(" ")
        ));
    }
    if let Some(report) = diff_screens(&terms, differ) {
        return Err(format!("screens differ {end}\n{report}"));
    }
    if exits[0] != exits[1] {
        return Err(format!(
            "exit differs {end}\n  nano:   {}\n  editor: {}",
            exit_text(&exits[0]),
            exit_text(&exits[1])
        ));
    }
    let mut files = NAMES.map(|name| list_files(&dir.join(name).join("work")));
    // Killed while running, the editor leaves its lock file, as a killed nano -G would.
    if exits[1] == Exit::Running {
        drop_editor_locks(&mut files);
    }
    if let Some(log) = show {
        log_screen(log, end, terms[0].screen());
        let _ = writeln!(log, "nano {}; its files:", exit_text(&exits[0]));
        for (name, entry) in &files[0] {
            let text = String::from_utf8_lossy(&entry.bytes);
            let _ = writeln!(log, "  {name} (mode {:o}): {text:?}", entry.mode);
        }
    }
    diff_files(&files[0], &files[1]).map_or(Ok(()), |r| Err(format!("files differ {end}\n{r}")))
}

fn log_screen(log: &mut String, what: &str, screen: &vt100::Screen) {
    let _ = writeln!(log, "nano's screen {what} (cursor: {}):", cursor(screen));
    for (r, row) in cells(screen).iter().enumerate() {
        let _ = writeln!(log, "  {r:3} |{}", line(row));
    }
}

fn line(row: &[Cell]) -> String {
    let s: String = row.iter().map(|c| c.text.as_str()).collect();
    s.trim_end().to_string()
}

fn exit_text(exit: &Exit) -> String {
    match exit {
        Exit::Code(code) => format!("exited with status {code}"),
        Exit::Signal(signal) => format!("killed by signal {signal}"),
        Exit::Running => "still running (killed by the test)".into(),
    }
}

/// Compares after a short pause and, if the screens differ, again after a long one.
fn compare_screens(terms: &mut [Term], differ: &[Region], what: &str) -> Result<(), String> {
    for quiet in [QUIET, QUIET_SURE] {
        for (name, term) in NAMES.iter().zip(terms.iter_mut()) {
            term.settle(quiet)
                .map_err(|e| format!("{name}, {what}: {e}"))?;
        }
        let Some(report) = diff_screens(terms, differ) else {
            return Ok(());
        };
        if quiet == QUIET_SURE {
            let mut report = format!("screens differ {what}\n{report}");
            for (name, term) in NAMES.iter().zip(terms.iter()) {
                if term.closed() {
                    let _ = writeln!(report, "  {name} has exited");
                }
            }
            return Err(report);
        }
    }
    Ok(())
}

/// One screen cell: its text and its look.
#[derive(Clone, PartialEq)]
struct Cell {
    text: String,
    style: String,
}

fn cells(screen: &vt100::Screen) -> Vec<Vec<Cell>> {
    let (rows, cols) = screen.size();
    (0..rows.get())
        .map(|row| {
            (0..cols.get())
                .map(|col| {
                    let cell = screen.cell(row, col);
                    let text = cell.map_or("", |c| c.contents());
                    // A blank cell reads as a space; the right half of a wide character stays empty.
                    let blank = text.is_empty() && !cell.is_some_and(|c| c.is_wide_continuation());
                    Cell {
                        text: if blank { " " } else { text }.into(),
                        style: cell.map_or_else(String::new, style),
                    }
                })
                .collect()
        })
        .collect()
}

/// The program name and version at the left of the title bar may differ: nano shows "GNU nano
/// 8.4", the editor its own name. When nano's row 0 starts with that, the columns of nano's name
/// and of the editor's first group of words (up to two blanks) read as `*` on both screens.
fn mask_title(nano: &mut [Vec<Cell>], editor: &mut [Vec<Cell>]) {
    const BRANDING: &str = "GNU nano 8.4";
    let (Some(a), Some(b)) = (nano.first_mut(), editor.first_mut()) else {
        return;
    };
    let first = |row: &[Cell]| row.iter().position(|c| c.text != " ");
    let Some(start) = first(a) else { return };
    let text: String = a[start..].iter().map(|c| c.text.as_str()).collect();
    if !text.starts_with(BRANDING) {
        return;
    }
    let mut end = start + BRANDING.len();
    let mut from = first(b).unwrap_or(start);
    let mut to = from;
    while to < b.len() && !(b[to].text == " " && b.get(to + 1).is_none_or(|c| c.text == " ")) {
        to += 1;
    }
    from = from.min(start);
    end = end.max(to).min(a.len()).min(b.len());
    for row in [a, b] {
        for cell in &mut row[from..end] {
            cell.text = "*".into();
        }
    }
}

/// nano's welcome message (shown when it starts without a file) names nano; the editor's leaves
/// the name out. The two are masked against each other only in the status bar (the third row from
/// the bottom, or the last with `-x`), and only when each shows its whole message in brackets,
/// centred the way nano centres messages, or, when that does not fit, the message from the left
/// edge cut at the right edge; all of a message's cells must have the look of nano's message,
/// and the rest of the row must be blank and plain. Then both rows read as `*`.
fn mask_welcome(nano: &mut [Vec<Cell>], editor: &mut [Vec<Cell>]) {
    const NANO: &str = "Welcome to nano.  For basic help, type Ctrl+G.";
    const EDITOR: &str = "For basic help, type Ctrl+G.";
    let rows = nano.len().min(editor.len());
    for r in [rows.wrapping_sub(3), rows.wrapping_sub(1)] {
        if r >= rows {
            continue;
        }
        let Some(look) = shown(&nano[r], NANO, None) else {
            continue;
        };
        if shown(&editor[r], EDITOR, Some(&look)).is_some() {
            for cell in nano[r].iter_mut().chain(editor[r].iter_mut()) {
                cell.text = "*".into();
                cell.style = "masked".into();
            }
        }
    }
}

/// Whether `row` shows `message` as a message, with the look `look` if given; its look if so.
fn shown(row: &[Cell], message: &str, look: Option<&str>) -> Option<String> {
    let width = row.len();
    let bracketed = format!("[ {message} ]");
    let len = bracketed.chars().count();
    let (start, text) = if len <= width {
        ((width - len) / 2, bracketed)
    } else {
        (0, message.chars().take(width).collect())
    };
    let end = start + text.chars().count();
    let look = look.map_or_else(
        || row.get(start).map(|c| c.style.clone()),
        |l| Some(l.into()),
    )?;
    let matches = row.iter().enumerate().all(|(i, cell)| {
        if (start..end).contains(&i) {
            cell.style == look
        } else {
            cell.text == " " && cell.style == "plain"
        }
    });
    let row_text: String = row[start..end].iter().map(|c| c.text.as_str()).collect();
    (matches && row_text == text).then_some(look)
}

fn style(cell: &vt100::Cell) -> String {
    let mut parts = Vec::new();
    for (on, name) in [
        (cell.bold(), "bold"),
        (cell.dim(), "dim"),
        (cell.italic(), "italic"),
        (cell.underline(), "underline"),
        (cell.inverse(), "inverse"),
    ] {
        if on {
            parts.push(name.to_string());
        }
    }
    for (color, name) in [(cell.fgcolor(), "fg"), (cell.bgcolor(), "bg")] {
        match color {
            vt100::Color::Default => {}
            vt100::Color::Idx(i) => parts.push(format!("{name}={i}")),
            vt100::Color::Rgb(r, g, b) => parts.push(format!("{name}=#{r:02x}{g:02x}{b:02x}")),
        }
    }
    if parts.is_empty() {
        "plain".into()
    } else {
        parts.join(" ")
    }
}

fn cursor(screen: &vt100::Screen) -> String {
    let (row, col) = screen.cursor_position();
    if screen.hide_cursor() {
        "hidden".into()
    } else {
        format!("row {row}, column {col}")
    }
}

/// The differences between the two screens, as a readable report; None if they match.
fn diff_screens(terms: &[Term], differ: &[Region]) -> Option<String> {
    let screens = [terms[0].screen(), terms[1].screen()];
    let [mut a, mut b] = screens.map(cells);
    mask_title(&mut a, &mut b);
    mask_welcome(&mut a, &mut b);
    let mut report = String::new();
    if a.len() != b.len() || a[0].len() != b[0].len() {
        let size = |c: &Vec<Vec<Cell>>| format!("{}x{}", c[0].len(), c.len());
        return Some(format!("  size: nano {}, editor {}\n", size(&a), size(&b)));
    }
    let ignored = |row: usize, col: usize| {
        let (row, col) = (row as u16, col as u16);
        differ.iter().any(|r: &Region| r.contains(row, col))
    };
    // Whether a cell's text differs (or its look, with `look`).
    let differs = |row: usize, col: usize, look: bool| {
        let (x, y) = (&a[row][col], &b[row][col]);
        !ignored(row, col)
            && if look {
                x.style != y.style
            } else {
                x.text != y.text
            }
    };
    let (text, look) = (false, true);
    let rows: Vec<usize> = (0..a.len())
        .filter(|&r| (0..a[r].len()).any(|c| differs(r, c, text) || differs(r, c, look)))
        .collect();
    let (ca, cb) = (cursor(screens[0]), cursor(screens[1]));
    if rows.is_empty() && ca == cb {
        return None;
    }

    let _ = writeln!(report, "  nano's screen (> marks the rows that differ):");
    for (r, row) in a.iter().enumerate() {
        let mark = if rows.contains(&r) { '>' } else { ' ' };
        let _ = writeln!(report, "  {mark}{r:3} |{}", line(row));
    }
    for &r in &rows {
        let width = a[r].len();
        let text_cols: Vec<usize> = (0..width).filter(|&c| differs(r, c, text)).collect();
        if !text_cols.is_empty() {
            let mut carets = vec![' '; width];
            for &c in &text_cols {
                carets[c] = '^';
            }
            let carets: String = carets.into_iter().collect();
            let _ = writeln!(report, "  row {r}:");
            let _ = writeln!(report, "    nano:   |{}", line(&a[r]));
            let _ = writeln!(report, "    editor: |{}", line(&b[r]));
            let _ = writeln!(report, "             {}", carets.trim_end());
        }
        // The look, in runs of columns with the same pair of styles.
        let mut col = 0;
        while col < width {
            if !differs(r, col, look) {
                col += 1;
                continue;
            }
            let start = col;
            while col < width
                && differs(r, col, look)
                && a[r][col].style == a[r][start].style
                && b[r][col].style == b[r][start].style
            {
                col += 1;
            }
            let _ = writeln!(
                report,
                "  row {r}, columns {start}-{}: nano {}, editor {}",
                col - 1,
                a[r][start].style,
                b[r][start].style
            );
        }
    }
    if ca != cb {
        let _ = writeln!(report, "  cursor: nano {ca}, editor {cb}");
    }
    Some(report)
}

/// A file left in the work directory: its mode and contents.
#[derive(PartialEq)]
struct Entry {
    mode: u32,
    bytes: Vec<u8>,
}

fn list_files(dir: &Path) -> BTreeMap<String, Entry> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .strip_prefix(dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let bytes = if meta.is_dir() {
                todo.push(path.clone());
                b"(directory)".to_vec()
            } else if meta.is_symlink() {
                let target = fs::read_link(&path).unwrap_or_default();
                format!("(symlink to {})", target.display()).into_bytes()
            } else {
                fs::read(&path).unwrap_or_default()
            };
            let mode = meta.permissions().mode() & 0o7777;
            out.insert(name, Entry { mode, bytes });
        }
    }
    out
}

/// The editor always locks its file (nano only with -G): while it runs, or when it was killed,
/// its lock files (`.NAME.swp`) that nano does not have are not compared.
fn drop_editor_locks(files: &mut [BTreeMap<String, Entry>; 2]) {
    let lock = |name: &str| {
        let base = name.rsplit('/').next().unwrap_or(name);
        base.starts_with('.') && base.ends_with(".swp")
    };
    let [nano, editor] = files;
    editor.retain(|name, _| !lock(name) || nano.contains_key(name));
}

fn diff_files(a: &BTreeMap<String, Entry>, b: &BTreeMap<String, Entry>) -> Option<String> {
    let mut report = String::new();
    for name in a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))) {
        match (a.get(name), b.get(name)) {
            (Some(_), None) => {
                let _ = writeln!(report, "  {name}: only nano has it");
            }
            (None, Some(_)) => {
                let _ = writeln!(report, "  {name}: only the editor has it");
            }
            (Some(x), Some(y)) if x != y => {
                if x.mode != y.mode {
                    let _ = writeln!(
                        report,
                        "  {name}: mode nano {:o}, editor {:o}",
                        x.mode, y.mode
                    );
                }
                if x.bytes != y.bytes {
                    let lines = |b: &[u8]| -> Vec<String> {
                        b.split_inclusive(|&c| c == b'\n')
                            .map(|l| format!("{:?}", String::from_utf8_lossy(l)))
                            .collect()
                    };
                    let (la, lb) = (lines(&x.bytes), lines(&y.bytes));
                    let i = (0..).find(|&i| la.get(i) != lb.get(i)).unwrap_or(0);
                    let none = || "(no line)".to_string();
                    let _ = writeln!(report, "  {name}: contents differ from line {}", i + 1);
                    let _ = writeln!(
                        report,
                        "    nano:   {}",
                        la.get(i).cloned().unwrap_or_else(none)
                    );
                    let _ = writeln!(
                        report,
                        "    editor: {}",
                        lb.get(i).cloned().unwrap_or_else(none)
                    );
                }
            }
            _ => {}
        }
    }
    (!report.is_empty()).then_some(report)
}

#[cfg(test)]
mod tests {
    use super::{Cell, mask_title, mask_welcome};

    fn screen(row0: &str) -> Vec<Vec<Cell>> {
        let cell = |c: char| Cell {
            text: c.to_string(),
            style: "inverse".into(),
        };
        vec![format!("{row0:<40}").chars().map(cell).collect()]
    }

    fn masked(nano: &str, editor: &str) -> (String, String) {
        let (mut a, mut b) = (screen(nano), screen(editor));
        mask_title(&mut a, &mut b);
        let text = |s: &[Vec<Cell>]| s[0].iter().map(|c| c.text.as_str()).collect::<String>();
        (text(&a), text(&b))
    }

    #[test]
    fn an_editor_name_of_another_length_is_masked() {
        let (a, b) = masked(
            "  GNU nano 8.4      notes.txt",
            "  Editor 0.1.0      notes.txt",
        );
        assert_eq!(a, b);
        assert!(a.starts_with("  ************ "), "{a}");
        let (a, b) = masked(
            "  GNU nano 8.4      notes.txt",
            "  Ed 1              notes.txt",
        );
        assert_eq!(a, b);
        assert_eq!(a.trim_end(), "  ************      notes.txt");
    }

    #[test]
    fn a_file_name_is_never_masked() {
        // On a narrow screen nano leaves its name out: the file name comes first.
        let (a, b) = masked("    abc.txt", "    xyz.txt");
        assert_ne!(a, b);
        let (a, b) = masked("  GNU nano 8.4   abc.txt", "  GNU nano 8.4   xyz.txt");
        assert!(a.trim_end().ends_with("abc.txt"), "{a}");
        assert!(b.trim_end().ends_with("xyz.txt"), "{b}");
    }

    /// A screen of three rows, the first two blank and plain, the status bar centred in 60
    /// columns, inverse.
    fn status(message: &str, centred: bool) -> Vec<Vec<Cell>> {
        let blank = || Cell {
            text: " ".into(),
            style: "plain".into(),
        };
        let mut row = vec![blank(); 60];
        let len = message.chars().count();
        let start = if centred { (60 - len) / 2 } else { 0 };
        for (i, c) in message.chars().enumerate() {
            row[start + i] = Cell {
                text: c.to_string(),
                style: "inverse".into(),
            };
        }
        vec![vec![blank(); 60], vec![blank(); 60], row]
    }

    fn masked_welcome(nano: Vec<Vec<Cell>>, editor: Vec<Vec<Cell>>) -> bool {
        let (mut a, mut b) = (nano, editor);
        mask_welcome(&mut a, &mut b);
        a[2].iter().chain(&b[2]).all(|c| c.text == "*")
    }

    #[test]
    fn only_the_two_welcome_messages_are_masked() {
        let nano = || status("[ Welcome to nano.  For basic help, type Ctrl+G. ]", true);
        let ours = "[ For basic help, type Ctrl+G. ]";
        assert!(masked_welcome(nano(), status(ours, true)));
        // Not centred, another look, another text, another row: compared as usual.
        assert!(!masked_welcome(nano(), status(ours, false)));
        let mut plain = status(ours, true);
        for c in &mut plain[2] {
            c.style = "plain".into();
        }
        assert!(!masked_welcome(nano(), plain));
        assert!(!masked_welcome(nano(), status("[ Something else ]", true)));
        let mut moved = status(ours, true);
        moved.swap(1, 2);
        assert!(!masked_welcome(nano(), moved));
    }
}
