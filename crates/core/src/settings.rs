//! Settings kept between runs, as `key = value` lines in `%APPDATA%\kiku\settings.txt`.

use std::{fs, path::PathBuf};

use crate::{
    controls::{DEFAULT_KEEP_DAYS, THRESHOLD_MAX_DB, THRESHOLD_MIN_DB},
    catalog::{self, Choice},
    engine::Device,
    keybinds::Keybinds,
    paths,
    subtitles::SubtitleSettings,
    theme::Themes,
    transcripts,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub mic_threshold_db: f32,
    pub computer_threshold_db: f32,
    /// The model's id; it may name an imported model that is gone, which runs the default instead.
    pub model: String,
    pub device: Device,
    /// Every finished line is saved (auto), or only what the save buttons save (manual).
    pub auto_save: bool,
    /// In auto save mode, only your mic's lines.
    pub save_mic_only: bool,
    /// In auto save mode, saved lines are deleted after this many days; 0 keeps them forever.
    pub keep_days: u32,
    pub keybinds: Keybinds,
    /// The app's theme, and the Custom theme if one was made.
    pub themes: Themes,
    pub subtitles: SubtitleSettings,
    /// The panel folds an app to one row after it has been quiet this many minutes; 0 never does.
    pub auto_fold_minutes: u32,
    pub block_answer: BlockAnswer,
    /// Apps blocked always, by name: never captured or captioned.
    pub blocked: Vec<String>,
    /// Once a day, ask the update server whether a newer Kiku is out.
    pub check_updates: bool,
}

/// What an app header's block button does: ask, or block without asking until Kiku quits or always.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockAnswer {
    #[default]
    Ask,
    Session,
    Always,
}

impl BlockAnswer {
    fn id(self) -> &'static str {
        match self {
            BlockAnswer::Ask => "ask",
            BlockAnswer::Session => "session",
            BlockAnswer::Always => "always",
        }
    }
}

impl Settings {
    pub fn choice(&self) -> Choice {
        Choice { model: catalog::by_id_or_default(&self.model), device: self.device }
    }
}

impl Default for Settings {
    /// Gates start fully open; the speech detector inside each model still ignores silence.
    fn default() -> Self {
        Self {
            mic_threshold_db: THRESHOLD_MIN_DB,
            computer_threshold_db: THRESHOLD_MIN_DB,
            model: catalog::DEFAULT_ID.into(),
            device: Device::Cpu,
            auto_save: true,
            save_mic_only: false,
            keep_days: DEFAULT_KEEP_DAYS,
            keybinds: Keybinds::default(),
            themes: Themes::default(),
            subtitles: SubtitleSettings::default(),
            auto_fold_minutes: 2,
            block_answer: BlockAnswer::Ask,
            blocked: Vec::new(),
            check_updates: true,
        }
    }
}

impl Settings {
    /// The saved settings, or defaults for anything missing or unreadable.
    pub fn load() -> Self {
        fs::read_to_string(path()).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    /// Saves the settings in one step, so a crash while saving leaves the previous ones.
    pub fn save(&self) -> std::io::Result<()> {
        let path = path();
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        transcripts::replace(&path, &self.to_text())
    }

    fn parse(text: &str) -> Self {
        let lines: Vec<(&str, &str)> = text.lines().filter_map(|line| line.split_once('=')).map(|(key, value)| (key.trim(), value.trim())).collect();
        let (mut settings, mut subtitles) = (Self::default(), Vec::new());
        for &(key, value) in &lines {
            match key {
                "mic_threshold_db" => settings.mic_threshold_db = threshold_db(value).unwrap_or(settings.mic_threshold_db),
                "computer_threshold_db" => {
                    settings.computer_threshold_db = threshold_db(value).unwrap_or(settings.computer_threshold_db)
                }
                "model" => settings.model = value.to_owned(),
                "device" => settings.device = if value == "gpu" { Device::Cuda } else { Device::Cpu },
                "keep_days" => settings.keep_days = value.parse().unwrap_or(settings.keep_days),
                "save_mode" => settings.auto_save = value != "manual",
                "auto_save" => settings.save_mic_only = value == "mine",
                "auto_fold" => settings.auto_fold_minutes = value.parse().unwrap_or(settings.auto_fold_minutes),
                "block_answer" => {
                    let answers = [BlockAnswer::Ask, BlockAnswer::Session, BlockAnswer::Always];
                    settings.block_answer = answers.into_iter().find(|answer| answer.id() == value).unwrap_or_default();
                }
                "blocked" if !value.is_empty() => settings.blocked.push(value.to_owned()),
                "check_updates" => settings.check_updates = value != "off",
                // Read together below, with the captions colours of older settings.
                key if key == "theme" || key.starts_with("custom.") => {}
                key if key.starts_with("subtitles.") => subtitles.push((key, value)),
                key => settings.keybinds.apply_setting(key, value),
            }
        }
        settings.subtitles.apply_settings(&subtitles);
        settings.themes = Themes::from_settings(&lines);
        settings
    }

    fn to_text(&self) -> String {
        let blocked: String = self.blocked.iter().map(|name| format!("blocked = {name}\n")).collect();
        format!(
            "mic_threshold_db = {}\ncomputer_threshold_db = {}\nmodel = {}\ndevice = {}\nsave_mode = {}\nauto_save = {}\nkeep_days = {}\nauto_fold = {}\nblock_answer = {}\ncheck_updates = {}\n{}{blocked}{}{}",
            self.mic_threshold_db,
            self.computer_threshold_db,
            self.model,
            if self.device == Device::Cuda { "gpu" } else { "cpu" },
            if self.auto_save { "auto" } else { "manual" },
            if self.save_mic_only { "mine" } else { "everything" },
            self.keep_days,
            self.auto_fold_minutes,
            self.block_answer.id(),
            if self.check_updates { "on" } else { "off" },
            self.themes.to_settings(),
            self.keybinds.to_settings(),
            self.subtitles.to_settings()
        )
    }
}

/// Whether settings were ever saved: if not, this is the app's first run.
pub fn exist() -> bool {
    path().exists()
}

/// A sensitivity read from the file, kept to the slider's range: a hand-edited NaN would keep the gate shut.
fn threshold_db(value: &str) -> Option<f32> {
    value.parse::<f32>().ok().filter(|db| db.is_finite()).map(|db| db.clamp(THRESHOLD_MIN_DB, THRESHOLD_MAX_DB))
}

fn path() -> PathBuf {
    paths::data_dir().join("settings.txt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn settings_round_trip_and_tolerate_junk() {
        let mut keybinds = Keybinds::default();
        keybinds.set_hold(vec![crate::keybinds::Key::Ctrl, crate::keybinds::Key::Alt]).unwrap();
        let settings = Settings {
            mic_threshold_db: -42.5,
            computer_threshold_db: -60.0,
            model: "moonshine-small".into(),
            device: Device::Cuda,
            auto_save: false,
            save_mic_only: true,
            keep_days: 7,
            keybinds,
            themes: {
                let mut themes = Themes::from(Theme::Blossom);
                themes.edit(|colours| colours.background = 0x101010);
                themes
            },
            subtitles: SubtitleSettings { on: true, rows: 3, ..Default::default() },
            auto_fold_minutes: 10,
            block_answer: BlockAnswer::Always,
            blocked: vec!["Discord".into(), "Steam, Inc. = client".into()],
            check_updates: false,
        };
        assert_eq!(Settings::parse(&settings.to_text()), settings);
        assert_eq!(Settings::parse("mic_threshold_db = loud\nunknown = 1"), Settings::default());
        assert_eq!(Settings::parse("mic_threshold_db = NaN\ncomputer_threshold_db = 50").computer_threshold_db, THRESHOLD_MAX_DB);
        assert_eq!(Settings::parse("mic_threshold_db = NaN").mic_threshold_db, Settings::default().mic_threshold_db);
    }
}
