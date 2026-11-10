//! Settings → General's Dictation card: how the key dictates, how long Kiku keeps listening after it ends,
//! and the stop phrases. The window saves [`DictationCard::settings`] after each change.

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_ESCAPE};

use kiku_core::dictation::{DictationKey, DictationSettings};

use super::field::Field;

/// Something to click in the Dictation card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationAction {
    Key(DictationKey),
    /// Keep listening for this many milliseconds, one of `dictation::RELEASE_DELAYS`.
    Delay(u64),
    /// Sends typing to the stop phrases' box.
    FocusStop,
}

#[derive(Default)]
pub struct DictationCard {
    key: DictationKey,
    /// Kept as the settings have it, so a delay the card doesn't offer stays until another is clicked.
    release_delay: Duration,
    /// The stop phrases as typed, apart by commas.
    pub stop: Field,
    typing: bool,
}

impl DictationCard {
    /// The card showing `settings`.
    pub fn open(settings: &DictationSettings) -> DictationCard {
        let mut stop = Field::default();
        stop.set(&settings.stop.join(", "));
        DictationCard { key: settings.key, release_delay: settings.release_delay, stop, typing: false }
    }

    pub fn click(&mut self, action: DictationAction) {
        self.typing = action == DictationAction::FocusStop;
        match action {
            DictationAction::Key(key) => self.key = key,
            DictationAction::Delay(ms) => self.release_delay = Duration::from_millis(ms),
            DictationAction::FocusStop => {}
        }
    }

    pub fn key_chosen(&self) -> DictationKey {
        self.key
    }

    pub fn delay_chosen(&self) -> Duration {
        self.release_delay
    }

    /// Whether typing goes to the stop phrases' box.
    pub fn typing(&self) -> bool {
        self.typing
    }

    pub fn stop_typing(&mut self) {
        self.typing = false;
    }

    /// Text typed into the stop phrases' box, for the tests; the window sends units and keys.
    #[cfg(test)]
    pub fn type_text(&mut self, text: &str) {
        self.stop.insert(text);
    }

    /// A UTF-16 unit typed into the stop phrases' box, as Windows sends them.
    pub fn type_unit(&mut self, unit: u16) {
        self.stop.type_unit(unit);
    }

    /// A key pressed in the box: an editing key edits it, Escape leaves it.
    pub fn key(&mut self, key: VIRTUAL_KEY, ctrl: bool, paste: impl FnOnce() -> Option<String>) {
        if !self.stop.edit(key, ctrl, paste) && key == VK_ESCAPE {
            self.typing = false;
        }
    }

    /// The settings as the card has them now.
    pub fn settings(&self) -> DictationSettings {
        let stop = self.stop.text.split(',').map(str::trim).filter(|phrase| !phrase.is_empty()).map(str::to_owned).collect();
        DictationSettings { key: self.key, release_delay: self.release_delay, stop }
    }
}
