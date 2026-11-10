//! Voice macros' Windows tests (`.loop/voice-macros/SPINE.md`), run by `voice-ci`: running a plan through a
//! recording executor, the clipboard block that holds files, and the keys a sequence sends. What lands in
//! another app can't be checked headless; that is the real-desktop check. Phase 2's tests
//! (`macros_phase2_tests.rs`) share its context and recording executor.

use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    time::Duration,
};

use kiku_core::{
    actions::{sequence_strokes, Private},
    keybinds::Key,
    macros::{run, Context, Executor, HowMuch, Macros, Mode, NaiveDateTime, OpenTarget, TranscriptLine, Variables, Who},
    words::{WordSettings, Words},
};
use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_KEYBOARD, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP};

use super::{clipboard, keys};

const MACROS: &str = r#"[[macro]]
name = "sign off"
say = "sign off"
anywhere = true
steps = [ { paste = "Thanks,\n{name}" } ]

[[macro]]
name = "send"
say = "send it"
steps = [ { press = "ctrl+k, ctrl+c" }, { wait = 100 }, { press = "down ×2" }, { notify = "sent" } ]
"#;

pub(super) struct Fake;

impl Context for Fake {
    fn variable(&self, name: &str) -> Option<Private> {
        (name == "name").then(|| Private::new("Sam".into()))
    }
    fn now(&self) -> NaiveDateTime {
        "2026-11-10T14:02:05".parse().unwrap()
    }
    fn clipboard(&self) -> Private {
        Private::new(String::new())
    }
    fn selection(&self) -> Private {
        Private::new(String::new())
    }
    fn app(&self) -> String {
        "Notepad".into()
    }
    fn window(&self) -> String {
        "Untitled - Notepad".into()
    }
    fn resolve(&self, path: &Path) -> PathBuf {
        path.to_owned()
    }
    fn read_file(&self, _path: &Path) -> Result<Private, String> {
        Err("no files here".into())
    }
    fn transcript(&self, _who: &Who, _how_much: &HowMuch) -> Vec<TranscriptLine> {
        Vec::new()
    }
    fn apply_rules(&self, text: &str) -> String {
        text.to_owned()
    }
    fn pick(&self, _choices: &[&str]) -> usize {
        0
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Done {
    PasteText(String),
    PasteFiles(Vec<PathBuf>),
    Press(Vec<Vec<Key>>),
    Copy,
    TypeText(String),
    Wait(Duration),
    Notify(String),
    Open(String),
    Switch(String),
    Run(PathBuf, Vec<String>),
    AskClaude(String),
    SetMode(Mode),
    ShowPalette,
}

#[derive(Default)]
pub(super) struct Recorder {
    pub(super) done: RefCell<Vec<Done>>,
    cancel_after: Cell<Option<usize>>,
}

impl Executor for Recorder {
    fn paste_text(&self, text: &Private) {
        self.done.borrow_mut().push(Done::PasteText(text.reveal().into()));
    }
    fn paste_files(&self, files: &[PathBuf]) {
        self.done.borrow_mut().push(Done::PasteFiles(files.to_vec()));
    }
    fn press(&self, chords: &[Vec<Key>]) {
        self.done.borrow_mut().push(Done::Press(chords.to_vec()));
    }
    fn copy(&self, _text: Option<&Private>) {
        self.done.borrow_mut().push(Done::Copy);
    }
    fn type_text(&self, text: &Private) {
        self.done.borrow_mut().push(Done::TypeText(text.reveal().into()));
    }
    fn wait(&self, how_long: Duration) {
        self.done.borrow_mut().push(Done::Wait(how_long));
    }
    fn notify(&self, message: &str) {
        self.done.borrow_mut().push(Done::Notify(message.into()));
    }
    fn cancelled(&self) -> bool {
        self.cancel_after.get().is_some_and(|after| self.done.borrow().len() >= after)
    }
    fn open(&self, target: &OpenTarget) -> Result<(), String> {
        let shown = match target {
            OpenTarget::Url(url) => url.reveal().to_owned(),
            OpenTarget::Path(path) => path.display().to_string(),
            OpenTarget::App(app) => app.clone(),
        };
        self.done.borrow_mut().push(Done::Open(shown));
        Ok(())
    }
    fn switch_to(&self, app: &str) -> Result<(), String> {
        self.done.borrow_mut().push(Done::Switch(app.into()));
        Ok(())
    }
    fn run_command(&self, program: &Path, args: &[Private], _timeout: Duration) -> Result<Private, String> {
        self.done.borrow_mut().push(Done::Run(program.to_owned(), args.iter().map(|arg| arg.reveal().to_owned()).collect()));
        Ok(Private::new(String::new()))
    }
    fn ask_claude(&self, text: &Private) -> Result<(), String> {
        self.done.borrow_mut().push(Done::AskClaude(text.reveal().into()));
        Ok(())
    }
    fn set_mode(&self, mode: &Mode) {
        self.done.borrow_mut().push(Done::SetMode(mode.clone()));
    }
    fn show_palette(&self) {
        self.done.borrow_mut().push(Done::ShowPalette);
    }
}

fn macros() -> Macros {
    let macros = Macros::parse(MACROS, &Words::default(), &Variables::parse("name = \"Sam\"\n").unwrap());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    macros
}

fn ran(said: &[&str], recorder: &Recorder) -> Vec<Done> {
    run(&macros().plan(said, "Notepad", &WordSettings::default(), &Mode::Normal), &Fake, recorder);
    recorder.done.take()
}

fn letter(c: char) -> Key {
    Key::Other(c as u16)
}

/// Each input as (virtual key, up, extended).
fn sent(inputs: &[INPUT]) -> Vec<(u16, bool, bool)> {
    inputs
        .iter()
        .map(|input| {
            assert_eq!(input.r#type, INPUT_KEYBOARD);
            let key = unsafe { input.Anonymous.ki };
            (key.wVk.0, key.dwFlags.contains(KEYEVENTF_KEYUP), key.dwFlags.contains(KEYEVENTF_EXTENDEDKEY))
        })
        .collect()
}

#[test]
fn t5_runner_pastes_text_once_per_run() {
    let done = ran(&["hello", "sign off", "see you"], &Recorder::default());
    assert_eq!(done, [Done::PasteText("hello Thanks,\nSam see you ".into())]);
    let long: Vec<String> = (0..200).map(|n| format!("line number {n} of a long dictation")).collect();
    let long: Vec<&str> = long.iter().map(String::as_str).collect();
    let done = ran(&long, &Recorder::default());
    assert_eq!(done.len(), 1, "{} steps", done.len());
    assert!(matches!(&done[0], Done::PasteText(_)));
}

#[test]
fn t5_files_go_on_the_clipboard_as_files() {
    let block = clipboard::files(&[PathBuf::from(r"D:\a\one.png"), PathBuf::from(r"D:\b\twö.pdf")]);
    let header: Vec<u32> = block[..20].as_chunks::<4>().0.iter().map(|&bytes| u32::from_le_bytes(bytes)).collect();
    assert_eq!(header, [20, 0, 0, 0, 1], "DROPFILES: pFiles, pt.x, pt.y, fNC, fWide");
    let paths: Vec<u8> = r"D:\a\one.png".encode_utf16().chain([0]).chain(r"D:\b\twö.pdf".encode_utf16()).chain([0, 0]).flat_map(u16::to_le_bytes).collect();
    assert_eq!(&block[20..], &paths[..]);
}

#[test]
fn t5_runner_runs_steps_in_order_and_stops_on_cancel() {
    let ctrl = |c| vec![Key::Ctrl, letter(c)];
    let expected = [
        Done::PasteText("one ".into()),
        Done::Press(vec![ctrl('K'), ctrl('C')]),
        Done::Wait(Duration::from_millis(100)),
        Done::Press(vec![vec![Key::Other(0x28)], vec![Key::Other(0x28)]]),
        Done::Notify("sent".into()),
        Done::PasteText("two ".into()),
    ];
    assert_eq!(ran(&["one", "send it", "two"], &Recorder::default()), expected);
    let recorder = Recorder::default();
    recorder.cancel_after.set(Some(3));
    assert_eq!(ran(&["one", "send it", "two"], &recorder), expected[..3]);
}

#[test]
fn t5_key_sequences_send_in_order() {
    let ctrl = |c| vec![Key::Ctrl, letter(c)];
    let inputs = keys::inputs(&sequence_strokes(&[ctrl('K'), ctrl('C')]));
    let (k, c) = (0x4B, 0x43);
    assert_eq!(sent(&inputs), [(0xA2, false, false), (k, false, false), (k, true, false), (0xA2, true, false), (0xA2, false, false), (c, false, false), (c, true, false), (0xA2, true, false)]);
    let plan = macros().plan(&["send it"], "Notepad", &WordSettings::default(), &Mode::Normal);
    let chords = plan.iter().filter_map(|step| if let kiku_core::macros::Step::Press(chords) = step { Some(chords) } else { None }).nth(1).unwrap();
    let down = (0x28, false, true);
    assert_eq!(sent(&keys::inputs(&sequence_strokes(chords))), [down, (0x28, true, true), down, (0x28, true, true)]);
}
