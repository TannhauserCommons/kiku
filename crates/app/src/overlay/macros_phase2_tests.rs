//! Voice macros, phase 2, on Windows (`.loop/voice-macros-2/SPINE.md`), run by `voice-ci`: `open` and `switch`
//! through the executor, a real script run with its arguments exactly as said, undo after a paste then a wait
//! (F19), and a copy of the selection waited for before the next step (F21).

use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use kiku_core::{
    actions::Private,
    keybinds::Key,
    macros::{run, Macros, Mode, Step, Template, Variables},
    words::{WordSettings, Words},
};

use super::{
    macro_run::{await_copy, launch, run_program},
    macros_tests::{Done, Fake, Recorder},
    undo_after,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn ran(toml: &str, said: &[&str]) -> Vec<Done> {
    let macros = Macros::parse(toml, &Words::default(), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    let recorder = Recorder::default();
    run(&macros.plan(said, "Notepad", &WordSettings::default(), &Mode::Normal), &Fake, &recorder);
    recorder.done.take()
}

#[test]
fn w_open_and_switch_go_through_the_executor() {
    let toml = r#"
[[macro]]
name = "search"
say = "search for {words}"
steps = [ { open = "https://duckduckgo.com/?q={words|url}" } ]

[[macro]]
name = "go to"
say = "go to {words}"
steps = [ { switch = "{words}" } ]
"#;
    assert_eq!(ran(toml, &["search for rust lifetimes"]), [Done::Open("https://duckduckgo.com/?q=rust%20lifetimes".into())]);
    assert_eq!(ran(toml, &["go to Discord"]), [Done::Switch("Discord".into())]);
}

/// A folder of its own under the temporary folder, emptied, with `scripts\` in it.
fn scripts(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-run-{name}-{}", std::process::id())).join("scripts");
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn script(folder: &Path, name: &str, text: &str) -> PathBuf {
    let path = folder.join(name);
    fs::write(&path, text.replace('\n', "\r\n")).unwrap();
    path
}

fn private(texts: &[&str]) -> Vec<Private> {
    texts.iter().map(|text| Private::new((*text).to_owned())).collect()
}

#[test]
fn w_run_starts_the_program_with_exact_arguments() {
    let folder = scripts("exact");
    // Delayed expansion prints the arguments without cmd reading them again, so `&` stays text.
    let echo = script(&folder, "echo.bat", "@echo off\nsetlocal EnableDelayedExpansion\nset \"one=%~1\"\nset \"two=%~2\"\necho(!one!,!two!\n");
    let printed = run_program(&echo, &private(&["a & b", "c"]), Duration::from_secs(10)).expect("the script runs");
    assert_eq!(printed.reveal().trim_end(), "a & b,c", "each argument exactly as given");
    let printed = run_program(&echo, &private(&["one two; del x", ""]), Duration::from_secs(10)).unwrap();
    assert_eq!(printed.reveal().trim_end(), "one two; del x,", "{:?}", printed.reveal());

    let started = launch(&echo, &private(&["a & b"])).unwrap();
    assert_ne!(started.flags & CREATE_NO_WINDOW, 0, "no window: {started:?}");
    let ps1 = launch(&folder.join("tool.ps1"), &private(&["x"])).unwrap();
    assert!(ps1.exe.to_string_lossy().to_lowercase().ends_with("powershell.exe"), "{ps1:?}");
    let ps1_args: Vec<String> = ps1.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
    assert_eq!(ps1_args[..4], ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"], "{ps1:?}");
    assert_eq!(ps1_args[4..], [folder.join("tool.ps1").display().to_string(), "x".into()]);
    let py = launch(&folder.join("tool.py"), &private(&["x"])).unwrap();
    assert!(py.exe.to_string_lossy().to_lowercase().ends_with("py.exe") || py.exe == Path::new("py"), "{py:?}");
    let exe = launch(&folder.join("tool.exe"), &private(&["x y"])).unwrap();
    assert_eq!((exe.exe.as_path(), exe.args.len()), (folder.join("tool.exe").as_path(), 1), "{exe:?}");
    assert!(launch(&folder.join("notes.txt"), &[]).is_err(), "not a program");

    let slow = script(&folder, "slow.bat", "@echo off\nping -n 8 127.0.0.1 > nul\necho done\n");
    let start = Instant::now();
    let ended = run_program(&slow, &[], Duration::from_millis(500));
    assert!(ended.is_err(), "ended at its timeout: {:?}", ended.map(|out| out.reveal().to_owned()));
    assert!(start.elapsed() < Duration::from_secs(4), "not waited out: {:?}", start.elapsed());
    let _ = fs::remove_dir_all(folder.parent().unwrap());
}

#[test]
fn w_undo_after_paste_then_wait_takes_the_paste_back() {
    let paste = Step::Paste { text: Template::literal("hello"), from: Some("greet".into()) };
    let pasted = undo_after(None, &paste, Some(5));
    assert_eq!(pasted, Some(5));
    let kept = [Step::Wait(Duration::from_millis(200)), Step::Notify("done".into()), Step::SetMode(Mode::Commands)];
    for step in &kept {
        assert_eq!(undo_after(pasted, step, None), Some(5), "{step:?} leaves the paste to undo");
    }
    assert_eq!(undo_after(pasted, &Step::Press(vec![vec![Key::Other(0x0D)]]), None), None, "a key press changes what undo would take back");
    assert_eq!(undo_after(pasted, &paste, Some(3)), Some(3), "the newest paste");
    assert_eq!(undo_after(pasted, &paste, None), None, "a paste that failed");
}

#[test]
fn w_copy_never_races_a_paste() {
    let (asked, slept) = (Cell::new(0u32), Cell::new(0u32));
    let copies_later = || {
        asked.set(asked.get() + 1);
        if asked.get() > 3 { 8 } else { 7 }
    };
    assert!(await_copy(7, &copies_later, &|_| slept.set(slept.get() + 1)), "the app's copy is waited for");
    assert!(slept.get() >= 2, "{} sleeps", slept.get());
    let slept = Cell::new(0u32);
    assert!(!await_copy(7, &|| 7, &|_| slept.set(slept.get() + 1)), "an app that never copies");
    assert!((1..=100).contains(&slept.get()), "gives up: {} sleeps", slept.get());

    let toml = "[[macro]]\nname = \"grab\"\nsay = \"grab it\"\nsteps = [ { copy = true }, { paste = \"pasted\" } ]\n";
    assert_eq!(ran(toml, &["grab it"]), [Done::Copy, Done::PasteText("pasted".into())], "in order");
}
