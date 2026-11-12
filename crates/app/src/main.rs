//! Kiku, a live captions overlay: a section for each app that plays audio, plus your microphone.
//! The model and sensitivity are picked in the panel's settings.

// A windowed app, so Windows doesn't open a console beside the panel.
#![windows_subsystem = "windows"]

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::Ordering,
        mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use kiku::{
    capture::{self, default_microphone, Capture, Chunk, Source},
    hardware, hosting, overlay,
    processes::{exe_path, snapshot, Process},
    sessions::Sessions,
};
use kiku_core::{
    captions::{Captions, StreamId, YOU, YOU_SECTION},
    controls::{Controls, Gate, Input, THRESHOLD_MIN_DB},
    engine::{self, Device, Engine, EngineStream, Line, ModelSpec, Result, POLL_INTERVAL},
    hardware::Hardware,
    log,
    misses::{self, Recent},
    remote::{self, HostCode, ModelRequest},
    recording::Recorder,
    catalog::{self, Choice, GPU_PACK},
    download::{self, runnable, Target as Download},
    settings::{EngineWhere, Settings},
    status::StatusFile,
    update,
    words::Output,
};

/// Audio this old when it reaches the decoder means captions are lagging; worth a log line.
const LAG_WARNING: Duration = Duration::from_secs(2);
const LAG_LOG_EVERY: Duration = Duration::from_secs(30);
/// Discovery runs on Windows' audio news; this pass catches anything a missed notification hides.
const SAFETY_NET_INTERVAL: Duration = Duration::from_secs(60);
/// An app's capture is released once its audio has been quiet this long, and restarted when it plays.
const QUIET_RELEASE: Duration = Duration::from_secs(60);
/// A source whose capture failed is retried after this, doubling with each failure in a row up to the
/// longest wait. A capture that ran that long before failing starts over at the first wait.
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LONGEST_RETRY: Duration = Duration::from_secs(300);
/// Chunks waiting for the decoder at most: about 30 s of audio from four sources. Past it, captures
/// drop audio rather than wait, so a stalled decoder can't make memory grow without end.
const QUEUED_CHUNKS: usize = 1200;
/// How long quitting waits for the last words to be transcribed and saved.
const QUIT_WAIT: Duration = Duration::from_secs(3);

/// A captured source and the name its captions are shown under.
struct Labeled {
    label: String,
    source: Source,
    /// The app's executable, for its icon.
    exe_path: Option<PathBuf>,
}

/// What discovery tells the decoder. Sources are numbered in the order they are added.
enum SourceEvent {
    Added(Labeled),
    /// The source's capture ended (its app exited, or capture failed); its stream can go.
    Ended(usize),
}

fn main() -> Result<()> {
    // Before the log starts, so a second start doesn't rotate the running Kiku's log away.
    if !overlay::claim_single_instance() {
        return Ok(());
    }
    log::start();
    let hardware = hardware::detect();
    log::write(format!("{hardware:?}"));
    catalog::set_gpu_memory_mb(hardware.gpu.as_ref().map_or(0, |gpu| gpu.vram_mb));
    let first_run = !kiku_core::settings::exist();
    let settings = Settings::load();
    let captions = Arc::new(Mutex::new(Captions { pinned: settings.pinned.clone(), ..Captions::default() }));
    let controls = Arc::new(Controls::new(settings.mic_threshold_db, settings.computer_threshold_db, settings.choice()));
    controls.keep_days.store(settings.keep_days, Ordering::Relaxed);
    controls.auto_save.store(settings.auto_save, Ordering::Relaxed);
    controls.save_mic_only.store(settings.save_mic_only, Ordering::Relaxed);
    controls.check_updates.store(settings.check_updates, Ordering::Relaxed);
    controls.dictation_only.store(settings.dictation_only, Ordering::Relaxed);
    controls.voices_apps.store(settings.voices_apps, Ordering::Relaxed);
    controls.voices_mic.store(settings.voices_mic, Ordering::Relaxed);
    controls.set_engine_where(settings.engine_where);
    controls.set_engine_host(&settings.engine_host);
    controls.take_engine_changed();
    controls.set_host_on(settings.host_on);
    controls.words.set_settings(settings.words);
    controls.words.refresh();
    controls.set_hotwords(controls.words.hotwords());
    for app in &settings.blocked {
        controls.block(app, true);
    }

    let (audio, chunks) = sync_channel(QUEUED_CHUNKS);
    let (events, source_events) = channel();
    let (wake_discovery, discovery_wakes) = channel();
    let (decoder_done, decoder_finished) = channel();
    let decoder = Decoder {
        captions: captions.clone(),
        controls: controls.clone(),
        recorder: RefCell::new(Recorder::start()),
        status: RefCell::default(),
        wake_discovery: wake_discovery.clone(),
        hardware: hardware.clone(),
        awaiting_gpu: Cell::new(false),
        last_notice: RefCell::default(),
        host: RefCell::default(),
        loaded_voices: Cell::new(false),
        speakers_failed: Cell::new(None),
    };
    let discovery = Discovery::new(audio, events, wake_discovery.clone(), controls.clone());
    let setup = overlay::Setup { settings, hardware, first_run };
    let updates = controls.clone();
    let updates_for_hosting = controls.clone();
    overlay::run(captions, controls.clone(), setup, wake_discovery, move |overlay| {
        std::thread::spawn(move || discovery.run(&discovery_wakes));
        // Not before setup is done, where checking can be turned off first.
        let checks = updates.clone();
        let allowed = move || kiku_core::settings::exist() && checks.check_updates.load(Ordering::Relaxed);
        let offer = move |release| {
            updates.offer_update(release);
            overlay.refresh();
        };
        std::thread::spawn(move || update::watch(allowed, offer));
        let hosting = updates_for_hosting;
        std::thread::spawn(move || hosting::run(&hosting));
        std::thread::spawn(move || {
            decoder.run(&chunks, &source_events, overlay);
            decoder.status.borrow_mut().clear();
            let _ = decoder_done.send(());
        });
    })?;
    // Let the decoder finish what's being said and save it before the process ends.
    controls.quitting.store(true, Ordering::Relaxed);
    let _ = decoder_finished.recv_timeout(QUIT_WAIT);
    log::write("closed");
    Ok(())
}

/// Keeps one capture running for the mic and for each app that plays audio and isn't blocked, while
/// captioning runs.
struct Discovery {
    audio: SyncSender<Chunk>,
    events: Sender<SourceEvent>,
    /// Wakes discovery for another pass: Windows' audio news, a capture ending, captioning stopping or
    /// starting, an app blocked or unblocked.
    wake: Sender<()>,
    controls: Arc<Controls>,
    running: Vec<Running>,
    /// Sources whose capture failed lately, and when to try each again.
    retries: HashMap<Target, Retry>,
    next_index: usize,
}

struct Running {
    index: usize,
    target: Target,
    capture: Capture,
    started: Instant,
    /// Since when the app has had no audio session playing; `None` while it plays, and for the mic.
    quiet_since: Option<Instant>,
}

struct Retry {
    after: Duration,
    at: Instant,
}

/// What a capture listens to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Target {
    /// Your mic, on the recording device that was Windows' default when it opened.
    Mic { device: String },
    /// An app, by its main process.
    App { pid: u32, name: String },
}

impl Discovery {
    fn new(audio: SyncSender<Chunk>, events: Sender<SourceEvent>, wake: Sender<()>, controls: Arc<Controls>) -> Self {
        Self { audio, events, wake, controls, running: Vec::new(), retries: HashMap::new(), next_index: 0 }
    }

    /// Runs until the decoder goes away: a pass whenever Windows says an app opened audio or started or
    /// stopped playing or an audio device changed, a capture ends (its app exited, or it failed),
    /// captioning stops or starts, a quiet app or a failed source is due, and once a minute regardless.
    fn run(mut self, wakes: &Receiver<()>) {
        if let Err(error) = wasapi::initialize_mta().ok() {
            log::write(format!("audio discovery unavailable: {error}"));
            return;
        }
        let mut sessions = Sessions::new(self.wake.clone());
        while let Some(next_pass) = self.tick(&mut sessions) {
            let _ = wakes.recv_timeout(next_pass.saturating_duration_since(Instant::now()));
            // One pass covers every wake-up that arrived meanwhile.
            while wakes.try_recv().is_ok() {}
        }
    }

    /// One pass: end the captures no longer wanted, then, unless captioning is stopped, capture the
    /// mic and every playing app. Returns when the next pass is due unless something wakes discovery
    /// first; `None` to stop.
    fn tick(&mut self, sessions: &mut Sessions) -> Option<Instant> {
        let now = Instant::now();
        let stopped = self.controls.stopped.load(Ordering::Relaxed);
        let processes = snapshot();
        let wanted = if stopped { Vec::new() } else { self.wanted(sessions, &processes) };
        for running in &mut self.running {
            if let Target::App { .. } = running.target {
                running.quiet_since = if wanted.contains(&running.target) { None } else { Some(running.quiet_since.unwrap_or(now)) };
            }
        }

        // While stopped nothing is captured, so Windows' mic-in-use light goes off. An app keeps its
        // capture through a quiet minute, so a pause doesn't cost the start of what it says next,
        // unless it was blocked.
        let controls = self.controls.clone();
        let keep = |running: &Running| {
            !stopped
                && match &running.target {
                    Target::App { pid, name } => {
                        processes.iter().any(|p| p.pid == *pid)
                            && running.quiet_since.is_none_or(|since| now < since + QUIET_RELEASE)
                            && !controls.is_blocked(name)
                            && !controls.dictation_only.load(Ordering::Relaxed)
                    }
                    // The default mic changed or was unplugged: the mic moves to the new default.
                    Target::Mic { .. } => wanted.contains(&running.target),
                }
        };
        if !self.retire(keep) {
            return None;
        }
        self.retries.retain(|target, _| wanted.contains(target));
        for target in wanted {
            let due = self.retries.get(&target).is_none_or(|retry| retry.at <= now);
            if due && !self.running.iter().any(|r| r.target == target) && !self.start(target) {
                return None;
            }
        }
        let releases = self.running.iter().filter_map(|r| r.quiet_since).map(|since| since + QUIET_RELEASE);
        let retries = self.retries.values().map(|retry| retry.at).filter(|&at| at > now);
        Some(releases.chain(retries).fold(now + SAFETY_NET_INTERVAL, Instant::min))
    }

    /// The default mic and every app playing audio that isn't blocked; only the mic in dictation only mode.
    fn wanted(&self, sessions: &mut Sessions, processes: &[Process]) -> Vec<Target> {
        let mic = default_microphone().map(|device| Target::Mic { device });
        if self.controls.dictation_only.load(Ordering::Relaxed) {
            return mic.into_iter().collect();
        }
        let apps = sessions.playing_apps(processes).unwrap_or_else(|error| {
            log::write(format!("audio session lookup failed: {error}"));
            Vec::new()
        });
        let apps = apps.into_iter().filter(|app| !self.controls.is_blocked(&app.name));
        mic.into_iter().chain(apps.map(|app| Target::App { pid: app.pid, name: app.name })).collect()
    }

    /// Announces the source, then starts capturing it, so the decoder always knows a chunk's source.
    fn start(&mut self, target: Target) -> bool {
        let labeled = match &target {
            Target::Mic { .. } => Labeled { label: YOU.into(), source: Source::Microphone, exe_path: None },
            Target::App { pid, name } => Labeled { label: name.clone(), source: Source::App { pid: *pid }, exe_path: exe_path(*pid) },
        };
        let (index, source) = (self.next_index, labeled.source.clone());
        log::write(format!("source {index}: listening to {}", labeled.label));
        if self.events.send(SourceEvent::Added(labeled)).is_err() {
            return false;
        }
        let wake = self.wake.clone();
        let capture = capture::spawn_notifying(index, source, self.audio.clone(), move || {
            let _ = wake.send(());
        });
        self.running.push(Running { index, target, capture, started: Instant::now(), quiet_since: None });
        self.next_index += 1;
        true
    }

    /// Ends the captures `keep` turns down and drops those that ended on their own, reporting why.
    /// Returns false once the decoder is gone.
    fn retire(&mut self, keep: impl Fn(&Running) -> bool) -> bool {
        let (ended, running): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.running).into_iter().partition(|r| r.capture.thread.is_finished() || !keep(r));
        self.running = running;
        for ended in ended {
            ended.capture.stop();
            match ended.capture.thread.join() {
                Ok(Err(error)) => {
                    log::write(format!("source {}: capture failed: {error}", ended.index));
                    self.put_off(ended.target, ended.started.elapsed());
                }
                _ => log::write(format!("source {}: capture ended", ended.index)),
            }
            if self.events.send(SourceEvent::Ended(ended.index)).is_err() {
                return false;
            }
        }
        true
    }

    /// Delays the next try of a target whose capture failed after running `ran_for`.
    fn put_off(&mut self, target: Target, ran_for: Duration) {
        let after = match self.retries.get(&target) {
            Some(retry) if ran_for < LONGEST_RETRY => (retry.after * 2).min(LONGEST_RETRY),
            _ => FIRST_RETRY,
        };
        self.retries.insert(target, Retry { after, at: Instant::now() + after });
    }
}

/// Turns captured audio into captions on its own thread.
struct Decoder {
    captions: Arc<Mutex<Captions>>,
    controls: Arc<Controls>,
    recorder: RefCell<Recorder>,
    status: RefCell<StatusFile>,
    /// Tells discovery captioning stopped or started, to release or reopen the captures.
    wake_discovery: Sender<()>,
    hardware: Hardware,
    /// The model runs on the CPU only until its GPU files (build or pack) finish downloading.
    awaiting_gpu: Cell<bool>,
    /// The last notice posted, so one event is told once however often the model loads.
    last_notice: RefCell<Option<String>>,
    /// Running on this PC though the host is wanted: when to try the host again, and an attempt under way.
    host: RefCell<HostRetry>,
    /// Whether the running model was loaded with the speaker model (or asked of the host to be).
    loaded_voices: Cell<bool>,
    /// After a load for speakers failed: the switches it was for. It isn't tried again until they change, so a
    /// load that keeps failing never cuts live speech over and over (the old model keeps running meanwhile).
    speakers_failed: Cell<Option<bool>>,
}

/// Trying the host again while the model runs on this PC.
struct HostRetry {
    next_try: Instant,
    /// An attempt on its own thread, with what it was made for.
    attempt: Option<HostAttempt>,
    /// The engine adopted can't take the current words as they are: load again to apply them.
    reload: bool,
}

struct HostAttempt {
    code: String,
    choice: Choice,
    /// Whether it asked for speakers.
    voices: bool,
    answer: Receiver<Result<Box<dyn Engine>>>,
}

impl Default for HostRetry {
    fn default() -> Self {
        Self { next_try: Instant::now(), attempt: None, reload: false }
    }
}


/// How long after the host fails the model runs on this PC before the host is tried again.
const HOST_RETRY: Duration = Duration::from_secs(60);

/// Per-source state that outlives a model switch, indexed like the sources were added.
#[derive(Default)]
struct Sources {
    /// The caption section of each app; `None` for the mic, whose lines are placed per line.
    sections: Vec<Option<usize>>,
    gates: Vec<Gate>,
    labels: Vec<String>,
    /// What each source's stream was given lately, for saving a miss; emptied once the source ends.
    recent: Vec<Recent>,
    /// False once a source has ended.
    alive: Vec<bool>,
    /// Streams opened so far, which numbers the next one.
    opened: StreamId,
}

/// An engine stream and the number its captions are keyed by.
struct Stream<'e> {
    id: StreamId,
    engine: Box<dyn EngineStream + 'e>,
    /// Audio went in since the last poll; a stream given nothing has nothing new to say.
    fed: bool,
    /// Whether it was opened telling voices apart.
    voices: bool,
}

impl Sources {
    /// A new stream for source `index`; `None`, logged, if the engine can't open one, so that
    /// source goes uncaptioned while the others carry on.
    fn open<'e>(&mut self, engine: &'e dyn Engine, index: usize, controls: &Controls) -> Option<Stream<'e>> {
        self.opened += 1;
        let voices = controls.voices_for(self.sections[index].is_none());
        let stream = engine.open_stream_with(voices).inspect_err(|error| log::write(format!("source {index}: couldn't open a stream: {error}")));
        Some(Stream { id: self.opened, engine: stream.ok()?, fed: false, voices })
    }
}

/// How [`Decoder::collect_all`] gathers lines.
#[derive(Clone, Copy, PartialEq)]
enum Collect {
    Poll,
    /// Poll, and type your finished lines.
    Dictating,
    /// Dictation just ended: finish what you were saying now, and type it.
    FlushMic,
    /// Captioning is stopping: finish what everyone is saying now.
    FlushAll,
}

/// Why decoding with one engine stopped.
enum Stop {
    /// Capture has ended for good; the app is closing.
    Finished,
    /// The user picked another model, already loaded.
    Switch(Box<dyn Engine>),
    /// The user stopped captioning; the model is released until they start again.
    Stopped,
    /// The engine stopped working (its helper process ended); it is loaded again.
    Lost,
}

/// The picks (pending, active), whether each can load, the fallback, and where models run with the host code.
type Readiness = (Option<Choice>, Choice, [bool; 2], Option<Choice>, (EngineWhere, String));

impl Decoder {
    /// Decodes until capture ends, switching models when the user asks and releasing the model while stopped.
    fn run(&self, chunks: &Receiver<Chunk>, source_events: &Receiver<SourceEvent>, overlay: overlay::Handle) {
        let mut sources = Sources::default();
        if !self.wait_while_stopped(&mut sources, chunks, source_events) {
            return;
        }
        let Some(mut engine) = self.load_or_wait(&mut sources, chunks, source_events, overlay) else {
            self.recorder.borrow_mut().finish();
            return;
        };
        loop {
            self.controls.set_helper_process(engine.helper_process());
            match self.decode(engine.as_ref(), &mut sources, chunks, source_events, overlay) {
                Stop::Finished => {
                    self.recorder.borrow_mut().finish();
                    return;
                }
                Stop::Switch(next) => engine = next,
                Stop::Stopped | Stop::Lost => {
                    drop(engine);
                    self.controls.set_helper_process(None);
                    self.unloaded();
                    overlay.refresh();
                    let restarted = self.wait_while_stopped(&mut sources, chunks, source_events)
                        .then(|| self.load_or_wait(&mut sources, chunks, source_events, overlay))
                        .flatten();
                    let Some(next) = restarted else {
                        self.recorder.borrow_mut().finish();
                        return;
                    };
                    engine = next;
                    overlay.refresh();
                }
            }
        }
    }

    /// Keeps track of sources until the user starts captioning again, while discovery releases the
    /// captures. Returns false if capture ended meanwhile.
    fn wait_while_stopped(&self, sources: &mut Sources, chunks: &Receiver<Chunk>, source_events: &Receiver<SourceEvent>) -> bool {
        // Discovery looks at whether captioning runs, here on stopping and below on starting.
        let _ = self.wake_discovery.send(());
        while self.controls.stopped.load(Ordering::Relaxed) {
            let disconnected = matches!(chunks.recv_timeout(POLL_INTERVAL), Err(RecvTimeoutError::Disconnected));
            if disconnected || self.controls.quitting.load(Ordering::Relaxed) {
                return false;
            }
            for event in source_events.try_iter() {
                self.track_source(sources, event);
            }
            self.report_status(sources);
        }
        let _ = self.wake_discovery.send(());
        true
    }

    fn decode(
        &self,
        engine: &dyn Engine,
        sources: &mut Sources,
        chunks: &Receiver<Chunk>,
        source_events: &Receiver<SourceEvent>,
        overlay: overlay::Handle,
    ) -> Stop {
        let alive = sources.alive.clone();
        let mut streams: Vec<_> = alive.iter().enumerate().map(|(index, &alive)| alive.then(|| sources.open(engine, index, &self.controls)).flatten()).collect();
        let mut next_poll = Instant::now() + POLL_INTERVAL;
        let mut last_lag_log: Option<Instant> = None;
        let mut was_dictating = false;
        loop {
            let received = chunks.recv_timeout(next_poll.saturating_duration_since(Instant::now()));
            if let Ok(chunk) = &received {
                let lag = chunk.captured.elapsed();
                if lag > LAG_WARNING && last_lag_log.is_none_or(|t| t.elapsed() > LAG_LOG_EVERY) {
                    log::write(format!("decoding is {:.1} s behind the audio", lag.as_secs_f32()));
                    last_lag_log = Some(Instant::now());
                }
            }
            self.take_source_events(engine, sources, &mut streams, source_events);
            match received {
                Ok(chunk) => self.feed(engine, sources, &mut streams, chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Stop::Finished,
            }
            // Quitting and stopping first finish and save what's being said, including audio still queued.
            let quitting = self.controls.quitting.load(Ordering::Relaxed);
            let stopped = self.controls.stopped.load(Ordering::Relaxed);
            if quitting || stopped {
                while let Ok(chunk) = chunks.try_recv() {
                    self.take_source_events(engine, sources, &mut streams, source_events);
                    self.feed(engine, sources, &mut streams, chunk);
                }
                self.finish_speech(engine, &mut streams, sources, overlay);
                return if quitting { Stop::Finished } else { Stop::Stopped };
            }
            if self.controls.save_miss.swap(false, Ordering::Relaxed) {
                self.save_miss(sources);
            }
            let dictating = self.controls.dictating.load(Ordering::Relaxed);
            if was_dictating && !dictating {
                // Let go of the key: what you were saying is finished now, not after a pause.
                self.collect_all(engine, &mut streams, sources, Collect::FlushMic);
                overlay.refresh();
                overlay.type_queued();
            }
            was_dictating = dictating;
            if Instant::now() >= next_poll {
                if engine.lost() {
                    let state = self.controls.model();
                    self.tell(match state.remote {
                        Some(_) => format!("Lost the host, so {} is loading again, on the host if it answers and on this PC if not.", state.active.model.name),
                        None => format!("{} stopped unexpectedly (its helper process ended), so it's loading again.", state.active.model.name),
                    });
                    return Stop::Lost;
                }
                let collect = if dictating { Collect::Dictating } else { Collect::Poll };
                if self.collect_all(engine, &mut streams, sources, collect) {
                    overlay.refresh();
                    if dictating || self.controls.has_heard() {
                        overlay.type_queued();
                    }
                }
                if let Some(request) = self.controls.take_save_request() {
                    let mut recorder = self.recorder.borrow_mut();
                    recorder.save_captions(&self.captions.lock().unwrap(), request);
                    recorder.write();
                }
                self.recorder.borrow_mut().tick(&self.controls);
                self.report_status(sources);
                if self.controls.words.refresh() {
                    overlay.refresh();
                }
                // New hotwords apply from the next phrase, unless turning them on or off needs a reload.
                self.controls.set_hotwords(self.controls.words.hotwords());
                let active = self.controls.model().active;
                let reload = self.controls.take_changed_hotwords().is_some_and(|words| !engine.set_hotwords(&active.hotwords(words)));
                let reload = self.controls.take_engine_changed() || reload || std::mem::take(&mut self.host.borrow_mut().reload) || self.speakers_stale();
                let mut from_host = self.host_answered();
                // A pick still downloading waits, and captions go on meanwhile.
                if self.controls.model().pending.is_some_and(|choice| self.can_load(choice)) || self.gpu_move_due() || reload || from_host.is_some() {
                    self.finish_speech(engine, &mut streams, sources, overlay);
                }
                let reloaded = || reload.then(|| self.reload()).flatten();
                if let Some(next) = self.load_requested_model().or_else(|| self.move_to_gpu()).or_else(reloaded).or_else(|| from_host.take().map(|engine| self.adopt_host(engine))) {
                    overlay.refresh();
                    return Stop::Switch(next);
                }
                self.follow_voice_switches(engine, sources, &mut streams, overlay);
                next_poll = Instant::now() + POLL_INTERVAL;
            }
        }
    }

    /// Opens and closes streams for sources discovery added or lost. Sources are added before their
    /// capture starts, so once a chunk has been received, its source's event is already queued.
    fn take_source_events<'e>(
        &self,
        engine: &'e dyn Engine,
        sources: &mut Sources,
        streams: &mut Vec<Option<Stream<'e>>>,
        source_events: &Receiver<SourceEvent>,
    ) {
        for event in source_events.try_iter() {
            let ended = if let SourceEvent::Ended(index) = &event { Some(*index) } else { None };
            self.track_source(sources, event);
            match ended {
                Some(index) => streams[index] = None,
                None => streams.push(sources.open(engine, streams.len(), &self.controls)),
            }
        }
    }

    /// Whether the running model's speaker model differs from what the switches ask: one to drop, or one to
    /// add once it is downloaded (the host has its own).
    fn speakers_stale(&self) -> bool {
        let wanted = self.controls.voices_wanted();
        if self.speakers_failed.get() == Some(wanted) {
            return false;
        }
        match self.loaded_voices.get() {
            true => !wanted,
            false => wanted && (self.controls.model().remote.is_some() || catalog::speaker_model().is_some()),
        }
    }

    /// Reopens the streams whose switch changed, finishing what they were saying first; their speakers start again.
    fn follow_voice_switches<'e>(&self, engine: &'e dyn Engine, sources: &mut Sources, streams: &mut [Option<Stream<'e>>], overlay: overlay::Handle) {
        let changed = |sources: &Sources, index: usize, stream: &Stream| stream.voices != self.controls.voices_for(sources.sections[index].is_none());
        if !streams.iter().enumerate().any(|(index, stream)| stream.as_ref().is_some_and(|stream| changed(sources, index, stream))) {
            return;
        }
        let now = Instant::now();
        for index in 0..streams.len() {
            if !streams[index].as_ref().is_some_and(|stream| changed(sources, index, stream)) {
                continue;
            }
            // Only this stream finishes: the mic goes on dictating when only the apps' switch changed.
            let mic = sources.sections[index].is_none();
            let collect = if mic && self.controls.dictating.load(Ordering::Relaxed) { Collect::FlushMic } else { Collect::FlushAll };
            self.collect_stream(engine, streams, sources, index, collect, now);
            streams[index] = sources.open(engine, index, &self.controls);
        }
        if self.controls.has_heard() {
            overlay.type_queued();
        }
        self.recorder.borrow_mut().write();
        overlay.refresh();
    }

    /// Ends every line in progress now and saves it, before this engine's streams go away.
    fn finish_speech<'e>(&self, engine: &'e dyn Engine, streams: &mut [Option<Stream<'e>>], sources: &mut Sources, overlay: overlay::Handle) {
        self.collect_all(engine, streams, sources, Collect::FlushAll);
        if self.controls.has_heard() {
            overlay.type_queued();
        }
        self.recorder.borrow_mut().write();
        overlay.refresh();
    }

    /// Loads a model to caption with: a switch the user asked for, else the current model, else the
    /// default. While none can run (on first run, before a download finishes) it keeps track of
    /// sources and waits for a download or another pick. Returns `None` if capture ended meanwhile.
    fn load_or_wait(&self, sources: &mut Sources, chunks: &Receiver<Chunk>, source_events: &Receiver<SourceEvent>, overlay: overlay::Handle) -> Option<Box<dyn Engine>> {
        loop {
            let tried = self.readiness();
            if let Some(engine) = self.load_requested_model().or_else(|| self.load_active_model()) {
                overlay.refresh();
                return Some(engine);
            }
            self.unloaded();
            overlay.refresh();
            // Try again once something changes, or in a minute when the host is wanted, not over and over on a model that fails.
            let host_retry = self.host_wanted().map(|_| self.host.borrow().next_try);
            while self.readiness() == tried && !host_retry.is_some_and(|at| Instant::now() >= at) {
                let disconnected = matches!(chunks.recv_timeout(POLL_INTERVAL), Err(RecvTimeoutError::Disconnected));
                if disconnected || self.controls.quitting.load(Ordering::Relaxed) {
                    return None;
                }
                for event in source_events.try_iter() {
                    self.track_source(sources, event);
                }
            }
        }
    }

    /// Records that no model runs, and drops a host attempt so a host that answers isn't left holding a model nobody uses.
    fn unloaded(&self) {
        self.host.borrow_mut().attempt = None;
        self.controls.unloaded();
    }

    /// What decides which model can load: the picks, and which of them can run.
    fn readiness(&self) -> Readiness {
        let state = self.controls.model();
        let ready = [state.pending.is_some_and(|choice| self.can_load(choice)), self.can_load(state.active)];
        (state.pending, state.active, ready, download::fallback(), (self.controls.engine_where(), self.controls.engine_host()))
    }

    /// Whether a load of `choice` can be tried: its files are here, or the host is wanted and may have them.
    fn can_load(&self, choice: Choice) -> bool {
        runnable(choice) || self.host_wanted().is_some()
    }

    /// The host's code, when the model should run there and the code is one.
    fn host_wanted(&self) -> Option<HostCode> {
        (self.controls.engine_where() == EngineWhere::Host).then(|| HostCode::parse(&self.controls.engine_host()).ok()).flatten()
    }

    /// The current model, or one that is installed if it can't load (e.g. its files were removed or
    /// not downloaded yet). Why the current model failed stays shown.
    fn load_active_model(&self) -> Option<Box<dyn Engine>> {
        let active = self.controls.model().active;
        let mut failure = None;
        let fallback = download::fallback().filter(|&choice| choice != active);
        for choice in std::iter::once(active).filter(|&choice| self.can_load(choice)).chain(fallback) {
            // Only the pick is tried on the host; what runs in its place is on this PC.
            match self.load(choice, choice == active) {
                Ok((engine, gpu_fallback)) => {
                    if choice != active {
                        let (name, instead) = (&active.model.name, &choice.model.name);
                        self.tell(match &failure {
                            Some(why) => format!("{name} couldn't load ({why}), so {instead} is running instead. Settings → Listening shows why."),
                            None => format!("{name} isn't downloaded, so {instead} is running until it is. Settings → Listening can download it."),
                        });
                    }
                    self.controls.finish_switch(choice, Ok(gpu_fallback));
                    match failure {
                        Some(why) => self.controls.finish_switch(active, Err(why)),
                        // Still the pick: it loads once its download finishes.
                        None => self.controls.wait_for(active),
                    }
                    return Some(engine);
                }
                Err(error) if choice == active && runnable(choice) => failure = Some(error.to_string()),
                Err(_) => {}
            }
        }
        if let Some(why) = failure {
            self.controls.finish_switch(active, Err(why));
        }
        None
    }

    /// Loads the model the user picked, once it can run (a pick still downloading waits). The
    /// current one keeps running if it fails.
    fn load_requested_model(&self) -> Option<Box<dyn Engine>> {
        let model = self.controls.model().pending.filter(|&choice| self.can_load(choice))?;
        match self.load(model, true) {
            Ok((engine, gpu_fallback)) => {
                self.controls.finish_switch(model, Ok(gpu_fallback));
                Some(engine)
            }
            Err(error) => {
                self.controls.finish_switch(model, Err(error.to_string()));
                None
            }
        }
    }

    /// Loads `choice`, on the host if that is wanted and `host` allows, else on this PC. A GPU model that
    /// can't run on the GPU now runs on the CPU, returned with why.
    fn load(&self, choice: Choice, host: bool) -> Result<(Box<dyn Engine>, Option<String>)> {
        self.controls.take_engine_changed();
        let wanted = self.controls.voices_wanted();
        let speakers = wanted.then(catalog::speaker_model).flatten();
        if let Some(loaded) = host.then(|| self.load_on_host(choice)).flatten() {
            self.loaded_voices.set(wanted);
            self.awaiting_gpu.set(false);
            return Ok((loaded, None));
        }
        let awaiting_gpu = choice.device() == Device::Cuda && !Download::Model(choice.model, choice.variant()).complete();
        // A failed load leaves the model still running, and whether it waits for its GPU files, as they were.
        let had_speakers = speakers.is_some();
        let loaded = self.load_where_possible(choice, speakers).inspect_err(|_| self.speakers_failed.set(Some(wanted)))?;
        self.loaded_voices.set(had_speakers);
        self.speakers_failed.set(None);
        self.awaiting_gpu.set(awaiting_gpu);
        self.controls.set_running_on(None);
        Ok(loaded)
    }

    /// Loads `choice` on the host, if one is wanted. When it can't be, says so once and why, and the
    /// host is tried again in a minute.
    fn load_on_host(&self, choice: Choice) -> Option<Box<dyn Engine>> {
        if self.controls.engine_where() != EngineWhere::Host {
            return None;
        }
        let Some(code) = self.host_wanted() else {
            self.tell_host_unreached("the host's code isn't one", choice);
            return None;
        };
        match load_on_host_logged(&code, &host_request(choice, &self.controls)) {
            Ok(engine) => {
                self.controls.set_running_on(Some(code.address().to_owned()));
                Some(engine)
            }
            Err(error) => {
                self.tell_host_unreached(&error.to_string(), choice);
                None
            }
        }
    }

    fn tell_host_unreached(&self, why: &str, choice: Choice) {
        self.host.borrow_mut().next_try = Instant::now() + HOST_RETRY;
        if runnable(choice) {
            self.tell(format!("Couldn't reach the host ({why}), so the model runs on this PC. It tries again every minute."));
        } else {
            self.tell(format!("Couldn't reach the host ({why}), and {} isn't on this PC. Kiku tries the host again every minute; Settings → Listening can download it here instead.", choice.model.name));
        }
    }

    /// While the model runs on this PC though the host is wanted: tries the host once a minute, on a
    /// thread of its own so captions go on, and hands over the engine once the host has loaded the model.
    fn host_answered(&self) -> Option<Box<dyn Engine>> {
        let mut retry = self.host.borrow_mut();
        let state = self.controls.model();
        let Some(code) = self.host_wanted().filter(|_| state.remote.is_none() && state.loaded) else {
            retry.attempt = None;
            return None;
        };
        let (code_text, voices) = (code.to_string(), self.controls.voices_wanted());
        if let Some(attempt) = retry.attempt.take() {
            let matches = attempt.code == code_text && attempt.choice == state.active && attempt.voices == voices;
            match attempt.answer.try_recv() {
                Err(TryRecvError::Empty) if matches => {
                    retry.attempt = Some(attempt);
                    return None;
                }
                Ok(Ok(engine)) if matches => return Some(engine),
                Ok(Err(error)) if matches => log::write(format!("the host still can't be reached: {error}")),
                // Made for something no longer wanted: dropped, which ends its connection.
                _ if !matches => return None,
                _ => {}
            }
            retry.next_try = Instant::now() + HOST_RETRY;
            return None;
        }
        if Instant::now() < retry.next_try {
            return None;
        }
        let request = host_request(state.active, &self.controls);
        let (send, answer) = channel();
        std::thread::spawn(move || {
            let _ = send.send(load_on_host_logged(&code, &request));
        });
        retry.attempt = Some(HostAttempt { code: code_text, choice: state.active, voices, answer });
        None
    }

    /// Makes an engine the host loaded the running one, with the words the model listens for now.
    fn adopt_host(&self, engine: Box<dyn Engine>) -> Box<dyn Engine> {
        let active = self.controls.model().active;
        self.controls.set_running_on(self.host_wanted().map(|code| code.address().to_owned()));
        self.controls.finish_switch(active, Ok(None));
        self.loaded_voices.set(self.controls.voices_wanted());
        if !engine.set_hotwords(&active.hotwords(self.controls.hotwords())) {
            self.host.borrow_mut().reload = true;
        }
        self.awaiting_gpu.set(false);
        engine
    }

    fn load_where_possible(&self, choice: Choice, speakers: Option<PathBuf>) -> Result<(Box<dyn Engine>, Option<String>)> {
        if choice.device() != Device::Cuda {
            return load_logged(choice, &self.controls, speakers).map(|engine| (engine, None));
        }
        let (why, what_to_do) = match self.gpu_unready(choice) {
            Some(unready) => unready,
            None => match load_logged(choice, &self.controls, speakers.clone()) {
                Ok(engine) => return Ok((engine, None)),
                Err(error) => (format!("the GPU test failed: {error}"), "Settings → Listening can switch it to the CPU."),
            },
        };
        self.tell(format!("{} couldn't use the GPU ({why}), so it's running on the CPU. {what_to_do}", choice.model.name));
        load_logged(choice.on_cpu(), &self.controls, speakers).map(|engine| (engine, Some(why)))
    }

    /// Logs `notice` and has the panel show it (with a tray balloon if hidden), unless it was the last one.
    fn tell(&self, notice: String) {
        log::write(&notice);
        let mut last = self.last_notice.borrow_mut();
        if last.as_ref() != Some(&notice) {
            self.controls.post_notice(notice.clone());
            *last = Some(notice);
        }
    }

    /// Loads the current model again once the GPU files it ran on the CPU without are in.
    fn move_to_gpu(&self) -> Option<Box<dyn Engine>> {
        if !self.gpu_move_due() {
            return None;
        }
        self.reload()
    }

    /// Loads the current model again; it keeps running if that fails.
    fn reload(&self) -> Option<Box<dyn Engine>> {
        let active = self.controls.model().active;
        let (engine, gpu_fallback) = self.load(active, true).ok()?;
        self.controls.finish_switch(active, Ok(gpu_fallback));
        Some(engine)
    }

    fn gpu_move_due(&self) -> bool {
        let active = self.controls.model().active;
        self.awaiting_gpu.get() && Download::Model(active.model, active.variant()).complete()
    }

    /// Why `choice` can't run on the GPU right now, checked before trying, and what to do about it: the
    /// card and driver, the GPU build and pack being there and intact, and free GPU memory.
    fn gpu_unready(&self, choice: Choice) -> Option<(String, &'static str)> {
        if let Some(problem) = self.hardware.gpu_problem() {
            return Some((problem, "Settings → Listening can switch it to the CPU."));
        }
        let pack = catalog::component(GPU_PACK);
        if !choice.variant().installed() {
            return Some(("its GPU build isn't downloaded".into(), "Settings → Listening can download it."));
        }
        if !pack.installed() {
            return Some(("CUDA files missing".into(), "Settings → Listening can download them."));
        }
        if let Err(problem) = pack.verify() {
            log::write(&problem);
            return Some(("CUDA files damaged".into(), "Settings → Listening can download them again."));
        }
        // A GPU model still running gives its memory back once this one has loaded.
        let state = self.controls.model();
        let held = if self.controls.helper_process().is_some() { state.active.variant().vram_mb } else { 0 };
        let free = hardware::free_vram_mb()? + held;
        let needed = choice.variant().vram_mb;
        let gb = |mb: u32| mb as f32 / 1024.0;
        let why = format!("{:.1} GB of GPU memory free, it needs {:.1} GB", gb(free), gb(needed));
        (free < needed).then_some((why, "Close apps using the GPU, then pick GPU again in Settings → Listening."))
    }

    /// Gates a chunk by its input's sensitivity, reports its level, and hands it to its stream, unless
    /// it is silence the stream's speech detector is done with. While dictating the mic isn't gated:
    /// holding the key says you are talking. In dictation only mode the mic is heard only then.
    fn feed<'e>(&self, engine: &'e dyn Engine, sources: &mut Sources, streams: &mut [Option<Stream<'e>>], mut chunk: Chunk) {
        let input = if sources.sections[chunk.source].is_some() { Input::Computer } else { Input::Mic };
        let dictating = self.controls.dictating.load(Ordering::Relaxed);
        let threshold = match input {
            Input::Mic if dictating => THRESHOLD_MIN_DB,
            _ => self.controls.threshold_db(input),
        };
        let gate = &mut sources.gates[chunk.source];
        let level = gate.apply(&mut chunk.samples, threshold);
        self.controls.report_level(input, level);
        let unheard = input == Input::Mic && !dictating && self.controls.dictation_only.load(Ordering::Relaxed);
        if gate.idle() || unheard {
            return;
        }
        sources.recent[chunk.source].push(&chunk.samples);
        let slot = &mut streams[chunk.source];
        let pushed = slot.as_mut().map(|stream| {
            stream.fed = true;
            stream.engine.push(&chunk.samples)
        });
        if let Some(Err(error)) = pushed {
            reopen(engine, sources, slot, chunk.source, &self.controls, &error);
        }
    }

    fn track_source(&self, sources: &mut Sources, event: SourceEvent) {
        match event {
            SourceEvent::Added(labeled) => {
                sources.sections.push(self.add_section(&labeled));
                sources.gates.push(Gate::default());
                sources.labels.push(labeled.label.clone());
                sources.recent.push(Recent::default());
                sources.alive.push(true);
            }
            SourceEvent::Ended(index) => {
                sources.alive[index] = false;
                sources.recent[index] = Recent::default();
            }
        }
    }

    /// Saves what each source still capturing heard lately, with the transcript.
    fn save_miss(&self, sources: &Sources) {
        let alive = sources.labels.iter().zip(&sources.recent).zip(&sources.alive).filter(|(_, alive)| **alive);
        let audio: Vec<(&str, &Recent)> = alive.map(|((label, recent), _)| (label.as_str(), recent)).collect();
        let sections = self.captions.lock().unwrap().sections.clone();
        match misses::save(&audio, &sections) {
            Ok(folder) => log::write(format!("saved a miss to {}", folder.display())),
            Err(error) => log::write(format!("couldn't save a miss: {error}")),
        }
    }

    /// Rewrites the status file the MCP server reads, if what it says changed or its heartbeat is due.
    fn report_status(&self, sources: &Sources) {
        let captions = self.captions.lock().unwrap();
        let mut capturing = vec![false; captions.sections.len()];
        for (section, &alive) in sources.sections.iter().zip(&sources.alive) {
            if let Some(slot) = capturing.get_mut(section.unwrap_or(YOU_SECTION)) {
                *slot |= alive;
            }
        }
        self.status.borrow_mut().update(&captions, &self.controls, &capturing);
    }

    fn add_section(&self, labeled: &Labeled) -> Option<usize> {
        match labeled.source {
            Source::App { .. } => Some(self.captions.lock().unwrap().add_app(&labeled.label, labeled.exe_path.clone())),
            Source::Microphone => None,
        }
    }

    /// Applies every stream's new lines; returns whether any captions changed.
    fn collect_all<'e>(&self, engine: &'e dyn Engine, streams: &mut [Option<Stream<'e>>], sources: &mut Sources, collect: Collect) -> bool {
        let now = Instant::now();
        (0..streams.len()).fold(false, |changed, index| self.collect_stream(engine, streams, sources, index, collect, now) | changed)
    }

    /// Applies stream `index`'s new lines; returns whether any captions changed.
    fn collect_stream<'e>(&self, engine: &'e dyn Engine, streams: &mut [Option<Stream<'e>>], sources: &mut Sources, index: usize, collect: Collect, now: Instant) -> bool {
        let Some(stream) = &mut streams[index] else { return false };
        let mic = sources.sections[index].is_none();
        let lines = match collect {
            Collect::FlushMic if mic => stream.engine.flush(),
            Collect::FlushAll => stream.engine.flush(),
            _ if !stream.fed => return false,
            _ => stream.engine.poll(),
        };
        stream.fed = false;
        let lines = match lines {
            Ok(lines) => lines,
            Err(error) => {
                reopen(engine, sources, &mut streams[index], index, &self.controls, &error);
                return false;
            }
        };
        let Some(stream) = &streams[index] else { return false };
        let section = sources.sections[index];
        let lines = self.controls.keep_voices(mic, lines);
        if lines.is_empty() {
            return false;
        }
        // Rules apply to each revision of a line in progress again; dictation types the words as heard.
        let shown = self.as_captions(&lines, &sources.labels[index]);
        let in_front = section.is_none().then(overlay::focused_app).flatten();
        let mut captions = self.captions.lock().unwrap();
        match section {
            Some(section) => captions.update_app(section, stream.id, &shown, now),
            None => captions.update_mic(stream.id, &shown, in_front.as_deref()),
        }
        self.recorder.borrow_mut().note(&self.controls, &captions, stream.id, section, &shown);
        if mic {
            let dictated = matches!(collect, Collect::Dictating | Collect::FlushMic);
            lines.iter().filter(|l| l.complete && !l.text.is_empty()).for_each(|l| self.controls.queue_mic_line(l.text.clone(), dictated));
        }
        true
    }

    /// Lines as captions show them, after the word rules for the app labelled `label` ("You" for the mic).
    fn as_captions(&self, lines: &[Line], label: &str) -> Vec<Line> {
        lines.iter().map(|line| Line { text: self.controls.words.apply(&line.text, label, Output::Captions), ..line.clone() }).collect()
    }
}

/// One stream failing must not stop the others: log it and give that source a fresh stream.
fn reopen<'e>(engine: &'e dyn Engine, sources: &mut Sources, slot: &mut Option<Stream<'e>>, index: usize, controls: &Controls, error: &dyn std::fmt::Display) {
    log::write(format!("source {index}: stream failed ({error}); reopening"));
    *slot = sources.open(engine, index, controls);
}

fn host_request(choice: Choice, controls: &Controls) -> ModelRequest {
    ModelRequest { model: choice.model.id.clone(), device: choice.device, voices: controls.voices_wanted(), hotwords: choice.hotwords(controls.hotwords()) }
}

/// Loads on the host and logs the outcome, naming the address and never the secret.
fn load_on_host_logged(code: &HostCode, request: &ModelRequest) -> Result<Box<dyn Engine>> {
    let start = Instant::now();
    let loaded = remote::connect(code, request);
    match &loaded {
        Ok(_) => log::write(format!("loaded {} on the host {} in {:.1} s", request.model, code.address(), start.elapsed().as_secs_f32())),
        Err(error) => log::write(format!("{} on the host {} failed to load: {error}", request.model, code.address())),
    }
    loaded
}

fn load_logged(choice: Choice, controls: &Controls, voices: Option<PathBuf>) -> Result<Box<dyn Engine>> {
    let start = Instant::now();
    if controls.voices_wanted() && voices.is_none() {
        log::write("speakers are on, but the speaker model isn't downloaded or is damaged, so lines have no speakers until it is");
    }
    let loaded = engine::load(&ModelSpec { voices, hotwords: choice.hotwords(controls.hotwords()), ..choice.spec() });
    match &loaded {
        Ok(_) => log::write(format!("loaded {} on {:?} in {:.1} s", choice.model.name, choice.device(), start.elapsed().as_secs_f32())),
        Err(error) => log::write(format!("{} on {:?} failed to load: {error}", choice.model.name, choice.device())),
    }
    loaded
}
