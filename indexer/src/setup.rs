//! Excavate's setup: one file to download. The release's zip rides at its end (scripts/package-release.ps1 appends
//! it); run, it unpacks the programs into a temporary folder and starts that copy, whose own first screen installs
//! Excavate (app/src/install.rs). Its arguments go through as they are, so `/S` installs without a window (winget),
//! and its exit code is the installer's. The temporary folder is removed once the installer is done.

#![windows_subsystem = "windows"]

use std::io::Cursor;
use std::path::Path;
use std::process::Command;

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            report(&error);
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32, String> {
    let me = std::env::current_exe().map_err(|error| format!("Where am I? {error}"))?;
    let bytes = std::fs::read(&me).map_err(|error| format!("Couldn't read the setup: {error}"))?;
    // The zip's central directory is at the very end, and its offsets are from the zip's own start: found past the
    // program in front of it.
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "This setup has no programs in it. Download it again".to_string())?;
    // `--unpack-to <folder>`: only unpacked there, nothing started (a check that the setup holds what it should).
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Some(at) = arguments.iter().position(|argument| argument == "--unpack-to") {
        let folder = arguments.get(at + 1).ok_or("--unpack-to needs a folder")?;
        archive.extract(folder).map_err(|error| format!("Couldn't unpack the programs: {error}"))?;
        return Ok(0);
    }
    let folder = std::env::temp_dir().join(format!("excavate-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).map_err(|error| format!("Couldn't make a temporary folder: {error}"))?;
    archive.extract(&folder).map_err(|error| format!("Couldn't unpack the programs: {error}"))?;
    let excavate = folder.join("excavate.exe");
    if !excavate.exists() {
        return Err("This setup has no excavate.exe in it. Download it again".into());
    }
    let status = Command::new(&excavate).args(&arguments).current_dir(&folder).status()
        .map_err(|error| format!("Couldn't start the installer: {error}"))?;
    remove_soon(&folder);
    Ok(status.code().unwrap_or(1))
}

/// The temporary copy, gone: tried for a few seconds, while what it started lets go of its files.
fn remove_soon(folder: &Path) {
    for _ in 0..20 {
        if std::fs::remove_dir_all(folder).is_ok() || !folder.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// A setup that can't go on says why, in Windows' own message box (it has no window of its own).
fn report(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;
    let text: Vec<u16> = message.encode_utf16().chain(std::iter::once(0)).collect();
    let title: Vec<u16> = "Excavate setup".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both texts outlive the call.
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR);
    }
}
