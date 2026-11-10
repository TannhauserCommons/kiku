//! Voice macros, phase 3 (`.loop/voice-macros-3/SPINE.md`): the dictation key held or toggled, the release
//! delay, stop phrases, Kiku's actions as `kiku` steps, the command keys as actions, what the MCP tools show,
//! the README, and Kiku's state as variables (P10). Times are injected.

mod common;

use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{letter, press, Event, Fake, Recorder};
use kiku_core::{
    dictation::{stop_phrase, Dictation, DictationKey, DictationSettings, MAX_RELEASE_DELAY},
    keybinds::{Command, Key},
    macros::{run, Customization, KikuAction, KikuState, KikuStep, Macro, Macros, Mode, SettingsTab, Step, StepSpec, Variables, README},
    settings::Settings,
    words::{WordSettings, Words},
};

const STOP: &str = "that's all Kiku";

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn machine(key: DictationKey, delay: u64) -> Dictation {
    Dictation::new(&DictationSettings { key, release_delay: ms(delay), stop: vec![STOP.into()] })
}

/// A bare press: down, then up `held` later; ticks at both.
fn bare_press(dictation: &mut Dictation, at: Instant, held: u64) {
    dictation.key_down(at);
    dictation.tick(at);
    dictation.key_up(at + ms(held));
    dictation.tick(at + ms(held));
}

#[test]
fn p1_hold_dictates_after_250_ms_and_stops_on_release() {
    let t = Instant::now();
    let mut dictation = machine(DictationKey::Hold, 0);
    dictation.key_down(t);
    assert_eq!(dictation.next_wake(), Some(t + ms(250)), "the hold is due at 250 ms");
    dictation.tick(t + ms(249));
    assert!(!dictation.dictating(), "not at 249 ms");
    dictation.tick(t + ms(250));
    assert!(dictation.dictating(), "at 250 ms");
    assert!(!dictation.latched(), "holding isn't latched");
    dictation.key_up(t + ms(900));
    dictation.tick(t + ms(900));
    assert!(!dictation.dictating(), "letting go ends it");
    assert_eq!(dictation.next_wake(), None);
}

#[test]
fn p1_a_chord_in_hold_never_dictates() {
    let t = Instant::now();
    let mut dictation = machine(DictationKey::Hold, 0);
    dictation.key_down(t);
    dictation.chord(t + ms(100), false);
    dictation.tick(t + ms(300));
    assert!(!dictation.dictating(), "a chord in the first 250 ms never dictates");
    dictation.key_up(t + ms(400));
    dictation.tick(t + ms(400));
    assert!(!dictation.dictating());

    let mut late = machine(DictationKey::Hold, 0);
    late.key_down(t);
    late.tick(t + ms(300));
    assert!(late.dictating());
    late.chord(t + ms(400), false);
    late.tick(t + ms(400));
    assert!(!late.dictating(), "in hold, a chord while dictating ends it, as before");
}

#[test]
fn p1_toggle_starts_and_stops_on_bare_presses() {
    let t = Instant::now();
    let mut dictation = machine(DictationKey::Toggle, 0);
    dictation.key_down(t);
    dictation.tick(t + ms(300));
    assert!(!dictation.dictating(), "toggle never dictates by holding");
    dictation.key_up(t + ms(320));
    dictation.tick(t + ms(320));
    assert!(dictation.dictating() && dictation.latched(), "a bare press starts it, latched");
    dictation.tick(t + ms(5000));
    assert!(dictation.dictating(), "and it stays on");
    bare_press(&mut dictation, t + ms(6000), 80);
    assert!(!dictation.dictating() && !dictation.latched(), "the next bare press ends it");
}

#[test]
fn p1_toggle_chords_leave_dictation_on_except_stop() {
    let t = Instant::now();
    let mut dictation = machine(DictationKey::Toggle, 0);
    bare_press(&mut dictation, t, 50);
    assert!(dictation.dictating());

    dictation.key_down(t + ms(1000));
    dictation.chord(t + ms(1050), false);
    dictation.key_up(t + ms(1100));
    dictation.tick(t + ms(1100));
    assert!(dictation.dictating() && dictation.latched(), "a command key leaves it on, and isn't a bare press");

    dictation.key_down(t + ms(2000));
    dictation.chord(t + ms(2050), true);
    dictation.tick(t + ms(2050));
    assert!(!dictation.dictating(), "stopping captions ends it at once");
    dictation.key_up(t + ms(2100));
    dictation.tick(t + ms(2100));
    assert!(!dictation.dictating(), "and letting go of that chord doesn't start it");

    let mut off = machine(DictationKey::Toggle, 0);
    off.key_down(t);
    off.chord(t + ms(20), false);
    off.key_up(t + ms(60));
    off.tick(t + ms(60));
    assert!(!off.dictating(), "a chord while off doesn't start it");
}

#[test]
fn p2_release_delay_keeps_listening() {
    let t = Instant::now();
    for key in [DictationKey::Hold, DictationKey::Toggle] {
        let mut dictation = machine(key, 500);
        match key {
            DictationKey::Hold => {
                dictation.key_down(t - ms(1000));
                dictation.tick(t - ms(500));
                dictation.key_up(t);
            }
            DictationKey::Toggle => {
                bare_press(&mut dictation, t - ms(2000), 50);
                dictation.key_down(t - ms(50));
                dictation.key_up(t);
            }
        }
        dictation.tick(t);
        assert!(dictation.dictating(), "{key:?}: still listening when it ends");
        assert_eq!(dictation.next_wake(), Some(t + ms(500)), "{key:?}");
        dictation.tick(t + ms(499));
        assert!(dictation.dictating(), "{key:?}: at 499 ms");
        dictation.tick(t + ms(500));
        assert!(!dictation.dictating(), "{key:?}: over at 500 ms");
        assert_eq!(dictation.next_wake(), None, "{key:?}");
    }

    let mut stopped = machine(DictationKey::Toggle, 500);
    stopped.start(t);
    stopped.stop(t);
    stopped.tick(t + ms(499));
    assert!(stopped.dictating(), "a stop step waits out the delay too");
    stopped.tick(t + ms(500));
    assert!(!stopped.dictating());

    let mut at_once = machine(DictationKey::Toggle, 500);
    at_once.start(t);
    at_once.stop_now();
    assert!(!at_once.dictating(), "stop_now has no delay");
    assert_eq!(at_once.next_wake(), None);

    let mut short = machine(DictationKey::Hold, 500);
    short.key_down(t);
    short.key_up(t + ms(100));
    short.tick(t + ms(100));
    assert!(!short.dictating(), "a press too short to dictate has no delay after it");
}

#[test]
fn p2_pressing_again_in_the_delay_carries_on() {
    let t = Instant::now();
    let mut dictation = machine(DictationKey::Hold, 500);
    dictation.key_down(t);
    dictation.tick(t + ms(300));
    dictation.key_up(t + ms(1000));
    dictation.tick(t + ms(1000));
    dictation.key_down(t + ms(1200));
    for at in [1200, 1250, 1600, 2000] {
        dictation.tick(t + ms(at));
        assert!(dictation.dictating(), "no gap at {at} ms");
    }
    dictation.key_up(t + ms(2100));
    dictation.tick(t + ms(2100));
    assert!(dictation.dictating(), "letting go again starts a new delay");
    assert_eq!(dictation.next_wake(), Some(t + ms(2600)));
    dictation.tick(t + ms(2600));
    assert!(!dictation.dictating());

    let mut toggled = machine(DictationKey::Toggle, 500);
    bare_press(&mut toggled, t, 50);
    bare_press(&mut toggled, t + ms(1000), 50);
    assert!(toggled.dictating(), "in the delay");
    bare_press(&mut toggled, t + ms(1200), 50);
    toggled.tick(t + ms(3000));
    assert!(toggled.dictating() && toggled.latched(), "a press in the delay carries on, as if it never ended");
}

#[test]
fn p2_delay_is_read_clamped_and_written() {
    let delay = |line: &str| Settings::parse(line).dictation.release_delay;
    assert_eq!(delay("dictation.release_delay = 5000"), MAX_RELEASE_DELAY);
    assert_eq!(delay("dictation.release_delay = 750"), ms(750));
    assert_eq!(delay("dictation.release_delay = 333"), ms(333), "the file takes any value in range");
    assert_eq!(delay("dictation.release_delay = -1"), Duration::ZERO);
    assert_eq!(delay("dictation.release_delay = soon"), Duration::ZERO);

    assert_eq!(Settings::parse("").dictation, DictationSettings::default());
    assert_eq!(DictationSettings::default(), DictationSettings { key: DictationKey::Hold, release_delay: Duration::ZERO, stop: vec![STOP.into()] });
    assert_eq!(Settings::parse("dictation.key = toggle").dictation.key, DictationKey::Toggle);
    assert_eq!(Settings::parse("dictation.key = sideways").dictation.key, DictationKey::Hold);
    assert!(Settings::parse("dictation.stop =").dictation.stop.is_empty(), "empty is none");

    for dictation in [
        DictationSettings { key: DictationKey::Toggle, release_delay: ms(750), stop: vec![STOP.into(), "over and out".into()] },
        DictationSettings { key: DictationKey::Hold, release_delay: ms(1000), stop: vec![] },
        DictationSettings::default(),
    ] {
        let settings = Settings { dictation: dictation.clone(), ..Settings::default() };
        let text = settings.to_text();
        assert_eq!(Settings::parse(&text), settings, "{text}");
    }
    let text = Settings { dictation: DictationSettings { key: DictationKey::Toggle, release_delay: ms(750), stop: vec![STOP.into(), "over and out".into()] }, ..Settings::default() }.to_text();
    for line in ["dictation.key = toggle", "dictation.release_delay = 750", "dictation.stop = that's all Kiku, over and out"] {
        assert!(text.lines().any(|written| written == line), "{line} in:\n{text}");
    }
}

#[test]
fn p3_stop_phrase_is_found_at_the_end_only() {
    let phrases = vec![STOP.to_owned()];
    assert_eq!(stop_phrase("write this, that's all Kiku.", &phrases), Some("write this"));
    assert_eq!(stop_phrase("Done. That's all Kiku", &phrases), Some("Done."), "a sentence's full stop stays");
    assert_eq!(stop_phrase("That's all, Kiku!", &phrases), Some(""));
    assert_eq!(stop_phrase("THATS ALL KIKU", &phrases), None, "the words must be the phrase's");
    assert_eq!(stop_phrase("that's all Kiku and more", &phrases), None, "only at the end");
    assert_eq!(stop_phrase("that's all Kikuyu", &phrases), None, "whole words");
    assert_eq!(stop_phrase("that's all Kiku", &phrases), Some(""), "the phrase alone");
    assert_eq!(stop_phrase("write this, that's all Kiku.", &[]), None, "no phrases, no match");
    let two = vec![STOP.to_owned(), "over and out".to_owned()];
    assert_eq!(stop_phrase("see you, over and out", &two), Some("see you"));
    assert_eq!(stop_phrase("see you that's all kiku", &two), Some("see you"));
}

fn stopping(macros: &str, words: &str) -> Macros {
    let mut macros = Macros::parse(macros, &Words::parse(words), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    macros.set_stop_phrases(vec![STOP.into()]);
    macros
}

fn ran(macros: &Macros, lines: &[&str], fake: &Fake) -> Vec<Event> {
    let recorder = Recorder::default();
    run(&macros.plan(lines, &fake.app, &WordSettings::default(), &Mode::Normal), fake, &recorder);
    recorder.events.take()
}

fn trimmed(events: Vec<Event>) -> Vec<Event> {
    events.into_iter().map(|event| if let Event::Paste(text) = event { Event::Paste(text.trim().to_owned()) } else { event }).collect()
}

fn stop_at_once() -> Event {
    Event::Kiku(KikuAction::StopDictation, None, None, true)
}

#[test]
fn p3_stop_phrase_plans_the_text_then_stops() {
    let toml = "[[macro]]\nname = \"new note\"\nsay = \"new note\"\nsteps = [ { press = \"ctrl+n\" } ]\n";
    let macros = stopping(toml, "teh => the\n");
    let fake = Fake::new("");
    assert_eq!(trimmed(ran(&macros, &["send teh file that's all Kiku."], &fake)), [Event::Paste("send the file".into()), stop_at_once()], "through the rules, then stop");
    let steps = macros.plan(&["send it that's all Kiku"], "Discord", &WordSettings::default(), &Mode::Normal);
    assert_eq!(steps.last(), Some(&Step::Kiku(KikuStep { at_once: true, ..KikuStep::new(KikuAction::StopDictation) })));
    assert_eq!(ran(&macros, &["new note, that's all Kiku"], &fake), [press(&[&[Key::Ctrl, letter('N')]]), stop_at_once()], "a macro before it still runs");
    assert_eq!(ran(&macros, &["That's all Kiku."], &fake), [stop_at_once()], "alone it only stops");
    assert_eq!(trimmed(ran(&macros, &["that's all Kiku and more"], &fake)), [Event::Paste("that's all Kiku and more".into())], "mid-line it is words");
    let none = stopping("", "");
    let mut none = none;
    none.set_stop_phrases(vec![]);
    assert_eq!(trimmed(ran(&none, &["send it that's all Kiku"], &fake)), [Event::Paste("send it that's all Kiku".into())], "no stop phrases");
}

#[test]
fn p4_a_start_step_stays_on_through_release() {
    let t = Instant::now();
    let mut dictation = machine(DictationKey::Hold, 0);
    dictation.key_down(t);
    dictation.tick(t + ms(300));
    dictation.start(t + ms(400));
    dictation.key_up(t + ms(600));
    dictation.tick(t + ms(600));
    assert!(dictation.dictating() && dictation.latched(), "letting go of the key that said start doesn't end it");
    dictation.tick(t + ms(9000));
    assert!(dictation.dictating());
    bare_press(&mut dictation, t + ms(10_000), 60);
    assert!(!dictation.dictating(), "the next bare press does");

    let mut toggled = machine(DictationKey::Hold, 0);
    toggled.toggle(t);
    assert!(toggled.dictating() && toggled.latched(), "toggle starts it");
    toggled.toggle(t + ms(100));
    assert!(!toggled.dictating(), "and ends it");
    toggled.start(t + ms(200));
    toggled.chord(t + ms(300), false);
    assert!(toggled.dictating(), "a step's dictation stays on through a command key");
    toggled.chord(t + ms(400), true);
    assert!(!toggled.dictating(), "but not through stopping captions");
}

fn one_step(step: &str) -> String {
    format!("[[macro]]\nname = \"act\"\nsay = \"do the thing\"\nsteps = [ {step} ]\n")
}

fn load(toml: &str) -> Macros {
    Macros::parse(toml, &Words::default(), &Variables::default())
}

fn planned(macros: &Macros, line: &str) -> Vec<Step> {
    macros.plan(&[line], "Notepad", &WordSettings::default(), &Mode::Normal)
}

#[test]
fn p5_every_action_reads_and_writes() {
    assert_eq!(KikuAction::ALL.len(), 21);
    for action in KikuAction::ALL {
        let name = action.name();
        assert_eq!(KikuAction::parse(name), Ok(action));
        assert_eq!(KikuAction::parse(&name.to_uppercase()), Ok(action), "case ignored");
        assert!(!action.label().is_empty());
        for written in [name.to_owned(), name.to_uppercase()] {
            let macros = load(&one_step(&format!("{{ kiku = \"{written}\" }}")));
            assert!(macros.skipped.is_empty(), "{written}: {:?}", macros.skipped);
            assert_eq!(planned(&macros, "do the thing"), [Step::Kiku(KikuStep::new(action))], "{written}");
        }
        let spec = StepSpec::Kiku { action: name.to_owned(), tab: None, search: None };
        let new = Macro { name: "act".into(), say: vec!["do the thing".into()], apps: vec![], anywhere: false, enabled: true, hotkey: None, listen: false, modes: vec![], steps: vec![spec.clone()] };
        let mut macros = Macros::default();
        macros.add(&new).unwrap();
        assert!(macros.to_text().contains(&format!("{{ kiku = \"{name}\" }}")), "{}", macros.to_text());
        assert_eq!(load(&macros.to_text()).entries()[0].steps, [spec]);
    }
    let names: Vec<&str> = KikuAction::ALL.iter().map(|action| action.name()).collect();
    assert!(names.contains(&"start dictation") && names.contains(&"open history") && !names.contains(&"quit"), "{names:?}");

    let options = load(&one_step("{ kiku = \"open settings\", tab = \"Words\" }"));
    let mut tabbed = KikuStep::new(KikuAction::OpenSettings);
    tabbed.tab = Some(kiku_core::macros::Named::Known(SettingsTab::Words));
    assert_eq!(planned(&options, "do the thing"), [Step::Kiku(tabbed)]);
    for tab in SettingsTab::ALL {
        assert_eq!(SettingsTab::parse(tab.name()), Ok(tab));
    }
    let written = StepSpec::Kiku { action: "open settings".into(), tab: Some("words".into()), search: None };
    let new = Macro { name: "s".into(), say: vec!["s".into()], apps: vec![], anywhere: false, enabled: true, hotkey: None, listen: false, modes: vec![], steps: vec![written.clone()] };
    let mut macros = Macros::default();
    macros.add(&new).unwrap();
    assert!(macros.to_text().contains("{ kiku = \"open settings\", tab = \"words\" }"), "{}", macros.to_text());
    assert_eq!(load(&macros.to_text()).entries()[0].steps, [written]);
}

/// The one reason `toml` was skipped for.
fn skipped_for(toml: &str) -> String {
    let macros = load(toml);
    assert_eq!(macros.skipped.len(), 1, "{:?}", macros.skipped);
    macros.skipped[0].1.clone()
}

#[test]
fn p5_unknown_actions_and_options_skip_with_the_list() {
    for unknown in ["quit", "nope"] {
        let why = skipped_for(&one_step(&format!("{{ kiku = \"{unknown}\" }}")));
        for action in KikuAction::ALL {
            assert!(why.contains(action.name()), "{unknown}: the reason lists {}: {why}", action.name());
        }
    }
    let why = skipped_for(&one_step("{ kiku = \"open settings\", search = \"x\" }"));
    assert!(why.contains("search") && why.contains("open history"), "{why}");
    let why = skipped_for(&one_step("{ kiku = \"open history\", tab = \"words\" }"));
    assert!(why.contains("tab") && why.contains("open settings"), "{why}");
    let why = skipped_for(&one_step("{ kiku = \"open settings\", tab = \"nowhere\" }"));
    assert!(why.contains("general") && why.contains("macros"), "the tabs are listed: {why}");
    let why = skipped_for(&one_step("{ paste = \"x\", tab = \"words\" }"));
    assert!(why.contains("kiku"), "tab is only for kiku: {why}");
}

#[test]
fn p5_open_history_search_is_a_private_template() {
    let macros = load("[[macro]]\nname = \"find\"\nsay = \"find {words}\"\nsteps = [ { kiku = \"open history\", search = \"{words}\" } ]\n");
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    let steps = planned(&macros, "find the launch codes");
    assert!(matches!(&steps[..], [Step::Kiku(KikuStep { search: Some(_), .. })]), "{steps:?}");
    assert!(!format!("{steps:?}").contains("launch codes"), "{steps:?}");
    let recorder = Recorder::default();
    run(&steps, &Fake::new(""), &recorder);
    assert_eq!(recorder.events.take(), [Event::Kiku(KikuAction::OpenHistory, None, Some("the launch codes".into()), false)]);
    let unknown = load("[[macro]]\nname = \"find\"\nsay = \"find\"\nsteps = [ { kiku = \"open history\", search = \"{nothing}\" } ]\n");
    assert_eq!(unknown.skipped.len(), 1, "a search's variables are checked as a paste's are");
}

#[test]
fn p6_command_keys_are_actions() {
    let mut seen = Vec::new();
    for command in Command::ALL {
        match (command, command.action()) {
            (Command::Quit, action) => assert_eq!(action, None, "quit is only a key"),
            (_, None) => panic!("{command:?} has no action"),
            (_, Some(action)) => {
                assert!(!seen.contains(&action), "{command:?} shares {action:?}");
                seen.push(action);
            }
        }
    }
    let expected = [
        (Command::Paste, KikuAction::PasteWhatISaid),
        (Command::Undo, KikuAction::Undo),
        (Command::Stop, KikuAction::ToggleCaptions),
        (Command::Lock, KikuAction::ToggleLock),
        (Command::Hide, KikuAction::TogglePanel),
        (Command::SaveMiss, KikuAction::SaveMiss),
        (Command::Subtitles, KikuAction::ToggleCaptionsMode),
        (Command::Palette, KikuAction::ShowPalette),
    ];
    for (command, action) in expected {
        assert_eq!(command.action(), Some(action), "{command:?}");
    }
    assert_eq!(planned(&load(""), "What can I say?"), [Step::Kiku(KikuStep::new(KikuAction::ShowPalette))]);
}

fn custom(name: &str, macros: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-phase3-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("macros.toml"), macros).unwrap();
    folder
}

#[test]
fn p9_mcp_lists_kiku_steps() {
    let toml = "[[macro]]\nname = \"dictate\"\nsay = \"start writing\"\nsteps = [ { kiku = \"start dictation\" } ]\n\n[[macro]]\nname = \"find\"\nsay = \"find {words}\"\nsteps = [ { kiku = \"open history\", search = \"{words}\" } ]\n\n[[macro]]\nname = \"bad\"\nsay = \"bad one\"\nsteps = [ { kiku = \"nope\" } ]\n";
    let folder = custom("mcp", toml);
    let found = Customization::read(&folder, &Words::default());
    let _ = fs::remove_dir_all(&folder);

    let listed = found.list_macros();
    assert!(listed.contains("kiku: start dictation"), "{listed}");
    assert!(listed.contains("kiku: open history"), "{listed}");
    let tried = found.try_phrase("start writing", "Notepad");
    assert!(tried.contains("kiku: start dictation"), "{tried}");
    let tried = found.try_phrase("find my secret plans", "Notepad");
    assert!(tried.contains("kiku: open history") && tried.contains("[private]"), "{tried}");
    assert!(!tried.contains("secret plans"), "{tried}");
    let bad_line = toml.lines().position(|line| line.contains("name = \"bad\"")).unwrap();
    let problems = found.problems();
    assert!(
        problems.iter().any(|(at, why)| at == &format!("macros.toml:{bad_line}") && why.contains("start dictation")),
        "the [[macro]] header's line, with the actions listed: {problems:?}"
    );
}

#[test]
fn p9_readme_lists_every_action() {
    for action in KikuAction::ALL {
        assert!(README.contains(action.name()), "README names {}", action.name());
    }
    for setting in ["dictation.key", "dictation.release_delay", "dictation.stop"] {
        assert!(README.contains(setting), "README explains {setting}");
    }
    for variable in ["{dictating}", "{captions}", "{mode}"] {
        assert!(README.contains(variable), "README lists {variable}");
    }
}

#[test]
fn p10_state_variables_fill() {
    let toml = "[[macro]]\nname = \"state\"\nsay = \"state\"\nsteps = [ { paste = \"{dictating} {captions} {mode}\" } ]\n\n[[macro]]\nname = \"look\"\nsay = \"look\"\nsteps = [ { open = \"https://example.com/?d={dictating}&m={mode}\" }, { kiku = \"open history\", search = \"{mode}\" } ]\n";
    let macros = Macros::parse(toml, &Words::default(), &Variables::parse("dictating = \"x\"\nmode = \"y\"\ncaptions = \"z\"\n").unwrap());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    let mut fake = Fake::new("dictating = \"x\"\nmode = \"y\"\ncaptions = \"z\"\n");
    fake.state = KikuState { dictating: true, captions: false, mode: "coding".into() };
    assert_eq!(trimmed(ran(&macros, &["state"], &fake)), [Event::Paste("on off coding".into())], "Kiku's state, not variables.toml's");
    fake.state = KikuState { dictating: false, captions: true, mode: "normal".into() };
    assert_eq!(trimmed(ran(&macros, &["state"], &fake)), [Event::Paste("off on normal".into())]);
    assert_eq!(
        ran(&macros, &["look"], &fake),
        [Event::Open("https://example.com/?d=off&m=normal".into()), Event::Kiku(KikuAction::OpenHistory, None, Some("normal".into()), false)],
        "in links and a kiku step's options too"
    );
}

#[test]
fn p10_templated_actions_are_checked_when_they_run() {
    let toml = "[[macro]]\nname = \"kiku do\"\nsay = \"kiku {words}\"\nsteps = [ { kiku = \"{words}\" } ]\n\n[[macro]]\nname = \"settings\"\nsay = \"settings {words}\"\nsteps = [ { kiku = \"open settings\", tab = \"{words}\" } ]\n";
    let macros = load(toml);
    assert!(macros.skipped.is_empty(), "a templated action is checked when it runs: {:?}", macros.skipped);
    let fake = Fake::new("");
    assert_eq!(ran(&macros, &["kiku start dictation"], &fake), [Event::Kiku(KikuAction::StartDictation, None, None, false)]);
    assert_eq!(ran(&macros, &["kiku Toggle Lock"], &fake), [Event::Kiku(KikuAction::ToggleLock, None, None, false)], "case ignored");
    for unknown in ["kiku quit", "kiku nope"] {
        let events = ran(&macros, &[unknown], &fake);
        let [Event::Notify(notice)] = &events[..] else { panic!("{unknown}: one notice, no action: {events:?}") };
        assert!(notice.contains("kiku do") && notice.contains("start dictation") && notice.contains("open history"), "{unknown}: {notice}");
    }
    assert_eq!(ran(&macros, &["settings words"], &fake), [Event::Kiku(KikuAction::OpenSettings, Some(SettingsTab::Words), None, false)]);
    let events = ran(&macros, &["settings nowhere"], &fake);
    let [Event::Notify(notice)] = &events[..] else { panic!("one notice: {events:?}") };
    assert!(notice.contains("settings") && notice.contains("general") && notice.contains("macros"), "{notice}");
}
