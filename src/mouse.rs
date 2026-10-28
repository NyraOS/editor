// SPDX-License-Identifier: MIT
//! Mouse reports from the terminal. The editor asks for xterm's mode 1006 (see `screen`), so a
//! report is `ESC [ < BUTTON ; COLUMN ; ROW M` for a press and `... m` for a release, the
//! numbers in decimal and counted from 1.
//!
//! The bytes of a report come from the terminal, but a program that writes to the terminal can
//! write them too, and anything the terminal does not understand ends up as input. So a report
//! is read strictly and nothing of one is ever typed as text: a report that is not exactly of
//! this form is dropped whole (up to its final byte), however long it is, and a report that is
//! well formed but means nothing to the editor is dropped too.

/// Where a report is at the most: three numbers of five digits and their separators, and the
/// three bytes before and the final one. Longer is not a report.
const MAX_LEN: usize = 24;

/// What the editor does with the mouse: a click of the left button, and a turn of the wheel.
/// Positions are the columns and rows of the screen, from 0. Every other report does nothing but
/// count as a key, as in nano (it wears the message on the status bar down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    Click { col: usize, row: usize },
    Wheel { up: bool, col: usize, row: usize },
    Other,
}

/// A report the editor understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    /// The left button went down, or came up, at this place.
    Press(usize, usize),
    Release(usize, usize),
    Wheel(Mouse),
    /// A well formed report of something the editor does not use (another button, a motion).
    Other,
}

/// What reading the start of the input gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// A report, and how many bytes it took.
    Report(Report, usize),
    /// Not a report: this many bytes are to be dropped; with `more`, the rest of the sequence
    /// has not arrived and what is parameters of it is to be dropped as it does.
    Drop { len: usize, more: bool },
    /// The start of a report; more bytes are needed.
    Incomplete,
}

/// Reads a report from the start of `b`, which begins with `ESC [ <`. A sequence of that form
/// (parameter bytes 0x20 to 0x3f, then a final byte 0x40 to 0x7e) is dropped whole, report or
/// not; so is a part of one that is too long, with the rest of it as it comes.
pub fn read_sgr(b: &[u8]) -> Read {
    debug_assert!(b.starts_with(b"\x1b[<"));
    let mut i = 3;
    while let Some(&c) = b.get(i) {
        if i >= MAX_LEN {
            return Read::Drop { len: i, more: true };
        }
        match c {
            0x20..=0x3f => i += 1,
            0x40..=0x7e => {
                let report = (c == b'M' || c == b'm')
                    .then(|| numbers(&b[3..i]))
                    .flatten()
                    .map(|n| report(n, c == b'M'));
                return match report {
                    Some(r) => Read::Report(r, i + 1),
                    None => Read::Drop {
                        len: i + 1,
                        more: false,
                    },
                };
            }
            // Not part of the sequence: it is read again as what it is.
            _ => {
                return Read::Drop {
                    len: i,
                    more: false,
                };
            }
        }
    }
    Read::Incomplete
}

/// The three numbers of a report: button, column, row. Each has one to five digits, and the
/// column and the row are not 0.
fn numbers(params: &[u8]) -> Option<[usize; 3]> {
    let mut found = [0; 3];
    let mut parts = params.split(|&c| c == b';');
    for slot in &mut found {
        let digits = parts.next()?;
        if digits.is_empty() || digits.len() > 5 {
            return None;
        }
        *slot = digits.iter().try_fold(0, |n, &d| {
            d.is_ascii_digit().then(|| n * 10 + usize::from(d - b'0'))
        })?;
    }
    (parts.next().is_none() && found[1] > 0 && found[2] > 0).then_some(found)
}

/// What the button code means. The low two bits are the button, 4, 8 and 16 add Shift, Alt and
/// Ctrl (nano ignores them), 32 is a motion and 64 is the wheel, whose buttons 0 and 1 are up
/// and down.
fn report([code, col, row]: [usize; 3], press: bool) -> Report {
    let (col, row) = (col - 1, row - 1);
    match code & !(4 | 8 | 16) {
        0 if press => Report::Press(col, row),
        0 => Report::Release(col, row),
        64 | 65 if press => Report::Wheel(Mouse::Wheel {
            up: code & 1 == 0,
            col,
            row,
        }),
        _ => Report::Other,
    }
}

/// The old report, `ESC [ M` and three bytes, which a terminal that does not know mode 1006 sends.
/// It is not read (the bytes are not text, but they look like it): all of it is dropped. How many
/// bytes that is, or 0 when they are not all there yet. The three bytes are never below a space,
/// so an Escape among them is the start of the next sequence, and is left.
pub fn old_report(b: &[u8]) -> usize {
    debug_assert!(b.starts_with(b"\x1b[M"));
    match b[3..].iter().take(3).position(|&c| c < 0x20) {
        Some(n) => 3 + n,
        None if b.len() < 6 => 0,
        None => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &str) -> Read {
        read_sgr(s.as_bytes())
    }

    #[test]
    fn a_click_is_a_press_and_a_release() {
        assert_eq!(read("\x1b[<0;10;5M"), Read::Report(Report::Press(9, 4), 10));
        assert_eq!(
            read("\x1b[<0;10;5mxyz"),
            Read::Report(Report::Release(9, 4), 10)
        );
        assert_eq!(read("\x1b[<0;1;1M"), Read::Report(Report::Press(0, 0), 9));
        assert_eq!(
            read("\x1b[<0;65535;65535M"),
            Read::Report(Report::Press(65534, 65534), 17)
        );
    }

    #[test]
    fn the_wheel_and_the_modifiers() {
        let wheel = |up| Report::Wheel(Mouse::Wheel { up, col: 4, row: 4 });
        assert_eq!(read("\x1b[<64;5;5M"), Read::Report(wheel(true), 10));
        assert_eq!(read("\x1b[<65;5;5M"), Read::Report(wheel(false), 10));
        // Shift, Alt and Ctrl make no difference, as in nano.
        assert_eq!(read("\x1b[<69;5;5M"), Read::Report(wheel(false), 10));
        assert_eq!(read("\x1b[<92;5;5M"), Read::Report(wheel(true), 10));
        assert_eq!(read("\x1b[<4;5;5m"), Read::Report(Report::Release(4, 4), 9));
    }

    #[test]
    fn what_the_editor_does_not_use_is_dropped_whole() {
        // Other buttons, a motion, a release of the wheel, a button that does not exist.
        for s in [
            "\x1b[<1;5;5M",
            "\x1b[<2;5;5m",
            "\x1b[<32;5;5M",
            "\x1b[<35;5;5M",
            "\x1b[<64;5;5m",
            "\x1b[<66;5;5M",
            "\x1b[<128;5;5M",
        ] {
            assert_eq!(read(s), Read::Report(Report::Other, s.len()), "{s:?}");
        }
    }

    #[test]
    fn a_report_that_is_not_exact_is_dropped_whole() {
        let drop = |len| Read::Drop { len, more: false };
        // Wrong number of numbers, empty numbers, zero places, too many digits.
        for s in [
            "\x1b[<0;5M",
            "\x1b[<0;5;5;5M",
            "\x1b[<0;;5M",
            "\x1b[<;5;5M",
            "\x1b[<0;0;5M",
            "\x1b[<0;5;0M",
            "\x1b[<0;123456;5M",
            "\x1b[<M",
            "\x1b[<0;5;5;M",
        ] {
            assert_eq!(read(s), drop(s.len()), "{s:?}");
        }
        // Another final byte ends it and goes with it; a byte that cannot be in it does not.
        assert_eq!(read("\x1b[<0;5;5A"), drop(9));
        assert_eq!(read("\x1b[<0;5~"), drop(7));
        assert_eq!(read("\x1b[<0;5\x1b[A"), drop(6));
        assert_eq!(read("\x1b[<0;5\rx"), drop(6));
        assert_eq!(read("\x1b[<0;5\u{e9}"), drop(6));
        // Characters that are not digits are no report either, but the sequence is whole.
        for s in [
            "\x1b[<-1;5;5M",
            "\x1b[<0;5;5 M",
            "\x1b[<?1;5;5M",
            "\x1b[<0;5;5:M",
        ] {
            assert_eq!(read(s), drop(s.len()), "{s:?}");
        }
    }

    #[test]
    fn an_unfinished_report_waits_for_more() {
        for s in ["\x1b[<", "\x1b[<0", "\x1b[<0;5;", "\x1b[<0;5;5"] {
            assert_eq!(read(s), Read::Incomplete, "{s:?}");
        }
    }

    #[test]
    fn a_long_report_is_dropped_however_long_it_is() {
        // Not a byte of it is left to be typed: what is read is dropped, the rest as it comes.
        let long = format!("\x1b[<{}M", "1;".repeat(100));
        assert_eq!(
            read(&long),
            Read::Drop {
                len: MAX_LEN,
                more: true
            }
        );
        let digits = format!("\x1b[<{}", "7".repeat(1000));
        assert_eq!(
            read(&digits),
            Read::Drop {
                len: MAX_LEN,
                more: true
            }
        );
        // The longest report that is valid fits.
        assert!(matches!(
            read("\x1b[<99999;99999;99999M"),
            Read::Report(Report::Other, 21)
        ));
    }
}
