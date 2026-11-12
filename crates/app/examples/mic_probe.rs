//! Diagnoses mic captions: records what the engine is fed and logs levels and lines per second.
//! Usage: mic_probe <seconds> <out.wav> [model_dir]

use std::{sync::mpsc::channel, time::Instant};

use kiku::capture::{self, Source};
use kiku_core::engine::{self, Backend, Device, ModelSpec, SAMPLE_RATE};
use wasapi::{initialize_mta, DeviceEnumerator, Direction};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: usize = args[1].parse().unwrap();
    let dir = args.get(3).map_or("models/en/tiny-streaming-en", String::as_str);

    initialize_mta().ok().unwrap();
    let mic = DeviceEnumerator::new().unwrap().get_default_device(&Direction::Capture).unwrap();
    println!("mic: {} | mix format: {:?}", mic.get_friendlyname().unwrap(), mic.get_iaudioclient().unwrap().get_mixformat().unwrap());

    let engine = engine::load(&ModelSpec { backend: Backend::MoonshineTinyStreaming, dir: dir.into(), device: Device::Cpu, vad: None, voices: None, hotwords: Vec::new() }).unwrap();
    let mut stream = engine.open_stream().unwrap();
    let (sink, chunks) = channel();
    capture::spawn(0, Source::Microphone, sink);

    let start = Instant::now();
    let mut recorded: Vec<f32> = Vec::new();
    let mut reported = 0;
    while recorded.len() < seconds * SAMPLE_RATE as usize {
        let chunk = chunks.recv().unwrap();
        stream.push(&chunk.samples).unwrap();
        recorded.extend(&chunk.samples);
        for line in stream.poll().unwrap().into_iter().filter(|l| l.complete) {
            println!("{:6.1}s line: {:?}", start.elapsed().as_secs_f32(), line.text);
        }
        let second = recorded.len() / SAMPLE_RATE as usize;
        if second > reported {
            let last = &recorded[(second - 1) * SAMPLE_RATE as usize..second * SAMPLE_RATE as usize];
            let rms = (last.iter().map(|s| s * s).sum::<f32>() / last.len() as f32).sqrt();
            let peak = last.iter().fold(0f32, |m, s| m.max(s.abs()));
            println!("{second:4}s rms {:6.1} dBFS peak {:6.1} dBFS", db(rms), db(peak));
            reported = second;
        }
    }
    write_wav(&args[2], &recorded);
}

fn db(level: f32) -> f32 {
    20.0 * level.max(1e-9).log10()
}

/// Writes 16-bit mono PCM with a plain 44-byte header.
fn write_wav(path: &str, samples: &[f32]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend(b"RIFF");
    wav.extend((36 + data.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes()); // format chunk size
    wav.extend(1u16.to_le_bytes()); // PCM
    wav.extend(1u16.to_le_bytes()); // mono
    wav.extend(SAMPLE_RATE.to_le_bytes());
    wav.extend((SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    wav.extend(2u16.to_le_bytes()); // bytes per frame
    wav.extend(16u16.to_le_bytes()); // bits per sample
    wav.extend(b"data");
    wav.extend((data.len() as u32).to_le_bytes());
    wav.extend(data);
    std::fs::write(path, wav).unwrap();
}
