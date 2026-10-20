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
        let term = Term::spawn(
            command,
            &scenario.args,
            &work,
            &home,
            scenario.cols,
            scenario.rows,
        )
        .map_err(|e| format!("{name} ({}): {e}", command.join(" ")))?;
        terms.push(term);
    }

    let mut what = "at the start".to_string();
    compare_screens(&mut terms, scenario, &what)?;
    if let Some(log) = show.as_deref_mut() {
        log_screen(log, &what, terms[0].screen());
    }
    for step in &scenario.steps {
        what = format!("after line {}, {}", step.line, step.text);
        for term in &mut terms {
            match &step.action {
                Action::Keys(bytes) => term.send(bytes),
                Action::Resize { cols, rows } => term.resize(*cols, *rows),
            }
            .map_err(io_err)?;
        }
        compare_screens(&mut terms, scenario, &what)?;
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
    if let Some(report) = diff_screens(&terms, scenario) {
        return Err(format!("screens differ {end}\n{report}"));
    }
    if exits[0] != exits[1] {
        return Err(format!(
            "exit differs {end}\n  nano:   {}\n  editor: {}",
            exit_text(&exits[0]),
            exit_text(&exits[1])
        ));
    }
    let files = NAMES.map(|name| list_files(&dir.join(name).join("work")));
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
fn compare_screens(terms: &mut [Term], scenario: &Scenario, what: &str) -> Result<(), String> {
    for quiet in [QUIET, QUIET_SURE] {
        for (name, term) in NAMES.iter().zip(terms.iter_mut()) {
            term.settle(quiet)
                .map_err(|e| format!("{name}, {what}: {e}"))?;
        }
        let Some(report) = diff_screens(terms, scenario) else {
            return Ok(());
        };
        if quiet == QUIET_SURE {
            return Err(format!("screens differ {what}\n{report}"));
        }
    }
    Ok(())
}

/// One screen cell: its text and its look.
#[derive(PartialEq)]
struct Cell {
    text: String,
    style: String,
}

fn cells(screen: &vt100::Screen) -> Vec<Vec<Cell>> {
    let (rows, cols) = screen.size();
    let mut out: Vec<Vec<Cell>> = (0..rows.get())
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
        .collect();
    // The program name and version at the left of the title bar (drawn in inverse) may differ:
    // GNU nano shows "GNU nano 8.4", the editor its own name. They read as asterisks.
    let title_bar = screen.cell(0, 0).is_some_and(|c| c.inverse());
    if let Some(title) = out.first_mut().filter(|_| title_bar) {
        let start = title.iter().position(|c| c.text != " ").unwrap_or(0);
        let mut end = start;
        while end < title.len()
            && !(title[end].text == " " && title.get(end + 1).is_none_or(|c| c.text == " "))
        {
            end += 1;
        }
        for cell in &mut title[start..end] {
            cell.text = "*".into();
        }
    }
    out
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
fn diff_screens(terms: &[Term], scenario: &Scenario) -> Option<String> {
    let screens = [terms[0].screen(), terms[1].screen()];
    let [a, b] = screens.map(cells);
    let mut report = String::new();
    if a.len() != b.len() || a[0].len() != b[0].len() {
        let size = |c: &Vec<Vec<Cell>>| format!("{}x{}", c[0].len(), c.len());
        return Some(format!("  size: nano {}, editor {}\n", size(&a), size(&b)));
    }
    let ignored = |row: usize, col: usize| {
        let (row, col) = (row as u16, col as u16);
        scenario
            .differ
            .iter()
            .any(|r: &Region| r.contains(row, col))
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
