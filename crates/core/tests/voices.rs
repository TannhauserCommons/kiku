//! Voices (`.loop/voices/SPINE.md`): the benchmark's conversations and metrics, labels per phrase, splitting
//! a phrase at a speaker change, overlap, and the engine's lines. The `#[ignore]`d tests need models and
//! LibriSpeech clips in `$KIKU_VOICES_DATA` (default `~/kiku-data`), and Parakeet Unified where
//! `KIKU_MODELS_DIR` points.

use std::{fs, path::PathBuf};

use kiku_core::{
    catalog,
    engine::{load_here, Device, Line, SAMPLE_RATE},
    engine_host::probe_clip,
    voices::{
        conversation::{build, Condition, Gaps, Utterance, TURNS},
        into_lines,
        metrics::{change_scores, cp_wer, line_accuracy, two_voice_share, wer, Errors, Hyp, HypLine, Share, Truth, Turn, Word},
        overlaps, split_points, split_text, Clusterer, Embedder, Part, Segment, Segmenter, Thresholds, Token,
    },
};

const RATE: f32 = SAMPLE_RATE as f32;

// ---- helpers ----

/// A turn whose words are given as (text, start, end).
fn turn(speaker: &str, words: &[(&str, f32, f32)]) -> Turn {
    Turn {
        speaker: speaker.into(),
        start: words[0].1,
        end: words[words.len() - 1].2,
        words: words.iter().map(|&(text, start, end)| Word { text: text.into(), start, end }).collect(),
    }
}

fn line(text: &str, start: f32, end: f32, label: Option<u32>) -> HypLine {
    HypLine { text: text.into(), start, end, label }
}

fn hyp(lines: Vec<HypLine>) -> Hyp {
    Hyp { lines }
}

/// Five speakers with six utterances each: a tone (a pitch per speaker, a length per utterance) padded with
/// 0.3 s of silence at both ends.
fn utterances() -> Vec<Utterance> {
    let mut all = Vec::new();
    for s in 0..5 {
        for u in 0..6 {
            let seconds = 1.0 + 0.25 * ((s + u) % 7) as f32;
            let pitch = 150.0 + 70.0 * s as f32;
            let mut audio = vec![0.0; (0.3 * RATE) as usize];
            audio.extend((0..(seconds * RATE) as usize).map(|i| 0.5 * (i as f32 * pitch * std::f32::consts::TAU / RATE).sin()));
            audio.extend(vec![0.0; (0.3 * RATE) as usize]);
            let text = (0..3 + u).map(|w| format!("w{s}{u}{w}")).collect::<Vec<_>>().join(" ");
            all.push(Utterance { speaker: format!("spk{s}"), text, audio });
        }
    }
    all
}

fn unit(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / norm).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    unit(a).iter().zip(unit(b)).map(|(x, y)| x * y).sum()
}

/// A unit vector along axis `axis` of 8, nudged by `noise` along the axis after it.
fn along(axis: usize, noise: f32) -> Vec<f32> {
    let mut v = vec![0.0; 8];
    v[axis] = 1.0;
    v[(axis + 1) % 8] = noise;
    unit(&v)
}

fn thresholds() -> Thresholds {
    Thresholds { similarity: 0.5, margin: 0.1, min_seconds: 1.5 }
}

fn data() -> PathBuf {
    std::env::var_os("KIKU_VOICES_DATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join("kiku-data"))
}

/// A LibriSpeech clip kept for these tests as raw 16 kHz mono 16-bit little-endian samples.
fn clip(name: &str) -> Vec<f32> {
    let path = data().join("fixtures").join(format!("{name}.s16"));
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    bytes.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b) as f32 / 32768.0).collect()
}

fn model(name: &str) -> PathBuf {
    let path = data().join("models").join(name);
    assert!(path.is_file(), "{} is missing", path.display());
    path
}

fn mixed(a: &[f32], b: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0; a.len().max(b.len())];
    for (i, x) in a.iter().enumerate() {
        out[i] += 0.5 * x;
    }
    for (i, x) in b.iter().enumerate() {
        out[i] += 0.5 * x;
    }
    out
}

fn overlap_seconds(spans: &[(f32, f32)]) -> f32 {
    spans.iter().map(|(start, end)| end - start).sum()
}

// ---- V1: the benchmark's conversations ----

#[test]
fn v1_conversations_are_deterministic() {
    let utterances = utterances();
    let all = Condition::all();
    assert_eq!(all.len(), 15);
    for condition in &all {
        assert_eq!(Condition::parse(&condition.name()).as_ref(), Some(condition), "{}", condition.name());
    }
    assert_eq!(Condition::parse("s2-fast"), Some(Condition { speakers: 2, gaps: Gaps::Fast, overlap: 0.0 }));
    assert_eq!(Condition::parse("s4-o30").map(|c| (c.speakers, c.overlap)), Some((4, 0.3)));
    assert_eq!(Condition::parse("s5-fast"), None);

    for condition in &all {
        let name = condition.name();
        let first = build(&utterances, condition, 7);
        let again = build(&utterances, condition, 7);
        assert_eq!(first.audio.iter().map(|x| x.to_bits()).collect::<Vec<_>>(), again.audio.iter().map(|x| x.to_bits()).collect::<Vec<_>>(), "{name}: same seed, same audio");
        assert_eq!(first.truth, again.truth, "{name}: same seed, same truth");
        assert_ne!(build(&utterances, condition, 8).truth, first.truth, "{name}: another seed, another conversation");

        let turns = &first.truth.turns;
        assert_eq!(turns.len(), TURNS, "{name}");
        let mut speakers: Vec<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
        speakers.sort();
        speakers.dedup();
        assert_eq!(speakers.len(), condition.speakers, "{name}: speakers");
        assert!(first.audio.len() as f32 >= turns.iter().map(|t| t.end).fold(0.0, f32::max) * RATE - 1.0, "{name}: audio covers every turn");

        for t in turns {
            assert!(!t.speaker.is_empty() && !t.words.is_empty(), "{name}: every word has a speaker");
            let words: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
            assert!(utterances.iter().any(|u| u.speaker == t.speaker && u.text.split(' ').eq(words.iter().copied())), "{name}: a turn is one utterance's words");
            assert!(t.words.iter().all(|w| w.start >= t.start - 1e-3 && w.end <= t.end + 1e-3 && w.start < w.end), "{name}: words inside their turn");
            assert!(t.end - t.start >= 1.0 - 0.03 && t.end - t.start <= 2.5 + 0.03, "{name}: silence trimmed, {} s", t.end - t.start);
        }
        for pair in turns.windows(2) {
            let (before, after) = (&pair[0], &pair[1]);
            assert_ne!(before.speaker, after.speaker, "{name}: consecutive turns are different speakers");
            let gap = after.start - before.end;
            let tolerance = 2.0 / RATE;
            if condition.overlap > 0.0 {
                let shorter = (before.end - before.start).min(after.end - after.start);
                assert!((-gap - condition.overlap * shorter).abs() <= tolerance, "{name}: overlap {} s of a {shorter} s turn", -gap);
            } else {
                let (low, high) = match condition.gaps {
                    Gaps::Fast => (0.0, 0.3),
                    Gaps::Normal => (0.3, 1.0),
                };
                assert!(gap >= low - tolerance && gap <= high + tolerance, "{name}: gap {gap} s");
            }
        }
    }
}

#[test]
fn v1_turn_audio_is_the_utterance_trimmed() {
    let utterances = utterances();
    let condition = Condition::parse("s2-normal").unwrap();
    let conversation = build(&utterances, &condition, 3);
    for t in &conversation.truth.turns {
        let at = |seconds: f32| conversation.audio[((seconds * RATE) as usize).min(conversation.audio.len() - 1)].abs();
        let loud = (0..20).map(|k| at(t.start + 0.05 + k as f32 * 0.04)).fold(0.0, f32::max);
        assert!(loud > 0.05, "the turn's audio is where its turn says");
    }
}

// ---- V2: the metrics ----

#[test]
fn v2_cp_wer_matches_meeteval_examples() {
    let truth = Truth {
        turns: vec![
            turn("A", &[("THE", 0.0, 0.3), ("CAT", 0.3, 0.6), ("SAT", 0.6, 1.0)]),
            turn("B", &[("ON", 1.2, 1.4), ("THE", 1.4, 1.6), ("MAT", 1.6, 2.0)]),
        ],
    };
    let errors = |hyp: &Hyp| (cp_wer(&truth, hyp), wer(&truth, hyp));

    // A permutation: labels are matched to speakers whichever way costs least; case and punctuation aside.
    let swapped = hyp(vec![line("The cat, sat.", 0.0, 1.0, Some(1)), line("On the mat!", 1.2, 2.0, Some(0))]);
    assert_eq!(errors(&swapped), (Errors { errors: 0, words: 6 }, Errors { errors: 0, words: 6 }));

    // Everything as one speaker: cpWER counts B's words as deleted and inserted under A; WER doesn't care.
    let one = hyp(vec![line("the cat sat", 0.0, 1.0, None), line("on the mat", 1.2, 2.0, None)]);
    assert_eq!(errors(&one), (Errors { errors: 6, words: 6 }, Errors { errors: 0, words: 6 }));

    // An extra label is all insertions; an empty one costs nothing; a substitution is one error.
    let extra = hyp(vec![
        line("the cat sat", 0.0, 1.0, Some(0)),
        line("uh huh", 1.0, 1.2, Some(2)),
        line("", 1.1, 1.2, Some(3)),
        line("on a mat", 1.2, 2.0, Some(1)),
    ]);
    assert_eq!(errors(&extra), (Errors { errors: 3, words: 6 }, Errors { errors: 3, words: 6 }));

    // A speaker never labelled: all deletions.
    let half = hyp(vec![line("the cat sat", 0.0, 1.0, Some(4))]);
    assert_eq!(errors(&half), (Errors { errors: 3, words: 6 }, Errors { errors: 3, words: 6 }));
    assert_eq!(Errors { errors: 3, words: 6 }.rate(), 0.5);
}

#[test]
fn v2_line_accuracy_uses_the_best_mapping() {
    let truth = Truth {
        turns: vec![
            turn("A", &[("a", 0.0, 0.5), ("b", 0.5, 1.0)]),
            turn("B", &[("c", 1.0, 1.5), ("d", 1.5, 2.0)]),
            turn("A", &[("e", 2.0, 2.5), ("f", 2.5, 3.0)]),
            turn("B", &[("g", 3.0, 3.5), ("h", 3.5, 4.0)]),
        ],
    };
    let labelled = |labels: [u32; 4]| hyp(labels.iter().enumerate().map(|(i, &l)| line("x", i as f32, i as f32 + 1.0, Some(l))).collect());
    assert_eq!(line_accuracy(&truth, &labelled([7, 9, 7, 9])), Share { hits: 4, of: 4 });
    assert_eq!(line_accuracy(&truth, &labelled([9, 7, 9, 7])), Share { hits: 4, of: 4 }, "labels mean nothing by themselves");
    assert_eq!(line_accuracy(&truth, &labelled([7, 9, 7, 7])), Share { hits: 3, of: 4 });
    assert_eq!(line_accuracy(&truth, &labelled([7, 7, 7, 7])), Share { hits: 2, of: 4 }, "one label maps to one speaker");
    assert_eq!(line_accuracy(&truth, &labelled([1, 2, 3, 4])), Share { hits: 2, of: 4 }, "one speaker gets one label");

    // A line's truth is the speaker of most of its words; a line holding none is left out.
    let spanning = hyp(vec![line("x", 0.0, 2.6, Some(5)), line("y", 3.0, 4.0, Some(6)), line("z", 10.0, 11.0, Some(5))]);
    assert_eq!(line_accuracy(&truth, &spanning), Share { hits: 2, of: 2 });
    assert_eq!(Share { hits: 3, of: 4 }.rate(), 0.75);
}

#[test]
fn v2_two_voice_share_and_change_scores() {
    let truth = Truth {
        turns: vec![
            turn("A", &[("a1", 0.0, 1.0), ("a2", 1.0, 2.0)]),
            turn("B", &[("b1", 2.2, 3.1), ("b2", 3.1, 4.0)]),
            turn("A", &[("a3", 4.1, 5.05), ("a4", 5.05, 6.0)]),
        ],
    };
    let split = hyp(vec![
        line("a1 a2", 0.0, 2.0, Some(0)),
        line("b1 b2", 2.3, 4.0, Some(1)),
        line("a3", 4.1, 5.0, Some(1)),
        line("a4", 5.0, 6.0, Some(0)),
    ]);
    assert_eq!(two_voice_share(&truth, &split), Share { hits: 0, of: 4 });
    let scores = change_scores(&truth, &split);
    assert_eq!((scores.truth, scores.found, scores.matched), (2, 2, 1), "2.3 is within 0.5 s of 2.2; 5.0 is 0.9 s from 4.1");
    assert_eq!((scores.recall(), scores.precision()), (0.5, 0.5));

    let merged = hyp(vec![line("a1 a2 b1 b2", 0.0, 4.0, None), line("a3 a4", 4.1, 6.0, None)]);
    assert_eq!(two_voice_share(&truth, &merged), Share { hits: 1, of: 2 });
    let scores = change_scores(&truth, &merged);
    assert_eq!((scores.truth, scores.found, scores.matched), (2, 0, 0));
    assert_eq!((scores.recall(), scores.precision()), (0.0, 1.0), "nothing found is never wrong");

    let out_of_order = hyp(vec![line("a3 a4", 4.1, 6.0, Some(0)), line("b1 b2", 2.2, 4.0, Some(1)), line("a1 a2", 0.0, 2.0, Some(0))]);
    let scores = change_scores(&truth, &out_of_order);
    assert_eq!((scores.found, scores.matched), (2, 2), "lines are taken in time order");
}

// ---- V3 (spine V4): labels per phrase ----

#[test]
fn v4_clusterer_separates_and_holds_labels() {
    let mut clusterer = Clusterer::new(thresholds());
    let a = |noise| along(0, noise);
    let b = |noise| along(2, noise);
    assert_eq!(clusterer.assign(&a(0.0), 3.0), 0);
    assert_eq!(clusterer.assign(&b(0.0), 3.0), 1);
    assert_eq!(clusterer.assign(&a(0.1), 3.0), 0);
    assert_eq!(clusterer.assign(&b(-0.1), 2.0), 1);
    assert_eq!(clusterer.assign(&a(-0.1), 2.0), 0);

    // A near-tie between two known speakers is one of them, but not confident enough to move a centroid.
    let before: Vec<Vec<f32>> = (0..2).map(|l| clusterer.centroid(l).unwrap().to_vec()).collect();
    let tie = unit(&[1.0, 0.0, 1.02, 0.0, 0.0, 0.0, 0.0, 0.0]);
    let label = clusterer.assign(&tie, 3.0);
    assert!(label < 2, "a near-tie starts no new speaker, got {label}");
    for l in 0..2 {
        assert_eq!(clusterer.centroid(l).unwrap(), before[l as usize], "a near-tie moves no centroid");
    }

    // Confident matches move their centroid; a voice like no one's is a new speaker.
    clusterer.assign(&a(0.3), 3.0);
    assert_ne!(clusterer.centroid(0).unwrap(), before[0]);
    assert!(cosine(clusterer.centroid(0).unwrap(), &a(0.0)) > 0.9);
    assert_eq!(clusterer.assign(&along(5, 0.0), 3.0), 2);
    assert_eq!(clusterer.centroid(3), None);

    // Labels already given stay: the first speakers are still 0 and 1 after many phrases.
    for _ in 0..50 {
        clusterer.assign(&b(0.05), 3.0);
    }
    assert_eq!(clusterer.assign(&a(0.0), 3.0), 0);
    assert_eq!(clusterer.assign(&b(0.0), 3.0), 1);
}

#[test]
fn v4_short_phrases_take_the_previous_label() {
    let mut clusterer = Clusterer::new(thresholds());
    assert_eq!(clusterer.assign(&along(0, 0.0), 2.0), 0);
    assert_eq!(clusterer.assign(&along(3, 0.0), 2.0), 1);
    let before = clusterer.centroid(1).unwrap().to_vec();
    assert_eq!(clusterer.assign(&along(0, 0.0), 1.0), 1, "under 1.5 s: the previous label, whoever it sounds like");
    assert_eq!(clusterer.centroid(1).unwrap(), before, "and no centroid moves");
    assert_eq!(clusterer.centroid(2), None, "nor does it start a speaker");
    assert_eq!(clusterer.assign(&along(0, 0.0), 1.5), 0, "1.5 s is long enough");
}

#[test]
#[ignore = "needs TitaNet-small and LibriSpeech clips in the voices data folder"]
fn v4_embeds_same_speaker_closer() {
    let embedder = Embedder::load(&model("nemo_en_titanet_small.onnx")).unwrap();
    let (a1, a2, b1) = (embedder.embed(&clip("a1")), embedder.embed(&clip("a2")), embedder.embed(&clip("b1")));
    assert!(!a1.is_empty() && a1.len() == b1.len());
    let same = cosine(&a1, &a2);
    assert!(same > cosine(&a1, &b1) && same > cosine(&a2, &b1), "same speaker {same}, others {} and {}", cosine(&a1, &b1), cosine(&a2, &b1));
}

// ---- V4 (spine V5): splitting a phrase at a speaker change ----

#[test]
fn v5_split_points_from_window_scores() {
    let scores = [0.9, 0.9, 0.3, 0.2, 0.4, 0.9, 0.9];
    let at = split_points(&scores, 1.5, 0.5, 0.6);
    assert_eq!(at.len(), 1, "one run under the threshold is one change: {at:?}");
    assert!((at[0] - 2.5).abs() < 1e-4, "between windows 3 and 4's centres: {at:?}");
    assert!(split_points(&[0.8, 0.7, 0.75, 0.9], 1.5, 0.5, 0.6).is_empty(), "noise above the threshold splits nothing");
    assert!(split_points(&[], 1.5, 0.5, 0.6).is_empty());
    assert_eq!(split_points(&[0.2, 0.9, 0.9, 0.1], 1.5, 0.5, 0.6).len(), 2);
}

#[test]
fn v5_text_splits_at_token_times() {
    let tokens: Vec<Token> = [(" Hel", 0.0), ("lo", 0.2), (" there", 0.5), (",", 0.9), (" how", 1.6), (" a", 1.8), ("re", 1.9), ("▁you", 2.1)]
        .iter()
        .map(|&(text, start)| Token { text: text.into(), start })
        .collect();
    assert_eq!(split_text(&tokens, &[1.0]), ["Hello there,", "how are you"]);
    assert_eq!(split_text(&tokens, &[0.1]), ["Hello", "there, how are you"], "a word goes whole to where it starts");
    assert_eq!(split_text(&tokens, &[1.85]), ["Hello there, how are", "you"], "are starts at 1.8 even if its last piece is later");
    assert_eq!(split_text(&tokens, &[]), ["Hello there, how are you"]);
    assert_eq!(split_text(&tokens, &[5.0]), ["Hello there, how are you", ""]);
}

// ---- V5 (spine V6): overlap ----

#[test]
fn v6_overlaps_are_merged_stretches_of_two_speakers() {
    let segment = |speaker, start, end| Segment { speaker, start, end };
    assert_eq!(overlaps(&[segment(0, 0.0, 2.0), segment(1, 2.0, 3.0)]), Vec::<(f32, f32)>::new(), "touching isn't overlapping");
    assert_eq!(overlaps(&[segment(1, 1.5, 3.0), segment(0, 0.0, 2.0)]), [(1.5, 2.0)]);
    assert_eq!(overlaps(&[segment(0, 0.0, 4.0), segment(1, 1.0, 2.0), segment(2, 1.5, 3.0)]), [(1.0, 3.0)], "merged");
    assert_eq!(overlaps(&[segment(0, 0.0, 2.0), segment(0, 1.0, 3.0)]), Vec::<(f32, f32)>::new(), "one speaker never overlaps itself");
}

#[test]
#[ignore = "needs pyannote segmentation-3.0, TitaNet-small and LibriSpeech clips in the voices data folder"]
fn v6_overlap_is_detected() {
    let segmenter = Segmenter::load(&model("sherpa-onnx-pyannote-segmentation-3-0/model.onnx"), &model("nemo_en_titanet_small.onnx")).unwrap();
    let (a, b) = (clip("a1"), clip("b1"));
    let both = overlap_seconds(&overlaps(&segmenter.segments(&mixed(&a, &b))));
    let alone = overlap_seconds(&overlaps(&segmenter.segments(&a)));
    assert!(both >= 1.0, "two voices at once: {both} s of overlap");
    assert!(alone < 0.5, "one voice: {alone} s of overlap");
}

// ---- V6 (spine V8): the engine ----

#[test]
fn v8_split_lines_keep_order_and_ids() {
    let part = |text: &str, voice, start| Part { text: text.into(), voice: Some(voice), start };
    let mut next = 41;
    let lines = into_lines(vec![part("and you?", 1, 3.2), part("", 0, 4.0), part("I'm fine,", 0, 1.0)], &mut next);
    assert_eq!(
        lines,
        [
            Line { id: 41, text: "I'm fine,".into(), complete: true, voice: Some(0) },
            Line { id: 42, text: "and you?".into(), complete: true, voice: Some(1) },
        ]
    );
    assert_eq!(next, 43);
    assert!(into_lines(vec![part(" ", 0, 0.0)], &mut next).is_empty());
    assert_eq!(next, 43);
}

#[test]
#[ignore = "needs Parakeet Unified (int8) where KIKU_MODELS_DIR points"]
fn v8_voices_off_is_todays_path() {
    let model = catalog::by_id("parakeet-unified").unwrap();
    let variant = model.variant(Device::Cpu);
    assert!(variant.installed(), "Parakeet Unified isn't installed where KIKU_MODELS_DIR points");
    let engine = load_here(&model.spec(variant)).unwrap();
    let clip = probe_clip();
    let mut audio = clip.clone();
    audio.extend(vec![0.0; SAMPLE_RATE as usize]);
    audio.extend(&clip);

    let mut stream = engine.open_stream().unwrap();
    let mut lines = Vec::new();
    for chunk in audio.chunks(SAMPLE_RATE as usize / 10) {
        stream.push(chunk).unwrap();
        lines.extend(stream.poll().unwrap());
    }
    lines.extend(stream.flush().unwrap());

    let heard: Vec<String> = lines.iter().map(|l| format!("{}\t{}\t{}", l.id, l.complete, l.text)).collect();
    let fixture = include_str!("fixtures/voices-off.txt");
    assert_eq!(heard, fixture.lines().collect::<Vec<_>>());
    assert!(lines.iter().all(|l| l.voice.is_none()));
}
