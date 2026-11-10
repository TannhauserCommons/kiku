//! sherpa-onnx models that transcribe whole phrases: Parakeet, Whisper, SenseVoice, ...
//!
//! A speech detector cuts the audio into phrases, and each is shown once it ends. That trades a second or two of delay for accuracy and costs nothing in silence.

use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::Path,
    sync::Mutex,
};

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineStream, VadModelConfig, VoiceActivityDetector};

use crate::{
    engine::{Device, Engine, EngineStream, Hotword, Line, ModelSpec, PhraseModel, Result, SAMPLE_RATE},
    transcripts,
};

/// Inference threads on the CPU; the 600M model is too slow on one.
const THREADS: i32 = 2;
const VAD_WINDOW: usize = 512;
/// A phrase longer than this is cut, so lines don't grow without end.
const MAX_PHRASE_SECONDS: f32 = 15.0;
/// Audio kept from before where the detector says a phrase starts: it reacts late to soft onsets.
const LEAD_IN_SAMPLES: i64 = SAMPLE_RATE as i64 / 2;
/// Audio kept after where the detector says a phrase ends: it calls a fading last word silence,
/// and the model needs a moment after the last word to finish it.
const TRAIL_SAMPLES: i64 = SAMPLE_RATE as i64 / 2;

pub struct PhraseEngine {
    recognizer: OfflineRecognizer,
    vad: VadModelConfig,
    kind: PhraseModel,
    /// The words to listen for, while the recognizer decodes for them.
    hotwords: Option<Mutex<Vec<Hotword>>>,
}

impl PhraseEngine {
    pub fn load(spec: &ModelSpec, kind: PhraseModel) -> Result<Self> {
        let mut config = OfflineRecognizerConfig::default();
        let model = &mut config.model_config;
        let file = |part| model_file(&spec.dir, part, spec.device);
        match kind {
            PhraseModel::NemoTransducer | PhraseModel::Transducer => {
                model.transducer.encoder = Some(file("encoder")?);
                model.transducer.decoder = Some(file("decoder")?);
                model.transducer.joiner = Some(file("joiner")?);
                if kind == PhraseModel::NemoTransducer {
                    model.model_type = Some("nemo_transducer".into());
                }
            }
            PhraseModel::NemoCtc => model.nemo_ctc.model = Some(file("model")?),
            PhraseModel::Paraformer => model.paraformer.model = Some(file("model")?),
            PhraseModel::SenseVoice => {
                model.sense_voice.model = Some(file("model")?);
                model.sense_voice.use_itn = true;
            }
            PhraseModel::Whisper => {
                model.whisper.encoder = Some(file("encoder")?);
                model.whisper.decoder = Some(file("decoder")?);
                model.whisper.task = Some("transcribe".into());
                // The model's own padding.
                model.whisper.tail_paddings = -1;
            }
            PhraseModel::Moonshine => {
                model.moonshine.preprocessor = Some(file("preprocess")?);
                model.moonshine.encoder = Some(file("encode")?);
                model.moonshine.uncached_decoder = Some(file("uncached_decode")?);
                model.moonshine.cached_decoder = Some(file("cached_decode")?);
            }
        }
        let tokens = tokens_file(&spec.dir)?;
        model.tokens = Some(tokens.clone());
        model.num_threads = THREADS;
        model.provider = Some(provider(spec.device).into());
        let hotwords = (kind == PhraseModel::NemoTransducer && !spec.hotwords.is_empty()).then(|| spec.hotwords.clone());
        if hotwords.is_some() {
            decode_for_hotwords(&mut config, Path::new(&tokens))?;
        }
        let recognizer = OfflineRecognizer::create(&config).ok_or("the model failed to load")?;
        let vad = vad_config(spec.vad.as_deref().ok_or("no speech detector given")?)?;
        Ok(Self { recognizer, vad, kind, hotwords: hotwords.map(Mutex::new) })
    }

    /// A stream for one phrase, listening for the hotwords if there are any.
    fn create_stream(&self) -> OfflineStream {
        match &self.hotwords {
            Some(hotwords) => self.recognizer.create_stream_with_hotwords(&hotwords_text(&hotwords.lock().unwrap())),
            None => self.recognizer.create_stream(),
        }
    }
}

impl Engine for PhraseEngine {
    fn open_stream(&self) -> Result<Box<dyn EngineStream + '_>> {
        Ok(Box::new(PhraseStream {
            engine: self,
            vad: VoiceActivityDetector::create(&self.vad, MAX_PHRASE_SECONDS * 2.0).ok_or("speech detector failed to load")?,
            pending: Vec::new(),
            heard: History::default(),
            phrase_end: 0,
            line: 0,
        }))
    }

    fn set_hotwords(&self, words: &[Hotword]) -> bool {
        match &self.hotwords {
            Some(hotwords) if !words.is_empty() => {
                *hotwords.lock().unwrap() = words.to_vec();
                true
            }
            Some(_) => false,
            None => words.is_empty() || self.kind != PhraseModel::NemoTransducer,
        }
    }
}

/// Phrases cut by the speech detector, each transcribed once complete.
struct PhraseStream<'a> {
    engine: &'a PhraseEngine,
    vad: VoiceActivityDetector,
    /// Audio not yet a whole detector window.
    pending: Vec<f32>,
    /// Recent audio, so a phrase can be transcribed with its lead-in.
    heard: History,
    /// Where the last phrase ended, so lead-in never repeats it.
    phrase_end: i64,
    line: u64,
}

impl EngineStream for PhraseStream<'_> {
    fn push(&mut self, audio: &[f32]) -> Result<()> {
        self.pending.extend_from_slice(audio);
        let whole = self.pending.len() / VAD_WINDOW * VAD_WINDOW;
        for window in self.pending[..whole].as_chunks::<VAD_WINDOW>().0 {
            self.vad.accept_waveform(window);
            self.heard.push(window);
        }
        self.pending.drain(..whole);
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<Line>> {
        let mut lines = Vec::new();
        while let Some(segment) = self.vad.front() {
            self.vad.pop();
            let start = (segment.start() - LEAD_IN_SAMPLES).max(self.phrase_end);
            let end = (segment.start() + segment.n() as i64 + TRAIL_SAMPLES).min(self.heard.end());
            self.phrase_end = end;
            let text = self.transcribe(self.heard.range(start, end));
            if !text.is_empty() {
                lines.push(Line { id: self.line, text, complete: true });
                self.line += 1;
            }
        }
        Ok(lines)
    }

    fn flush(&mut self) -> Result<Vec<Line>> {
        self.vad.flush();
        self.poll()
    }
}

impl PhraseStream<'_> {
    fn transcribe(&self, samples: &[f32]) -> String {
        let stream = self.engine.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.engine.recognizer.decode(&stream);
        let text = stream.get_result().map(|r| r.text.trim().to_owned()).unwrap_or_default();
        match &self.engine.hotwords {
            Some(hotwords) => without_repeats(&text, &hotwords.lock().unwrap()),
            None => text,
        }
    }
}

/// The last stretch of a stream's audio, addressed by sample position since the stream began.
#[derive(Default)]
struct History {
    samples: Vec<f32>,
    /// Position of `samples[0]`.
    start: i64,
}

/// Enough for the longest phrase plus the detector's delay in ending it.
const HISTORY_SAMPLES: usize = 2 * MAX_PHRASE_SECONDS as usize * SAMPLE_RATE as usize;

impl History {
    fn push(&mut self, audio: &[f32]) {
        self.samples.extend_from_slice(audio);
        if self.samples.len() > 2 * HISTORY_SAMPLES {
            let dropped = self.samples.len() - HISTORY_SAMPLES;
            self.samples.drain(..dropped);
            self.start += dropped as i64;
        }
    }

    /// The position just after the newest sample.
    fn end(&self) -> i64 {
        self.start + self.samples.len() as i64
    }

    /// The audio from `start` to `end`, clipped to what is still kept.
    fn range(&self, start: i64, end: i64) -> &[f32] {
        let index = |at: i64| (at - self.start).clamp(0, self.samples.len() as i64) as usize;
        &self.samples[index(start)..index(end)]
    }
}

fn provider(device: Device) -> &'static str {
    if device == Device::Cuda { "cuda" } else { "cpu" }
}

/// The ONNX file for `part` of a model, named the ways sherpa-onnx's exports name them: `encoder.int8.onnx`,
/// `encoder-epoch-99-avg-1.onnx`, `tiny.en-encoder.onnx`, `model.onnx`. If a folder has several
/// precisions, int8 is preferred on the CPU and avoided on the GPU, where it would bounce between devices.
fn model_file(dir: &Path, part: &str, device: Device) -> Result<String> {
    let mut found: Vec<String> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| is_part(&name.to_lowercase(), part))
        .collect();
    found.sort_by_key(|name| (name.contains(".int8.") != (device == Device::Cpu), name.clone()));
    let name = found.first().ok_or_else(|| format!("no {part} model in {}", dir.display()))?;
    Ok(dir.join(name).to_string_lossy().into_owned())
}

/// Whether `name` is an ONNX file for `part`: one of its `-`-separated pieces starts with `<part>.`.
pub(crate) fn is_part(name: &str, part: &str) -> bool {
    let Some(stem) = name.strip_suffix(".onnx") else { return false };
    stem.split('-').any(|piece| piece.split('.').next() == Some(part))
}

/// `tokens.txt`, or Whisper's `<name>-tokens.txt`.
fn tokens_file(dir: &Path) -> Result<String> {
    let found = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .find(|path| path.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with("tokens.txt")));
    found.map(|path| path.to_string_lossy().into_owned()).ok_or_else(|| format!("no tokens.txt in {}", dir.display()).into())
}

/// sherpa-onnx's per-stream hotwords: a phrase per `/`-separated line, its words split by spaces, then
/// its boost as a score (`:2.5`). Pieces it would read as a line break or a score are left out of the words.
fn hotwords_text(words: &[Hotword]) -> String {
    let phrases = words.iter().filter_map(|word| {
        let phrase = hotword_pieces(word).join(" ");
        (!phrase.is_empty()).then(|| format!("{phrase} :{}", word.boost))
    });
    phrases.collect::<Vec<_>>().join("/")
}

fn hotword_pieces(word: &Hotword) -> Vec<&str> {
    word.text.split(|c: char| c == '/' || c.is_whitespace()).filter(|piece| !piece.is_empty() && !piece.starts_with(':')).collect()
}

/// `text` with a hotword decoded twice or more in a row written once: beam search can repeat a boosted word
/// said once ("Nguyen Nguyen"), the more so the stronger it is. The last copy stays, with what follows it.
fn without_repeats(text: &str, words: &[Hotword]) -> String {
    let bare = |piece: &str| piece.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let mut pieces: Vec<&str> = text.split_whitespace().collect();
    let before = pieces.len();
    for word in words {
        let phrase: Vec<String> = hotword_pieces(word).into_iter().map(bare).collect();
        let n = phrase.len();
        let at_phrase = |pieces: &[&str], at: usize| n > 0 && at + n <= pieces.len() && pieces[at..at + n].iter().map(|p| bare(p)).eq(phrase.iter().cloned());
        let mut at = 0;
        while at < pieces.len() {
            if at_phrase(&pieces, at) && at_phrase(&pieces, at + n) {
                pieces.drain(at..at + n);
            } else {
                at += 1;
            }
        }
    }
    if pieces.len() == before { text.to_owned() } else { pieces.join(" ") }
}

/// Has the recognizer decode with modified beam search, which hotwords need. sherpa-onnx splits a
/// hotword into the model's tokens with a SentencePiece vocabulary, which the downloads lack, so one
/// is made from `tokens.txt` and kept in the temporary folder, named by its contents.
fn decode_for_hotwords(config: &mut OfflineRecognizerConfig, tokens: &Path) -> Result<()> {
    let vocab = bpe_vocab(&fs::read_to_string(tokens)?);
    let mut hasher = DefaultHasher::new();
    vocab.hash(&mut hasher);
    let path = std::env::temp_dir().join(format!("kiku-{:016x}.vocab", hasher.finish()));
    if !path.is_file() {
        transcripts::replace(&path, &vocab)?;
    }
    config.decoding_method = Some("modified_beam_search".into());
    config.model_config.modeling_unit = Some("bpe".into());
    config.model_config.bpe_vocab = Some(path.to_string_lossy().into_owned());
    Ok(())
}

/// A SentencePiece vocabulary (`piece score` lines) from `tokens.txt` (`piece id` lines). A BPE
/// tokenizer's ids follow its merges, scored about minus the id, so pieces are chosen as it would.
fn bpe_vocab(tokens: &str) -> String {
    let piece = |line: &str| match line.split_whitespace().collect::<Vec<_>>()[..] {
        [piece, id] => Some(format!("{piece} -{}\n", id.parse::<u32>().ok()?)),
        _ => None,
    };
    tokens.lines().filter_map(piece).collect()
}

/// The speech detector settings sherpa-onnx's own Parakeet examples use, with a longer phrase limit,
/// a shorter shortest one, and phrases cut only at longer pauses so each has more context.
fn vad_config(model: &Path) -> Result<VadModelConfig> {
    if !model.is_file() {
        return Err(format!("speech detector model missing: {}", model.display()).into());
    }
    let mut config = VadModelConfig::default();
    config.silero_vad.model = Some(model.to_string_lossy().into_owned());
    config.silero_vad.threshold = 0.5;
    config.silero_vad.min_silence_duration = 0.5;
    // Speech shorter than this is dropped as noise; 0.25 s dropped quick single words.
    config.silero_vad.min_speech_duration = 0.1;
    config.silero_vad.max_speech_duration = MAX_PHRASE_SECONDS;
    config.silero_vad.window_size = VAD_WINDOW as i32;
    config.sample_rate = SAMPLE_RATE as i32;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_files_are_found_by_part() {
        for (name, part) in [("encoder.int8.onnx", "encoder"), ("encoder-epoch-99-avg-1.onnx", "encoder"), ("tiny.en-decoder.onnx", "decoder"), ("model.onnx", "model"), ("uncached_decode.int8.onnx", "uncached_decode")] {
            assert!(is_part(name, part), "{name}");
        }
        for (name, part) in [("encoder.weights", "encoder"), ("uncached_decode.onnx", "cached_decode"), ("encoder.onnx", "encode"), ("silero_vad.onnx", "model")] {
            assert!(!is_part(name, part), "{name}");
        }
    }

    #[test]
    fn hotwords_become_sherpa_lines() {
        let words = ["Kiku", " New  York ", "AC/DC", "x :2.0", "", ":3"].map(|text| Hotword { text: text.into(), boost: 1.5 });
        let strong = Hotword { text: "Lull".into(), boost: 2.5 };
        assert_eq!(hotwords_text(&[&words[..], &[strong]].concat()), "Kiku :1.5/New York :1.5/AC DC :1.5/x :1.5/Lull :2.5");
    }

    #[test]
    fn a_hotword_repeated_back_to_back_is_written_once() {
        let words = ["Nguyen", "New York", "Kiku"].map(|text| Hotword { text: text.into(), boost: 2.5 });
        assert_eq!(without_repeats("Dr. Nguyen Nguyen Nguyen will call back.", &words), "Dr. Nguyen will call back.");
        assert_eq!(without_repeats("I think Kiku, kiku missed that word", &words), "I think kiku missed that word");
        assert_eq!(without_repeats("in New York New York.", &words), "in New York.");
        assert_eq!(without_repeats("Kiku and Kiku again", &words), "Kiku and Kiku again");
        assert_eq!(without_repeats("No  no, Kiku.", &words), "No  no, Kiku.", "untouched text keeps its spacing");
    }

    #[test]
    fn tokens_become_a_scored_vocabulary() {
        assert_eq!(bpe_vocab("<unk> 0\n▁t 1\nbroken\nx y\n<blk> 1024\n"), "<unk> -0\n▁t -1\n<blk> -1024\n");
    }
}
