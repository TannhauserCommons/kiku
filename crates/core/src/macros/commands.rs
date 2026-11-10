//! Running the user's own programs, safely: off until Settings allows it, only from `custom\scripts\` unless
//! allowed from anywhere, each command approved by its exact text in the app, never through a shell.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use super::template::Template;

/// `macros.toml`'s defaults for a `run` step, and the longest it may wait.
pub(super) const DEFAULT_OUTPUT: &str = "none";
pub(super) const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// The panel waits for a run, so a program gets a minute at most.
const MAX_TIMEOUT_MS: u64 = 60_000;

/// What a `run` step does with what the program prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutput {
    Paste,
    Notify,
    /// Kept under this name for the macro's later steps.
    Variable(String),
    None,
}

impl RunOutput {
    /// `paste`, `notify`, `variable:name` or `none`; whether the name is usable is the compiler's to check.
    fn parse(text: &str) -> Result<RunOutput, String> {
        Ok(match text.trim() {
            "paste" => RunOutput::Paste,
            "notify" => RunOutput::Notify,
            DEFAULT_OUTPUT => RunOutput::None,
            other => match other.strip_prefix("variable:") {
                Some(name) if !name.trim().is_empty() => RunOutput::Variable(name.trim().to_owned()),
                _ => return Err("output is paste, notify, variable:name or none".into()),
            },
        })
    }
}

/// A program and its arguments as a macro wrote them; arguments are filled when it runs, each one argument
/// whatever it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// Relative to `custom\` (`scripts\...`), or absolute.
    pub program: PathBuf,
    pub args: Vec<Template>,
    pub output: RunOutput,
    pub timeout: Duration,
    /// The arguments as written, which is what the user approves.
    pub written: Vec<String>,
    /// Worked out once from the command as written, so binding a slot never changes it.
    fingerprint: String,
}

impl Command {
    /// A command from a step's text: a relative `program` is one of `scripts\`, `written` the arguments as
    /// written and `args` them parsed.
    pub(super) fn new(program: &str, written: &[String], args: Vec<Template>, output: &str, timeout_ms: u64) -> Result<Command, String> {
        if !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(format!("timeout_ms is from 1 to {MAX_TIMEOUT_MS}"));
        }
        let program = program_path(program)?;
        let fingerprint = fingerprint(&program, written, output);
        Ok(Command { program, args, output: RunOutput::parse(output)?, timeout: Duration::from_millis(timeout_ms), written: written.to_vec(), fingerprint })
    }

    /// A hash of the program and the arguments as written: what an approval is kept as.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// This command with other (bound) arguments; it is the same command as approved.
    pub(super) fn with_args(&self, args: Vec<Template>) -> Command {
        Command { args, ..self.clone() }
    }

    /// Whether the program is one of `custom\scripts\`.
    pub(super) fn in_scripts(&self) -> bool {
        !self.program.is_absolute()
    }
}

/// `program` as a path: absolute as it is, else inside `scripts\`, never leaving it.
fn program_path(program: &str) -> Result<PathBuf, String> {
    let path = Path::new(program.trim());
    if path.as_os_str().is_empty() {
        return Err("run needs a program".into());
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.components().any(|part| !matches!(part, Component::Normal(_) | Component::CurDir)) {
        return Err("a program of scripts\\ must stay in that folder (no .. or drive)".into());
    }
    Ok(Path::new("scripts").join(path))
}

/// SHA-256 of the program, each argument and the output as written, each led by its length so no two
/// commands share text.
fn fingerprint(program: &Path, args: &[String], output: &str) -> String {
    let mut hash = Sha256::new();
    let program = program.to_string_lossy();
    for part in std::iter::once(program.as_ref()).chain(args.iter().map(String::as_str)).chain([output.trim()]) {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hex(hash)
}

/// SHA-256 of a program file's bytes, as an approval records it.
pub fn contents_hash(bytes: &[u8]) -> String {
    hex(Sha256::new().chain_update(bytes))
}

fn hex(hash: Sha256) -> String {
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Settings → Macros' switches: whether macros may run programs, and from anywhere or only `custom\scripts\`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandPolicy {
    pub enabled: bool,
    pub anywhere: bool,
}

/// The commands the user approved in the app, kept in `approved-commands.txt` as a fingerprint a line, then
/// the hash of the program file's bytes when it was approved. The app always records the contents, so an
/// edited script needs approving again; an approval without them (`add`) covers the command as written only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Approvals(BTreeMap<String, Option<String>>);

impl Approvals {
    /// `approved-commands.txt`'s text: `<fingerprint>` or `<fingerprint> <contents hash>` a line.
    pub fn parse(text: &str) -> Approvals {
        let mut lines = text.lines().map(|line| line.split_whitespace());
        Approvals(lines.by_ref().filter_map(|mut words| Some((words.next()?.to_owned(), words.next().map(str::to_owned)))).collect())
    }

    /// Like [`Approvals::parse`], keeping only the lines that record the program file's contents: the app
    /// trusts no approval that would cover whatever the file comes to hold.
    pub fn parse_with_contents(text: &str) -> Approvals {
        let mut approvals = Approvals::parse(text);
        approvals.0.retain(|_, contents| contents.is_some());
        approvals
    }

    pub fn approves(&self, command: &Command) -> bool {
        self.0.contains_key(command.fingerprint())
    }

    /// Approves the command as written, whatever the program file holds.
    pub fn add(&mut self, command: &Command) {
        self.0.insert(command.fingerprint().to_owned(), None);
    }

    /// Approves the command and the program file as it is now (`contents` is its bytes).
    pub fn add_with_contents(&mut self, command: &Command, contents: &[u8]) {
        self.0.insert(command.fingerprint().to_owned(), Some(contents_hash(contents)));
    }

    /// The hash of the program file's bytes recorded with the approval, if any.
    pub fn contents(&self, command: &Command) -> Option<&str> {
        self.0.get(command.fingerprint())?.as_deref()
    }

    pub fn to_text(&self) -> String {
        self.0.iter().map(|(fingerprint, contents)| format!("{fingerprint}{}\n", contents.as_ref().map(|hash| format!(" {hash}")).unwrap_or_default())).collect()
    }
}

/// Where approvals are kept for the macros of `custom`: `approved-commands.txt` beside it, in Kiku's data folder.
pub fn approvals_path(custom: &Path) -> PathBuf {
    custom.parent().unwrap_or(custom).join("approved-commands.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(output: &str) -> Command {
        Command::new("a.bat", &[], vec![], output, 1000).unwrap()
    }

    #[test]
    fn approvals_keep_both_line_forms() {
        let (plain, full) = (command("none"), command("paste"));
        let mut approvals = Approvals::default();
        approvals.add(&plain);
        approvals.add_with_contents(&full, b"echo hi");
        assert_eq!(approvals.contents(&plain), None);
        assert_eq!(approvals.contents(&full), Some(contents_hash(b"echo hi").as_str()));
        let again = Approvals::parse(&approvals.to_text());
        assert_eq!(again, approvals);
        assert!(again.approves(&plain) && again.approves(&full));
        assert_eq!(approvals.to_text().lines().count(), 2);
    }

    #[test]
    fn approvals_without_contents_are_dropped_when_asked() {
        let (plain, full) = (command("none"), command("paste"));
        let mut approvals = Approvals::default();
        approvals.add(&plain);
        approvals.add_with_contents(&full, b"echo hi");
        let trusted = Approvals::parse_with_contents(&approvals.to_text());
        assert!(!trusted.approves(&plain) && trusted.approves(&full));
    }

    #[test]
    fn output_is_part_of_the_approval() {
        assert_ne!(command("none").fingerprint(), command("paste").fingerprint());
    }

    #[test]
    fn timeout_is_at_most_a_minute() {
        let why = Command::new("a.bat", &[], vec![], "none", 60_001).unwrap_err();
        assert!(why.contains("60000"), "{why}");
        assert!(Command::new("a.bat", &[], vec![], "none", 60_000).is_ok());
    }
}
