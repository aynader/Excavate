//! Whether Windows has Media Foundation, which plays and decodes recordings and videos (`audio`, `sound`, `video`).
//! Windows' N and KN editions come without it until the Media Feature Pack is added, so Excavate's programs load its
//! libraries only when they're first used (delay-loaded: see app/build.rs and indexer/build.rs), and each use asks here
//! first: calling into a library that isn't there would end the program.

use crate::wide;
use std::sync::OnceLock;
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use windows::core::PCWSTR;

/// The libraries the programs delay-load, each of which Media Foundation needs.
pub const LIBRARIES: [&str; 2] = ["mfplat.dll", "mfreadwrite.dll"];

/// What a recording or a video says when Media Foundation isn't there.
pub const MISSING: &str = "Windows' Media Feature Pack isn't installed, so recordings and videos can't be played or read";

/// Whether Media Foundation is there: each of its libraries loads from Windows' own folder. Asked once; the libraries
/// stay loaded (they'd be loaded on first use anyway). With `EXCAVATE_PRETEND_NO_MEDIA` set, as if it weren't: an N
/// edition's way, on any PC.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if std::env::var_os("EXCAVATE_PRETEND_NO_MEDIA").is_some() {
            return false;
        }
        LIBRARIES.iter().all(|name| {
            let name = wide(name);
            // SAFETY: a system library loaded by name from System32 only, and kept.
            unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32).is_ok() }
        })
    })
}

/// `available`, as the error each use of Media Foundation returns without it.
pub fn check() -> Result<(), String> {
    if available() { Ok(()) } else { Err(MISSING.to_string()) }
}
