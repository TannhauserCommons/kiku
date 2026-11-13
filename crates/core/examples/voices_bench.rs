//! The voices benchmark (`.loop/voices/SPINE.md`): seeded conversations built in memory from LibriSpeech, run
//! through Kiku's Parakeet Unified (CPU) as the app feeds it, and scored by `voices::metrics`.
//!
//! Usage: voices_bench --set <LibriSpeech test-clean> [--condition <name>[,<name>...]|all]
//!   [--method baseline|labels|split-windows|overlap] [--conversations N] [--half tune|test|all] [--opus] [--json out.jsonl]
//!   `baseline` is Kiku with voices off; the others turn voices on in the engine's own phrase stream (`labels` labels whole phrases,
//!   `split-windows` is what ships).
//!   `overlap` is `split-windows` plus overlap detection (pyannote segmentation-3.0 over each whole conversation, offline: an upper
//!   bound on what a live detector would find), scored against the truth's overlap in seconds and in lines.
//!   `names` (`.loop/speakers/NAMES.md`, N6 and N8) enrols speakers from one chapter, then recognises them (and strangers never enrolled)
//!   in conversations built from their other chapters, heard as two streams as after an app restarts; `--half` picks the half of the
//!   speakers (tune or test), `--rounds N` the conversations, `--enrol-seconds S` how much each speaker is enrolled from,
//!   `--named-similarity a,b,..` and `--session-similarity a,b,..` the bars to try (one line each). It prints the right-name share of the
//!   book's speakers' lines of 1.5 s or more, the share of strangers' lines given any name, and how many returning speakers kept their number.
//!   `pool` (F15) opens one stream of three strangers (this half's speakers) in a run that already knows 4 saved and 16 unnamed voices of
//!   the other half's speakers, and counts the strangers' lines of 1.5 s or more that take one of those numbers or names; `--seeds a,b` varies
//!   how many unnamed voices a stream is told about.
//! Needs `ffmpeg` on PATH and Parakeet Unified where `KIKU_MODELS_DIR` points (the label and split methods also the
//! TitaNet-small model, `overlap` also `sherpa-onnx-pyannote-segmentation-3-0/` in `$KIKU_MODELS_DIR`). Prints numbers only.

use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Instant,
};

use kiku_core::{
    catalog, paths,
    engine::{Backend, Device, EngineStream, Line, ModelSpec, Result, SAMPLE_RATE},
    sherpa::{Phrase, PhraseEngine, PhraseStream},
    voices::{
        conversation::{build, Condition, Conversation, Utterance},
        book::{Place, Print, SavedVoices, VoiceScope},
        metrics::{change_scores, cp_wer, line_accuracy, overlap_fractions, overlap_seconds, two_voice_share, wer, ChangeScores, Errors, FractionError, Hyp, HypLine, Overlap, Share, Truth, Turn},
        overlaps,
        session::{Bars, Session},
        HeardVoice,
        Segmenter,
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
    half: String,
    rounds: usize,
    enrol_seconds: f32,
    bars: Vec<Bars>,
    json: Option<PathBuf>,
}

fn main() -> Result<()> {
    let options = parse(std::env::args().skip(1).collect())?;
    match options.method.as_str() {
        "names" => return run_names(&options),
        "pool" => return run_pool(&options),
        _ => {}
    }
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

/// A comma-separated list of numbers.
fn number_list(list: &str) -> Result<Vec<f32>> {
    Ok(list.split(',').map(str::parse).collect::<std::result::Result<_, _>>()?)
}

fn parse(args: Vec<String>) -> Result<Options> {
    let (mut set, mut condition, mut method, mut count, mut half) = (None, "all".to_string(), "baseline".to_string(), 10u64, "all".to_string());
    let (mut opus, mut json) = (false, None);
    let (mut rounds, mut enrol_seconds, mut named, mut session, mut seed_caps) = (24usize, 30.0f32, vec![Bars::default().named], vec![Bars::default().session], vec![Bars::default().seeds]);
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
            "--rounds" => rounds = value()?.parse()?,
            "--enrol-seconds" => enrol_seconds = value()?.parse()?,
            "--named-similarity" => named = number_list(&value()?)?,
            "--session-similarity" => session = number_list(&value()?)?,
            "--seeds" => seed_caps = value()?.split(',').map(str::parse).collect::<std::result::Result<_, _>>()?,
            "--opus" => opus = true,
            "--json" => json = Some(PathBuf::from(value()?)),
            _ => return Err(format!("unknown option {flag}").into()),
        }
    }
    if !matches!(method.as_str(), "baseline" | "labels" | "split-windows" | "overlap" | "names" | "pool") {
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
    let mut bars = Vec::new();
    for &named in &named {
        for &session in &session {
            bars.extend(seed_caps.iter().map(|&seeds| Bars { named, session, seeds }));
        }
    }
    Ok(Options { set: set.ok_or("--set <LibriSpeech test-clean folder> is needed")?, conditions, method, seeds: seeds.collect(), opus, half, rounds, enrol_seconds, bars, json })
}

/// The first utterances of each speaker, decoded by ffmpeg.
fn load_utterances(set: &Path) -> Result<Vec<Utterance>> {
    let mut utterances = Vec::new();
    for speaker in sorted_entries(set)? {
        let id = speaker.file_name().unwrap().to_string_lossy().into_owned();
        let mut transcripts = Vec::new();
        for chapter in sorted_entries(&speaker)? {
            transcripts.extend(chapter_transcripts(&chapter)?);
        }
        transcripts.sort();
        for (flac, text) in transcripts.into_iter().take(UTTERANCES_PER_SPEAKER) {
            utterances.push(read_utterance(&id, &flac, &text)?);
        }
    }
    Ok(utterances)
}

/// A chapter's recordings and what they say, in order.
fn chapter_transcripts(chapter: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut transcripts = Vec::new();
    for file in sorted_entries(chapter)? {
        if file.file_name().unwrap().to_string_lossy().ends_with(".trans.txt") {
            for line in fs::read_to_string(&file)?.lines() {
                let (id, text) = line.split_once(' ').ok_or("a transcript line without text")?;
                transcripts.push((chapter.join(format!("{id}.flac")), text.to_lowercase()));
            }
        }
    }
    transcripts.sort();
    Ok(transcripts)
}

fn read_utterance(speaker: &str, flac: &Path, text: &str) -> Result<Utterance> {
    let audio = decode(&["-i", flac.to_str().unwrap(), "-f", "f32le", "-ar", "16000", "-ac", "1", "-"], Vec::new())?;
    Ok(Utterance { speaker: speaker.to_owned(), text: text.to_owned(), audio: floats(&audio) })
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

// ---- The `names` method (`.loop/speakers/NAMES.md`, N6 and N8) ----

/// One speaker's recordings: what they are enrolled from (one chapter) and what they are recognised in (other chapters).
struct Person {
    id: String,
    enrol: Vec<Utterance>,
    later: Vec<Utterance>,
    /// Whether `later` comes from chapters other than the enrolment's, as a new call would.
    other_chapters: bool,
}

/// How many saved voices every round has, and how many speakers talk in it.
const BOOK_SIZE: usize = 6;
const TALKERS: usize = 3;
/// Utterances per speaker the conversations are built from.
const LATER_UTTERANCES: usize = 12;
/// Rounds are run on this many threads at a time.
const WORKERS: usize = 3;

/// What `names` counts over its rounds, lines of at least [`LONG_LINE`] seconds only.
#[derive(Default)]
struct Names {
    /// Lines of speakers in the book: given their own name, another's, or none.
    right: usize,
    wrong: usize,
    unnamed: usize,
    /// Lines of speakers never enrolled: given a name, or not.
    strangers: usize,
    stranger_named: usize,
    /// Speakers heard in both halves of a conversation (two streams, as after a restart), and how many kept their number.
    returned: usize,
    returned_same: usize,
    /// Lines of the second half whose speaker held the number they had in the first.
    second_lines: usize,
    second_same: usize,
    /// Speakers of the second half who took the number another speaker had in the first: two people mistaken for one.
    merged: usize,
}

impl Names {
    fn add(&mut self, other: &Names) {
        let (a, b) = (&mut *self, other);
        for (x, y) in [
            (&mut a.right, b.right), (&mut a.wrong, b.wrong), (&mut a.unnamed, b.unnamed), (&mut a.strangers, b.strangers), (&mut a.stranger_named, b.stranger_named),
            (&mut a.returned, b.returned), (&mut a.returned_same, b.returned_same), (&mut a.second_lines, b.second_lines), (&mut a.second_same, b.second_same), (&mut a.merged, b.merged),
        ] {
            *x += y;
        }
    }

    fn line(&self, bars: Bars, rounds: usize) -> String {
        let share = |part: usize, whole: usize| if whole == 0 { 0.0 } else { part as f64 / whole as f64 };
        let named = self.right + self.wrong + self.unnamed;
        format!(
            "names named>={:.2} session>={:.2} seeds={} rounds={rounds} named_lines={named} right={:.3} wrong_name={:.3} no_name={:.3} stranger_lines={} stranger_named={:.4} returning_speakers={} same_number={:.3} second_half_lines={} line_same_number={:.3} merged_speakers={}",
            bars.named,
            bars.session,
            bars.seeds,
            share(self.right, named),
            share(self.wrong, named),
            share(self.unnamed, named),
            self.strangers,
            share(self.stranger_named, self.strangers),
            self.returned,
            share(self.returned_same, self.returned),
            self.second_lines,
            share(self.second_same, self.second_lines),
            self.merged,
        )
    }
}

fn run_names(options: &Options) -> Result<()> {
    let people = load_people(&options.set, options.enrol_seconds)?;
    let model = catalog::by_id("parakeet-unified").ok_or("no Parakeet Unified in the catalog")?;
    let spec = ModelSpec { voices: Some(paths::models_dir().join(EMBEDDING_MODEL)), ..model.spec(model.variant(Device::Cpu)) };
    let Backend::Phrases(kind) = spec.backend else { return Err("Parakeet isn't a phrase model".into()) };
    let engine = PhraseEngine::load(&spec, kind)?;
    // A half's speakers: the tune half is every other speaker from the first, the test half the rest.
    let pool: Vec<&Person> = people.iter().enumerate().filter(|(i, _)| (i % 2 == 1) == (options.half == "test")).map(|(_, person)| person).collect();
    let capable: Vec<&Person> = pool.iter().copied().filter(|person| person.other_chapters).collect();
    eprintln!("{} speakers in the {} half, {} of them with later chapters to be recognised in", pool.len(), options.half, capable.len());
    let enrolled: HashMap<&str, (Print, f32)> = capable.iter().map(|person| Ok((person.id.as_str(), enrol(&engine, person)?))).collect::<Result<_>>()?;
    let (engine, pool, capable, enrolled) = (&engine, &pool, &capable, &enrolled);
    for &bars in &options.bars {
        let rounds: Vec<usize> = (0..options.rounds).collect();
        let mut total = Names::default();
        for batch in rounds.chunks(WORKERS) {
            let done: Vec<Result<Names>> = thread::scope(|scope| {
                let jobs: Vec<_> = batch.iter().map(|&round| scope.spawn(move || recognise(engine, pool, capable, enrolled, bars, round))).collect();
                jobs.into_iter().map(|job| job.join().expect("a round panicked")).collect()
            });
            for names in done {
                total.add(&names?);
            }
        }
        println!("{}", total.line(bars, options.rounds));
    }
    Ok(())
}

/// Each speaker's recordings: the first chapter's utterances up to `enrol_seconds` of audio for enrolment, and the
/// first utterances of the other chapters (of the same chapter after those, for a speaker with one) to be recognised in.
fn load_people(set: &Path, enrol_seconds: f32) -> Result<Vec<Person>> {
    let mut people = Vec::new();
    for speaker in sorted_entries(set)? {
        let id = speaker.file_name().unwrap().to_string_lossy().into_owned();
        let chapters: Vec<Vec<(PathBuf, String)>> = sorted_entries(&speaker)?.iter().map(|chapter| chapter_transcripts(chapter)).collect::<Result<_>>()?;
        let mut first = chapters[0].iter();
        let (mut enrol, mut seconds) = (Vec::new(), 0.0);
        for (flac, text) in first.by_ref() {
            if seconds >= enrol_seconds {
                break;
            }
            let utterance = read_utterance(&id, flac, text)?;
            seconds += utterance.audio.len() as f32 / SAMPLE_RATE as f32;
            enrol.push(utterance);
        }
        let other_chapters = chapters.len() > 1;
        let rest: Vec<&(PathBuf, String)> = if other_chapters { chapters[1..].iter().flatten().collect() } else { first.collect() };
        let later = rest.into_iter().take(LATER_UTTERANCES).map(|(flac, text)| read_utterance(&id, flac, text)).collect::<Result<_>>()?;
        people.push(Person { id, enrol, later, other_chapters });
    }
    Ok(people)
}

/// A speaker's print and the audio it rests on, as a stream in a call would hear it before the speaker is named.
fn enrol(engine: &PhraseEngine, person: &Person) -> Result<(Print, f32)> {
    let gap = vec![0.0; SAMPLE_RATE as usize / 2];
    let audio: Vec<f32> = person.enrol.iter().flat_map(|utterance| utterance.audio.iter().chain(&gap).copied().collect::<Vec<_>>()).collect();
    let mut stream = engine.open_phrases_knowing(true, &[])?;
    for chunk in audio.chunks(CHUNK) {
        stream.push(chunk)?;
        stream.phrases();
    }
    stream.finish();
    stream.phrases();
    let heard = stream.voice_prints()?.unwrap_or_default();
    let loudest = heard.into_iter().max_by(|a, b| a.seconds.total_cmp(&b.seconds)).ok_or_else(|| format!("speaker {} enrolled no voice", person.id))?;
    Ok((loudest.print, loudest.seconds))
}

/// One round: a book of [`BOOK_SIZE`] enrolled speakers, and a conversation of [`TALKERS`] (all, two, one or none of them from
/// the book, by round, the others strangers) heard as two streams, the second opened after the first has handed back.
fn recognise(engine: &PhraseEngine, pool: &[&Person], capable: &[&Person], enrolled: &HashMap<&str, (Print, f32)>, bars: Bars, round: usize) -> Result<Names> {
    let mut order: Vec<usize> = (0..capable.len()).collect();
    shuffle(&mut order, round as u64 + 1);
    let in_book: Vec<&Person> = order.iter().take(BOOK_SIZE).map(|&i| capable[i]).collect();
    let mut strangers: Vec<&Person> = pool.iter().copied().filter(|person| !in_book.iter().any(|b| b.id == person.id)).collect();
    let mut order: Vec<usize> = (0..strangers.len()).collect();
    shuffle(&mut order, round as u64 + 1000);
    strangers = order.iter().map(|&i| strangers[i]).collect();
    let known_talkers = TALKERS - round % (TALKERS + 1);
    let talkers: Vec<&Person> = in_book.iter().take(known_talkers).chain(strangers.iter().take(TALKERS - known_talkers)).copied().collect();

    let saved = SavedVoices::in_memory();
    for person in &in_book {
        let (print, seconds) = &enrolled[person.id.as_str()];
        saved.add(&person.id, print.clone(), *seconds, VoiceScope::Everywhere, None);
    }
    let utterances: Vec<Utterance> = talkers.iter().flat_map(|person| person.later.iter().cloned()).collect();
    let condition = Condition::parse(&format!("s{TALKERS}-fast")).ok_or("no such condition")?;
    let conversation = build(&utterances, &condition, round as u64);
    let cut = halfway(&conversation.truth);

    let (mut session, place) = (Session::with_bars(bars), Place::App("bench".into()));
    let mut halves: Vec<Vec<(f32, f32, Option<u32>)>> = Vec::new();
    for (audio, offset) in [(&conversation.audio[..cut], 0.0), (&conversation.audio[cut..], cut as f32 / SAMPLE_RATE as f32)] {
        let book = saved.snapshot();
        let (mut following, known) = session.follow(place.clone(), Some("bench"), &book);
        let mut stream = engine.open_phrases_knowing(true, &known)?;
        let mut phrases = Vec::new();
        for chunk in audio.chunks(CHUNK) {
            stream.push(chunk)?;
            phrases.extend(stream.phrases());
        }
        stream.finish();
        phrases.extend(stream.phrases());
        let parts: Vec<(f32, f32, u32)> = phrases.iter().flat_map(|phrase| {
            let ends = phrase.parts.iter().skip(1).map(|part| part.start).chain([phrase.end]).collect::<Vec<_>>();
            phrase.parts.iter().zip(ends).filter_map(|(part, end)| Some((part.start + offset, end + offset, part.voice?))).collect::<Vec<_>>()
        }).collect();
        let mut lines: Vec<Line> = parts.iter().map(|&(_, _, label)| Line { id: 0, text: String::new(), complete: true, voice: Some(label) }).collect();
        session.number_lines(&mut following, &mut lines, &saved, || stream.voice_prints().ok().flatten());
        halves.push(parts.iter().zip(&lines).map(|(&(start, end, _), line)| (start, end, line.voice)).collect());
        let heard = stream.voice_prints()?.unwrap_or_default();
        session.hand_back(&mut following, &heard, &saved.snapshot(), &saved);
    }
    Ok(score_names(&conversation.truth, &halves, &session.names(&place, &saved.snapshot()), &in_book))
}

/// Scores each long line: its speaker (by the truth turn it overlaps most) against the name its number carries.
fn score_names(truth: &Truth, halves: &[Vec<(f32, f32, Option<u32>)>], names: &HashMap<u32, String>, in_book: &[&Person]) -> Names {
    let speaker_of = |start: f32, end: f32| {
        let overlap = |turn: &&Turn| (end.min(turn.end) - start.max(turn.start)).max(0.0);
        truth.turns.iter().max_by(|a, b| overlap(a).total_cmp(&overlap(b))).map(|turn| turn.speaker.as_str())
    };
    let mut scores = Names::default();
    let mut numbers: Vec<HashMap<&str, Vec<u32>>> = vec![HashMap::new(), HashMap::new()];
    for (half, lines) in halves.iter().enumerate() {
        for &(start, end, number) in lines.iter().filter(|(start, end, _)| end - start >= LONG_LINE) {
            let Some(speaker) = speaker_of(start, end) else { continue };
            let name = number.and_then(|number| names.get(&number));
            if in_book.iter().any(|person| person.id == speaker) {
                match name {
                    Some(name) if name == speaker => scores.right += 1,
                    Some(_) => scores.wrong += 1,
                    None => scores.unnamed += 1,
                }
            } else {
                scores.strangers += 1;
                scores.stranger_named += usize::from(name.is_some());
            }
            if let Some(number) = number {
                numbers[half].entry(speaker).or_default().push(number);
            }
        }
    }
    let usual = |numbers: &[u32]| numbers.iter().max_by_key(|&&n| numbers.iter().filter(|&&m| m == n).count()).copied();
    for (speaker, second) in &numbers[1] {
        let Some(first) = numbers[0].get(speaker) else { continue };
        let before = usual(first);
        scores.returned += 1;
        scores.returned_same += usize::from(usual(second) == before);
        scores.second_lines += second.len();
        scores.second_same += second.iter().filter(|&&number| Some(number) == before).count();
        let others = numbers[0].iter().filter(|(other, _)| *other != speaker).filter_map(|(_, numbers)| usual(numbers));
        scores.merged += usize::from(others.into_iter().any(|number| Some(number) == usual(second)));
    }
    scores
}

/// The sample between the middle two turns of a conversation, where two streams (an app restarted) would part.
fn halfway(truth: &Truth) -> usize {
    let middle = truth.turns.len() / 2;
    let seconds = (truth.turns[middle - 1].end + truth.turns[middle].start) / 2.0;
    (seconds * SAMPLE_RATE as f32) as usize
}

/// A shuffle fixed by `seed`.
fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(i, (state % (i as u64 + 1)) as usize);
    }
}

// ---- The `pool` method: strangers heard by a stream that already knows many other voices (F15) ----

/// Voices of other speakers the run already knows when a stream of strangers opens: saved ones and unnamed ones.
const POOL_SAVED: usize = 4;
const POOL_UNNAMED: usize = 16;

/// What `pool` counts: strangers' lines (1.5 s or more) that took a number or name already in the pool.
#[derive(Default)]
struct Pool {
    lines: usize,
    took_number: usize,
    took_name: usize,
    /// Voices the pool held at the start, summed over rounds.
    known: usize,
}

fn run_pool(options: &Options) -> Result<()> {
    let people = load_people(&options.set, options.enrol_seconds)?;
    let model = catalog::by_id("parakeet-unified").ok_or("no Parakeet Unified in the catalog")?;
    let spec = ModelSpec { voices: Some(paths::models_dir().join(EMBEDDING_MODEL)), ..model.spec(model.variant(Device::Cpu)) };
    let Backend::Phrases(kind) = spec.backend else { return Err("Parakeet isn't a phrase model".into()) };
    let engine = PhraseEngine::load(&spec, kind)?;
    // The strangers come from this half's speakers; the pool's voices from the other half's, unrelated calls.
    let half = |test: bool| -> Vec<&Person> { people.iter().enumerate().filter(|(i, _)| (i % 2 == 1) == test).map(|(_, person)| person).collect() };
    let (strangers, others) = (half(options.half == "test"), half(options.half != "test"));
    let enrolled: HashMap<&str, (Print, f32)> = others.iter().map(|person| Ok((person.id.as_str(), enrol(&engine, person)?))).collect::<Result<_>>()?;
    eprintln!("{} strangers' speakers from the {} half, a pool from {} others", strangers.len(), options.half, others.len());
    let (engine, strangers, others, enrolled) = (&engine, &strangers, &others, &enrolled);
    for &bars in &options.bars {
        let mut total = Pool::default();
        let rounds: Vec<usize> = (0..options.rounds).collect();
        for batch in rounds.chunks(WORKERS) {
            let done: Vec<Result<Pool>> = thread::scope(|scope| {
                let jobs: Vec<_> = batch.iter().map(|&round| scope.spawn(move || pool_round(engine, strangers, others, enrolled, bars, round))).collect();
                jobs.into_iter().map(|job| job.join().expect("a round panicked")).collect()
            });
            for pool in done {
                let pool = pool?;
                (total.lines, total.took_number, total.took_name, total.known) = (total.lines + pool.lines, total.took_number + pool.took_number, total.took_name + pool.took_name, total.known + pool.known);
            }
        }
        let share = |part: usize| if total.lines == 0 { 0.0 } else { part as f64 / total.lines as f64 };
        println!(
            "pool named>={:.2} session>={:.2} seeds={} rounds={} strangers_lines={} took_a_number={:.4} ({}) took_a_name={:.4} ({}) voices_known_per_round={:.1}",
            bars.named,
            bars.session,
            bars.seeds,
            options.rounds,
            total.lines,
            share(total.took_number),
            total.took_number,
            share(total.took_name),
            total.took_name,
            total.known as f64 / options.rounds as f64,
        );
    }
    Ok(())
}

/// One round: the run knows [`POOL_SAVED`] saved voices and up to [`POOL_UNNAMED`] unnamed ones of other speakers; a stream
/// of three strangers opens and its lines are counted for taking one of them.
fn pool_round(engine: &PhraseEngine, strangers: &[&Person], others: &[&Person], enrolled: &HashMap<&str, (Print, f32)>, bars: Bars, round: usize) -> Result<Pool> {
    let mut order: Vec<usize> = (0..others.len()).collect();
    shuffle(&mut order, round as u64 + 7);
    let saved = SavedVoices::in_memory();
    let (mut session, place) = (Session::with_bars(bars), Place::App("bench".into()));
    for &i in order.iter().take(POOL_SAVED) {
        let (print, seconds) = &enrolled[others[i].id.as_str()];
        saved.add(&others[i].id, print.clone(), *seconds, VoiceScope::Everywhere, None);
    }
    // Each unnamed voice was heard in a call of its own, as the run's voices accumulate.
    for &i in order.iter().skip(POOL_SAVED).take(POOL_UNNAMED) {
        let (print, seconds) = &enrolled[others[i].id.as_str()];
        let (mut following, known) = session.follow(place.clone(), Some("bench"), &saved.snapshot());
        // The speaker is new to the stream, so after the voices it was told about.
        let label = known.len() as u32;
        let heard = [HeardVoice { label, print: print.clone(), strength: *seconds, seconds: *seconds }];
        let mut lines = [Line { id: 0, text: String::new(), complete: true, voice: Some(label) }];
        session.number_lines(&mut following, &mut lines, &saved, || Some(heard.to_vec()));
        session.hand_back(&mut following, &heard, &saved.snapshot(), &saved);
    }
    let known_before = session.voice_count(&place);

    let mut order: Vec<usize> = (0..strangers.len()).collect();
    shuffle(&mut order, round as u64 + 3000);
    let utterances: Vec<Utterance> = order.iter().take(TALKERS).flat_map(|&i| strangers[i].later.iter().cloned()).collect();
    let condition = Condition::parse(&format!("s{TALKERS}-fast")).ok_or("no such condition")?;
    let conversation = build(&utterances, &condition, round as u64);

    let book = saved.snapshot();
    let (mut following, known) = session.follow(place.clone(), Some("bench"), &book);
    let mut stream = engine.open_phrases_knowing(true, &known)?;
    let mut phrases = Vec::new();
    for chunk in conversation.audio.chunks(CHUNK) {
        stream.push(chunk)?;
        phrases.extend(stream.phrases());
    }
    stream.finish();
    phrases.extend(stream.phrases());
    let parts: Vec<(f32, f32, u32)> = phrases.iter().flat_map(|phrase| {
        let ends = phrase.parts.iter().skip(1).map(|part| part.start).chain([phrase.end]).collect::<Vec<_>>();
        phrase.parts.iter().zip(ends).filter_map(|(part, end)| Some((part.start, end, part.voice?))).collect::<Vec<_>>()
    }).collect();
    let mut lines: Vec<Line> = parts.iter().map(|&(_, _, label)| Line { id: 0, text: String::new(), complete: true, voice: Some(label) }).collect();
    session.number_lines(&mut following, &mut lines, &saved, || stream.voice_prints().ok().flatten());
    let names = session.names(&place, &saved.snapshot());
    let mut pool = Pool { known: known_before, ..Pool::default() };
    for (&(start, end, _), line) in parts.iter().zip(&lines) {
        if end - start < LONG_LINE {
            continue;
        }
        pool.lines += 1;
        if let Some(number) = line.voice {
            pool.took_number += usize::from((number as usize) < known_before);
            pool.took_name += usize::from(names.contains_key(&number));
        }
    }
    Ok(pool)
}
