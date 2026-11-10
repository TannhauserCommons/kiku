//! Voice macros phase 2's performance gates (`.loop/voice-macros-2/SPINE.md`), in release mode:
//! `cargo test --release -p kiku-core --test macros_phase2_perf -- --ignored --nocapture`. Compiled only in
//! release and ignored by default, as phase 1's; `scripts/macros2-gate.sh` runs them.
#![cfg(not(debug_assertions))]

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use kiku_core::macros::{Approvals, Macros, Mode, Step, Variables};
use kiku_core::words::{WordSettings, Words};

/// A word of letters only, different for each `n`.
fn word(mut n: usize) -> String {
    let mut word = String::from("q");
    loop {
        word.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
        if n == 0 {
            return word;
        }
    }
}

/// Four modes of the user's, and a thousand macros: each fifth for every mode, the rest for one mode each;
/// every tenth runs a program, every seventh is only for Discord.
fn thousand_macros() -> String {
    let modes: String = (1..=4).map(|n| format!("[[mode]]\nname = \"mode{n}\"\n\n")).collect();
    let macros: String = (0..1000)
        .map(|n| {
            let (a, b) = (word(n), word(n + 5000));
            let modes = if n % 5 == 0 { String::new() } else { format!("modes = [\"mode{}\"]\n", n % 5) };
            let apps = if n % 7 == 0 { "apps = [\"Discord\"]\n" } else { "" };
            let step = if n % 10 == 3 { format!("{{ run = \"s{n}.bat\", args = [\"{{heard}}\"] }}") } else { "{ paste = \"macro text {heard}\" }".to_owned() };
            format!("[[macro]]\nname = \"m{n}\"\nsay = [\"{a} {b}\", \"please {a} {b}\"]\n{modes}{apps}steps = [ {step} ]\n\n")
        })
        .collect();
    modes + &macros
}

/// Ten thousand dictated lines; every hundredth says a macro's phrase.
fn dictated() -> Vec<String> {
    (0..10_000)
        .map(|n| match n % 100 {
            0 => format!("{} {}", word(n % 1000), word(n % 1000 + 5000)),
            _ => format!("so the meeting moved to Thursday {n}"),
        })
        .collect()
}

fn loaded() -> Macros {
    let macros = Macros::parse(&thousand_macros(), &Words::default(), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    assert_eq!(macros.entries().len(), 1000);
    macros
}

fn best_of_five(mut work: impl FnMut()) -> Duration {
    (0..5)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed()
        })
        .min()
        .unwrap()
}

fn report(name: &str, took: Duration, limit: Duration) {
    println!("{name}: {:.3} ms (limit {} ms)", took.as_secs_f64() * 1000.0, limit.as_millis());
    assert!(took < limit, "{name} took {took:?}, over {limit:?}");
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p6_modes_add_no_cost() {
    let macros = loaded();
    let lines = dictated();
    let said: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (settings, mode) = (WordSettings::default(), Mode::Own("mode2".into()));
    let took = best_of_five(|| {
        black_box(macros.plan(black_box(&said), "Discord", &settings, &mode));
    });
    report("p6_modes_add_no_cost", took, Duration::from_millis(50));
    let from = |line: &str| match &macros.plan(&[line], "Discord", &settings, &mode)[..] {
        [Step::Paste { from, .. }] => from.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(from(&format!("{} {}", word(0), word(5000))), Some("m0".into()), "a macro of every mode runs in mode2");
    assert_eq!(from(&format!("{} {}", word(2), word(5002))), Some("m2".into()), "and one of mode2");
    assert_eq!(from(&format!("{} {}", word(1), word(5001))), None, "one of mode1 is words");
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p7_available_is_fast() {
    let macros = loaded();
    let mode = Mode::Own("mode3".into());
    let took = best_of_five(|| {
        black_box(macros.available(black_box("Discord"), &mode));
    });
    report("p7_available_is_fast", took, Duration::from_millis(1));
    assert_eq!(macros.available("Discord", &mode).len(), 400, "every fifth for every mode, and mode3's");
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p8_approval_check_is_fast() {
    let macros = loaded();
    let commands = macros.commands();
    assert_eq!(commands.len(), 100);
    let mut approvals = Approvals::default();
    for (_, command) in commands.iter().step_by(2) {
        approvals.add(command);
    }
    let took = best_of_five(|| {
        for round in 0..100 {
            for (_, command) in &commands {
                black_box(approvals.approves(black_box(command)) ^ (round == 0));
            }
        }
    });
    report("p8_approval_check_is_fast", took, Duration::from_millis(5));
    assert_eq!(commands.iter().filter(|(_, command)| approvals.approves(command)).count(), 50);
}
