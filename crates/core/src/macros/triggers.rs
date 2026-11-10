//! Running a macro other than by saying it while holding the dictation key: by its shortcut, or by saying it
//! as a whole line with no key held (`listen = true`, after the wake word if one is set).

use std::borrow::Cow;

use super::{Macros, Mode, Step, Target};
use crate::{
    actions::{self, matching::Said},
    keybinds::{Hotkey, Keybinds},
};

impl Macros {
    /// The shortcuts of the macros that are on, in file order, to hand the keyboard hook, with each one's macro;
    /// and the macros whose shortcut clashes with Kiku's own keys (the captions key, or it with a command key),
    /// with why.
    #[allow(clippy::type_complexity)]
    pub fn hotkeys(&self, keybinds: &Keybinds) -> (Vec<(Hotkey, String)>, Vec<(String, String)>) {
        let (mut handed, mut refused) = (Vec::new(), Vec::new());
        for macro_ in self.entries.iter().filter(|macro_| macro_.enabled) {
            let Some(hotkey) = &macro_.hotkey else { continue };
            match keybinds.clash(hotkey) {
                Some(why) => refused.push((macro_.name.clone(), why)),
                None => handed.push((hotkey.clone(), macro_.name.clone())),
            }
        }
        (handed, refused)
    }

    /// The steps of the macro named `name` run alone (by its shortcut) in the app labelled `app`: `{heard}` is
    /// empty. An error says why it can't run: there is no such macro, it is off, or where or in which modes it
    /// runs.
    pub fn plan_macro(&self, name: &str, app: &str, mode: &Mode) -> Result<Vec<Step>, String> {
        let macro_ = self.entries.iter().find(|macro_| macro_.name == name).ok_or_else(|| format!("There is no macro called {name}"))?;
        let run = *self.run_of.get(name).ok_or_else(|| format!("{name} is off"))?;
        if !macro_.apps.is_empty() && !macro_.apps.iter().any(|only| only.to_lowercase() == app.to_lowercase()) {
            return Err(format!("{name} runs only in {}", macro_.apps.join(", ")));
        }
        let mode = self.modes.effective(app, mode);
        if !macro_.runs_in(&mode) {
            return Err(format!("{name} doesn't run in {} mode", mode.name()));
        }
        Ok(self.steps(&Target { run, slots: Vec::new() }, Vec::new(), ""))
    }

    /// The steps of the listening macro `line` (a finished line from your mic, said without the dictation key)
    /// is one of the phrases of, as a whole line, in `app`. With a `wake` word (empty is none) the line must
    /// start with it, and it isn't part of the phrase; `mode` scopes it as it does dictation.
    pub fn plan_heard(&self, line: &str, app: &str, wake: &str, mode: &Mode) -> Option<Vec<Step>> {
        let line = after_wake(line, wake)?;
        let mode = self.modes.effective(app, mode);
        match self.scoped(&mode).listening.sections(app).said(&line, str::to_owned) {
            Some(Said::Line(target, taken)) => Some(self.steps(target, taken, &line)),
            _ => None,
        }
    }

    /// Whether some macro that is on listens, so the mic's lines are worth handing to [`Macros::plan_heard`].
    pub fn listens(&self) -> bool {
        self.scoped.values().any(|scoped| !scoped.listening.is_empty())
    }
}

/// What `line` says after `wake` (a word or more, case and punctuation aside); `None` when it doesn't start
/// with it or says nothing more. With no wake word, the line.
fn after_wake<'a>(line: &'a str, wake: &str) -> Option<Cow<'a, str>> {
    let wake = actions::tokens(wake);
    if wake.is_empty() {
        return Some(Cow::Borrowed(line));
    }
    let chars: Vec<char> = line.chars().collect();
    let spoken = actions::words(&chars);
    let starts = spoken.len() > wake.len() && spoken.iter().zip(&wake).all(|(word, token)| word.token == *token);
    starts.then(|| Cow::Owned(chars[spoken[wake.len() - 1].end..].iter().collect()))
}
