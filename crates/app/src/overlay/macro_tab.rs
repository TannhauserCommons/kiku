//! The settings window's Macros tab: the macros of `custom\macros.toml` listed with a search and on/off, and
//! an editor with the phrases, the step list and an Add step menu, saved keeping the file's comments.

use std::{fs, io::ErrorKind, path::{Path, PathBuf}};

use windows::Win32::UI::Input::KeyboardAndMouse::{VIRTUAL_KEY, VK_ESCAPE};

use kiku_core::{
    actions::{escape, unescape},
    keybinds::{Hotkey, Key, Keybinds},
    macros::{approvals_path, contents_hash, Approvals, BuiltinPhrases, Command, CommandPolicy, KikuAction, Macro, Macros, StepSpec, Variables},
    transcripts,
    words::Words,
};

use super::field::Field;

/// Something to click in the Macros tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroAction {
    /// Opens the editor on a new macro.
    New,
    /// Opens the editor on the macro listed at this place.
    Edit(usize),
    /// Turns the macro listed at this place on or off.
    Toggle(usize),
    Delete(usize),
    /// Sends typing to a box.
    Focus(MacroBox),
    AddStepMenu,
    AddStep(StepKind),
    RemoveStep(usize),
    /// Records the shortcut of the press step at this place.
    Record(usize),
    /// Records the macro's shortcut.
    RecordHotkey,
    /// Takes the macro's shortcut away.
    ClearHotkey,
    /// Turns on or off running the macro when its phrase is said without the dictation key.
    ToggleListen,
    /// Writes the macro being edited to the file.
    Save,
    /// Leaves the editor without saving.
    Back,
    /// Approves the command listed at this place among those needing approval.
    Approve(usize),
    /// Settings → Macros' "Let macros run programs".
    ToggleRunPrograms,
    /// "Let macros run programs from anywhere".
    ToggleRunAnywhere,
    /// Turns a built-in phrase on or off.
    TogglePhrase(Phrase),
    /// Changes what the run step at this place does with its output.
    CycleOutput(usize),
    /// Opens or closes the action picker of the kiku step at this place.
    KikuPicker(usize),
    /// Picks the action of the kiku step at `step`.
    PickKiku { step: usize, action: KikuAction },
}

/// A built-in phrase Settings can turn off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phrase {
    CommandMode,
    NormalMode,
    DictationOff,
    DictationOn,
    WhatCanISay,
}

impl Phrase {
    pub const ALL: [Phrase; 5] = [Phrase::CommandMode, Phrase::NormalMode, Phrase::DictationOff, Phrase::DictationOn, Phrase::WhatCanISay];

    /// What is said, and what it does.
    pub fn label(self) -> &'static str {
        match self {
            Phrase::CommandMode => "\"command mode\": only macros run",
            Phrase::NormalMode => "\"normal mode\": dictate again",
            Phrase::DictationOff => "\"dictation off\": hear only these phrases",
            Phrase::DictationOn => "\"dictation on\": dictate again",
            Phrase::WhatCanISay => "\"what can I say\": show the macro palette",
        }
    }

    pub fn is_on(self, phrases: &BuiltinPhrases) -> bool {
        match self {
            Phrase::CommandMode => phrases.command_mode,
            Phrase::NormalMode => phrases.normal_mode,
            Phrase::DictationOff => phrases.dictation_off,
            Phrase::DictationOn => phrases.dictation_on,
            Phrase::WhatCanISay => phrases.what_can_i_say,
        }
    }

    fn flip(self, phrases: &mut BuiltinPhrases) {
        let on = !self.is_on(phrases);
        match self {
            Phrase::CommandMode => phrases.command_mode = on,
            Phrase::NormalMode => phrases.normal_mode = on,
            Phrase::DictationOff => phrases.dictation_off = on,
            Phrase::DictationOn => phrases.dictation_on = on,
            Phrase::WhatCanISay => phrases.what_can_i_say = on,
        }
    }
}

/// What a shortcut being recorded is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recording {
    /// The press step at this place.
    Step(usize),
    /// The macro's own shortcut.
    Hotkey,
}

/// A box in the Macros tab that takes typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroBox {
    Search,
    Name,
    /// A phrase, by its place.
    Say(usize),
    /// A step's text, by its place.
    Step(usize),
    /// The apps it runs in, apart by commas; empty is everywhere.
    Apps,
    /// The wake word listening macros wait for (`macros.wake` in settings); empty is none.
    Wake,
    /// One argument of the run step at `step`, by its place.
    Argument { step: usize, at: usize },
}

/// The Add step menu's choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Paste,
    PasteFile,
    Attach,
    Press,
    Copy,
    Type,
    Wait,
    Notify,
    Open,
    Switch,
    Run,
    AskClaude,
    Mode,
    /// One of Kiku's own actions.
    Kiku,
}

impl StepKind {
    pub const ALL: [StepKind; 14] = [
        StepKind::Paste,
        StepKind::PasteFile,
        StepKind::Attach,
        StepKind::Press,
        StepKind::Copy,
        StepKind::Type,
        StepKind::Wait,
        StepKind::Notify,
        StepKind::Open,
        StepKind::Switch,
        StepKind::Run,
        StepKind::AskClaude,
        StepKind::Mode,
        StepKind::Kiku,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StepKind::Paste => "Paste text",
            StepKind::PasteFile => "Paste a file's text",
            StepKind::Attach => "Attach files",
            StepKind::Press => "Press keys",
            StepKind::Copy => "Copy",
            StepKind::Type => "Type text",
            StepKind::Wait => "Wait",
            StepKind::Notify => "Show a notice",
            StepKind::Open => "Open a link, file or app",
            StepKind::Switch => "Switch to an app",
            StepKind::Run => "Run a program",
            StepKind::AskClaude => "Ask Claude",
            StepKind::Mode => "Change mode",
            StepKind::Kiku => "Kiku action",
        }
    }

    /// What the step's box asks for while it is empty.
    pub fn placeholder(self) -> &'static str {
        match self {
            StepKind::Paste => "Text, e.g. Thanks, {name}",
            StepKind::PasteFile => "Path of a text file",
            StepKind::Attach => "Paths of files, apart by |",
            StepKind::Press => "Shortcut, e.g. Ctrl+Shift+T",
            StepKind::Copy => "Text (empty copies the selection)",
            StepKind::Type => "Text typed key by key",
            StepKind::Wait => "Milliseconds",
            StepKind::Notify => "Message for the panel",
            StepKind::Open => "Link, path or app name",
            StepKind::Switch => "App name",
            StepKind::Run => "Program, e.g. search.ps1",
            StepKind::AskClaude => "What to ask, e.g. Summarise {transcript:Discord:10m|names}",
            StepKind::Mode => "Mode name, e.g. commands",
            StepKind::Kiku => "", // depends on the action: see `EditedStep::placeholder`
        }
    }
}

impl StepKind {
    /// Whether the step's text is shown with line breaks and backslashes escaped (a path or a shortcut has none to show).
    fn escaped(self) -> bool {
        matches!(self, StepKind::Paste | StepKind::Copy | StepKind::Type | StepKind::Notify | StepKind::AskClaude)
    }
}

/// What a run step does with what the program prints, which a button cycles through.
const OUTPUTS: [&str; 3] = ["none", "paste", "notify"];
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// A step being edited: its text as typed.
pub struct EditedStep {
    pub kind: StepKind,
    pub text: Field,
    /// A run step's arguments, always ending in a blank one to type the next into.
    pub args: Vec<Field>,
    /// A kiku step's action as the file writes it: a name, or a template the editor leaves alone. Its `text`
    /// is open settings' `tab` or open history's `search`.
    action: String,
    /// The `tab` and `search` of a templated action, whose box is not shown, kept as the file has them.
    kept_options: (Option<String>, Option<String>),
    /// A pasted file's `raw`, which the editor keeps as the file has it.
    raw: bool,
    /// A run step's output and timeout; the timeout stays as the file has it.
    output: String,
    timeout_ms: u64,
}

impl EditedStep {
    fn new(kind: StepKind, text: &str, raw: bool) -> EditedStep {
        let shown = if kind.escaped() { escape(text) } else { text.to_owned() };
        let mut step = EditedStep { kind, text: text_field(&shown), args: Vec::new(), action: String::new(), kept_options: (None, None), raw, output: OUTPUTS[0].to_owned(), timeout_ms: DEFAULT_TIMEOUT_MS };
        step.keep_a_blank_argument();
        step
    }

    /// A new step of `kind`; a kiku step starts on the first action.
    fn blank(kind: StepKind) -> EditedStep {
        let mut step = EditedStep::new(kind, "", false);
        if kind == StepKind::Kiku {
            step.action = KikuAction::ALL[0].name().to_owned();
        }
        step
    }

    fn keep_a_blank_argument(&mut self) {
        if self.kind == StepKind::Run {
            keep_a_blank(&mut self.args);
        }
    }

    /// The kiku step's action as the picker's button shows it: its label, or a template as written.
    pub fn action_label(&self) -> &str {
        KikuAction::parse(&self.action).map_or(self.action.as_str(), |action| action.label())
    }

    /// What the step's box asks for while it is empty; none when it has no box (a kiku step whose action
    /// takes no text).
    pub fn placeholder(&self) -> Option<&'static str> {
        if self.kind != StepKind::Kiku {
            return Some(self.kind.placeholder());
        }
        match KikuAction::parse(&self.action) {
            Ok(KikuAction::OpenSettings) => Some("Tab, e.g. words (empty just opens settings)"),
            Ok(KikuAction::OpenHistory) => Some("Words to search for (empty opens History)"),
            _ => None,
        }
    }

    fn pick(&mut self, action: KikuAction) {
        if self.action != action.name() {
            self.text.clear();
        }
        self.action = action.name().to_owned();
        self.kept_options = (None, None);
    }

    /// What a run step does with its output, as the file writes it.
    pub fn output(&self) -> &str {
        &self.output
    }

    fn cycle_output(&mut self) {
        let next = OUTPUTS.iter().position(|output| *output == self.output).map_or(0, |at| (at + 1) % OUTPUTS.len());
        self.output = OUTPUTS[next].to_owned();
    }

    fn of(spec: &StepSpec) -> EditedStep {
        match spec {
            StepSpec::Paste(text) => EditedStep::new(StepKind::Paste, text, false),
            StepSpec::PasteFile { path, raw } => EditedStep::new(StepKind::PasteFile, path, *raw),
            StepSpec::Attach(paths) => EditedStep::new(StepKind::Attach, &paths.join(" | "), false),
            StepSpec::Press(keys) => EditedStep::new(StepKind::Press, keys, false),
            StepSpec::Copy(text) => EditedStep::new(StepKind::Copy, text.as_deref().unwrap_or_default(), false),
            StepSpec::Type(text) => EditedStep::new(StepKind::Type, text, false),
            StepSpec::Wait(milliseconds) => EditedStep::new(StepKind::Wait, &milliseconds.to_string(), false),
            StepSpec::Notify(text) => EditedStep::new(StepKind::Notify, text, false),
            StepSpec::Open(text) => EditedStep::new(StepKind::Open, text, false),
            StepSpec::Switch(text) => EditedStep::new(StepKind::Switch, text, false),
            StepSpec::Run { program, args, output, timeout_ms } => {
                let mut step = EditedStep { output: output.clone(), timeout_ms: *timeout_ms, ..EditedStep::new(StepKind::Run, program, false) };
                step.args = args.iter().map(|arg| text_field(arg)).collect();
                step.keep_a_blank_argument();
                step
            }
            StepSpec::AskClaude(text) => EditedStep::new(StepKind::AskClaude, text, false),
            StepSpec::Mode(name) => EditedStep::new(StepKind::Mode, name, false),
            StepSpec::Kiku { action, tab, search } => {
                let mut step = EditedStep { action: action.clone(), ..EditedStep::new(StepKind::Kiku, tab.as_ref().or(search.as_ref()).map_or("", String::as_str), false) };
                if KikuAction::parse(action).is_err() {
                    step.kept_options = (tab.clone(), search.clone());
                }
                step
            }
        }
    }

    fn spec(&self) -> Result<StepSpec, String> {
        let text = if self.kind.escaped() { unescape(&self.text.text) } else { self.text.text.clone() };
        Ok(match self.kind {
            StepKind::Paste => StepSpec::Paste(text),
            StepKind::PasteFile => StepSpec::PasteFile { path: text, raw: self.raw },
            StepKind::Attach => StepSpec::Attach(text.split('|').map(str::trim).filter(|path| !path.is_empty()).map(str::to_owned).collect()),
            StepKind::Press => StepSpec::Press(text),
            StepKind::Copy => StepSpec::Copy(Some(text).filter(|text| !text.trim().is_empty())),
            StepKind::Type => StepSpec::Type(text),
            StepKind::Wait => StepSpec::Wait(text.trim().parse().map_err(|_| "A wait takes a whole number of milliseconds")?),
            StepKind::Notify => StepSpec::Notify(text),
            StepKind::Open => StepSpec::Open(text),
            StepKind::Switch => StepSpec::Switch(text),
            StepKind::Run => {
                let program = text.trim().to_owned();
                if program.is_empty() {
                    return Err("A run step needs a program".into());
                }
                let args = self.args.iter().map(|arg| arg.text.clone()).filter(|arg| !arg.is_empty()).collect();
                StepSpec::Run { program, args, output: self.output.clone(), timeout_ms: self.timeout_ms }
            }
            StepKind::AskClaude => StepSpec::AskClaude(text),
            StepKind::Mode => StepSpec::Mode(text.trim().to_owned()),
            StepKind::Kiku => self.kiku_spec(),
        })
    }
}

impl EditedStep {
    fn kiku_spec(&self) -> StepSpec {
        let typed = Some(self.text.text.trim().to_owned()).filter(|text| !text.is_empty());
        let (tab, search) = match KikuAction::parse(&self.action) {
            Ok(KikuAction::OpenSettings) => (typed, None),
            Ok(KikuAction::OpenHistory) => (None, typed),
            _ => self.kept_options.clone(),
        };
        StepSpec::Kiku { action: self.action.clone(), tab, search }
    }
}

/// The macro being edited. Whether it may be said mid-sentence stays as the file has it.
pub struct Editor {
    pub name: Field,
    /// The phrases, always ending in a blank one to type the next into.
    pub say: Vec<Field>,
    pub steps: Vec<EditedStep>,
    /// The apps it runs in, apart by commas.
    pub apps: Field,
    /// Its shortcut as written ("Ctrl+Alt+M"), if it has one.
    pub hotkey: Option<String>,
    /// Whether saying a phrase without the dictation key runs it.
    pub listen: bool,
    /// The macro as it is in the file, by name; none while it is new.
    replacing: Option<Macro>,
}

impl Editor {
    fn new(replacing: Option<&Macro>) -> Editor {
        let mut editor = Editor {
            name: text_field(replacing.map_or("", |known| &known.name)),
            say: replacing.map_or_else(Vec::new, |known| known.say.iter().map(|phrase| text_field(phrase)).collect()),
            steps: replacing.map_or_else(Vec::new, |known| known.steps.iter().map(EditedStep::of).collect()),
            apps: text_field(&replacing.map_or_else(String::new, |known| known.apps.join(", "))),
            hotkey: replacing.and_then(|known| known.hotkey.as_ref().map(Hotkey::name)),
            listen: replacing.is_some_and(|known| known.listen),
            replacing: replacing.cloned(),
        };
        editor.keep_a_blank_phrase();
        editor
    }

    fn keep_a_blank_phrase(&mut self) {
        keep_a_blank(&mut self.say);
        self.steps.iter_mut().for_each(EditedStep::keep_a_blank_argument);
    }

    fn field(&mut self, which: MacroBox) -> Option<&mut Field> {
        match which {
            MacroBox::Name => Some(&mut self.name),
            MacroBox::Say(at) => self.say.get_mut(at),
            MacroBox::Step(at) => self.steps.get_mut(at).map(|step| &mut step.text),
            MacroBox::Apps => Some(&mut self.apps),
            MacroBox::Search | MacroBox::Wake => None,
            MacroBox::Argument { step, at } => self.steps.get_mut(step).and_then(|step| step.args.get_mut(at)),
        }
    }

    /// The macro as typed.
    fn build(&self) -> Result<Macro, String> {
        let name = self.name.text.trim().to_owned();
        let say: Vec<String> = self.say.iter().map(|phrase| phrase.text.trim().to_owned()).filter(|phrase| !phrase.is_empty()).collect();
        let hotkey = self.hotkey.as_deref().map(Hotkey::parse).transpose()?;
        if name.is_empty() || (say.is_empty() && hotkey.is_none()) {
            return Err("A macro needs a name, and something to say or a shortcut".into());
        }
        let steps = self.steps.iter().map(EditedStep::spec).collect::<Result<_, _>>()?;
        let apps = self.apps.text.split(',').map(str::trim).filter(|app| !app.is_empty()).map(str::to_owned).collect();
        let (anywhere, enabled) = self.replacing.as_ref().map_or((false, true), |known| (known.anywhere, known.enabled));
        let modes = self.replacing.as_ref().map_or_else(Vec::new, |known| known.modes.clone());
        Ok(Macro { name, say, apps, anywhere, enabled, hotkey, listen: self.listen, modes, steps })
    }
}

/// Makes the last of `fields` a blank one, to type the next into.
fn keep_a_blank(fields: &mut Vec<Field>) {
    if fields.last().is_none_or(|last| !last.text.is_empty()) {
        fields.push(Field::default());
    }
}

fn text_field(text: &str) -> Field {
    let mut field = Field::default();
    field.set(text);
    field
}

/// The macros the folder's files have now.
fn read_macros(folder: &Path) -> Macros {
    let read = |name: &str| fs::read_to_string(folder.join(name)).unwrap_or_default();
    let variables = Variables::parse(&read("variables.toml")).unwrap_or_default();
    Macros::parse(&read("macros.toml"), &Words::default(), &variables)
}

/// The Macros tab's state.
#[derive(Default)]
pub struct MacroTab {
    folder: PathBuf,
    macros: Macros,
    search: Field,
    editor: Option<Editor>,
    focus: Option<MacroBox>,
    menu: bool,
    /// The kiku step whose action picker is open.
    picker: Option<usize>,
    /// What is waiting for a shortcut.
    recording: Option<Recording>,
    /// Kiku's own keys, which a macro's shortcut can't be.
    keybinds: Keybinds,
    /// Kiku hears the mic only while dictating, so listening macros can't run.
    dictation_only: bool,
    /// The words that must start a line for a listening macro to run.
    wake: Field,
    /// Why the last change to the file was refused.
    problem: Option<String>,
    /// Whether `problem` is about the shortcut, which the editor shows under the Shortcut row.
    shortcut_refused: bool,
    /// Whether macros may run programs; the window saves it as `macros.run` and `macros.run_anywhere`.
    policy: CommandPolicy,
    /// The built-in phrases that are on; the window saves them as `macros.phrases`.
    phrases: BuiltinPhrases,
    /// The macro whose Delete was pressed once, which a second press deletes.
    armed: Option<usize>,
    /// The commands of the macros that the approvals kept beside the folder don't cover (or no longer
    /// cover, as the program changed), with their macro's name.
    unapproved: Vec<(String, Command)>,
    /// Approved commands whose program file isn't there, so every run is refused.
    missing: Vec<(String, Command)>,
}

impl MacroTab {
    /// The tab over `folder`'s (`custom\`) `macros.toml`.
    pub fn open(folder: &Path) -> MacroTab {
        let mut tab = MacroTab { folder: folder.to_owned(), macros: read_macros(folder), ..MacroTab::default() };
        tab.find_unapproved();
        tab
    }

    /// What the tab needs of the app's settings: Kiku's own keys (a macro's shortcut can't clash with them),
    /// whether Kiku is in dictation only mode (listening macros can't run then) and the wake word.
    pub fn set_settings(&mut self, keybinds: &Keybinds, dictation_only: bool, wake: &str) {
        self.keybinds = keybinds.clone();
        self.dictation_only = dictation_only;
        if self.wake.text != wake {
            self.wake.set(wake);
        }
    }

    /// Whether macros may run programs, from settings.
    pub fn set_policy(&mut self, policy: CommandPolicy) {
        self.policy = policy;
    }

    /// Which built-in phrases are on, from settings.
    pub fn set_phrases(&mut self, phrases: BuiltinPhrases) {
        self.phrases = phrases;
    }

    pub fn phrases(&self) -> BuiltinPhrases {
        self.phrases
    }

    /// The commands waiting for the user's approval, each with the name of the macro that runs it.
    pub fn unapproved(&self) -> &[(String, Command)] {
        &self.unapproved
    }

    /// The approved commands whose program isn't found, each with the name of the macro that runs it.
    pub fn missing_programs(&self) -> &[(String, Command)] {
        &self.missing
    }

    /// The macro whose Delete asks for a second press.
    pub fn armed(&self) -> Option<usize> {
        self.armed
    }

    /// Whether macros may run programs, as the switches have it.
    pub fn policy(&self) -> CommandPolicy {
        self.policy
    }

    /// The wake word as typed.
    pub fn wake(&self) -> &str {
        &self.wake.text
    }

    pub fn wake_field(&self) -> &Field {
        &self.wake
    }

    /// The refusal of the macro's shortcut, if that was the last refusal.
    pub fn shortcut_problem(&self) -> Option<&str> {
        self.problem().filter(|_| self.shortcut_refused)
    }

    /// Beside the Listen switch: why listening macros won't run now (dictation only mode), if they won't.
    pub fn listen_note(&self) -> Option<&'static str> {
        self.dictation_only.then_some("In dictation only mode Kiku doesn't hear your mic without the key, so listening macros don't run.")
    }

    /// The macros matching the search by name or phrase, with their places in the file.
    pub fn listed(&self) -> Vec<(usize, &Macro)> {
        let needle = self.search.text.trim().to_lowercase();
        let matches = |known: &Macro| needle.is_empty() || std::iter::once(&known.name).chain(&known.say).any(|text| text.to_lowercase().contains(&needle));
        self.macros.entries().iter().enumerate().filter(|(_, known)| matches(known)).collect()
    }

    /// The macros the file has that couldn't be read: where, and why.
    pub fn skipped(&self) -> &[(String, String)] {
        &self.macros.skipped
    }

    pub fn search(&self) -> &Field {
        &self.search
    }

    pub fn editor(&self) -> Option<&Editor> {
        self.editor.as_ref()
    }

    pub fn focus(&self) -> Option<MacroBox> {
        self.focus
    }

    pub fn menu_open(&self) -> bool {
        self.menu
    }

    pub fn picker(&self) -> Option<usize> {
        self.picker
    }

    pub fn recording(&self) -> Option<Recording> {
        self.recording
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Whether typing goes to a box.
    pub fn typing(&self) -> bool {
        self.focus.is_some()
    }

    pub fn stop_typing(&mut self) {
        self.focus = None;
    }

    pub fn cancel_recording(&mut self) {
        self.recording = None;
    }

    /// What a click on `action` does.
    pub fn click(&mut self, action: MacroAction) {
        let menu_was_open = std::mem::take(&mut self.menu);
        let picker_was = self.picker.take();
        let armed = self.armed.take();
        self.problem = None;
        self.shortcut_refused = false;
        if !matches!(action, MacroAction::Record(_) | MacroAction::RecordHotkey) {
            self.recording = None;
        }
        match action {
            MacroAction::New => self.edit(None),
            MacroAction::Edit(at) => self.edit(Some(at)),
            MacroAction::Toggle(at) => self.toggle(at),
            MacroAction::Delete(at) if armed == Some(at) => self.delete(at),
            MacroAction::Delete(at) => self.armed = Some(at),
            MacroAction::Focus(which) => self.focus = Some(which),
            MacroAction::AddStepMenu => self.menu = !menu_was_open,
            MacroAction::AddStep(kind) => self.add_step(kind),
            MacroAction::RemoveStep(at) => {
                if let Some(editor) = self.editor.as_mut().filter(|editor| at < editor.steps.len()) {
                    editor.steps.remove(at);
                    self.focus = None;
                }
            }
            MacroAction::Record(at) => self.recording = Some(Recording::Step(at)),
            MacroAction::RecordHotkey => self.recording = Some(Recording::Hotkey),
            MacroAction::ClearHotkey => self.edit_with(|editor| editor.hotkey = None),
            MacroAction::ToggleListen => self.edit_with(|editor| editor.listen = !editor.listen),
            MacroAction::Save => self.save(),
            MacroAction::Back => self.edit_nothing(),
            MacroAction::Approve(at) => self.approve(at),
            MacroAction::ToggleRunPrograms => self.policy.enabled = !self.policy.enabled,
            MacroAction::ToggleRunAnywhere => self.policy.anywhere = !self.policy.anywhere,
            MacroAction::TogglePhrase(phrase) => phrase.flip(&mut self.phrases),
            MacroAction::CycleOutput(at) => self.edit_with(|editor| {
                if let Some(step) = editor.steps.get_mut(at) {
                    step.cycle_output();
                }
            }),
            MacroAction::KikuPicker(at) => self.picker = Some(at).filter(|at| picker_was != Some(*at)),
            MacroAction::PickKiku { step, action } => {
                // The step's box may go with the old action, so typing mustn't go to it unseen.
                self.focus = None;
                self.edit_with(|editor| {
                    if let Some(step) = editor.steps.get_mut(step) {
                        step.pick(action);
                    }
                });
            }
        }
    }

    /// Text typed into the focused box; the window sends units (see [`MacroTab::type_unit`]), the tests text.
    #[cfg(test)]
    pub fn type_text(&mut self, text: &str) {
        self.edit_focused(|field| field.insert(text));
    }

    /// A UTF-16 unit typed into the focused box, as Windows sends them.
    pub fn type_unit(&mut self, unit: u16) {
        self.edit_focused(|field| field.type_unit(unit));
    }

    /// A key pressed in the focused box: an editing key edits it, Escape leaves it.
    pub fn key(&mut self, key: VIRTUAL_KEY, ctrl: bool, paste: impl FnOnce() -> Option<String>) {
        let mut edited = false;
        self.edit_focused(|field| edited = field.edit(key, ctrl, paste));
        if !edited && key == VK_ESCAPE {
            self.focus = None;
        }
    }

    /// The shortcut pressed while one was recording.
    pub fn record(&mut self, keys: &[Key]) {
        let Some(recording) = self.recording.take() else { return };
        let shortcut = keys.iter().map(|key| key.name()).collect::<Vec<_>>().join("+");
        let Some(editor) = self.editor.as_mut() else { return };
        match recording {
            Recording::Step(at) => {
                if let Some(step) = editor.steps.get_mut(at) {
                    step.text.set(&shortcut);
                }
            }
            Recording::Hotkey => editor.hotkey = Some(shortcut),
        }
    }

    fn edit_focused(&mut self, edit: impl FnOnce(&mut Field)) {
        let field = match (self.focus, self.editor.as_mut()) {
            (Some(MacroBox::Search), _) => Some(&mut self.search),
            (Some(MacroBox::Wake), _) => Some(&mut self.wake),
            (Some(which), Some(editor)) => editor.field(which),
            _ => None,
        };
        if let Some(field) = field {
            edit(field);
        }
        if let Some(editor) = &mut self.editor {
            editor.keep_a_blank_phrase();
        }
    }

    fn edit(&mut self, at: Option<usize>) {
        self.editor = Some(Editor::new(at.and_then(|at| self.macros.entries().get(at))));
        self.focus = None;
    }

    fn edit_nothing(&mut self) {
        self.editor = None;
        self.focus = None;
    }

    fn edit_with(&mut self, change: impl FnOnce(&mut Editor)) {
        if let Some(editor) = &mut self.editor {
            change(editor);
        }
    }

    fn add_step(&mut self, kind: StepKind) {
        if let Some(editor) = &mut self.editor {
            editor.steps.push(EditedStep::blank(kind));
            self.focus = (kind != StepKind::Kiku).then_some(MacroBox::Step(editor.steps.len() - 1));
        }
    }

    fn save(&mut self) {
        let Some(editor) = &self.editor else { return };
        let built = editor.build();
        if let Some(why) = built.as_ref().ok().and_then(|new| self.shortcut_clash(new)) {
            return self.refuse_shortcut(why);
        }
        let saved = built.and_then(|new| match editor.replacing.as_ref().map(|old| old.name.clone()) {
            Some(old) => self.change_file(|macros| macros.replace(&old, &new)),
            None => self.change_file(|macros| macros.add(&new)),
        });
        if self.keep(saved) {
            self.edit_nothing();
        }
    }

    /// Why `new`'s shortcut can't be, if it clashes with Kiku's own keys.
    fn shortcut_clash(&self, new: &Macro) -> Option<String> {
        new.hotkey.as_ref().and_then(|hotkey| self.keybinds.clash(hotkey))
    }

    /// Says why the shortcut was refused, and shows the one the file has again.
    fn refuse_shortcut(&mut self, why: String) {
        if let Some(editor) = &mut self.editor {
            editor.hotkey = editor.replacing.as_ref().and_then(|known| known.hotkey.as_ref().map(Hotkey::name));
        }
        self.problem = Some(why);
        self.shortcut_refused = true;
    }

    fn toggle(&mut self, at: usize) {
        let Some(name) = self.macros.entries().get(at).map(|known| known.name.clone()) else { return };
        let changed = self.change_file(|macros| {
            let known = macros.entries().iter().find(|known| known.name == name).ok_or("There is no macro with that name")?;
            let flipped = Macro { enabled: !known.enabled, ..known.clone() };
            macros.replace(&name, &flipped)
        });
        self.keep(changed);
    }

    fn delete(&mut self, at: usize) {
        let Some(name) = self.macros.entries().get(at).map(|known| known.name.clone()) else { return };
        let changed = self.change_file(|macros| if macros.remove(&name) { Ok(()) } else { Err("There is no macro with that name".into()) });
        self.keep(changed);
    }

    /// Approves the command listed at this place with its program file as it is now, so the program changing
    /// asks again. A file that isn't there is approved as empty.
    fn approve(&mut self, at: usize) {
        let Some((_, command)) = self.unapproved.get(at) else { return };
        let approved = self.program_bytes(command).and_then(|bytes| {
            let mut approvals = self.read_approvals();
            approvals.add_with_contents(command, &bytes);
            let path = approvals_path(&self.folder);
            let written = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| transcripts::replace(&path, &approvals.to_text()));
            written.map_err(|why| format!("Couldn't write approved-commands.txt: {why}"))
        });
        match approved {
            Ok(()) => self.find_unapproved(),
            Err(why) => self.problem = Some(why),
        }
    }

    fn read_approvals(&self) -> Approvals {
        Approvals::parse(&fs::read_to_string(approvals_path(&self.folder)).unwrap_or_default())
    }

    /// What the command's program file holds; a missing one reads as empty.
    fn program_bytes(&self, command: &Command) -> Result<Vec<u8>, String> {
        match fs::read(self.folder.join(&command.program)) {
            Err(why) if why.kind() == ErrorKind::NotFound => Ok(Vec::new()),
            read => read.map_err(|why| format!("Couldn't read {}: {why}", command.program.display())),
        }
    }

    /// Works out which commands need approval: those the approvals don't hold, or hold for a program file
    /// that has changed since.
    fn find_unapproved(&mut self) {
        let approvals = self.read_approvals();
        let covered = |command: &Command| {
            let current = self.program_bytes(command).ok().map(|bytes| contents_hash(&bytes));
            current.is_some() && approvals.contents(command) == current.as_deref()
        };
        let (unapproved, approved): (Vec<_>, Vec<_>) = self.macros.commands().into_iter().partition(|(_, command)| !covered(command));
        self.missing = approved.into_iter().filter(|(_, command)| !self.folder.join(&command.program).exists()).collect();
        self.unapproved = unapproved;
    }

    /// The file as it is now with `change` made, written back; refused, nothing is written.
    fn change_file(&self, change: impl FnOnce(&mut Macros) -> Result<(), String>) -> Result<Macros, String> {
        let mut next = read_macros(&self.folder);
        change(&mut next)?;
        let path = self.folder.join("macros.toml");
        fs::create_dir_all(&self.folder).and_then(|()| transcripts::replace(&path, &next.to_text())).map_err(|why| format!("Couldn't write macros.toml: {why}"))?;
        Ok(next)
    }

    /// Keeps what was written, or why it couldn't be; returns whether it is kept.
    fn keep(&mut self, written: Result<Macros, String>) -> bool {
        match written {
            Ok(next) => {
                self.macros = next;
                self.find_unapproved();
                true
            }
            Err(why) => {
                self.problem = Some(why);
                false
            }
        }
    }
}
