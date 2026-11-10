//! Voice macros: a phrase said in dictation runs an ordered list of steps (pastes, key presses, files put on
//! the clipboard, ...) filled with variables, in the app in front. Macros live in `custom\macros.toml`; the
//! `>` lines of `words.txt` are one-step macros in the same index.

use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use crate::{
    actions::matching::{Index, Trigger},
    words::{Kind, Words},
};

mod book;
mod commands;
mod compile;
mod file;
mod modes;
mod open;
mod palette;
mod plan;
mod report;
mod template;
mod transcript;
mod triggers;

/// The time templates are filled at, re-exported so the app needs no `chrono` of its own.
pub use chrono::NaiveDateTime;
pub use book::MacroBook;
pub use commands::{approvals_path, contents_hash, Approvals, Command, CommandPolicy, RunOutput};
pub use file::{read_text_file, Macro, StepSpec, Variables, MAX_ATTACHED, MAX_FILE_BYTES};
pub use modes::{BuiltinPhrases, Mode, Modes};
pub use open::{open_target, OpenTarget};
pub use palette::Available;
pub use plan::{run, run_step, Executor, Step};
pub use report::Customization;
pub use template::{Context, Template};
pub use transcript::{HowMuch, Transcript, TranscriptLine, Who};

/// What `custom\README.md` holds, for people and agents.
pub const README: &str = include_str!("readme.md");

/// The local time now, for templates.
pub fn local_now() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

use compile::{compile, from_action};
use plan::Prepared;

/// What a phrase in the index runs: the steps of macro `run`, and the names its slots give what they take.
#[derive(Clone, PartialEq)]
struct Target {
    run: usize,
    slots: Vec<String>,
}

/// What can be said in one mode: every phrase that works there, and those of the macros that listen.
#[derive(Clone, Default)]
struct Scoped {
    index: Index<Target>,
    listening: Index<Target>,
}

/// A mode's triggers as they are collected: those said, and those of macros that listen.
type Triggers<'a> = (Vec<Trigger<'a, Target>>, Vec<Trigger<'a, Target>>);

/// Every macro, from `macros.toml` and `words.txt`'s `>` lines, indexed for dictation.
#[derive(Clone)]
pub struct Macros {
    /// Macros that couldn't be read: where (`macros.toml:12`, the line of its `[[macro]]`) and why. Reasons
    /// never quote a step's text or a variable's value.
    pub skipped: Vec<(String, String)>,
    text: String,
    words: Words,
    variables: Variables,
    entries: Vec<Macro>,
    /// Each entry's place among the file's `[[macro]]` tables.
    tables: Vec<usize>,
    /// The steps of every macro that can run, by `Target::run`.
    runs: Vec<Vec<Prepared>>,
    /// What the Try boxes show for each of `runs`.
    labels: Vec<String>,
    /// Each loaded macro that is on, by name: its place in `runs`.
    run_of: HashMap<String, usize>,
    /// What each mode hears, built when the file is read so a line costs lookups, not a scan.
    scoped: HashMap<Mode, Scoped>,
    modes: Modes,
    phrases: BuiltinPhrases,
    policy: CommandPolicy,
    approvals: Approvals,
    /// Every `run` step's command with its macro's name, in file order.
    commands: Vec<(String, Command)>,
}

impl Default for Macros {
    fn default() -> Self {
        Macros::build(String::new(), Words::default(), Variables::default())
    }
}

impl Macros {
    /// Reads `macros.toml`'s text, with `words`' `>` lines as one-step macros and `variables`' names known.
    pub fn parse(toml: &str, words: &Words, variables: &Variables) -> Macros {
        Macros::build(toml.to_owned(), words.clone(), variables.clone())
    }

    /// The macros of `macros.toml`, in file order, those turned off too.
    pub fn entries(&self) -> &[Macro] {
        &self.entries
    }

    /// Adds `new` at the end of the file, keeping its comments and order; a macro that wouldn't load (or
    /// whose name is taken) is refused, saying why.
    pub fn add(&mut self, new: &Macro) -> Result<(), String> {
        if self.entries.iter().any(|known| known.name == new.name) {
            return Err("There is a macro with that name already".into());
        }
        compile(new, &self.variables, &self.modes)?;
        shortcut_is_free(&self.entries, new, None)?;
        let text = file::with_macro(&self.text, new)?;
        self.reload(text);
        Ok(())
    }

    /// Writes `new` where the macro named `name` is, keeping its place and the comments before it; refused,
    /// saying why, when there is no such macro or `new` wouldn't load or takes another macro's name.
    pub fn replace(&mut self, name: &str, new: &Macro) -> Result<(), String> {
        let at = self.entries.iter().position(|known| known.name == name).ok_or("There is no macro with that name")?;
        if new.name != name && self.entries.iter().any(|known| known.name == new.name) {
            return Err("There is a macro with that name already".into());
        }
        compile(new, &self.variables, &self.modes)?;
        shortcut_is_free(&self.entries, new, Some(name))?;
        let text = file::with_macro_at(&self.text, new, self.tables[at])?;
        self.reload(text);
        Ok(())
    }

    /// Removes the macro named `name`; false when there is none.
    pub fn remove(&mut self, name: &str) -> bool {
        let Some(at) = self.entries.iter().position(|known| known.name == name) else { return false };
        match file::without_macro(&self.text, self.tables[at]) {
            Ok(text) => {
                self.reload(text);
                true
            }
            Err(_) => false,
        }
    }

    /// `macros.toml`'s text, as written with any edits.
    pub fn to_text(&self) -> String {
        self.text.clone()
    }

    /// The user's modes from `macros.toml`'s `[[mode]]` tables.
    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// Which built-in phrases plans hear (Settings).
    pub fn set_phrases(&mut self, phrases: BuiltinPhrases) {
        self.phrases = phrases;
    }

    /// Whether `run` steps may run (Settings) and the commands the user approved: a `run` step that may not
    /// plans as a notice saying why.
    pub fn set_commands(&mut self, policy: CommandPolicy, approvals: Approvals) {
        (self.policy, self.approvals) = (policy, approvals);
    }

    /// Every `run` step's command, in file order, with its macro's name: what the Macros tab asks to approve.
    pub fn commands(&self) -> Vec<(String, Command)> {
        self.commands.clone()
    }

    /// What `mode` hears; a mode the file no longer has hears what `normal` does.
    fn scoped(&self, mode: &Mode) -> &Scoped {
        self.scoped.get(mode).unwrap_or(&self.scoped[&Mode::Normal])
    }

    fn reload(&mut self, text: String) {
        let (phrases, policy, approvals) = (self.phrases, self.policy, std::mem::take(&mut self.approvals));
        *self = Macros::build(text, std::mem::take(&mut self.words), std::mem::take(&mut self.variables));
        (self.phrases, self.policy, self.approvals) = (phrases, policy, approvals);
    }

    fn build(text: String, words: Words, variables: Variables) -> Macros {
        let (mut entries, mut tables, mut compiled, mut skipped) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let found = file::read_modes(&text);
        let (modes, refused) = Modes::from_tables(found.iter().map(|(_, table)| table.clone()).collect());
        skipped.extend(refused.into_iter().map(|(at, why)| (format!("macros.toml:{}", file::line_of(&text, found[at].0)), why)));
        let mut names = HashSet::new();
        let mut load = |table: usize, read: Result<Macro, String>| {
            let macro_ = read?;
            if !names.insert(macro_.name.clone()) {
                return Err(format!("another macro is called {}", macro_.name));
            }
            let ready = compile(&macro_, &variables, &modes)?;
            shortcut_is_free(&entries, &macro_, None)?;
            compiled.push(ready);
            entries.push(macro_);
            tables.push(table);
            Ok(())
        };
        match file::read_macros(&text) {
            Ok(read) => {
                for (table, (offset, read)) in read.into_iter().enumerate() {
                    if let Err(why) = load(table, read) {
                        skipped.push((format!("macros.toml:{}", file::line_of(&text, offset)), why));
                    }
                }
            }
            Err((line, why)) => skipped.push((format!("macros.toml:{line}"), why)),
        }
        let commands = entries.iter().zip(&compiled).flat_map(|(macro_, ready)| ready.steps.iter().filter_map(|step| match step {
            Prepared::Ready(Step::Run { command, .. }) => Some((macro_.name.clone(), command.clone())),
            _ => None,
        })).collect::<Vec<_>>();
        let (mut runs, mut labels, mut run_of) = (Vec::new(), Vec::new(), HashMap::new());
        let mut triggers: HashMap<Mode, Triggers> = modes.all().map(|mode| (mode, Default::default())).collect();
        let quick = words.entries().iter().filter_map(|entry| match &entry.kind {
            Kind::Action(action) => Some((vec![entry.section.as_deref()], from_action(action), None, false, &[][..])),
            _ => None,
        });
        let written = entries.iter().zip(compiled).filter(|(macro_, _)| macro_.enabled).map(|(macro_, compiled)| {
            let sections = if macro_.apps.is_empty() { vec![None] } else { macro_.apps.iter().map(|app| Some(app.as_str())).collect() };
            (sections, compiled, Some(macro_.name.as_str()), macro_.listen, &macro_.modes[..])
        });
        for (sections, compiled, name, listens, scope) in quick.chain(written) {
            let run = runs.len();
            for phrase in &compiled.phrases {
                for &section in &sections {
                    let trigger = || Trigger { section, phrase: phrase.parts.clone(), anywhere: compiled.anywhere, value: Target { run, slots: phrase.slots.clone() } };
                    for (_, (said, listened)) in triggers.iter_mut().filter(|(mode, _)| modes::works_in(scope, mode)) {
                        if listens {
                            listened.push(trigger());
                        }
                        said.push(trigger());
                    }
                }
            }
            if let Some(name) = name {
                run_of.insert(name.to_owned(), run);
            }
            runs.push(compiled.steps);
            labels.push(compiled.label);
        }
        let scoped = triggers.into_iter().map(|(mode, (said, listened))| (mode, Scoped { index: Index::build(said.into_iter()), listening: Index::build(listened.into_iter()) })).collect();
        let (phrases, policy, approvals) = Default::default();
        Macros { skipped, text, words, variables, entries, tables, runs, labels, run_of, scoped, modes, phrases, policy, approvals, commands }
    }
}

/// Why `new`'s shortcut is another macro's (not that of `except`, the macro it replaces).
fn shortcut_is_free(entries: &[Macro], new: &Macro, except: Option<&str>) -> Result<(), String> {
    let Some(hotkey) = &new.hotkey else { return Ok(()) };
    match entries.iter().find(|known| known.hotkey.as_ref() == Some(hotkey) && Some(known.name.as_str()) != except) {
        Some(known) => Err(format!("{} is already the shortcut of {}", hotkey.name(), known.name)),
        None => Ok(()),
    }
}

/// Counts only: macros hold private text.
impl fmt::Debug for Macros {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Macros").field("macros", &self.entries.len()).field("skipped", &self.skipped.len()).finish()
    }
}
