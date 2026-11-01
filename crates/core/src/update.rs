//! Updates, published as Kiku's releases: `<releases>/latest/download/latest.json` (`latest-arm64.json` for
//! ARM64 builds) names the newest version and its installer, signed with an ed25519 key whose public half is
//! compiled in. A check fetches that one file, at most once a day, and sends nothing else.
//!
//! The releases' address (e.g. `https://github.com/<owner>/kiku/releases`, laid out as GitHub's: `latest/
//! download/<file>` and `download/<tag>/<file>`) and the key come from `KIKU_RELEASES` and `KIKU_UPDATE_KEY`
//! at build time (release builds in CI); a build without them never checks. A manifest's `moved_to` names
//! the address to use from then on, so it can change without stranding copies that haven't updated yet.

use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{download, log, paths};

/// Where releases are published unless a manifest moved them; empty in builds without one.
const RELEASES: &str = match option_env!("KIKU_RELEASES") {
    Some(releases) => releases,
    None => "",
};
/// The ed25519 public key updates are signed with, in hex.
const PUBLIC_KEY: &str = match option_env!("KIKU_UPDATE_KEY") {
    Some(key) => key,
    None => "",
};
/// Checks are at least this far apart, across restarts.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the checker wakes to see whether a check is due and allowed.
const WAKE_EVERY: Duration = Duration::from_secs(60 * 60);
/// The manifest for this build's architecture; x64 keeps the name every copy from before ARM64 builds reads.
const MANIFEST: &str = if cfg!(target_arch = "aarch64") { "latest-arm64.json" } else { "latest.json" };
/// The manifest is a few hundred bytes; anything much bigger isn't one.
const MANIFEST_LIMIT: u64 = 64 * 1024;
/// What a signature is made over starts with this, so it can't be mistaken for anything else signed.
const SIGNED_PREFIX: &str = "kiku-update-v1";

/// A version on offer, as the signed manifest describes it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Release {
    pub version: String,
    /// The installer's address.
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// The releases' address from now on; empty, or the current one, when they haven't moved.
    #[serde(default)]
    pub moved_to: String,
    /// A line or two on what changed.
    pub notes: String,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(flatten)]
    release: Release,
    /// Hex ed25519 signature over [`Release::signed_text`].
    signature: String,
}

impl Release {
    /// What the manifest's signature covers: every field, one a line, the notes last since they may span lines.
    pub fn signed_text(&self) -> String {
        let Release { version, url, size, sha256, moved_to, notes } = self;
        format!("{SIGNED_PREFIX}\n{version}\n{url}\n{size}\n{sha256}\n{moved_to}\n{notes}")
    }
}

/// Whether this build can check for updates: it was given an address and a key.
pub fn available() -> bool {
    !RELEASES.is_empty() && public_key().is_some()
}

/// Where releases (updates and the model files Kiku serves) are: the last address a signed manifest moved
/// them to, or the build's own.
pub fn releases() -> String {
    let moved = fs::read_to_string(moved_path()).ok().map(|text| text.trim().to_owned());
    moved.filter(|releases| is_address(releases)).unwrap_or_else(|| RELEASES.into())
}

/// Checks whenever one is due and `allowed` says so, handing a newer version to `offer`. Runs forever.
pub fn watch(allowed: impl Fn() -> bool, offer: impl Fn(Release)) {
    loop {
        if available() && allowed() && due() {
            mark_checked();
            match check() {
                Ok(Some(release)) => offer(release),
                Ok(None) => {}
                Err(error) => log::write(format!("Update check failed: {error}")),
            }
        }
        thread::sleep(WAKE_EVERY);
    }
}

/// Fetches the manifest and returns the version it offers if it is newer than this one.
pub fn check() -> Result<Option<Release>, String> {
    let key = public_key().ok_or("this build has no update key")?;
    let url = format!("{}/latest/download/{MANIFEST}", releases());
    let mut response = download::agent().get(&url).call().map_err(|e| format!("couldn't reach the update server: {e}"))?;
    let json = response.body_mut().with_config().limit(MANIFEST_LIMIT).read_to_string().map_err(|e| format!("couldn't read the update manifest: {e}"))?;
    let release = verify(&json, &key)?;
    if !release.moved_to.is_empty() && release.moved_to != releases() {
        log::write(format!("Releases moved to {}", release.moved_to));
        let _ = fs::write(moved_path(), &release.moved_to);
    }
    Ok(is_newer(&release.version, env!("CARGO_PKG_VERSION")).then_some(release))
}

/// Downloads the installer of `release` to the temporary folder, checking its size and sha256 (which the
/// signature covers). Returns where it is.
pub fn download(release: &Release) -> Result<PathBuf, String> {
    let folder = std::env::temp_dir().join("kiku-update");
    // Older installers go; one being run can't be removed, which is fine.
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).map_err(|e| format!("couldn't make {}: {e}", folder.display()))?;
    let path = folder.join(format!("Kiku-{}-setup.exe", release.version));
    let result = save(release, &path);
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result.map(|()| path)
}

fn save(release: &Release, path: &std::path::Path) -> Result<(), String> {
    let mut response = download::agent().get(&release.url).call().map_err(|e| format!("couldn't download the update: {e}"))?;
    let mut body = response.body_mut().with_config().limit(release.size + 1).reader();
    let mut file = fs::File::create(path).map_err(|e| format!("couldn't save the update: {e}"))?;
    let (mut hasher, mut size, mut buffer) = (Sha256::new(), 0, vec![0; 1 << 16]);
    loop {
        let read = body.read(&mut buffer).map_err(|e| format!("the update's download stopped: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|e| format!("couldn't save the update: {e}"))?;
        size += read as u64;
    }
    if size != release.size || format!("{:x}", hasher.finalize()) != release.sha256 {
        return Err("the update downloaded isn't the one the manifest describes".into());
    }
    file.sync_all().map_err(|e| format!("couldn't save the update: {e}"))
}

/// The release a manifest describes, if `key` signed it and its fields make sense.
fn verify(json: &str, key: &VerifyingKey) -> Result<Release, String> {
    let manifest: Manifest = serde_json::from_str(json).map_err(|e| format!("the update manifest is malformed: {e}"))?;
    let signature = from_hex::<64>(&manifest.signature).map(|bytes| Signature::from_bytes(&bytes)).ok_or("the update manifest's signature is malformed")?;
    let release = manifest.release;
    key.verify_strict(release.signed_text().as_bytes(), &signature).map_err(|_| "the update manifest isn't signed by Kiku's key")?;
    let moved_to = release.moved_to.is_empty() || is_address(&release.moved_to);
    if parse_version(&release.version).is_none() || !release.url.starts_with("https://") || from_hex::<32>(&release.sha256).is_none() || !moved_to {
        return Err("the update manifest's fields are malformed".into());
    }
    Ok(release)
}

/// Whether `candidate` is a later version than `current`; both are `major.minor.patch`.
fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.').map(|part| part.parse().ok().filter(|_| part.bytes().all(|b| b.is_ascii_digit())));
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// An `https://` address to put paths after: no query, fragment, spaces or trailing slash.
fn is_address(text: &str) -> bool {
    let plain = |b: u8| b.is_ascii_graphic() && !b"?#\\\"<>".contains(&b);
    text.starts_with("https://") && text.len() <= 200 && !text.ends_with('/') && text.bytes().all(plain)
}

fn public_key() -> Option<VerifyingKey> {
    VerifyingKey::from_bytes(&from_hex::<32>(PUBLIC_KEY)?).ok()
}

fn from_hex<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != 2 * N || !hex.is_ascii() {
        return None;
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

/// Whether the last check was a day or more ago (or is unknown, or in the future: a clock put back).
fn due() -> bool {
    let last = fs::read_to_string(checked_path()).ok().and_then(|text| text.trim().parse::<u64>().ok());
    let now = now_secs();
    last.is_none_or(|last| last > now || now - last >= CHECK_EVERY.as_secs())
}

fn mark_checked() {
    let _ = fs::create_dir_all(paths::data_dir());
    let _ = fs::write(checked_path(), now_secs().to_string());
}

fn checked_path() -> PathBuf {
    paths::data_dir().join("update-checked.txt")
}

fn moved_path() -> PathBuf {
    paths::data_dir().join("update-releases.txt")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const SEED: [u8; 32] = [7; 32];

    fn release() -> Release {
        Release {
            version: "0.2.0".into(),
            url: "https://github.com/example/kiku/releases/download/v0.2.0/Kiku-0.2.0-setup.exe".into(),
            size: 41_000_000,
            sha256: "ab".repeat(32),
            moved_to: "https://github.com/example/kiku/releases".into(),
            notes: "Faster captions.\nFewer dropped words.".into(),
        }
    }

    fn manifest(release: &Release, signature: &str) -> String {
        let mut json = serde_json::json!({
            "version": release.version, "url": release.url, "size": release.size, "sha256": release.sha256, "notes": release.notes,
        });
        if !release.moved_to.is_empty() {
            json["moved_to"] = release.moved_to.as_str().into();
        }
        json["signature"] = signature.into();
        json.to_string()
    }

    fn signed(release: &Release) -> String {
        let signature = SigningKey::from_bytes(&SEED).sign(release.signed_text().as_bytes());
        manifest(release, &signature.to_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    fn key() -> VerifyingKey {
        SigningKey::from_bytes(&SEED).verifying_key()
    }

    #[test]
    fn a_signed_manifest_verifies_and_any_change_breaks_it() {
        let release = release();
        assert_eq!(verify(&signed(&release), &key()), Ok(release.clone()));
        for (from, to) in [("0.2.0-setup", "0.2.0-evil"), ("example/kiku/releases\"", "evil/kiku/releases\"")] {
            assert!(verify(&signed(&release).replace(from, to), &key()).is_err(), "{to}");
        }
        let unmoved = Release { moved_to: String::new(), ..release.clone() };
        assert!(!signed(&unmoved).contains("moved_to"));
        assert_eq!(verify(&signed(&unmoved), &key()), Ok(unmoved));
        let other_key = SigningKey::from_bytes(&[8; 32]).verifying_key();
        assert!(verify(&signed(&release), &other_key).is_err());
        assert!(verify(&manifest(&release, "00"), &key()).is_err());
        assert!(verify("{}", &key()).is_err());
    }

    #[test]
    fn signed_but_malformed_fields_are_refused() {
        for bad in [
            Release { version: "0.2".into(), ..release() },
            Release { url: "http://downloads.example/setup.exe".into(), ..release() },
            Release { sha256: "not hex".into(), ..release() },
            Release { moved_to: "http://github.com/example/kiku/releases".into(), ..release() },
            Release { moved_to: "https://github.com/example/kiku/releases/".into(), ..release() },
            Release { moved_to: "https://github.com/example/kiku/releases?x".into(), ..release() },
        ] {
            assert!(verify(&signed(&bad), &key()).is_err(), "{bad:?}");
        }
    }

    /// Signed by `scripts/release.py` with the seed [7; 32], so the signer and this check agree on the text and key.
    #[test]
    fn manifests_signed_by_the_release_script_verify() {
        let json = r#"{"version": "0.2.0", "url": "https://github.com/example/kiku/releases/download/v0.2.0/Kiku-0.2.0-setup.exe", "size": 41000000, "sha256": "abababababababababababababababababababababababababababababababab", "moved_to": "https://github.com/example/kiku/releases", "notes": "Faster captions.\nFewer dropped words.", "signature": "7fe294620c1cd35ba474a752bf08a69030ee322a30d19b3ad73e65d0cd6c0eda42e8ef41977e3445b418b7801dccf491dd12a4cd9f316c6d19a10c48265bb602"}"#;
        assert_eq!(verify(json, &key()), Ok(release()));
        let public: String = key().to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(public, "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c", "release.py public-key");
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        for bad in ["0.1", "0.1.0.1", "v0.1.0", "0.1.-1", "0.1.+1", "0.1.x"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }
}
