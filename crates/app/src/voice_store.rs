//! The saved voices' file, `voices.dat`: encrypted with DPAPI for this Windows user, so it can't be read
//! by another account or on another PC. Core knows the bytes' format; this does the Windows call.

use std::{
    ffi::c_void,
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use kiku_core::{
    engine::Result,
    log, paths, transcripts,
    voices::book::{SavedVoices, VoiceStore},
};
use windows::Win32::{
    Foundation::{LocalFree, HLOCAL},
    Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB},
};

const FILE: &str = "voices.dat";

/// The saved voices in `%APPDATA%\kiku\voices.dat`.
pub fn saved_voices() -> SavedVoices {
    SavedVoices::new(Box::new(DpapiStore::new(paths::data_dir().join(FILE))))
}

struct DpapiStore {
    path: PathBuf,
    /// Why nothing is saved this run: the file couldn't be read, or couldn't be kept aside, so a save would destroy it.
    refused: Mutex<Option<String>>,
}

impl DpapiStore {
    fn new(path: PathBuf) -> Self {
        Self { path, refused: Mutex::new(None) }
    }

    fn refuse(&self) {
        *self.refused.lock().unwrap() = Some("The voices file couldn't be read or kept, so names aren't saved this run.".into());
    }
}

impl VoiceStore for DpapiStore {
    fn load(&self) -> Result<Option<Vec<u8>>> {
        match fs::read(&self.path) {
            Ok(sealed) => Ok(Some(unseal(&sealed)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => {
                // A good file that can't be opened now (sharing, permissions) stays where it is, and nothing overwrites it.
                self.refuse();
                Err(error.into())
            }
        }
    }

    fn save(&self, bytes: &[u8]) -> Result<()> {
        if let Some(why) = self.refusal() {
            return Err(why.into());
        }
        fs::create_dir_all(self.path.parent().unwrap_or(Path::new("")))?;
        Ok(transcripts::replace_bytes(&self.path, &seal(bytes)?)?)
    }

    fn set_aside(&self) {
        if self.refusal().is_some() {
            return;
        }
        let backup = self.path.with_extension("dat.bak");
        match fs::rename(&self.path, &backup) {
            Ok(()) => log::write(format!("the unreadable {FILE} was kept as {}", backup.display())),
            Err(error) => {
                log::write(format!("the unreadable {FILE} couldn't be kept aside: {error}"));
                self.refuse();
            }
        }
    }

    fn refusal(&self) -> Option<String> {
        self.refused.lock().unwrap().clone()
    }
}

fn seal(plain: &[u8]) -> Result<Vec<u8>> {
    transform(plain, |input, output| unsafe { CryptProtectData(input, None, None, None, None, 0, output) })
}

fn unseal(sealed: &[u8]) -> Result<Vec<u8>> {
    transform(sealed, |input, output| unsafe { CryptUnprotectData(input, None, None, None, None, 0, output) })
}

/// Runs one of DPAPI's calls on `bytes` and copies out the result, freeing what Windows allocated.
fn transform(bytes: &[u8], call: impl FnOnce(&CRYPT_INTEGER_BLOB, &mut CRYPT_INTEGER_BLOB) -> windows::core::Result<()>) -> Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(bytes.len())?, pbData: bytes.as_ptr().cast_mut() };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    call(&input, &mut output)?;
    // SAFETY: on success Windows returns `cbData` bytes at `pbData`, which are ours to free.
    let copy = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast::<c_void>()))) };
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use kiku_core::voices::book::{Print, VoiceScope};

    use super::*;

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("kiku-voice-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        folder
    }

    #[test]
    fn voices_round_trip_through_an_encrypted_file() {
        let path = folder("round").join(FILE);
        let print = Print::new(&[3.0, 4.0]).unwrap();
        let id = SavedVoices::new(Box::new(DpapiStore::new(path.clone()))).add("Maya", print, 12.0, VoiceScope::Everywhere, None);
        let raw = fs::read(&path).unwrap();
        assert!(!raw.windows(4).any(|window| window == b"Maya"), "the name is not in the file as text");
        let again = SavedVoices::new(Box::new(DpapiStore::new(path))).snapshot();
        assert_eq!(again.get(id).unwrap().name, "Maya");
    }

    #[test]
    fn an_unreadable_file_is_kept_as_a_backup_before_a_new_one_is_written() {
        let path = folder("backup").join(FILE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let backup = path.with_extension("dat.bak");
        fs::write(&backup, b"older").unwrap();
        fs::write(&path, b"not encrypted at all").unwrap();
        let voices = SavedVoices::new(Box::new(DpapiStore::new(path.clone())));
        assert!(voices.snapshot().voices().is_empty());
        assert_eq!(fs::read(&backup).unwrap(), b"not encrypted at all", "replaces an older backup");
        assert!(!path.exists(), "nothing overwrote it yet");
        voices.add("Maya", Print::new(&[1.0, 0.0]).unwrap(), 5.0, VoiceScope::Everywhere, None);
        assert_eq!(SavedVoices::new(Box::new(DpapiStore::new(path))).snapshot().voices().len(), 1);
    }

    #[test]
    fn a_file_that_cannot_be_kept_aside_is_never_overwritten() {
        let path = folder("stuck").join(FILE);
        fs::create_dir_all(path.with_extension("dat.bak")).unwrap();
        fs::write(&path, b"not encrypted at all").unwrap();
        let voices = SavedVoices::new(Box::new(DpapiStore::new(path.clone())));
        assert!(voices.refusal().is_some());
        assert!(voices.rename(0, "Maya").is_err());
        voices.add("Maya", Print::new(&[1.0, 0.0]).unwrap(), 5.0, VoiceScope::Everywhere, None);
        assert!(voices.snapshot().voices().is_empty(), "nothing is kept that couldn't be saved");
        assert_eq!(fs::read(&path).unwrap(), b"not encrypted at all");
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_left_in_place_and_nothing_is_saved() {
        let path = folder("busy").join(FILE);
        fs::create_dir_all(&path).unwrap();
        let voices = SavedVoices::new(Box::new(DpapiStore::new(path.clone())));
        assert!(voices.refusal().is_some());
        assert!(path.is_dir() && !path.with_extension("dat.bak").exists(), "an io error moves nothing aside");
    }
}
