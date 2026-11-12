//! Updates (About, "Update automatically"; off until turned on). Once a day, Excavate asks its GitHub page which
//! release is the latest; a newer one is downloaded (`Excavate-<version>-windows-x64.zip`), checked against the
//! signature beside it (`.zip.sig`: ECDSA P-256 over the zip's SHA-256, made with the key only Excavate's maker keeps,
//! `scripts/release-key.ps1`; the public half is `PUBLIC_KEY`), unpacked into `%LOCALAPPDATA%\Excavate\updates\<version>`
//! and used at the next start (`apply_staged`): each program beside excavate.exe is renamed `.old` (Windows lets a
//! running program be renamed, not overwritten), the new one copied in its place, and the new excavate.exe started.
//! The `.old` files go at the start after. A release that isn't signed, or doesn't match its signature, is never used.
//! What's asked and downloaded is in the log. Releases are packed by `scripts/package-release.ps1` (docs/RELEASING.md).

use crate::settings::log;
use config::Paths;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The GitHub repository releases come from ("owner/name"). With None, nothing is asked.
pub const REPO: Option<&str> = Some("aynader/excavate");

/// The public half of the release-signing key (`BCRYPT_ECCPUBLIC_BLOB`, from `scripts/release-key.ps1`).
const PUBLIC_KEY: [u8; 72] = [
    0x45, 0x43, 0x53, 0x31, 0x20, 0x00, 0x00, 0x00, 0xfd, 0xf4, 0xa6, 0x4d, 0x84, 0x6c, 0x7f, 0x18, 0x43, 0x96, 0x1b, 0xa8,
    0xe8, 0x2a, 0xe3, 0x50, 0x44, 0x9c, 0x48, 0x63, 0xd4, 0x1f, 0x41, 0x1e, 0x1b, 0x32, 0x0a, 0x93, 0x19, 0xa3, 0x2b, 0x19,
    0x02, 0x1d, 0x79, 0x07, 0x91, 0x4a, 0x37, 0xe0, 0x6f, 0x3d, 0x78, 0x08, 0x70, 0x00, 0x32, 0x9b, 0x26, 0x41, 0x60, 0x46,
    0x7c, 0x15, 0xab, 0xd6, 0xf9, 0x7c, 0x78, 0xaa, 0x46, 0x16, 0xa0, 0x30,
];

/// What an update may hold: Excavate's programs and the libraries beside them, by name, nothing in folders (with
/// Microsoft's C++ runtime, which a fresh Windows may not have: `indexer::models::RUNTIME`).
const FILES: &[&str] = &["excavate.exe", "excavate-indexer.exe", "excavate-helper.exe", "pdfium.dll", "onnxruntime.dll", "DirectML.dll",
                         "msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll", "vcomp140.dll"];

/// How often updates are looked for, with the setting on.
pub const EVERY: i64 = 24 * 3600;

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Not looked for since starting.
    Idle,
    /// No repository to ask yet.
    NotPublished,
    Checking,
    Latest,
    /// Downloading this version: how much of it, 0 to 1.
    Downloading(String, f32),
    /// This version is checked and waits for the next start.
    Ready(String),
    Failed(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Idle);

pub fn status() -> Status {
    if feed().is_none() {
        return Status::NotPublished;
    }
    STATUS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

fn set(status: Status) {
    *STATUS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
    let _ = slint::invoke_from_event_loop(|| crate::with_app(|app| crate::setup::fill_about(app)));
}

/// Where the latest release is asked for. (A debug build may be pointed elsewhere, for tests: `EXCAVATE_UPDATE_FEED`.)
fn feed() -> Option<String> {
    if cfg!(debug_assertions) {
        if let Ok(url) = std::env::var("EXCAVATE_UPDATE_FEED") {
            return Some(url);
        }
    }
    REPO.map(|repo| format!("https://api.github.com/repos/{repo}/releases/latest"))
}

/// "v0.2.1" or "0.2.1-beta.3" as numbers to compare: 0, 2, 1 (then the beta's 3, a beta before the release itself).
fn version_key(text: &str) -> Vec<u64> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let (release, pre) = match text.split_once('-') {
        Some((release, pre)) => (release, Some(pre)),
        None => (text, None),
    };
    let mut key: Vec<u64> = release.split('.').map(|part| part.parse().unwrap_or(0)).collect();
    key.resize(3, 0);
    match pre {
        // A pre-release comes before its release: 0 then its number; the release itself is u64::MAX.
        Some(pre) => key.push(pre.rsplit('.').next().and_then(|n| n.parse().ok()).unwrap_or(0)),
        None => key.push(u64::MAX),
    }
    key
}

pub fn newer(latest: &str, current: &str) -> bool {
    version_key(latest) > version_key(current)
}

/// Looks for an update now, on a thread. The callers decide when: once a day with the setting on, when it's turned
/// on, or About's "Check now".
pub fn check(paths: &Paths) {
    let Some(feed) = feed() else { return };
    if matches!(status(), Status::Checking | Status::Downloading(..)) {
        return;
    }
    set(Status::Checking);
    let updates = paths.updates();
    let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
        let result = look(&feed, &updates);
        if let Err(error) = &result {
            log(&format!("update: {error}"));
        }
        set(match result {
            Ok(Some(version)) => Status::Ready(version),
            Ok(None) => Status::Latest,
            Err(error) => Status::Failed(error),
        });
    });
}

/// Asks for the latest release; a newer one is downloaded, checked and unpacked. Its version, when there's one ready.
fn look(feed: &str, updates: &Path) -> Result<Option<String>, String> {
    let mut answer = Vec::new();
    winsys::http::download(feed, &mut answer, &mut |_, _| {}, &|| false).map_err(|error| format!("couldn't ask GitHub: {error}"))?;
    let release: serde_json::Value = serde_json::from_slice(&answer).map_err(|_| "GitHub's answer wasn't readable".to_string())?;
    let tag = release["tag_name"].as_str().ok_or("the latest release has no version")?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    let current = env!("CARGO_PKG_VERSION");
    if !newer(&version, current) {
        log(&format!("update: {version} is the latest, and this is {current}"));
        return Ok(None);
    }
    let folder = updates.join(&version);
    if folder.join("ready").exists() {
        return Ok(Some(version));
    }
    let name = format!("Excavate-{version}-windows-x64.zip");
    let asset = |wanted: &str| -> Option<String> {
        release["assets"].as_array()?.iter()
            .find(|asset| asset["name"].as_str() == Some(wanted))
            .and_then(|asset| asset["browser_download_url"].as_str().map(String::from))
    };
    let (Some(zip_url), Some(sig_url)) = (asset(&name), asset(&format!("{name}.sig"))) else {
        return Err(format!("release {version} has no {name} with its signature"));
    };
    log(&format!("update: {version} is out (this is {current}): downloading"));
    let mut signature_hex = Vec::new();
    winsys::http::download(&sig_url, &mut signature_hex, &mut |_, _| {}, &|| false).map_err(|error| format!("couldn't download the signature: {error}"))?;
    let mut zip = Vec::new();
    let mut said = 0.0;
    winsys::http::download(&zip_url, &mut zip, &mut |received, total| {
        if total > 0 {
            let part = received as f32 / total as f32;
            if part - said >= 0.05 || part >= 1.0 {
                said = part;
                set(Status::Downloading(version.clone(), part));
            }
        }
    }, &|| false).map_err(|error| format!("couldn't download {name}: {error}"))?;
    let signature = hex(std::str::from_utf8(&signature_hex).unwrap_or("").trim()).ok_or("its signature isn't readable")?;
    let hash: [u8; 32] = Sha256::digest(&zip).into();
    if !winsys::signature::verify_p256(&PUBLIC_KEY, &hash, &signature) {
        return Err(format!("{name} doesn't match its signature: not used"));
    }
    unpack(&zip, &folder)?;
    std::fs::write(folder.join("ready"), &version).map_err(|error| error.to_string())?;
    log(&format!("update: {version} checked against its signature and ready for the next start"));
    Ok(Some(version))
}

fn hex(text: &str) -> Option<Vec<u8>> {
    (text.len() % 2 == 0).then_some(())?;
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok()).collect()
}

/// Unpacks the release's files (only those `FILES` names, nothing in folders) into `folder`.
fn unpack(zip: &[u8], folder: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|_| "the update isn't a readable zip".to_string())?;
    let _ = std::fs::remove_dir_all(folder);
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let mut found = Vec::new();
    for at in 0..archive.len() {
        let mut entry = archive.by_index(at).map_err(|error| error.to_string())?;
        let Ok(name) = entry.name().map(|name| name.to_string()) else { continue };
        let Some(&known) = FILES.iter().find(|file| file.eq_ignore_ascii_case(&name)) else { continue };
        let mut out = std::fs::File::create(folder.join(known)).map_err(|error| error.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|error| error.to_string())?;
        found.push(known);
    }
    if !found.contains(&"excavate.exe") {
        let _ = std::fs::remove_dir_all(folder);
        return Err("the update has no excavate.exe".into());
    }
    Ok(())
}

/// The newest update that's ready and newer than this one, if any.
pub fn staged(paths: &Paths) -> Option<(String, PathBuf)> {
    let current = env!("CARGO_PKG_VERSION");
    std::fs::read_dir(paths.updates()).ok()?
        .flatten()
        .filter_map(|entry| {
            let version = std::fs::read_to_string(entry.path().join("ready")).ok()?;
            newer(&version, current).then(|| (version, entry.path()))
        })
        .max_by(|a, b| version_key(&a.0).cmp(&version_key(&b.0)))
}

/// At start: the `.old` files of the last update go, and an update that's ready is put in place, then started (true:
/// this process ends). A copy that fails is undone, and the update set aside. Not for a debug build or a sandbox.
pub fn apply_staged(paths: &Paths) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let Some(dir) = exe.parent().map(Path::to_path_buf) else { return false };
    for file in FILES {
        let _ = std::fs::remove_file(dir.join(format!("{file}.old")));
    }
    // (A sandbox's copy only when a test asks: `EXCAVATE_UPDATE_TEST`, with the copy in a folder of its own.)
    let test = std::env::var_os("EXCAVATE_UPDATE_TEST").is_some();
    if cfg!(debug_assertions) || (std::env::var_os("EXCAVATE_HOME").is_some() && !test) {
        return false;
    }
    let Some((version, folder)) = staged(paths) else { return false };
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut put: Vec<PathBuf> = Vec::new();
    let mut result: Result<(), String> = Ok(());
    for file in FILES {
        let new = folder.join(file);
        if !new.exists() {
            continue;
        }
        let target = dir.join(file);
        if target.exists() {
            let old = dir.join(format!("{file}.old"));
            if let Err(error) = std::fs::rename(&target, &old) {
                result = Err(format!("{file}: {error}"));
                break;
            }
            moved.push((old, target.clone()));
        }
        if let Err(error) = std::fs::copy(&new, &target) {
            result = Err(format!("{file}: {error}"));
            break;
        }
        put.push(target);
    }
    if let Err(error) = result {
        // Undone: what was copied goes, what was renamed comes back; this update is set aside.
        for target in put {
            let _ = std::fs::remove_file(target);
        }
        for (old, target) in moved {
            let _ = std::fs::rename(old, target);
        }
        let _ = std::fs::remove_file(folder.join("ready"));
        log(&format!("update: couldn't put {version} in place ({error}); staying on {}", env!("CARGO_PKG_VERSION")));
        return false;
    }
    let _ = std::fs::remove_dir_all(&folder);
    log(&format!("update: {version} put in place; starting it"));
    let args: Vec<String> = std::env::args().skip(1).filter(|arg| arg != "--after-update").collect();
    std::process::Command::new(&exe).args(args).arg("--after-update").spawn().is_ok()
}

/// About's "Restart now": Excavate quits, and starts again on the update.
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--after-update").spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("0.1.1", "0.1.0"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        assert!(newer("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(newer("0.2.0", "0.2.0-beta.3"));
        assert!(!newer("0.2.0-beta.3", "0.2.0"));
    }

    #[test]
    fn signatures_are_checked() {
        let hash: [u8; 32] = Sha256::digest(b"not a release").into();
        assert!(!winsys::signature::verify_p256(&PUBLIC_KEY, &hash, &[0u8; 64]));
        assert_eq!(hex("0aff"), Some(vec![0x0a, 0xff]));
        assert_eq!(hex("0g"), None);
    }
}
