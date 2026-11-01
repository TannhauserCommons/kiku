//! Checks whether streams sharing one engine affect each other.
//! Feeds two WAVs (16 kHz mono 16-bit) in lockstep and prints the second stream's lines,
//! once with both streams on one engine and once with an engine each.
//! Usage: stream_crosstalk <model_dir> <a.wav> <b.wav>

use kiku_core::engine::{load, Backend, Device, Engine, ModelSpec, SAMPLE_RATE};

const CHUNK: usize = SAMPLE_RATE as usize / 10;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let spec = ModelSpec { backend: Backend::MoonshineTinyStreaming, dir: args[1].clone().into(), device: Device::Cpu, vad: None };
    let (a, b) = (read_wav(&args[2]), read_wav(&args[3]));

    let shared = load(&spec).unwrap();
    println!("shared engine: {:?}", lines_of_second(shared.as_ref(), shared.as_ref(), &a, &b));
    let (first, second) = (load(&spec).unwrap(), load(&spec).unwrap());
    println!("separate engines: {:?}", lines_of_second(first.as_ref(), second.as_ref(), &a, &b));
}

fn lines_of_second(engine_a: &dyn Engine, engine_b: &dyn Engine, a: &[f32], b: &[f32]) -> Vec<String> {
    let (mut stream_a, mut stream_b) = (engine_a.open_stream().unwrap(), engine_b.open_stream().unwrap());
    let mut lines = Vec::new();
    for (chunk_a, chunk_b) in a.chunks(CHUNK).cycle().zip(b.chunks(CHUNK)) {
        stream_a.push(chunk_a).unwrap();
        stream_a.poll().unwrap();
        stream_b.push(chunk_b).unwrap();
        lines.extend(stream_b.poll().unwrap().into_iter().filter(|l| l.complete).map(|l| l.text));
    }
    lines
}

fn read_wav(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
}
