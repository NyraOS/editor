// SPDX-License-Identifier: MIT
//! The colours of the interface, as the nanorc files of GNU nano 8.4 set them (`set titlecolor`
//! and the others). They use the syntax of the colours of a syntax (see `syntax::parse_hue`), so
//! the only thing that ever reaches the terminal from them is a number from the nanorc.
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
    /// The guide stripe (`guidestripe`): inverse without a colour.
    pub stripe: Option<Hue>,
    /// The scroll bar (`indicator`): its track is plain without a colour, its thumb inverse.
    pub scroller: Option<Hue>,
    /// Whether the nanorc files set a colour of the interface. Then `NO_COLOR` changes nothing.
    pub user_set: bool,
}

/// The names of the settings of the interface colours that work.
pub const NAMES: [&str; 11] = [
    "titlecolor",
    "statuscolor",
    "errorcolor",
    "selectedcolor",
    "numbercolor",
    "keycolor",
    "functioncolor",
    "promptcolor",
    "spotlightcolor",
    "stripecolor",
    "scrollercolor",
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
            stripe: None,
            scroller: None,
            user_set: false,
        }
    }
}

impl Colours {
    /// Gives the setting called `name` (one of `NAMES`) its colour.
    pub fn set(&mut self, name: &str, hue: Hue) {
        let slot = match name {
            "titlecolor" => &mut self.title,
            "statuscolor" => &mut self.status,
            "errorcolor" => &mut self.error,
            "selectedcolor" => &mut self.selected,
            "numbercolor" => &mut self.number,
            "keycolor" => &mut self.key,
            "functioncolor" => &mut self.function,
            "promptcolor" => &mut self.prompt,
            "spotlightcolor" => &mut self.spotlight,
            "stripecolor" => &mut self.stripe,
            "scrollercolor" => &mut self.scroller,
            _ => return,
        };
        *slot = Some(hue);
        self.user_set = true;
    }

    /// `NO_COLOR` is set. As nano 8.4 does, it takes the colours away from the two parts that
    /// have one by default (error messages and the search match), which become inverse, and only
    /// when the nanorc files set no colour of the interface at all. Nothing else changes: not the
    /// colours of a syntax, and not a colour the user chose.
    pub fn no_colour(&mut self) {
        if !self.user_set {
            self.error = None;
            self.spotlight = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hue() -> Hue {
        Hue {
            fg: Some(1),
            ..Hue::default()
        }
    }

    #[test]
    fn no_colour_takes_the_two_default_colours_away() {
        let mut c = Colours::default();
        c.no_colour();
        assert_eq!((c.error, c.spotlight), (None, None));
    }

    #[test]
    fn no_colour_changes_nothing_when_a_colour_is_set() {
        let mut c = Colours::default();
        c.set("stripecolor", hue());
        assert!(c.user_set && c.stripe == Some(hue()));
        c.no_colour();
        assert_eq!(c.error, Colours::default().error);
        assert_eq!(c.spotlight, Colours::default().spotlight);
    }

    #[test]
    fn every_name_has_a_place() {
        for name in NAMES {
            let mut c = Colours::default();
            c.set(name, hue());
            assert!(c.user_set, "{name}");
            assert_ne!(
                c,
                Colours {
                    user_set: true,
                    ..Colours::default()
                },
                "{name}"
            );
        }
    }
}
