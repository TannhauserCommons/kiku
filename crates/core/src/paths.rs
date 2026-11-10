//! Where the app keeps its settings, log, transcripts, database and models.

use std::path::PathBuf;

/// The app's folder in the user's data folder: `%APPDATA%\kiku` on Windows,
/// `~/Library/Application Support/kiku` on macOS and `~/.local/share/kiku` on Linux.
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_default().join("kiku")
}

/// What people and agents write for Kiku, beside `words.txt`: `macros.toml`, `variables.toml` and `snippets`.
pub fn custom_dir() -> PathBuf {
    data_dir().join("custom")
}

/// Downloaded models, kept per user and out of roaming profiles: `%LOCALAPPDATA%\kiku\models` on
/// Windows, `~/Library/Application Support/kiku/models` on macOS and `~/.local/share/kiku/models` on Linux.
/// `KIKU_MODELS_DIR` names another, e.g. for trying downloads without touching the real one.
pub fn models_dir() -> PathBuf {
    std::env::var_os("KIKU_MODELS_DIR").map(PathBuf::from).unwrap_or_else(|| dirs::data_local_dir().unwrap_or_default().join("kiku").join("models"))
}

/// Where an imported model's `model.toml` goes: `imported/<id>` in the models folder.
pub fn imports_dir() -> PathBuf {
    models_dir().join("imported")
}

/// The folder holding the running executable and the libraries next to it.
pub fn app_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}
