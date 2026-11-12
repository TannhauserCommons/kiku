//! Models run in a small helper process, `kiku-engine`, so a crash in a model's runtime (ONNX
//! Runtime, the GPU stack) ends only the helper, and the app loads the model again. NVIDIA's
//! libraries also keep about a gigabyte of memory until the process that loaded them ends, so ending
//! the helper is the only way unloading a GPU model gives all of it back.
//!
//! The app drives the helper over its stdin and stdout in small binary frames: a kind byte, a
//! little-endian `u32` length, then the payload. Audio goes one way without replies; polls get lines back.

use std::{
    collections::HashMap,
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use crate::engine::{load_here, Backend, Device, Engine, EngineStream, Hotword, Line, ModelSpec, Result, SAMPLE_RATE};

const HELPER: &str = "kiku-engine";
/// Loading a model and hearing the test clip takes seconds; a GPU stack that hangs gets this long.
const LOAD_TIMEOUT: Duration = Duration::from_secs(90);
/// How long trying an imported model may take: loading it and hearing the clip on the CPU.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);
/// Four seconds of a public domain LibriVox reading of A Tale of Two Cities, 16 kHz mono 16-bit.
const PROBE_CLIP: &[u8] = include_bytes!("../assets/probe.wav");
/// Windows' exit code for a process whose DLLs couldn't be found.
const DLL_NOT_FOUND: u32 = 0xC000_0135;

/// The start of the load request's line naming the speaker model.
const VOICES_LINE: &str = "voices ";

// Requests.
pub(crate) const LOAD: u8 = 1;
pub(crate) const OPEN: u8 = 2;
pub(crate) const PUSH: u8 = 3;
pub(crate) const POLL: u8 = 4;
pub(crate) const FLUSH: u8 = 5;
pub(crate) const CLOSE: u8 = 6;
pub(crate) const HOTWORDS: u8 = 7;
// Replies.
pub(crate) const OK: u8 = 0;
pub(crate) const LINES: u8 = 1;
pub(crate) const ERROR: u8 = 2;

/// An engine running in the helper process; dropping it ends the process.
pub struct HostedEngine {
    child: Mutex<Child>,
    pipes: Mutex<Pipes>,
    next_stream: AtomicU64,
}

struct Pipes {
    to_helper: BufWriter<ChildStdin>,
    from_helper: BufReader<ChildStdout>,
}

impl HostedEngine {
    /// Starts the helper and loads `spec` in it. The helper tries the model on a short clip before
    /// answering, so a GPU that can't run it fails here rather than mid-sentence.
    pub fn start(spec: &ModelSpec) -> Result<Self> {
        // Lazy is CUDA 12's default; set so a system-wide EAGER can't load every kernel up front (4.6 GB instead of 1 GB of RAM).
        let mut child = Command::new(helper_path()?).env("CUDA_MODULE_LOADING", "LAZY").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
        let mut to_helper = BufWriter::new(child.stdin.take().ok_or("helper has no stdin")?);
        let mut from_helper = BufReader::new(child.stdout.take().ok_or("helper has no stdout")?);
        write_frame(&mut to_helper, LOAD, &encode_spec(spec))?;
        // Read the reply on a thread, so a hung load can be timed out.
        let (reply_tx, reply_rx) = mpsc::channel();
        thread::spawn(move || {
            let reply = read_frame(&mut from_helper);
            let _ = reply_tx.send((reply, from_helper));
        });
        let failed = |child: &mut Child, why: String| -> Result<Self> {
            let _ = child.kill();
            Err(match child.wait() {
                Ok(status) if exit_code(status) == Some(DLL_NOT_FOUND) => "a GPU library is missing next to the app".into(),
                _ => why.into(),
            })
        };
        let from_helper = match reply_rx.recv_timeout(LOAD_TIMEOUT) {
            Ok((Ok((ERROR, message)), _)) => return failed(&mut child, String::from_utf8_lossy(&message).into_owned()),
            Ok((Ok(_), from_helper)) => from_helper,
            Ok((Err(_), _)) | Err(mpsc::RecvTimeoutError::Disconnected) => return failed(&mut child, "the model's helper stopped while loading".into()),
            Err(mpsc::RecvTimeoutError::Timeout) => return failed(&mut child, format!("loading took over {} s", LOAD_TIMEOUT.as_secs())),
        };
        let pipes = Pipes { to_helper, from_helper };
        Ok(Self { child: Mutex::new(child), pipes: Mutex::new(pipes), next_stream: AtomicU64::new(0) })
    }

    /// Sends a request and waits for its reply's payload; an error reply becomes an `Err`.
    fn request(&self, kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let mut pipes = self.pipes.lock().unwrap();
        write_frame(&mut pipes.to_helper, kind, payload)?;
        let (reply, payload) = read_frame(&mut pipes.from_helper).map_err(|_| "the model's helper stopped unexpectedly")?;
        match reply {
            ERROR => Err(String::from_utf8_lossy(&payload).into_owned().into()),
            _ => Ok(payload),
        }
    }

    /// Sends a request that gets no reply.
    fn send(&self, kind: u8, payload: &[u8]) -> Result<()> {
        write_frame(&mut self.pipes.lock().unwrap().to_helper, kind, payload).map_err(|_| "the model's helper stopped unexpectedly".into())
    }
}

impl Engine for HostedEngine {
    fn open_stream_with(&self, voices: bool) -> Result<Box<dyn EngineStream + '_>> {
        let id = self.next_stream.fetch_add(1, Ordering::Relaxed);
        self.request(OPEN, &encode_open(id, voices))?;
        Ok(Box::new(HostedStream { engine: self, id }))
    }

    fn helper_process(&self) -> Option<u32> {
        Some(self.child.lock().unwrap().id())
    }

    fn lost(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(Some(_)))
    }

    fn set_hotwords(&self, words: &[Hotword]) -> bool {
        // A helper that can't answer is lost; loading again starts a new one.
        let lines: Vec<String> = words.iter().map(encode_hotword).collect();
        self.request(HOTWORDS, lines.join("\n").as_bytes()).is_ok_and(|reply| reply == [1])
    }
}

impl Drop for HostedEngine {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

struct HostedStream<'a> {
    engine: &'a HostedEngine,
    id: u64,
}

impl EngineStream for HostedStream<'_> {
    fn push(&mut self, audio: &[f32]) -> Result<()> {
        let mut payload = self.id.to_le_bytes().to_vec();
        payload.extend(audio.iter().flat_map(|sample| sample.to_le_bytes()));
        self.engine.send(PUSH, &payload)
    }

    fn poll(&mut self) -> Result<Vec<Line>> {
        decode_lines(&self.engine.request(POLL, &self.id.to_le_bytes())?)
    }

    fn flush(&mut self) -> Result<Vec<Line>> {
        decode_lines(&self.engine.request(FLUSH, &self.id.to_le_bytes())?)
    }
}

impl Drop for HostedStream<'_> {
    fn drop(&mut self) {
        let _ = self.engine.send(CLOSE, &self.id.to_le_bytes());
    }
}

/// The helper's side: answers requests on stdin until the app closes it.
pub fn serve() {
    load_libraries_only_from_app_folder();
    let (mut input, mut output) = (BufReader::new(io::stdin().lock()), BufWriter::new(io::stdout().lock()));
    // The engine lives as long as the helper, so its streams can borrow it for good.
    let mut engine: Option<&'static dyn Engine> = None;
    let mut streams: HashMap<u64, Box<dyn EngineStream>> = HashMap::new();
    // A push has no reply, so its error waits for the stream's next poll.
    let mut failed: HashMap<u64, String> = HashMap::new();
    while let Ok((kind, payload)) = read_frame(&mut input) {
        let id = payload.get(..8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()));
        let reply = match (kind, engine) {
            (LOAD, _) => Some(decode_spec(&payload).and_then(|spec| load_here(&spec)).and_then(|loaded| {
                // A first run is where a GPU stack missing a library or a kernel for this card fails.
                hear(loaded.as_ref(), &probe_clip())?;
                engine = Some(Box::leak(loaded));
                Ok(Vec::new())
            })),
            (OPEN, Some(engine)) => Some(engine.open_stream_with(opens_with_voices(&payload)).map(|stream| {
                streams.insert(id, stream);
                Vec::new()
            })),
            (PUSH, _) => {
                let audio: Vec<f32> = payload[8..].as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)).collect();
                if let Some(Err(error)) = streams.get_mut(&id).map(|stream| stream.push(&audio)) {
                    failed.insert(id, error.to_string());
                }
                None
            }
            (POLL | FLUSH, _) => Some(match (failed.remove(&id), streams.get_mut(&id)) {
                (Some(error), _) => Err(error.into()),
                (None, Some(stream)) => if kind == POLL { stream.poll() } else { stream.flush() }.map(|lines| encode_lines(&lines)),
                (None, None) => Err("no such stream".into()),
            }),
            (CLOSE, _) => {
                streams.remove(&id);
                None
            }
            (HOTWORDS, Some(engine)) => Some(Ok(vec![engine.set_hotwords(&decode_hotwords(payload_lines(&payload))) as u8])),
            _ => Some(Err("no model loaded".into())),
        };
        let written = match reply {
            Some(Ok(payload)) => write_frame(&mut output, if kind == POLL || kind == FLUSH { LINES } else { OK }, &payload),
            Some(Err(error)) => write_frame(&mut output, ERROR, error.to_string().as_bytes()),
            None => Ok(()),
        };
        if written.is_err() {
            break;
        }
    }
}

/// What `engine` hears in `audio`: its finished lines, joined.
pub fn hear(engine: &dyn Engine, audio: &[f32]) -> Result<String> {
    let mut stream = engine.open_stream()?;
    let mut lines: Vec<Line> = Vec::new();
    let mut take = |new: Vec<Line>| {
        for line in new {
            lines.retain(|l| l.id != line.id);
            lines.push(line);
        }
    };
    for chunk in audio.chunks(SAMPLE_RATE as usize / 10) {
        stream.push(chunk)?;
        take(stream.poll()?);
    }
    take(stream.flush()?);
    Ok(lines.iter().filter(|l| l.complete).map(|l| l.text.as_str()).collect::<Vec<_>>().join(" "))
}

/// The bundled test clip as samples.
pub fn probe_clip() -> Vec<f32> {
    PROBE_CLIP[44..].as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b) as f32 / 32768.0).collect()
}

/// Tries `backend` on the model in `dir` in a helper process, on the CPU, and returns what it heard
/// in the test clip. sherpa-onnx ends the whole process on a model it can't read, hence the helper.
pub fn probe(backend: Backend, dir: &Path, vad: Option<&Path>) -> Result<String> {
    let mut command = Command::new(helper_path()?);
    command.args(["--probe", backend.name()]).arg(dir);
    if let Some(vad) = vad {
        command.arg(vad);
    }
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (heard, error) = (read_on_thread(child.stdout.take()), read_on_thread(child.stderr.take()));
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("took over {} s", PROBE_TIMEOUT.as_secs()).into());
        }
        thread::sleep(Duration::from_millis(50));
    };
    let (heard, error) = (heard.join().unwrap_or_default(), error.join().unwrap_or_default());
    match status.success() {
        true => Ok(heard.trim().to_owned()),
        false if !error.trim().is_empty() => Err(error.trim().to_owned().into()),
        false => Err(format!("the model couldn't be read ({})", status).into()),
    }
}

/// Reads all of `pipe` on a thread of its own, so a child writing more than a pipe holds (ONNX
/// Runtime's warnings, say) never blocks waiting for it to be read.
fn read_on_thread(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// The helper's side of [`probe`]: `--probe <backend> <dir> [vad]`. Prints what it heard.
pub fn serve_probe(args: &[String]) -> std::process::ExitCode {
    let heard = (|| -> Result<String> {
        let backend = args.first().and_then(|name| Backend::from_name(name)).ok_or("unknown backend")?;
        let dir = args.get(1).ok_or("no model folder")?.into();
        let spec = ModelSpec { backend, dir, device: Device::Cpu, vad: args.get(2).map(Into::into), voices: None, hotwords: Vec::new() };
        hear(load_here(&spec)?.as_ref(), &probe_clip())
    })();
    match heard {
        Ok(text) => {
            println!("{text}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Makes the helper find DLLs only next to it and in System32, so a CUDA or cuDNN on PATH or in the
/// working folder can never be loaded in place of the GPU pack.
fn load_libraries_only_from_app_folder() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32};
        unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
    }
}

fn exit_code(status: ExitStatus) -> Option<u32> {
    status.code().map(|code| code as u32)
}

/// The helper sits next to the app, or one folder up for the examples in `target/release/examples`.
pub fn helper_path() -> Result<PathBuf> {
    let name = format!("{HELPER}{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .skip(1)
        .take(2)
        .map(|folder| folder.join(&name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("{name} is missing next to the app").into())
}

pub(crate) fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    writer.write_all(&[kind])?;
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

pub(crate) fn read_frame(reader: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    read_frame_up_to(reader, u32::MAX as usize)
}

/// Like `read_frame`, but a frame longer than `limit` is an error, so a bad length can't allocate gigabytes.
pub(crate) fn read_frame_up_to(reader: &mut impl Read, limit: usize) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    reader.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    if length > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too long"));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok((header[0], payload))
}

/// `backend`, `device`, the speech detector (or nothing), the folder, the hotwords and, last and only with voices
/// on, `voices <speaker model>`, a line each. A reader from before voices skips that line as it skips a hotword
/// it can't read, and without voices the bytes are what they were.
fn encode_spec(spec: &ModelSpec) -> Vec<u8> {
    let device = if spec.device == Device::Cuda { "cuda" } else { "cpu" };
    let vad = spec.vad.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
    let hotwords: String = spec.hotwords.iter().map(|word| format!("\n{}", encode_hotword(word))).collect();
    let voices = spec.voices.as_ref().map(|path| format!("\n{VOICES_LINE}{}", path.display())).unwrap_or_default();
    format!("{}\n{device}\n{vad}\n{}{hotwords}{voices}", spec.backend.name(), spec.dir.display()).into_bytes()
}

fn decode_spec(payload: &[u8]) -> Result<ModelSpec> {
    let mut parts = payload_lines(payload).into_iter();
    let backend = parts.next().as_deref().and_then(Backend::from_name).ok_or("unknown backend")?;
    let device = if parts.next().as_deref() == Some("cuda") { Device::Cuda } else { Device::Cpu };
    let vad = parts.next().filter(|path| !path.is_empty()).map(Into::into);
    let dir = parts.next().ok_or("no model folder")?.into();
    let rest: Vec<String> = parts.collect();
    let voices = rest.iter().find_map(|line| line.strip_prefix(VOICES_LINE)).map(Into::into);
    Ok(ModelSpec { backend, device, vad, voices, dir, hotwords: decode_hotwords(rest) })
}

/// An OPEN request: the stream's id and, only for a stream that tells voices apart, a byte of 1 after it. A reader
/// from before voices stops after the id, and without voices the bytes are what they were.
pub(crate) fn encode_open(id: u64, voices: bool) -> Vec<u8> {
    let mut payload = id.to_le_bytes().to_vec();
    payload.extend(voices.then_some(1));
    payload
}

/// Whether an OPEN request asks for a stream that tells voices apart.
pub(crate) fn opens_with_voices(payload: &[u8]) -> bool {
    payload.get(8) == Some(&1)
}

/// A hotword as one line: its boost, a space and its text.
pub(crate) fn encode_hotword(word: &Hotword) -> String {
    format!("{} {}", word.boost, word.text)
}

pub(crate) fn decode_hotwords(lines: impl IntoIterator<Item = String>) -> Vec<Hotword> {
    let read = |line: &str| line.split_once(' ').and_then(|(boost, text)| Some(Hotword { text: text.to_owned(), boost: boost.parse().ok()? }));
    lines.into_iter().filter_map(|line| read(&line)).collect()
}

/// A payload's lines; a hotword is one line.
pub(crate) fn payload_lines(payload: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(payload);
    if text.is_empty() { Vec::new() } else { text.split('\n').map(str::to_owned).collect() }
}

/// A count, then each line's id, finished flag, text length and text; and, when any line has a speaker,
/// each line's after them (0 for none, else the voice plus one). A reader that stops after the lines
/// never sees the voices, so a host or a client without them still works.
pub(crate) fn encode_lines(lines: &[Line]) -> Vec<u8> {
    let mut bytes = (lines.len() as u32).to_le_bytes().to_vec();
    for line in lines {
        bytes.extend(line.id.to_le_bytes());
        bytes.push(line.complete as u8);
        bytes.extend((line.text.len() as u32).to_le_bytes());
        bytes.extend(line.text.as_bytes());
    }
    if lines.iter().any(|line| line.voice.is_some()) {
        bytes.extend(lines.iter().flat_map(|line| line.voice.map_or(0, |voice| voice + 1).to_le_bytes()));
    }
    bytes
}

pub(crate) fn decode_lines(bytes: &[u8]) -> Result<Vec<Line>> {
    fn take<'a>(reader: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        let (head, rest) = reader.split_at_checked(n).ok_or("garbled reply from the model's helper")?;
        *reader = rest;
        Ok(head)
    }
    let mut reader = bytes;
    let count = u32::from_le_bytes(take(&mut reader, 4)?.try_into()?);
    let mut lines = Vec::new();
    for _ in 0..count {
        let id = u64::from_le_bytes(take(&mut reader, 8)?.try_into()?);
        let complete = take(&mut reader, 1)?[0] != 0;
        let length = u32::from_le_bytes(take(&mut reader, 4)?.try_into()?) as usize;
        let text = String::from_utf8_lossy(take(&mut reader, length)?).into_owned();
        lines.push(Line { id, text, complete, voice: None });
    }
    if !reader.is_empty() {
        for line in &mut lines {
            line.voice = u32::from_le_bytes(take(&mut reader, 4)?.try_into()?).checked_sub(1);
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_specs_survive_the_pipe() {
        let lines = vec![Line { id: 7, text: "Bonjour, ça va ?".into(), complete: true, voice: None }, Line { id: 8, text: String::new(), complete: false, voice: None }];
        assert_eq!(decode_lines(&encode_lines(&lines)).unwrap(), lines);
        let said = vec![Line { voice: Some(0), ..lines[0].clone() }, Line { voice: None, ..lines[1].clone() }, Line { voice: Some(3), ..lines[0].clone() }];
        assert_eq!(decode_lines(&encode_lines(&said)).unwrap(), said);
        let unsaid: Vec<Line> = said.iter().map(|line| Line { voice: None, ..line.clone() }).collect();
        assert_eq!(encode_lines(&said).len(), encode_lines(&unsaid).len() + 4 * said.len(), "a voice per line after the lines, and none without speakers");
        let mut spec = ModelSpec { backend: Backend::Phrases(crate::engine::PhraseModel::NemoTransducer), dir: r"C:\models\sherpa\x-fp16".into(), device: Device::Cuda, vad: Some(r"C:\vad.onnx".into()), voices: None, hotwords: Vec::new() };
        let word = |text: &str, boost| Hotword { text: text.into(), boost };
        for hotwords in [vec![], vec![word("Kiku", 1.5), word("Tannhauser Commons", 2.25)]] {
            spec.hotwords = hotwords;
            spec.voices = spec.voices.is_none().then(|| r"C:\speakers.onnx".into());
            let read = decode_spec(&encode_spec(&spec)).unwrap();
            assert_eq!((read.backend, read.device, &read.dir, &read.vad, &read.voices, &read.hotwords), (spec.backend, spec.device, &spec.dir, &spec.vad, &spec.voices, &spec.hotwords));
        }
        assert_eq!(encode_open(5, false), 5u64.to_le_bytes(), "a stream without voices opens as it always did");
        assert!(opens_with_voices(&encode_open(5, true)) && !opens_with_voices(&encode_open(5, false)));
        let mut framed = Vec::new();
        write_frame(&mut framed, POLL, b"abc").unwrap();
        assert_eq!(read_frame(&mut framed.as_slice()).unwrap(), (POLL, b"abc".to_vec()));
    }

    /// With voices on, lines come through the helper's pipe with their speaker (the test clip has one voice), and
    /// a stream opened without voices on the same engine gives the same words and no speaker.
    /// Needs Parakeet Unified and the speaker model installed (`KIKU_MODELS_DIR` can point at them) and the
    /// helper built (`cargo build -p kiku-core --bin kiku-engine`).
    #[test]
    #[ignore]
    fn voices_cross_the_pipe() {
        let model = crate::catalog::by_id_or_default(crate::catalog::DEFAULT_ID);
        let variant = model.variants.iter().find(|variant| variant.installed()).expect("Parakeet Unified is installed");
        let voices = crate::catalog::speaker_model().expect("the speaker model is installed");
        let engine = crate::engine::load(&ModelSpec { voices: Some(voices), ..model.spec(variant) }).unwrap();
        let said = |voices| {
            let mut stream = engine.open_stream_with(voices).unwrap();
            for chunk in probe_clip().chunks(SAMPLE_RATE as usize / 10) {
                stream.push(chunk).unwrap();
            }
            stream.flush().unwrap()
        };
        let (with, without) = (said(true), said(false));
        assert!(!with.is_empty() && with.iter().all(|line| line.voice == Some(0)), "{with:?}");
        assert!(without.iter().all(|line| line.voice.is_none()), "{without:?}");
        let words = |lines: &[Line]| lines.iter().map(|line| line.text.clone()).collect::<Vec<_>>().join(" ");
        assert_eq!(words(&with), words(&without));
    }

    /// A word's strength changed on a loaded model, as Settings' sliders do, applies from the next phrase:
    /// made very strong, the word is heard where it wasn't said, and set back, it isn't. Needs Parakeet
    /// Unified installed (`KIKU_MODELS_DIR` can point at the folder) and the helper built
    /// (`cargo build -p kiku-core --bin kiku-engine`).
    #[test]
    #[ignore]
    fn a_hotwords_strength_changes_what_is_heard() {
        let model = crate::catalog::by_id_or_default(crate::catalog::DEFAULT_ID);
        let variant = model.variants.iter().find(|variant| variant.installed()).expect("Parakeet Unified is installed");
        let lull = |boost| vec![Hotword { text: "Lull".into(), boost }];
        let engine = crate::engine::load(&ModelSpec { hotwords: lull(1.5), ..model.spec(variant) }).unwrap();
        let heard = || hear(engine.as_ref(), &probe_clip()).unwrap();
        let usual = heard();
        assert!(!usual.contains("Lull"), "{usual}");
        assert!(engine.set_hotwords(&lull(8.0)), "a new strength needs no reload");
        let strong = heard();
        assert!(strong.contains("Lull"), "{strong}");
        assert!(engine.set_hotwords(&lull(1.5)));
        assert_eq!(heard(), usual);
    }
}
