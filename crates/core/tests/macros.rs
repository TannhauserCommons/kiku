//! Voice macros' core acceptance tests (`.loop/voice-macros/SPINE.md`), on any OS with a fake context. Tests
//! are named by the unit that makes them pass (`t1_` .. `t4_`); the gate runs a unit's and every earlier one's.

mod common;

use std::{fs, path::PathBuf};

use common::{events, letter, macros, paste, press, said, Event, Fake};
use kiku_core::{
    keybinds::Key,
    macros::{Macro, Macros, Mode, Step, StepSpec, Template, Transcript, TranscriptLine, Variables},
    words::{Output, Scope, Transform, WordSettings, Words},
};

/// A value that must never show anywhere but the paste itself.
const CANARY: &str = "s3cr3t-CANARY-77";
const VARIABLES: &str = "name = \"Sam\"\nemail = \"s3cr3t-CANARY-77@example.com\"\nx = \"  hello wide world  \"\ny = \"a b&c/d\"\nz = \"one\\ntwo\\r\\nthree\"\nt = \"hello there, World!\"\n";

/// The spine's four example macros.
const EXAMPLES: &str = r#"# My macros
[[macro]]
name  = "quote him"
say   = ["quote him", "quote what he said"]
apps  = ["Discord"]
steps = [ { paste = "{transcript:Discord:paragraph|quote}" } ]

[[macro]]
name     = "sign off"
say      = "sign off"
anywhere = true
steps    = [ { paste = "Thanks,\n{name}" } ]

[[macro]]
name  = "go to line"
say   = "go to line {n:number}"
steps = [ { press = "ctrl+g" }, { paste = "{n}" }, { press = "enter" } ]

[[macro]]
name  = "meme"
say   = "send the meme"
steps = [ { attach = "D:/Pictures/meme.png" }, { press = "enter" } ]
"#;

const ENTER: Key = Key::Other(0x0D);
const DOWN: Key = Key::Other(0x28);

fn fill(template: &str, fake: &Fake) -> String {
    Template::parse(template).unwrap_or_else(|why| panic!("{template}: {why}")).fill(fake).unwrap().reveal().to_owned()
}

fn fake() -> Fake {
    Fake::new(VARIABLES)
}

/// A folder of its own under the temporary folder, emptied.
fn folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-macros-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn pastes(plan: &[Step]) -> usize {
    plan.iter().filter(|step| matches!(step, Step::Paste { .. })).count()
}

fn notices(events: &[Event]) -> Vec<&str> {
    events.iter().filter_map(|event| if let Event::Notify(message) = event { Some(message.as_str()) } else { None }).collect()
}

// ---- M1: the variable language ----

#[test]
fn t1_text_without_variables_passes_through() {
    let fake = fake();
    assert_eq!(fill("plain text, with: punctuation!", &fake), "plain text, with: punctuation!");
    assert_eq!(fill("{{braces}} and }} alone {{", &fake), "{braces} and } alone {");
    assert_eq!(fill("", &fake), "");
    assert!(Template::parse("{unclosed").is_err());
}

#[test]
fn t1_personal_variables_fill_and_stay_private() {
    let fake = fake();
    let template = Template::parse("mail {email}").unwrap();
    let filled = template.fill(&fake).unwrap();
    assert_eq!(filled.reveal(), format!("mail {CANARY}@example.com"));
    assert!(!format!("{filled:?} {template:?} {:?}", fake.variables).contains(CANARY));
    assert_eq!(template.names(), ["email"], "a name that isn't built in is the file's to check");
    assert!(Template::parse("{date} {transcript:Discord} {clipboard|trim}").unwrap().names().is_empty());
}

#[test]
fn t1_date_and_time_format_from_the_clock() {
    let fake = fake();
    assert_eq!(fill("{date}", &fake), "2026-11-10");
    assert_eq!(fill("{date:%d %B %Y}", &fake), "10 November 2026");
    assert_eq!(fill("{time}", &fake), "14:02");
    assert_eq!(fill("{time:%H:%M}", &fake), "14:02");
    assert_eq!(fill("{weekday}", &fake), "Tuesday");
    assert_eq!(fill("on {weekday} at {time:%H.%M}", &fake), "on Tuesday at 14.02");
}

#[test]
fn t1_filters_chain_left_to_right() {
    let fake = fake();
    assert_eq!(fill("{x|trim|uppercase}", &fake), "HELLO WIDE WORLD");
    assert_eq!(fill("{x|trim|words:2}", &fake), "hello wide");
    assert_eq!(fill("{x|words:2}", &fake), "hello wide");
    assert_eq!(fill("{x|trim|chars:5}", &fake), "hello");
    assert_eq!(fill("{x|trim|words:1|uppercase}", &fake), "HELLO");
    assert_eq!(fill("{x|trim|title}", &fake), "Hello Wide World");
    assert_eq!(fill("{y|url}", &fake), "a%20b%26c%2Fd");
    assert_eq!(fill("{z|oneline}", &fake), "one two three");
    assert_eq!(fill("{z|quote}", &fake), "> one\n> two\n> three");
    assert_eq!(fill("{z|quote|oneline}", &fake), "> one > two > three");
}

#[test]
fn t1_transforms_are_filters() {
    let fake = fake();
    let text = "hello there, World!";
    let settings = WordSettings { transforms: Scope::Both, ..WordSettings::default() };
    for transform in Transform::ALL {
        let words = Words::parse(&format!("! {}\n", transform.name()));
        let expected = words.apply(text, "", Output::Dictation, &settings);
        assert_ne!(expected, "", "{}", transform.name());
        assert_eq!(fill(&format!("{{t|{}}}", transform.name()), &fake), expected, "{}", transform.name());
    }
}

#[test]
fn t1_rules_filter_uses_the_word_rules() {
    let mut fake = fake();
    fake.words = Words::parse("kicku => Kiku\n[Discord]\ngg => good game\n");
    fake.selection = "kicku and gg".into();
    assert_eq!(fill("{selection|rules}", &fake), "Kiku and good game");
    assert_eq!(fill("{selection|rules|uppercase}", &fake), "KIKU AND GOOD GAME");
    fake.app = "Firefox".into();
    assert_eq!(fill("{selection|rules}", &fake), "Kiku and gg", "the rules of the app in front");
}

#[test]
fn t1_unknown_names_fail_at_load() {
    for (template, name) in [("{email|shout}", "shout"), ("{email|words:many}", "many"), ("{date|shout}", "shout"), ("{transcript:Discord:two parsecs}", "two parsecs"), ("{transcript:Discord:3 lines|frobnicate}", "frobnicate"), ("{random}", "random")] {
        let why = Template::parse(template).err().unwrap_or_else(|| panic!("{template} parsed"));
        assert!(why.contains(name), "{why} should name {name}");
    }
    assert_eq!(Template::parse("hi {nope|trim}").unwrap().names(), ["nope"]);
}

// ---- M2: transcript variables ----

/// Discord, you and Firefox, with Discord's last line still being heard.
fn talk() -> Vec<TranscriptLine> {
    let mut lines = vec![
        said("Discord", 125, "so are we meeting"),
        said("You", 95, "yes at three"),
        said("Discord", 65, "great see you then"),
        said("Firefox", 60, "video says subscribe"),
        said("Discord", 15, "wait one more thing"),
        said("Discord", 10, "the room is booked"),
    ];
    lines[5].complete = false;
    lines
}

fn talking() -> Fake {
    let mut fake = fake();
    fake.transcript = Transcript::new(talk());
    fake
}

#[test]
fn t2_last_line_from_an_app() {
    let fake = talking();
    assert_eq!(fill("{transcript:Discord}", &fake), "wait one more thing");
    assert_eq!(fill("{transcript:Discord:line}", &fake), "wait one more thing");
    assert_eq!(fill("{last_line:Discord}", &fake), "wait one more thing");
    assert_eq!(fill("{transcript:discord}", &fake), "wait one more thing", "app names in any case");
}

#[test]
fn t2_who_selects_speakers() {
    let mut fake = talking();
    assert_eq!(fill("{transcript:You}", &fake), "yes at three");
    assert_eq!(fill("{transcript:others}", &fake), "wait one more thing");
    assert_eq!(fill("{transcript:others:3 lines}", &fake), "great see you then\nvideo says subscribe\nwait one more thing");
    assert_eq!(fill("{transcript:everyone:3 lines}", &fake), "great see you then\nvideo says subscribe\nwait one more thing");
    assert_eq!(fill("{transcript:everyone:4 lines}", &fake), "yes at three\ngreat see you then\nvideo says subscribe\nwait one more thing");
    fake.app = "Firefox".into();
    assert_eq!(fill("{transcript:front}", &fake), "video says subscribe");
    assert_eq!(fill("{last_line:front}", &fake), "video says subscribe");
}

#[test]
fn t2_how_much_by_count_time_pause_and_me() {
    let fake = talking();
    assert_eq!(fill("{transcript:Discord:3 lines}", &fake), "so are we meeting\ngreat see you then\nwait one more thing");
    assert_eq!(fill("{transcript:everyone:2m}", &fake), "yes at three\ngreat see you then\nvideo says subscribe\nwait one more thing");
    assert_eq!(fill("{last_minute:everyone}", &fake), "video says subscribe\nwait one more thing");
    assert_eq!(fill("{transcript:everyone:1m}", &fake), fill("{last_minute:everyone}", &fake));
    assert_eq!(fill("{transcript:everyone:90s}", &fake), "great see you then\nvideo says subscribe\nwait one more thing");
    assert_eq!(fill("{transcript:everyone:since me}", &fake), "great see you then\nvideo says subscribe\nwait one more thing");
    assert_eq!(fill("{transcript:Discord:since me}", &fake), "great see you then\nwait one more thing");
    assert_eq!(fill("{transcript:Discord:session}", &fake), "so are we meeting\ngreat see you then\nwait one more thing");
    assert_eq!(fill("{transcript:everyone:session}", &fake).lines().count(), 5);

    // A pause of 3 s or more starts a paragraph; 2 s doesn't.
    let mut fake = fake;
    fake.transcript = Transcript::new(vec![said("Discord", 30, "alpha"), said("Discord", 28, "beta"), said("Discord", 25, "gamma"), said("Discord", 24, "delta")]);
    assert_eq!(fill("{transcript:Discord:paragraph}", &fake), "gamma\ndelta");
    fake.transcript = Transcript::new(vec![said("Discord", 30, "alpha"), said("Discord", 28, "beta")]);
    assert_eq!(fill("{transcript:Discord:paragraph}", &fake), "alpha\nbeta");
}

#[test]
fn t2_names_and_times_filters() {
    let fake = talking();
    assert_eq!(fill("{transcript:others:2 lines|names}", &fake), "Firefox: video says subscribe\nDiscord: wait one more thing");
    assert_eq!(fill("{transcript:everyone:since me|names|quote}", &fake), "> Discord: great see you then\n> Firefox: video says subscribe\n> Discord: wait one more thing");
    assert_eq!(fill("{transcript:Discord:3 lines|times}", &fake), "[14:00] so are we meeting\n[14:01] great see you then\n[14:01] wait one more thing");
    assert_eq!(fill("{transcript:You|names|times}", &fake), "[14:00] You: yes at three");
}

#[test]
fn t2_nothing_said_is_empty_and_skipped() {
    let fake = talking();
    assert_eq!(fill("{transcript:Zoom}", &fake), "");
    assert_eq!(fill("{transcript:Zoom:5m|names|quote}", &fake), "");
    assert_eq!(fill("{clipboard}", &fake), "");
    assert_eq!(fill("{selection|trim|names}", &fake), "");
}

// ---- M3: the files and the index ----

#[test]
fn t3_macros_toml_loads_with_aliases_slots_and_apps() {
    let macros = macros(EXAMPLES, VARIABLES);
    let names: Vec<&str> = macros.entries().iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["quote him", "sign off", "go to line", "meme"]);
    let quote = &macros.entries()[0];
    assert_eq!((quote.say.clone(), quote.apps.clone(), quote.anywhere, quote.enabled), (vec!["quote him".to_owned(), "quote what he said".to_owned()], vec!["Discord".to_owned()], false, true));
    assert!(macros.entries()[1].anywhere);
    assert_eq!(macros.entries()[3].steps, [StepSpec::Attach(vec!["D:/Pictures/meme.png".into()]), StepSpec::Press("enter".into())]);

    let mut fake = fake();
    fake.transcript = Transcript::new(vec![said("Discord", 30, "first"), said("Discord", 20, "we should"), said("Discord", 18, "ship it")]);
    for alias in ["quote him", "Quote what he said."] {
        assert_eq!(events(&macros, &[alias], &fake), [paste("> we should\n> ship it")], "{alias}");
    }
    fake.app = "discord".into();
    assert_eq!(events(&macros, &["quote him"], &fake), [paste("> we should\n> ship it")], "apps match in any case");
    fake.app = "Firefox".into();
    assert_eq!(events(&macros, &["quote him"], &fake), [paste("quote him ")], "only in Discord");
    assert_eq!(events(&macros, &["go to line 42"], &fake), [press(&[&[Key::Ctrl, letter('G')]]), paste("42"), press(&[&[ENTER]])]);
    let plan = macros.plan(&["send the meme"], "Firefox", &WordSettings::default(), &Mode::Normal);
    assert_eq!(plan.len(), 2);
    assert!(matches!(&plan[0], Step::Attach { files, from } if files == &[PathBuf::from("D:/Pictures/meme.png")] && from == "meme"));

    let private = common::macros(r#"[[macro]]
name = "paste email"
say = "paste email"
steps = [ { paste = "{email}" }, { copy = "{email}" }, { type = "{email}" }, { notify = "pasted" } ]
"#, VARIABLES);
    let plan = private.plan(&["paste email"], "Discord", &WordSettings::default(), &Mode::Normal);
    assert_eq!(plan.len(), 4);
    assert!(!format!("{plan:?} {private:?} {:?}", private.entries()).contains(CANARY), "no value in Debug output");

    let off = macros_off();
    assert_eq!(off.entries().len(), 1, "a macro that's off is still listed");
    assert_eq!(events(&off, &["select everything"], &fake), [paste("select everything ")]);
}

#[test]
fn t3_unknown_names_skip_their_macro_with_its_line() {
    let lines = [
        "[[macro]]",
        "name = \"good\"",
        "say = \"good one\"",
        "steps = [ { paste = \"{email}\" } ]",
        "",
        "[[macro]]",
        "name = \"bad variable\"",
        "say = \"bad variable\"",
        "steps = [ { paste = \"hi {nope}\" } ]",
        "",
        "[[macro]]",
        "name = \"bad filter\"",
        "say = \"bad filter\"",
        "steps = [ { paste = \"{email|shout}\" } ]",
        "",
        "[[macro]]",
        "name = \"bad argument\"",
        "say = \"bad argument\"",
        "steps = [ { paste = \"{email|words:many}\" } ]",
        "",
        "[[macro]]",
        "name = \"also good\"",
        "say = \"also good\"",
        "steps = [ { press = \"ctrl+a\" } ]",
    ];
    let macros = Macros::parse(&lines.join("\n"), &Words::default(), &Variables::parse(VARIABLES).unwrap());
    let names: Vec<&str> = macros.entries().iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["good", "also good"]);
    let places: Vec<&str> = macros.skipped.iter().map(|(place, _)| place.as_str()).collect();
    assert_eq!(places, ["macros.toml:6", "macros.toml:11", "macros.toml:16"], "{:?}", macros.skipped);
    for ((_, why), name) in macros.skipped.iter().zip(["nope", "shout", "many"]) {
        assert!(why.contains(name), "{why} should name {name}");
        assert!(!why.contains(CANARY), "a reason holds a value: {why}");
    }
    let fake = fake();
    assert_eq!(events(&macros, &["good one"], &fake), [paste(&format!("{CANARY}@example.com"))]);
}

fn macros_off() -> Macros {
    macros("[[macro]]\nname = \"all\"\nsay = \"select everything\"\nenabled = false\nsteps = [ { press = \"ctrl+a\" } ]\n", VARIABLES)
}

#[test]
fn t3_words_quick_lines_are_one_step_macros() {
    let variables = Variables::parse(VARIABLES).unwrap();
    let quick = Macros::parse("", &Words::parse("> paste email => paste me@example.com\n> select all => press ctrl+a\n[Discord]\n> paste email => paste discord@example.com\n"), &variables);
    assert!(quick.skipped.is_empty(), "{:?}", quick.skipped);
    assert!(quick.entries().is_empty(), "entries are macros.toml's");
    let long = macros(r#"[[macro]]
name = "paste email"
say = "paste email"
anywhere = true
steps = [ { paste = "me@example.com" } ]

[[macro]]
name = "select all"
say = "select all"
steps = [ { press = "ctrl+a" } ]

[[macro]]
name = "paste email in Discord"
say = "paste email"
apps = ["Discord"]
anywhere = true
steps = [ { paste = "discord@example.com" } ]
"#, VARIABLES);
    let mut fake = fake();
    for app in ["Discord", "Firefox"] {
        fake.app = app.into();
        let batches: [&[&str]; 3] = [&["paste email"], &["select all"], &["my email is paste email", "select all", "thanks"]];
        for said in batches {
            assert_eq!(events(&quick, said, &fake), events(&long, said, &fake), "{said:?} in {app}");
        }
    }
    assert_eq!(events(&quick, &["select all"], &fake), [press(&[&[Key::Ctrl, letter('A')]])]);
    assert_eq!(events(&quick, &["my email is paste email"], &fake), [paste("my email is me@example.com ")]);
    let braces = Macros::parse("", &Words::parse("> braces => paste {email}\n"), &variables);
    assert_eq!(events(&braces, &["braces"], &fake), [paste("{email}")], "a > line's text is pasted as written");
}

#[test]
fn t3_slots_take_words_and_numbers() {
    let macros = macros(r#"[[macro]]
name = "search"
say = ["search for {words}", "look up {words}"]
steps = [ { press = "ctrl+l" }, { paste = "https://duckduckgo.com/?q={words|url}" }, { press = "enter" } ]

[[macro]]
name = "go to line"
say = "go to line {n:number}"
steps = [ { paste = "line {n}" } ]
"#, VARIABLES);
    let fake = fake();
    let search = |q: &str| [press(&[&[Key::Ctrl, letter('L')]]), paste(&format!("https://duckduckgo.com/?q={q}")), press(&[&[ENTER]])];
    assert_eq!(events(&macros, &["search for cute cats"], &fake), search("cute%20cats"));
    assert_eq!(events(&macros, &["Search for cute cats."], &fake), search("cute%20cats"), "case and the last full stop aside");
    assert_eq!(events(&macros, &["look up rust"], &fake), search("rust"));
    for (said, line) in [("go to line three", "3"), ("go to line 3", "3"), ("Go to line twenty.", "20"), ("go to line 42", "42")] {
        assert_eq!(events(&macros, &[said], &fake), [paste(&format!("line {line}"))], "{said}");
    }
    assert_eq!(events(&macros, &["go to line banana"], &fake), [paste("go to line banana ")]);
    assert_eq!(events(&macros, &["search for"], &fake), [paste("search for ")], "a slot takes at least one word");

    let echo = common::macros("[[macro]]\nname = \"echo\"\nsay = \"echo {words}\"\nsteps = [ { paste = \"{heard|rules}\" } ]\n", VARIABLES);
    let mut fake = fake;
    fake.words = Words::parse("kicku => Kiku\n[Discord]\ngg => good game\n");
    assert_eq!(events(&echo, &["echo kicku and gg"], &fake), [paste("echo Kiku and good game")], "{{heard}} is the line said");
    fake.app = "Firefox".into();
    assert_eq!(events(&echo, &["echo kicku and gg"], &fake), [paste("echo Kiku and gg")]);
}

#[test]
fn t3_whole_line_unless_anywhere() {
    let toml = r#"[[macro]]
name = "new tab"
say = "new tab"
steps = [ { press = "ctrl+t" } ]

[[macro]]
name = "sign off"
say = "sign off"
anywhere = true
steps = [ { paste = "Thanks,\n{name}" } ]

[[macro]]
name = "greet"
say = "greet"
steps = [ { paste = "Hello!" } ]
"#;
    let macros = macros(toml, VARIABLES);
    let fake = fake();
    assert_eq!(events(&macros, &["please open a new tab now"], &fake), [paste("please open a new tab now ")]);
    assert_eq!(events(&macros, &["New tab."], &fake), [press(&[&[Key::Ctrl, letter('T')]])]);
    assert_eq!(events(&macros, &["thanks sign off bye"], &fake), [paste("thanks Thanks,\nSam bye ")]);
    assert_eq!(events(&macros, &["say greet please"], &fake), [paste("say greet please ")]);
    assert_eq!(events(&macros, &["greet"], &fake), [paste("Hello!")]);

    let bad = Macros::parse(
        "[[macro]]\nname = \"bad\"\nsay = \"bad\"\nanywhere = true\nsteps = [ { paste = \"x\" }, { press = \"enter\" } ]\n",
        &Words::default(),
        &Variables::parse(VARIABLES).unwrap(),
    );
    assert!(bad.entries().is_empty());
    assert_eq!(bad.skipped.len(), 1);
    assert!(bad.skipped[0].1.contains("anywhere"), "{:?}", bad.skipped);
}

#[test]
fn t3_editing_keeps_comments_and_order() {
    let original = "# My macros, kept as written\n\n[[macro]]\nname = \"one\"   # the first\nsay = \"first thing\"\nsteps = [ { paste = \"1\" } ]\n\n# between them\n[[macro]]\nname = \"two\"\nsay = \"second thing\"\nsteps = [ { press = \"ctrl+b\" } ]\n";
    let mut macros = macros(original, VARIABLES);
    let added = Macro {
        name: "three".into(),
        say: vec!["third thing".into(), "number three".into()],
        apps: vec!["Discord".into()],
        anywhere: false,
        enabled: true,
        hotkey: None,
        listen: false,
        modes: vec![],
        steps: vec![StepSpec::Paste("Hi {name}\nbye".into()), StepSpec::Press("Ctrl+A".into())],
    };
    macros.add(&added).unwrap();
    let text = macros.to_text();
    assert!(text.starts_with(original.trim_end()), "comments and order kept:\n{text}");
    assert_eq!(macros.entries().last(), Some(&added));
    let reread = common::macros(&text, VARIABLES);
    assert_eq!(reread.entries(), macros.entries(), "what was written reads back the same");
    assert_eq!(events(&macros, &["number three"], &fake()), [paste("Hi Sam\nbye"), press(&[&[Key::Ctrl, letter('A')]])]);

    let bad = Macro { name: "four".into(), say: vec!["four".into()], apps: vec![], anywhere: false, enabled: true, hotkey: None, listen: false, modes: vec![], steps: vec![StepSpec::Paste("{nope}".into())] };
    assert!(macros.add(&bad).unwrap_err().contains("nope"));
    assert!(macros.add(&Macro { say: vec!["other words".into()], ..added.clone() }).is_err(), "a name that's taken");
    assert_eq!(macros.to_text(), text, "a refused macro changes nothing");

    assert!(macros.remove("three"));
    assert!(!macros.remove("three"));
    assert_eq!(macros.to_text(), original);
    assert!(macros.remove("one"));
    assert!(macros.to_text().contains("# between them"));
    assert_eq!(macros.entries().iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), ["two"]);
}

// ---- M4: planning and running ----

#[test]
fn t4_adjacent_text_is_one_paste() {
    let long = "x".repeat(5000);
    let toml = format!("[[macro]]\nname = \"sign off\"\nsay = \"sign off\"\nanywhere = true\nsteps = [ {{ paste = \"Thanks,\\n{{name}}\" }} ]\n\n[[macro]]\nname = \"long\"\nsay = \"long one\"\nsteps = [ {{ paste = \"{long}\" }} ]\n\n[[macro]]\nname = \"typed\"\nsay = \"type it\"\nsteps = [ {{ type = \"hi\" }} ]\n");
    let macros = macros(&toml, VARIABLES);
    let fake = fake();
    let settings = WordSettings::default();
    let plan = macros.plan(&["hello", "sign off", "bye"], "Discord", &settings, &Mode::Normal);
    assert_eq!(pastes(&plan), 1, "{plan:?}");
    assert!(!plan.iter().any(|step| matches!(step, Step::Type { .. })));
    assert_eq!(events(&macros, &["hello", "sign off", "bye"], &fake), [paste("hello Thanks,\nSam bye ")]);
    assert_eq!(events(&macros, &["hello sign off", "and more"], &fake), [paste("hello Thanks,\nSam and more ")]);

    let plan = macros.plan(&["long one"], "Discord", &settings, &Mode::Normal);
    assert_eq!((plan.len(), pastes(&plan)), (1, 1));
    assert_eq!(events(&macros, &["long one"], &fake), [paste(&long)]);

    let plan = macros.plan(&["type it"], "Discord", &settings, &Mode::Normal);
    assert!(matches!(plan[..], [Step::Type { .. }]), "a type step only where written: {plan:?}");
    assert_eq!(events(&macros, &["type it"], &fake), [Event::Type("hi".into())]);
}

#[test]
fn t4_a_batch_runs_in_order() {
    let macros = macros("[[macro]]\nname = \"new tab\"\nsay = \"new tab\"\nsteps = [ { press = \"ctrl+t\" }, { wait = 200 }, { notify = \"opened\" } ]\n", VARIABLES);
    let fake = fake();
    assert_eq!(
        events(&macros, &["hello", "new tab", "bye"], &fake),
        [paste("hello "), press(&[&[Key::Ctrl, letter('T')]]), Event::Wait(std::time::Duration::from_millis(200)), Event::Notify("opened".into()), paste("bye ")]
    );
    let copies = common::macros("[[macro]]\nname = \"c\"\nsay = \"copy it\"\nsteps = [ { copy = true }, { copy = \"{name}\" } ]\n", VARIABLES);
    assert_eq!(events(&copies, &["copy it"], &fake), [Event::Copy(None), Event::Copy(Some("Sam".into()))]);

    let empty = common::macros(r#"[[macro]]
name = "what zoom said"
say = "paste zoom"
steps = [ { paste = "{transcript:Zoom}" }, { press = "enter" } ]
"#, VARIABLES);
    let done = events(&empty, &["paste zoom"], &fake);
    assert!(!done.iter().any(|event| matches!(event, Event::Paste(_))), "an empty paste is skipped: {done:?}");
    assert!(done.contains(&press(&[&[ENTER]])), "the other steps still run: {done:?}");
    let told = notices(&done);
    assert_eq!(told.len(), 1, "{done:?}");
    assert!(told[0].contains("what zoom said"), "{}", told[0]);
}

#[test]
fn t4_paste_file_reads_text_with_variables() {
    let dir = folder("paste-file");
    fs::write(dir.join("note.txt"), "Hi {name}, it's {weekday}.\n").unwrap();
    fs::write(dir.join("big.txt"), "a".repeat((1 << 20) + 1)).unwrap();
    fs::write(dir.join("binary.bin"), [0x89, b'P', b'N', b'G', 0, 0, 0xFF, 0x00, 0x9F]).unwrap();
    let wide: Vec<u8> = [0xFF, 0xFE].into_iter().chain("héllo".encode_utf16().flat_map(u16::to_le_bytes)).collect();
    fs::write(dir.join("wide.txt"), wide).unwrap();
    let step = |name: &str, file: &str, raw: bool| format!("[[macro]]\nname = \"{name}\"\nsay = \"{name}\"\nsteps = [ {{ paste_file = \"{file}\", raw = {raw} }} ]\n\n");
    let toml: String = [step("note", "note.txt", false), step("raw note", "note.txt", true), step("big", "big.txt", false), step("binary", "binary.bin", false), step("gone", "gone.txt", false), step("wide", "wide.txt", false)].concat();
    let macros = macros(&toml, VARIABLES);
    let mut fake = fake();
    fake.folder = dir.clone();
    assert_eq!(events(&macros, &["note"], &fake), [paste("Hi Sam, it's Tuesday.\n")]);
    assert_eq!(events(&macros, &["raw note"], &fake), [paste("Hi {name}, it's {weekday}.\n")]);
    assert_eq!(events(&macros, &["wide"], &fake), [paste("héllo")]);
    for (said, why) in [("big", "1 MiB"), ("binary", "use attach"), ("gone", "gone.txt")] {
        let done = events(&macros, &[said], &fake);
        assert_eq!(notices(&done).len(), 1, "{said}: {done:?}");
        assert!(notices(&done)[0].contains(why), "{said}: {done:?}");
        assert!(!done.iter().any(|event| matches!(event, Event::Paste(_))), "{said}: {done:?}");
    }
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn t4_attach_lists_files() {
    let dir = folder("attach");
    for name in ["a.png", "b.pdf"] {
        fs::write(dir.join(name), b"file").unwrap();
    }
    let macros = macros("[[macro]]\nname = \"send\"\nsay = \"send them\"\nsteps = [ { attach = [\"a.png\", \"b.pdf\"] }, { press = \"enter\" } ]\n\n[[macro]]\nname = \"missing\"\nsay = \"send missing\"\nsteps = [ { attach = [\"a.png\", \"nowhere.png\"] } ]\n", VARIABLES);
    let mut fake = fake();
    fake.folder = dir.clone();
    let plan = macros.plan(&["send them"], "Discord", &WordSettings::default(), &Mode::Normal);
    assert!(matches!(&plan[0], Step::Attach { files, .. } if files == &[PathBuf::from("a.png"), PathBuf::from("b.pdf")]), "{plan:?}");
    assert_eq!(events(&macros, &["send them"], &fake), [Event::Files(vec![dir.join("a.png"), dir.join("b.pdf")]), press(&[&[ENTER]])]);
    let done = events(&macros, &["send missing"], &fake);
    assert!(!done.iter().any(|event| matches!(event, Event::Files(_))), "{done:?}");
    assert!(notices(&done).first().is_some_and(|told| told.contains("nowhere.png")), "{done:?}");

    let many: Vec<String> = (0..21).map(|n| format!("\"{n}.png\"")).collect();
    let toml = format!("[[macro]]\nname = \"many\"\nsay = \"many\"\nsteps = [ {{ attach = [{}] }} ]\n", many.join(", "));
    let refused = Macros::parse(&toml, &Words::default(), &Variables::default());
    assert!(refused.entries().is_empty());
    assert!(refused.skipped.first().is_some_and(|(_, why)| why.contains("20")), "{:?}", refused.skipped);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn t4_press_sequences_and_repeats() {
    let macros = macros(r#"[[macro]]
name = "comment"
say = "comment that"
steps = [ { press = "ctrl+k, ctrl+c" } ]

[[macro]]
name = "down three"
say = "down three lines"
steps = [ { press = "down ×3" } ]

[[macro]]
name = "down n"
say = "down {n:number}"
steps = [ { press = "down ×{n}, enter" } ]
"#, VARIABLES);
    let fake = fake();
    assert_eq!(events(&macros, &["comment that"], &fake), [press(&[&[Key::Ctrl, letter('K')], &[Key::Ctrl, letter('C')]])]);
    assert_eq!(events(&macros, &["down three lines"], &fake), [press(&[&[DOWN], &[DOWN], &[DOWN]])]);
    assert_eq!(events(&macros, &["down four"], &fake), [press(&[&[DOWN], &[DOWN], &[DOWN], &[DOWN], &[ENTER]])]);
    assert_eq!(events(&macros, &["down 2"], &fake), [press(&[&[DOWN], &[DOWN], &[ENTER]])]);

    for bad in ["ctrl+nonsense", "win+l", "down ×", "down ×{m}"] {
        let toml = format!("[[macro]]\nname = \"bad\"\nsay = \"bad {{n:number}}\"\nsteps = [ {{ press = \"{bad}\" }} ]\n");
        let refused = Macros::parse(&toml, &Words::default(), &Variables::default());
        assert_eq!((refused.entries().len(), refused.skipped.len()), (0, 1), "{bad}");
    }
}
