//! Modes change what dictation does: `normal` (dictation and macros), `commands` (only macros), `off` (only
//! the phrases that change modes), and the user's own from `macros.toml`'s `[[mode]]` tables.

use super::{
    file::Macro,
    kiku::{KikuAction, KikuStep},
    plan::Step,
};
use crate::actions;

/// What dictation does now.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Normal,
    Commands,
    Off,
    /// One of the user's `[[mode]]`s, by name: dictation and the macros scoped to it.
    Own(String),
}

impl Mode {
    /// The mode called `name`: a built-in one, else the user's.
    pub fn named(name: &str) -> Mode {
        match name {
            "normal" => Mode::Normal,
            "commands" => Mode::Commands,
            "off" => Mode::Off,
            own => Mode::Own(own.to_owned()),
        }
    }

    /// Its name as `macros.toml` writes it.
    pub fn name(&self) -> &str {
        match self {
            Mode::Normal => "normal",
            Mode::Commands => "commands",
            Mode::Off => "off",
            Mode::Own(name) => name,
        }
    }
}

/// Whether something scoped to `modes` (names; empty is every mode but `off`) works in `mode`. The one check
/// plans, the indexes and the palette share.
pub(super) fn works_in(modes: &[String], mode: &Mode) -> bool {
    *mode != Mode::Off && (modes.is_empty() || modes.iter().any(|name| name == mode.name()))
}

impl Macro {
    pub(super) fn runs_in(&self, mode: &Mode) -> bool {
        works_in(&self.modes, mode)
    }
}

/// One of the user's `[[mode]]` tables.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OwnMode {
    pub name: String,
    /// Apps (as captions name them) that switch to it while in front.
    pub apps: Vec<String>,
}

/// The user's modes, with the apps that switch to each while in front.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Modes {
    own: Vec<OwnMode>,
}

impl Modes {
    /// The modes `tables` define, with those that can't be used (a built-in's name, one taken, no name) left
    /// out and reported by their place in `tables` and why.
    pub(super) fn from_tables(tables: Vec<Result<OwnMode, String>>) -> (Modes, Vec<(usize, String)>) {
        let (mut modes, mut refused) = (Modes::default(), Vec::new());
        for (at, table) in tables.into_iter().enumerate() {
            let added = table.and_then(|own| match Mode::named(&own.name) {
                _ if own.name.trim().is_empty() => Err("a mode needs a name".to_owned()),
                Mode::Own(_) if modes.own.iter().all(|known| known.name != own.name) => Ok(own),
                Mode::Own(_) => Err(format!("another mode is called {}", own.name)),
                _ => Err(format!("{} is a built-in mode", own.name)),
            });
            match added {
                Ok(own) => modes.own.push(own),
                Err(why) => refused.push((at, why)),
            }
        }
        (modes, refused)
    }

    /// The mode called `name`, built-in or the user's.
    pub(super) fn lookup(&self, name: &str) -> Result<Mode, String> {
        match Mode::named(name) {
            Mode::Own(_) if self.own.iter().all(|known| known.name != name) => Err(format!("there is no mode called {name}")),
            mode => Ok(mode),
        }
    }

    /// Every mode there is.
    pub(super) fn all(&self) -> impl Iterator<Item = Mode> + '_ {
        [Mode::Normal, Mode::Commands, Mode::Off].into_iter().chain(self.own.iter().map(|own| Mode::Own(own.name.clone())))
    }

    /// There are no modes of the user's own.
    pub fn is_empty(&self) -> bool {
        self.own.is_empty()
    }

    /// The user's mode whose `apps` name `app` (as captions name it, any case).
    pub fn for_app(&self, app: &str) -> Option<Mode> {
        let own = self.own.iter().find(|own| own.apps.iter().any(|name| name.eq_ignore_ascii_case(app)))?;
        Some(Mode::Own(own.name.clone()))
    }

    /// The mode dictation is in with `app` in front when `chosen` was chosen: an app's own mode while `chosen`
    /// is `normal`, else `chosen`.
    pub fn effective(&self, app: &str, chosen: &Mode) -> Mode {
        match chosen {
            Mode::Normal => self.for_app(app).unwrap_or(Mode::Normal),
            other => other.clone(),
        }
    }
}

/// Which built-in phrases are heard ("command mode", "normal mode", "dictation off", "dictation on", "what can
/// I say"); each can be turned off in Settings, and is then just words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinPhrases {
    pub command_mode: bool,
    pub normal_mode: bool,
    pub dictation_off: bool,
    pub dictation_on: bool,
    pub what_can_i_say: bool,
}

/// The words in the longest built-in phrase.
const MOST_WORDS: usize = 4;

impl Default for BuiltinPhrases {
    fn default() -> Self {
        Self { command_mode: true, normal_mode: true, dictation_off: true, dictation_on: true, what_can_i_say: true }
    }
}

impl BuiltinPhrases {
    /// What `line` does when it is one of the phrases that is on, as a whole line, case and punctuation aside.
    pub(super) fn heard(&self, line: &str) -> Option<Step> {
        // Most lines are longer than any phrase: skip tokenising them.
        if line.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).nth(MOST_WORDS).is_some() {
            return None;
        }
        let said = actions::tokens(line).join(" ");
        let (on, step) = match said.as_str() {
            "command mode" => (self.command_mode, Step::SetMode(Mode::Commands)),
            "normal mode" => (self.normal_mode, Step::SetMode(Mode::Normal)),
            "dictation off" => (self.dictation_off, Step::SetMode(Mode::Off)),
            "dictation on" => (self.dictation_on, Step::SetMode(Mode::Normal)),
            "what can i say" => (self.what_can_i_say, Step::Kiku(KikuStep::new(KikuAction::ShowPalette))),
            _ => return None,
        };
        on.then_some(step)
    }

    /// The ids of the phrases turned off, as `macros.phrases` keeps them.
    pub fn turned_off(&self) -> String {
        let phrases = [("command", self.command_mode), ("normal", self.normal_mode), ("off", self.dictation_off), ("on", self.dictation_on), ("say", self.what_can_i_say)];
        phrases.into_iter().filter(|(_, on)| !on).map(|(id, _)| id).collect::<Vec<_>>().join(" ")
    }

    /// All on but the phrases whose ids `turned_off` lists.
    pub fn from_turned_off(turned_off: &str) -> Self {
        let mut phrases = Self::default();
        for id in turned_off.split_whitespace() {
            match id {
                "command" => phrases.command_mode = false,
                "normal" => phrases.normal_mode = false,
                "off" => phrases.dictation_off = false,
                "on" => phrases.dictation_on = false,
                "say" => phrases.what_can_i_say = false,
                _ => {}
            }
        }
        phrases
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_turned_off_round_trip() {
        let phrases = BuiltinPhrases { command_mode: false, dictation_on: false, ..BuiltinPhrases::default() };
        assert_eq!(phrases.turned_off(), "command on");
        assert_eq!(BuiltinPhrases::from_turned_off(&phrases.turned_off()), phrases);
        assert_eq!(BuiltinPhrases::from_turned_off(""), BuiltinPhrases::default());
    }

    #[test]
    fn a_built_in_or_repeated_name_is_refused() {
        let table = |name: &str| Ok(OwnMode { name: name.into(), apps: vec![] });
        let (modes, refused) = Modes::from_tables(vec![table("coding"), table("off"), table("coding"), table("")]);
        assert_eq!(modes.all().count(), 4);
        assert_eq!(refused.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn a_phrase_with_detached_punctuation_is_heard() {
        let phrases = BuiltinPhrases::default();
        assert_eq!(phrases.heard("What can I say ?"), Some(Step::Kiku(KikuStep::new(KikuAction::ShowPalette))));
        assert_eq!(phrases.heard("What can I say?"), Some(Step::Kiku(KikuStep::new(KikuAction::ShowPalette))));
        assert_eq!(phrases.heard("what can I say about the weather today in this very long line"), None);
    }
}
