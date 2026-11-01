//! Measures an engine's CPU cost and decode latency on a speech/silence loop.
//! Usage: asr_bench <model id, e.g. moonshine-tiny or parakeet-tdt-v2> <speech.wav: 16 kHz mono 16-bit> [audio_seconds] [cpu|gpu]
//! Audio is fed in real time, 100 ms at a time, the way live capture will.

use std::time::{Duration, Instant};

use kiku_core::{
    engine::{load, Device, SAMPLE_RATE},
    catalog::{self, Choice},
};
use windows::Win32::{Foundation::FILETIME, System::Threading::{GetCurrentProcess, GetProcessTimes}};

const CHUNK: usize = SAMPLE_RATE as usize / 10;
const SILENCE_SECONDS: usize = 10;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let device = if args.get(4).is_some_and(|d| d == "gpu") { Device::Cuda } else { Device::Cpu };
    let choice = Choice { model: catalog::by_id_or_default(&args[1]), device };
    let seconds: f64 = args.get(3).map_or(60.0, |s| s.parse().unwrap());
    let speech = read_wav_16k_mono(&args[2]);
    let cycle: Vec<f32> = speech.iter().copied().chain(std::iter::repeat_n(0.0, SILENCE_SECONDS * SAMPLE_RATE as usize)).collect();

    let load_start = Instant::now();
    let engine = load(&choice.spec()).unwrap();
    println!("{} on {device:?} loaded in {:.1} s", choice.model.name, load_start.elapsed().as_secs_f32());
    let mut stream = engine.open_stream().unwrap();

    let (start, cpu_start) = (Instant::now(), process_cpu_seconds());
    let mut poll_times = Vec::new();
    for (i, chunk) in cycle.chunks(CHUNK).cycle().take((seconds * 10.0) as usize).enumerate() {
        stream.push(chunk).unwrap();
        let poll_start = Instant::now();
        let lines = stream.poll().unwrap();
        poll_times.push(poll_start.elapsed());
        lines.iter().filter(|l| l.complete).for_each(|l| println!("{}", l.text));
        sleep_until(start + Duration::from_millis(100 * (i as u64 + 1)));
    }

    let cpu = process_cpu_seconds() - cpu_start;
    let threads = std::thread::available_parallelism().unwrap().get() as f64;
    let speech_share = speech.len() as f64 / cycle.len() as f64;
    poll_times.sort();
    let percentile = |q: f64| poll_times[((poll_times.len() - 1) as f64 * q) as usize].as_millis();
    println!("\n{seconds:.0} s of audio, {:.0}% speech", 100.0 * speech_share);
    println!("CPU: {:.1}% of one core = {:.2}% of all {threads} threads", 100.0 * cpu / seconds, 100.0 * cpu / seconds / threads);
    println!("poll time ms: p50 {} p90 {} max {}", percentile(0.5), percentile(0.9), percentile(1.0));
}

fn read_wav_16k_mono(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    // Plain 44-byte PCM header; the test clips are written that way.
    bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}

fn process_cpu_seconds() -> f64 {
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user).unwrap() };
    let seconds = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e7;
    seconds(kernel) + seconds(user)
}

fn sleep_until(deadline: Instant) {
    if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
    }
}
