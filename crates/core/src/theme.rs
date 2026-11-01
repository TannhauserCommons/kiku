//! The app's look, chosen in settings: each theme styles the panel, the settings window and captions mode.
//! Six presets, and one Custom theme made by editing a theme's colours.

/// A look for the whole app, saved as a `theme = night` line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Night,
    Day,
    Lagoon,
    Meadow,
    Blossom,
    Graphite,
    /// The user's own colours, built on a preset (see [`Themes`]).
    Custom,
}

impl Theme {
    /// The presets.
    pub const ALL: [Theme; 6] = [Theme::Night, Theme::Day, Theme::Lagoon, Theme::Meadow, Theme::Blossom, Theme::Graphite];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Night => "Night",
            Theme::Day => "Day",
            Theme::Lagoon => "Lagoon",
            Theme::Meadow => "Meadow",
            Theme::Blossom => "Blossom",
            Theme::Graphite => "Graphite",
            Theme::Custom => "Custom",
        }
    }

    /// The preset named `name`, in any case.
    pub fn preset(name: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|theme| theme.name().eq_ignore_ascii_case(name))
    }

    /// A preset's colours. Custom has none of its own ([`Themes::colours`] has them), so it gives Night's.
    pub const fn colours(self) -> Colours {
        match self {
            Theme::Night | Theme::Custom => Colours { background: 0x1d2633, accent: 0x6ccfe0, pill: 0xc2e2f6, text: 0x0d2a3d },
            Theme::Day => Colours { background: 0xf4f9fd, accent: 0x0a5296, pill: 0xe4f0f9, text: 0x123049 },
            Theme::Lagoon => Colours { background: 0x123a40, accent: 0x72e6c3, pill: 0xbfeee3, text: 0x0b3a33 },
            Theme::Meadow => Colours { background: 0xf3fbf5, accent: 0x145f2a, pill: 0xc9ecc8, text: 0x10331a },
            Theme::Blossom => Colours { background: 0xfef4f8, accent: 0x9e1450, pill: 0xf6c3d7, text: 0x3b1326 },
            Theme::Graphite => Colours { background: 0x26282c, accent: 0xa9c4e3, pill: 0x2b2e34, text: 0xeef0f3 },
        }
    }
}

/// A theme's colours that can be edited, as `0xRRGGBB`: the window's, the accent, and captions mode's pills and text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colours {
    pub background: u32,
    pub accent: u32,
    pub pill: u32,
    pub text: u32,
}

impl Colours {
    /// A light background, with dark text.
    pub fn is_light(&self) -> bool {
        let channel = |shift: u32| ((self.background >> shift) & 0xff) as f32 / 255.0;
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0) > 0.5
    }
}

/// The one Custom theme: a preset's colours, edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustomTheme {
    /// The preset it was made from, whose surfaces it keeps where its colours allow; never Custom.
    pub base: Theme,
    pub colours: Colours,
}

/// The theme shown, and the Custom theme if one was made. Saved as a `theme = …` line, then `custom.…` lines.
///
/// Editing a preset's colours makes them the Custom theme, replacing any before, and shows it:
/// ```
/// use kiku_core::theme::{Theme, Themes};
/// let mut themes = Themes::from(Theme::Day);
/// themes.edit(|colours| colours.accent = 0xd9415e);
/// assert_eq!((themes.chosen(), themes.base(), themes.colours().background), (Theme::Custom, Theme::Day, Theme::Day.colours().background));
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Themes {
    chosen: Theme,
    /// Always there while Custom is chosen.
    custom: Option<CustomTheme>,
}

impl From<Theme> for Themes {
    fn from(theme: Theme) -> Self {
        let mut themes = Themes::default();
        themes.choose(theme);
        themes
    }
}

impl Themes {
    pub fn chosen(&self) -> Theme {
        self.chosen
    }

    /// The Custom theme, chosen or not.
    pub fn custom(&self) -> Option<&CustomTheme> {
        self.custom.as_ref()
    }

    /// The preset the chosen theme is built on: itself, or Custom's base.
    pub fn base(&self) -> Theme {
        match (self.chosen, &self.custom) {
            (Theme::Custom, Some(custom)) => custom.base,
            (chosen, _) => chosen,
        }
    }

    /// The chosen theme's colours.
    pub fn colours(&self) -> Colours {
        match (self.chosen, &self.custom) {
            (Theme::Custom, Some(custom)) => custom.colours,
            (chosen, _) => chosen.colours(),
        }
    }

    pub fn is_light(&self) -> bool {
        self.colours().is_light()
    }

    /// Shows `theme`. Custom is first made from the theme shown if there is none yet.
    pub fn choose(&mut self, theme: Theme) {
        if theme == Theme::Custom && self.custom.is_none() {
            self.custom = Some(CustomTheme { base: self.base(), colours: self.colours() });
        }
        self.chosen = theme;
    }

    /// Changes the colours shown. A preset stays as it is: its colours, changed, become the Custom
    /// theme (replacing the one before), which is then shown. A change that changes nothing does nothing.
    pub fn edit(&mut self, change: impl FnOnce(&mut Colours)) {
        let mut colours = self.colours();
        change(&mut colours);
        if colours == self.colours() {
            return;
        }
        match &mut self.custom {
            Some(custom) if self.chosen == Theme::Custom => custom.colours = colours,
            _ => {
                self.custom = Some(CustomTheme { base: self.base(), colours });
                self.chosen = Theme::Custom;
            }
        }
    }

    /// Reads the `theme` and `custom.<name>` lines of settings; anything invalid keeps the default.
    ///
    /// Captions colours picked by hand before themes could be edited (`subtitles.pill` and `subtitles.text`)
    /// become the Custom theme, built on the theme saved. Older still, a `subtitles.theme` naming a preset
    /// means they were never shown.
    pub fn from_settings(lines: &[(&str, &str)]) -> Self {
        let value = |name: &str| lines.iter().rev().find(|(key, _)| *key == name).map(|&(_, value)| value);
        let colour = |name: &str| value(name).and_then(parse_colour);
        let saved = value("theme").unwrap_or_default();
        let base = value("custom.base").and_then(Theme::preset);
        let mut themes = Themes { chosen: Theme::preset(saved).unwrap_or_default(), custom: None };
        if let Some(base) = base {
            let mut colours = base.colours();
            let fields = [
                (&mut colours.background, "custom.background"),
                (&mut colours.accent, "custom.accent"),
                (&mut colours.pill, "custom.pill"),
                (&mut colours.text, "custom.text"),
            ];
            for (field, name) in fields {
                *field = colour(name).unwrap_or(*field);
            }
            themes.custom = Some(CustomTheme { base, colours });
            if saved.eq_ignore_ascii_case("custom") {
                themes.chosen = Theme::Custom;
            }
        } else if value("subtitles.theme").is_none_or(|theme| theme.eq_ignore_ascii_case("custom")) {
            let (pill, text) = (colour("subtitles.pill"), colour("subtitles.text"));
            if pill.is_some() || text.is_some() {
                themes.edit(|colours| {
                    colours.pill = pill.unwrap_or(colours.pill);
                    colours.text = text.unwrap_or(colours.text);
                });
            }
        }
        themes
    }

    pub fn to_settings(&self) -> String {
        let mut lines = format!("theme = {}\n", self.chosen.name().to_lowercase());
        if let Some(CustomTheme { base, colours }) = self.custom {
            lines.push_str(&format!(
                "custom.base = {}\ncustom.background = #{:06x}\ncustom.accent = #{:06x}\ncustom.pill = #{:06x}\ncustom.text = #{:06x}\n",
                base.name().to_lowercase(),
                colours.background,
                colours.accent,
                colours.pill,
                colours.text,
            ));
        }
        lines
    }
}

/// `#rrggbb` as `0xRRGGBB`.
fn parse_colour(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Themes {
        let lines: Vec<_> = text.lines().filter_map(|l| l.split_once('=')).map(|(key, value)| (key.trim(), value.trim())).collect();
        Themes::from_settings(&lines)
    }

    #[test]
    fn editing_a_preset_makes_the_one_custom_theme_and_leaves_the_preset() {
        let mut themes = Themes::from(Theme::Lagoon);
        themes.edit(|colours| colours.text = 0x112233);
        let custom = CustomTheme { base: Theme::Lagoon, colours: Colours { text: 0x112233, ..Theme::Lagoon.colours() } };
        assert_eq!((themes.chosen(), themes.custom()), (Theme::Custom, Some(&custom)));
        themes.edit(|colours| colours.pill = 0x445566);
        assert_eq!(themes.colours(), Colours { pill: 0x445566, ..custom.colours }, "editing Custom changes it in place");
        themes.choose(Theme::Day);
        assert_eq!(themes.colours(), Theme::Day.colours());
        let before = themes;
        themes.edit(|colours| colours.text = Theme::Day.colours().text);
        assert_eq!(themes, before, "an edit that changes nothing keeps Custom and the preset");
        themes.edit(|colours| colours.background = 0x000000);
        assert_eq!(themes.custom().map(|custom| (custom.base, custom.colours.text)), Some((Theme::Day, Theme::Day.colours().text)), "replaced");
        assert!(!themes.is_light());
    }

    #[test]
    fn settings_round_trip_migrate_and_tolerate_junk() {
        for themes in [Themes::default(), Themes::from(Theme::Custom), {
            let mut themes = Themes::from(Theme::Meadow);
            themes.edit(|colours| colours.accent = 0xd9415e);
            themes.choose(Theme::Graphite);
            themes
        }] {
            assert_eq!(read(&themes.to_settings()), themes);
        }
        assert_eq!(read("theme = custom\ncustom.base = custom\ncustom.accent = blue"), Themes::default());
        assert_eq!(read("theme = plaid"), Themes::default());

        let picked = read("theme = day\nsubtitles.pill = #000000\nsubtitles.text = #ffffff");
        let colours = Colours { pill: 0, text: 0xffffff, ..Theme::Day.colours() };
        assert_eq!((picked.chosen(), picked.base(), picked.colours()), (Theme::Custom, Theme::Day, colours), "hand-picked captions colours");
        assert_eq!(read("subtitles.theme = Blue\nsubtitles.pill = #000000"), Themes::default(), "an old preset never showed them");
    }
}
