//! Captions mode: the newest lines from every app and your mic shown like movie subtitles, and how they look.
//!
//! Saved in settings as `subtitles.<name> = value` lines. Its colours are the app theme's (see [`crate::theme`]).

use crate::{
    captions::{Caption, Captions, Section},
    theme::Themes,
};

/// Rows of text shown at most; a long line wraps onto several.
pub const MAX_ROWS: u8 = 4;
/// Fonts offered first, all shipped with Windows 10.
pub const PRESET_FONTS: [&str; 6] = ["Segoe UI", "Bahnschrift", "Calibri", "Georgia", "Verdana", "Consolas"];

/// How each line looks. Colours are `0xRRGGBB`.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub pill: u32,
    pub text: u32,
    pub font: String,
    /// Text size in DIPs.
    pub size: u16,
}

/// Captions mode's settings.
#[derive(Clone, Debug, PartialEq)]
pub struct SubtitleSettings {
    /// Captions mode shows instead of the transcript panel.
    pub on: bool,
    pub rows: u8,
    /// Fade out after a few quiet seconds; off keeps the last lines up.
    pub auto_hide: bool,
    /// The pills' opacity in percent, whatever the theme; 0 leaves only the text. Text is always opaque.
    pub opacity: u8,
    pub font: String,
    /// Text size in DIPs.
    pub size: u16,
    /// Where the bottom-centre of the subtitles was dragged to, in screen pixels; `None` for the default.
    pub anchor: Option<(i32, i32)>,
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self { on: false, rows: 2, auto_hide: true, opacity: 92, font: PRESET_FONTS[0].to_owned(), size: 22, anchor: None }
    }
}

impl SubtitleSettings {
    /// How lines look in the app's theme.
    pub fn look(&self, themes: &Themes) -> Look {
        let colours = themes.colours();
        Look { pill: colours.pill, text: colours.text, font: self.font.clone(), size: self.size }
    }

    /// Reads the `subtitles.<name> = value` lines of settings; anything invalid keeps what was there.
    pub fn apply_settings(&mut self, lines: &[(&str, &str)]) {
        for &(key, value) in lines {
            let key = key.strip_prefix("subtitles.").unwrap_or(key);
            match key {
                "on" => self.on = value == "yes",
                "rows" => self.rows = value.parse().map_or(self.rows, |rows: u8| rows.clamp(1, MAX_ROWS)),
                "auto_hide" => self.auto_hide = value != "no",
                "opacity" => self.opacity = value.parse().map_or(self.opacity, |percent: u8| percent.min(100)),
                "font" if !value.is_empty() => self.font = value.to_owned(),
                "size" => self.size = value.parse().map_or(self.size, |size: u16| size.clamp(12, 72)),
                "anchor" => {
                    self.anchor = value.split_once(',').and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)));
                }
                _ => {}
            }
        }
    }

    pub fn to_settings(&self) -> String {
        let yes_no = |on| if on { "yes" } else { "no" };
        let mut lines = format!(
            "subtitles.on = {}\nsubtitles.rows = {}\nsubtitles.auto_hide = {}\nsubtitles.opacity = {}\nsubtitles.font = {}\nsubtitles.size = {}\n",
            yes_no(self.on),
            self.rows,
            yes_no(self.auto_hide),
            self.opacity,
            self.font,
            self.size,
        );
        if let Some((x, y)) = self.anchor {
            lines.push_str(&format!("subtitles.anchor = {x},{y}\n"));
        }
        lines
    }
}

/// Each section's revision at some moment: lines added or revised since are newer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mark(Vec<u64>);

impl Mark {
    pub fn now(captions: &Captions) -> Self {
        Self(captions.sections.iter().map(Section::revision).collect())
    }
}

/// The newest `count` lines added or revised after `mark`, from every app and your mic, oldest first,
/// with their section index. Your lines shown inside apps are left out: the You section has them all.
pub fn newest<'a>(captions: &'a Captions, mark: &Mark, count: usize) -> Vec<(usize, &'a Caption)> {
    let mut lines = Vec::new();
    for (index, section) in captions.sections.iter().enumerate() {
        let start = mark.0.get(index).map_or(0, |&revision| section.changed_since(revision));
        let own = section.captions.range(start..).rev().filter(|caption| caption.speaker.is_none()).take(count);
        lines.extend(own.map(|caption| (index, caption)));
    }
    lines.sort_by_key(|(_, caption)| caption.time);
    lines.split_off(lines.len().saturating_sub(count))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::{captions::YOU_SECTION, engine::Line};

    fn line(id: u64, text: &str) -> Line {
        Line { id, text: text.into(), complete: true, voice: None }
    }

    fn texts(lines: &[(usize, &Caption)]) -> Vec<String> {
        lines.iter().map(|(section, caption)| format!("{section}:{}", caption.text)).collect()
    }

    #[test]
    fn lines_from_apps_and_you_interleave_by_time_without_your_copies() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let discord = captions.add_app("Discord", None);
        captions.update_app(discord, 1, &[line(1, "Joining?")], now);
        std::thread::sleep(std::time::Duration::from_millis(2));
        captions.update_mic(2, &[line(1, "On my way.")], Some("Discord"));
        std::thread::sleep(std::time::Duration::from_millis(2));
        captions.update_app(discord, 1, &[line(2, "Cool.")], now);
        let all = newest(&captions, &Mark::default(), 5);
        assert_eq!(texts(&all), ["1:Joining?", format!("{YOU_SECTION}:On my way.").as_str(), "1:Cool."]);
        assert_eq!(texts(&newest(&captions, &Mark::default(), 2)), [format!("{YOU_SECTION}:On my way.").as_str(), "1:Cool."]);
    }

    #[test]
    fn only_lines_new_or_revised_after_the_mark_show() {
        let (mut captions, now) = (Captions::default(), Instant::now());
        let app = captions.add_app("App", None);
        captions.update_app(app, 1, &[line(1, "Old."), Line { id: 2, text: "in prog".into(), complete: false, voice: None }], now);
        let mark = Mark::now(&captions);
        assert!(newest(&captions, &mark, 4).is_empty());
        captions.update_app(app, 1, &[line(2, "in progress.")], now);
        let later = captions.add_app("Later", None);
        captions.update_app(later, 3, &[line(1, "Hi.")], now);
        assert_eq!(texts(&newest(&captions, &mark, 4)), ["1:in progress.", "2:Hi."]);
    }

    fn read(text: &str) -> SubtitleSettings {
        let lines: Vec<_> = text.lines().filter_map(|l| l.split_once('=')).map(|(key, value)| (key.trim(), value.trim())).collect();
        let mut settings = SubtitleSettings::default();
        settings.apply_settings(&lines);
        settings
    }

    #[test]
    fn settings_round_trip_and_tolerate_junk() {
        let font = "Comic Sans MS".to_owned();
        let settings = SubtitleSettings { on: true, rows: 3, auto_hide: false, opacity: 0, font, anchor: Some((960, -40)), ..Default::default() };
        assert_eq!(read(&settings.to_settings()), settings);
        let junk = read("rows = 9\nopacity = 250\npill = blue\ntheme = plaid\nanchor = 1");
        assert_eq!(junk, SubtitleSettings { rows: MAX_ROWS, opacity: 100, ..Default::default() });
    }
}
