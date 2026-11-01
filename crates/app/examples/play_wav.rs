//! Test helper: plays a WAV file a number of times, as a separately capturable app.
//! Usage: play_wav <file.wav> [times]

use windows::{core::HSTRING, Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_SYNC}};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let times: u32 = args.get(2).map_or(1, |s| s.parse().unwrap());
    for _ in 0..times {
        unsafe { PlaySoundW(&HSTRING::from(&args[1]), None, SND_FILENAME | SND_SYNC).ok().unwrap() };
    }
}
