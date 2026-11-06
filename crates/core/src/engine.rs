//! Backend-neutral speech-to-text interface.
//!
//! The rest of the app talks only to [`Engine`] and [`EngineStream`], so a new
//! model family (e.g. sherpa-onnx, Parakeet) is one new module plus one
//! [`Backend`] variant.

use std::{error::Error, path::PathBuf, time::Duration};

use serde::Deserialize;

use crate::{
    engine_host::HostedEngine,
    moonshine::MoonshineEngine,
    sherpa::PhraseEngine,
};

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Sample rate every engine accepts; capture resamples to this.
pub const SAMPLE_RATE: u32 = 16_000;

/// How often the app asks each stream for new lines. Moonshine analyzes at most every 200 ms of
/// new audio, so polling faster gains nothing.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// A loaded speech-to-text model that can serve several live audio streams.
pub trait Engine: Send + Sync {
    /// Opens an independent stream, e.g. one per captured app.
    fn open_stream(&self) -> Result<Box<dyn EngineStream + '_>>;

    /// The separate process running this engine, if any, so its CPU and GPU use can be counted.
    fn helper_process(&self) -> Option<u32> {
        None
    }

    /// Whether the engine stopped working for good (its helper process ended), so it must be loaded again.
    fn lost(&self) -> bool {
        false
    }

    /// Changes the words to listen for (see [`ModelSpec::hotwords`]) from the next phrase on. Returns false
    /// when that needs the model loaded again with them: turning hotwords on or off changes how it decodes.
    fn set_hotwords(&self, _words: &[Hotword]) -> bool {
        true
    }
}

/// One live audio source being transcribed.
pub trait EngineStream: Send {
    /// Queues mono f32 audio at [`SAMPLE_RATE`]. Cheap; inference happens in [`poll`](Self::poll).
    fn push(&mut self, audio: &[f32]) -> Result<()>;

    /// Runs the model on queued audio and returns the lines that changed since the last poll.
    fn poll(&mut self) -> Result<Vec<Line>>;

    /// Ends whatever is being said now without waiting for a pause, e.g. when dictation stops,
    /// and returns the lines that changed. The stream stays usable.
    fn flush(&mut self) -> Result<Vec<Line>>;
}

/// A phrase of transcript. A line keeps its `id` while its text is revised,
/// and stops changing once `complete` is set.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: u64,
    pub text: String,
    pub complete: bool,
}

/// Which backend a model directory is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Backend {
    MoonshineTinyStreaming,
    MoonshineSmallStreaming,
    MoonshineMediumStreaming,
    /// sherpa-onnx models that transcribe whole phrases, cut by a speech detector (Parakeet, Whisper, ...).
    Phrases(PhraseModel),
}

/// The kinds of sherpa-onnx model that transcribe a whole phrase at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhraseModel {
    /// NeMo's TDT and RNN-T transducers, e.g. Parakeet.
    NemoTransducer,
    /// icefall's transducers, e.g. zipformer.
    Transducer,
    NemoCtc,
    Paraformer,
    SenseVoice,
    Whisper,
    /// Moonshine v1, through sherpa-onnx.
    Moonshine,
}

impl Backend {
    const ALL: [Backend; 10] = [
        Backend::MoonshineTinyStreaming,
        Backend::MoonshineSmallStreaming,
        Backend::MoonshineMediumStreaming,
        Backend::Phrases(PhraseModel::NemoTransducer),
        Backend::Phrases(PhraseModel::Transducer),
        Backend::Phrases(PhraseModel::NemoCtc),
        Backend::Phrases(PhraseModel::Paraformer),
        Backend::Phrases(PhraseModel::SenseVoice),
        Backend::Phrases(PhraseModel::Whisper),
        Backend::Phrases(PhraseModel::Moonshine),
    ];

    /// A stable name, used in the catalog, `model.toml` and to tell the helper what to load.
    pub fn name(self) -> &'static str {
        match self {
            Backend::MoonshineTinyStreaming => "moonshine-tiny-streaming",
            Backend::MoonshineSmallStreaming => "moonshine-small-streaming",
            Backend::MoonshineMediumStreaming => "moonshine-medium-streaming",
            Backend::Phrases(PhraseModel::NemoTransducer) => "nemo-transducer",
            Backend::Phrases(PhraseModel::Transducer) => "transducer",
            Backend::Phrases(PhraseModel::NemoCtc) => "nemo-ctc",
            Backend::Phrases(PhraseModel::Paraformer) => "paraformer",
            Backend::Phrases(PhraseModel::SenseVoice) => "sense-voice",
            Backend::Phrases(PhraseModel::Whisper) => "whisper",
            Backend::Phrases(PhraseModel::Moonshine) => "moonshine-v1",
        }
    }

    /// What the user is told the model is, e.g. after importing it.
    pub fn description(self) -> &'static str {
        match self {
            Backend::MoonshineTinyStreaming => "Moonshine Tiny (streaming)",
            Backend::MoonshineSmallStreaming => "Moonshine Small (streaming)",
            Backend::MoonshineMediumStreaming => "Moonshine Medium (streaming)",
            Backend::Phrases(PhraseModel::NemoTransducer) => "NeMo transducer",
            Backend::Phrases(PhraseModel::Transducer) => "icefall transducer",
            Backend::Phrases(PhraseModel::NemoCtc) => "NeMo CTC",
            Backend::Phrases(PhraseModel::Paraformer) => "Paraformer",
            Backend::Phrases(PhraseModel::SenseVoice) => "SenseVoice",
            Backend::Phrases(PhraseModel::Whisper) => "Whisper",
            Backend::Phrases(PhraseModel::Moonshine) => "Moonshine v1",
        }
    }

    /// Moonshine's own runtime crashes on CUDA, so its streaming models run on the CPU only.
    pub fn cpu_only(self) -> bool {
        matches!(self, Backend::MoonshineTinyStreaming | Backend::MoonshineSmallStreaming | Backend::MoonshineMediumStreaming)
    }

    /// Whether the model transcribes whole phrases cut by a speech detector.
    pub fn needs_vad(self) -> bool {
        matches!(self, Backend::Phrases(_))
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
    }
}

impl TryFrom<String> for Backend {
    type Error = String;

    fn try_from(name: String) -> std::result::Result<Self, String> {
        Self::from_name(&name).ok_or_else(|| format!("unknown backend {name}"))
    }
}

/// Where a model runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Cpu,
    /// An NVIDIA GPU through CUDA.
    Cuda,
}

/// A model on disk, the backend that runs it, and where.
#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub backend: Backend,
    pub dir: PathBuf,
    pub device: Device,
    /// The speech detector, for models that transcribe whole phrases.
    pub vad: Option<PathBuf>,
    /// Words to listen for, e.g. names the model would otherwise mishear. NeMo transducers decode
    /// with beam search when there are any; other models ignore them.
    pub hotwords: Vec<Hotword>,
}

/// A word or phrase to listen for, and how strongly: the score each of its tokens gains while decoding.
/// The more it gains, the likelier the word is heard, also where it wasn't said.
#[derive(Clone, Debug, PartialEq)]
pub struct Hotword {
    pub text: String,
    pub boost: f32,
}

/// Loads the model described by `spec` in a helper process (see [`crate::engine_host`]), so a crash
/// in its runtime ends only the helper, and unloading a GPU model frees all the memory NVIDIA's libraries hold.
///
/// ```no_run
/// use kiku_core::engine::{load, Backend, Device, ModelSpec};
/// let spec = ModelSpec { backend: Backend::MoonshineTinyStreaming, dir: "models/en/tiny-streaming-en".into(), device: Device::Cpu, vad: None, hotwords: Vec::new() };
/// let engine = load(&spec)?;
/// let mut stream = engine.open_stream()?;
/// # Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
/// ```
pub fn load(spec: &ModelSpec) -> Result<Box<dyn Engine>> {
    Ok(Box::new(HostedEngine::start(spec)?))
}

/// Loads the model in this process.
pub fn load_here(spec: &ModelSpec) -> Result<Box<dyn Engine>> {
    match spec.backend {
        Backend::MoonshineTinyStreaming
        | Backend::MoonshineSmallStreaming
        | Backend::MoonshineMediumStreaming => Ok(Box::new(MoonshineEngine::load(spec)?)),
        Backend::Phrases(kind) => Ok(Box::new(PhraseEngine::load(spec, kind)?)),
    }
}
