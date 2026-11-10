//! What the voice macros tests share (`.loop/voice-macros/SPINE.md`): a fake [`Context`] with a fixed clock,
//! set clipboard, selection, files and transcript, and an [`Executor`] that records what it was asked to do
//! (phase 2's steps too: `.loop/voice-macros-2/SPINE.md`).
#![allow(dead_code)]

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{NaiveDate, NaiveDateTime};
use kiku_core::{
    actions::Private,
    keybinds::Key,
    macros::{read_text_file, run, Context, Executor, HowMuch, Macros, Mode, OpenTarget, Transcript, TranscriptLine, Variables, Who},
    words::{Output, WordSettings, Words},
};

/// Tuesday 10 November 2026, 14:02:05.
pub fn now() -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 11, 10).unwrap().and_hms_opt(14, 2, 5).unwrap()
}

/// A finished line `seconds` before [`now`].
pub fn said(speaker: &str, seconds: i64, text: &str) -> TranscriptLine {
    TranscriptLine { speaker: speaker.into(), time: now() - chrono::Duration::seconds(seconds), text: text.into(), complete: true }
}

pub struct Fake {
    pub variables: Variables,
    pub clipboard: String,
    pub selection: String,
    pub app: String,
    pub window: String,
    /// Where relative paths go.
    pub folder: PathBuf,
    pub transcript: Transcript,
    pub words: Words,
    pub picked: usize,
}

impl Fake {
    /// `variables.toml`'s text, in Discord, nothing said, nothing on the clipboard.
    pub fn new(variables: &str) -> Fake {
        Fake {
            variables: Variables::parse(variables).unwrap(),
            clipboard: String::new(),
            selection: String::new(),
            app: "Discord".into(),
            window: "#general - Discord".into(),
            folder: std::env::temp_dir(),
            transcript: Transcript::default(),
            words: Words::default(),
            picked: 0,
        }
    }
}

impl Context for Fake {
    fn variable(&self, name: &str) -> Option<Private> {
        self.variables.get(name)
    }
    fn now(&self) -> NaiveDateTime {
        now()
    }
    fn clipboard(&self) -> Private {
        Private::new(self.clipboard.clone())
    }
    fn selection(&self) -> Private {
        Private::new(self.selection.clone())
    }
    fn app(&self) -> String {
        self.app.clone()
    }
    fn window(&self) -> String {
        self.window.clone()
    }
    fn resolve(&self, path: &Path) -> PathBuf {
        self.folder.join(path)
    }
    fn read_file(&self, path: &Path) -> Result<Private, String> {
        read_text_file(path)
    }
    fn transcript(&self, who: &Who, how_much: &HowMuch) -> Vec<TranscriptLine> {
        self.transcript.select(who, how_much, &self.app, now())
    }
    fn apply_rules(&self, text: &str) -> String {
        self.words.apply(text, &self.app, Output::Dictation, &WordSettings::default())
    }
    fn pick(&self, _choices: &[&str]) -> usize {
        self.picked
    }
}

/// What a run asked the executor to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Paste(String),
    Files(Vec<PathBuf>),
    Press(Vec<Vec<Key>>),
    Copy(Option<String>),
    Type(String),
    Wait(Duration),
    Notify(String),
    /// What `open` was given: a URL, a path or an app's name, as text.
    Open(String),
    Switch(String),
    /// The program as resolved, and its arguments.
    Run(PathBuf, Vec<String>),
    AskClaude(String),
    SetMode(Mode),
    ShowPalette,
}

pub struct Recorder {
    pub events: RefCell<Vec<Event>>,
    /// Cancelled once this many events were recorded.
    pub cancel_after: Cell<Option<usize>>,
    /// What a program run prints, or why it failed.
    pub output: RefCell<Result<String, String>>,
}

impl Default for Recorder {
    fn default() -> Self {
        Recorder { events: RefCell::default(), cancel_after: Cell::default(), output: RefCell::new(Ok(String::new())) }
    }
}

impl Executor for Recorder {
    fn paste_text(&self, text: &Private) {
        self.events.borrow_mut().push(Event::Paste(text.reveal().to_owned()));
    }
    fn paste_files(&self, files: &[PathBuf]) {
        self.events.borrow_mut().push(Event::Files(files.to_vec()));
    }
    fn press(&self, chords: &[Vec<Key>]) {
        self.events.borrow_mut().push(Event::Press(chords.to_vec()));
    }
    fn copy(&self, text: Option<&Private>) {
        self.events.borrow_mut().push(Event::Copy(text.map(|text| text.reveal().to_owned())));
    }
    fn type_text(&self, text: &Private) {
        self.events.borrow_mut().push(Event::Type(text.reveal().to_owned()));
    }
    fn wait(&self, how_long: Duration) {
        self.events.borrow_mut().push(Event::Wait(how_long));
    }
    fn notify(&self, message: &str) {
        self.events.borrow_mut().push(Event::Notify(message.to_owned()));
    }
    fn cancelled(&self) -> bool {
        self.cancel_after.get().is_some_and(|after| self.events.borrow().len() >= after)
    }
    fn open(&self, target: &OpenTarget) -> Result<(), String> {
        let shown = match target {
            OpenTarget::Url(url) => url.reveal().to_owned(),
            OpenTarget::Path(path) => path.display().to_string(),
            OpenTarget::App(app) => app.clone(),
        };
        self.events.borrow_mut().push(Event::Open(shown));
        Ok(())
    }
    fn switch_to(&self, app: &str) -> Result<(), String> {
        self.events.borrow_mut().push(Event::Switch(app.to_owned()));
        Ok(())
    }
    fn run_command(&self, program: &Path, args: &[Private], _timeout: Duration) -> Result<Private, String> {
        self.events.borrow_mut().push(Event::Run(program.to_owned(), args.iter().map(|arg| arg.reveal().to_owned()).collect()));
        self.output.borrow().clone().map(Private::new)
    }
    fn ask_claude(&self, text: &Private) -> Result<(), String> {
        self.events.borrow_mut().push(Event::AskClaude(text.reveal().to_owned()));
        Ok(())
    }
    fn set_mode(&self, mode: &Mode) {
        self.events.borrow_mut().push(Event::SetMode(mode.clone()));
    }
    fn show_palette(&self) {
        self.events.borrow_mut().push(Event::ShowPalette);
    }
}

/// `macros.toml`'s text with `variables.toml`'s names and no `words.txt`; every macro must load.
pub fn macros(toml: &str, variables: &str) -> Macros {
    let macros = Macros::parse(toml, &Words::default(), &Variables::parse(variables).unwrap());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    macros
}

/// What dictating `lines` in the fake's app does.
pub fn events(macros: &Macros, lines: &[&str], fake: &Fake) -> Vec<Event> {
    events_in(macros, lines, fake, &Mode::Normal, &Recorder::default())
}

/// What dictating `lines` in the fake's app does in `mode`, through `recorder`.
pub fn events_in(macros: &Macros, lines: &[&str], fake: &Fake, mode: &Mode, recorder: &Recorder) -> Vec<Event> {
    run(&macros.plan(lines, &fake.app, &WordSettings::default(), mode), fake, recorder);
    recorder.events.take()
}

pub fn paste(text: &str) -> Event {
    Event::Paste(text.into())
}

pub fn press(chords: &[&[Key]]) -> Event {
    Event::Press(chords.iter().map(|chord| chord.to_vec()).collect())
}

pub fn letter(c: char) -> Key {
    Key::Other(c as u16)
}
