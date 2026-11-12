//! Telling voices apart inside one stream (`.loop/voices/SPINE.md`): a speaker embedding per phrase,
//! online clustering into labels, splitting a phrase where the speaker changes, and finding overlap.
//!
//! A voice print is biometric data: embeddings stay in memory, are never logged and never leave the process.

pub mod conversation;
pub mod metrics;

use std::path::Path;

use sherpa_onnx::{OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};

use crate::{
    engine::{Line, Result, SAMPLE_RATE},
    log,
    sherpa::Phrase,
};

/// Audio quieter than this (about -60 dBFS) has no voice to embed.
const SILENT_RMS: f32 = 1e-3;
/// The most audio of a phrase that is embedded (its middle): longer phrases score worse and cost more, and
/// the edges of a long phrase are where a neighbouring voice is likeliest.
const MAX_EMBED_SECONDS: f32 = 3.0;

/// Speaker embeddings (NVIDIA's TitaNet-small through sherpa-onnx).
pub struct Embedder {
    extractor: SpeakerEmbeddingExtractor,
}

impl Embedder {
    /// Loads the embedding model, e.g. `nemo_en_titanet_small.onnx`, on the CPU.
    pub fn load(model: &Path) -> Result<Self> {
        let config = SpeakerEmbeddingExtractorConfig { model: Some(model.to_string_lossy().into_owned()), ..Default::default() };
        let extractor = SpeakerEmbeddingExtractor::create(&config).ok_or_else(|| format!("can't load the speaker model {}", model.display()))?;
        Ok(Self { extractor })
    }

    /// The embedder from `model`, or none (logged) when it can't be loaded: voices are optional, so a damaged
    /// file must not stop captions.
    pub fn load_or_none(model: &Path) -> Option<Self> {
        Self::load(model).map_err(|error| log::write(format!("voices are off: {error}"))).ok()
    }

    /// The embedding of 16 kHz mono audio (its middle `MAX_EMBED_SECONDS` of it); empty when the audio is too
    /// short or too quiet to embed.
    pub fn embed(&self, samples: &[f32]) -> Vec<f32> {
        let samples = middle(samples, (MAX_EMBED_SECONDS * SAMPLE_RATE as f32) as usize);
        if too_quiet(samples) {
            return Vec::new();
        }
        let Some(stream) = self.extractor.create_stream() else { return Vec::new() };
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return Vec::new();
        }
        self.extractor.compute(&stream).unwrap_or_default()
    }
}

/// Whether audio is under [`SILENT_RMS`], with no voice to embed.
fn too_quiet(samples: &[f32]) -> bool {
    let rms = (samples.iter().map(|x| x * x).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    rms < SILENT_RMS
}

/// The middle `len` samples of `samples`, or all of them when there are fewer.
fn middle(samples: &[f32], len: usize) -> &[f32] {
    let from = samples.len().saturating_sub(len) / 2;
    &samples[from..samples.len().min(from + len)]
}

/// When the clusterer trusts a match.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    /// The cosine similarity a phrase needs with a speaker's centroid to be that speaker; below it, a new speaker.
    pub similarity: f32,
    /// How far the best match must beat the second best for the phrase to move the best's centroid.
    pub margin: f32,
    /// Phrases shorter than this (seconds) take the previous label and move no centroid.
    pub min_seconds: f32,
}

impl Thresholds {
    /// Whether a phrase `seconds` long is long enough to embed and cluster.
    pub fn embeds(&self, seconds: f32) -> bool {
        seconds >= self.min_seconds
    }
}

impl Default for Thresholds {
    /// The values tuned on the benchmark's tune half (`.loop/voices/progress.md`).
    fn default() -> Self {
        Self { similarity: 0.4, margin: 0.05, min_seconds: 1.5 }
    }
}

/// Online clustering of one stream's phrases into speaker labels (0, 1, 2, ... in order of first appearance).
/// A label once given never changes.
pub struct Clusterer {
    thresholds: Thresholds,
    speakers: Vec<Speaker>,
    last: Option<u32>,
}

/// A speaker's centroid: the running mean of the phrases that confidently matched it.
struct Speaker {
    centroid: Vec<f32>,
    matches: f32,
}

impl Clusterer {
    pub fn new(thresholds: Thresholds) -> Self {
        Self { thresholds, speakers: Vec::new(), last: None }
    }

    /// The label of a phrase `seconds` long with this embedding. A phrase too short, or with no embedding, takes
    /// the previous label (0 at the start) and changes nothing.
    pub fn assign(&mut self, embedding: &[f32], seconds: f32) -> u32 {
        let label = if !self.thresholds.embeds(seconds) || embedding.is_empty() {
            self.last.unwrap_or(0)
        } else {
            self.match_or_add(embedding)
        };
        self.last = Some(label);
        label
    }

    /// The label of a part of a split phrase, or of a whole phrase, `seconds` long: as [`assign`](Self::assign) when
    /// long enough to cluster, else the known speaker closest to its embedding if that passes `similarity` (a short part
    /// follows a change, so the previous label is likelier wrong), else the previous label.
    pub fn assign_part(&mut self, embedding: &[f32], seconds: f32) -> u32 {
        if self.thresholds.embeds(seconds) {
            return self.assign(embedding, seconds);
        }
        let best = self.speakers.iter().enumerate().map(|(i, s)| (i as u32, cosine(&s.centroid, embedding))).max_by(|a, b| a.1.total_cmp(&b.1));
        let label = match best {
            Some((label, score)) if !embedding.is_empty() && score >= self.thresholds.similarity => label,
            _ => self.last.unwrap_or(0),
        };
        self.last = Some(label);
        label
    }

    /// The running mean of a speaker's confident matches.
    pub fn centroid(&self, label: u32) -> Option<&[f32]> {
        self.speakers.get(label as usize).map(|speaker| speaker.centroid.as_slice())
    }

    fn match_or_add(&mut self, embedding: &[f32]) -> u32 {
        let mut scores: Vec<(u32, f32)> = self.speakers.iter().enumerate().map(|(i, s)| (i as u32, cosine(&s.centroid, embedding))).collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        match scores.as_slice() {
            [(best, score), rest @ ..] if *score >= self.thresholds.similarity => {
                let second = rest.first().map_or(f32::NEG_INFINITY, |&(_, score)| score);
                if score - second >= self.thresholds.margin {
                    self.speakers[*best as usize].absorb(embedding);
                }
                *best
            }
            _ => {
                self.speakers.push(Speaker { centroid: embedding.to_vec(), matches: 1.0 });
                self.speakers.len() as u32 - 1
            }
        }
    }
}

impl Speaker {
    fn absorb(&mut self, embedding: &[f32]) {
        self.matches += 1.0;
        for (c, e) in self.centroid.iter_mut().zip(embedding) {
            *c += (e - *c) / self.matches;
        }
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b)).max(f32::EPSILON)
}

/// Audio at a phrase's end (seconds) that may stay outside every window before one more is added.
const MIN_TAIL: f32 = 0.1;

/// The windows a phrase is cut into to find where its speaker changes (`.loop/voices/progress.md`, V4's splitting).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowParams {
    /// Seconds each window lasts.
    pub window: f32,
    /// Seconds from one window's start to the next's.
    pub step: f32,
    /// Windows whose cosine similarity with the next is under this belong to different speakers.
    pub threshold: f32,
}

impl Default for WindowParams {
    /// The values tuned on the benchmark's tune half.
    fn default() -> Self {
        Self { window: 0.7, step: 0.7, threshold: 0.25 }
    }
}

/// The embeddings of a phrase's windows. They serve twice: where consecutive ones stop matching the speaker
/// changes, and the mean of those inside a part is the part's voice, so a part is embedded again only when no
/// window fits inside it.
pub struct Windows {
    /// Each window's start (seconds from the phrase's start) and embedding (empty for one too quiet).
    windows: Vec<(f32, Vec<f32>)>,
    params: WindowParams,
}

impl Windows {
    /// Embeds the windows of `samples`, `step` apart from the start, and one last window ending where the audio
    /// does when more than [`MIN_TAIL`] seconds would stay uncovered, so a change near the end can be found.
    pub fn embed(embedder: &Embedder, samples: &[f32], params: WindowParams) -> Self {
        let at = |seconds: f32| (seconds * SAMPLE_RATE as f32) as usize;
        let length = at(params.window);
        let mut starts: Vec<usize> = (0..).map(|i| at(i as f32 * params.step)).take_while(|&start| start + length <= samples.len()).collect();
        if starts.last().is_some_and(|last| samples.len() - (last + length) > at(MIN_TAIL)) {
            starts.push(samples.len() - length);
        }
        let windows = starts.into_iter().map(|start| (start as f32 / SAMPLE_RATE as f32, embedder.embed(&samples[start..start + length]))).collect();
        Self { windows, params }
    }

    /// No windows at all.
    pub fn none(params: WindowParams) -> Self {
        Self { windows: Vec::new(), params }
    }

    /// Where the speaker changes, seconds from the phrase's start. (A change next to the last, irregular
    /// window is placed as if it were a step later: at most half a step off.)
    pub fn cuts(&self) -> Vec<f32> {
        let WindowParams { window, step, threshold } = self.params;
        let similarity = |pair: &[(f32, Vec<f32>)]| if pair[0].1.is_empty() || pair[1].1.is_empty() { 1.0 } else { cosine(&pair[0].1, &pair[1].1) };
        let scores: Vec<f32> = self.windows.windows(2).map(similarity).collect();
        split_points(&scores, window, step, threshold)
    }

    /// The mean embedding of the windows wholly inside `from..to` seconds; empty when there are none.
    pub fn mean(&self, from: f32, to: f32) -> Vec<f32> {
        const SLACK: f32 = 1e-3;
        let window = self.params.window;
        let inside = self.windows.iter().filter(|(start, embedding)| *start >= from - SLACK && start + window <= to + SLACK && !embedding.is_empty());
        let mut sum: Vec<f32> = Vec::new();
        let mut count = 0.0;
        for (_, embedding) in inside {
            sum.resize(embedding.len(), 0.0);
            sum.iter_mut().zip(embedding).for_each(|(s, e)| *s += e);
            count += 1.0;
        }
        sum.iter_mut().for_each(|s| *s /= count);
        sum
    }
}

/// Where a phrase changes speaker, from the similarity of consecutive embedding windows (`window` seconds
/// long, every `step` seconds): each run of scores under `threshold` is one change, placed at the run's lowest
/// score, halfway between the two windows' centres. Returns seconds from the phrase's start.
pub fn split_points(scores: &[f32], window: f32, step: f32, threshold: f32) -> Vec<f32> {
    let mut points = Vec::new();
    let mut at = 0;
    while at < scores.len() {
        let run = scores[at..].iter().take_while(|&&score| score < threshold).count();
        if run == 0 {
            at += 1;
            continue;
        }
        let lowest = (at..at + run).min_by(|&a, &b| scores[a].total_cmp(&scores[b])).unwrap_or(at);
        points.push((lowest as f32 + 0.5) * step + window / 2.0);
        at += run;
    }
    points
}

/// A token of a transcript and when it starts, seconds from the phrase's start. A token starting with a space
/// or `▁` starts a word.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub text: String,
    pub start: f32,
}

/// The text of `tokens` cut at each time in `at` (ascending): `at.len() + 1` parts, trimmed, possibly empty.
/// A word goes whole to the part its first token starts in.
pub fn split_text(tokens: &[Token], at: &[f32]) -> Vec<String> {
    const WORD_START: [char; 2] = [' ', '\u{2581}'];
    let mut parts = vec![String::new(); at.len() + 1];
    let mut part = 0;
    for (i, token) in tokens.iter().enumerate() {
        if i == 0 || token.text.starts_with(WORD_START) {
            part = at.partition_point(|&time| time <= token.start);
            if !parts[part].is_empty() {
                parts[part].push(' ');
            }
        }
        parts[part].push_str(token.text.trim_start_matches(WORD_START));
    }
    parts.iter().map(|part| part.trim().to_owned()).collect()
}

/// A piece of a phrase with its own speaker.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub text: String,
    pub voice: Option<u32>,
    /// Seconds from the stream's start.
    pub start: f32,
}

/// Neighbouring parts with the same voice as one part: a cut between them was no change of speaker.
pub fn join_same_voice(parts: Vec<Part>) -> Vec<Part> {
    let mut joined: Vec<Part> = Vec::new();
    for part in parts {
        match joined.last_mut() {
            Some(last) if last.voice == part.voice => {
                last.text.push(' ');
                last.text.push_str(&part.text);
            }
            _ => joined.push(part),
        }
    }
    joined
}

/// A phrase's parts as complete lines in time order, empty parts left out, numbered from `next_id`
/// (which is advanced past them).
pub fn into_lines(mut parts: Vec<Part>, next_id: &mut u64) -> Vec<Line> {
    parts.retain(|part| !part.text.trim().is_empty());
    parts.sort_by(|a, b| a.start.total_cmp(&b.start));
    parts
        .into_iter()
        .map(|part| {
            *next_id += 1;
            Line { id: *next_id - 1, text: part.text, complete: true, voice: part.voice }
        })
        .collect()
}

/// Gives one stream's phrases their speakers: each is cut where the speaker changes and its parts are
/// labelled by an online [`Clusterer`]. Everything it learns about voices stays in memory, in the stream.
pub struct Labeller<'a> {
    embedder: &'a Embedder,
    clusterer: Clusterer,
    windows: WindowParams,
    split: bool,
}

impl<'a> Labeller<'a> {
    /// A labeller with the tuned thresholds and windows (`.loop/voices/progress.md`); `split` cuts phrases at a
    /// change of speaker, else each is labelled whole.
    pub fn new(embedder: &'a Embedder, split: bool) -> Self {
        Self { embedder, clusterer: Clusterer::new(Thresholds::default()), windows: WindowParams::default(), split }
    }

    /// The parts of `phrase`, whose speech is `audio`, each with its voice; neighbours of one voice are joined.
    pub fn parts(&mut self, phrase: &Phrase, audio: &[f32]) -> Vec<Part> {
        let windows = match self.split && !phrase.tokens.is_empty() {
            true => Windows::embed(self.embedder, audio, self.windows),
            false => Windows::none(self.windows),
        };
        let cuts = windows.cuts();
        let texts = if cuts.is_empty() { vec![phrase.text.clone()] } else { split_text(&phrase.tokens, &cuts) };
        let edges: Vec<f32> = std::iter::once(0.0).chain(cuts).chain([phrase.end - phrase.start]).collect();
        let clip = |from: f32, to: f32| {
            let sample = |seconds: f32| ((seconds * SAMPLE_RATE as f32) as usize).min(audio.len());
            &audio[sample(from)..sample(to)]
        };
        let parts = texts.into_iter().zip(edges.windows(2)).filter(|(text, _)| !text.is_empty()).map(|(text, edge)| {
            let mut embedding = windows.mean(edge[0], edge[1]);
            if embedding.is_empty() {
                embedding = self.embedder.embed(clip(edge[0], edge[1]));
            }
            Part { text, voice: Some(self.clusterer.assign_part(&embedding, edge[1] - edge[0])), start: phrase.start + edge[0] }
        });
        join_same_voice(parts.collect())
    }
}

/// A stretch of one speaker found by speaker segmentation (pyannote segmentation-3.0 through sherpa-onnx's
/// diarization), seconds from the audio's start.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub speaker: u32,
    pub start: f32,
    pub end: f32,
}

/// Speaker segmentation of short audio, overlap included.
pub struct Segmenter {
    diarization: OfflineSpeakerDiarization,
}

impl Segmenter {
    /// Loads pyannote's segmentation model (`model.onnx` of segmentation-3.0) and the embedding model.
    pub fn load(segmentation: &Path, embedding: &Path) -> Result<Self> {
        let mut config = OfflineSpeakerDiarizationConfig::default();
        config.segmentation.pyannote.model = Some(segmentation.to_string_lossy().into_owned());
        config.embedding.model = Some(embedding.to_string_lossy().into_owned());
        // Its windows needn't overlap: shifting by a tenth costs ten times as much for the same segments.
        config.segmentation.pyannote.window_shift_ratio = 1.0;
        let diarization = OfflineSpeakerDiarization::create(&config).ok_or_else(|| format!("can't load the segmentation model {}", segmentation.display()))?;
        Ok(Self { diarization })
    }

    /// The stretches of each speaker in 16 kHz mono audio, in order of start.
    pub fn segments(&self, samples: &[f32]) -> Vec<Segment> {
        let Some(result) = self.diarization.process(samples) else { return Vec::new() };
        let segments = result.sort_by_start_time();
        segments.into_iter().map(|s| Segment { speaker: s.speaker.max(0) as u32, start: s.start, end: s.end }).collect()
    }
}

/// The stretches where two or more speakers' segments overlap, merged, in time order, as (start, end) seconds.
pub fn overlaps(segments: &[Segment]) -> Vec<(f32, f32)> {
    let mut times: Vec<f32> = segments.iter().flat_map(|segment| [segment.start, segment.end]).collect();
    times.sort_by(f32::total_cmp);
    times.dedup();
    let mut found: Vec<(f32, f32)> = Vec::new();
    for pair in times.windows(2) {
        let mut speakers: Vec<u32> = segments.iter().filter(|s| s.start <= pair[0] && pair[1] <= s.end).map(|s| s.speaker).collect();
        speakers.sort_unstable();
        speakers.dedup();
        if speakers.len() < 2 {
            continue;
        }
        match found.last_mut() {
            Some(last) if last.1 == pair[0] => last.1 = pair[1],
            _ => found.push((pair[0], pair[1])),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{join_same_voice, middle, too_quiet, Clusterer, Embedder, Part, Thresholds, WindowParams, Windows};

    #[test]
    fn a_speaker_model_that_cannot_load_means_voices_off() {
        let bad = std::env::temp_dir().join("kiku-no-such-speaker-model.onnx");
        assert!(Embedder::load_or_none(&bad).is_none());
    }

    #[test]
    fn parts_of_one_voice_are_joined() {
        let part = |text: &str, voice, start| Part { text: text.into(), voice: Some(voice), start };
        let parts = vec![part("so I said", 0, 1.0), part("to him", 0, 2.0), part("really?", 1, 3.0), part("yes", 0, 4.0)];
        assert_eq!(join_same_voice(parts), [part("so I said to him", 0, 1.0), part("really?", 1, 3.0), part("yes", 0, 4.0)]);
    }

    fn windows(embeddings: &[[f32; 2]]) -> Windows {
        Windows { windows: embeddings.iter().enumerate().map(|(i, e)| (i as f32, e.to_vec())).collect(), params: WindowParams { window: 1.0, step: 1.0, threshold: 0.5 } }
    }

    #[test]
    fn windows_find_the_change_and_give_each_part_its_voice() {
        let windows = windows(&[[1.0, 0.0], [1.0, 0.1], [0.0, 1.0], [0.1, 1.0]]);
        assert_eq!(windows.cuts(), [2.0]);
        assert_eq!(windows.mean(0.0, 2.0), [1.0, 0.05]);
        assert_eq!(windows.mean(2.0, 4.0), [0.05, 1.0]);
        assert!(windows.mean(0.0, 0.5).is_empty(), "no window fits inside");
    }

    #[test]
    fn a_short_part_takes_the_nearest_known_speaker() {
        let mut clusterer = Clusterer::new(Thresholds::default());
        assert_eq!(clusterer.assign_part(&[1.0, 0.0], 2.0), 0);
        assert_eq!(clusterer.assign_part(&[0.0, 1.0], 2.0), 1);
        assert_eq!(clusterer.assign_part(&[0.9, 0.1], 0.6), 0, "closest, though the last label was 1");
        assert_eq!(clusterer.assign_part(&[-1.0, -1.0], 0.6), 0, "no one close: the previous label");
        assert_eq!(clusterer.centroid(0), Some([1.0, 0.0].as_slice()), "a short part moves no centroid");
        assert_eq!(clusterer.assign_part(&[], 0.6), 0);
    }

    #[test]
    fn middle_keeps_the_centre_of_long_audio_only() {
        let samples: Vec<f32> = (0..10).map(|i| i as f32).collect();
        assert_eq!(middle(&samples, 4), [3.0, 4.0, 5.0, 6.0]);
        assert_eq!(middle(&samples, 10), samples);
        assert_eq!(middle(&samples, 20), samples);
    }

    #[test]
    fn near_silence_is_too_quiet_to_embed() {
        assert!(too_quiet(&[]));
        assert!(too_quiet(&[5e-4, -5e-4, 5e-4]), "-66 dBFS");
        assert!(!too_quiet(&[3e-3, -3e-3, 3e-3]), "-50 dBFS still embeds");
    }
}
