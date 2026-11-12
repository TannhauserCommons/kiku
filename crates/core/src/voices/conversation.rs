//! The voices benchmark's conversations: LibriSpeech utterances (or any speakers' utterances) placed one after
//! another as turns, deterministically from a seed, with the condition's turn gaps or overlap, and the truth of
//! who said each word when.

use std::collections::BTreeSet;

use super::metrics::{Truth, Turn, Word};
use crate::engine::SAMPLE_RATE;

/// One speaker's recording and its transcript, 16 kHz mono.
#[derive(Clone, Debug)]
pub struct Utterance {
    pub speaker: String,
    pub text: String,
    pub audio: Vec<f32>,
}

/// The silence between turns when they don't overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gaps {
    /// 0 to 300 ms, most conversational turn gaps.
    Fast,
    /// 300 to 1000 ms.
    Normal,
}

/// A benchmark condition: how many speakers, and the gaps between turns or how much they overlap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Condition {
    pub speakers: usize,
    pub gaps: Gaps,
    /// Each turn starts this share of the shorter of it and the turn before before that turn ends (0, 0.1, 0.3
    /// or 1); when above 0, `gaps` doesn't apply (and is `Fast`). At 100% a speaker's own turns can overlap: a
    /// turn nested in the one before ends with it, so the next can't start after it.
    pub overlap: f32,
}

impl Condition {
    /// The 15 conditions: 2, 3 and 4 speakers with fast or normal gaps, or with 10%, 30% or 100% overlap.
    pub fn all() -> Vec<Condition> {
        let kinds = [(Gaps::Fast, 0.0), (Gaps::Normal, 0.0), (Gaps::Fast, 0.1), (Gaps::Fast, 0.3), (Gaps::Fast, 1.0)];
        (2..=4).flat_map(|speakers| kinds.map(|(gaps, overlap)| Condition { speakers, gaps, overlap })).collect()
    }

    /// `s2-fast`, `s3-normal`, `s4-o30`, ...
    pub fn name(&self) -> String {
        let kind = match (self.overlap, self.gaps) {
            (o, _) if o > 0.0 => format!("o{}", (o * 100.0).round()),
            (_, Gaps::Fast) => "fast".into(),
            (_, Gaps::Normal) => "normal".into(),
        };
        format!("s{}-{kind}", self.speakers)
    }

    pub fn parse(name: &str) -> Option<Condition> {
        Condition::all().into_iter().find(|c| c.name() == name)
    }
}

/// Turns per conversation.
pub const TURNS: usize = 12;

/// A conversation's audio (16 kHz mono) and its truth.
#[derive(Clone, Debug, PartialEq)]
pub struct Conversation {
    pub audio: Vec<f32>,
    pub truth: Truth,
}

/// A conversation of [`TURNS`] turns among `condition.speakers` speakers drawn from `utterances` by `seed`
/// (the same seed, the same bytes). Consecutive turns are different speakers; each turn is one utterance with
/// its leading and trailing silence trimmed, its words spread over it by their length.
pub fn build(utterances: &[Utterance], condition: &Condition, seed: u64) -> Conversation {
    let mut rng = Rng(seed);
    let names: Vec<&str> = utterances.iter().map(|u| u.speaker.as_str()).collect::<BTreeSet<_>>().into_iter().collect();
    assert!(names.len() >= condition.speakers, "not enough speakers");
    let mut chosen = names;
    rng.shuffle(&mut chosen);
    chosen.truncate(condition.speakers);

    // Every speaker speaks once in a random order, then anyone but the last speaker.
    let mut order: Vec<usize> = (0..chosen.len()).collect();
    rng.shuffle(&mut order);
    while order.len() < TURNS {
        let previous = order[order.len() - 1];
        order.push((previous + 1 + rng.below(chosen.len() - 1)) % chosen.len());
    }

    // Each speaker's utterances in random order, all used before any is used again.
    let mut queues: Vec<Vec<usize>> = vec![Vec::new(); chosen.len()];
    let mut audio: Vec<f32> = Vec::new();
    let mut turns: Vec<Turn> = Vec::new();
    let mut previous: Option<(usize, usize)> = None;
    for speaker in order {
        if queues[speaker].is_empty() {
            let mut own: Vec<usize> = (0..utterances.len()).filter(|&i| utterances[i].speaker == chosen[speaker]).collect();
            rng.shuffle(&mut own);
            queues[speaker] = own;
        }
        let utterance = &utterances[queues[speaker].pop().unwrap()];
        let speech = trimmed(&utterance.audio);

        let start = match previous {
            None => 0,
            Some((before_start, before_end)) if condition.overlap > 0.0 => {
                let shorter = (before_end - before_start).min(speech.len());
                before_end - (condition.overlap * shorter as f32).round() as usize
            }
            Some((_, before_end)) => {
                let (low, high) = match condition.gaps {
                    Gaps::Fast => (0.0, 0.3),
                    Gaps::Normal => (0.3, 1.0),
                };
                before_end + ((low + (high - low) * rng.unit()) * SAMPLE_RATE as f32).round() as usize
            }
        };
        let end = start + speech.len();
        if audio.len() < end {
            audio.resize(end, 0.0);
        }
        for (mixed, x) in audio[start..end].iter_mut().zip(speech) {
            *mixed += x;
        }
        turns.push(turn(chosen[speaker], &utterance.text, start, end));
        previous = Some((start, end));
    }
    Conversation { audio, truth: Truth { turns } }
}

/// The audio between its first and last 10 ms frames within 35 dB of its loudest.
fn trimmed(audio: &[f32]) -> &[f32] {
    const FRAME: usize = SAMPLE_RATE as usize / 100;
    let peak = |frame: &[f32]| frame.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let floor = peak(audio) * 10f32.powf(-35.0 / 20.0);
    let loud: Vec<usize> = audio.chunks(FRAME).enumerate().filter(|(_, f)| peak(f) > floor).map(|(i, _)| i).collect();
    match (loud.first(), loud.last()) {
        (Some(first), Some(last)) => &audio[first * FRAME..((last + 1) * FRAME).min(audio.len())],
        _ => &[],
    }
}

/// A turn over samples `start..end` with its words spread by their length.
fn turn(speaker: &str, text: &str, start: usize, end: usize) -> Turn {
    let seconds = |sample: f32| sample / SAMPLE_RATE as f32;
    let words: Vec<&str> = text.split(' ').collect();
    let total: usize = words.iter().map(|w| w.chars().count()).sum();
    let mut done = 0;
    let words = words
        .into_iter()
        .map(|text| {
            let from = start as f32 + (end - start) as f32 * done as f32 / total as f32;
            done += text.chars().count();
            let to = start as f32 + (end - start) as f32 * done as f32 / total as f32;
            Word { text: text.into(), start: seconds(from), end: seconds(to) }
        })
        .collect();
    Turn { speaker: speaker.into(), start: seconds(start as f32), end: seconds(end as f32), words }
}

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// In `0..n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// In `[0, 1)`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i + 1));
        }
    }
}
