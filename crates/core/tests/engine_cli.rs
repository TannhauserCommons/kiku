//! `kiku-engine`'s commands for a host without the app: `--models` lists the catalog's models and which are
//! installed, `--download <id> [--device cpu|gpu]` fetches one. The download test needs the network and
//! runs with `--include-ignored`.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

/// Runs `kiku-engine` with `args`, its models in `models`.
fn engine(models: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_kiku-engine"))
        .args(args)
        .env("KIKU_MODELS_DIR", models)
        .stdin(Stdio::null())
        .output()
        .expect("kiku-engine runs")
}

/// `--models`' line for `id`.
fn listed(models: &Path, id: &str) -> String {
    let output = engine(models, &["--models"]);
    assert!(output.status.success(), "--models failed: {}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.lines().find(|line| line.split_whitespace().next() == Some(id)).unwrap_or_else(|| panic!("no line for {id} in:\n{text}")).to_owned()
}

/// A fresh, empty models folder.
fn empty_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("kiku-engine-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

#[test]
fn models_command_lists_the_catalog() {
    let empty = empty_folder("list");
    let line = listed(&empty, "parakeet-unified");
    assert!(line.contains("cpu: not installed"), "{line}");
    let unknown = engine(&empty, &["--download", "no-such-model"]);
    assert!(!unknown.status.success(), "downloading an unknown model succeeded");
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("parakeet-unified"), "the error names the models there are");
}

/// Needs Parakeet Unified's CPU build in `KIKU_MODELS_DIR`.
#[test]
#[ignore]
fn models_command_sees_installed_models() {
    let installed = PathBuf::from(std::env::var("KIKU_MODELS_DIR").expect("KIKU_MODELS_DIR names a folder with Parakeet Unified"));
    let line = listed(&installed, "parakeet-unified");
    assert!(line.contains("cpu: installed"), "{line}");
}

#[test]
#[ignore]
fn download_command_fetches_a_model() {
    let folder = empty_folder("download");
    let first = engine(&folder, &["--download", "moonshine-tiny"]);
    assert!(first.status.success(), "--download failed: {}", String::from_utf8_lossy(&first.stderr));
    assert!(listed(&folder, "moonshine-tiny").contains("cpu: installed"));
    let started = Instant::now();
    let again = engine(&folder, &["--download", "moonshine-tiny"]);
    assert!(again.status.success());
    assert!(String::from_utf8_lossy(&again.stdout).contains("already installed"), "{}", String::from_utf8_lossy(&again.stdout));
    assert!(started.elapsed() < Duration::from_secs(5), "a second download took {:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(&folder);
}
