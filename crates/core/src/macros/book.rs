//! The macros the app holds: `custom\macros.toml` and `variables.toml`, read again when either file
//! changes, or when `words.txt` does (its `>` lines are macros too).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::{approvals_path, Approvals, BuiltinPhrases, CommandPolicy, Macros, Variables, README};
use crate::{file_watch::FileWatch, log, paths, transcripts, words::Words};

pub struct MacroBook {
    folder: PathBuf,
    state: Mutex<Loaded>,
}

#[derive(Default)]
struct Loaded {
    macros: Arc<Macros>,
    variables: Arc<Variables>,
    /// The rules the macros were built with.
    words: Option<Arc<Words>>,
    macros_file: FileWatch,
    variables_file: FileWatch,
    /// What Settings say, given to the macros each time they are built.
    phrases: BuiltinPhrases,
    policy: CommandPolicy,
    /// The approvals that record a program's contents, from `approved-commands.txt`.
    approvals: Approvals,
    approvals_file: FileWatch,
}

impl Loaded {
    /// Gives the macros the settings and approvals.
    fn equip(&mut self) {
        let macros = Arc::make_mut(&mut self.macros);
        macros.set_phrases(self.phrases);
        macros.set_commands(self.policy, self.approvals.clone());
    }
}

impl Default for MacroBook {
    fn default() -> Self {
        Self::at(paths::custom_dir())
    }
}

impl MacroBook {
    /// The macros kept in `folder`, read on the first [`MacroBook::refresh`].
    pub fn at(folder: PathBuf) -> Self {
        Self { folder, state: Mutex::default() }
    }

    /// The folder macros' relative paths are read from.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Which built-in phrases the macros hear (Settings).
    pub fn set_phrases(&self, phrases: BuiltinPhrases) {
        let mut state = self.state.lock().unwrap();
        if state.phrases != phrases {
            state.phrases = phrases;
            state.equip();
        }
    }

    /// Whether macros may run programs (Settings).
    pub fn set_policy(&self, policy: CommandPolicy) {
        let mut state = self.state.lock().unwrap();
        if state.policy != policy {
            state.policy = policy;
            state.equip();
        }
    }

    /// Reads the files again if any changed (looking at most once a second; `approved-commands.txt` too) or
    /// `words` is not what the macros were built with; returns whether they were read. Macros that can't load are logged by place and
    /// reason, never by value.
    pub fn refresh(&self, words: &Arc<Words>) -> bool {
        let mut state = self.state.lock().unwrap();
        let words_changed = state.words.as_ref().is_none_or(|known| !Arc::ptr_eq(known, words));
        let macros_changed = state.macros_file.changed(&self.folder.join("macros.toml"));
        let variables_changed = state.variables_file.changed(&self.folder.join("variables.toml"));
        let approvals_changed = state.approvals_file.changed(&approvals_path(&self.folder));
        if !(words_changed || macros_changed || variables_changed || approvals_changed) {
            return false;
        }
        if approvals_changed {
            state.approvals = Approvals::parse_with_contents(&fs::read_to_string(approvals_path(&self.folder)).unwrap_or_default());
        }
        if words_changed || macros_changed || variables_changed {
            if state.words.is_none() {
                self.write_readme();
            }
            let variables = self.read_variables();
            let macros = Macros::parse(&self.read("macros.toml"), words, &variables);
            for (place, why) in &macros.skipped {
                log::write(format!("{place} skipped: {why}"));
            }
            state.words = Some(words.clone());
            state.variables = Arc::new(variables);
            state.macros = Arc::new(macros);
        }
        state.equip();
        true
    }

    /// The macros as last read.
    pub fn macros(&self) -> Arc<Macros> {
        self.state.lock().unwrap().macros.clone()
    }

    /// The user's own values as last read.
    pub fn variables(&self) -> Arc<Variables> {
        self.state.lock().unwrap().variables.clone()
    }

    /// Writes `README.md` when the folder has none; one that is there is never touched.
    fn write_readme(&self) {
        let path = self.folder.join("README.md");
        if !path.exists()
            && let Err(why) = fs::create_dir_all(&self.folder).and_then(|()| transcripts::replace(&path, README))
        {
            log::write(format!("{} not written: {why}", path.display()));
        }
    }

    fn read_variables(&self) -> Variables {
        Variables::parse(&self.read("variables.toml")).unwrap_or_else(|why| {
            log::write(format!("variables.toml skipped: {why}"));
            Variables::default()
        })
    }

    /// A file of the folder; a missing or unreadable one is empty.
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.folder.join(name)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_read_once_and_again_when_the_rules_change() {
        let folder = std::env::temp_dir().join(format!("kiku-book-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("macros.toml"), "[[macro]]\nname = \"hi\"\nsay = \"hi\"\nsteps = [ { paste = \"{name}\" } ]\n").unwrap();
        fs::write(folder.join("variables.toml"), "name = \"Sam\"\n").unwrap();
        let book = MacroBook::at(folder.clone());
        let words = Arc::new(Words::default());
        assert!(book.refresh(&words));
        assert_eq!(book.macros().entries().len(), 1);
        assert!(book.macros().skipped.is_empty());
        assert!(!book.refresh(&words), "nothing changed");
        assert!(book.refresh(&Arc::new(Words::default())), "other rules");
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn the_readme_is_written_once_and_never_overwritten() {
        let folder = std::env::temp_dir().join(format!("kiku-readme-{}", std::process::id())).join("custom");
        let readme = folder.join("README.md");
        let words = Arc::new(Words::default());
        MacroBook::at(folder.clone()).refresh(&words);
        assert_eq!(fs::read_to_string(&readme).unwrap(), README);
        fs::write(&readme, "mine").unwrap();
        MacroBook::at(folder.clone()).refresh(&words);
        assert_eq!(fs::read_to_string(&readme).unwrap(), "mine");
        fs::remove_dir_all(folder.parent().unwrap()).unwrap();
    }
}
