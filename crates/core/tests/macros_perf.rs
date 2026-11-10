//! Voice macros' performance gates (`.loop/voice-macros/SPINE.md`; `pt1_` is unit M8's), in release mode:
//! `cargo test --release -p kiku-core --test macros_perf -- --nocapture`. The limits are generous so a busy
//! machine passes; each test prints its time for `progress.md`. They are timing gates for release builds only:
//! compiled only there and ignored by default, so debug and CI test runs leave them out; `scripts/macros-gate.sh`
//! runs them with `--ignored`.
#![cfg(not(debug_assertions))]

mod common;

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use common::{said, Fake};
use kiku_core::{
    actions,
    macros::{Macros, Mode, Step, Template, Transcript, Variables},
    words::{WordSettings, Words},
};

const SENTENCES: [&str; 6] = [
    "so the meeting moved to Thursday",
    "because everyone was travelling",
    "and we couldn't agree on anything at all",
    "let's pick it up again next week",
    "can you hear me now",
    "I think the build is green",
];

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

/// A thousand macros: two aliases each, every third with a number slot, every fifth taking the rest of the line.
fn thousand_macros() -> String {
    (0..1000)
        .map(|n| {
            let (a, b) = (word(n), word(n + 5000));
            let say = match n % 15 {
                0 => format!("[\"{a} {b} {{words}}\", \"do {a} with {{words}}\"]"),
                3 | 6 | 9 | 12 => format!("[\"{a} {b} {{n:number}}\", \"{a} line {{n:number}}\"]"),
                _ => format!("[\"{a} {b}\", \"please {a} {b}\"]"),
            };
            let step = if n % 3 != 1 { "{ paste = \"macro text {heard}\" }".to_owned() } else { "{ press = \"ctrl+shift+k\" }".to_owned() };
            format!("[[macro]]\nname = \"m{n}\"\nsay = {say}\nsteps = [ {step} ]\n\n")
        })
        .collect()
}

/// Ten thousand dictated lines; every hundredth says a macro's phrase.
fn dictated() -> Vec<String> {
    (0..10_000)
        .map(|n| match n % 100 {
            0 => format!("{} {}", word(n % 1000), word(n % 1000 + 5000)),
            _ => format!("{} {n}", SENTENCES[n % SENTENCES.len()]),
        })
        .collect()
}

/// The time `work` takes, the best of five runs.
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
fn p1_no_macros_costs_nothing() {
    let words = Words::parse("");
    let macros = Macros::parse("", &words, &Variables::default());
    let lines = dictated();
    let said: Vec<&str> = lines.iter().map(String::as_str).collect();
    let settings = WordSettings::default();
    let took = best_of_five(|| {
        black_box(macros.plan(black_box(&said), "Discord", &settings, &Mode::Normal));
    });
    report("p1_no_macros_costs_nothing", took, Duration::from_millis(5));

    let plan = macros.plan(&said, "Discord", &settings, &Mode::Normal);
    let plain = words.plan(&lines, "Discord", &settings);
    let [Step::Paste { text, .. }] = &plan[..] else { panic!("one paste, got {} steps", plan.len()) };
    let [actions::Step::Paste(expected)] = &plain[..] else { panic!("plain dictation is one paste") };
    assert_eq!(text.fill(&Fake::new("")).unwrap(), *expected);
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p2_plan_against_1000_macros() {
    let macros = Macros::parse(&thousand_macros(), &Words::default(), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    assert_eq!(macros.entries().len(), 1000);
    let lines = dictated();
    let said: Vec<&str> = lines.iter().map(String::as_str).collect();
    let settings = WordSettings::default();
    let took = best_of_five(|| {
        black_box(macros.plan(black_box(&said), "Discord", &settings, &Mode::Normal));
    });
    report("p2_plan_against_1000_macros", took, Duration::from_millis(50));
    let plan = macros.plan(&said, "Discord", &settings, &Mode::Normal);
    assert!(plan.iter().filter(|step| matches!(step, Step::Press(_))).count() >= 30, "the macros said ran");
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p3_index_builds_fast() {
    let toml = thousand_macros();
    let (words, variables) = (Words::default(), Variables::default());
    let took = best_of_five(|| {
        black_box(Macros::parse(black_box(&toml), &words, &variables));
    });
    report("p3_index_builds_fast", took, Duration::from_millis(20));
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p4_transcript_variable_over_a_long_session() {
    let speakers = ["Discord", "You", "Firefox", "Discord"];
    let lines = (0..20_000).map(|n| said(speakers[n % 4], 20_000 - n as i64, SENTENCES[n % SENTENCES.len()])).collect();
    let mut fake = Fake::new("");
    fake.transcript = Transcript::new(lines);
    let template = Template::parse("{transcript:others:5m|names}").unwrap();
    let took = best_of_five(|| {
        black_box(template.fill(black_box(&fake)).unwrap());
    });
    report("p4_transcript_variable_over_a_long_session", took, Duration::from_millis(2));
    let filled = template.fill(&fake).unwrap();
    assert_eq!(filled.reveal().lines().count(), 225, "300 s of lines, a quarter of them yours");
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p5_template_fill() {
    let variables: String = (0..10).map(|n| format!("v{n} = \"value number {n}\"\n")).collect();
    let fake = Fake::new(&variables);
    let filler = "lorem ipsum dolor sit amet ".repeat(7);
    let source: String = (0..10).map(|n| format!("{filler}{{v{n}}}")).collect();
    let source = format!("{source}{}", "x".repeat(2000usize.saturating_sub(source.chars().count())));
    assert!(source.chars().count() >= 2000);
    let template = Template::parse(&source).unwrap();
    let took = best_of_five(|| {
        for _ in 0..10_000 {
            black_box(template.fill(black_box(&fake)).unwrap());
        }
    });
    report("p5_template_fill", took, Duration::from_millis(50));
}

/// The thousand macros with every other one listening (`listen = true`).
fn half_listening() -> String {
    thousand_macros()
        .split_terminator("\n\n")
        .enumerate()
        .map(|(n, table)| if n % 2 == 0 { format!("{table}\nlisten = true\n\n") } else { format!("{table}\n\n") })
        .collect()
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn pt1_listening_against_1000_macros() {
    let macros = Macros::parse(&half_listening(), &Words::default(), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    assert!(macros.listens());
    let lines = dictated();
    let took = best_of_five(|| {
        for line in &lines {
            black_box(macros.plan_heard(black_box(line), "Discord", "", &Mode::Normal));
        }
    });
    report("pt1_listening_against_1000_macros", took, Duration::from_millis(50));
    let ran = lines.iter().filter(|line| macros.plan_heard(line, "Discord", "", &Mode::Normal).is_some()).count();
    assert!(ran >= 30, "the listening macros said ran: {ran}");
}
