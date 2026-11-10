//! Planning dictated lines into steps, and running the steps through an [`Executor`].

use std::{
    borrow::Cow,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{
    commands::{contents_hash, Command, RunOutput},
    modes::Mode,
    open::{open_target, OpenTarget},
    template::{Context, Template},
    Macros, Target,
};
use crate::{
    actions::{matching::{Chunk, Said}, Private},
    keybinds::Key,
    words::{Output, WordSettings},
};

/// The largest program file whose contents are checked against an approval.
const MAX_PROGRAM_BYTES: u64 = 64 << 20;

/// One thing a plan does, in order. Templates are filled when it runs, not when it is planned.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Text pasted at once; `from` names the macro when the text is only that macro's.
    Paste { text: Template, from: Option<String> },
    /// A text file's contents, its variables filled unless `raw`.
    PasteFile { path: PathBuf, raw: bool, from: String },
    /// Files put on the clipboard as files, then pasted.
    Attach { files: Vec<PathBuf>, from: String },
    /// Chords pressed one after another.
    Press(Vec<Vec<Key>>),
    /// The selection (`None`) or this text, to the clipboard.
    Copy { text: Option<Template>, from: String },
    /// Key by key; only when a macro says so.
    Type { text: Template, from: String },
    Wait(Duration),
    Notify(String),
    /// A link, file, folder or app, checked by [`super::open_target`] once filled.
    Open { target: Template, from: String },
    /// Brings the app of this name to the front.
    Switch { app: Template, from: String },
    /// A program the user allowed and approved; `contents` is the hash of its file when approved, checked
    /// just before it runs.
    Run { command: Command, from: String, contents: Option<String> },
    /// Its filled text, handed to Claude.
    AskClaude { text: Template, from: String },
    SetMode(Mode),
    /// Opens the palette of what can be said here.
    ShowPalette,
}

/// What runs steps in the app in front: the app's goes through the lent clipboard and the keyboard.
pub trait Executor {
    fn paste_text(&self, text: &Private);
    fn paste_files(&self, files: &[PathBuf]);
    fn press(&self, chords: &[Vec<Key>]);
    /// The selection (`None`) or this text, to the clipboard.
    fn copy(&self, text: Option<&Private>);
    fn type_text(&self, text: &Private);
    fn wait(&self, how_long: Duration);
    /// A message on the panel; holds no private value but a program's output a macro asked to show.
    fn notify(&self, message: &str);
    /// Asked between steps: true stops the rest.
    fn cancelled(&self) -> bool;
    /// Opens a target [`super::open_target`] allowed; an error says why it couldn't.
    fn open(&self, target: &OpenTarget) -> Result<(), String>;
    /// Brings the newest window of `app` (as captions name it) to the front; an error when it has none.
    fn switch_to(&self, app: &str) -> Result<(), String>;
    /// Runs `program` (resolved) with each of `args` as one argument, in a hidden window, ended at `timeout`;
    /// what it printed, or why it failed. The error says why in Kiku's words (not found, timed out, exit code
    /// N) and never holds the program's output, stderr or an argument.
    fn run_command(&self, program: &Path, args: &[Private], timeout: Duration) -> Result<Private, String>;
    /// Hands `text` to Claude as the ✨ buttons do.
    fn ask_claude(&self, text: &Private) -> Result<(), String>;
    fn set_mode(&self, mode: &Mode);
    fn show_palette(&self);
}

impl Macros {
    /// What dictating `lines` (finished lines, as heard, in order) does in the app labelled `app` in `mode` (as
    /// chosen; an app's own mode applies by [`super::Modes::effective`]): text not taken by a macro goes through
    /// the word rules as dictation does, and adjacent text is one paste.
    pub fn plan(&self, lines: &[&str], app: &str, settings: &WordSettings, mode: &Mode) -> Vec<Step> {
        let apply = |line: &str| self.words.apply(line, app, Output::Dictation, settings);
        let mut chosen = mode.clone();
        let mut batch = Batch::default();
        for line in lines {
            if let Some(step) = self.phrases.heard(line) {
                chosen = step.mode_set().unwrap_or(chosen);
                batch.steps(vec![step]);
                batch.after_paste = false;
                continue;
            }
            let mode = self.modes.effective(app, &chosen);
            let index = &self.scoped(&mode).index;
            let pastes = !matches!(mode, Mode::Commands | Mode::Off);
            if index.is_empty() {
                let text = apply(line);
                if pastes && !text.trim().is_empty() {
                    batch.text(&text);
                    batch.text(" ");
                }
                continue;
            }
            match index.sections(app).said(line, apply) {
                Some(Said::Line(target, taken)) => {
                    let steps = self.steps(target, taken, line);
                    let after_paste = matches!(steps.last(), Some(Step::Paste { .. }));
                    if matches!(steps.first(), Some(Step::Paste { .. })) {
                        batch.space_after_paste();
                    }
                    chosen = steps.iter().rev().find_map(Step::mode_set).unwrap_or(chosen);
                    batch.steps(steps);
                    batch.after_paste = after_paste;
                }
                Some(Said::Text(chunks)) if pastes => {
                    batch.space_after_paste();
                    for chunk in chunks {
                        match chunk {
                            Chunk::Text(text) => batch.text(&text),
                            Chunk::Trigger(target) => batch.steps(self.steps(target, Vec::new(), line)),
                        }
                    }
                    batch.text(" ");
                    batch.after_paste = false;
                }
                _ => {}
            }
        }
        batch.finish()
    }

    /// The steps of the macro `target` finds, with its slots taking `taken` and `{heard}` the line.
    pub(super) fn steps(&self, target: &Target, taken: Vec<String>, line: &str) -> Vec<Step> {
        let heard = std::iter::once(("heard", line.trim().to_owned()));
        let values: Vec<(&str, Private)> = target.slots.iter().map(String::as_str).zip(taken).chain(heard).map(|(name, value)| (name, Private::new(value))).collect();
        self.runs[target.run].iter().map(|step| self.allowed(step.bound(&values))).collect()
    }

    /// `step`, or a notice saying why not when it runs a program that Settings or the approvals don't allow.
    fn allowed(&self, step: Step) -> Step {
        let Step::Run { command, from, .. } = &step else { return step };
        let refusal = if !self.policy.enabled {
            "running programs is off (Settings → Macros)"
        } else if !self.policy.anywhere && !command.in_scripts() {
            "programs run only from custom\\scripts\\ (Settings → Macros can allow anywhere)"
        } else if !self.approvals.approves(command) {
            "this command needs approval in Settings → Macros"
        } else {
            let contents = self.approvals.contents(command).map(str::to_owned);
            return Step::Run { command: command.clone(), from: from.clone(), contents };
        };
        Step::Notify(format!("{from}: {refusal}"))
    }

    /// The Words tab's Try rows for `text`: `Words::preview`'s, with dictation showing macros (`[its name]`,
    /// nothing filled or run) and `>` lines as `Words` does.
    pub fn preview_rows(&self, text: &str, settings: &WordSettings) -> Vec<(String, String)> {
        self.words.preview_with(text, settings, |app| self.preview(text, app, settings))
    }

    /// The labels of the macros that dictating `line` in `app` runs, found as `plan` finds them.
    pub(super) fn triggered(&self, line: &str, app: &str, settings: &WordSettings) -> Vec<String> {
        let label = |target: &Target| self.labels[target.run].clone();
        let index = &self.scoped(&self.modes.effective(app, &Mode::Normal)).index;
        match index.sections(app).said(line, |line| self.words.apply(line, app, Output::Dictation, settings)) {
            Some(Said::Line(target, _)) => vec![label(target)],
            Some(Said::Text(chunks)) => chunks.iter().filter_map(|chunk| match chunk {
                Chunk::Trigger(target) => Some(label(target)),
                Chunk::Text(_) => None,
            }).collect(),
            None => Vec::new(),
        }
    }

    /// Dictating `text` in `app` as the Try boxes show it.
    pub fn preview(&self, text: &str, app: &str, settings: &WordSettings) -> String {
        let label = |target: &Target| self.labels[target.run].clone();
        let shown = match self.scoped(&Mode::Normal).index.sections(app).said(text, |line| self.words.apply(line, app, Output::Dictation, settings)) {
            None => String::new(),
            Some(Said::Line(target, _)) => label(target),
            Some(Said::Text(chunks)) => chunks.into_iter().map(|chunk| match chunk {
                Chunk::Text(text) => text,
                Chunk::Trigger(target) => label(target),
            }).collect(),
        };
        shown.trim_end().to_owned()
    }
}

/// The most one chord may repeat, so a spoken number can't hold the keyboard.
pub(super) const MAX_REPEAT: usize = 100;

/// A step as compiled: a press may repeat by a slot's value, known only when the plan is made.
#[derive(Clone)]
pub(super) enum Prepared {
    Ready(Step),
    Press(Vec<Beat>),
}

/// A chord and how many times it is pressed.
#[derive(Clone)]
pub(super) struct Beat {
    pub chord: Vec<Key>,
    pub times: Times,
}

#[derive(Clone)]
pub(super) enum Times {
    Fixed(usize),
    Slot(String),
}

impl Prepared {
    /// The step, with each of `values` in place of its name in the templates and the repeat counts.
    fn bound(&self, values: &[(&str, Private)]) -> Step {
        let count = |times: &Times| match times {
            Times::Fixed(count) => *count,
            Times::Slot(name) => values.iter().find(|(known, _)| known == name).and_then(|(_, value)| value.reveal().parse().ok()).map_or(0, |count: usize| count.min(MAX_REPEAT)),
        };
        match self {
            Prepared::Ready(step) => step.bound(values),
            Prepared::Press(beats) => Step::Press(beats.iter().flat_map(|beat| std::iter::repeat_n(beat.chord.clone(), count(&beat.times))).collect()),
        }
    }
}

impl Step {
    /// The mode this step switches to, if it does.
    fn mode_set(&self) -> Option<Mode> {
        match self {
            Step::SetMode(mode) => Some(mode.clone()),
            _ => None,
        }
    }

    /// This step with each of `values` in place of its name in the templates.
    pub fn bound(&self, values: &[(&str, Private)]) -> Step {
        let bind = |template: &Template| values.iter().fold(template.clone(), |template, (name, value)| template.bind(name, value));
        match self {
            Step::Paste { text, from } => Step::Paste { text: bind(text), from: from.clone() },
            Step::Copy { text, from } => Step::Copy { text: text.as_ref().map(bind), from: from.clone() },
            Step::Type { text, from } => Step::Type { text: bind(text), from: from.clone() },
            Step::Open { target, from } => Step::Open { target: bind(target), from: from.clone() },
            Step::Switch { app, from } => Step::Switch { app: bind(app), from: from.clone() },
            Step::AskClaude { text, from } => Step::AskClaude { text: bind(text), from: from.clone() },
            Step::Run { command, from, contents } => Step::Run { command: command.with_args(command.args.iter().map(bind).collect()), from: from.clone(), contents: contents.clone() },
            other => other.clone(),
        }
    }
}

/// A plan being made: text waits to join the text next to it, so adjacent text is one paste.
#[derive(Default)]
struct Batch {
    steps: Vec<Step>,
    waiting: Template,
    /// The macro the waiting text is only from, if it is.
    from: Option<String>,
    /// The last thing planned was a macro's paste, which a following text is separated from by a space.
    after_paste: bool,
}

impl Batch {
    fn text(&mut self, text: &str) {
        self.waiting.push_str(text);
        self.from = None;
    }

    fn space_after_paste(&mut self) {
        if std::mem::take(&mut self.after_paste) {
            self.text(" ");
        }
    }

    fn steps(&mut self, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Paste { text, from } => {
                    self.from = if self.waiting.is_empty() || self.from == from { from } else { None };
                    self.waiting.append(&text);
                }
                other => {
                    self.flush();
                    self.steps.push(other);
                }
            }
        }
    }

    fn flush(&mut self) {
        if !self.waiting.is_empty() {
            self.steps.push(Step::Paste { text: std::mem::take(&mut self.waiting), from: self.from.take() });
        }
    }

    fn finish(mut self) -> Vec<Step> {
        self.flush();
        self.steps
    }
}

/// Runs one step, as the panel does on each tick; a `run` step whose output goes to a variable gives its name
/// and value, which the steps after it are bound with.
pub fn run_step(step: &Step, context: &dyn Context, executor: &dyn Executor) -> Option<(String, Private)> {
    let fill = |text: &Template, from: Option<&str>| text.fill(context).map_err(|why| notice(executor, from, &why)).ok();
    let paste = |text: Private, from: Option<&str>| {
        if text.reveal().is_empty() {
            if let Some(from) = from {
                notice(executor, Some(from), "had nothing to paste");
            }
        } else {
            executor.paste_text(&text);
        }
    };
    match step {
        Step::Paste { text, from } => {
            if let Some(text) = fill(text, from.as_deref()) {
                paste(text, from.as_deref());
            }
        }
        Step::PasteFile { path, raw, from } => {
            let from = Some(from.as_str());
            let Ok(contents) = context.read_file(&context.resolve(path)).map_err(|why| notice(executor, from, &why)) else { return None };
            if *raw {
                paste(contents, from);
                return None;
            }
            // A file's text is private: its errors say where, never what.
            let cant_fill = || notice(executor, from, &format!("{} has a {{…}} Kiku can't fill (paste it with raw = true)", path.display()));
            match Template::parse(contents.reveal()).ok().and_then(|template| template.fill(context).ok()) {
                Some(text) => paste(text, from),
                None => cant_fill(),
            }
        }
        Step::Attach { files, from } => {
            let files: Vec<PathBuf> = files.iter().map(|file| context.resolve(file)).collect();
            match files.iter().find(|file| !file.is_file()) {
                Some(missing) => notice(executor, Some(from), &format!("{} doesn't exist", missing.display())),
                None => executor.paste_files(&files),
            }
        }
        Step::Press(chords) => executor.press(chords),
        Step::Copy { text: None, .. } => executor.copy(None),
        Step::Copy { text: Some(text), from } => {
            if let Some(text) = fill(text, Some(from)) {
                if text.reveal().is_empty() {
                    notice(executor, Some(from), "had nothing to copy");
                } else {
                    executor.copy(Some(&text));
                }
            }
        }
        Step::Type { text, from } => {
            if let Some(text) = fill(text, Some(from)) {
                executor.type_text(&text);
            }
        }
        Step::Wait(how_long) => executor.wait(*how_long),
        Step::Notify(message) => executor.notify(message),
        Step::SetMode(mode) => executor.set_mode(mode),
        Step::ShowPalette => executor.show_palette(),
        Step::Open { target, from } => {
            let from = Some(from.as_str());
            let text = fill(target, from)?;
            if let Err(why) = open_target(text.reveal()).and_then(|target| executor.open(&target)) {
                notice(executor, from, &why);
            }
        }
        Step::Switch { app, from } => {
            let from = Some(from.as_str());
            let text = fill(app, from)?;
            match text.reveal().trim() {
                "" => notice(executor, from, "there is no app to switch to"),
                app => {
                    if let Err(why) = executor.switch_to(app) {
                        notice(executor, from, &why);
                    }
                }
            }
        }
        Step::Run { command, from, contents } => {
            let from = Some(from.as_str());
            let output = run_program(command, contents.as_deref(), from, context, executor)?;
            match &command.output {
                RunOutput::Paste => paste(output, None),
                RunOutput::Notify => notice(executor, from, output.reveal()),
                RunOutput::Variable(name) => return Some((name.clone(), output)),
                RunOutput::None => {}
            }
        }
        Step::AskClaude { text, from } => {
            let from = Some(from.as_str());
            let text = fill(text, from)?;
            if text.reveal().trim().is_empty() {
                notice(executor, from, "had nothing to ask Claude");
            } else if let Err(why) = executor.ask_claude(&text) {
                notice(executor, from, &why);
            }
        }
    }
    None
}

/// Whether the program file still holds what was approved; one that can't be read (or is huge) doesn't.
fn unchanged(program: &Path, approved: &str) -> bool {
    let Ok(file) = std::fs::File::open(program) else { return false };
    let mut bytes = Vec::new();
    file.take(MAX_PROGRAM_BYTES + 1).read_to_end(&mut bytes).is_ok() && bytes.len() as u64 <= MAX_PROGRAM_BYTES && contents_hash(&bytes) == approved
}

/// What the program printed, without its last line break; a failure is a notice naming the macro and the
/// reason, never the output or an argument.
fn run_program(command: &Command, approved: Option<&str>, from: Option<&str>, context: &dyn Context, executor: &dyn Executor) -> Option<Private> {
    let program = context.resolve(&command.program);
    if approved.is_some_and(|approved| !unchanged(&program, approved)) {
        let name = program.file_name().unwrap_or_default().to_string_lossy();
        notice(executor, from, &format!("{name} changed since you approved it: approve it again in Settings → Macros"));
        return None;
    }
    let args = command.args.iter().map(|arg| arg.fill(context)).collect::<Result<Vec<_>, _>>().map_err(|why| notice(executor, from, &why)).ok()?;
    let printed = executor.run_command(&program, &args, command.timeout).map_err(|why| notice(executor, from, &why)).ok()?;
    let text = printed.reveal();
    let text = text.strip_suffix('\n').unwrap_or(text);
    Some(Private::new(text.strip_suffix('\r').unwrap_or(text).to_owned()))
}

/// Runs `steps` in order, until the executor says it was cancelled. A step that can't run (a file that can't
/// be read, nothing to paste) is a notice saying why, naming its macro, and the others go on. A program's
/// output kept as a variable is known to the steps after it.
pub fn run(steps: &[Step], context: &dyn Context, executor: &dyn Executor) {
    let mut kept: Vec<(String, Private)> = Vec::new();
    for step in steps {
        if executor.cancelled() {
            break;
        }
        let step = if kept.is_empty() {
            Cow::Borrowed(step)
        } else {
            Cow::Owned(step.bound(&kept.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>()))
        };
        kept.extend(run_step(&step, context, executor));
    }
}

/// A notice that names the macro it is about.
fn notice(executor: &dyn Executor, from: Option<&str>, message: &str) {
    match from {
        Some(from) => executor.notify(&format!("{from}: {message}")),
        None => executor.notify(message),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use chrono::NaiveDateTime;

    use super::*;
    use crate::macros::transcript::{HowMuch, TranscriptLine, Who};

    struct FileContext(&'static str);

    impl Context for FileContext {
        fn variable(&self, _: &str) -> Option<Private> {
            None
        }
        fn now(&self) -> NaiveDateTime {
            NaiveDateTime::default()
        }
        fn clipboard(&self) -> Private {
            Private::new(String::new())
        }
        fn selection(&self) -> Private {
            Private::new(String::new())
        }
        fn app(&self) -> String {
            String::new()
        }
        fn window(&self) -> String {
            String::new()
        }
        fn resolve(&self, path: &Path) -> PathBuf {
            path.to_path_buf()
        }
        fn read_file(&self, _: &Path) -> Result<Private, String> {
            Ok(Private::new(self.0.to_string()))
        }
        fn transcript(&self, _: &Who, _: &HowMuch) -> Vec<TranscriptLine> {
            Vec::new()
        }
        fn apply_rules(&self, text: &str) -> String {
            text.to_string()
        }
        fn pick(&self, _: &[&str]) -> usize {
            0
        }
    }

    #[derive(Default)]
    struct Notices(RefCell<Vec<String>>);

    impl Executor for Notices {
        fn paste_text(&self, _: &Private) {}
        fn paste_files(&self, _: &[PathBuf]) {}
        fn press(&self, _: &[Vec<Key>]) {}
        fn copy(&self, _: Option<&Private>) {}
        fn type_text(&self, _: &Private) {}
        fn wait(&self, _: Duration) {}
        fn notify(&self, message: &str) {
            self.0.borrow_mut().push(message.to_string());
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn open(&self, _: &OpenTarget) -> Result<(), String> {
            Ok(())
        }
        fn switch_to(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn run_command(&self, _: &Path, _: &[Private], _: Duration) -> Result<Private, String> {
            Ok(Private::new(String::new()))
        }
        fn ask_claude(&self, _: &Private) -> Result<(), String> {
            Ok(())
        }
        fn set_mode(&self, _: &Mode) {}
        fn show_palette(&self) {}
    }

    #[test]
    fn a_files_template_error_never_quotes_the_file() {
        let step = Step::PasteFile { path: "notes.txt".into(), raw: false, from: "sign off".into() };
        let executor = Notices::default();
        run(&[step], &FileContext("key {sk_live_abc}"), &executor);
        let notices = executor.0.borrow();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("sign off") && notices[0].contains("notes.txt"), "{notices:?}");
        assert!(!notices[0].contains("sk_live_abc"), "{notices:?}");
    }

    fn run_step_of(program: &Path, contents: &[u8]) -> Step {
        let command = Command::new(&program.to_string_lossy(), &[], vec![], "none", 1000).unwrap();
        Step::Run { command, from: "hello".into(), contents: Some(super::contents_hash(contents)) }
    }

    #[test]
    fn a_program_runs_only_while_it_holds_what_was_approved() {
        let folder = std::env::temp_dir().join(format!("kiku-run-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let program = folder.join("hello.ps1");
        std::fs::write(&program, "echo hi").unwrap();
        let step = run_step_of(&program, b"echo hi");
        let ran = |step: &Step| {
            let executor = Notices::default();
            run_step(step, &FileContext(""), &executor);
            executor.0.take()
        };
        assert!(ran(&step).is_empty(), "unchanged: runs");
        std::fs::write(&program, "echo evil").unwrap();
        let notices = ran(&step);
        assert!(notices.len() == 1 && notices[0].starts_with("hello: hello.ps1 changed since you approved it"), "{notices:?}");
        std::fs::remove_file(&program).unwrap();
        assert_eq!(ran(&step).len(), 1, "unreadable: refused");
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
