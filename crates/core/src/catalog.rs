//! The models Kiku offers, read from the bundled `catalog.json`, plus models the user imported, and
//! where their files live.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock, RwLock,
    },
    time::{Duration, Instant},
};

use serde::Deserialize;

use crate::{
    engine::{Backend, Device, Hotword, ModelSpec},
    imports, paths, update,
};

const BUNDLED: &str = include_str!("../catalog.json");
/// The model used when none was picked, and recommended: the most accurate.
pub const DEFAULT_ID: &str = "parakeet-unified";
/// The speech detector that models transcribing whole phrases need.
pub const VAD: &str = "silero-vad";
/// The speaker model that tells voices apart, when voices are on.
pub const SPEAKERS: &str = "titanet-small";
/// NVIDIA's CUDA libraries, for running models on the GPU.
pub const GPU_PACK: &str = "gpu-pack";
/// How long where a model's files are is remembered: settings redraw 15 times a second while their
/// meters move, too often to check every file of every model each time.
const LOCATION_TTL: Duration = Duration::from_secs(2);
/// A GPU variant is picked only if the card has this much more memory than the variant's peak,
/// leaving room for the desktop and other apps: 8 GB for Parakeet Unified's 4.76 GB in fp32.
const VRAM_HEADROOM: f32 = 1.6;

/// A model the user can pick.
#[derive(Debug, PartialEq, Deserialize)]
pub struct Model {
    /// Stable name kept in settings.
    pub id: String,
    pub name: String,
    pub family: String,
    pub backend: Backend,
    /// ISO 639-1 codes.
    pub languages: Vec<String>,
    #[serde(default)]
    pub params: String,
    /// Word error rate on 200 LibriSpeech test-clean utterances (`examples/footprint`).
    pub wer_percent: Option<f32>,
    pub licence: Option<Licence>,
    /// Components it needs, e.g. [`VAD`].
    #[serde(default)]
    pub requires: Vec<String>,
    /// Listens for hotwords; only where they were measured to help without costing accuracy.
    #[serde(default)]
    pub hotwords: bool,
    pub variants: Vec<Variant>,
}

/// A build of a model for one device and precision.
#[derive(Debug, PartialEq, Deserialize)]
pub struct Variant {
    pub device: Device,
    pub precision: String,
    /// Folder relative to a models folder, or an absolute path for imported models.
    pub dir: String,
    /// Peak memory while transcribing, from `examples/footprint`.
    pub ram_mb: u32,
    pub vram_mb: u32,
    pub files: Vec<File>,
}

#[derive(Debug, PartialEq, Deserialize)]
pub struct Licence {
    /// SPDX expression, with `LicenseRef-` for licences SPDX doesn't list.
    pub spdx: String,
    pub url: String,
    pub attribution: String,
}

/// A file to download, pinned by size and hash.
#[derive(Debug, PartialEq, Deserialize)]
pub struct File {
    pub path: String,
    /// The file itself, or the zip archive it is in (see `zip`).
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// Where the file's bytes are in the zip at `url`, which is fetched with a Range request for just
    /// those. The GPU pack's DLLs come this way from NVIDIA's and Microsoft's own archives.
    pub zip: Option<ZipEntry>,
}

/// A file inside a zip archive: its compressed bytes, past the entry's local header.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct ZipEntry {
    pub offset: u64,
    pub compressed: u64,
    pub method: ZipMethod,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZipMethod {
    Stored,
    Deflate,
}

impl File {
    /// The bytes of `url` to fetch: where they start and how many.
    pub fn span(&self) -> (u64, u64) {
        self.zip.map_or((0, self.size), |entry| (entry.offset, entry.compressed))
    }

    /// Bytes downloaded for the file.
    pub fn download_size(&self) -> u64 {
        self.span().1
    }
}

/// Files that aren't a model but that models or devices need: the speech detector, the GPU pack.
#[derive(Debug, PartialEq, Deserialize)]
pub struct Component {
    pub id: String,
    pub name: String,
    /// Folder relative to the root it is installed in.
    pub dir: String,
    /// `app` for files that must sit next to the app's own libraries; models' folders otherwise.
    #[serde(default)]
    pub root: Root,
    pub licence: Licence,
    pub files: Vec<File>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Root {
    #[default]
    Models,
    App,
}

#[derive(Deserialize)]
struct Bundled {
    /// Base of the URLs written `{release}/<asset>`: files with no pinned upstream copy, published with
    /// Kiku's releases (`{releases}`, see [`update::releases`]).
    release: String,
    components: Vec<Component>,
    models: Vec<Model>,
}

struct Catalog {
    models: RwLock<Vec<&'static Model>>,
    components: Vec<Component>,
}

/// Total memory of the GPU models would run on, in MB; 0 when unknown.
static GPU_MEMORY_MB: AtomicU32 = AtomicU32::new(0);
/// Components found damaged: the stamp of their files when checked, and what was wrong.
static DAMAGED: Mutex<BTreeMap<String, (String, String)>> = Mutex::new(BTreeMap::new());
/// Where each variant's or component's files were last found, and when.
static LOCATIONS: Mutex<Option<HashMap<String, Found>>> = Mutex::new(None);
type Found = (Instant, Option<PathBuf>);

/// Forgets where files were found, after a download or delete changed them.
pub fn refresh_locations() {
    *LOCATIONS.lock().unwrap() = None;
}

/// `find`'s answer for `key`, reused for [`LOCATION_TTL`].
fn remembered(key: &str, find: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    if let Some((_, found)) = LOCATIONS.lock().unwrap().as_ref().and_then(|known| known.get(key)).filter(|(at, _)| at.elapsed() < LOCATION_TTL) {
        return found.clone();
    }
    let found = find();
    LOCATIONS.lock().unwrap().get_or_insert_default().insert(key.to_owned(), (Instant::now(), found.clone()));
    found
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut bundled = parse(BUNDLED, &update::releases()).expect("the bundled catalog parses");
        let leaked: Vec<&'static Model> = bundled.models.drain(..).map(|m| &*Box::leak(Box::new(m))).collect();
        let catalog = Catalog { models: RwLock::new(leaked), components: bundled.components };
        for model in imports::load_all() {
            add_to(&catalog, model);
        }
        catalog
    })
}

/// The catalog in `json`, with `{releases}` in its URLs standing for `releases`.
fn parse(json: &str, releases: &str) -> serde_json::Result<Bundled> {
    let mut bundled: Bundled = serde_json::from_str(json)?;
    let release = bundled.release.replace("{releases}", releases);
    let files = bundled.models.iter_mut().flat_map(|m| &mut m.variants).flat_map(|v| &mut v.files);
    for file in files.chain(bundled.components.iter_mut().flat_map(|c| &mut c.files)) {
        file.url = file.url.replace("{release}", &release);
    }
    Ok(bundled)
}

/// Every model offered, bundled ones first.
pub fn models() -> Vec<&'static Model> {
    catalog().models.read().unwrap().clone()
}

pub fn by_id(id: &str) -> Option<&'static Model> {
    models().into_iter().find(|m| m.id == id)
}

/// The model with this id, or the default if it is unknown (e.g. an imported model since removed).
pub fn by_id_or_default(id: &str) -> &'static Model {
    by_id(id).or_else(|| by_id(DEFAULT_ID)).expect("the default model is bundled")
}

pub fn component(id: &str) -> &'static Component {
    component_by_id(id).expect("components are bundled")
}

/// The speaker model's file, if it is downloaded and whole.
pub fn speaker_model() -> Option<PathBuf> {
    let speakers = component(SPEAKERS);
    speakers.verify().ok().map(|()| speakers.file_path(0))
}

pub fn component_by_id(id: &str) -> Option<&'static Component> {
    catalog().components.iter().find(|c| c.id == id)
}

/// Adds an imported model, replacing one with the same id. Models live as long as the app, so a
/// replaced one stays allocated: imports are rare and small.
pub fn add(model: Model) -> &'static Model {
    add_to(catalog(), model)
}

fn add_to(catalog: &Catalog, model: Model) -> &'static Model {
    let model: &'static Model = Box::leak(Box::new(model));
    let mut models = catalog.models.write().unwrap();
    models.retain(|m| m.id != model.id);
    models.push(model);
    model
}

pub fn remove(id: &str) {
    catalog().models.write().unwrap().retain(|m| m.id != id);
}

/// Deletes `variant`'s files, or forgets an imported model, whose files are the user's own.
pub fn delete(model: &'static Model, variant: &Variant) -> std::io::Result<()> {
    if model.imported() {
        imports::forget(&model.id)?;
        remove(&model.id);
        return Ok(());
    }
    let Some(location) = variant.location() else { return Ok(()) };
    // Only ever a model's own folder inside a models folder.
    if variant.dir.is_empty() || !model_roots().iter().any(|root| location.starts_with(root) && location != *root) {
        return Err(std::io::Error::other(format!("{} isn't in a models folder", location.display())));
    }
    let removed = fs::remove_dir_all(location);
    refresh_locations();
    removed
}

/// Deletes a component's files, and only those: the GPU pack shares the app's folder.
pub fn delete_component(component: &Component) -> std::io::Result<()> {
    let Some(location) = component.location() else { return Ok(()) };
    let removed = component.files.iter().try_for_each(|file| fs::remove_file(location.join(&file.path)));
    let _ = fs::remove_file(paths::models_dir().join(format!("{}.verified", component.id)));
    refresh_locations();
    removed
}

/// Tells variant picking how much memory the GPU has, once hardware is detected.
pub fn set_gpu_memory_mb(total: u32) {
    GPU_MEMORY_MB.store(total, Ordering::Relaxed);
}

/// Folders models are looked for in, in order: the user's models folder, where downloads go, then
/// `models` next to the app and in the working folder, where earlier installs and development keep them.
pub fn model_roots() -> Vec<PathBuf> {
    let beside_app = paths::app_dir().map(|dir| dir.join("models"));
    [Some(paths::models_dir()), beside_app, Some(PathBuf::from("models"))].into_iter().flatten().collect()
}

impl Model {
    /// The variant to run on `device`, for the GPU this PC has (see [`set_gpu_memory_mb`]).
    pub fn variant(&self, device: Device) -> &Variant {
        self.variant_for(device, GPU_MEMORY_MB.load(Ordering::Relaxed))
    }

    /// The variant to run on `device`: on a GPU with `gpu_memory_mb` (0 if unknown), the most precise
    /// one that fits; the CPU's if the model has no GPU build.
    pub fn variant_for(&self, device: Device, gpu_memory_mb: u32) -> &Variant {
        let gpu: Vec<&Variant> = self.variants.iter().filter(|v| v.device == Device::Cuda).collect();
        let picked = match device {
            Device::Cuda => gpu.iter().find(|v| gpu_memory_mb == 0 || v.fits(gpu_memory_mb)).or(gpu.last()).copied(),
            Device::Cpu => None,
        };
        picked.or_else(|| self.variants.iter().find(|v| v.device == Device::Cpu)).unwrap_or(&self.variants[0])
    }

    pub fn variant_with(&self, device: Device, precision: &str) -> Option<&Variant> {
        self.variants.iter().find(|v| v.device == device && v.precision == precision)
    }

    pub fn has_gpu_variant(&self) -> bool {
        self.variants.iter().any(|v| v.device == Device::Cuda)
    }

    pub fn imported(&self) -> bool {
        self.variants.iter().any(|v| Path::new(&v.dir).is_absolute())
    }

    /// Where to load `variant` from, with its speech detector if it needs one.
    pub fn spec(&self, variant: &Variant) -> ModelSpec {
        let vad = self.requires.iter().any(|r| r == VAD).then(|| component(VAD).file_path(0));
        ModelSpec { backend: self.backend, dir: variant.path(), device: variant.device, vad, voices: None, hotwords: Vec::new() }
    }
}

impl Variant {
    /// Whether a GPU with `gpu_memory_mb` runs this, with room left for the desktop and other apps.
    pub fn fits(&self, gpu_memory_mb: u32) -> bool {
        self.gpu_memory_needed_mb() <= gpu_memory_mb
    }

    /// The GPU memory a card needs to run this, its peak with room to spare.
    pub fn gpu_memory_needed_mb(&self) -> u32 {
        (self.vram_mb as f32 * VRAM_HEADROOM) as u32
    }

    /// The folder holding every file, if any does.
    pub fn location(&self) -> Option<PathBuf> {
        if Path::new(&self.dir).is_absolute() {
            return Some(PathBuf::from(&self.dir)).filter(|dir| dir.is_dir());
        }
        remembered(&self.dir, || locate(&model_roots(), &self.dir, &self.files))
    }

    pub fn installed(&self) -> bool {
        self.location().is_some()
    }

    /// Where the files are, or where a download puts them.
    pub fn path(&self) -> PathBuf {
        self.location().unwrap_or_else(|| paths::models_dir().join(&self.dir))
    }

    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

impl Component {
    /// The root a download puts this in.
    pub fn install_root(&self) -> PathBuf {
        match self.root {
            Root::App => paths::app_dir().unwrap_or_default(),
            Root::Models => paths::models_dir(),
        }
    }

    pub fn location(&self) -> Option<PathBuf> {
        let roots: Vec<PathBuf> = match self.root {
            Root::App => paths::app_dir().into_iter().collect(),
            Root::Models => model_roots(),
        };
        remembered(&self.id, || locate(&roots, &self.dir, &self.files))
    }

    pub fn installed(&self) -> bool {
        self.location().is_some()
    }

    /// Where the component's `index`th file is, or would be once downloaded.
    pub fn file_path(&self, index: usize) -> PathBuf {
        self.location().unwrap_or_else(|| self.install_root().join(&self.dir)).join(&self.files[index].path)
    }

    /// What downloading the component costs.
    pub fn size(&self) -> u64 {
        self.files.iter().map(File::download_size).sum()
    }

    /// Checks every file against its sha256. Files already checked are remembered by size and
    /// modification time, so the hashing (seconds, for the GPU pack) happens once.
    pub fn verify(&self) -> Result<(), String> {
        let dir = self.location().ok_or_else(|| format!("the {} isn't downloaded", self.name))?;
        let marker = paths::models_dir().join(format!("{}.verified", self.id));
        let stamp: String = self
            .files
            .iter()
            .map(|file| {
                let metadata = dir.join(&file.path).metadata().ok();
                let modified = metadata.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                format!("{} {} {}\n", file.path, metadata.map_or(0, |m| m.len()), modified.map_or(0, |t| t.as_secs()))
            })
            .collect();
        if fs::read_to_string(&marker).is_ok_and(|saved| saved == stamp) {
            return Ok(());
        }
        // A damaged file is hashed once, not at every look, until it changes.
        if let Some((_, problem)) = DAMAGED.lock().unwrap().get(&self.id).filter(|(damaged, _)| *damaged == stamp) {
            return Err(problem.clone());
        }
        for file in &self.files {
            // A file that can't be read now (locked while written or scanned) isn't damaged, so it isn't remembered.
            let hash = crate::download::sha256(&dir.join(&file.path)).map_err(|e| format!("couldn't read {}: {e}", file.path))?;
            if hash != file.sha256 {
                let problem = format!("{} in the {} is damaged: download it again", file.path, self.name);
                DAMAGED.lock().unwrap().insert(self.id.clone(), (stamp, problem.clone()));
                return Err(problem);
            }
        }
        let _ = fs::create_dir_all(paths::models_dir()).and_then(|_| fs::write(&marker, stamp));
        Ok(())
    }
}

/// The first of `roots` holding all of `files` under `dir`.
fn locate(roots: &[PathBuf], dir: &str, files: &[File]) -> Option<PathBuf> {
    roots.iter().map(|root| root.join(dir)).find(|dir| all_present(dir, files))
}

/// Whether every file is in `dir` at its full size. Hashes are checked when downloading.
fn all_present(dir: &Path, files: &[File]) -> bool {
    !files.is_empty() && files.iter().all(|f| dir.join(&f.path).metadata().is_ok_and(|m| m.len() == f.size))
}

/// A model and the preferred device: what the user picks in settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Choice {
    pub model: &'static Model,
    /// Kept as picked even for CPU-only models, so switching back to a GPU model uses it again.
    pub device: Device,
}

impl Choice {
    pub fn variant(&self) -> &'static Variant {
        self.model.variant(self.device)
    }

    /// Where the model actually runs: the preferred device if the model supports it, else the CPU.
    pub fn device(&self) -> Device {
        self.variant().device
    }

    pub fn spec(&self) -> ModelSpec {
        self.model.spec(self.variant())
    }

    /// `words`, if the model listens for hotwords; otherwise none.
    pub fn hotwords(&self, words: Vec<Hotword>) -> Vec<Hotword> {
        if self.model.hotwords { words } else { Vec::new() }
    }

    /// The same model on the CPU.
    pub fn on_cpu(&self) -> Choice {
        Choice { device: Device::Cpu, ..*self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moonshine_runs_on_the_cpu_even_when_the_gpu_is_preferred() {
        let moonshine = Choice { model: by_id_or_default("moonshine-medium"), device: Device::Cuda };
        assert_eq!(moonshine.device(), Device::Cpu);
        let parakeet = Choice { model: by_id_or_default("parakeet-tdt-v2"), device: Device::Cuda };
        assert_eq!(parakeet.device(), Device::Cuda);
        assert!(parakeet.variant().dir.ends_with("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-fp16"));
    }

    #[test]
    fn no_model_needs_the_speaker_model_to_count_as_installed() {
        assert!(models().iter().all(|model| !model.requires.iter().any(|id| id == SPEAKERS)));
    }

    #[test]
    fn unknown_ids_fall_back_to_the_default() {
        assert_eq!(by_id_or_default("moonshine-small").name, "Moonshine Small");
        assert_eq!(by_id_or_default("gone").id, "parakeet-unified");
    }

    #[test]
    fn every_file_is_pinned() {
        let bundled = parse(BUNDLED, "https://github.com/example/kiku/releases").unwrap();
        let files = bundled.models.iter().flat_map(|m| &m.variants).flat_map(|v| &v.files);
        for file in files.chain(bundled.components.iter().flat_map(|c| &c.files)) {
            assert!(file.url.starts_with("https://") && !file.url.contains('{'), "{}", file.url);
            assert!(!file.url.contains("huggingface.co") || file.url.contains("/resolve/") && !file.url.contains("/resolve/main/"));
            assert_eq!(file.sha256.len(), 64);
            assert!(file.size > 0);
        }
        for model in &bundled.models {
            assert!(model.variants.iter().any(|v| v.device == Device::Cpu), "{} runs on the CPU", model.id);
            for id in &model.requires {
                assert!(bundled.components.iter().any(|c| &c.id == id), "{id} is bundled");
            }
        }
    }

    #[test]
    fn a_model_is_installed_only_with_every_file_of_the_variant_for_the_device() {
        // Moonshine Tiny's files are small; Unified's GPU build stands for a variant with none here.
        let cpu = by_id_or_default("moonshine-tiny").variant(Device::Cpu);
        let gpu = by_id_or_default("parakeet-unified").variant_for(Device::Cuda, 24 * 1024);
        let root = std::env::temp_dir().join("kiku-catalog-test");
        let _ = fs::remove_dir_all(&root);
        // The CPU build's files, the last one cut short.
        for (index, file) in cpu.files.iter().enumerate() {
            let path = root.join(&cpu.dir).join(&file.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let size = if index + 1 == cpu.files.len() { file.size - 1 } else { file.size };
            fs::File::create(&path).unwrap().set_len(size).unwrap();
        }
        let roots = [root.join("missing"), root.clone()];
        assert_eq!(locate(&roots, &cpu.dir, &cpu.files), None);
        let last = cpu.files.last().unwrap();
        fs::File::create(root.join(&cpu.dir).join(&last.path)).unwrap().set_len(last.size).unwrap();
        assert_eq!(locate(&roots, &cpu.dir, &cpu.files), Some(root.join(&cpu.dir)));
        assert_eq!(locate(&roots, &gpu.dir, &gpu.files), None, "the GPU build is a folder of its own");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn gpu_pack_files_are_zip_entries() {
        let pack = component(GPU_PACK);
        assert_eq!(pack.root, Root::App, "ONNX Runtime loads its CUDA provider from its own folder");
        assert!(pack.files.iter().all(|file| file.zip.is_some_and(|entry| entry.compressed > 0) && file.url.ends_with(".zip")));
        assert!(pack.size() < pack.files.iter().map(|f| f.size).sum::<u64>(), "a download counts compressed bytes");
    }

    #[test]
    fn gpu_variants_are_picked_by_memory() {
        let unified = by_id_or_default("parakeet-unified");
        let precision = |total: u32| unified.variant_for(Device::Cuda, total).precision.as_str();
        assert_eq!(precision(24 * 1024), "fp32");
        assert_eq!(precision(8 * 1024), "fp32");
        assert_eq!(precision(6 * 1024), "fp16");
        assert_eq!(precision(0), "fp32");
    }
}
