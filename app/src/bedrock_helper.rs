//! Excavate Bedrock, the helper (a Windows service, `engine::service`): offered on the Indexers step (its consent
//! card) and in Settings. Installing and removing it is `excavate-bedrock.exe --install` / `--remove` through Windows'
//! administrator prompt, once each. A sandbox (`EXCAVATE_NO_ELEVATE`) never asks Windows: it logs what it would do and
//! pretends it went well, so the card and the row can be tried.

use crate::settings::log;
use crate::with_app;
use std::sync::Mutex;

/// What's happening now that the service itself can't say: "installing", or "failed" with the reason.
static NOW: Mutex<Option<(&'static str, String)>> = Mutex::new(None);
/// In a sandbox: what it would be now.
static PRETEND_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pretend() -> bool {
    std::env::var_os("EXCAVATE_NO_ELEVATE").is_some()
}

/// "missing", "installing", "ready" or "failed", and the reason when it failed.
pub fn state() -> (&'static str, String) {
    if let Some(now) = NOW.lock().unwrap_or_else(|p| p.into_inner()).clone() {
        return now;
    }
    let ready = if pretend() { PRETEND_READY.load(std::sync::atomic::Ordering::SeqCst) } else { engine::names::service_installed() };
    (if ready { "ready" } else { "missing" }, String::new())
}

/// The drives it reads (NTFS ones): what Settings' row counts.
pub fn drives() -> usize {
    use engine::service::Source;
    engine::service::Pc.drives().len()
}

fn program() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?.parent()?.join("excavate-bedrock.exe");
    exe.exists().then_some(exe)
}

/// "Allow" (`install == true`) or "Remove the helper": the program through Windows' prompt, on a thread; the card and
/// the row follow.
pub fn change(install: bool) {
    *NOW.lock().unwrap_or_else(|p| p.into_inner()) = Some(("installing", String::new()));
    refresh();
    let _ = std::thread::Builder::new().name("bedrock helper".into()).spawn(move || {
        let what = if install { "--install" } else { "--remove" };
        let outcome: Result<(), String> = if pretend() {
            log(&format!("bedrock helper: would ask Windows to run excavate-bedrock {what} (pretend)"));
            std::thread::sleep(std::time::Duration::from_millis(800));
            PRETEND_READY.store(install, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        } else if let Some(exe) = program() {
            log(&format!("bedrock helper: asking Windows for administrator rights to run excavate-bedrock {what}"));
            match winsys::process::run_elevated(&exe.to_string_lossy(), what) {
                winsys::process::Elevated::Ended(0) => Ok(()),
                winsys::process::Elevated::Ended(code) => {
                    let note = std::fs::read_to_string(std::env::temp_dir().join("excavate-bedrock-error.txt")).unwrap_or_default();
                    Err(if note.is_empty() { format!("It didn't work (code {code}).") } else { format!("It didn't work: {note}.") })
                }
                // "No" at Windows' prompt: nothing changed, and nothing to say about it.
                winsys::process::Elevated::Declined => Err(String::new()),
                winsys::process::Elevated::Failed(code) => Err(format!("Windows couldn't start it (error {code}).")),
            }
        } else {
            Err("excavate-bedrock.exe isn't beside Excavate.".into())
        };
        match &outcome {
            Ok(()) => log(&format!("bedrock helper: {}", if install { "installed" } else { "removed" })),
            Err(why) if why.is_empty() => log("bedrock helper: Windows' prompt declined"),
            Err(why) => log(&format!("bedrock helper: {why}")),
        }
        *NOW.lock().unwrap_or_else(|p| p.into_inner()) = match outcome {
            Err(why) if !why.is_empty() => Some(("failed", why)),
            _ => None,
        };
        refresh();
    });
}

/// The settings window shows what's so now.
fn refresh() {
    let _ = slint::invoke_from_event_loop(|| with_app(|app| crate::setup::fill(app)));
}
