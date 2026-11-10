//! Voice macros, phase 2 (`.loop/voice-macros-2/SPINE.md`): modes, `open`, `switch`, `run` (off until allowed,
//! each command approved), `ask_claude`, what the MCP server shows, the palette's list, and F14.

mod common;

use std::{fs, path::{Path, PathBuf}};

use common::{events, events_in, macros, Event, Fake, Recorder};
use kiku_core::{
    macros::{open_target, run, run_step, Approvals, BuiltinPhrases, CommandPolicy, Customization, Macros, Mode, OpenTarget, Step, Variables},
    settings::Settings,
    words::{WordSettings, Words},
};

const VARIABLES: &str = "name = \"Sam\"\nemail = \"me@example.com\"\n";

/// `macros.toml`'s text, keeping what can't load in `skipped`.
fn load(toml: &str) -> Macros {
    Macros::parse(toml, &Words::default(), &Variables::parse(VARIABLES).unwrap())
}

fn plan(macros: &Macros, lines: &[&str], app: &str, mode: &Mode) -> Vec<Step> {
    macros.plan(lines, app, &WordSettings::default(), mode)
}

fn coding() -> Mode {
    Mode::Own("coding".into())
}

/// What dictating `lines` does in `mode`, in the fake's app (Discord).
fn heard_in(macros: &Macros, lines: &[&str], mode: &Mode) -> Vec<Event> {
    events_in(macros, lines, &Fake::new(VARIABLES), mode, &Recorder::default())
}

/// The text of every paste, trimmed.
fn pasted(events: &[Event]) -> Vec<String> {
    events.iter().filter_map(|event| if let Event::Paste(text) = event { Some(text.trim().to_owned()) } else { None }).collect()
}

fn notices(events: &[Event]) -> Vec<String> {
    events.iter().filter_map(|event| if let Event::Notify(text) = event { Some(text.clone()) } else { None }).collect()
}

const MODES: &str = r#"
[[mode]]
name = "coding"

[[mode]]
name = "writing"

[[macro]]
name = "build"
say = "build it"
modes = ["coding"]
steps = [ { press = "ctrl+b" } ]

[[macro]]
name = "hello"
say = "say hello"
steps = [ { paste = "hi" } ]

[[macro]]
name = "start coding"
say = "start coding"
steps = [ { mode = "coding" } ]
"#;

#[test]
fn n1_modes_scope_macros() {
    let macros = macros(MODES, VARIABLES);
    let plain = heard_in(&common::macros("", ""), &["build it"], &Mode::Normal);
    assert!(matches!(heard_in(&macros, &["build it"], &coding())[..], [Event::Press(_)]), "a coding macro fires in coding");
    assert_eq!(heard_in(&macros, &["build it"], &Mode::Normal), plain, "and is just words elsewhere");
    assert_eq!(heard_in(&macros, &["build it"], &Mode::Own("writing".into())), plain);
    for mode in [Mode::Normal, Mode::Commands, coding(), Mode::Own("writing".into())] {
        assert_eq!(pasted(&heard_in(&macros, &["say hello"], &mode)), ["hi"], "a macro without modes fires in {mode:?}");
    }
    assert!(heard_in(&macros, &["say hello"], &Mode::Off).is_empty(), "but not when dictation is off");

    let unknown = load("[[macro]]\nname = \"x\"\nsay = \"x\"\nmodes = [\"nope\"]\nsteps = [ { paste = \"x\" } ]\n");
    assert_eq!(unknown.skipped.len(), 1, "a mode no one defined: {:?}", unknown.skipped);
    assert_eq!(unknown.skipped[0].0, "macros.toml:1");
    assert!(unknown.skipped[0].1.contains("nope"), "{:?}", unknown.skipped);
    assert_eq!(Mode::named("commands"), Mode::Commands);
    assert_eq!(Mode::named("coding"), coding());
    assert_eq!(coding().name(), "coding");
}

#[test]
fn n1_commands_mode_pastes_nothing_else() {
    let macros = macros(MODES, VARIABLES);
    assert!(plan(&macros, &["just talking here"], "Discord", &Mode::Commands).is_empty(), "nothing pasted");
    let events = heard_in(&macros, &["just talking", "say hello", "more talk"], &Mode::Commands);
    assert_eq!(pasted(&events), ["hi"], "only the macro: {events:?}");
    assert_eq!(events.len(), 1, "{events:?}");
}

#[test]
fn n1_off_mode_hears_only_mode_phrases() {
    let mut macros = macros(MODES, VARIABLES);
    for said in ["say hello", "hello there", "start coding"] {
        assert!(plan(&macros, &[said], "Discord", &Mode::Off).is_empty(), "{said}");
    }
    assert_eq!(plan(&macros, &["Dictation on."], "Discord", &Mode::Off), [Step::SetMode(Mode::Normal)]);
    assert_eq!(plan(&macros, &["command mode"], "Discord", &Mode::Off), [Step::SetMode(Mode::Commands)]);
    macros.set_phrases(BuiltinPhrases { dictation_on: false, ..BuiltinPhrases::default() });
    assert!(plan(&macros, &["dictation on"], "Discord", &Mode::Off).is_empty(), "a phrase turned off is just words, and off pastes none");
}

#[test]
fn n1_mode_phrases_and_steps_switch_modes() {
    let mut macros = macros(MODES, VARIABLES);
    let switches = |macros: &Macros, said: &str| plan(macros, &[said], "Discord", &Mode::Normal);
    assert_eq!(switches(&macros, "Command mode."), [Step::SetMode(Mode::Commands)]);
    assert_eq!(switches(&macros, "normal mode"), [Step::SetMode(Mode::Normal)]);
    assert_eq!(switches(&macros, "Dictation off"), [Step::SetMode(Mode::Off)]);
    assert_eq!(plan(&macros, &["dictation on"], "Discord", &Mode::Commands), [Step::SetMode(Mode::Normal)]);
    assert_eq!(switches(&macros, "start coding"), [Step::SetMode(coding())], "a mode step");
    assert_eq!(heard_in(&macros, &["start coding"], &Mode::Normal), [Event::SetMode(coding())], "run, it sets the mode");
    assert!(
        !switches(&macros, "let's go to command mode").iter().any(|step| matches!(step, Step::SetMode(_))),
        "only as the whole line"
    );

    macros.set_phrases(BuiltinPhrases { command_mode: false, ..BuiltinPhrases::default() });
    let events = heard_in(&macros, &["command mode"], &Mode::Normal);
    assert_eq!(pasted(&events), ["command mode"], "a phrase turned off is just words: {events:?}");
    assert_eq!(switches(&macros, "normal mode"), [Step::SetMode(Mode::Normal)], "the others still work");

    let unknown = load("[[macro]]\nname = \"x\"\nsay = \"x\"\nsteps = [ { mode = \"nope\" } ]\n");
    assert_eq!(unknown.skipped.len(), 1, "a step to a mode no one defined: {:?}", unknown.skipped);
}

#[test]
fn n1_app_modes_switch_by_app() {
    let toml = MODES.replacen("name = \"coding\"\n", "name = \"coding\"\napps = [\"Code\"]\n", 1);
    let macros = macros(&toml, VARIABLES);
    assert_eq!(macros.modes().for_app("Code"), Some(coding()));
    assert_eq!(macros.modes().for_app("code"), Some(coding()), "apps as captions name them, any case");
    assert_eq!(macros.modes().for_app("Discord"), None);
    assert_eq!(macros.modes().effective("Code", &Mode::Normal), coding(), "Code in front: its mode");
    assert_eq!(macros.modes().effective("Code", &Mode::Commands), Mode::Commands, "a mode chosen by voice wins");
    assert_eq!(macros.modes().effective("Discord", &Mode::Normal), Mode::Normal);
    assert!(matches!(plan(&macros, &["build it"], "Code", &Mode::Normal)[..], [Step::Press(_)]), "build it runs in Code");
    assert!(matches!(plan(&macros, &["build it"], "Discord", &Mode::Normal)[..], [Step::Paste { .. }]), "and is words elsewhere");
    assert!(plan(&macros, &["just words"], "Code", &Mode::Off).is_empty(), "off stays off");
}

#[test]
fn n2_open_targets_are_validated() {
    for url in ["https://example.com/a?b=c", "http://localhost:8080/", "mailto:me@example.com"] {
        assert!(matches!(open_target(url), Ok(OpenTarget::Url(_))), "{url}: {:?}", open_target(url));
    }
    for refused in ["javascript:alert(1)", "file:///etc/passwd", "ftp://example.com", "cmd /c del x", "notepad & calc", ""] {
        let why = open_target(refused).expect_err(refused);
        assert!(!why.trim().is_empty(), "{refused}: a reason");
    }
    let folder = std::env::temp_dir();
    assert_eq!(open_target(&folder.display().to_string()), Ok(OpenTarget::Path(folder.clone())));
    let missing = folder.join(format!("kiku-missing-{}", std::process::id()));
    assert!(open_target(&missing.display().to_string()).is_err(), "a path that isn't there");
    assert_eq!(open_target("Notepad"), Ok(OpenTarget::App("Notepad".into())));
    assert_eq!(open_target("Visual Studio Code"), Ok(OpenTarget::App("Visual Studio Code".into())));
}

#[test]
fn n2_open_fills_and_encodes() {
    let macros = macros(
        r#"
[[macro]]
name = "search"
say = "search for {words}"
steps = [ { open = "https://duckduckgo.com/?q={words|url}" } ]

[[macro]]
name = "visit"
say = "visit {words}"
steps = [ { open = "{words}" } ]

[[macro]]
name = "go to"
say = "go to {words}"
steps = [ { switch = "{words}" } ]
"#,
        "",
    );
    let fake = Fake::new("");
    assert_eq!(events(&macros, &["search for rust lifetimes"], &fake), [Event::Open("https://duckduckgo.com/?q=rust%20lifetimes".into())]);
    assert_eq!(events(&macros, &["go to Discord"], &fake), [Event::Switch("Discord".into())]);
    let refused = events(&macros, &["visit javascript:alert"], &fake);
    assert!(matches!(&refused[..], [Event::Notify(why)] if why.contains("visit")), "a target that fails the checks is a notice naming its macro: {refused:?}");
    assert!(matches!(&plan(&macros, &["go to Discord"], "Discord", &Mode::Normal)[..], [Step::Switch { .. }]));
}

const RUN: &str = r#"
[[macro]]
name = "say hello"
say = "say hello"
steps = [ { run = "hello.bat", args = ["{heard}"], output = "paste" } ]
"#;

fn allowed() -> CommandPolicy {
    CommandPolicy { enabled: true, anywhere: false }
}

/// `macros` allowed to run programs, with each of its commands approved.
fn approved(mut macros: Macros, policy: CommandPolicy) -> Macros {
    let mut approvals = Approvals::default();
    for (_, command) in macros.commands() {
        approvals.add(&command);
    }
    macros.set_commands(policy, approvals);
    macros
}

fn is_run(steps: &[Step]) -> bool {
    matches!(steps, [Step::Run { .. }])
}

/// The one notice `steps` are, naming the macro `name`.
fn one_notice(steps: &[Step], name: &str) -> String {
    match steps {
        [Step::Notify(why)] if why.contains(name) => why.clone(),
        other => panic!("a notice naming {name}, got {other:?}"),
    }
}

#[test]
fn n4_run_is_off_until_allowed() {
    let fresh = macros(RUN, "");
    let why = one_notice(&plan(&fresh, &["say hello"], "Discord", &Mode::Normal), "say hello");
    assert!(why.contains("off"), "{why}");
    let off = approved(macros(RUN, ""), CommandPolicy::default());
    one_notice(&plan(&off, &["say hello"], "Discord", &Mode::Normal), "say hello");
    assert!(is_run(&plan(&approved(macros(RUN, ""), allowed()), &["say hello"], "Discord", &Mode::Normal)));

    assert_eq!(Settings::default().run_programs, CommandPolicy::default(), "off unless turned on");
    assert!(!CommandPolicy::default().enabled && !CommandPolicy::default().anywhere);
    let settings = Settings { run_programs: CommandPolicy { enabled: true, anywhere: true }, ..Settings::default() };
    assert_eq!(Settings::parse(&settings.to_text()), settings);
    let phrases = Settings { mode_phrases: BuiltinPhrases { dictation_off: false, ..BuiltinPhrases::default() }, ..Settings::default() };
    assert_eq!(Settings::parse(&phrases.to_text()), phrases);
    assert_eq!(Settings::default().mode_phrases, BuiltinPhrases::default());
}

#[test]
fn n4_run_needs_approval_by_exact_command() {
    let mut macros = macros(RUN, "");
    macros.set_commands(allowed(), Approvals::default());
    let why = one_notice(&plan(&macros, &["say hello"], "Discord", &Mode::Normal), "say hello");
    assert!(why.contains("approv"), "{why}");

    let commands = macros.commands();
    assert_eq!(commands.len(), 1);
    let (name, command) = &commands[0];
    assert_eq!(name, "say hello");
    let mut approvals = Approvals::default();
    assert!(!approvals.approves(command));
    approvals.add(command);
    assert!(approvals.approves(command));
    let kept = approvals.to_text();
    assert!(!kept.contains("hello.bat") && !kept.contains("heard"), "kept as hashes: {kept}");
    let approvals = Approvals::parse(&kept);
    assert!(approvals.approves(command), "read back");
    macros.set_commands(allowed(), approvals.clone());
    assert!(is_run(&plan(&macros, &["say hello"], "Discord", &Mode::Normal)));
    assert!(is_run(&plan(&macros, &["say hello"], "Discord", &Mode::Normal)), "approved once, runs every time");

    let changed = [RUN.replace("[\"{heard}\"]", "[\"{heard}\", \"--all\"]"), RUN.replace("hello.bat", "other.bat")];
    for toml in changed {
        let mut changed = common::macros(&toml, "");
        assert_ne!(changed.commands()[0].1.fingerprint(), command.fingerprint());
        changed.set_commands(allowed(), approvals.clone());
        let why = one_notice(&plan(&changed, &["say hello"], "Discord", &Mode::Normal), "say hello");
        assert!(why.contains("approv"), "a changed command needs approval again: {why}");
    }
}

#[test]
fn n4_run_only_from_scripts_unless_anywhere() {
    let fake = Fake::new("");
    let recorder = Recorder::default();
    let ran = events_in(&approved(macros(RUN, ""), allowed()), &["say hello"], &fake, &Mode::Normal, &recorder);
    let [Event::Run(program, _)] = &ran[..] else { panic!("{ran:?}") };
    assert_eq!(program, &fake.folder.join("scripts").join("hello.bat"), "a program of custom\\scripts\\");

    let tool = std::env::temp_dir().join("tool.exe");
    let outside = RUN.replace("\"hello.bat\"", &format!("'{}'", tool.display()));
    let why = one_notice(&plan(&approved(macros(&outside, ""), allowed()), &["say hello"], "Discord", &Mode::Normal), "say hello");
    assert!(why.contains("scripts"), "{why}");
    let anywhere = approved(macros(&outside, ""), CommandPolicy { enabled: true, anywhere: true });
    assert!(is_run(&plan(&anywhere, &["say hello"], "Discord", &Mode::Normal)), "allowed from anywhere");
    let ran = events_in(&anywhere, &["say hello"], &fake, &Mode::Normal, &Recorder::default());
    assert!(matches!(&ran[..], [Event::Run(program, _)] if *program == tool), "{ran:?}");

    let escaping = load(&RUN.replace("hello.bat", "../evil.bat"));
    assert_eq!(escaping.skipped.len(), 1, "a script must stay in the scripts folder: {:?}", escaping.skipped);
    assert!(escaping.skipped[0].0.starts_with("macros.toml:"));
}

#[test]
fn n4_slots_never_become_extra_arguments() {
    let toml = "[[macro]]\nname = \"note\"\nsay = \"note {words}\"\nsteps = [ { run = \"note.bat\", args = [\"{words}\"] } ]\n";
    let macros = approved(macros(toml, ""), allowed());
    let ran = events(&macros, &["note a & b; del x"], &Fake::new(""));
    assert!(matches!(&ran[..], [Event::Run(_, args)] if args == &["a & b; del x"]), "one argument, exactly as said: {ran:?}");
    let quoted = events(&macros, &["note \"a\" b"], &Fake::new(""));
    assert!(matches!(&quoted[..], [Event::Run(_, args)] if args == &["\"a\" b"]), "{quoted:?}");
}

#[test]
fn n4_run_output_is_private() {
    let toml = r#"
[[macro]]
name = "fetch"
say = "fetch {words}"
steps = [ { run = "fetch.bat", args = ["{words}"], output = "variable:result" }, { paste = "got {result}" } ]

[[macro]]
name = "show"
say = "show it"
steps = [ { run = "show.bat", output = "paste", timeout_ms = 500 } ]
"#;
    let macros = approved(macros(toml, ""), allowed());
    let plan = plan(&macros, &["fetch hunter2"], "Discord", &Mode::Normal);
    assert!(!format!("{plan:?}").contains("hunter2"), "{plan:?}");

    let (fake, recorder) = (Fake::new(""), Recorder::default());
    *recorder.output.borrow_mut() = Ok("top secret\r\n".into());
    let kept = run_step(&plan[0], &fake, &recorder);
    assert_eq!(kept.as_ref().map(|(name, value)| (name.as_str(), value.reveal())), Some(("result", "top secret")), "a line break at the end is dropped");
    assert!(!format!("{kept:?}").contains("top secret"));
    let recorder = Recorder::default();
    *recorder.output.borrow_mut() = Ok("top secret\n".into());
    run(&plan, &fake, &recorder);
    assert_eq!(pasted(&recorder.events.take()), ["got top secret"], "a later step uses the output");

    let recorder = Recorder::default();
    *recorder.output.borrow_mut() = Ok("printed\n".into());
    assert_eq!(pasted(&events_in(&macros, &["show it"], &fake, &Mode::Normal, &recorder)), ["printed"]);
    let recorder = Recorder::default();
    *recorder.output.borrow_mut() = Err("timed out after 500 ms".into());
    let failed = events_in(&macros, &["show it"], &fake, &Mode::Normal, &recorder);
    assert!(notices(&failed).iter().any(|why| why.contains("show")), "a failure names the macro: {failed:?}");
    assert!(pasted(&failed).is_empty());
}

#[test]
fn n5_ask_claude_plans_its_filled_text_privately() {
    let macros = macros("[[macro]]\nname = \"ask\"\nsay = \"ask about {words}\"\nsteps = [ { ask_claude = \"Summarise:\\n{words}\" } ]\n", "");
    let plan = plan(&macros, &["ask about rust lifetimes"], "Discord", &Mode::Normal);
    assert!(matches!(plan[..], [Step::AskClaude { .. }]), "{plan:?}");
    assert!(!format!("{plan:?}").contains("lifetimes"), "{plan:?}");
    assert_eq!(events(&macros, &["ask about rust lifetimes"], &Fake::new("")), [Event::AskClaude("Summarise:\nrust lifetimes".into())]);
}

/// A folder of its own under the temporary folder, holding `custom\` with these files.
fn custom(name: &str, macros: &str, variables: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-phase2-{name}-{}", std::process::id())).join("custom");
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("macros.toml"), macros).unwrap();
    fs::write(folder.join("variables.toml"), variables).unwrap();
    folder
}

fn remove(custom: &Path) {
    let _ = fs::remove_dir_all(custom.parent().unwrap());
}

const LISTED: &str = r#"
[[mode]]
name = "coding"

[[macro]]
name = "Paste my email"
say = ["paste my email", "my email"]
apps = ["Discord"]
steps = [ { paste = "{email} secret paste text" }, { press = "enter" } ]

[[macro]]
name = "Token"
say = "use the token"
modes = ["coding"]
hotkey = "Ctrl+Alt+M"
steps = [ { run = "token.bat", args = ["--token=abc123"], output = "paste" } ]

[[macro]]
name = "Resting"
say = "rest now"
enabled = false
listen = true
steps = [ { notify = "resting" } ]
"#;

#[test]
fn n6_list_macros_masks_values() {
    let folder = custom("list", LISTED, VARIABLES);
    let listed = Customization::read(&folder, &Words::default()).list_macros();
    for shown in ["Paste my email", "paste my email", "my email", "Discord", "paste", "press", "Token", "coding", "run", "Ctrl+Alt+M", "Resting", "rest now", "notify"] {
        assert!(listed.contains(shown), "{shown} is listed: {listed}");
    }
    for hidden in ["me@example.com", "secret paste text", "abc123", "--token", "resting\"", "{email}"] {
        assert!(!listed.contains(hidden), "{hidden} isn't: {listed}");
    }
    remove(&folder);
}

#[test]
fn n6_check_reports_file_and_line() {
    let toml = "[[macro]]\nname = \"good\"\nsay = \"good\"\nsteps = [ { paste = \"{name}\" } ]\n\n[[macro]]\nname = \"bad step\"\nsay = \"bad\"\nsteps = [ { fly = \"away\" } ]\n\n[[macro]]\nname = \"bad variable\"\nsay = \"worse\"\nsteps = [ { paste = \"{nobody}\" } ]\n";
    let folder = custom("check", toml, VARIABLES);
    let found = Customization::read(&folder, &Words::default());
    let places: Vec<&str> = found.problems().iter().map(|(place, _)| place.as_str()).collect();
    assert_eq!(places, ["macros.toml:6", "macros.toml:11"], "{:?}", found.problems());
    assert!(found.problems()[1].1.contains("nobody"), "{:?}", found.problems());
    remove(&folder);

    let folder = custom("check-variables", "", "secret = \"hunter2\"\noops\n");
    let found = Customization::read(&folder, &Words::default());
    assert_eq!(found.problems().iter().map(|(place, _)| place.as_str()).collect::<Vec<_>>(), ["variables.toml:2"], "{:?}", found.problems());
    assert!(!format!("{:?}", found.problems()).contains("hunter2"));
    remove(&folder);

    let folder = custom("check-clean", LISTED, VARIABLES);
    assert!(Customization::read(&folder, &Words::default()).problems().is_empty());
    remove(&folder);
}

#[test]
fn n6_try_phrase_masks_private_text() {
    let toml = "[[macro]]\nname = \"email\"\nsay = \"paste email\"\nsteps = [ { paste = \"{email}\" }, { press = \"enter\" } ]\n";
    let folder = custom("try", toml, VARIABLES);
    let found = Customization::read(&folder, &Words::default());
    let tried = found.try_phrase("paste email", "Notepad");
    assert!(tried.contains("paste") && tried.contains("[private]") && tried.contains("press"), "{tried}");
    assert!(tried.contains("email"), "names the macro: {tried}");
    assert!(!tried.contains("me@example.com") && !tried.contains("{email}"), "{tried}");
    assert!(!found.try_phrase("nothing like it", "Notepad").contains("press"));
    remove(&folder);
}

const PALETTE: &str = r#"
[[mode]]
name = "coding"

[[macro]]
name = "a"
say = ["alpha one", "alpha two"]
steps = [ { paste = "a" } ]

[[macro]]
name = "b"
hotkey = "Ctrl+Alt+B"
steps = [ { paste = "b" } ]

[[macro]]
name = "c"
say = "charlie"
apps = ["Discord"]
steps = [ { paste = "c" } ]

[[macro]]
name = "d"
say = "delta"
enabled = false
steps = [ { paste = "d" } ]

[[macro]]
name = "e"
say = "echo"
modes = ["coding"]
steps = [ { paste = "e" } ]

[[macro]]
name = "f"
say = "foxtrot"
steps = [ { paste = "f" } ]
"#;

#[test]
fn n7_palette_lists_what_can_be_said_here() {
    let mut macros = macros(PALETTE, "");
    let names = |app: &str, mode: &Mode| macros_names(&macros, app, mode);
    assert_eq!(names("Notepad", &Mode::Normal), ["a", "b", "f"]);
    assert_eq!(names("Discord", &Mode::Normal), ["a", "b", "c", "f"]);
    assert_eq!(names("Notepad", &coding()), ["a", "b", "e", "f"]);
    assert!(names("Notepad", &Mode::Off).is_empty());
    let listed = macros.available("Notepad", &Mode::Normal);
    assert_eq!(listed[0].phrase.as_deref(), Some("alpha one"), "the first phrase");
    assert_eq!(listed[1].phrase, None);
    assert_eq!(listed[1].hotkey.as_ref().map(|hotkey| hotkey.name()), Some("Ctrl+Alt+B".to_owned()));

    assert_eq!(plan(&macros, &["What can I say?"], "Notepad", &Mode::Normal), [Step::ShowPalette]);
    assert_eq!(plan(&macros, &["what can I say"], "Notepad", &Mode::Off), [Step::ShowPalette], "heard while off too");
    macros.set_phrases(BuiltinPhrases { what_can_i_say: false, ..BuiltinPhrases::default() });
    assert!(!plan(&macros, &["what can I say"], "Notepad", &Mode::Normal).contains(&Step::ShowPalette));
}

fn macros_names(macros: &Macros, app: &str, mode: &Mode) -> Vec<String> {
    macros.available(app, mode).into_iter().map(|available| available.name).collect()
}

#[test]
fn n8_copy_and_type_notices_name_their_macro() {
    let toml = "[[macro]]\nname = \"grab\"\nsay = \"grab it\"\nsteps = [ { copy = \"{clipboard}\" } ]\n\n[[macro]]\nname = \"typist\"\nsay = \"type it\"\nsteps = [ { type = \"{file:missing.txt}\" } ]\n";
    let macros = macros(toml, "");
    let fake = Fake::new("");
    let grabbed = events(&macros, &["grab it"], &fake);
    assert!(!grabbed.iter().any(|event| matches!(event, Event::Copy(_))), "an empty copy leaves the clipboard alone: {grabbed:?}");
    assert!(notices(&grabbed).iter().any(|why| why.contains("grab")), "and says so, naming the macro: {grabbed:?}");
    let typed = events(&macros, &["type it"], &fake);
    assert!(notices(&typed).iter().any(|why| why.contains("typist")), "{typed:?}");
    assert_eq!(events(&common::macros("[[macro]]\nname = \"c\"\nsay = \"copy hi\"\nsteps = [ { copy = \"hi\" } ]\n", ""), &["copy hi"], &fake), [Event::Copy(Some("hi".into()))]);
}
