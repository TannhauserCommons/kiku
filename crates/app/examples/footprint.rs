//! Measures what a model really costs and how well it hears: RAM, VRAM, CPU, latency and word error rate,
//! through the same engine path the app uses (speech detector, lead-in and trailing audio). A GPU model's
//! helper process is counted with this one.
//!
//! Usage:
//!   footprint <model id> <cpu|gpu> <set folder> [realtime utterances, default 10] [--json results.jsonl]
//!   footprint --table results.jsonl        prints a Markdown table of saved runs
//! The set folder holds `<id>.wav` (16 kHz mono 16-bit) and `<id>.txt` (reference text) pairs, e.g. 200
//! LibriSpeech test-clean utterances.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use kiku::overlay::usage::UsageMeter;
use kiku_core::{
    engine::{load, Device, EngineStream, POLL_INTERVAL, SAMPLE_RATE},
    catalog,
};
use serde_json::{json, Value};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX},
        Threading::{GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

/// Audio arrives in 100 ms chunks, as capture delivers it.
const CHUNK: usize = SAMPLE_RATE as usize / 10;
/// Silence after each utterance in the accuracy run, so the speech detector ends the phrase naturally.
const GAP_SECONDS: usize = 1;
/// Silence after each utterance in the real-time run, long enough for any model to finish the line.
const REALTIME_GAP_SECONDS: usize = 3;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--table") {
        return print_table(Path::new(&args[1]));
    }
    let model = catalog::by_id_or_default(&args[0]);
    // `gpu` runs the most precise GPU build; `gpu:fp16` names one.
    let (device, precision) = args[1].split_once(':').unwrap_or((&args[1], ""));
    let device = if device == "gpu" { Device::Cuda } else { Device::Cpu };
    let variant = if precision.is_empty() { model.variant(device) } else { model.variant_with(device, precision).expect("the model has that build") };
    let set = load_set(Path::new(&args[2]));
    let realtime_count: usize = args.get(3).filter(|a| !a.starts_with("--")).map_or(10, |n| n.parse().unwrap());
    let json_out = args.iter().position(|a| a == "--json").map(|i| PathBuf::from(&args[i + 1]));

    let start = memory(None);
    let mut usage = UsageMeter::new();
    let baseline = memory(None);
    let load_start = Instant::now();
    let engine = load(&model.spec(variant)).expect("model loads");
    let load_s = load_start.elapsed().as_secs_f64();
    let mut stream = engine.open_stream().unwrap();
    let helper = engine.helper_process();
    usage.watch_helper(helper);
    let loaded = memory(helper);
    let vram_loaded = usage.sample().gpu_memory_bytes;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;

    // Accuracy: every utterance, as fast as the model can go.
    let wall_start = Instant::now();
    let mut errors = 0;
    let mut reference_words = 0;
    for utterance in &set {
        let heard = transcribe(stream.as_mut(), &utterance.audio);
        let (e, n) = word_errors(&normalize(&utterance.text), &normalize(&heard));
        (errors, reference_words) = (errors + e, reference_words + n);
    }
    let audio_s: f64 = set.iter().map(|u| u.audio.len() as f64 / SAMPLE_RATE as f64).sum();
    let wall_s = wall_start.elapsed().as_secs_f64();
    let accuracy_usage = usage.sample();
    let cpu_s = accuracy_usage.cpu_percent as f64 / 100.0 * threads * wall_s;
    let vram_peak = accuracy_usage.gpu_memory_bytes;

    // Live use: a few utterances in real time, for CPU share and how long a line takes after speech ends.
    let mut realtime_stream = engine.open_stream().unwrap();
    usage.sample();
    let latencies = realtime(realtime_stream.as_mut(), &set[..realtime_count.min(set.len())]);
    let realtime_usage = usage.sample();
    let peak = memory(helper);
    drop(stream);
    drop(realtime_stream);
    drop(engine);
    let dropped = memory(None);

    let result = json!({
        "model": model.id,
        "device": if device == Device::Cuda { "gpu" } else { "cpu" },
        "precision": variant.precision,
        "utterances": set.len(),
        "audio_minutes": round(audio_s / 60.0, 1),
        "wer_percent": round(100.0 * errors as f64 / reference_words.max(1) as f64, 2),
        "load_s": round(load_s, 1),
        "speed_x_realtime": round(audio_s / wall_s, 1),
        "cpu_seconds_per_audio_minute": round(60.0 * cpu_s / audio_s, 2),
        "live_cpu_percent_all_threads": round(realtime_usage.cpu_percent as f64, 2),
        "live_cpu_percent_one_core": round(realtime_usage.cpu_percent as f64 * threads, 1),
        "latency_p50_ms": percentile(&latencies, 0.5),
        "latency_p90_ms": percentile(&latencies, 0.9),
        "ram_at_start_mb": start.working_mb,
        "ram_baseline_mb": baseline.working_mb,
        "ram_loaded_mb": loaded.working_mb,
        "ram_peak_mb": peak.peak_working_mb,
        "ram_after_unload_mb": dropped.working_mb,
        "private_loaded_mb": loaded.private_mb,
        "vram_loaded_mb": vram_loaded / 1_000_000,
        "vram_peak_mb": vram_peak.max(vram_loaded) / 1_000_000,
    });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if let Some(path) = json_out {
        let mut file = fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(file, "{result}").unwrap();
    }
}

struct Utterance {
    audio: Vec<f32>,
    text: String,
}

fn load_set(folder: &Path) -> Vec<Utterance> {
    let mut wavs: Vec<PathBuf> = fs::read_dir(folder).unwrap().filter_map(|e| Some(e.ok()?.path())).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
    wavs.sort();
    wavs.into_iter()
        .map(|wav| {
            let bytes = fs::read(&wav).unwrap();
            // A plain 44-byte PCM header, as the set is written.
            let audio = bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
            Utterance { audio, text: fs::read_to_string(wav.with_extension("txt")).unwrap() }
        })
        .collect()
}

/// What the model heard in one utterance, fed with a short silence after it so the phrase ends as in live use.
fn transcribe(stream: &mut dyn EngineStream, audio: &[f32]) -> String {
    let silence = vec![0.0; GAP_SECONDS * SAMPLE_RATE as usize];
    let mut heard = Vec::new();
    for (index, chunk) in audio.chunks(CHUNK).chain(silence.chunks(CHUNK)).enumerate() {
        stream.push(chunk).unwrap();
        if polls_after(index) {
            heard.extend(stream.poll().unwrap().into_iter().filter(|l| l.complete).map(|l| l.text));
        }
    }
    heard.extend(stream.flush().unwrap().into_iter().filter(|l| l.complete).map(|l| l.text));
    heard.join(" ")
}

/// Feeds utterances at real speed; returns, per utterance, the milliseconds from the end of its audio
/// to its last finished line.
fn realtime(stream: &mut dyn EngineStream, set: &[Utterance]) -> Vec<f64> {
    let silence = vec![0.0; REALTIME_GAP_SECONDS * SAMPLE_RATE as usize];
    let start = Instant::now();
    let mut sent = 0usize;
    let mut latencies = Vec::new();
    for utterance in set {
        let mut ended = None;
        let mut last_line = None;
        let chunks = utterance.audio.chunks(CHUNK).map(|c| (c, false)).chain(silence.chunks(CHUNK).map(|c| (c, true)));
        for (index, (chunk, after_speech)) in chunks.enumerate() {
            if after_speech && ended.is_none() {
                ended = Some(Instant::now());
            }
            stream.push(chunk).unwrap();
            sent += chunk.len();
            if polls_after(index) && stream.poll().unwrap().iter().any(|l| l.complete) {
                last_line = Some(Instant::now());
            }
            let due = start + Duration::from_secs_f64(sent as f64 / SAMPLE_RATE as f64);
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
        }
        if let (Some(ended), Some(line)) = (ended, last_line) {
            latencies.push(line.saturating_duration_since(ended).as_secs_f64() * 1000.0);
        }
    }
    latencies
}

/// Whether the app would poll after this chunk: it polls every [`POLL_INTERVAL`], not every chunk.
fn polls_after(chunk_index: usize) -> bool {
    let chunks_per_poll = (POLL_INTERVAL.as_millis() / 100).max(1) as usize;
    (chunk_index + 1) % chunks_per_poll == 0
}

/// Lowercase words without punctuation, keeping apostrophes, as LibriSpeech references are written.
fn normalize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace(|c: char| !(c.is_alphanumeric() || c == '\'' || c.is_whitespace()), " ")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Substitutions, deletions and insertions between reference and hypothesis, and the reference length.
fn word_errors(reference: &[String], hypothesis: &[String]) -> (usize, usize) {
    let mut row: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, h) in hypothesis.iter().enumerate() {
            let substitution = diagonal + usize::from(r != h);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    (row[hypothesis.len()], reference.len())
}

struct Memory {
    working_mb: u64,
    peak_working_mb: u64,
    private_mb: u64,
}

/// This process's memory, plus the helper's if one runs the model.
fn memory(helper: Option<u32>) -> Memory {
    let own = process_memory(unsafe { GetCurrentProcess() });
    let Some(helper) = helper.and_then(|pid| unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()) else {
        return own;
    };
    let other = process_memory(helper);
    let _ = unsafe { CloseHandle(helper) };
    Memory {
        working_mb: own.working_mb + other.working_mb,
        peak_working_mb: own.peak_working_mb + other.peak_working_mb,
        private_mb: own.private_mb + other.private_mb,
    }
}

fn process_memory(process: HANDLE) -> Memory {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    let pointer = &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS;
    unsafe { GetProcessMemoryInfo(process, pointer, counters.cb).unwrap() };
    let mb = |bytes: usize| bytes as u64 / 1_000_000;
    Memory { working_mb: mb(counters.WorkingSetSize), peak_working_mb: mb(counters.PeakWorkingSetSize), private_mb: mb(counters.PrivateUsage) }
}

fn percentile(values: &[f64], q: f64) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!(sorted[((sorted.len() - 1) as f64 * q).round() as usize].round())
}

fn round(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

/// The README table, one row per saved run. RAM is shown as what the model adds to the process.
fn print_table(results: &Path) {
    let columns = [
        ("Model", "model"), ("Device", "device"), ("WER %", "wer_percent"), ("Speed ×RT", "speed_x_realtime"),
        ("Live CPU % (all threads)", "live_cpu_percent_all_threads"), ("Line p50 / p90 ms", ""),
        ("Model RAM MB", "ram_loaded_mb"), ("Peak RAM MB", "ram_peak_mb"), ("VRAM MB", "vram_peak_mb"),
    ];
    println!("| {} |", columns.map(|(title, _)| title).join(" | "));
    println!("|{}", " --- |".repeat(columns.len()));
    for line in fs::read_to_string(results).unwrap().lines() {
        let run: Value = serde_json::from_str(line).unwrap();
        let cells: Vec<String> = columns
            .iter()
            .map(|(_, key)| match *key {
                "" => format!("{} / {}", run["latency_p50_ms"], run["latency_p90_ms"]),
                // Above the baseline, which includes the test audio held in memory.
                "ram_loaded_mb" | "ram_peak_mb" => (run[key].as_i64().unwrap_or(0) - run["ram_baseline_mb"].as_i64().unwrap_or(0)).to_string(),
                key => run[key].as_str().map_or_else(|| run[key].to_string(), str::to_owned),
            })
            .collect();
        println!("| {} |", cells.join(" | "));
    }
}
