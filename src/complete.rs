// SPDX-License-Identifier: MIT
//! Word completion (^]): the words that can complete the fragment before the cursor.

use ropey::Rope;
use std::collections::HashSet;

/// Where each run of ten decimal digits (Unicode "Nd") starts: Rust has no test for them alone.
const DIGIT_RUNS: [u32; 72] = [
    0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
    0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80,
    0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450,
    0x114D0, 0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x11F50,
    0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6,
    0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

/// Whether `c` is part of a word: a letter or decimal digit, or one of the extra `wordchars`.
/// Not `char::is_alphanumeric`: it also takes the other numbers (superscripts, fractions,
/// circled digits), which nano's `iswalnum` does not.
pub fn is_word_char(c: char, wordchars: &str) -> bool {
    c.is_alphabetic() || is_decimal_digit(c) || wordchars.contains(c)
}

fn is_decimal_digit(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    let c = u32::from(c);
    let after = DIGIT_RUNS.partition_point(|&zero| zero <= c);
    after > 0 && c - DIGIT_RUNS[after - 1] < 10
}

/// Where the word fragment before the cursor (at `cursor`) starts, as a character index.
pub fn fragment_start(text: &Rope, cursor: usize, wordchars: &str) -> usize {
    let mut start = cursor;
    while start > 0 && is_word_char(text.char(start - 1), wordchars) {
        start -= 1;
    }
    start
}

/// The words that can complete a fragment, found one at a time: the next ^] in a row scans on
/// from where the last one stopped, so the first never reads more of the text than it needs.
pub struct Completion {
    fragment: String,
    /// Where the fragment starts: the word being completed, which is not a candidate.
    skip: usize,
    /// Where the scan goes on, as a character index.
    scan: usize,
    /// What the endings already offered were: each is offered once.
    seen: HashSet<String>,
}

impl Completion {
    pub fn new(fragment: String, skip: usize) -> Self {
        Completion {
            fragment,
            skip,
            scan: 0,
            seen: HashSet::new(),
        }
    }

    /// What the next word of `text` that starts with the fragment and is longer than it adds to
    /// it, not offered before, in the order the words appear.
    pub fn next_ending(&mut self, text: &Rope, wordchars: &str) -> Option<String> {
        let mut word = String::new();
        let mut start = 0;
        let mut chars = text.chars_at(self.scan);
        loop {
            let c = chars.next();
            if let Some(c) = c.filter(|&c| is_word_char(c, wordchars)) {
                if word.is_empty() {
                    start = self.scan;
                }
                word.push(c);
                self.scan += 1;
                continue;
            }
            // nano leaves out a word at the end of a line that is one byte longer than the
            // fragment; the end of the text counts as the end of a line.
            let more_on_line = c.is_some_and(|c| c != '\n');
            let fits = word.len() > self.fragment.len() + usize::from(!more_on_line);
            let mut ending = None;
            if start != self.skip && fits && word.starts_with(&self.fragment) {
                let tail = &word[self.fragment.len()..];
                if !self.seen.contains(tail) {
                    ending = Some(tail.to_string());
                }
            }
            if c.is_some() {
                self.scan += 1;
            }
            if let Some(ending) = ending {
                self.seen.insert(ending.clone());
                return Some(ending);
            }
            c?;
            word.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endings(text: &str, fragment: &str, skip: usize) -> Vec<String> {
        let text = Rope::from_str(text);
        let mut completion = Completion::new(fragment.to_string(), skip);
        std::iter::from_fn(|| completion.next_ending(&text, "")).collect()
    }

    #[test]
    fn other_numbers_are_not_word_characters() {
        for c in ['½', '①', '²', '٣', 'Ⅻ', '日', '\u{93e}', '𝟗', 'é', '7'] {
            let expected = !matches!(c, '½' | '①' | '²');
            assert_eq!(is_word_char(c, ""), expected, "{c}");
        }
    }

    #[test]
    fn each_ending_comes_once_in_the_order_of_the_text() {
        assert_eq!(
            endings("foo foobar foobaz foobar foo", "foo", 24),
            ["bar", "baz"]
        );
    }

    #[test]
    fn the_first_of_many_candidates_is_found_at_once() {
        let words: Vec<String> = (0..50_000).map(|i| format!("word{i}")).collect();
        let text = Rope::from_str(&format!("{}\nwo", words.join(" ")));
        let fragment = String::from("wo");
        let skip = text.len_chars() - 2;
        let mut completion = Completion::new(fragment, skip);
        assert_eq!(completion.next_ending(&text, "").as_deref(), Some("rd0"));
        assert_eq!(completion.next_ending(&text, "").as_deref(), Some("rd1"));
        // Counted in work, not time: it read the few characters of the first two words, not
        // the 50,000 (scanning all of them every time took seconds).
        assert!(completion.scan < 20, "{}", completion.scan);
    }
}
