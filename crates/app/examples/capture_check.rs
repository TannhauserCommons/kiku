//! Captures one process (by PID) for a few seconds and prints how much audio arrived and how loud it was.
//! Usage: capture_check <pid> [seconds]
use std::{sync::mpsc::channel, time::{Duration, Instant}};

use kiku::capture::{self, Source};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pid = args[1].parse().unwrap();
    let seconds: u64 = args.get(2).map_or(5, |s| s.parse().unwrap());
    let (sink, chunks) = channel();
    let _capture = capture::spawn(0, Source::App { pid }, sink);
    let start = Instant::now();
    let (mut samples, mut energy, mut chunks_seen) = (0usize, 0f64, 0);
    while start.elapsed() < Duration::from_secs(seconds) {
        if let Ok(chunk) = chunks.recv_timeout(Duration::from_millis(500)) {
            chunks_seen += 1;
            samples += chunk.samples.len();
            energy += chunk.samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        }
    }
    let rms = (energy / samples.max(1) as f64).sqrt();
    println!("{chunks_seen} chunks, {samples} samples ({:.1} s of audio in {seconds} s), rms {rms:.4}", samples as f64 / 16000.0);
}
