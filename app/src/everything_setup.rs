//! Installing Everything in one step: voidtools' own installer, checked against the SHA-256 voidtools publishes, run
//! silently (Windows asks for approval once), then started and waited for until its list of files is ready.

use everything::{Everything, Fix, Health};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INSTALLER: &str = "Everything-1.4.1.1032.x64-Setup.exe";
const URL: &str = "https://www.voidtools.com/Everything-1.4.1.1032.x64-Setup.exe";
/// From https://www.voidtools.com/Everything-1.4.1.1032.sha256.
const SHA256: &str = "c42efad041d4c0bb4d4ac97ae7cbe89f153ec1fe078772392e749c7f5d5282d3";

fn digest(path: &Path) -> Option<String> {
    // About 2 MB: read whole.
    let bytes = std::fs::read(path).ok()?;
    Some(Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Gets the installer into `folder` (once), checked. `progress(received, total)` while it downloads.
pub fn download(folder: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let target = folder.join(INSTALLER);
    if digest(&target).as_deref() == Some(SHA256) {
        return Ok(target);
    }
    let partial = target.with_extension("part");
    let mut out = std::fs::File::create(&partial).map_err(|error| error.to_string())?;
    winsys::http::download(URL, &mut out, progress, &|| false)?;
    out.flush().map_err(|error| error.to_string())?;
    drop(out);
    if digest(&partial).as_deref() != Some(SHA256) {
        let _ = std::fs::remove_file(&partial);
        return Err("The download didn't match voidtools' checksum, so it wasn't run.".into());
    }
    std::fs::rename(&partial, &target).map_err(|error| error.to_string())?;
    Ok(target)
}

/// The whole install. `progress(what's happening, 0 to 1 or below 0 when there's no measure)`.
pub fn install(folder: &Path, everything: &Everything, progress: &dyn Fn(String, f32)) -> Result<(), String> {
    let installer = download(folder, &mut |received, total| {
        if total > 0 {
            progress(format!("Downloading from voidtools.com… {} of {}", crate::format::size(received), crate::format::size(total)),
                     received as f32 / total as f32);
        }
    }).map_err(|error| format!("Couldn't download it: {error}"))?;
    // The test switch: everything up to running it (on a PC that has Everything).
    if std::env::var_os("EXCAVATE_NO_EVERYTHING").is_some() {
        return Err("Test: the installer was downloaded and checked, and not run.".into());
    }
    progress("Installing. Windows will ask for your approval…".into(), -1.0);
    let code = winsys::shell::run_elevated(&installer.display().to_string(), "/S", Duration::from_secs(600))
        .map_err(|error| format!("It wasn't installed: {error}"))?
        .ok_or("The installer is still running after 10 minutes.")?;
    if code != 0 {
        return Err(format!("The installer stopped (code {code})."));
    }
    let _ = std::fs::remove_file(&installer);
    let exe = everything::installed_everything().ok_or("The installer finished, but Everything isn't there.")?;
    progress("Starting Everything…".into(), -1.0);
    // Started as the installer leaves it set up (it runs with Windows from now on).
    let _ = std::process::Command::new(exe).arg("-startup").spawn();
    progress("Making its list of your files…".into(), -1.0);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(180) {
        let status = everything.status();
        if status.available && status.db_loaded {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("Everything was installed, but its list isn't ready yet. It'll be used once it is.".into())
}

/// How fresh an answer `health` may reuse.
pub const HEALTH_FRESH: Duration = Duration::from_secs(600);

/// Whether Everything keeps up (`Everything::health`), with this process's probe. `EXCAVATE_EVERYTHING_HEALTH` says
/// "loading", "behind" or "live" instead, for tests.
pub fn health(everything: &Everything, probe: &Path, fresh: Duration) -> Health {
    match std::env::var("EXCAVATE_EVERYTHING_HEALTH").as_deref() {
        Ok("loading") => Health::Loading,
        Ok("behind") => Health::Behind(Fix::StartService),
        Ok("live") => Health::Live,
        _ => everything.health(probe, fresh),
    }
}

/// Gets Everything watching the drives again: starts or installs its service (Windows asks for approval), then, when the
/// Everything running doesn't take to it, starts Everything again. Or only starts it again. `progress` says what's
/// happening (every second while Everything makes its list, with `last` time's length). Says how long that took when
/// Everything was started again. Never give up on it early: starting it again would start its list over (three
/// clicks took about 5½ minutes, 3 of them the last list).
pub fn fix(fix: Fix, everything: &Everything, probe: &Path, last: Option<Duration>, progress: &dyn Fn(&str))
           -> Result<Option<Duration>, String> {
    // With the health pretended, the fix is too: its steps, without Windows' prompt or Everything.
    if std::env::var_os("EXCAVATE_EVERYTHING_HEALTH").is_some() {
        progress("Waiting for your approval in the Windows prompt…");
        std::thread::sleep(Duration::from_secs(2));
        progress("Starting Everything again…");
        let started = Instant::now();
        let before = last.map(|last| format!(", {} last time", clock(last))).unwrap_or_default();
        while started.elapsed() < Duration::from_secs(6) {
            progress(&format!("Everything is making its list of files again: {} so far{before}", clock(started.elapsed())));
            std::thread::sleep(Duration::from_secs(1));
        }
        return Ok(Some(started.elapsed()));
    }
    let exe = everything::installed_everything().ok_or("Everything isn't where Excavate can find it.")?;
    if fix != Fix::Restart {
        progress("Waiting for your approval in the Windows prompt…");
        let option = if fix == Fix::StartService { "-start-service" } else { "-install-service" };
        let code = winsys::shell::run_elevated(&exe, option, Duration::from_secs(60))
            .map_err(|error| format!("Its service wasn't started: {error}"))?;
        // (Still running after a minute: the service may well be up, which what follows shows.)
        if let Some(code) = code.filter(|&code| code != 0) {
            return Err(format!("Its service wasn't started (code {code})."));
        }
        progress("Checking whether Everything sees new files now…");
        everything.forget_health();
        if everything.notices(probe, Duration::from_secs(3)) == Some(true) {
            return Ok(None);
        }
    }
    // The Everything running began without its service, and only a new one uses it.
    progress("Starting Everything again…");
    let _ = std::process::Command::new(&exe).arg("-exit").status();
    let started = Instant::now();
    while everything.status().available && started.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(250));
    }
    std::process::Command::new(&exe).arg("-startup").spawn().map_err(|error| format!("Everything didn't start: {error}"))?;
    everything.forget_health();
    let started = Instant::now();
    let before = last.map(|last| format!(", {} last time", clock(last))).unwrap_or_default();
    while started.elapsed() < Duration::from_secs(30 * 60) {
        let status = everything.status();
        if status.available && status.db_loaded {
            return Ok(Some(started.elapsed()));
        }
        progress(&format!("Everything is making its list of files again: {} so far{before}", clock(started.elapsed())));
        std::thread::sleep(Duration::from_secs(1));
    }
    Err("Everything is still making its list after half an hour. Excavate reads your folders directly meanwhile.".into())
}

/// "0:42", "3:05".
pub fn clock(time: Duration) -> String {
    let seconds = time.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
