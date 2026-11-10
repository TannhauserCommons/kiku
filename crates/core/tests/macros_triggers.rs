//! Voice macros' triggers (unit M8, `.loop/voice-macros/SPINE.md`, "Triggers"): a macro run by its shortcut,
//! or by saying it with no key held (`listen = true`, after the wake word if one is set).

mod common;

use common::{events, letter, macros, paste, press, Event, Fake, Recorder};
use kiku_core::{
    keybinds::{Command, Hotkey, Key, Keybinds},
    macros::{run, Macro, Macros, Mode, Step, StepSpec, Variables},
    settings::Settings,
    words::Words,
};

const VARIABLES: &str = "name = \"Sam\"\n";
const ENTER: Key = Key::Other(0x0D);
const RIGHT: Key = Key::Other(0x27);
const ESCAPE: Key = Key::Other(0x1B);
const F13: Key = Key::Other(0x7C);

/// `macros.toml`'s text, keeping what can't load in `skipped`.
fn load(toml: &str) -> Macros {
    Macros::parse(toml, &Words::default(), &Variables::parse(VARIABLES).unwrap())
}

fn names(macros: &Macros) -> Vec<&str> {
    macros.entries().iter().map(|entry| entry.name.as_str()).collect()
}

fn hotkey(text: &str) -> Hotkey {
    Hotkey::parse(text).unwrap_or_else(|why| panic!("{text}: {why}"))
}

/// What running `steps` asks the executor to do.
fn ran(steps: &[Step]) -> Vec<Event> {
    let recorder = Recorder::default();
    run(steps, &Fake::new(VARIABLES), &recorder);
    recorder.events.into_inner()
}

/// One macro with only a shortcut, for the file.
fn keyed(name: &str, shortcut: &str) -> String {
    format!("[[macro]]\nname = \"{name}\"\nhotkey = \"{shortcut}\"\nsteps = [ {{ paste = \"x\" }} ]\n\n")
}

#[test]
fn t8_shortcuts_parse_and_are_checked() {
    let good = ["Ctrl+Alt+M", "ctrl+shift+f5", "F13", "Win+F2"];
    let bad = ["M", "Shift+A", "F5", "Win+L", "Ctrl+Alt+Delete", "Ctrl+Alt", "Ctrl+A+B", "Ctrl+Alt+Nope"];
    let (mut toml, mut lines) = (String::from("# shortcuts\n"), Vec::new());
    for (n, shortcut) in good.iter().enumerate() {
        toml.push_str(&keyed(&format!("good {n}"), shortcut));
    }
    for (n, shortcut) in bad.iter().enumerate() {
        lines.push(toml.lines().count() + 1);
        toml.push_str(&keyed(&format!("bad {n}"), shortcut));
    }
    let macros = load(&toml);
    assert_eq!(names(&macros), ["good 0", "good 1", "good 2", "good 3"], "{:?}", macros.skipped);
    let places: Vec<String> = lines.iter().map(|line| format!("macros.toml:{line}")).collect();
    assert_eq!(macros.skipped.iter().map(|(place, _)| place.clone()).collect::<Vec<_>>(), places, "{:?}", macros.skipped);
    assert!(macros.skipped.iter().all(|(_, why)| !why.trim().is_empty()));
    assert_eq!(macros.entries()[0].hotkey, Some(hotkey("Ctrl+Alt+M")));

    assert_eq!(hotkey("alt+ctrl+m"), hotkey("Ctrl+Alt+M"), "modifiers in any order");
    assert_eq!(hotkey("alt+ctrl+m").name(), "Ctrl+Alt+M");
    assert_eq!(hotkey("ctrl+shift+f5").name(), "Ctrl+Shift+F5");
    assert_eq!(hotkey("f13").name(), "F13");
    for shortcut in bad {
        assert!(Hotkey::parse(shortcut).is_err(), "{shortcut}");
    }
}

#[test]
fn t8_shortcut_clashes_are_refused() {
    let toml = [
        keyed("first", "Ctrl+Alt+M"),
        keyed("second", "alt+ctrl+m"),
        keyed("pastes", "Ctrl+Win+V"),
        keyed("one", "Ctrl+Win+1"),
        "[[macro]]\nname = \"off\"\nhotkey = \"Ctrl+Alt+O\"\nenabled = false\nsteps = [ { paste = \"x\" } ]\n".to_owned(),
    ]
    .concat();
    let macros = load(&toml);
    assert_eq!(names(&macros), ["first", "pastes", "one", "off"], "{:?}", macros.skipped);
    assert_eq!(macros.skipped.len(), 1);
    assert!(macros.skipped[0].1.contains("first"), "the later one is skipped, naming the earlier: {:?}", macros.skipped);

    let (handed, refused) = macros.hotkeys(&Keybinds::default());
    assert_eq!(handed, [(hotkey("Ctrl+Alt+M"), "first".to_owned()), (hotkey("Ctrl+Win+1"), "one".to_owned())], "a macro that is off keeps no key");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].0, "pastes");
    assert!(!refused[0].1.trim().is_empty());

    let mut binds = Keybinds::default();
    binds.set_command(Command::Paste, letter('1')).unwrap();
    let (handed, refused) = macros.hotkeys(&binds);
    assert_eq!(handed.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(), ["first", "pastes"], "V runs no command now, 1 does");
    assert_eq!(refused.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["one"]);

    let mut binds = Keybinds::default();
    binds.set_hold(vec![Key::Ctrl, F13]).unwrap();
    let (handed, refused) = load(&keyed("held", "Ctrl+F13")).hotkeys(&binds);
    assert!(handed.is_empty(), "the captions key itself");
    assert_eq!(refused.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["held"]);
}

#[test]
fn t8_a_macro_needs_a_trigger() {
    let toml = "[[macro]]\nname = \"nothing\"\nsteps = [ { paste = \"x\" } ]\n\n[[macro]]\nname = \"key only\"\nhotkey = \"Ctrl+Alt+K\"\nsteps = [ { paste = \"keyed\" } ]\n\n[[macro]]\nname = \"listen only\"\nlisten = true\nsteps = [ { press = \"right\" } ]\n";
    let macros = load(toml);
    assert_eq!(names(&macros), ["key only"]);
    assert!(macros.entries()[0].say.is_empty());
    assert_eq!(macros.skipped.iter().map(|(place, _)| place.as_str()).collect::<Vec<_>>(), ["macros.toml:1", "macros.toml:10"], "{:?}", macros.skipped);
    assert_eq!(events(&macros, &["key only"], &Fake::new(VARIABLES)), [paste("key only ")], "a shortcut-only macro isn't said");
}

#[test]
fn t8_shortcut_runs_the_macro_alone() {
    let toml = r#"[[macro]]
name   = "greet"
say    = "greet them"
hotkey = "Ctrl+Alt+G"
steps  = [ { paste = "Hello {name} [{heard}]" }, { press = "ctrl+enter" } ]

[[macro]]
name    = "off"
say     = "off one"
hotkey  = "Ctrl+Alt+O"
enabled = false
steps   = [ { paste = "x" } ]

[[macro]]
name   = "discord only"
say    = "in discord"
hotkey = "Ctrl+Alt+D"
apps   = ["Discord"]
steps  = [ { paste = "gg" } ]

[[macro]]
name   = "go to line"
say    = "go to line {num:number}"
hotkey = "Ctrl+Alt+L"
steps  = [ { press = "ctrl+g" }, { paste = "{num}" } ]

[[macro]]
name   = "down"
say    = "down {count:number}"
hotkey = "Ctrl+Alt+J"
steps  = [ { press = "down ×{count}" } ]
"#;
    let macros = load(toml);
    assert_eq!(names(&macros), ["greet", "off", "discord only"]);
    assert_eq!(macros.skipped.len(), 2, "{:?}", macros.skipped);
    assert!(macros.skipped[0].0 == "macros.toml:21" && macros.skipped[0].1.contains("num"), "{:?}", macros.skipped);
    assert!(macros.skipped[1].0 == "macros.toml:27" && macros.skipped[1].1.contains("count"), "{:?}", macros.skipped);

    let ctrl_enter: &[Key] = &[Key::Ctrl, ENTER];
    let alone = macros.plan_macro("greet", "Firefox", &Mode::Normal).unwrap();
    assert_eq!(ran(&alone), [paste("Hello Sam []"), press(&[ctrl_enter])], "{{heard}} is empty");
    let mut fake = Fake::new(VARIABLES);
    fake.app = "Firefox".into();
    assert_eq!(events(&macros, &["greet them"], &fake), [paste("Hello Sam [greet them]"), press(&[ctrl_enter])], "the same steps as saying it");

    assert!(macros.plan_macro("off", "Firefox", &Mode::Normal).is_err(), "a macro that is off");
    let elsewhere = macros.plan_macro("discord only", "Firefox", &Mode::Normal).unwrap_err();
    assert!(elsewhere.contains("Discord"), "says where it runs: {elsewhere}");
    assert_eq!(ran(&macros.plan_macro("discord only", "Discord", &Mode::Normal).unwrap()), [paste("gg")]);
    assert!(macros.plan_macro("no such macro", "Discord", &Mode::Normal).is_err());
}

const LISTENING: &str = r#"[[macro]]
name   = "next slide"
say    = ["next slide", "next"]
listen = true
steps  = [ { press = "right" } ]

[[macro]]
name  = "quiet"
say   = "quiet one"
steps = [ { press = "ctrl+q" } ]

[[macro]]
name   = "go to line"
say    = "go to line {n:number}"
listen = true
steps  = [ { press = "ctrl+g" }, { paste = "{n}" }, { press = "enter" } ]

[[macro]]
name   = "slack only"
say    = "mark read"
listen = true
apps   = ["Slack"]
steps  = [ { press = "escape" } ]
"#;

fn listening() -> Macros {
    let macros = Macros::parse(LISTENING, &Words::parse("> clear all => press ctrl+a\n"), &Variables::parse(VARIABLES).unwrap());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    macros
}

/// What the line `heard` without the key runs in `app`; nothing when no listening macro takes it.
fn heard(macros: &Macros, line: &str, app: &str, wake: &str) -> Option<Vec<Event>> {
    macros.plan_heard(line, app, wake, &Mode::Normal).map(|steps| ran(&steps))
}

#[test]
fn t8_listen_hears_whole_lines_only() {
    let macros = listening();
    let right = Some(vec![press(&[&[RIGHT]])]);
    assert_eq!(heard(&macros, "Next slide.", "PowerPoint", ""), right);
    assert_eq!(heard(&macros, "next", "PowerPoint", ""), right, "an alias");
    assert_eq!(heard(&macros, "go to the next slide", "PowerPoint", ""), None, "not the whole line");
    assert_eq!(heard(&macros, "so next slide please", "PowerPoint", ""), None);
    assert_eq!(heard(&macros, "quiet one", "PowerPoint", ""), None, "a macro that doesn't listen");
    assert_eq!(heard(&macros, "clear all", "PowerPoint", ""), None, "a words.txt > line doesn't listen");
    let ctrl_g: &[Key] = &[Key::Ctrl, letter('G')];
    assert_eq!(heard(&macros, "Go to line five.", "PowerPoint", ""), Some(vec![press(&[ctrl_g]), paste("5"), press(&[&[ENTER]])]));
    assert_eq!(heard(&macros, "mark read", "PowerPoint", ""), None, "apps scope it");
    assert_eq!(heard(&macros, "mark read", "Slack", ""), Some(vec![press(&[&[ESCAPE]])]));

    let mut fake = Fake::new(VARIABLES);
    fake.app = "PowerPoint".into();
    assert_eq!(events(&macros, &["next slide"], &fake), [press(&[&[RIGHT]])], "said holding the key, it runs as before");
}

#[test]
fn t8_wake_word_starts_the_line() {
    let macros = listening();
    let right = Some(vec![press(&[&[RIGHT]])]);
    assert_eq!(heard(&macros, "Computer, next slide.", "PowerPoint", "computer"), right);
    assert_eq!(heard(&macros, "COMPUTER next slide", "PowerPoint", "Computer"), right, "case aside");
    assert_eq!(heard(&macros, "next slide", "PowerPoint", "computer"), None, "without the wake word");
    assert_eq!(heard(&macros, "Computer.", "PowerPoint", "computer"), None, "the wake word alone");
    assert_eq!(heard(&macros, "hey computer, next slide", "PowerPoint", "computer"), None, "it starts the line");
    assert_eq!(heard(&macros, "Hey Kiku, next.", "PowerPoint", "hey kiku"), right, "a wake word of two words");
    assert_eq!(heard(&macros, "Hey, next.", "PowerPoint", "hey kiku"), None);
}

#[test]
fn t8_listening_is_off_unless_asked() {
    assert!(!Macros::default().listens());
    assert!(!load("[[macro]]\nname = \"a\"\nsay = \"a thing\"\nsteps = [ { press = \"right\" } ]\n").listens());
    assert!(!load("[[macro]]\nname = \"a\"\nsay = \"a thing\"\nlisten = true\nenabled = false\nsteps = [ { press = \"right\" } ]\n").listens(), "off");
    assert!(!Macros::parse("", &Words::parse("> clear all => press ctrl+a\n"), &Variables::default()).listens());
    assert!(load("[[macro]]\nname = \"a\"\nsay = \"a thing\"\nlisten = true\nsteps = [ { press = \"right\" } ]\n").listens());
    assert!(listening().listens());
}

#[test]
fn t8_triggers_round_trip_the_file() {
    let original = "# mine\n[[macro]]\nname = \"one\"   # the first\nsay = \"first one\"\nsteps = [ { paste = \"1\" } ]\n";
    let mut macros = macros(original, VARIABLES);
    let slides = Macro {
        name: "slides".into(),
        say: vec!["next slide".into()],
        apps: vec![],
        anywhere: false,
        enabled: true,
        hotkey: Some(hotkey("alt+ctrl+m")),
        listen: true,
        modes: vec![],
        steps: vec![StepSpec::Press("right".into())],
    };
    macros.add(&slides).unwrap();
    let keyed_only = Macro { name: "keyed".into(), say: vec![], hotkey: Some(hotkey("Ctrl+Alt+K")), listen: false, ..slides.clone() };
    macros.add(&keyed_only).unwrap();
    let text = macros.to_text();
    assert!(text.starts_with(original.trim_end()), "comments and order kept:\n{text}");
    assert!(text.contains("hotkey = \"Ctrl+Alt+M\"") && text.contains("listen = true"), "{text}");
    assert_eq!(macros.entries()[1..], [slides.clone(), keyed_only.clone()]);
    assert_eq!(common::macros(&text, VARIABLES).entries(), macros.entries(), "what was written reads back the same");

    let clash = Macro { name: "clash".into(), say: vec!["clash".into()], listen: false, ..slides.clone() };
    assert!(macros.add(&clash).is_err(), "a shortcut another macro has");
    assert_eq!(macros.to_text(), text, "a refused macro changes nothing");

    let plain = Macro { hotkey: None, listen: false, ..slides.clone() };
    macros.replace("slides", &plain).unwrap();
    let text = macros.to_text();
    assert!(!text.contains("Ctrl+Alt+M") && !text.contains("listen"), "{text}");
    assert_eq!(macros.entries()[1], plain);

    assert_eq!(Settings::default().macros_wake, "", "no wake word unless set");
    let settings = Settings { macros_wake: "hey kiku".into(), ..Settings::default() };
    assert!(settings.to_text().contains("macros.wake = hey kiku"), "{}", settings.to_text());
    assert_eq!(Settings::parse(&settings.to_text()), settings);
}
