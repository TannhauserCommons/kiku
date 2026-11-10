//! Voice macros' triggers on Windows (unit M8, `.loop/voice-macros/SPINE.md`, "Triggers"), run by `voice-ci`:
//! the keyboard hook catching a macro's shortcut, a line said without the key running a listening macro, and
//! the Macros tab's Trigger rows writing the file.

use std::{fs, path::PathBuf};

use windows::Win32::{
    System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
    UI::WindowsAndMessaging::{KBDLLHOOKSTRUCT, KBDLLHOOKSTRUCT_FLAGS, LLKHF_INJECTED, LLKHF_UP},
};

use kiku_core::{
    actions::key_strokes,
    captions::Captions,
    controls::Controls,
    keybinds::{Hotkey, Key, Keybinds},
    macros::{MacroBook, Macros, Step, Variables},
    settings::Settings,
    words::Words,
};

use super::{
    heard_steps,
    keys::{self, KeyEvent, Outcome},
    macro_tab::{MacroAction, MacroBox, MacroTab, StepKind},
    render::{Renderer, Surface},
    render_bench::{panel, settings_window},
    view::{Action, Tab, Target, View},
};

const CTRL: u16 = 0xA2;
const ALT: u16 = 0xA4;
const SHIFT: u16 = 0xA0;
const WIN: u16 = 0x5B;
const M: u16 = 0x4D;
const ONE: u16 = 0x31;
const F2: u16 = 0x71;
const RIGHT: Key = Key::Other(0x27);

/// The hook's state, with `shortcuts` as the macros' (in that order) and the default captions key, Ctrl+Win.
struct Hook(keys::Context);

impl Hook {
    fn with(shortcuts: &[&str]) -> Hook {
        let mut context = keys::Context::new(Keybinds::default());
        context.set_macro_hotkeys(shortcuts.iter().map(|shortcut| Hotkey::parse(shortcut).unwrap()).collect());
        Hook(context)
    }

    /// A key typed on the keyboard.
    fn key(&mut self, vk: u16, down: bool) -> Outcome {
        self.sent(vk, down, KBDLLHOOKSTRUCT_FLAGS(0), 0)
    }

    /// A key some program sent, with `flags` and `dwExtraInfo` as it set them.
    fn sent(&mut self, vk: u16, down: bool, flags: KBDLLHOOKSTRUCT_FLAGS, extra: usize) -> Outcome {
        let flags = if down { flags } else { flags | LLKHF_UP };
        keys::handle(&mut self.0, &KBDLLHOOKSTRUCT { vkCode: vk.into(), flags, dwExtraInfo: extra, ..Default::default() })
    }

    /// Presses `vks` in order, then lets them go in reverse; what each did, downs then ups.
    fn chord(&mut self, vks: &[u16]) -> Vec<Outcome> {
        let downs: Vec<Outcome> = vks.iter().map(|&vk| self.key(vk, true)).collect();
        downs.into_iter().chain(vks.iter().rev().map(|&vk| self.key(vk, false)).collect::<Vec<_>>()).collect()
    }
}

/// The macro a key-down ran, by its place.
fn ran_macro(outcome: &Outcome) -> Option<usize> {
    outcome.events.iter().find_map(|event| if let KeyEvent::Macro { index, .. } = event { Some(*index) } else { None })
}

fn generation(outcome: &Outcome) -> Option<u32> {
    outcome.events.iter().find_map(|event| if let KeyEvent::Macro { generation, .. } = event { Some(*generation) } else { None })
}

#[test]
fn w8_hook_runs_a_macro_shortcut() {
    let mut hook = Hook::with(&["Ctrl+Win+F2", "Ctrl+Alt+M"]);
    assert_eq!(hook.key(CTRL, true), Outcome::default(), "modifiers pass through");
    assert_eq!(hook.key(ALT, true), Outcome::default());
    let m = hook.key(M, true);
    assert!(m.swallow, "{m:?}");
    assert_eq!(ran_macro(&m), Some(1), "{m:?}");
    assert_eq!(m.events.len(), 1, "{m:?}");
    let up = hook.key(M, false);
    assert!(up.swallow && up.events.is_empty(), "its key-up is swallowed too: {up:?}");
    hook.key(ALT, false);
    hook.key(CTRL, false);

    let typed = Hook::with(&["Ctrl+Alt+M"]).chord(&[CTRL, M]);
    assert!(typed.iter().all(|outcome| !outcome.swallow && ran_macro(outcome).is_none()), "Ctrl+M is the app's: {typed:?}");
    let more = Hook::with(&["Ctrl+Alt+M"]).chord(&[CTRL, ALT, SHIFT, M]);
    assert!(more.iter().all(|outcome| !outcome.swallow && ran_macro(outcome).is_none()), "Ctrl+Alt+Shift+M isn't Ctrl+Alt+M: {more:?}");

    let mut hook = Hook::with(&["Ctrl+Win+1"]);
    hook.key(CTRL, true);
    assert_eq!(hook.key(WIN, true).events, [KeyEvent::Held], "the captions key is held as before");
    let one = hook.key(ONE, true);
    assert!(one.swallow && ran_macro(&one) == Some(0), "{one:?}");
    assert!(!one.events.contains(&KeyEvent::Other), "a macro, not another key: {one:?}");
    assert!(hook.key(ONE, false).swallow);

    let mut hook = Hook::with(&["Ctrl+Alt+M"]);
    let first = generation(&hook.chord(&[CTRL, ALT, M])[2]);
    hook.0.set_macro_hotkeys(vec![Hotkey::parse("Ctrl+Alt+M").unwrap()]);
    let second = generation(&hook.chord(&[CTRL, ALT, M])[2]);
    assert!(first.is_some() && second.is_some() && first != second, "new shortcuts, a new generation: {first:?} {second:?}");

    let mut hook = Hook::with(&["Ctrl+Alt+M"]);
    hook.0.set_capturing(true);
    for vk in [CTRL, ALT, M] {
        assert_eq!(hook.key(vk, true), Outcome { swallow: true, events: vec![KeyEvent::Captured { vk, down: true }], mask: false }, "{vk:#x}");
    }
}

#[test]
fn w8_alt_and_win_shortcuts_mask_the_menu() {
    let mut hook = Hook::with(&["Ctrl+Alt+M", "Win+F2"]);
    let alt = hook.chord(&[CTRL, ALT, M]);
    assert!(alt[4].mask, "letting go of Alt after Ctrl+Alt+M: {alt:?}");
    assert!(!alt[5].mask, "Ctrl needs no mask");
    let win = hook.chord(&[WIN, F2]);
    assert_eq!(ran_macro(&win[1]), Some(1));
    assert!(win[3].mask, "letting go of Win after Win+F2: {win:?}");
    let tapped = hook.chord(&[ALT]);
    assert!(tapped.iter().all(|outcome| !outcome.mask), "Alt alone is left to Windows: {tapped:?}");
}

#[test]
fn w8_only_kikus_own_keys_are_ignored() {
    let sent = keys::inputs(&key_strokes(&[Key::Ctrl, Key::Other(M)]));
    let tag = unsafe { sent[0].Anonymous.ki.dwExtraInfo };
    assert_ne!(tag, 0, "Kiku tags the keys it sends");
    assert!(sent.iter().all(|input| unsafe { input.Anonymous.ki.dwExtraInfo } == tag));

    let mut hook = Hook::with(&["Ctrl+Alt+M"]);
    let ours: Vec<Outcome> = [CTRL, ALT, M].iter().map(|&vk| hook.sent(vk, true, LLKHF_INJECTED, tag)).collect();
    assert!(ours.iter().all(|outcome| *outcome == Outcome::default()), "Kiku's own keys: {ours:?}");
    for vk in [M, ALT, CTRL] {
        hook.sent(vk, false, LLKHF_INJECTED, tag);
    }
    let theirs: Vec<Outcome> = [CTRL, ALT, M].iter().map(|&vk| hook.sent(vk, true, LLKHF_INJECTED, 0)).collect();
    assert_eq!(ran_macro(&theirs[2]), Some(0), "keys another program sent count as typed: {theirs:?}");
    assert!(theirs[2].swallow);
}

/// A folder of its own under the temporary folder, emptied.
fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-triggers-{name}-{}", std::process::id()));
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

#[test]
fn w8_a_heard_line_runs_a_listen_macro() {
    let controls = controls_over(
        "heard",
        "[[macro]]\nname = \"next slide\"\nsay = \"next slide\"\nlisten = true\nsteps = [ { press = \"right\" } ]\n",
    );
    let right = [Step::Press(vec![vec![RIGHT]])];
    controls.queue_mic_line("Next slide.".into(), false);
    controls.queue_mic_line("what a nice day".into(), false);
    assert_eq!(heard_steps(&controls, "PowerPoint", ""), right);
    assert!(heard_steps(&controls, "PowerPoint", "").is_empty(), "each line runs once");

    controls.queue_mic_line("next slide".into(), true);
    assert!(heard_steps(&controls, "PowerPoint", "").is_empty(), "a line said holding the key is dictation's");
    assert_eq!(controls.take_typing(), ["next slide"], "and is typed as dictation");

    controls.queue_mic_line("next slide".into(), false);
    controls.queue_mic_line("Computer, next slide.".into(), false);
    assert_eq!(heard_steps(&controls, "PowerPoint", "computer"), right, "with a wake word, only the line that starts with it");

    let quiet = controls_over("quiet", "[[macro]]\nname = \"next slide\"\nsay = \"next slide\"\nsteps = [ { press = \"right\" } ]\n");
    quiet.queue_mic_line("next slide".into(), false);
    assert!(quiet.take_heard().is_empty(), "no macro listens, so nothing waits");
}

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

    fn targets(&mut self) -> Vec<Target> {
        let (captions, keybinds) = (Captions::default(), Keybinds::default());
        let settings = settings_window(panel(&captions, &self.view, &keybinds, 560.0));
        let (_, regions) = self.renderer.render_settings(&mut self.surface, &settings).unwrap();
        regions.targets.iter().map(|(_, target)| *target).collect()
    }

    /// What the screen says now.
    fn texts(&mut self) -> Vec<String> {
        self.targets();
        self.surface.texts()
    }

    fn says(&mut self, part: &str) -> bool {
        self.texts().iter().any(|text| text.contains(part))
    }

    fn shows(&mut self, action: MacroAction) -> bool {
        self.targets().contains(&Target::Click(Action::Macro(action)))
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

    /// The file's text and its macros, every one of which loads.
    fn saved(&self) -> (String, Macros) {
        let text = fs::read_to_string(self.folder.join("macros.toml")).unwrap_or_default();
        let macros = Macros::parse(&text, &Words::default(), &Variables::default());
        assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
        (text, macros)
    }
}

#[test]
fn ui_trigger_rows_edit_the_file() {
    let dir = folder("rows");
    fs::write(dir.join("macros.toml"), "# slides\n[[macro]]\nname = \"Next\"\nsay = \"next slide\"\nsteps = [ { press = \"right\" } ]\n").unwrap();
    let mut window = Window::new(&dir);
    assert!(window.targets().contains(&Target::Click(Action::ShowTab(Tab::Macros))));
    window.view.tab = Tab::Macros;
    window.view.macros.set_settings(&Keybinds::default(), false, "");

    window.click(MacroAction::Edit(0));
    window.click(MacroAction::RecordHotkey);
    window.view.macros.record(&[Key::Ctrl, Key::Alt, Key::Other(M)]);
    window.click(MacroAction::Save);
    let (text, macros) = window.saved();
    assert!(text.starts_with("# slides\n") && text.contains("hotkey = \"Ctrl+Alt+M\""), "{text}");
    assert_eq!(macros.entries()[0].hotkey, Some(Hotkey::parse("Ctrl+Alt+M").unwrap()));
    assert!(window.says("Ctrl") && window.says("Alt"), "the list shows the shortcut: {:?}", window.texts());

    window.click(MacroAction::Edit(0));
    window.click(MacroAction::RecordHotkey);
    window.view.macros.record(&[Key::Ctrl, Key::Win, Key::Other('V' as u16)]);
    window.click(MacroAction::Save);
    let why = window.view.macros.problem().expect("Ctrl+Win+V is Kiku's paste: refused, saying why").to_owned();
    assert!(why.contains("Ctrl+Win+V"), "the reason names the shortcut: {why}");
    assert!(window.texts().contains(&why), "the reason is drawn: {:?}", window.texts());
    assert_eq!(window.view.macros.editor().and_then(|editor| editor.hotkey.as_deref()), Some("Ctrl+Alt+M"), "the refused shortcut isn't shown as set");
    assert_eq!(window.saved().0, text, "and nothing is written");

    window.click(MacroAction::ClearHotkey);
    window.click(MacroAction::Save);
    let (text, macros) = window.saved();
    assert!(!text.contains("hotkey"), "{text}");
    assert_eq!(macros.entries()[0].hotkey, None);

    window.click(MacroAction::Edit(0));
    window.click(MacroAction::ToggleListen);
    window.click(MacroAction::Save);
    let (text, macros) = window.saved();
    assert!(text.contains("listen = true"), "{text}");
    assert!(macros.entries()[0].listen);
    assert!(window.says("no key needed"), "the list says a listening macro needs no key: {:?}", window.texts());

    window.type_into(MacroBox::Wake, "computer");
    assert_eq!(window.view.macros.wake(), "computer", "the wake word is set on the list, for every macro; the window saves it as macros.wake");
    window.click(MacroAction::Edit(0));
    assert!(!window.shows(MacroAction::Focus(MacroBox::Wake)), "the editor has no wake word box of its own");
    assert!(window.says("wake word"), "the editor says a wake word is in use: {:?}", window.texts());
    assert!(window.view.macros.listen_note().is_none(), "nothing to say while the mic is heard without the key");
    window.view.macros.set_settings(&Keybinds::default(), true, "computer");
    assert!(window.view.macros.listen_note().is_some(), "dictation only mode: listening macros can't run, and the editor says so");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn ui_a_shortcut_alone_makes_a_macro() {
    let dir = folder("steps");
    let mut window = Window::new(&dir);
    window.view.tab = Tab::Macros;
    window.view.macros.set_settings(&Keybinds::default(), false, "");
    window.click(MacroAction::New);
    window.type_into(MacroBox::Name, "Keyed");
    window.click(MacroAction::RecordHotkey);
    window.view.macros.record(&[Key::Ctrl, Key::Alt, Key::Other('K' as u16)]);
    window.click(MacroAction::AddStepMenu);
    window.click(MacroAction::AddStep(StepKind::Paste));
    window.type_into(MacroBox::Step(0), "keyed text");
    window.click(MacroAction::Save);
    let (text, macros) = window.saved();
    let made = &macros.entries()[0];
    assert!(made.say.is_empty() && made.hotkey == Some(Hotkey::parse("Ctrl+Alt+K").unwrap()), "a shortcut is enough to save a macro: {text}");
    let _ = fs::remove_dir_all(dir);
}
