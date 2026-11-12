//! The voices benchmark's metrics: who said each line, lines holding two voices, speaker changes found, and
//! word error rates with and without speakers (cpWER as meeteval defines it). Counts, so conversations add up.

use std::collections::{BTreeMap, BTreeSet};

use super::{overlaps, Segment};

/// What was really said: turns in time order, each one speaker's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Truth {
    pub turns: Vec<Turn>,
}

/// One speaker's utterance, seconds from the conversation's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub speaker: String,
    pub start: f32,
    pub end: f32,
    pub words: Vec<Word>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

/// What Kiku heard: lines with their times and labels (`None`: no label, all such lines one speaker).
#[derive(Clone, Debug, PartialEq)]
pub struct Hyp {
    pub lines: Vec<HypLine>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HypLine {
    pub text: String,
    pub start: f32,
    pub end: f32,
    pub label: Option<u32>,
}

/// `hits` out of `of`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Share {
    pub hits: usize,
    pub of: usize,
}

impl Share {
    pub fn rate(self) -> f64 {
        ratio(self.hits, self.of, 0.0)
    }
}

/// Word errors (substitutions, deletions, insertions) against the reference's word count.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Errors {
    pub errors: usize,
    pub words: usize,
}

impl Errors {
    pub fn rate(self) -> f64 {
        ratio(self.errors, self.words, 0.0)
    }
}

/// Speaker changes: true ones, ones found, and found within [`CHANGE_TOLERANCE`] of a true one (one to one).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChangeScores {
    pub truth: usize,
    pub found: usize,
    pub matched: usize,
}

impl ChangeScores {
    /// Matched over true changes; 1 when there are none.
    pub fn recall(self) -> f64 {
        ratio(self.matched, self.truth, 1.0)
    }

    /// Matched over found changes; 1 when none were found.
    pub fn precision(self) -> f64 {
        ratio(self.matched, self.found, 1.0)
    }
}

/// How near a found change must be to a true one, in seconds.
pub const CHANGE_TOLERANCE: f32 = 0.5;

/// Overlapped speech: how much of it is really there, how much was found, and how much of that is both, in seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overlap {
    pub truth: f64,
    pub found: f64,
    pub both: f64,
}

impl Overlap {
    /// Both over what is really there; 1 when nothing is.
    pub fn recall(self) -> f64 {
        if self.truth == 0.0 { 1.0 } else { self.both / self.truth }
    }

    /// Both over what was found; 1 when nothing was.
    pub fn precision(self) -> f64 {
        if self.found == 0.0 { 1.0 } else { self.both / self.found }
    }
}

/// Per line, how far the overlapped share of it that was found is from the true share (0 to 1), summed over lines.
/// Unlike a yes or no per line it doesn't saturate on a long line with a little overlap.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FractionError {
    pub lines: usize,
    pub error: f64,
}

impl FractionError {
    /// The mean difference per line; 0 when there are no lines.
    pub fn mean(self) -> f64 {
        if self.lines == 0 { 0.0 } else { self.error / self.lines as f64 }
    }
}

fn ratio(part: usize, whole: usize, when_empty: f64) -> f64 {
    if whole == 0 {
        when_empty
    } else {
        part as f64 / whole as f64
    }
}

// A word is in a line when its midpoint is in the line's [start, end]. Lines holding no word are left out of
// line accuracy and the two-voice share.

/// Lines whose label is their true speaker (the speaker of most of their words), under the one-to-one mapping
/// of labels to speakers that gets the most lines right.
pub fn line_accuracy(truth: &Truth, hyp: &Hyp) -> Share {
    let speakers = speaker_names(truth);
    let labels = label_set(hyp);
    let mut hits = vec![vec![0i64; labels.len()]; speakers.len()];
    let mut of = 0;
    for line in &hyp.lines {
        let counts = words_per_speaker(truth, line);
        // Ties go to the first speaker by name.
        let Some((speaker, _)) = counts.iter().rev().max_by_key(|(_, count)| **count) else { continue };
        let row = speakers.iter().position(|s| s == speaker).unwrap();
        hits[row][labels.iter().position(|l| *l == line.label).unwrap()] += 1;
        of += 1;
    }
    // Padded with zeros: a speaker or label left over scores no hits.
    let size = speakers.len().max(labels.len());
    let lost = cheapest_assignment(size, |row, column| -hits.get(row).and_then(|r| r.get(column)).copied().unwrap_or(0));
    Share { hits: (-lost) as usize, of }
}

/// Lines holding words of two or more speakers.
pub fn two_voice_share(truth: &Truth, hyp: &Hyp) -> Share {
    let held: Vec<usize> = hyp.lines.iter().map(|line| words_per_speaker(truth, line)).filter(|c| !c.is_empty()).map(|c| c.len()).collect();
    Share { hits: held.iter().filter(|&&speakers| speakers > 1).count(), of: held.len() }
}

/// True changes: each turn's start whose speaker differs from the turn before. Found changes: each line's
/// start whose label differs from the line before (lines in time order).
pub fn change_scores(truth: &Truth, hyp: &Hyp) -> ChangeScores {
    let mut turns: Vec<&Turn> = truth.turns.iter().collect();
    turns.sort_by(|a, b| a.start.total_cmp(&b.start));
    let real: Vec<f32> = turns.windows(2).filter(|w| w[0].speaker != w[1].speaker).map(|w| w[1].start).collect();
    let lines = lines_by_time(hyp);
    let found: Vec<f32> = lines.windows(2).filter(|w| w[0].label != w[1].label).map(|w| w[1].start).collect();

    // Both ascending, so taking the earliest pairs within the tolerance matches as many as can be.
    let (mut matched, mut next) = (0, 0);
    for change in &found {
        while next < real.len() && real[next] < change - CHANGE_TOLERANCE {
            next += 1;
        }
        if next < real.len() && real[next] <= change + CHANGE_TOLERANCE {
            matched += 1;
            next += 1;
        }
    }
    ChangeScores { truth: real.len(), found: found.len(), matched }
}

/// Where two speakers' turns overlap, seconds from the start (a turn is the utterance's span, pauses inside it
/// included): what an overlap detector is scored against.
pub fn truth_overlaps(truth: &Truth) -> Vec<(f32, f32)> {
    let speakers = speaker_names(truth);
    let speaker = |name: &str| speakers.iter().position(|s| *s == name).unwrap() as u32;
    let turns: Vec<Segment> = truth.turns.iter().map(|t| Segment { speaker: speaker(&t.speaker), start: t.start, end: t.end }).collect();
    overlaps(&turns)
}

/// The overlap `found` (merged, in time order) against the true one, in seconds.
pub fn overlap_seconds(truth: &Truth, found: &[(f32, f32)]) -> Overlap {
    let real = truth_overlaps(truth);
    let total = |spans: &[(f32, f32)]| spans.iter().map(|(from, to)| f64::from(to - from)).sum::<f64>();
    let both = real.iter().map(|&(from, to)| f64::from(inside(found, from, to))).sum();
    Overlap { truth: total(&real), found: total(found), both }
}

/// The error of the overlapped share of each line that is found against the true one.
pub fn overlap_fractions(truth: &Truth, hyp: &Hyp, found: &[(f32, f32)]) -> FractionError {
    let real = truth_overlaps(truth);
    let mut sum = FractionError::default();
    for line in hyp.lines.iter().filter(|line| line.end > line.start) {
        let share = |spans: &[(f32, f32)]| f64::from(inside(spans, line.start, line.end) / (line.end - line.start));
        sum.lines += 1;
        sum.error += (share(found) - share(&real)).abs();
    }
    sum
}

/// Seconds of `from..to` that lie in `spans` (merged, so none counts twice).
pub fn inside(spans: &[(f32, f32)], from: f32, to: f32) -> f32 {
    spans.iter().map(|&(start, end)| (end.min(to) - start.max(from)).max(0.0)).sum()
}

/// cpWER: each speaker's words concatenated in time order against each label's lines concatenated, under the
/// speaker-to-label permutation with the fewest errors; an unmatched speaker's words are deletions, an
/// unmatched label's insertions. Words are compared lowercased, punctuation aside (apostrophes kept).
pub fn cp_wer(truth: &Truth, hyp: &Hyp) -> Errors {
    let spoken: Vec<Vec<String>> = speaker_names(truth).iter().map(|name| spoken_words(truth, Some(name))).collect();
    let lines = lines_by_time(hyp);
    let heard: Vec<Vec<String>> = label_set(hyp)
        .iter()
        .map(|label| lines.iter().filter(|line| line.label == *label).flat_map(|line| words(&line.text)).collect())
        .collect();

    // Square, padded: a speaker with no label left is all deletions, a label with no speaker all insertions.
    let size = spoken.len().max(heard.len());
    let cost = |row: usize, column: usize| match (spoken.get(row), heard.get(column)) {
        (Some(said), Some(got)) => edit_distance(said, got) as i64,
        (Some(said), None) => said.len() as i64,
        (None, Some(got)) => got.len() as i64,
        (None, None) => 0,
    };
    Errors { errors: cheapest_assignment(size, cost) as usize, words: spoken.iter().map(Vec::len).sum() }
}

/// Plain WER: every word in time order against every line in time order, speakers aside.
pub fn wer(truth: &Truth, hyp: &Hyp) -> Errors {
    let said = spoken_words(truth, None);
    let got: Vec<String> = lines_by_time(hyp).iter().flat_map(|line| words(&line.text)).collect();
    Errors { errors: edit_distance(&said, &got), words: said.len() }
}

/// Lowercased words of `text` with everything but letters, digits and apostrophes dropped.
fn words(text: &str) -> Vec<String> {
    let clean = |word: &str| word.chars().filter(|c| c.is_alphanumeric() || *c == '\'').flat_map(char::to_lowercase).collect::<String>();
    text.split_whitespace().map(clean).filter(|word| !word.is_empty()).collect()
}

/// The words of one speaker (or all) in time order.
fn spoken_words(truth: &Truth, speaker: Option<&str>) -> Vec<String> {
    let mut spoken: Vec<&Word> = truth.turns.iter().filter(|t| speaker.is_none_or(|s| t.speaker == s)).flat_map(|t| &t.words).collect();
    spoken.sort_by(|a, b| a.start.total_cmp(&b.start));
    spoken.into_iter().flat_map(|word| words(&word.text)).collect()
}

fn speaker_names(truth: &Truth) -> Vec<&str> {
    let names: BTreeSet<&str> = truth.turns.iter().map(|t| t.speaker.as_str()).collect();
    names.into_iter().collect()
}

fn label_set(hyp: &Hyp) -> Vec<Option<u32>> {
    let labels: BTreeSet<Option<u32>> = hyp.lines.iter().map(|line| line.label).collect();
    labels.into_iter().collect()
}

fn lines_by_time(hyp: &Hyp) -> Vec<&HypLine> {
    let mut lines: Vec<&HypLine> = hyp.lines.iter().collect();
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    lines
}

/// How many of the line's words each speaker said, counting the words whose midpoint is in the line.
fn words_per_speaker<'a>(truth: &'a Truth, line: &HypLine) -> BTreeMap<&'a str, usize> {
    let mut counts = BTreeMap::new();
    for turn in &truth.turns {
        for word in &turn.words {
            if (line.start..=line.end).contains(&((word.start + word.end) / 2.0)) {
                *counts.entry(turn.speaker.as_str()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Word-level Levenshtein distance.
fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(x != y)).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The least total cost of giving each of `n` rows its own column (the Hungarian algorithm).
fn cheapest_assignment(n: usize, cost: impl Fn(usize, usize) -> i64) -> i64 {
    let (mut row_price, mut column_price) = (vec![0i64; n + 1], vec![0i64; n + 1]);
    let mut owner = vec![0usize; n + 1];
    for row in 1..=n {
        owner[0] = row;
        let (mut column, mut slack, mut came_from, mut seen) = (0, vec![i64::MAX; n + 1], vec![0usize; n + 1], vec![false; n + 1]);
        loop {
            seen[column] = true;
            let (current, mut best, mut next) = (owner[column], i64::MAX, 0);
            for other in (1..=n).filter(|&c| !seen[c]) {
                let reduced = cost(current - 1, other - 1) - row_price[current] - column_price[other];
                if reduced < slack[other] {
                    (slack[other], came_from[other]) = (reduced, column);
                }
                if slack[other] < best {
                    (best, next) = (slack[other], other);
                }
            }
            for c in 0..=n {
                if seen[c] {
                    row_price[owner[c]] += best;
                    column_price[c] -= best;
                } else {
                    slack[c] -= best;
                }
            }
            column = next;
            if owner[column] == 0 {
                break;
            }
        }
        while column != 0 {
            let before = came_from[column];
            owner[column] = owner[before];
            column = before;
        }
    }
    (1..=n).map(|column| cost(owner[column] - 1, column - 1)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(speaker: &str, start: f32, end: f32) -> Turn {
        Turn { speaker: speaker.into(), start, end, words: Vec::new() }
    }

    fn line(start: f32, end: f32) -> HypLine {
        HypLine { text: String::new(), start, end, label: None }
    }

    #[test]
    fn overlap_is_scored_in_seconds_and_per_line() {
        let truth = Truth { turns: vec![turn("a", 0.0, 4.0), turn("b", 3.0, 6.0), turn("a", 5.0, 8.0)] };
        assert_eq!(truth_overlaps(&truth), [(3.0, 4.0), (5.0, 6.0)]);

        // Finds the first second and half the second, and a second where there is none.
        let found = [(3.0, 4.0), (5.0, 5.5), (7.0, 8.0)];
        let scores = overlap_seconds(&truth, &found);
        assert_eq!(scores, Overlap { truth: 2.0, found: 2.5, both: 1.5 });
        assert_eq!((scores.recall(), scores.precision()), (0.75, 0.6));
        assert_eq!(overlap_seconds(&Truth { turns: vec![turn("a", 0.0, 1.0)] }, &[]).recall(), 1.0);

        // Lines of 3 s, 3.5 s and 1.5 s: true shares 0, 0.57, 0 and found shares 0, 0.43, 0.67.
        let hyp = Hyp { lines: vec![line(0.0, 3.0), line(3.0, 6.5), line(6.5, 8.0)] };
        let errors = overlap_fractions(&truth, &hyp, &found);
        assert_eq!(errors.lines, 3);
        assert!((errors.mean() - (0.0 + 0.5 / 3.5 + 1.0 / 1.5) / 3.0).abs() < 1e-6, "{}", errors.mean());
    }
}
