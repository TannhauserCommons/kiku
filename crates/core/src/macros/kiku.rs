//! The `kiku` step: Kiku's own actions (dictation, captions, the panel, settings, history) as macro steps,
//! from one list that the file reader, the editor, the command keys, the MCP tools and the README share.

use super::template::{shown, Template};
use crate::actions::Private;

/// Something Kiku itself does. No quit: a macro never ends Kiku.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KikuAction {
    StartDictation,
    StopDictation,
    ToggleDictation,
    StartCaptions,
    StopCaptions,
    ToggleCaptions,
    ShowPanel,
    HidePanel,
    TogglePanel,
    LockPanel,
    UnlockPanel,
    ToggleLock,
    CaptionsModeOn,
    CaptionsModeOff,
    ToggleCaptionsMode,
    PasteWhatISaid,
    Undo,
    SaveMiss,
    ShowPalette,
    /// Takes `tab`.
    OpenSettings,
    /// Takes `search`.
    OpenHistory,
}

impl KikuAction {
    pub const ALL: [KikuAction; 21] = [
        KikuAction::StartDictation,
        KikuAction::StopDictation,
        KikuAction::ToggleDictation,
        KikuAction::StartCaptions,
        KikuAction::StopCaptions,
        KikuAction::ToggleCaptions,
        KikuAction::ShowPanel,
        KikuAction::HidePanel,
        KikuAction::TogglePanel,
        KikuAction::LockPanel,
        KikuAction::UnlockPanel,
        KikuAction::ToggleLock,
        KikuAction::CaptionsModeOn,
        KikuAction::CaptionsModeOff,
        KikuAction::ToggleCaptionsMode,
        KikuAction::PasteWhatISaid,
        KikuAction::Undo,
        KikuAction::SaveMiss,
        KikuAction::ShowPalette,
        KikuAction::OpenSettings,
        KikuAction::OpenHistory,
    ];

    /// How `macros.toml` writes it: `start dictation`, `open history`, ...
    pub fn name(self) -> &'static str {
        TEXTS[self as usize].0
    }

    /// How the editor's picker shows it.
    pub fn label(self) -> &'static str {
        TEXTS[self as usize].1
    }

    /// The action called `name`, any case; an error lists every action.
    pub fn parse(name: &str) -> Result<KikuAction, String> {
        parse(name)
    }
}

impl Listed for KikuAction {
    const WHAT: &'static str = "Kiku's actions";
    fn all() -> &'static [Self] {
        &KikuAction::ALL
    }
    fn name(self) -> &'static str {
        KikuAction::name(self)
    }
}

/// Why a `kiku` step can't take the options it has: `tab` is only for open settings, `search` for open history.
pub(super) fn check_options(action: KikuAction, tab: bool, search: bool) -> Result<(), String> {
    if tab && action != KikuAction::OpenSettings {
        return Err("tab is only for open settings".into());
    }
    if search && action != KikuAction::OpenHistory {
        return Err("search is only for open history".into());
    }
    Ok(())
}

/// Each action's name and label, in declaration order.
const TEXTS: [(&str, &str); 21] = [
    ("start dictation", "Start dictation"),
    ("stop dictation", "Stop dictation"),
    ("toggle dictation", "Toggle dictation"),
    ("start captions", "Start captions"),
    ("stop captions", "Stop captions"),
    ("toggle captions", "Toggle captions"),
    ("show panel", "Show panel"),
    ("hide panel", "Hide panel"),
    ("toggle panel", "Toggle panel"),
    ("lock panel", "Lock panel"),
    ("unlock panel", "Unlock panel"),
    ("toggle lock", "Toggle lock"),
    ("captions mode on", "Captions mode on"),
    ("captions mode off", "Captions mode off"),
    ("toggle captions mode", "Toggle captions mode"),
    ("paste what I said", "Paste what I said"),
    ("undo", "Undo"),
    ("save miss", "Save miss"),
    ("show palette", "Show palette"),
    ("open settings", "Open settings"),
    ("open history", "Open history"),
];

/// A tab of the settings window, for `open settings`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsTab {
    General,
    Listening,
    Apps,
    Words,
    Appearance,
    Shortcuts,
    Macros,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 7] =
        [SettingsTab::General, SettingsTab::Listening, SettingsTab::Apps, SettingsTab::Words, SettingsTab::Appearance, SettingsTab::Shortcuts, SettingsTab::Macros];

    /// As `tab = "..."` writes it: `general`, `words`, ...
    pub fn name(self) -> &'static str {
        match self {
            SettingsTab::General => "general",
            SettingsTab::Listening => "listening",
            SettingsTab::Apps => "apps",
            SettingsTab::Words => "words",
            SettingsTab::Appearance => "appearance",
            SettingsTab::Shortcuts => "shortcuts",
            SettingsTab::Macros => "macros",
        }
    }

    /// The tab called `name`, any case; an error lists every tab.
    pub fn parse(name: &str) -> Result<SettingsTab, String> {
        parse(name)
    }
}

impl Listed for SettingsTab {
    const WHAT: &'static str = "the settings tabs";
    fn all() -> &'static [Self] {
        &SettingsTab::ALL
    }
    fn name(self) -> &'static str {
        SettingsTab::name(self)
    }
}

/// A list a `kiku` step picks a name from: Kiku's actions, the settings tabs.
pub(super) trait Listed: Copy + 'static {
    /// The list, for messages.
    const WHAT: &'static str;
    fn all() -> &'static [Self];
    fn name(self) -> &'static str;

    /// The one called `name`, any case.
    fn find(name: &str) -> Option<Self> {
        Self::all().iter().copied().find(|item| item.name().eq_ignore_ascii_case(name.trim()))
    }

    /// Why `said` (as a message may show it) isn't one, listing them all.
    fn unknown(said: &str) -> String {
        let valid: Vec<&str> = Self::all().iter().map(|item| item.name()).collect();
        format!("{said} isn't one of {}: {}", Self::WHAT, valid.join(", "))
    }
}

/// The one called `name` as a file writes it; an error quotes it only as [`shown`] allows.
fn parse<T: Listed>(name: &str) -> Result<T, String> {
    T::find(name).ok_or_else(|| T::unknown(&shown(name.trim(), "that")))
}

/// A name known when the file is read, or a template filled and checked when the step runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Named<T> {
    Known(T),
    Later(Template),
}

impl<T: Clone> Named<T> {
    /// This name with `bind` applied to its template, if it is one.
    pub(super) fn bound(&self, bind: impl Fn(&Template) -> Template) -> Named<T> {
        match self {
            Named::Later(template) => Named::Later(bind(template)),
            known => known.clone(),
        }
    }
}

/// A `kiku` step as planned.
#[derive(Clone, Debug, PartialEq)]
pub struct KikuStep {
    pub action: Named<KikuAction>,
    pub tab: Option<Named<SettingsTab>>,
    pub search: Option<Template>,
    /// A stop phrase's stop: dictation ends with no release delay. Never written in a file.
    pub at_once: bool,
    /// The macro, for notices; set only when something is checked or filled as the step runs.
    pub from: Option<String>,
}

impl KikuStep {
    /// `action` with no options.
    pub fn new(action: KikuAction) -> KikuStep {
        KikuStep { action: Named::Known(action), tab: None, search: None, at_once: false, from: None }
    }
}

/// A `kiku` step filled and checked, as the executor gets it.
#[derive(Clone, Debug, PartialEq)]
pub struct KikuRequest {
    pub action: KikuAction,
    pub tab: Option<SettingsTab>,
    /// `open history`'s search, filled; private.
    pub search: Option<Private>,
    pub at_once: bool,
}

impl KikuRequest {
    /// `action` with no options, as a command key asks for it.
    pub fn new(action: KikuAction) -> KikuRequest {
        KikuRequest { action, tab: None, search: None, at_once: false }
    }
}

/// Kiku's state for the `{dictating}`, `{captions}` and `{mode}` variables.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KikuState {
    pub dictating: bool,
    /// Captioning is running (not stopped).
    pub captions: bool,
    /// The mode's name as heard now.
    pub mode: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texts_follow_the_declaration_order() {
        for (at, action) in KikuAction::ALL.into_iter().enumerate() {
            assert_eq!(action as usize, at);
            assert!(action.label().eq_ignore_ascii_case(action.name()));
        }
    }
}
