//! The macro palette: a popover in the panel listing what can be said in the app in front, in the mode; a
//! click runs one.

use kiku_core::macros::{Available, Macros, Mode, Step};

/// The palette as opened: for an app and a mode, with what was available then.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub app: String,
    pub mode: Mode,
    pub entries: Vec<Available>,
}

impl Palette {
    /// The palette for `app` in `mode`.
    pub fn open(macros: &Macros, app: &str, mode: &Mode) -> Palette {
        Palette { app: app.to_owned(), mode: mode.clone(), entries: macros.available(app, mode) }
    }

    /// The steps of the entry at `at`, run as its shortcut would run it; an error says why it can't.
    pub fn steps(&self, at: usize, macros: &Macros) -> Result<Vec<Step>, String> {
        let entry = self.entries.get(at).ok_or("That macro isn't listed any more")?;
        macros.plan_macro(&entry.name, &self.app, &self.mode)
    }
}
