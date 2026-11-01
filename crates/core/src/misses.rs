//! "Save a miss" (Ctrl+Win+B): the last 30 s each source heard, as the model heard it, plus the
//! transcript, so a missed word can be replayed with `asr_bench` and diagnosed.

use std::{collections::VecDeque, fs, path::PathBuf};

use crate::{
    captions::Section,
    engine::{Result, SAMPLE_RATE},
    transcripts,
};

const KEEP_SAMPLES: usize = 30 * SAMPLE_RATE as usize;

/// A source's last 30 s of audio.
#[derive(Default)]
pub struct Recent(VecDeque<f32>);

impl Recent {
    pub fn push(&mut self, audio: &[f32]) {
        self.0.extend(audio);
        let excess = self.0.len().saturating_sub(KEEP_SAMPLES);
        self.0.drain(..excess);
    }
}

/// Writes each source's audio as `<label>.wav`, and the transcript, into a new folder under
/// `%APPDATA%\kiku\misses`; returns the folder.
pub fn save(sources: &[(&str, &Recent)], sections: &[Section]) -> Result<PathBuf> {
    let folder = directory().join(transcripts::stamp_now());
    fs::create_dir_all(&folder)?;
    for (label, recent) in sources {
        fs::write(folder.join(format!("{}.wav", transcripts::sanitize(label))), wav(&recent.0))?;
    }
    transcripts::save_in(&folder, sections, "transcript")?;
    Ok(folder)
}

fn directory() -> PathBuf {
    transcripts::directory().with_file_name("misses")
}

/// 16-bit mono PCM at [`SAMPLE_RATE`], the format `asr_bench` reads.
fn wav(samples: &VecDeque<f32>) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_bytes as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    for &sample in samples {
        bytes.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_30_seconds() {
        let mut recent = Recent::default();
        recent.push(&vec![0.0; KEEP_SAMPLES]);
        recent.push(&[0.5; 10]);
        assert_eq!(recent.0.len(), KEEP_SAMPLES);
        assert_eq!(recent.0.back(), Some(&0.5));
        assert_eq!(wav(&recent.0).len(), 44 + 2 * KEEP_SAMPLES);
    }
}
