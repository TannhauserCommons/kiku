//! Downloads models and components from the catalog on one worker thread.
//!
//! Each file goes to `<root>/.staging/<job>/<dir>/<file>.part`, resumes with an HTTP Range request
//! (accepted only as a 206 whose Content-Range matches), and is checked against its sha256. Once
//! every file of a job is in, each folder is renamed into place, so a model is either complete or
//! absent. The queue is saved to `downloads.json` and comes back paused on the next launch.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use flate2::read::DeflateDecoder;
use sha2::{Digest, Sha256};

use crate::{
    catalog::{self, Choice, Component, File, Model, Variant, ZipMethod, GPU_PACK},
    engine::Device,
    log, paths,
};

/// Room left free on a drive after a download, so it doesn't fill up to the last byte.
const SPARE_BYTES: u64 = 200_000_000;
/// How often progress is published and the panel told.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
/// Waits after a network error, then the download fails until the user retries.
const RETRY_WAITS: [u64; 4] = [2, 5, 15, 30];

/// The background downloader. Cloning gives another handle to the same one.
#[derive(Clone)]
pub struct Downloads {
    shared: Arc<Shared>,
}

struct Shared {
    jobs: Mutex<Vec<Job>>,
    wake: Condvar,
    /// Asks the running job to stop at its next block (paused, cancelled or quitting).
    interrupt: AtomicBool,
    /// The job the worker is on; it stays set until the worker has dealt with a pause or cancel.
    running: Mutex<Option<String>>,
    /// Bytes of the running job that are on disk.
    done: AtomicU64,
    on_change: Box<dyn Fn() + Send + Sync>,
}

/// A queued download and how it is going.
#[derive(Clone)]
struct Job {
    key: String,
    parts: Vec<Part>,
    state: State,
    total: u64,
    /// Bytes on disk when the job last stopped, or of the running job when last published.
    done: u64,
    bytes_per_second: f64,
}

/// Files that go in one folder under one root.
#[derive(Clone)]
struct Part {
    root: PathBuf,
    dir: String,
    files: &'static [File],
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Queued,
    Running,
    Paused,
    Failed(String),
}

/// What the panel shows for a download.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub state: State,
    pub done: u64,
    pub total: u64,
    pub bytes_per_second: f64,
}

/// What a download is of: a model's variant, with the components it needs, or a component alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Model(&'static Model, &'static Variant),
    Component(&'static Component),
}

impl Target {
    /// A stable name for the download, kept in `downloads.json`: `<model>/<device>/<precision>` or `<component>`.
    pub fn key(&self) -> String {
        match self {
            Target::Model(model, variant) => format!("{}/{}/{}", model.id, device_name(variant.device), variant.precision),
            Target::Component(component) => component.id.clone(),
        }
    }

    fn from_key(key: &str) -> Option<Target> {
        let mut parts = key.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(id), Some(device), Some(precision)) => {
                let model = catalog::by_id(id)?;
                let device = if device == "cuda" { Device::Cuda } else { Device::Cpu };
                Some(Target::Model(model, model.variant_with(device, precision)?))
            }
            (Some(id), None, None) => Some(Target::Component(catalog::component_by_id(id)?)),
            _ => None,
        }
    }

    /// Whether nothing is left to fetch: the files and every component they need are there.
    pub fn complete(&self) -> bool {
        self.parts().is_empty() && self.gpu_pack().is_none()
    }

    /// Bytes left to fetch, the GPU pack's included, before counting partly downloaded files.
    pub fn size(&self) -> u64 {
        let pack = self.gpu_pack().map_or(0, |pack| pack.size());
        self.parts().iter().flat_map(|p| p.files).map(File::download_size).sum::<u64>() + pack
    }

    /// The GPU pack, if this is a GPU build and the pack is missing: a download of its own, queued
    /// with the model's so its progress shows by itself. Windows only: a Linux host's CUDA libraries come from the system.
    pub fn gpu_pack(&self) -> Option<Target> {
        if !cfg!(windows) {
            return None;
        }
        let pack = catalog::component(GPU_PACK);
        let gpu_model = matches!(self, Target::Model(_, variant) if variant.device == Device::Cuda);
        (gpu_model && !pack.installed()).then_some(Target::Component(pack))
    }

    /// Everything to fetch: the variant, then missing components it needs, but the GPU pack.
    fn parts(&self) -> Vec<Part> {
        let component_part = |c: &'static Component| Part { root: c.install_root(), dir: c.dir.clone(), files: &c.files };
        match *self {
            Target::Component(component) => vec![component_part(component)],
            Target::Model(model, variant) => {
                let needs = model.requires.iter().map(|id| catalog::component(id));
                let missing = needs.filter(|c| !c.installed()).map(component_part);
                let model_part = Part { root: paths::models_dir(), dir: variant.dir.clone(), files: &variant.files };
                std::iter::once(model_part).filter(|_| !variant.installed()).chain(missing).collect()
            }
        }
    }
}

/// Whether `choice` can load: its files are there, or at least those of its CPU build.
pub fn runnable(choice: Choice) -> bool {
    [choice, choice.on_cpu()].iter().any(|choice| Target::Model(choice.model, choice.variant()).complete())
}

/// What runs when the model picked can't: Parakeet Unified on the CPU, else any model installed.
pub fn fallback() -> Option<Choice> {
    let default = catalog::by_id_or_default(catalog::DEFAULT_ID);
    std::iter::once(default).chain(catalog::models()).map(|model| Choice { model, device: Device::Cpu }).find(|&choice| runnable(choice))
}

impl Downloads {
    /// Starts the worker. The saved queue comes back paused; `on_change` is called (on the worker
    /// thread) whenever progress or a download's state changes.
    pub fn start(on_change: impl Fn() + Send + Sync + 'static) -> Self {
        let jobs = load_queue().into_iter().filter_map(|key| Target::from_key(&key)).map(|t| new_job(t, State::Paused)).collect();
        let shared = Arc::new(Shared {
            jobs: Mutex::new(jobs),
            wake: Condvar::new(),
            interrupt: AtomicBool::new(false),
            running: Mutex::new(None),
            done: AtomicU64::new(0),
            on_change: Box::new(on_change),
        });
        let worker = Arc::clone(&shared);
        thread::Builder::new().name("downloads".into()).spawn(move || work(&worker)).expect("the download thread starts");
        Self { shared }
    }

    /// Queues `target`, or resumes it if it is paused or failed; a GPU build's missing pack comes after it.
    pub fn download(&self, target: Target) {
        let targets = [Some(target).filter(|t| !t.parts().is_empty()), target.gpu_pack()];
        self.update(|jobs| {
            for target in targets.into_iter().flatten() {
                let key = target.key();
                match jobs.iter_mut().find(|j| j.key == key) {
                    Some(job) if matches!(job.state, State::Paused | State::Failed(_)) => job.state = State::Queued,
                    Some(_) => {}
                    None => jobs.push(new_job(target, State::Queued)),
                }
            }
        });
    }

    pub fn pause(&self, target: Target) {
        let key = target.key();
        self.update(|jobs| {
            if let Some(job) = jobs.iter_mut().find(|j| j.key == key && matches!(j.state, State::Queued | State::Running)) {
                job.state = State::Paused;
            }
        });
        self.interrupt_if_running(&key);
    }

    /// Stops and forgets a download, deleting what it fetched so far.
    pub fn cancel(&self, target: Target) {
        let key = target.key();
        let mut removed = None;
        self.update(|jobs| {
            if let Some(index) = jobs.iter().position(|j| j.key == key) {
                removed = Some(jobs.remove(index));
            }
        });
        // The running job's worker cleans up after itself once interrupted.
        if !self.interrupt_if_running(&key) {
            removed.iter().for_each(remove_staging);
        }
    }

    /// Pauses everything, e.g. when quitting; the queue is saved and resumes next time.
    pub fn pause_all(&self) {
        self.update(|jobs| {
            for job in jobs.iter_mut().filter(|j| matches!(j.state, State::Queued | State::Running)) {
                job.state = State::Paused;
            }
        });
        self.shared.interrupt.store(true, Ordering::Relaxed);
    }

    pub fn progress(&self, target: Target) -> Option<Progress> {
        let key = target.key();
        let jobs = self.shared.jobs.lock().unwrap();
        jobs.iter().find(|j| j.key == key).map(|job| self.progress_of(job))
    }

    /// Progress over every download still running or queued, if any is.
    pub fn active(&self) -> Option<Progress> {
        let jobs = self.shared.jobs.lock().unwrap();
        let active: Vec<Progress> = jobs.iter().filter(|j| matches!(j.state, State::Queued | State::Running)).map(|j| self.progress_of(j)).collect();
        (!active.is_empty()).then(|| Progress {
            state: State::Running,
            done: active.iter().map(|p| p.done).sum(),
            total: active.iter().map(|p| p.total).sum(),
            bytes_per_second: active.iter().map(|p| p.bytes_per_second).sum(),
        })
    }

    fn progress_of(&self, job: &Job) -> Progress {
        let done = if job.state == State::Running { self.shared.done.load(Ordering::Relaxed) } else { job.done };
        Progress { state: job.state.clone(), done, total: job.total, bytes_per_second: job.bytes_per_second }
    }

    /// Changes the queue, saves it, wakes the worker and tells the panel.
    fn update(&self, change: impl FnOnce(&mut Vec<Job>)) {
        let mut jobs = self.shared.jobs.lock().unwrap();
        change(&mut jobs);
        save_queue(&jobs);
        drop(jobs);
        self.shared.wake.notify_all();
        (self.shared.on_change)();
    }

    /// Interrupts the job named `key` if it is the one running; returns whether it was.
    fn interrupt_if_running(&self, key: &str) -> bool {
        let running = self.shared.running.lock().unwrap().as_deref() == Some(key);
        if running {
            self.shared.interrupt.store(true, Ordering::Relaxed);
        }
        running
    }
}

fn new_job(target: Target, state: State) -> Job {
    let parts = target.parts();
    let on_disk = parts.iter().flat_map(|part| part.files.iter().map(|file| part_len(&staging_dir(&target.key(), part), file))).sum();
    let total = parts.iter().flat_map(|p| p.files).map(File::download_size).sum();
    Job { key: target.key(), parts, state, total, done: on_disk, bytes_per_second: 0.0 }
}

/// Runs queued jobs one at a time until the app ends.
fn work(shared: &Shared) {
    loop {
        let job = {
            let mut jobs = shared.jobs.lock().unwrap();
            loop {
                if let Some(job) = jobs.iter_mut().find(|j| j.state == State::Queued) {
                    job.state = State::Running;
                    // Under the queue's lock, so a pause or cancel from now on finds it running.
                    *shared.running.lock().unwrap() = Some(job.key.clone());
                    shared.interrupt.store(false, Ordering::Relaxed);
                    break job.clone();
                }
                jobs = shared.wake.wait(jobs).unwrap();
            }
        };
        shared.done.store(job.done, Ordering::Relaxed);
        (shared.on_change)();
        let result = run(shared, &job);
        let mut jobs = shared.jobs.lock().unwrap();
        let still_queued = jobs.iter().position(|j| j.key == job.key);
        match (result, still_queued) {
            (Ok(()), Some(index)) => {
                jobs.remove(index);
                log::write(format!("downloaded {}", job.key));
            }
            (Ok(()), None) => {}
            (Err(Stop::Interrupted), None) => remove_staging(&job),
            (Err(Stop::Interrupted), Some(index)) => {
                jobs[index].done = shared.done.load(Ordering::Relaxed);
                if jobs[index].state == State::Running {
                    jobs[index].state = State::Paused;
                }
            }
            (Err(Stop::Failed(why)), Some(index)) => {
                log::write(format!("download of {} failed: {why}", job.key));
                jobs[index].done = shared.done.load(Ordering::Relaxed);
                jobs[index].state = State::Failed(why);
            }
            (Err(Stop::Failed(_)), None) => remove_staging(&job),
        }
        save_queue(&jobs);
        *shared.running.lock().unwrap() = None;
        drop(jobs);
        (shared.on_change)();
    }
}

enum Stop {
    Interrupted,
    Failed(String),
}

impl From<io::Error> for Stop {
    fn from(error: io::Error) -> Self {
        Stop::Failed(error.to_string())
    }
}

/// Fetches and checks every file of `job`, then moves its folders into place.
fn run(shared: &Shared, job: &Job) -> Result<(), Stop> {
    check_space(job)?;
    let agent = agent();
    let mut meter = Meter::new(&job.key);
    for part in &job.parts {
        let staging = staging_dir(&job.key, part);
        fs::create_dir_all(&staging)?;
        for file in part.files {
            fetch_with_retries(&agent, file, &staging, shared, &mut meter)?;
        }
    }
    for part in &job.parts {
        install(&staging_dir(&job.key, part), &part.root.join(&part.dir))?;
    }
    catalog::refresh_locations();
    remove_staging(job);
    Ok(())
}

/// Fails early if a drive the job writes to lacks room for what is left to fetch.
fn check_space(job: &Job) -> Result<(), Stop> {
    let mut roots: Vec<&Path> = job.parts.iter().map(|p| p.root.as_path()).collect();
    roots.dedup();
    for root in roots {
        let left: u64 = job
            .parts
            .iter()
            .filter(|p| p.root == root)
            .flat_map(|p| p.files.iter().map(move |f| room_for(&staging_dir(&job.key, p), f)))
            .sum();
        fs::create_dir_all(root)?;
        let free = fs4::available_space(root)?;
        if free < left + SPARE_BYTES {
            return Err(Stop::Failed(format!("needs {:.1} GB free on the drive of {}, which has {:.1} GB", (left + SPARE_BYTES) as f64 / 1e9, root.display(), free as f64 / 1e9)));
        }
    }
    Ok(())
}

fn fetch_with_retries(agent: &ureq::Agent, file: &File, staging: &Path, shared: &Shared, meter: &mut Meter) -> Result<(), Stop> {
    let mut waits = RETRY_WAITS.iter();
    loop {
        match fetch(agent, file, staging, shared, meter) {
            Err(Fetch::Network(why)) => match waits.next() {
                Some(&seconds) => {
                    log::write(format!("download of {} interrupted ({why}); retrying in {seconds} s", file.path));
                    sleep_unless_interrupted(shared, Duration::from_secs(seconds))?;
                }
                None => return Err(Stop::Failed(format!("network error: {why}"))),
            },
            Err(Fetch::Stop(stop)) => return Err(stop),
            Ok(()) => return Ok(()),
        }
    }
}

enum Fetch {
    /// Worth retrying.
    Network(String),
    Stop(Stop),
}

impl From<io::Error> for Fetch {
    fn from(error: io::Error) -> Self {
        Fetch::Stop(error.into())
    }
}

/// Downloads one file into `staging`, resuming its `.part`, then unpacks it and checks its hash.
fn fetch(agent: &ureq::Agent, file: &File, staging: &Path, shared: &Shared, meter: &mut Meter) -> Result<(), Fetch> {
    if unpacked(staging, file) {
        return Ok(());
    }
    let done = staging.join(&file.path);
    let part = staging.join(format!("{}.part", file.path));
    let have = fs::metadata(&part).map_or(0, |m| m.len()).min(file.download_size());
    if have < file.download_size() {
        download_rest(agent, file, &part, have, shared, meter)?;
    }
    if !unpack(file, &part, &done)? {
        meter.add(shared, -(file.download_size() as i64));
        return Err(Fetch::Network(format!("{} arrived damaged (wrong sha256)", file.path)));
    }
    Ok(())
}

/// Appends the rest of `file`'s bytes to `part`, which holds the first `have`. A resumed file, and a
/// zip entry always, is asked for with a Range request, and only a 206 for exactly that range is taken.
fn download_rest(agent: &ureq::Agent, file: &File, part: &Path, mut have: u64, shared: &Shared, meter: &mut Meter) -> Result<(), Fetch> {
    let (start, length) = file.span();
    let mut request = agent.get(&file.url);
    if start + have > 0 {
        request = request.header("Range", format!("bytes={}-{}", start + have, start + length - 1));
    }
    let response = request.call().map_err(|e| Fetch::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let resumed = status == 206;
    if resumed {
        let range = response.headers().get("content-range").and_then(|v| v.to_str().ok()).unwrap_or_default();
        let expected = format!("bytes {}-{}/", start + have, start + length - 1);
        let whole_file_total = file.zip.is_none() && !range.ends_with(&format!("/{length}"));
        if !range.starts_with(&expected) || whole_file_total {
            let _ = fs::remove_file(part);
            return Err(Fetch::Network(format!("the server sent the wrong part ({range}); starting the file over")));
        }
    } else if status != 200 || file.zip.is_some() {
        // A 200 for a zip entry would be the whole archive.
        return Err(Fetch::Stop(Stop::Failed(format!("the server answered {status} for {}", file.path))));
    }
    // A 200 is the whole file: start over.
    let mut out = OpenOptions::new().create(true).write(true).append(resumed).truncate(!resumed).open(part)?;
    if !resumed {
        meter.add(shared, -(have as i64));
        have = 0;
    }
    let mut body = response.into_body().into_reader();
    let mut buffer = vec![0u8; 1 << 16];
    while have < length {
        if shared.interrupt.load(Ordering::Relaxed) {
            return Err(Fetch::Stop(Stop::Interrupted));
        }
        let read = body.read(&mut buffer).map_err(|e| Fetch::Network(e.to_string()))?;
        if read == 0 {
            return Err(Fetch::Network("the connection closed early".into()));
        }
        let read = read.min((length - have) as usize);
        out.write_all(&buffer[..read])?;
        have += read as u64;
        meter.add(shared, read as i64);
    }
    out.sync_all()?;
    Ok(())
}

/// Turns the downloaded bytes in `part` into the file at `done` (inflating a zip entry) if its sha256
/// is right; otherwise deletes them and returns false.
fn unpack(file: &File, part: &Path, done: &Path) -> io::Result<bool> {
    let candidate = match file.zip.map(|entry| entry.method) {
        Some(ZipMethod::Deflate) => {
            let inflated = part.with_extension("inflated");
            let mut out = fs::File::create(&inflated)?;
            let copied = io::copy(&mut DeflateDecoder::new(io::BufReader::new(fs::File::open(part)?)), &mut out);
            drop(out);
            match copied {
                Ok(_) => fs::remove_file(part)?,
                // Bytes that don't inflate arrived damaged; any other error (a full disk) keeps them for a retry.
                Err(error) if matches!(error.kind(), io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof) => {
                    fs::remove_file(part)?;
                    fs::remove_file(&inflated)?;
                    return Ok(false);
                }
                Err(error) => {
                    let _ = fs::remove_file(&inflated);
                    return Err(error);
                }
            }
            inflated
        }
        Some(ZipMethod::Stored) | None => part.to_path_buf(),
    };
    if sha256(&candidate)? != file.sha256 {
        fs::remove_file(&candidate)?;
        return Ok(false);
    }
    fs::rename(&candidate, done)?;
    Ok(true)
}

/// Moves a staged folder into place: whole when its destination doesn't exist yet, else file by file.
fn install(staged: &Path, destination: &Path) -> io::Result<()> {
    if !destination.exists() {
        fs::create_dir_all(destination.parent().unwrap_or(destination))?;
        return fs::rename(staged, destination);
    }
    for entry in fs::read_dir(staged)? {
        let entry = entry?;
        fs::rename(entry.path(), destination.join(entry.file_name()))?;
    }
    Ok(())
}

/// Measures speed and publishes progress at most every [`PROGRESS_EVERY`].
struct Meter {
    key: String,
    since: Instant,
    bytes_since: i64,
}

impl Meter {
    fn new(key: &str) -> Self {
        Self { key: key.to_owned(), since: Instant::now(), bytes_since: 0 }
    }

    fn add(&mut self, shared: &Shared, bytes: i64) {
        let done = shared.done.load(Ordering::Relaxed) as i64 + bytes;
        shared.done.store(done.max(0) as u64, Ordering::Relaxed);
        self.bytes_since += bytes.max(0);
        let elapsed = self.since.elapsed();
        if elapsed < PROGRESS_EVERY {
            return;
        }
        let speed = self.bytes_since as f64 / elapsed.as_secs_f64();
        if let Some(job) = shared.jobs.lock().unwrap().iter_mut().find(|j| j.key == self.key) {
            // Smoothed, so the figure doesn't jump with every block.
            job.bytes_per_second = if job.bytes_per_second == 0.0 { speed } else { 0.7 * job.bytes_per_second + 0.3 * speed };
        }
        (self.since, self.bytes_since) = (Instant::now(), 0);
        (shared.on_change)();
    }
}

fn sleep_unless_interrupted(shared: &Shared, duration: Duration) -> Result<(), Stop> {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        if shared.interrupt.load(Ordering::Relaxed) {
            return Err(Stop::Interrupted);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .user_agent(concat!("kiku/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn staging_dir(key: &str, part: &Part) -> PathBuf {
    part.root.join(".staging").join(key.replace('/', "_")).join(&part.dir)
}

fn remove_staging(job: &Job) {
    let mut roots: Vec<&Path> = job.parts.iter().map(|p| p.root.as_path()).collect();
    roots.dedup();
    for root in roots {
        let _ = fs::remove_dir_all(root.join(".staging").join(job.key.replace('/', "_")));
        // Only goes if no other download is staged there.
        let _ = fs::remove_dir(root.join(".staging"));
    }
}

/// Bytes of `file` already downloaded into `staging`.
fn part_len(staging: &Path, file: &File) -> u64 {
    if unpacked(staging, file) {
        return file.download_size();
    }
    fs::metadata(staging.join(format!("{}.part", file.path))).map_or(0, |m| m.len()).min(file.download_size())
}

fn unpacked(staging: &Path, file: &File) -> bool {
    staging.join(&file.path).metadata().is_ok_and(|m| m.len() == file.size)
}

/// Disk room `file` still needs in `staging`: what is left to download, and room to inflate a zip entry.
fn room_for(staging: &Path, file: &File) -> u64 {
    if unpacked(staging, file) {
        return 0;
    }
    let inflating = if file.zip.is_some() { file.size } else { 0 };
    file.download_size() - part_len(staging, file) + inflating
}

pub fn sha256(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn device_name(device: Device) -> &'static str {
    match device {
        Device::Cpu => "cpu",
        Device::Cuda => "cuda",
    }
}

fn queue_path() -> PathBuf {
    paths::models_dir().join("downloads.json")
}

fn load_queue() -> Vec<String> {
    fs::read_to_string(queue_path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn save_queue(jobs: &[Job]) {
    let keys: Vec<&str> = jobs.iter().map(|j| j.key.as_str()).collect();
    let saved = fs::create_dir_all(paths::models_dir()).and_then(|_| crate::transcripts::replace(&queue_path(), &serde_json::to_string(&keys).unwrap_or_default()));
    if let Err(error) = saved {
        log::write(format!("couldn't save the download queue: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{write::DeflateEncoder, Compression};

    use super::*;
    use crate::catalog::ZipEntry;

    #[test]
    fn zip_entries_are_inflated_and_checked() {
        let folder = std::env::temp_dir().join("kiku-unpack-test");
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let text = b"cudnn ".repeat(1000);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&text).unwrap();
        let compressed = encoder.finish().unwrap();
        let digest = format!("{:x}", Sha256::digest(&text));
        let entry = ZipEntry { offset: 100, compressed: compressed.len() as u64, method: ZipMethod::Deflate };
        let file = File { path: "x.dll".into(), url: "https://example.com/a.zip".into(), size: text.len() as u64, sha256: digest, zip: Some(entry) };
        let (part, done) = (folder.join("x.dll.part"), folder.join("x.dll"));
        assert_eq!(file.span(), (100, compressed.len() as u64));

        fs::write(&part, &compressed).unwrap();
        assert!(unpack(&file, &part, &done).unwrap());
        assert_eq!(fs::read(&done).unwrap(), text);
        assert!(!part.exists());
        assert_eq!(part_len(&folder, &file), file.download_size());

        // Damaged bytes are thrown away, so the file starts over.
        fs::remove_file(&done).unwrap();
        fs::write(&part, &compressed[..compressed.len() / 2]).unwrap();
        assert!(!unpack(&file, &part, &done).unwrap());
        assert!(!part.exists() && !done.exists());
        fs::write(&part, [0xFF; 64]).unwrap();
        assert!(!unpack(&file, &part, &done).unwrap(), "bytes that don't inflate are damaged, not an error");
        assert!(!part.exists() && !done.exists());
        let _ = fs::remove_dir_all(&folder);
    }
}
