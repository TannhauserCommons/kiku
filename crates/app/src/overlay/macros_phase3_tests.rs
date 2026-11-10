//! Voice macros, phase 3, on Windows (`.loop/voice-macros-3/SPINE.md`), run by `voice-ci`: command keys and
//! `kiku` steps through one dispatch, the toggle key and the release delay keeping `Controls::dictating`, a
//! shortcut macro toggling dictation, and the settings and editor rows and the Dictating badge as drawn.

use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Mutex},
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::HWND,
    System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
};

use kiku_core::{
    captions::Captions,
    controls::Controls,
    dictation::{DictationKey, DictationSettings, RELEASE_DELAYS},
    keybinds::{Command, Keybinds},
    macros::{run_step, KikuAction, KikuRequest, KikuStep, MacroBook, Macros, Mode, Named, SettingsTab, Step, Variables},
    settings::Settings,
    words::{WordSettings, Words},
};

use super::{
    dictation_card::{DictationAction, DictationCard},
    dispatch::Dispatch,
    keys::KeyEvent,
    macro_run::MacroRun,
    macro_tab::{MacroAction, MacroBox, MacroTab, StepKind},
    render::{Panel, Renderer, Surface},
    render_bench::{panel, settings_window},
    view::{Action, Tab, Target, View},
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-phase3-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

/// Controls whose macros are `macros.toml` in a folder of their own.
fn controls_over(name: &str, toml: &str) -> Controls {
    let dir = folder(name);
    fs::write(dir.join("macros.toml"), toml).unwrap();
    let mut controls = Controls::new(-50.0, -50.0, Settings::default().choice());
    controls.macros = MacroBook::at(dir);
    controls.macros.refresh(&controls.words.words());
    controls
}

/// What running `steps` through the app's executor asks the panel to perform.
fn asked(controls: &Controls, steps: &[Step]) -> Vec<KikuRequest> {
    let (captions, lent) = (Mutex::new(Captions::default()), RefCell::new(None));
    steps
        .iter()
        .flat_map(|step| {
            let run = MacroRun::new(HWND::default(), controls, &captions, &lent);
            run_step(step, &run, &run);
            run.finish().actions
        })
        .collect()
}

fn dispatch(key: DictationKey, delay: u64) -> Dispatch {
    Dispatch::new(&DictationSettings { key, release_delay: ms(delay), stop: vec![] })
}

fn dictating(controls: &Controls) -> bool {
    controls.dictating.load(Ordering::Relaxed)
}

#[test]
fn w_keys_and_steps_go_through_one_dispatch() {
    let controls = controls_over("dispatch", "");
    let mut keys = dispatch(DictationKey::Hold, 0);
    let now = Instant::now();
    for command in Command::ALL.into_iter().filter(|command| *command != Command::Quit) {
        let action = keys.key(KeyEvent::Command(command), now, &controls).unwrap_or_else(|| panic!("{command:?} asks for an action"));
        assert_eq!(Some(action), command.action(), "{command:?}");
        let request = KikuRequest::new(action);
        assert_eq!(asked(&controls, &[Step::Kiku(KikuStep::new(action))]), std::slice::from_ref(&request), "{action:?} as a step asks the same");
        assert_eq!(keys.perform(request.clone(), now, &controls), Some(request), "{action:?} goes on to the panel either way");
    }
    assert_eq!(keys.key(KeyEvent::Command(Command::Quit), now, &controls), None, "quit stays a key");

    for (action, on) in [(KikuAction::StartDictation, true), (KikuAction::StopDictation, false), (KikuAction::ToggleDictation, true), (KikuAction::ToggleDictation, false)] {
        assert_eq!(keys.perform(KikuRequest::new(action), now, &controls), None, "{action:?} is the dispatch's own");
        assert_eq!(dictating(&controls), on, "{action:?}");
    }
}

#[test]
fn w_toggle_key_sets_controls_dictating() {
    let controls = controls_over("toggle", "");
    let mut keys = dispatch(DictationKey::Toggle, 0);
    let t = Instant::now();
    keys.key(KeyEvent::Held, t, &controls);
    assert!(!dictating(&controls), "not while the key is down");
    keys.key(KeyEvent::Released, t + ms(60), &controls);
    assert!(dictating(&controls) && keys.latched(), "a bare press turns it on");

    keys.key(KeyEvent::Held, t + ms(1000), &controls);
    keys.key(KeyEvent::Other, t + ms(1020), &controls);
    keys.key(KeyEvent::Released, t + ms(1060), &controls);
    assert!(dictating(&controls), "a chord leaves it on");
    keys.key(KeyEvent::Held, t + ms(1500), &controls);
    assert_eq!(keys.key(KeyEvent::Command(Command::Lock), t + ms(1520), &controls), Some(KikuAction::ToggleLock));
    keys.key(KeyEvent::Released, t + ms(1560), &controls);
    assert!(dictating(&controls), "so does a command key");

    keys.key(KeyEvent::Held, t + ms(2000), &controls);
    keys.key(KeyEvent::Released, t + ms(2050), &controls);
    assert!(!dictating(&controls) && !keys.latched(), "the next bare press turns it off");

    keys.key(KeyEvent::Held, t + ms(3000), &controls);
    keys.key(KeyEvent::Released, t + ms(3050), &controls);
    keys.key(KeyEvent::Held, t + ms(4000), &controls);
    keys.key(KeyEvent::Command(Command::Stop), t + ms(4020), &controls);
    assert!(!dictating(&controls), "stopping captions ends it");
}

#[test]
fn w_release_delay_runs_on_the_panels_timer() {
    let controls = controls_over("delay", "");
    let mut keys = dispatch(DictationKey::Hold, 500);
    let t = Instant::now();
    keys.key(KeyEvent::Held, t, &controls);
    assert_eq!(keys.next_wake(), Some(t + ms(250)), "the timer is due when holding starts dictating");
    keys.tick(t + ms(250), &controls);
    assert!(dictating(&controls));
    keys.key(KeyEvent::Released, t + ms(1000), &controls);
    assert!(dictating(&controls), "still listening once let go");
    assert_eq!(keys.next_wake(), Some(t + ms(1500)));
    keys.tick(t + ms(1499), &controls);
    assert!(dictating(&controls));
    keys.tick(t + ms(1500), &controls);
    assert!(!dictating(&controls), "over when the timer fires");
    assert_eq!(keys.next_wake(), None);
}

#[test]
fn w_hotkey_macro_toggles_dictation() {
    let controls = controls_over("hotkey", "[[macro]]\nname = \"dictate\"\nhotkey = \"ctrl+alt+d\"\nsteps = [ { kiku = \"toggle dictation\" } ]\n");
    let macros = controls.macros.macros();
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    let (handed, refused) = macros.hotkeys(&Keybinds::default());
    assert_eq!((handed.len(), refused.len()), (1, 0));
    let mut keys = dispatch(DictationKey::Hold, 0);
    for on in [true, false, true] {
        let steps = macros.plan_macro("dictate", "Notepad", &Mode::Normal).unwrap();
        for request in asked(&controls, &steps) {
            assert_eq!(keys.perform(request, Instant::now(), &controls), None, "dictation's own");
        }
        assert_eq!(dictating(&controls), on);
        assert_eq!(keys.latched(), on, "a step's dictation stays on until told");
    }
}

/// A window drawn as the user would see it, with what can be clicked and what it says.
struct Drawn {
    targets: Vec<Target>,
    texts: Vec<String>,
}

fn draw(view: &View, settings: bool, with: impl FnOnce(Panel) -> Panel) -> Drawn {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let (captions, keybinds) = (Captions::default(), Keybinds::default());
    let (mut renderer, mut surface) = (Renderer::new().unwrap(), Surface::default());
    let drawn = with(panel(&captions, view, &keybinds, 560.0));
    let (_, regions) = if settings { renderer.render_settings(&mut surface, &settings_window(drawn)) } else { renderer.render(&mut surface, &drawn) }.unwrap();
    Drawn { targets: regions.targets.iter().map(|(_, target)| *target).collect(), texts: surface.texts() }
}

#[test]
fn ui_dictation_card_edits_the_settings() {
    let mut view = View::new();
    view.tab = Tab::General;
    view.dictation = DictationCard::open(&DictationSettings::default());
    let card = |action| Target::Click(Action::Dictation(action));
    let drawn = draw(&view, true, |panel| panel);
    for action in [DictationAction::Key(DictationKey::Hold), DictationAction::Key(DictationKey::Toggle), DictationAction::FocusStop] {
        assert!(drawn.targets.contains(&card(action)), "{action:?}: {:?}", drawn.targets);
    }
    for delay in RELEASE_DELAYS {
        assert!(drawn.targets.contains(&card(DictationAction::Delay(delay))), "keep listening {delay} ms");
    }
    assert!(drawn.texts.iter().any(|text| text.contains("that's all Kiku")), "the stop phrase shows: {:?}", drawn.texts);

    view.dictation.click(DictationAction::Key(DictationKey::Toggle));
    view.dictation.click(DictationAction::Delay(750));
    view.dictation.click(DictationAction::FocusStop);
    view.dictation.type_text(", over and out");
    let dictation = view.dictation.settings();
    assert_eq!(dictation, DictationSettings { key: DictationKey::Toggle, release_delay: ms(750), stop: vec!["that's all Kiku".into(), "over and out".into()] });
    let text = Settings { dictation, ..Settings::default() }.to_text();
    for line in ["dictation.key = toggle", "dictation.release_delay = 750", "dictation.stop = that's all Kiku, over and out"] {
        assert!(text.lines().any(|written| written == line), "{line} saved in:\n{text}");
    }

    let mut setup = View::new();
    setup.setup = Some(super::view::SetupStep::Keys);
    let drawn = draw(&setup, true, |panel| panel);
    assert!(drawn.targets.contains(&card(DictationAction::Key(DictationKey::Toggle))), "setup's Keys step offers Hold / Toggle: {:?}", drawn.targets);
}

/// The Macros tab over `macros.toml` in `folder`, clicked only where it drew a button.
struct Editor {
    view: View,
}

impl Editor {
    fn new(folder: &Path) -> Editor {
        let mut view = View::new();
        view.tab = Tab::Macros;
        view.macros = MacroTab::open(folder);
        Editor { view }
    }

    fn click(&mut self, action: MacroAction) {
        let drawn = draw(&self.view, true, |panel| panel);
        assert!(drawn.targets.contains(&Target::Click(Action::Macro(action))), "{action:?} isn't on screen: {:?}", drawn.targets);
        self.view.macros.click(action);
    }

    fn type_into(&mut self, into: MacroBox, text: &str) {
        self.click(MacroAction::Focus(into));
        self.view.macros.type_text(text);
    }
}

#[test]
fn ui_kiku_step_picker_writes_the_file() {
    let dir = folder("picker");
    let mut editor = Editor::new(&dir);
    editor.click(MacroAction::New);
    editor.type_into(MacroBox::Name, "Words tab");
    editor.type_into(MacroBox::Say(0), "show my words");
    editor.click(MacroAction::AddStepMenu);
    editor.click(MacroAction::AddStep(StepKind::Kiku));
    editor.click(MacroAction::KikuPicker(0));
    let drawn = draw(&editor.view, true, |panel| panel);
    for action in KikuAction::ALL {
        assert!(drawn.targets.contains(&Target::Click(Action::Macro(MacroAction::PickKiku { step: 0, action }))), "the picker offers {action:?}");
    }
    editor.click(MacroAction::PickKiku { step: 0, action: KikuAction::OpenSettings });
    editor.type_into(MacroBox::Step(0), "words");
    editor.click(MacroAction::Save);

    let text = fs::read_to_string(dir.join("macros.toml")).unwrap();
    assert!(text.contains("{ kiku = \"open settings\", tab = \"words\" }"), "{text}");
    let macros = Macros::parse(&text, &Words::default(), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    let mut expected = KikuStep::new(KikuAction::OpenSettings);
    expected.tab = Some(Named::Known(SettingsTab::Words));
    assert_eq!(macros.plan(&["show my words"], "Notepad", &WordSettings::default(), &Mode::Normal), [Step::Kiku(expected)]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ui_badge_shows_while_latched() {
    let view = View::new();
    let badge = |drawn: &Drawn| drawn.texts.iter().any(|text| text.contains("Dictating"));
    assert!(badge(&draw(&view, false, |panel| Panel { dictating: true, latched: true, ..panel })), "latched: the badge shows");
    assert!(!badge(&draw(&view, false, |panel| Panel { dictating: true, latched: false, ..panel })), "held: no badge");
    assert!(!badge(&draw(&view, false, |panel| panel)), "not dictating: none");
}
