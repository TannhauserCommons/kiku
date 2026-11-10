//! What can be said here: the macros the palette lists for the app in front and the mode.

use super::{file::Macro, Macros, Mode};
use crate::keybinds::Hotkey;

/// A macro that would run here, as the palette shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    pub name: String,
    /// The first thing to say, if it has one.
    pub phrase: Option<String>,
    pub hotkey: Option<Hotkey>,
}

impl Macros {
    /// The macros of `macros.toml` that would run in `app` in `mode` (on, in their apps and modes), in file order.
    pub fn available(&self, app: &str, mode: &Mode) -> Vec<Available> {
        let mode = self.modes.effective(app, mode);
        let here = |macro_: &&Macro| macro_.enabled && macro_.runs_in(&mode) && (macro_.apps.is_empty() || macro_.apps.iter().any(|only| only.eq_ignore_ascii_case(app)));
        self.entries.iter().filter(here).map(|macro_| Available { name: macro_.name.clone(), phrase: macro_.say.first().cloned(), hotkey: macro_.hotkey.clone() }).collect()
    }
}
