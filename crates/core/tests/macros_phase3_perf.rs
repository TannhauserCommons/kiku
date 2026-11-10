//! Voice macros phase 3's performance gates (`.loop/voice-macros-3/SPINE.md`), in release mode:
//! `cargo test --release -p kiku-core --test macros_phase3_perf -- --ignored --nocapture`. Compiled only in
//! release and ignored by default, as the earlier phases'; `scripts/macros3-gate.sh` runs them.
#![cfg(not(debug_assertions))]

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use kiku_core::{
    dictation::{Dictation, DictationKey, DictationSettings},
    macros::{KikuAction, KikuStep, Macros, Mode, Step, Variables},
    words::{WordSettings, Words},
};

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

/// A thousand macros, every tenth a `kiku` step.
fn thousand_macros() -> String {
    (0..1000)
        .map(|n| {
            let step = if n % 10 == 0 { "{ kiku = \"toggle lock\" }" } else { "{ paste = \"macro text {heard}\" }" };
            format!("[[macro]]\nname = \"m{n}\"\nsay = [\"{} {}\"]\nsteps = [ {step} ]\n\n", word(n), word(n + 5000))
        })
        .collect()
}

/// Ten thousand dictated lines: every hundredth a macro's phrase, every fiftieth ending with a stop phrase.
fn dictated() -> Vec<String> {
    (0..10_000)
        .map(|n| match n % 100 {
            0 => format!("{} {}", word(n % 1000), word(n % 1000 + 5000)),
            50 => format!("so that's the plan for {n}, that's all Kiku"),
            _ => format!("so the meeting moved to Thursday {n}"),
        })
        .collect()
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
fn p11_stop_phrases_add_no_cost() {
    let mut macros = Macros::parse(&thousand_macros(), &Words::default(), &Variables::default());
    assert!(macros.skipped.is_empty(), "{:?}", macros.skipped);
    macros.set_stop_phrases(vec!["that's all Kiku".into(), "over and out".into(), "end dictation now".into()]);
    let lines = dictated();
    let said: Vec<&str> = lines.iter().map(String::as_str).collect();
    let settings = WordSettings::default();
    let took = best_of_five(|| {
        black_box(macros.plan(black_box(&said), "Discord", &settings, &Mode::Normal));
    });
    report("p11_stop_phrases_add_no_cost", took, Duration::from_millis(50));
    let stops = macros.plan(&said, "Discord", &settings, &Mode::Normal).into_iter().filter(|step| matches!(step, Step::Kiku(KikuStep { at_once: true, .. }))).count();
    assert_eq!(stops, 100, "each stop phrase stops");
    assert_eq!(macros.plan(&[&format!("{} {}", word(0), word(5000))], "Discord", &settings, &Mode::Normal), [Step::Kiku(KikuStep::new(KikuAction::ToggleLock))]);
}

#[test]
#[ignore = "a timing gate: run in release with --ignored"]
fn p12_dictation_machine_is_cheap() {
    let settings = DictationSettings { key: DictationKey::Toggle, release_delay: Duration::from_millis(500), stop: vec![] };
    let start = Instant::now();
    let took = best_of_five(|| {
        let mut dictation = Dictation::new(&settings);
        for n in 0..1_000_000u64 {
            let at = start + Duration::from_millis(n);
            match n % 4 {
                0 => dictation.key_down(at),
                1 => dictation.key_up(at),
                2 => dictation.chord(at, n % 1000 == 2),
                _ => dictation.tick(at),
            }
            black_box(dictation.next_wake());
        }
        black_box(dictation.dictating());
    });
    report("p12_dictation_machine_is_cheap", took, Duration::from_millis(50));
}
