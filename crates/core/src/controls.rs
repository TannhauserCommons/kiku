//! Live settings shared by the overlay (which changes them) and the decoder (which applies them),
//! and the sensitivity gate that silences quiet input before any model hears it.

use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Mutex,
};

/// Transcripts are kept this many days unless settings say otherwise.
pub const DEFAULT_KEEP_DAYS: u32 = 30;

use crate::{catalog::Choice, engine::SAMPLE_RATE, update::Release, words::WordBook};

/// Slider range for sensitivity thresholds, in dBFS. The minimum means "let everything through".
pub const THRESHOLD_MIN_DB: f32 = -70.0;
pub const THRESHOLD_MAX_DB: f32 = 0.0;
/// Reported by the level meters when nothing was heard.
pub const SILENT_DB: f32 = -120.0;

/// What a save button asks to save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveRequest {
    /// One section, by its index in the captions.
    Section(usize),
    All,
}

/// The two kinds of input with their own sensitivity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Input {
    Mic,
    Computer,
}

/// State changed from the panel and read on the audio side.
pub struct Controls {
    pub stopped: AtomicBool,
    /// The captions key is held: your finished lines are typed into the focused app.
    pub dictating: AtomicBool,
    /// The app is closing: finish what's being said and save it.
    pub quitting: AtomicBool,
    /// Auto save mode: every finished line goes to the history database. Off (manual mode), only
    /// what the save buttons ask for is saved.
    pub auto_save: AtomicBool,
    /// In auto save mode, save only your mic's lines, not what apps say.
    pub save_mic_only: AtomicBool,
    /// In auto save mode, saved lines are deleted after this many days; 0 keeps them forever.
    pub keep_days: AtomicU32,
    /// The process running a GPU model, or 0: its CPU and GPU use count as the app's.
    helper_process: AtomicU32,
    /// A save button's request, waiting for the decoder, which owns the database.
    save_request: Mutex<Option<SaveRequest>>,
    /// Asked to save the last 30 s of audio, to diagnose a missed word.
    pub save_miss: AtomicBool,
    /// Apps discovery doesn't capture.
    blocked: Mutex<Vec<Blocked>>,
    /// Dictated text waiting for the overlay to type it.
    typing: Mutex<Vec<String>>,
    mic: InputControls,
    computer: InputControls,
    model: Mutex<ModelState>,
    /// Something the decoder wants the panel to tell the user, e.g. why the GPU wasn't used.
    notice: Mutex<Option<String>>,
    /// Whether to check for updates (Settings → General).
    pub check_updates: AtomicBool,
    /// Dictation only: no app is captured, and the mic is heard only while the captions key is held.
    pub dictation_only: AtomicBool,
    /// A newer version the update check found, waiting for the panel to offer it.
    update: Mutex<Option<Release>>,
    /// What a check asked for by hand found: whether there is a newer version, or why it failed.
    checked: Mutex<Option<Result<bool, String>>>,
    /// Word rules for captions and dictation, from `words.txt`, and where they apply.
    pub words: WordBook,
    /// The words the model listens for, and whether they changed since the decoder last took them.
    hotwords: Mutex<(Vec<String>, bool)>,
}

/// An app that isn't captured or captioned: until Kiku quits, or always (saved in settings).
#[derive(Clone, Debug, PartialEq)]
pub struct Blocked {
    pub name: String,
    pub always: bool,
}

/// The running model and device, a switch the user asked for, and the last switch that failed.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelState {
    pub active: Choice,
    /// Requested but not yet loaded; a model still downloading waits here.
    pub pending: Option<Choice>,
    pub failed: Option<Choice>,
    /// Why it failed, for the panel to say.
    pub failure: Option<String>,
    /// A model is running. Not while waiting for one to be downloaded, or while stopped.
    pub loaded: bool,
    /// Why the active model runs on the CPU though the GPU was picked.
    pub gpu_fallback: Option<String>,
}

impl ModelState {
    /// What the user last picked, loaded or not.
    pub fn wanted(&self) -> Choice {
        self.pending.unwrap_or(self.active)
    }
}

struct InputControls {
    threshold_db: AtomicF32,
    /// Loudest level heard since the meter last read it.
    peak_db: AtomicF32,
}

impl Controls {
    pub fn new(mic_threshold_db: f32, computer_threshold_db: f32, model: Choice) -> Self {
        Self {
            stopped: AtomicBool::new(false),
            dictating: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
            auto_save: AtomicBool::new(true),
            save_mic_only: AtomicBool::new(false),
            save_request: Mutex::new(None),
            helper_process: AtomicU32::new(0),
            keep_days: AtomicU32::new(DEFAULT_KEEP_DAYS),
            save_miss: AtomicBool::new(false),
            blocked: Mutex::new(Vec::new()),
            typing: Mutex::new(Vec::new()),
            mic: InputControls::new(mic_threshold_db),
            computer: InputControls::new(computer_threshold_db),
            model: Mutex::new(ModelState { active: model, pending: None, failed: None, failure: None, loaded: false, gpu_fallback: None }),
            notice: Mutex::new(None),
            check_updates: AtomicBool::new(true),
            dictation_only: AtomicBool::new(false),
            update: Mutex::new(None),
            checked: Mutex::new(None),
            words: WordBook::default(),
            hotwords: Mutex::new((Vec::new(), false)),
        }
    }

    pub fn model(&self) -> ModelState {
        self.model.lock().unwrap().clone()
    }

    /// Asks the decoder to switch to `model`.
    pub fn request_model(&self, model: Choice) {
        let mut state = self.model.lock().unwrap();
        state.pending = (model != state.active).then_some(model);
        (state.failed, state.failure) = (None, None);
    }

    /// Records the outcome of loading `model`, which was pending: why it runs on the CPU if the GPU was
    /// picked but couldn't be used, or why it failed.
    pub fn finish_switch(&self, model: Choice, outcome: Result<Option<String>, String>) {
        let mut state = self.model.lock().unwrap();
        match outcome {
            Ok(gpu_fallback) => {
                (state.active, state.loaded, state.gpu_fallback) = (model, true, gpu_fallback);
            }
            Err(why) => (state.failed, state.failure) = (Some(model), Some(why)),
        }
        if state.pending == Some(model) {
            state.pending = None;
        }
    }

    /// Picks `model` again while another runs in its place until it is downloaded, unless the user
    /// picked something else meanwhile.
    pub fn wait_for(&self, model: Choice) {
        let mut state = self.model.lock().unwrap();
        if state.pending.is_none() && model != state.active {
            state.pending = Some(model);
        }
    }

    /// Records that no model is running, e.g. while stopped or waiting for a download.
    pub fn unloaded(&self) {
        self.model.lock().unwrap().loaded = false;
    }

    pub fn post_notice(&self, text: String) {
        *self.notice.lock().unwrap() = Some(text);
    }

    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().unwrap().take()
    }

    pub fn offer_update(&self, release: Release) {
        *self.update.lock().unwrap() = Some(release);
    }

    pub fn take_update(&self) -> Option<Release> {
        self.update.lock().unwrap().take()
    }

    pub fn post_checked(&self, found: Result<bool, String>) {
        *self.checked.lock().unwrap() = Some(found);
    }

    pub fn take_checked(&self) -> Option<Result<bool, String>> {
        self.checked.lock().unwrap().take()
    }

    /// Sets the words the model listens for (see `ModelSpec::hotwords`); the decoder hands them on.
    pub fn set_hotwords(&self, words: Vec<String>) {
        let mut hotwords = self.hotwords.lock().unwrap();
        if hotwords.0 != words {
            *hotwords = (words, true);
        }
    }

    /// The words to load a model with.
    pub fn hotwords(&self) -> Vec<String> {
        self.hotwords.lock().unwrap().0.clone()
    }

    /// The words, if they changed since last taken.
    pub fn take_changed_hotwords(&self) -> Option<Vec<String>> {
        let mut hotwords = self.hotwords.lock().unwrap();
        std::mem::take(&mut hotwords.1).then(|| hotwords.0.clone())
    }

    pub fn set_helper_process(&self, pid: Option<u32>) {
        self.helper_process.store(pid.unwrap_or(0), Ordering::Relaxed);
    }

    pub fn helper_process(&self) -> Option<u32> {
        Some(self.helper_process.load(Ordering::Relaxed)).filter(|&pid| pid != 0)
    }

    /// Asks for a section's lines, or every section's, to be saved to the history database.
    pub fn request_save(&self, request: SaveRequest) {
        let mut pending = self.save_request.lock().unwrap();
        // Saving everything covers any one section asked for too.
        if *pending != Some(SaveRequest::All) {
            *pending = Some(request);
        }
    }

    pub fn take_save_request(&self) -> Option<SaveRequest> {
        self.save_request.lock().unwrap().take()
    }

    /// Stops capturing the app named `name`, until Kiku quits or `always`; blocking it again replaces how long.
    pub fn block(&self, name: &str, always: bool) {
        let mut blocked = self.blocked.lock().unwrap();
        blocked.retain(|app| app.name != name);
        blocked.push(Blocked { name: name.to_owned(), always });
    }

    pub fn unblock(&self, name: &str) {
        self.blocked.lock().unwrap().retain(|app| app.name != name);
    }

    pub fn is_blocked(&self, name: &str) -> bool {
        self.blocked.lock().unwrap().iter().any(|app| app.name == name)
    }

    /// The blocked apps, in the order they were blocked.
    pub fn blocked(&self) -> Vec<Blocked> {
        self.blocked.lock().unwrap().clone()
    }

    /// Queues dictated text for the overlay to type.
    pub fn queue_typing(&self, text: String) {
        self.typing.lock().unwrap().push(text);
    }

    /// Dictated text not yet typed, oldest first.
    pub fn take_typing(&self) -> Vec<String> {
        std::mem::take(&mut *self.typing.lock().unwrap())
    }

    pub fn threshold_db(&self, input: Input) -> f32 {
        self.input(input).threshold_db.get()
    }

    pub fn set_threshold_db(&self, input: Input, db: f32) {
        self.input(input).threshold_db.set(db.clamp(THRESHOLD_MIN_DB, THRESHOLD_MAX_DB));
    }

    /// Records a level for the meter; the meter shows the loudest since it last read.
    pub fn report_level(&self, input: Input, db: f32) {
        self.input(input).peak_db.raise(db);
    }

    /// The loudest level since the last call, for drawing the meter.
    pub fn take_level(&self, input: Input) -> f32 {
        self.input(input).peak_db.replace(SILENT_DB)
    }

    fn input(&self, input: Input) -> &InputControls {
        match input {
            Input::Mic => &self.mic,
            Input::Computer => &self.computer,
        }
    }
}

impl InputControls {
    fn new(threshold_db: f32) -> Self {
        Self { threshold_db: AtomicF32::new(threshold_db), peak_db: AtomicF32::new(SILENT_DB) }
    }
}

/// Silences audio below a threshold, like a voice-activation gate. It stays open a little
/// after the last loud block so the ends of words aren't cut off.
#[derive(Default)]
pub struct Gate {
    open_for: usize,
    /// Samples of unbroken silence out of the gate, up to and including the last block.
    silent_for: usize,
}

/// Samples the gate stays open after the last block above the threshold.
const HOLD_SAMPLES: usize = SAMPLE_RATE as usize / 2;
/// Silence still fed to a model after sound: enough for every model's speech detector to end the
/// phrase and keep the audio it takes after it.
const FED_SILENCE_SAMPLES: usize = 2 * SAMPLE_RATE as usize;

impl Gate {
    /// Applies the gate to `samples` in place and returns their level in dBFS.
    pub fn apply(&mut self, samples: &mut [f32], threshold_db: f32) -> f32 {
        let level = level_db(samples);
        if level >= threshold_db {
            self.open_for = HOLD_SAMPLES;
        } else if self.open_for > 0 {
            self.open_for = self.open_for.saturating_sub(samples.len());
        } else {
            samples.fill(0.0);
        }
        let silent = samples.iter().all(|&sample| sample == 0.0);
        self.silent_for = if silent { self.silent_for + samples.len() } else { 0 };
        level
    }

    /// Whether the block just gated can be left out: it is silence after enough silence that every
    /// model's speech detector has finished the phrase before it, so feeding it would only cost CPU.
    pub fn idle(&self) -> bool {
        self.silent_for > FED_SILENCE_SAMPLES
    }
}

/// RMS level in dBFS.
fn level_db(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return SILENT_DB;
    }
    let mean_square = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    (10.0 * mean_square.log10()).max(SILENT_DB)
}

/// An f32 stored in an `AtomicU32`.
struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }

    fn replace(&self, value: f32) -> f32 {
        f32::from_bits(self.0.swap(value.to_bits(), Ordering::Relaxed))
    }

    fn raise(&self, value: f32) {
        let _ = self.0.try_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
            (value > f32::from_bits(bits)).then_some(value.to_bits())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(amplitude: f32) -> Vec<f32> {
        vec![amplitude; SAMPLE_RATE as usize / 10]
    }

    #[test]
    fn changed_hotwords_are_taken_once() {
        let choice = Choice { model: crate::catalog::by_id_or_default(crate::catalog::DEFAULT_ID), device: crate::engine::Device::Cpu };
        let controls = Controls::new(THRESHOLD_MIN_DB, THRESHOLD_MIN_DB, choice);
        controls.set_hotwords(vec!["Kiku".into()]);
        assert_eq!(controls.take_changed_hotwords(), Some(vec!["Kiku".into()]));
        assert_eq!(controls.take_changed_hotwords(), None);
        controls.set_hotwords(vec!["Kiku".into()]);
        assert_eq!(controls.take_changed_hotwords(), None, "the same words are no change");
        assert_eq!(controls.hotwords(), ["Kiku"]);
    }

    #[test]
    fn quiet_audio_is_silenced_and_loud_audio_passes() {
        let mut gate = Gate::default();
        let mut quiet = block(0.001); // -60 dBFS
        gate.apply(&mut quiet, -40.0);
        assert!(quiet.iter().all(|&s| s == 0.0));
        let mut loud = block(0.1); // -20 dBFS
        gate.apply(&mut loud, -40.0);
        assert!(loud.iter().all(|&s| s == 0.1));
    }

    #[test]
    fn gate_holds_open_briefly_after_speech() {
        let mut gate = Gate::default();
        gate.apply(&mut block(0.1), -40.0);
        let mut tail = block(0.001);
        gate.apply(&mut tail, -40.0);
        assert!(tail.iter().all(|&s| s == 0.001), "word endings must not be cut");
        for _ in 0..5 {
            gate.apply(&mut block(0.001), -40.0);
        }
        let mut later = block(0.001);
        gate.apply(&mut later, -40.0);
        assert!(later.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn silence_is_fed_until_speech_detectors_are_done_then_left_out() {
        let mut gate = Gate::default();
        gate.apply(&mut block(0.1), -40.0);
        assert!(!gate.idle());
        let mut silent_blocks_fed = 0;
        loop {
            gate.apply(&mut block(0.0), -40.0);
            if gate.idle() {
                break;
            }
            silent_blocks_fed += 1;
        }
        assert_eq!(silent_blocks_fed * block(0.0).len(), FED_SILENCE_SAMPLES);
        gate.apply(&mut block(0.1), -40.0);
        assert!(!gate.idle(), "sound is always fed");
    }

    #[test]
    fn blocking_again_replaces_how_long_and_unblocking_ends_it() {
        let choice = Choice { model: crate::catalog::by_id_or_default(crate::catalog::DEFAULT_ID), device: crate::engine::Device::Cpu };
        let controls = Controls::new(THRESHOLD_MIN_DB, THRESHOLD_MIN_DB, choice);
        controls.block("Discord", false);
        controls.block("Opera", true);
        controls.block("Discord", true);
        assert!(controls.is_blocked("Discord") && !controls.is_blocked("Steam"));
        assert_eq!(controls.blocked().iter().map(|app| (app.name.as_str(), app.always)).collect::<Vec<_>>(), [("Opera", true), ("Discord", true)]);
        controls.unblock("Discord");
        assert!(!controls.is_blocked("Discord"));
    }

    #[test]
    fn meter_reports_the_loudest_level_since_last_read() {
        let choice = Choice { model: crate::catalog::by_id_or_default(crate::catalog::DEFAULT_ID), device: crate::engine::Device::Cpu };
        let controls = Controls::new(THRESHOLD_MIN_DB, THRESHOLD_MIN_DB, choice);
        controls.report_level(Input::Mic, -30.0);
        controls.report_level(Input::Mic, -50.0);
        assert_eq!(controls.take_level(Input::Mic), -30.0);
        assert_eq!(controls.take_level(Input::Mic), SILENT_DB);
    }
}
