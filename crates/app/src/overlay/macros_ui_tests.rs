//! The Macros tab driven off-screen (`.loop/voice-macros/SPINE.md`, "Frontend checks"), run by `voice-ci`:
//! each click goes to a button the settings window drew, through the handler the window calls, with
//! `macros.toml` in a temporary folder. Phase 2's (`.loop/voice-macros-2/SPINE.md`) are at the end: approving
//! commands, the palette, two-press Delete and the mode chip.

use std::{fs, path::PathBuf};

use windows::Win32::{
    System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
    UI::WindowsAndMessaging::{KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, LLKHF_UP},
};

use kiku_core::{
    captions::Captions,
    keybinds::{Command, Key, Keybinds},
    macros::{approvals_path, run, Approvals, CommandPolicy, Macro, Macros, Mode, StepSpec, Variables},
    words::{WordSettings, Words},
};

use super::{
    keys::{self, KeyEvent},
    macro_tab::{MacroAction, MacroBox, MacroTab, StepKind},
    macros_tests::{Done, Fake, Recorder},
    palette::Palette,
    render::{Panel, Renderer, Surface},
    render_bench::{panel, settings_window},
    view::{Action, Tab, Target, View},
};

/// The settings window, drawn as the user would see it.
struct Window {
    folder: PathBuf,
    view: View,
    renderer: Renderer,
    surface: Surface,
}

impl Window {
    fn new(folder: &std::path::Path) -> Window {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let mut view = View::new();
        view.macros = MacroTab::open(folder);
        Window { folder: folder.to_owned(), view, renderer: Renderer::new().unwrap(), surface: Surface::default() }
    }

    /// What can be clicked now.
    fn targets(&mut self) -> Vec<Target> {
        let (captions, keybinds) = (Captions::default(), Keybinds::default());
        let settings = settings_window(panel(&captions, &self.view, &keybinds, 560.0));
        let (_, regions) = self.renderer.render_settings(&mut self.surface, &settings).unwrap();
        regions.targets.iter().map(|(_, target)| *target).collect()
    }

    /// Whether `action`'s button lies wholly inside the window, where it can be clicked.
    fn inside(&mut self, action: MacroAction) -> bool {
        let (captions, keybinds) = (Captions::default(), Keybinds::default());
        let settings = settings_window(panel(&captions, &self.view, &keybinds, 560.0));
        let (_, regions) = self.renderer.render_settings(&mut self.surface, &settings).unwrap();
        let (width, height) = super::settings::SIZE;
        regions.targets.iter().any(|(at, target)| {
            *target == Target::Click(Action::Macro(action)) && at.left >= 0.0 && at.top >= 0.0 && at.right <= width && at.bottom <= height
        })
    }

    fn shows(&mut self, action: MacroAction) -> bool {
        self.targets().contains(&Target::Click(Action::Macro(action)))
    }

    /// Whether some text on screen holds `part`.
    fn says(&mut self, part: &str) -> bool {
        self.targets();
        self.surface.texts().iter().any(|text| text.contains(part))
    }

    /// Clicks `action`, which must be on screen.
    fn click(&mut self, action: MacroAction) {
        if !self.shows(action) {
            let file = fs::read_to_string(self.folder.join("macros.toml")).unwrap_or_default();
            panic!("{action:?} isn't on screen; drawn: {:?}\nmacros.toml:\n{file}", self.targets());
        }
        self.view.macros.click(action);
    }

    fn type_into(&mut self, into: MacroBox, text: &str) {
        self.click(MacroAction::Focus(into));
        self.view.macros.type_text(text);
    }

    fn open_tab(&mut self) {
        assert!(self.targets().contains(&Target::Click(Action::ShowTab(Tab::Macros))), "the nav bar has a Macros tab");
        self.view.tab = Tab::Macros;
    }
}

fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-macros-ui-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn saved(folder: &std::path::Path) -> (String, Macros) {
    let text = fs::read_to_string(folder.join("macros.toml")).unwrap_or_default();
    let variables = Variables::parse(&fs::read_to_string(folder.join("variables.toml")).unwrap_or_default()).unwrap();
    let macros = Macros::parse(&text, &Words::default(), &variables);
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    (text, macros)
}

#[test]
fn ui_a_macro_is_made_listed_and_deleted_in_the_macros_tab() {
    let dir = folder("make");
    let mut window = Window::new(&dir);
    window.open_tab();

    window.click(MacroAction::New);
    window.type_into(MacroBox::Name, "Paste my email");
    window.type_into(MacroBox::Say(0), "paste my email");
    window.type_into(MacroBox::Apps, "Discord");
    window.click(MacroAction::AddStepMenu);
    for kind in [StepKind::Paste, StepKind::PasteFile, StepKind::Attach, StepKind::Press, StepKind::Copy, StepKind::Type, StepKind::Wait, StepKind::Notify] {
        assert!(window.shows(MacroAction::AddStep(kind)), "Add step offers {kind:?}");
    }
    window.click(MacroAction::AddStep(StepKind::Paste));
    window.type_into(MacroBox::Step(0), "me@example.com");
    window.click(MacroAction::AddStepMenu);
    window.click(MacroAction::AddStep(StepKind::Press));
    window.click(MacroAction::Record(1));
    window.view.macros.record(&[Key::Ctrl, Key::Other('A' as u16)]);
    window.click(MacroAction::Save);

    let (text, macros) = saved(&dir);
    let made = Macro {
        name: "Paste my email".into(),
        say: vec!["paste my email".into()],
        apps: vec!["Discord".into()],
        anywhere: false,
        enabled: true,
        hotkey: None,
        listen: false,
        modes: vec![],
        steps: vec![StepSpec::Paste("me@example.com".into()), StepSpec::Press("Ctrl+A".into())],
    };
    assert_eq!(macros.entries(), [made], "{text}");

    let mut window = Window::new(&dir);
    window.open_tab();
    assert!(window.shows(MacroAction::Edit(0)), "the list shows it");
    assert!(window.shows(MacroAction::Toggle(0)), "with its on/off");
    assert!(!window.shows(MacroAction::Edit(1)));
    window.type_into(MacroBox::Search, "nothing like it");
    assert!(!window.shows(MacroAction::Edit(0)), "the search hides what doesn't match");

    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::Delete(0));
    window.click(MacroAction::Delete(0));
    let (text, macros) = saved(&dir);
    assert!(macros.entries().is_empty());
    assert!(!text.contains("[[macro]]"), "{text}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ui_editing_keeps_the_files_comments() {
    let dir = folder("comments");
    let original = "# mine\n[[macro]]\nname = \"one\"  # first\nsay = \"first one\"\nsteps = [ { paste = \"1\" } ]\n";
    fs::write(dir.join("macros.toml"), original).unwrap();
    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::New);
    window.type_into(MacroBox::Name, "two");
    window.type_into(MacroBox::Say(0), "second one");
    window.click(MacroAction::AddStepMenu);
    window.click(MacroAction::AddStep(StepKind::Paste));
    window.type_into(MacroBox::Step(0), "2");
    window.click(MacroAction::Save);
    let (text, macros) = saved(&dir);
    assert!(text.starts_with(original.trim_end()), "{text}");
    assert_eq!(macros.entries().len(), 2);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ui_lines_survive_editing_and_the_add_step_menu_stays_in_the_window() {
    let dir = folder("lines");
    let original = "[[macro]]\nname = \"Sign off\"\nsay = \"sign off\"\nsteps = [ { paste = \"Thanks,\\n{name}\" }, { wait = 200 }, { notify = \"Signed off\" } ]\n";
    fs::write(dir.join("macros.toml"), original).unwrap();
    fs::write(dir.join("variables.toml"), "name = \"Sam\"\n").unwrap();
    let paste = |macros: &Macros| macros.entries()[0].steps[0].clone();

    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::Edit(0));
    window.type_into(MacroBox::Name, " now");
    window.click(MacroAction::Save);
    let (text, macros) = saved(&dir);
    assert_eq!(macros.entries()[0].name, "Sign off now", "{text}");
    assert_eq!(paste(&macros), StepSpec::Paste("Thanks,\n{name}".into()), "editing another field keeps both lines: {text}");

    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::Edit(0));
    window.type_into(MacroBox::Step(0), "!");
    window.click(MacroAction::Save);
    let (text, macros) = saved(&dir);
    assert_eq!(paste(&macros), StepSpec::Paste("Thanks,\n{name}!".into()), "editing the text itself keeps both lines: {text}");

    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::Edit(0));
    window.click(MacroAction::AddStepMenu);
    for kind in [StepKind::Paste, StepKind::PasteFile, StepKind::Attach, StepKind::Press, StepKind::Copy, StepKind::Type, StepKind::Wait, StepKind::Notify] {
        assert!(window.inside(MacroAction::AddStep(kind)), "Add step's {kind:?} lies inside the window");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ui_the_words_try_box_shows_macros() {
    let macros = Macros::parse(
        "[[macro]]\nname = \"Sign off email\"\nsay = \"sign off\"\nanywhere = true\nsteps = [ { paste = \"Thanks\" } ]\n\n[[macro]]\nname = \"New tab\"\nsay = \"open a tab\"\nsteps = [ { press = \"ctrl+t\" } ]\n",
        &Words::default(),
        &Variables::default(),
    );
    let settings = WordSettings::default();
    assert_eq!(macros.preview("sign off", "", &settings), "[Sign off email]");
    assert_eq!(macros.preview("thanks sign off bye", "", &settings), "thanks [Sign off email] bye");
    assert_eq!(macros.preview("open a tab", "", &settings), "[New tab]");
    assert_eq!(macros.preview("nothing here", "", &settings), "nothing here");
}

const RUN: &str = "[[macro]]\nname = \"Hello\"\nsay = \"say hello\"\nsteps = [ { run = \"hello.bat\", args = [\"{heard}\"], output = \"paste\" } ]\n";

#[test]
fn ui_commands_need_approval_in_the_macros_tab() {
    let dir = folder("approve").join("custom");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("macros.toml"), RUN).unwrap();
    let mut window = Window::new(&dir);
    window.open_tab();
    assert!(window.says("needs approval"), "a command not yet approved is shown as such");
    assert!(window.says("hello.bat"), "with the exact command to approve");
    window.click(MacroAction::Approve(0));
    let approvals = Approvals::parse(&fs::read_to_string(approvals_path(&dir)).expect("approvals are kept beside custom\\"));
    let (_, macros) = saved(&dir);
    assert!(approvals.approves(&macros.commands()[0].1));
    assert!(!window.says("needs approval"));

    fs::write(dir.join("macros.toml"), RUN.replace("[\"{heard}\"]", "[\"{heard}\", \"--all\"]")).unwrap();
    let mut window = Window::new(&dir);
    window.open_tab();
    assert!(window.says("needs approval"), "a changed command needs approval again");

    assert_eq!(window.view.macros.policy(), CommandPolicy::default(), "off until turned on");
    window.click(MacroAction::ToggleRunPrograms);
    assert_eq!(window.view.macros.policy(), CommandPolicy { enabled: true, anywhere: false });
    window.click(MacroAction::ToggleRunAnywhere);
    assert_eq!(window.view.macros.policy(), CommandPolicy { enabled: true, anywhere: true });
    let _ = fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn ui_delete_takes_two_presses() {
    let dir = folder("delete");
    let original = "[[macro]]\nname = \"one\"\nsay = \"first one\"\nsteps = [ { paste = \"1\" } ]\n";
    fs::write(dir.join("macros.toml"), original).unwrap();
    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::Delete(0));
    assert_eq!(saved(&dir).0, original, "one press deletes nothing");
    assert!(window.shows(MacroAction::Delete(0)), "it asks for a second press");
    window.click(MacroAction::Edit(0));
    window.click(MacroAction::Back);
    window.click(MacroAction::Delete(0));
    assert_eq!(saved(&dir).0, original, "another click in between starts again");
    window.click(MacroAction::Delete(0));
    assert!(saved(&dir).1.entries().is_empty(), "the second press deletes");
    let _ = fs::remove_dir_all(dir);
}

/// The panel as drawn with `view`, changed by `with`: what can be clicked and what it says.
fn live(view: &View, with: impl FnOnce(Panel) -> Panel) -> (Vec<Target>, Vec<String>) {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds) = (Captions::default(), Keybinds::default());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let (_, regions) = renderer.render(&mut surface, &with(panel(&captions, view, &keybinds, 560.0))).unwrap();
    (regions.targets.iter().map(|(_, target)| *target).collect(), surface.texts())
}

const PALETTE: &str = "[[macro]]\nname = \"Alpha\"\nsay = [\"alpha one\", \"alpha two\"]\nsteps = [ { paste = \"a\" } ]\n\n[[macro]]\nname = \"Charlie\"\nsay = \"charlie\"\napps = [\"Discord\"]\nsteps = [ { paste = \"c\" } ]\n\n[[macro]]\nname = \"Foxtrot\"\nsay = \"foxtrot\"\nsteps = [ { paste = \"f\" } ]\n";

#[test]
fn ui_palette_opens_lists_and_runs() {
    let mut hook = keys::Context::new(Keybinds::default());
    let mut key = |vk: u16, down: bool| {
        let flags = if down { KBDLLHOOKSTRUCT_FLAGS(0) } else { LLKHF_UP };
        keys::handle(&mut hook, &KBDLLHOOKSTRUCT { vkCode: vk.into(), flags, ..Default::default() })
    };
    key(0xA2, true);
    key(0x5B, true);
    assert!(key(u16::from(b'M'), true).events.contains(&KeyEvent::Command(Command::Palette)), "Ctrl+Win+M opens the palette");

    let macros = Macros::parse(PALETTE, &Words::default(), &Variables::default());
    let mut view = View::new();
    view.palette = Some(Palette::open(&macros, "Notepad", &Mode::Normal));
    let (targets, texts) = live(&view, |panel| panel);
    assert!(targets.contains(&Target::Click(Action::RunFromPalette(0))) && targets.contains(&Target::Click(Action::RunFromPalette(1))), "{targets:?}");
    assert!(!targets.contains(&Target::Click(Action::RunFromPalette(2))), "Charlie runs only in Discord: {targets:?}");
    for shown in ["Alpha", "alpha one", "Foxtrot", "foxtrot"] {
        assert!(texts.iter().any(|text| text.contains(shown)), "{shown}: {texts:?}");
    }
    assert!(!texts.iter().any(|text| text.contains("Charlie")), "{texts:?}");

    let steps = view.palette.as_ref().unwrap().steps(1, &macros).expect("Foxtrot runs");
    let recorder = Recorder::default();
    run(&steps, &Fake, &recorder);
    assert_eq!(recorder.done.take(), [Done::PasteText("f".into())], "a click runs it");
}

#[test]
fn ui_mode_chip_shows_the_mode() {
    let view = View::new();
    let (_, normal) = live(&view, |panel| panel);
    let (_, commands) = live(&view, |panel| Panel { mode: Mode::Commands, ..panel });
    let chip: Vec<&String> = commands.iter().filter(|text| !normal.contains(text)).collect();
    assert!(chip.iter().any(|text| text.to_lowercase().contains("command")), "commands mode shows: {chip:?}");
    let (_, coding) = live(&view, |panel| Panel { mode: Mode::Own("coding".into()), ..panel });
    assert!(coding.iter().any(|text| text.contains("coding")), "a mode of the user's shows its name: {coding:?}");
    assert!(!normal.iter().any(|text| text.contains("coding")), "normal shows none");
}

#[test]
fn ui_a_run_step_takes_its_program_and_each_argument_in_boxes_of_their_own() {
    let dir = folder("run-boxes");
    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::New);
    window.type_into(MacroBox::Name, "Search");
    window.type_into(MacroBox::Say(0), "search for {words}");
    window.click(MacroAction::AddStepMenu);
    window.click(MacroAction::AddStep(StepKind::Run));
    window.type_into(MacroBox::Step(0), "search.ps1");
    assert!(window.shows(MacroAction::Focus(MacroBox::Argument { step: 0, at: 0 })), "a box for the first argument");
    assert!(window.says("one argument per box"), "with a hint saying how arguments are passed");
    window.type_into(MacroBox::Argument { step: 0, at: 0 }, "{words}");
    window.type_into(MacroBox::Argument { step: 0, at: 1 }, "--quiet");
    assert!(window.shows(MacroAction::Focus(MacroBox::Argument { step: 0, at: 2 })), "a blank box always last");
    window.click(MacroAction::Save);
    let (text, macros) = saved(&dir);
    assert!(text.contains("run = \"search.ps1\""), "{text}");
    match &macros.entries()[0].steps[0] {
        StepSpec::Run { program, args, .. } => assert_eq!((program.as_str(), args.as_slice()), ("search.ps1", ["{words}".to_owned(), "--quiet".to_owned()].as_slice()), "{text}"),
        other => panic!("a run step: {other:?}"),
    }
    assert!(!text.contains("\\n"), "no line breaks joined into the program: {text}");

    let mut window = Window::new(&dir);
    window.open_tab();
    window.click(MacroAction::Edit(0));
    window.type_into(MacroBox::Argument { step: 0, at: 1 }, "-x");
    window.click(MacroAction::Save);
    let (text, macros) = saved(&dir);
    assert!(matches!(&macros.entries()[0].steps[0], StepSpec::Run { args, .. } if args == &["{words}", "--quiet-x"]), "editing keeps each argument its own: {text}");
    let _ = fs::remove_dir_all(dir);
}
