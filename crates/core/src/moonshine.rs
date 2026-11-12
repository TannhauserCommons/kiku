//! Moonshine streaming backend, over its C API (`core/moonshine-c-api.h`).

use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr, CString},
};

use crate::engine::{Backend, Engine, EngineStream, Line, ModelSpec, Result, SAMPLE_RATE};

const HEADER_VERSION: i32 = 30000;
const ARCH_TINY_STREAMING: u32 = 2;
const ARCH_SMALL_STREAMING: u32 = 4;
const ARCH_MEDIUM_STREAMING: u32 = 5;
/// Moonshine rebuilds a stream's whole line history on every poll, so streams are
/// restarted (which clears it) once this many lines have all completed.
const RECYCLE_AFTER_LINES: usize = 32;
/// Audio kept to replay a line still being said into a restarted stream: more than a line holds
/// when it is first seen (its 0.5 s lead-in, and up to 0.5 s waiting to be analyzed).
const REPLAY_SAMPLES: usize = 3 * SAMPLE_RATE as usize;
/// `moonshine_transcribe_stream` flag: analyze all audio added, however little.
const FORCE_UPDATE: u32 = 1;

/// Runs one Moonshine model; all streams share its weights.
pub struct MoonshineEngine {
    handle: i32,
}

impl MoonshineEngine {
    pub fn load(spec: &ModelSpec) -> Result<Self> {
        let arch = arch_of(spec.backend);
        force_single_inference_thread();

        let dir = CString::new(spec.dir.to_string_lossy().as_bytes())?;
        let options = Options::new(&[("return_audio_data", "false")]);
        let handle = check(unsafe {
            moonshine_load_transcriber_from_files(dir.as_ptr(), arch, options.as_ptr(), options.len(), HEADER_VERSION)
        })?;
        Ok(Self { handle })
    }
}

impl Engine for MoonshineEngine {
    fn open_stream_with(&self, _voices: bool) -> Result<Box<dyn EngineStream + '_>> {
        let stream = check(unsafe { moonshine_create_stream(self.handle, 0) })?;
        check(unsafe { moonshine_start_stream(self.handle, stream) })?;
        Ok(Box::new(MoonshineStream { engine: self, handle: stream, recent: VecDeque::new(), pushed: 0, carried: None }))
    }
}

impl Drop for MoonshineEngine {
    fn drop(&mut self) {
        unsafe { moonshine_free_transcriber(self.handle) }
    }
}

struct MoonshineStream<'a> {
    engine: &'a MoonshineEngine,
    handle: i32,
    /// The last audio pushed, and how much was pushed since the stream started, for replaying.
    recent: VecDeque<f32>,
    pushed: usize,
    carried: Option<Carried>,
}

/// A line a restart cut short, carried on by the fresh stream: the id it is shown under, and the id
/// Moonshine gives it again, once seen.
struct Carried {
    shown: u64,
    renamed: Option<u64>,
}

impl EngineStream for MoonshineStream<'_> {
    fn push(&mut self, audio: &[f32]) -> Result<()> {
        check(unsafe {
            moonshine_transcribe_add_audio_to_stream(
                self.engine.handle,
                self.handle,
                audio.as_ptr(),
                audio.len() as u64,
                SAMPLE_RATE as i32,
                0,
            )
        })?;
        self.recent.extend(audio);
        self.recent.drain(..self.recent.len().saturating_sub(REPLAY_SAMPLES));
        self.pushed += audio.len();
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<Line>> {
        let (mut changed, history) = self.transcribe(0)?;
        if history.len >= RECYCLE_AFTER_LINES {
            match history.in_progress {
                None => changed.extend(self.recycle()?),
                // Continuous audio may never pause between lines, so past twice the limit the stream
                // restarts mid-line, once the line is young enough to replay.
                Some(line) if history.len >= 2 * RECYCLE_AFTER_LINES && self.audio_since(line.start).is_some() => {
                    changed.extend(self.restart_mid_line()?)
                }
                Some(_) => {}
            }
        }
        Ok(changed)
    }

    fn flush(&mut self) -> Result<Vec<Line>> {
        let (mut changed, _) = self.transcribe(0)?;
        changed.extend(self.recycle()?);
        Ok(changed)
    }
}

/// How many lines a stream holds, and the first not yet complete.
struct History {
    len: usize,
    in_progress: Option<InProgress>,
}

/// A line still being said: its id, and the sample of the stream it starts at.
struct InProgress {
    id: u64,
    start: usize,
}

impl MoonshineStream<'_> {
    /// Returns the lines that changed since the last call, and the stream's history.
    fn transcribe(&mut self, flags: u32) -> Result<(Vec<Line>, History)> {
        let mut transcript = std::ptr::null_mut();
        check(unsafe { moonshine_transcribe_stream(self.engine.handle, self.handle, flags, &mut transcript) })?;
        if transcript.is_null() {
            return Ok((Vec::new(), History { len: 0, in_progress: None }));
        }
        // The transcript is owned by Moonshine and valid until the next call, so copy it out now.
        let transcript = unsafe { &*transcript };
        let lines = unsafe { std::slice::from_raw_parts(transcript.lines, transcript.line_count as usize) };
        if let (Some(carried), Some(first)) = (&mut self.carried, lines.first()) {
            carried.renamed.get_or_insert(first.id);
        }
        let in_progress = lines.iter().find(|line| line.is_complete == 0).map(|line| InProgress {
            id: self.shown_id(line.id),
            start: (line.start_time * SAMPLE_RATE as f32).round() as usize,
        });
        let changed = lines.iter().filter(|line| line.is_updated != 0).map(|line| Line { id: self.shown_id(line.id), ..to_line(line) });
        Ok((changed.collect(), History { len: lines.len(), in_progress }))
    }

    /// Stops the stream, collects lines from audio not yet analyzed, and starts it afresh.
    /// Line ids keep counting up across restarts, so captions stay distinct.
    fn recycle(&mut self) -> Result<Vec<Line>> {
        check(unsafe { moonshine_stop_stream(self.engine.handle, self.handle) })?;
        let (drained, _) = self.transcribe(0)?;
        self.start()?;
        Ok(drained)
    }

    /// Restarts the stream without cutting the line in progress: its audio is fed again to the fresh
    /// stream, whose first line carries on under the line's id. Returns the lines that changed.
    fn restart_mid_line(&mut self) -> Result<Vec<Line>> {
        // Analyze everything pushed first, so the line in progress is the only one stopping could cut.
        let (changed, history) = self.transcribe(FORCE_UPDATE)?;
        let replay = match &history.in_progress {
            Some(line) => match self.audio_since(line.start) {
                Some(audio) => audio,
                None => return Ok(changed),
            },
            None => Vec::new(),
        };
        check(unsafe { moonshine_stop_stream(self.engine.handle, self.handle) })?;
        // Stopping only finishes the line in progress early; the replay redoes it.
        self.transcribe(0)?;
        self.start()?;
        self.carried = history.in_progress.map(|line| Carried { shown: line.id, renamed: None });
        self.push(&replay)?;
        Ok(changed)
    }

    /// Starts the stopped stream afresh; Moonshine counts its samples from here.
    fn start(&mut self) -> Result<()> {
        check(unsafe { moonshine_start_stream(self.engine.handle, self.handle) })?;
        self.recent.clear();
        self.pushed = 0;
        self.carried = None;
        Ok(())
    }

    /// The audio pushed since sample `start` of the stream, if it is still kept.
    fn audio_since(&self, start: usize) -> Option<Vec<f32>> {
        let from = self.recent.len().checked_sub(self.pushed.checked_sub(start)?)?;
        Some(self.recent.range(from..).copied().collect())
    }

    /// The id a line is shown under: Moonshine's, except for a line carried across a restart.
    fn shown_id(&self, id: u64) -> u64 {
        match &self.carried {
            Some(Carried { shown, renamed: Some(renamed) }) if *renamed == id => *shown,
            _ => id,
        }
    }
}

impl Drop for MoonshineStream<'_> {
    fn drop(&mut self) {
        unsafe {
            moonshine_stop_stream(self.engine.handle, self.handle);
            moonshine_free_stream(self.engine.handle, self.handle);
        }
    }
}

fn arch_of(backend: Backend) -> u32 {
    match backend {
        Backend::MoonshineTinyStreaming => ARCH_TINY_STREAMING,
        Backend::MoonshineSmallStreaming => ARCH_SMALL_STREAMING,
        Backend::MoonshineMediumStreaming => ARCH_MEDIUM_STREAMING,
        Backend::Phrases(_) => unreachable!("not a Moonshine streaming model"),
    }
}

fn to_line(raw: &RawLine) -> Line {
    let text = if raw.text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(raw.text) }.to_string_lossy().trim().to_owned()
    };
    Line { id: raw.id, text, complete: raw.is_complete != 0, voice: None }
}

/// Moonshine otherwise lets ONNX Runtime start a busy-waiting worker per core.
/// It reads this flag through the C runtime's `getenv`, which `std::env::set_var` updates only off Windows.
fn force_single_inference_thread() {
    #[cfg(windows)]
    unsafe { _putenv_s(c"MOONSHINE_ORT_SINGLE_THREAD".as_ptr(), c"1".as_ptr()) };
    #[cfg(not(windows))]
    unsafe { std::env::set_var("MOONSHINE_ORT_SINGLE_THREAD", "1") };
}

fn check(code: i32) -> Result<i32> {
    if code >= 0 {
        return Ok(code);
    }
    let message = unsafe { CStr::from_ptr(moonshine_error_to_string(code)) };
    Err(format!("moonshine error {code}: {}", message.to_string_lossy()).into())
}

/// Owns the C strings behind a `moonshine_option_t` array.
struct Options {
    _strings: Vec<CString>,
    raw: Vec<RawOption>,
}

impl Options {
    fn new(pairs: &[(&str, &str)]) -> Self {
        let strings: Vec<CString> =
            pairs.iter().flat_map(|(k, v)| [*k, *v]).map(|s| CString::new(s).unwrap()).collect();
        let raw = strings.chunks(2).map(|kv| RawOption { name: kv[0].as_ptr(), value: kv[1].as_ptr() }).collect();
        Self { _strings: strings, raw }
    }

    fn as_ptr(&self) -> *const RawOption {
        self.raw.as_ptr()
    }

    fn len(&self) -> u64 {
        self.raw.len() as u64
    }
}

#[repr(C)]
struct RawOption {
    name: *const c_char,
    value: *const c_char,
}

#[repr(C)]
struct RawLine {
    text: *const c_char,
    audio_data: *const f32,
    audio_data_count: usize,
    start_time: f32,
    duration: f32,
    id: u64,
    is_complete: i8,
    is_updated: i8,
    is_new: i8,
    has_text_changed: i8,
    have_speakers_changed: i8,
    speaker_spans: *const c_void,
    speaker_span_count: u64,
    last_transcription_latency_ms: u32,
    words: *const c_void,
    word_count: u64,
}

#[repr(C)]
struct RawTranscript {
    lines: *mut RawLine,
    line_count: u64,
}

unsafe extern "C" {
    #[cfg(windows)]
    fn _putenv_s(name: *const c_char, value: *const c_char) -> i32;

    fn moonshine_error_to_string(error: i32) -> *const c_char;
    fn moonshine_load_transcriber_from_files(
        path: *const c_char,
        model_arch: u32,
        options: *const RawOption,
        options_count: u64,
        version: i32,
    ) -> i32;
    fn moonshine_free_transcriber(handle: i32);
    fn moonshine_create_stream(handle: i32, flags: u32) -> i32;
    fn moonshine_free_stream(handle: i32, stream: i32) -> i32;
    fn moonshine_start_stream(handle: i32, stream: i32) -> i32;
    fn moonshine_stop_stream(handle: i32, stream: i32) -> i32;
    fn moonshine_transcribe_add_audio_to_stream(
        handle: i32,
        stream: i32,
        data: *const f32,
        len: u64,
        sample_rate: i32,
        flags: u32,
    ) -> i32;
    fn moonshine_transcribe_stream(handle: i32, stream: i32, flags: u32, out: *mut *mut RawTranscript) -> i32;
}
