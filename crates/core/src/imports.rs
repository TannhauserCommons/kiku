//! Models the user brought: a folder of sherpa-onnx or Moonshine files, described by a `model.toml`
//! in `imported/<id>` of the models folder. The files stay where the user keeps them.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    catalog::{self, Model, Variant, VAD},
    engine::{Backend, Device, PhraseModel},
    engine_host, paths,
    sherpa::is_part,
};

/// What `model.toml` holds.
#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct Imported {
    pub name: String,
    /// A [`Backend`] name.
    pub backend: String,
    /// The folder with the model's files.
    pub dir: String,
    /// What the model heard in the test clip when it was imported.
    #[serde(default)]
    pub heard: String,
}

/// Every imported model whose description reads; the others are logged and skipped.
pub fn load_all() -> Vec<Model> {
    let Ok(entries) = fs::read_dir(paths::imports_dir()) else { return Vec::new() };
    entries
        .filter_map(|entry| {
            let folder = entry.ok()?.path();
            let id = folder.file_name()?.to_string_lossy().into_owned();
            let read = fs::read_to_string(folder.join("model.toml")).map_err(|e| e.to_string());
            let imported = read.and_then(|text| toml::from_str::<Imported>(&text).map_err(|e| e.to_string()));
            match imported.and_then(|i| i.into_model(&id)) {
                Ok(model) => Some(model),
                Err(error) => {
                    crate::log::write(format!("skipped imported model {id}: {error}"));
                    None
                }
            }
        })
        .collect()
}

impl Imported {
    pub fn into_model(self, id: &str) -> Result<Model, String> {
        let backend = Backend::from_name(&self.backend).ok_or_else(|| format!("unknown backend {}", self.backend))?;
        let variant = |device| Variant { device, precision: String::new(), dir: self.dir.clone(), ram_mb: 0, vram_mb: 0, files: Vec::new() };
        // Moonshine's own runtime crashes on CUDA; every sherpa-onnx model can run on either.
        let mut variants = vec![variant(Device::Cpu)];
        if !backend.cpu_only() {
            variants.push(variant(Device::Cuda));
        }
        Ok(Model {
            id: id.to_owned(),
            name: self.name,
            family: "imported".into(),
            backend,
            languages: Vec::new(),
            params: String::new(),
            wer_percent: None,
            licence: None,
            requires: if backend.needs_vad() { vec![VAD.into()] } else { Vec::new() },
            hotwords: false,
            variants,
        })
    }

    /// Writes the description for `id`, replacing any earlier one.
    pub fn save(&self, id: &str) -> std::io::Result<()> {
        let folder = paths::imports_dir().join(id);
        fs::create_dir_all(&folder)?;
        crate::transcripts::replace(&folder.join("model.toml"), &toml::to_string(self).map_err(std::io::Error::other)?)
    }
}

/// A model imported from a folder, and what it heard in the test clip.
pub struct Import {
    pub model: &'static Model,
    pub heard: String,
}

/// Works out what the model in `dir` is from its file names, tries each possibility in a helper
/// process, and adds the first that hears the test clip to the catalog with a `model.toml`.
pub fn import(dir: &Path) -> Result<Import, String> {
    let vad = catalog::component(VAD).file_path(0);
    let mut failures = Vec::new();
    for backend in candidates(dir)? {
        match engine_host::probe(backend, dir, backend.needs_vad().then_some(vad.as_path())) {
            Ok(heard) if !heard.is_empty() => {
                let id = free_id(dir);
                let name = dir.file_name().map_or_else(|| id.clone(), |n| n.to_string_lossy().into_owned());
                let imported = Imported { name, backend: backend.name().into(), dir: dir.display().to_string(), heard: heard.clone() };
                imported.save(&id).map_err(|e| format!("couldn't save the model's description: {e}"))?;
                return Ok(Import { model: catalog::add(imported.into_model(&id)?), heard });
            }
            Ok(_) => failures.push(format!("as {}, it heard nothing in the test clip", backend.description())),
            Err(error) => failures.push(format!("as {}: {error}", backend.description())),
        }
    }
    Err(format!("Kiku couldn't run this model. {}", failures.join("; ")))
}

/// What the model in `dir` could be, most likely first, judged by its file names; or which kind of
/// model it is that Kiku can't run.
pub fn candidates(dir: &Path) -> Result<Vec<Backend>, String> {
    let names: Vec<String> = fs::read_dir(dir)
        .map_err(|e| format!("couldn't read the folder: {e}"))?
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().to_lowercase()))
        .collect();
    let has = |part: &str| names.iter().any(|name| is_part(name, part));
    let has_tokens = names.iter().any(|name| name.ends_with("tokens.txt"));
    if names.iter().any(|name| name == "streaming_config.json") && names.iter().any(|name| name == "decoder_kv.ort") {
        return Ok(moonshine_streaming(dir));
    }
    if has_tokens && has("preprocess") && has("uncached_decode") {
        return Ok(vec![Backend::Phrases(PhraseModel::Moonshine)]);
    }
    if has_tokens && has("encoder") && has("decoder") && has("joiner") {
        return Ok([PhraseModel::NemoTransducer, PhraseModel::Transducer].map(Backend::Phrases).to_vec());
    }
    if has_tokens && has("encoder") && has("decoder") {
        return Ok(vec![Backend::Phrases(PhraseModel::Whisper)]);
    }
    if has_tokens && has("model") {
        return Ok([PhraseModel::SenseVoice, PhraseModel::Paraformer, PhraseModel::NemoCtc].map(Backend::Phrases).to_vec());
    }
    Err(unsupported(&names))
}

/// Moonshine's streaming sizes, told apart by the encoder's width in `streaming_config.json`.
fn moonshine_streaming(dir: &Path) -> Vec<Backend> {
    let all = vec![Backend::MoonshineTinyStreaming, Backend::MoonshineSmallStreaming, Backend::MoonshineMediumStreaming];
    let config = fs::read_to_string(dir.join("streaming_config.json")).ok().and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    match config.and_then(|c| c["encoder_dim"].as_u64()) {
        Some(320) => vec![Backend::MoonshineTinyStreaming],
        Some(620) => vec![Backend::MoonshineSmallStreaming],
        Some(768) => vec![Backend::MoonshineMediumStreaming],
        _ => all,
    }
}

/// Why a folder isn't a model Kiku can run, naming what it looks like.
fn unsupported(names: &[String]) -> String {
    let any = |extensions: &[&str]| names.iter().any(|name| extensions.iter().any(|ext| name.ends_with(ext)));
    let what = if any(&[".gguf", ".ggml"]) {
        "a GGUF model (whisper.cpp or llama.cpp)"
    } else if any(&[".nemo"]) {
        "a NeMo checkpoint (.nemo); export it to ONNX with sherpa-onnx's NeMo scripts first"
    } else if any(&[".safetensors", ".pt", ".pth", ".ckpt", ".bin"]) {
        "PyTorch or Hugging Face weights; Kiku needs a sherpa-onnx ONNX export"
    } else if any(&[".mlmodelc", ".mlpackage", ".tflite", ".engine"]) {
        "a Core ML, TensorFlow Lite or TensorRT model"
    } else if any(&[".onnx", ".ort"]) && !names.iter().any(|name| name.ends_with("tokens.txt")) {
        "ONNX files without a tokens.txt"
    } else if any(&[".onnx", ".ort"]) {
        "ONNX files with names Kiku doesn't recognise (it expects sherpa-onnx's: encoder/decoder/joiner, model.onnx, ...)"
    } else {
        "a folder with no model files"
    };
    format!("This is {what}. Kiku runs sherpa-onnx models that transcribe whole phrases (NeMo or icefall transducers, NeMo CTC, Paraformer, SenseVoice, Whisper, Moonshine) and Moonshine streaming models.")
}

/// Forgets an imported model; its files are left alone.
pub fn forget(id: &str) -> std::io::Result<()> {
    let folder = paths::imports_dir().join(id);
    fs::remove_file(folder.join("model.toml"))?;
    fs::remove_dir(&folder)
}

/// An id for a model imported from `dir`: its folder name, made safe for a file name.
pub fn id_for(dir: &Path) -> String {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c.to_ascii_lowercase() } else { '-' }).collect();
    format!("imported-{}", safe.trim_matches('-'))
}

/// [`id_for`] `dir`, numbered when another folder of the same name was imported already, so importing
/// it never replaces that one. Importing the same folder again keeps its id.
fn free_id(dir: &Path) -> String {
    let base = id_for(dir);
    let dir = dir.display().to_string();
    let taken = |id: &str| catalog::by_id(id).is_some_and(|model| model.variants.iter().all(|v| v.dir != dir));
    let (mut id, mut number) = (base.clone(), 1);
    while taken(&id) {
        number += 1;
        id = format!("{base}-{number}");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_recognised_by_file_names() {
        let folder = std::env::temp_dir().join("kiku-import-test").join("sherpa-onnx-zipformer-en");
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let kinds = |files: &[&str]| {
            for entry in fs::read_dir(&folder).unwrap() {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            for file in files {
                fs::write(folder.join(file), "").unwrap();
            }
            candidates(&folder)
        };
        let transducer = ["encoder-epoch-99-avg-1.onnx", "decoder-epoch-99-avg-1.onnx", "joiner-epoch-99-avg-1.onnx", "tokens.txt"];
        assert_eq!(kinds(&transducer).unwrap(), [Backend::Phrases(PhraseModel::NemoTransducer), Backend::Phrases(PhraseModel::Transducer)]);
        assert_eq!(kinds(&["tiny.en-encoder.int8.onnx", "tiny.en-decoder.int8.onnx", "tiny.en-tokens.txt"]).unwrap(), [Backend::Phrases(PhraseModel::Whisper)]);
        assert_eq!(kinds(&["model.int8.onnx", "tokens.txt"]).unwrap().len(), 3);
        assert!(kinds(&["ggml-base.en.bin.gguf"]).unwrap_err().contains("GGUF"));
        assert!(kinds(&["model.onnx"]).unwrap_err().contains("tokens.txt"));
        let _ = fs::remove_dir_all(folder.parent().unwrap());
    }

    #[test]
    fn descriptions_round_trip_and_become_models() {
        let imported = Imported { name: "My Whisper".into(), backend: "nemo-transducer".into(), dir: r"D:\models\x".into(), heard: "hi".into() };
        let text = toml::to_string(&imported).unwrap();
        assert_eq!(toml::from_str::<Imported>(&text).unwrap(), imported);
        let model = imported.into_model("imported-x").unwrap();
        assert_eq!(model.variants.len(), 2);
        assert_eq!(model.requires, [VAD]);
        assert_eq!(id_for(Path::new(r"D:\models\Sherpa ONNX Whisper (tiny)")), "imported-sherpa-onnx-whisper--tiny");
    }
}
