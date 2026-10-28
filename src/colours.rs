// SPDX-License-Identifier: MIT
//! The colours of the interface, as the nanorc files of GNU nano 8.4 set them (`set titlecolor`
//! and the others). They use the syntax of the colours of a syntax (see `syntax::parse_hue`), so
//! the only thing that ever reaches the terminal from them is a number from the nanorc.
//!
//! `stripecolor` is not here: it colours the guide stripe (`--guidestripe`), which the editor does
//! not have yet, so it is refused with that option.
//!
//! An element with no colour of its own looks as it does in nano by default: inverse video, or
//! plain. The default of the error messages and of the search match is a colour.

use crate::syntax::Hue;

/// The colour of each part of the interface; `None`: the default look of that part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colours {
    pub title: Option<Hue>,
    pub status: Option<Hue>,
    pub error: Option<Hue>,
    pub selected: Option<Hue>,
    pub number: Option<Hue>,
    pub key: Option<Hue>,
    pub function: Option<Hue>,
    /// The prompt bar; without a colour it has the title bar's.
    pub prompt: Option<Hue>,
    pub spotlight: Option<Hue>,
}

/// The names of the settings of the interface colours that work.
pub const NAMES: [&str; 9] = [
    "titlecolor",
    "statuscolor",
    "errorcolor",
    "selectedcolor",
    "numbercolor",
    "keycolor",
    "functioncolor",
    "promptcolor",
    "spotlightcolor",
];

impl Default for Colours {
    fn default() -> Self {
        Self {
            title: None,
            status: None,
            // Bold white on red.
            error: Some(Hue {
                fg: Some(7),
                bg: Some(1),
                bold: true,
                italic: false,
            }),
            selected: None,
            number: None,
            key: None,
            function: None,
            prompt: None,
            // Black on light yellow.
            spotlight: Some(Hue {
                fg: Some(0),
                bg: Some(11),
                bold: false,
                italic: false,
            }),
        }
    }
}

impl Colours {
    /// The setting called `name` (one of `NAMES`).
    pub fn setting(&mut self, name: &str) -> Option<&mut Option<Hue>> {
        Some(match name {
            "titlecolor" => &mut self.title,
            "statuscolor" => &mut self.status,
            "errorcolor" => &mut self.error,
            "selectedcolor" => &mut self.selected,
            "numbercolor" => &mut self.number,
            "keycolor" => &mut self.key,
            "functioncolor" => &mut self.function,
            "promptcolor" => &mut self.prompt,
            "spotlightcolor" => &mut self.spotlight,
            _ => return None,
        })
    }
}
