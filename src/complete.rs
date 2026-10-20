// SPDX-License-Identifier: MIT
//! Word completion (^]): the words that can complete the fragment before the cursor.

use ropey::Rope;

/// The completions being offered: the next ^] in a row takes the next one.
pub struct Completion {
    /// What each one adds to the fragment, in the order they are offered.
    pub endings: Vec<String>,
    pub next: usize,
}

/// Whether `c` is part of a word: a letter or digit, or one of the extra `wordchars`.
pub fn is_word_char(c: char, wordchars: &str) -> bool {
    c.is_alphanumeric() || wordchars.contains(c)
}

/// Where the word fragment before the cursor (at `cursor`) starts, as a character index.
pub fn fragment_start(text: &Rope, cursor: usize, wordchars: &str) -> usize {
    let mut start = cursor;
    while start > 0 && is_word_char(text.char(start - 1), wordchars) {
        start -= 1;
    }
    start
}

/// What the words of `text` that start with `fragment` and are longer than it add to it, each
/// once, in the order they appear. The word that starts at `skip` is the one being completed.
/// Called for each open buffer in turn, with the same `endings`.
pub fn collect_endings(
    text: &Rope,
    fragment: &str,
    skip: Option<usize>,
    wordchars: &str,
    endings: &mut Vec<String>,
) {
    let mut word = String::new();
    let mut start = 0;
    let mut end_word = |word: &mut String, start: usize, more_on_line: bool| {
        // nano leaves out a word at the end of a line that is one byte longer than the fragment.
        let fits = word.len() > fragment.len() + usize::from(!more_on_line);
        if Some(start) != skip && fits && word.starts_with(fragment) {
            let ending = &word[fragment.len()..];
            if !endings.iter().any(|e| e == ending) {
                endings.push(ending.to_string());
            }
        }
        word.clear();
    };
    for (i, c) in text.chars().enumerate() {
        if !is_word_char(c, wordchars) {
            end_word(&mut word, start, c != '\n');
            continue;
        }
        if word.is_empty() {
            start = i;
        }
        word.push(c);
    }
    end_word(&mut word, start, false);
}
