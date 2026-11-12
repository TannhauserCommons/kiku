//! Kiku cuts speech that never pauses (people talking over each other) into phrases itself: each stretch of audio
//! is transcribed once, in order, and none is lost. Needs Parakeet Unified and the voices fixtures (`#[ignore]`d).

use std::{fs, path::PathBuf};

use kiku_core::{
    catalog,
    engine::{Backend, Device, EngineStream, SAMPLE_RATE},
    sherpa::PhraseEngine,
};

fn clip(name: &str) -> Vec<f32> {
    let data = std::env::var_os("KIKU_VOICES_DATA").map(PathBuf::from).expect("KIKU_VOICES_DATA names the voices data folder");
    let path = data.join("fixtures").join(format!("{name}.s16"));
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b) as f32 / 32768.0).collect()
}

/// Two voices at once, then the first voice's next clip over the second's start: about 13 s without a pause.
fn talking_over_each_other() -> Vec<f32> {
    let (a1, a2, b1) = (clip("a1"), clip("a2"), clip("b1"));
    let mut audio = vec![0.0; a1.len() + a2.len()];
    for (i, x) in a1.iter().chain(&a2).enumerate() {
        audio[i] += 0.5 * x;
    }
    for (i, x) in b1.iter().chain(&b1).enumerate().take(audio.len()) {
        audio[i] += 0.5 * x;
    }
    audio
}

#[test]
#[ignore = "needs Parakeet Unified and the voices fixtures"]
fn speech_without_a_pause_is_cut_into_ordered_phrases() {
    let model = catalog::by_id("parakeet-unified").unwrap();
    let spec = model.spec(model.variant(Device::Cpu));
    let Backend::Phrases(kind) = spec.backend else { panic!("Parakeet isn't a phrase model") };
    let engine = PhraseEngine::load(&spec, kind).unwrap();
    let audio = talking_over_each_other();
    let seconds = audio.len() as f32 / SAMPLE_RATE as f32;

    let mut stream = engine.open_phrases().unwrap();
    let mut phrases = Vec::new();
    for chunk in audio.chunks(SAMPLE_RATE as usize / 10) {
        stream.push(chunk).unwrap();
        phrases.extend(stream.phrases());
    }
    stream.finish();
    phrases.extend(stream.phrases());

    let times: Vec<(f32, f32)> = phrases.iter().map(|p| (p.start, p.end)).collect();
    assert!(phrases.len() >= 2, "{seconds} s without a pause is more than one phrase: {times:?}");
    assert!(phrases.iter().all(|p| !p.text.is_empty() && p.end - p.start <= 10.5), "each phrase heard, none past the cut: {times:?}");
    assert!(times.windows(2).all(|w| w[0].1 <= w[1].0 + 1e-3), "in order, no audio twice: {times:?}");
    assert!(times[0].0 < 1.0 && times[times.len() - 1].1 > seconds - 1.5, "from start to end, none lost: {times:?} of {seconds} s");
}
