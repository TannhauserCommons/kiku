//! The voices benchmark (`.loop/voices/SPINE.md`): seeded conversations built in memory from LibriSpeech, run
//! through Kiku's Parakeet Unified (CPU) as the app feeds it, and scored by `voices::metrics`.
//!
//! Usage: voices_bench --set <LibriSpeech test-clean> [--condition <name>[,<name>...]|all]
//!   [--method baseline|labels|split-windows|overlap] [--conversations N] [--half tune|test|all] [--opus] [--json out.jsonl]
//!   `baseline` is Kiku with voices off; the others turn voices on in the engine's own phrase stream (`labels` labels whole phrases,
//!   `split-windows` is what ships).
//!   `overlap` is `split-windows` plus overlap detection (pyannote segmentation-3.0 over each whole conversation, offline: an upper
//!   bound on what a live detector would find), scored against the truth's overlap in seconds and in lines.
//! Needs `ffmpeg` on PATH and Parakeet Unified where `KIKU_MODELS_DIR` points (the label and split methods also the
//! TitaNet-small model, `overlap` also `sherpa-onnx-pyannote-segmentation-3-0/` in `$KIKU_MODELS_DIR`). Prints numbers only.

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Instant,
};

use kiku_core::{
    catalog, paths,
    engine::{Backend, Device, EngineStream, ModelSpec, Result, SAMPLE_RATE},
    sherpa::{Phrase, PhraseEngine, PhraseStream},
    voices::{
        conversation::{build, Condition, Conversation, Utterance},
        metrics::{change_scores, cp_wer, line_accuracy, overlap_fractions, overlap_seconds, two_voice_share, wer, ChangeScores, Errors, FractionError, Hyp, HypLine, Overlap, Share, Truth},
        overlaps, Segmenter,
    },
};

/// Utterances used per speaker (the first by file name), so the set fits in memory.
const UTTERANCES_PER_SPEAKER: usize = 12;
/// What the app feeds the engine at a time.
const CHUNK: usize = SAMPLE_RATE as usize / 10;
/// The shortest line the speaker accuracy target counts (seconds).
const LONG_LINE: f32 = 1.5;
const EMBEDDING_MODEL: &str = "nemo_en_titanet_small.onnx";
const SEGMENTATION_MODEL: &str = "sherpa-onnx-pyannote-segmentation-3-0/model.onnx";

struct Options {
    set: PathBuf,
    conditions: Vec<Condition>,
    method: String,
    seeds: Vec<u64>,
    opus: bool,
    json: Option<PathBuf>,
}

fn main() -> Result<()> {
    let options = parse(std::env::args().skip(1).collect())?;
    let utterances = load_utterances(&options.set)?;
    let model = catalog::by_id("parakeet-unified").ok_or("no Parakeet Unified in the catalog")?;
    let voices = (options.method != "baseline").then(|| paths::models_dir().join(EMBEDDING_MODEL));
    let spec = ModelSpec { voices, ..model.spec(model.variant(Device::Cpu)) };
    let Backend::Phrases(kind) = spec.backend else { return Err("Parakeet isn't a phrase model".into()) };
    let engine = PhraseEngine::load(&spec, kind)?;
    let segmenter = if options.method == "overlap" { Some(Segmenter::load(&paths::models_dir().join(SEGMENTATION_MODEL), &paths::models_dir().join(EMBEDDING_MODEL))?) } else { None };
    let whole_phrases = options.method == "labels";

    for condition in &options.conditions {
        let started = Instant::now();
        let mut total = Totals::default();
        for &seed in &options.seeds {
            let mut conversation = build(&utterances, condition, seed);
            if options.opus {
                conversation.audio = through_opus(&conversation.audio)?;
            }
            total.add(&engine, whole_phrases, segmenter.as_ref(), &conversation, &format!("{} seed {seed}", condition.name()))?;
        }
        let line = total.line(&condition.name(), &options.method, options.seeds.len());
        println!("{line}");
        if let Some(path) = &options.json {
            let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{}", total.json(&condition.name(), &options.method, options.seeds.len(), started.elapsed().as_secs_f64()))?;
        }
    }
    Ok(())
}

fn parse(args: Vec<String>) -> Result<Options> {
    let (mut set, mut condition, mut method, mut count, mut half) = (None, "all".to_string(), "baseline".to_string(), 10u64, "all".to_string());
    let (mut opus, mut json) = (false, None);
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--set" => set = Some(PathBuf::from(value()?)),
            "--condition" => condition = value()?,
            "--method" => method = value()?,
            "--conversations" => count = value()?.parse()?,
            "--half" => {
                half = value()?;
                if !matches!(half.as_str(), "tune" | "test" | "all") {
                    return Err(format!("--half takes tune, test or all, not {half}").into());
                }
            }
            "--opus" => opus = true,
            "--json" => json = Some(PathBuf::from(value()?)),
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    if !matches!(method.as_str(), "baseline" | "labels" | "split-windows" | "overlap") {
        return Err(format!("method {method} isn't built yet").into());
    }
    let conditions = match condition.as_str() {
        "all" => Condition::all(),
        names => names.split(',').map(|name| Condition::parse(name).ok_or_else(|| format!("unknown condition {name}"))).collect::<std::result::Result<_, _>>()?,
    };
    let seeds = (0..count).filter(|seed| match half.as_str() {
        "tune" => seed % 2 == 0,
        "test" => seed % 2 == 1,
        _ => true, // all
    });
    Ok(Options { set: set.ok_or("--set <LibriSpeech test-clean folder> is needed")?, conditions, method, seeds: seeds.collect(), opus, json })
}

/// The first utterances of each speaker, decoded by ffmpeg.
fn load_utterances(set: &Path) -> Result<Vec<Utterance>> {
    let mut utterances = Vec::new();
    for speaker in sorted_entries(set)? {
        let mut transcripts = Vec::new();
        for chapter in sorted_entries(&speaker)? {
            for file in sorted_entries(&chapter)? {
                let name = file.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with(".trans.txt") {
                    for line in fs::read_to_string(&file)?.lines() {
                        let (id, text) = line.split_once(' ').ok_or("a transcript line without text")?;
                        transcripts.push((chapter.join(format!("{id}.flac")), text.to_lowercase()));
                    }
                }
            }
        }
        transcripts.sort();
        for (flac, text) in transcripts.into_iter().take(UTTERANCES_PER_SPEAKER) {
            let audio = decode(&["-i", flac.to_str().unwrap(), "-f", "f32le", "-ar", "16000", "-ac", "1", "-"], Vec::new())?;
            utterances.push(Utterance { speaker: speaker.file_name().unwrap().to_string_lossy().into_owned(), text, audio: floats(&audio) });
        }
    }
    Ok(utterances)
}

fn sorted_entries(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(folder)?.map(|e| e.map(|e| e.path())).collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

/// The audio as Discord sends it: Opus at 32 kbit/s, decoded again.
fn through_opus(audio: &[f32]) -> Result<Vec<f32>> {
    let raw: Vec<u8> = audio.iter().flat_map(|x| x.to_le_bytes()).collect();
    let opus = decode(&["-f", "f32le", "-ar", "16000", "-ac", "1", "-i", "-", "-c:a", "libopus", "-b:a", "32k", "-f", "ogg", "-"], raw)?;
    Ok(floats(&decode(&["-f", "ogg", "-i", "-", "-f", "f32le", "-ar", "16000", "-ac", "1", "-"], opus)?))
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)).collect()
}

/// What ffmpeg writes to stdout for `args`, given `input` on stdin.
fn decode(args: &[&str], input: Vec<u8>) -> Result<Vec<u8>> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || stdin.write_all(&input));
    let mut output = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut output)?;
    writer.join().unwrap()?;
    if !child.wait()?.success() {
        return Err("ffmpeg failed".into());
    }
    Ok(output)
}

/// Counts and costs summed over a condition's conversations.
#[derive(Default)]
struct Totals {
    lines: usize,
    accuracy: Share,
    accuracy_long: Share,
    two_voice: Share,
    changes: ChangeScores,
    wer: Errors,
    cp_wer: Errors,
    cpu_seconds: f64,
    audio_seconds: f64,
    /// The longest the engine took to return after a chunk of audio: the worst case of decoding one phrase.
    max_decode_seconds: f64,
    /// Overlap detection (the `overlap` method): in seconds, per line, and the CPU it took.
    detected: bool,
    overlap_time: Overlap,
    overlap_fractions: FractionError,
    overlap_cpu_seconds: f64,
}

impl Totals {
    /// Runs the engine over a conversation as the app does and scores what it heard.
    fn add(&mut self, engine: &PhraseEngine, whole_phrases: bool, segmenter: Option<&Segmenter>, conversation: &Conversation, label: &str) -> Result<()> {
        let cpu = cpu_seconds();
        let (phrases, slowest) = phrases(engine, whole_phrases, &conversation.audio)?;
        self.max_decode_seconds = self.max_decode_seconds.max(slowest);
        let hyp = hyp_of(phrases);
        if let Some(segmenter) = segmenter {
            let detecting = cpu_seconds();
            let found = overlaps(&segmenter.segments(&conversation.audio));
            self.overlap_cpu_seconds += cpu_seconds() - detecting;
            self.score_overlap(&conversation.truth, &hyp, &found);
        }
        self.cpu_seconds += cpu_seconds() - cpu;
        self.audio_seconds += conversation.audio.len() as f64 / SAMPLE_RATE as f64;

        let truth = &conversation.truth;
        self.lines += hyp.lines.len();
        let (a, t) = (line_accuracy(truth, &hyp), two_voice_share(truth, &hyp));
        let long = Hyp { lines: hyp.lines.iter().filter(|line| line.end - line.start >= LONG_LINE).cloned().collect() };
        let l = line_accuracy(truth, &long);
        (self.accuracy.hits, self.accuracy.of) = (self.accuracy.hits + a.hits, self.accuracy.of + a.of);
        (self.accuracy_long.hits, self.accuracy_long.of) = (self.accuracy_long.hits + l.hits, self.accuracy_long.of + l.of);
        (self.two_voice.hits, self.two_voice.of) = (self.two_voice.hits + t.hits, self.two_voice.of + t.of);
        let conversation_wer = wer(truth, &hyp);
        if hyp.lines.is_empty() || conversation_wer.rate() >= 0.9 {
            let seconds = conversation.audio.len() as f64 / SAMPLE_RATE as f64;
            eprintln!("note: {label}: a conversation of {seconds:.0} s heard {} lines, wer={:.3}", hyp.lines.len(), conversation_wer.rate());
        }
        let c = change_scores(truth, &hyp);
        (self.changes.truth, self.changes.found, self.changes.matched) = (self.changes.truth + c.truth, self.changes.found + c.found, self.changes.matched + c.matched);
        for (sum, errors) in [(&mut self.wer, conversation_wer), (&mut self.cp_wer, cp_wer(truth, &hyp))] {
            (sum.errors, sum.words) = (sum.errors + errors.errors, sum.words + errors.words);
        }
        Ok(())
    }

    fn score_overlap(&mut self, truth: &Truth, hyp: &Hyp, found: &[(f32, f32)]) {
        self.detected = true;
        let scores = overlap_seconds(truth, found);
        let sum = &mut self.overlap_time;
        (sum.truth, sum.found, sum.both) = (sum.truth + scores.truth, sum.found + scores.found, sum.both + scores.both);
        let errors = overlap_fractions(truth, hyp, found);
        (self.overlap_fractions.lines, self.overlap_fractions.error) = (self.overlap_fractions.lines + errors.lines, self.overlap_fractions.error + errors.error);
    }

    fn line(&self, condition: &str, method: &str, conversations: usize) -> String {
        format!("{condition} {method} conversations={conversations} {}", self.numbers())
    }

    fn numbers(&self) -> String {
        let overlap = if self.detected {
            let time = self.overlap_time;
            format!(
                " overlap_s={:.1} overlap_found_s={:.1} overlap_r={:.3} overlap_p={:.3} overlap_line_error={:.3} overlap_cpu_ms_per_s={:.1}",
                time.truth,
                time.found,
                time.recall(),
                time.precision(),
                self.overlap_fractions.mean(),
                self.overlap_cpu_seconds * 1000.0 / self.audio_seconds,
            )
        } else {
            String::new()
        };
        format!(
            "lines={} acc={:.3} acc_long={:.3} two_voice={:.3} change_r={:.3} change_p={:.3} wer={:.3} cpwer={:.3} cpu_ms_per_s={:.1} max_decode_s={:.2}{overlap}",
            self.lines,
            self.accuracy.rate(),
            self.accuracy_long.rate(),
            self.two_voice.rate(),
            self.changes.recall(),
            self.changes.precision(),
            self.wer.rate(),
            self.cp_wer.rate(),
            self.cpu_ms_per_s(),
            self.max_decode_seconds,
        )
    }

    fn cpu_ms_per_s(&self) -> f64 {
        self.cpu_seconds * 1000.0 / self.audio_seconds
    }

    fn json(&self, condition: &str, method: &str, conversations: usize, wall_seconds: f64) -> String {
        serde_json::json!({
            "condition": condition, "method": method, "conversations": conversations, "lines": self.lines,
            "acc": self.accuracy.rate(), "acc_long": self.accuracy_long.rate(), "two_voice": self.two_voice.rate(),
            "change_r": self.changes.recall(), "change_p": self.changes.precision(),
            "wer": self.wer.rate(), "cpwer": self.cp_wer.rate(), "cpu_ms_per_s": self.cpu_ms_per_s(),
            "wall_seconds": wall_seconds,
        })
        .to_string()
    }
}

/// The phrases the engine's stream finds in `audio` (with their speakers when voices are on), and the longest
/// (seconds) it took to return after a chunk.
fn phrases(engine: &PhraseEngine, whole_phrases: bool, audio: &[f32]) -> Result<(Vec<Phrase>, f64)> {
    let mut stream = engine.open_phrases_splitting(!whole_phrases)?;
    let (mut phrases, mut slowest) = (Vec::new(), 0.0f64);
    let mut timed = |stream: &mut PhraseStream, phrases: &mut Vec<Phrase>| {
        let started = Instant::now();
        phrases.extend(stream.phrases());
        slowest = slowest.max(started.elapsed().as_secs_f64());
    };
    for chunk in audio.chunks(CHUNK) {
        stream.push(chunk)?;
        timed(&mut stream, &mut phrases);
    }
    stream.finish();
    timed(&mut stream, &mut phrases);
    Ok((phrases, slowest))
}

/// The lines of one stream's phrases: a phrase's parts when it has them (voices on), else the phrase.
fn hyp_of(phrases: Vec<Phrase>) -> Hyp {
    let mut lines = Vec::new();
    for phrase in phrases {
        if phrase.parts.is_empty() {
            lines.push(HypLine { text: phrase.text, start: phrase.start, end: phrase.end, label: None });
            continue;
        }
        let ends = phrase.parts.iter().skip(1).map(|part| part.start).chain([phrase.end]);
        lines.extend(phrase.parts.iter().zip(ends).map(|(part, end)| HypLine { text: part.text.clone(), start: part.start, end, label: part.voice }));
    }
    Hyp { lines }
}

/// This process's CPU time (user and system), in seconds: the method's whole cost, the recognizer's included.
#[cfg(unix)]
fn cpu_seconds() -> f64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}

#[cfg(windows)]
fn cpu_seconds() -> f64 {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::{GetCurrentProcess, GetProcessTimes}};
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo-handle of this process and four FILETIMEs that outlive the call.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
    let seconds = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) as f64 * 1e-7;
    seconds(kernel) + seconds(user)
}
