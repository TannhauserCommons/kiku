//! What Kiku is doing right now, for the MCP server: whether captioning runs, the model, the save
//! mode, and each app and the mic with whether it is being captured and when it was last heard.
//! The app writes it to `status.json` in its data folder when it changes and at least every
//! [`HEARTBEAT`], so a reader can tell a running Kiku from a file left behind. It holds no caption
//! text: lines reach the disk only through the history database, as the save mode allows.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};

use crate::{
    captions::{Captions, YOU_SECTION},
    controls::Controls,
    engine::Device,
    log, paths, transcripts,
};

/// How often the status is written even when nothing changed.
pub const HEARTBEAT: Duration = Duration::from_secs(30);
/// Changes are written at most this often, since line counts change with every line heard.
const MIN_GAP: Duration = Duration::from_secs(2);

pub fn path() -> PathBuf {
    paths::data_dir().join("status.json")
}

/// Writes the status file whenever what it says changes, and on the heartbeat.
#[derive(Default)]
pub struct StatusFile {
    last: Option<Value>,
    written: Option<Instant>,
}

impl StatusFile {
    /// `capturing` says, per caption section, whether a capture feeds it now.
    pub fn update(&mut self, captions: &Captions, controls: &Controls, capturing: &[bool]) {
        let since = self.written.map_or(HEARTBEAT, |at| at.elapsed());
        if since < MIN_GAP {
            return;
        }
        let status = snapshot(captions, controls, capturing);
        let changed = self.last.as_ref() != Some(&status);
        if since < HEARTBEAT && !(changed && since >= MIN_GAP) {
            return;
        }
        let mut file = status.clone();
        file["updated_at"] = json!(seconds(SystemTime::now()));
        if let Err(error) = transcripts::replace(&path(), &file.to_string()) {
            log::write(format!("couldn't write the status file: {error}"));
        }
        self.last = Some(status);
        self.written = Some(Instant::now());
    }

    /// Removes the file as Kiku quits, so readers know it isn't running.
    pub fn clear(&mut self) {
        let _ = fs::remove_file(path());
        self.last = None;
    }
}

fn snapshot(captions: &Captions, controls: &Controls, capturing: &[bool]) -> Value {
    let model = controls.model();
    let device = |choice: crate::catalog::Choice| if choice.device() == Device::Cuda { "gpu" } else { "cpu" };
    let sources: Vec<Value> = captions
        .sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            json!({
                "name": section.label,
                "kind": if index == YOU_SECTION { "mic" } else { "app" },
                "capturing": capturing.get(index).copied().unwrap_or(false),
                "last_heard": section.captions.back().map(|caption| seconds(caption.time)),
                "lines": section.captions.len(),
            })
        })
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "captioning": !controls.stopped.load(Ordering::Relaxed),
        "dictating": controls.dictating.load(Ordering::Relaxed),
        "model": model.active.model.name,
        "device": device(model.active),
        "runs_on": model.remote.as_deref().unwrap_or("this PC"),
        "switching_to": model.pending.map(|choice| format!("{} on the {}", choice.model.name, device(choice))),
        "save_mode": if controls.auto_save.load(Ordering::Relaxed) { "auto" } else { "manual" },
        "saves_only_mic": controls.save_mic_only.load(Ordering::Relaxed),
        "keep_days": controls.keep_days.load(Ordering::Relaxed),
        "sources": sources,
    })
}

/// Seconds since the Unix epoch, as the history database stores times.
pub fn seconds(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}
