//! Experimental: Excavate's own index of the drives' names (`engine::names`), answering name searches in Everything's
//! place while it's switched on (Settings, General). It runs in this process, so a search is a call, not a round trip;
//! it's saved in the local data folder and loads in a fraction of a second at the next start.

use crate::app::App;
use crate::settings::log;
use crate::with_app;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static NAMES: Mutex<Option<Arc<engine::names::Names>>> = Mutex::new(None);
/// Built from the drives' file tables with administrator rights, right now (`build_then_on`).
static BUILDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn slot() -> std::sync::MutexGuard<'static, Option<Arc<engine::names::Names>>> {
    NAMES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts or stops the index to match the setting, and points name searches at it or back at Everything.
pub fn apply(app: &App) {
    let on = app.settings.borrow().own_index;
    pace(app);
    let mut slot = slot();
    match (on, slot.is_some()) {
        (true, false) => {
            log("own index: on");
            let names = engine::names::Names::start(app.paths.names());
            everything::set_local(Some(names.clone()));
            follow(names.clone());
            *slot = Some(names);
        }
        (false, true) => {
            log("own index: off");
            everything::set_local(None);
            if let Some(names) = slot.take() {
                names.stop();
            }
        }
        _ => {}
    }
}

/// Catching up on a drive at the speed control's level, now and each time it moves (a walk going on follows within a
/// folder): more folders listed at once at Fast and Full, and at normal priority at Full, as the indexer reads there.
pub fn pace(app: &App) {
    let speed = app.settings.borrow().speed();
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    engine::walk::set_pace(speed.name_listers(cores), speed.background_io());
}

/// Says how starting goes: in the log once it's done, and to the search window's footer and Settings' Everything line
/// at each step (listing for the first time, catching up, live), which polling Everything's health alone would miss
/// after its first answer. Ends when the index is switched off.
fn follow(names: Arc<engine::names::Names>) {
    let weak = Arc::downgrade(&names);
    drop(names);
    let _ = std::thread::Builder::new().name("own index start".into()).spawn(move || {
        let started = Instant::now();
        let mut said: Option<engine::names::Phase> = None;
        let mut answering = false;
        loop {
            // Switched off (or quitting): this one's no longer the one answering.
            let Some(names) = weak.upgrade() else { return };
            if !slot().as_ref().is_some_and(|current| Arc::ptr_eq(current, &names)) {
                return;
            }
            let state = names.state();
            if !answering && names.ready() {
                answering = true;
                log(&format!("own index: answering after {:.2} s", started.elapsed().as_secs_f64()));
            }
            if said != Some(state.phase) {
                said = Some(state.phase);
                if state.phase == engine::names::Phase::Building && !state.not_loaded.is_empty() {
                    log(&format!("own index: listing the drives afresh, as the saved index couldn't be used ({})", state.not_loaded));
                }
                let health = everything::Local::health(&*names);
                let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                    if everything::is_local() {
                        app.set_everything_health(health);
                        crate::setup::everything_changed(app);
                    }
                }));
                if matches!(state.phase, engine::names::Phase::Live | engine::names::Phase::Failed) {
                    log(&format!("own index: {:?} after {:.1} s: {} names on {} drives; loaded in {:?}, built in {:?}, caught up in {:?} (journal: {:?}; not from it: {:?}); {} folders couldn't be listed; {} watches{}",
                                 state.phase, started.elapsed().as_secs_f64(), state.entries, state.volumes.len(), state.loaded_in,
                                 state.built_in, state.caught_up_in, state.journal, state.journal_failed, state.refused, state.watches,
                                 if state.error.is_empty() { String::new() } else { format!("; {}", state.error) }));
                    return;
                }
            }
            drop(names);
            std::thread::sleep(Duration::from_millis(250));
        }
    });
}

/// The indexer not chosen holds no memory: with Excavate's own index on, Everything is asked to exit (its
/// service, if installed, stays: stopping it would need administrator rights); with Everything chosen again, it's
/// started again if Excavate was what closed it. With `EXCAVATE_PRETEND_EVERYTHING` set, what would be done is only
/// logged (tests never close or start the person's own Everything).
pub fn match_everything(app: &App) {
    let own = app.settings.borrow().own_index;
    let closed = app.settings.borrow().everything_closed;
    let pretend = std::env::var_os("EXCAVATE_PRETEND_EVERYTHING").is_some();
    if own {
        std::thread::spawn(move || {
            let running = everything_running();
            let Some(exe) = everything::installed_everything().filter(|_| running) else { return };
            if pretend {
                log(&format!("own index: would ask Everything to exit ({exe} -exit; pretend)"));
            } else {
                log("own index: asking Everything to exit");
                let _ = std::process::Command::new(&exe).arg("-exit").status();
            }
            let _ = slint::invoke_from_event_loop(|| with_app(|app| {
                app.settings.borrow_mut().everything_closed = true;
                app.settings.borrow().save(&app.paths);
            }));
        });
    } else if closed {
        app.settings.borrow_mut().everything_closed = false;
        app.settings.borrow().save(&app.paths);
        if let Some(exe) = everything::installed_everything() {
            if pretend {
                log(&format!("own index: would start Everything again ({exe} -startup; pretend)"));
            } else {
                log("own index: starting Everything again");
                let _ = std::process::Command::new(exe).arg("-startup").spawn();
            }
        }
    }
}

/// Whether Everything's own program is running (its window; the service has none). Pretending, as the switch says.
fn everything_running() -> bool {
    match std::env::var("EXCAVATE_PRETEND_EVERYTHING").as_deref() {
        Ok("running") => true,
        Ok(_) => false,
        Err(_) => winsys::window::any_of_class("EVERYTHING_TASKBAR_NOTIFICATION"),
    }
}

/// Whether switching it on would build it from nothing (no saved index): then Windows' administrator prompt is offered
/// first, explained in a popup of its own before it (`AdminAsk`).
pub fn from_nothing(app: &App) -> bool {
    // (With Excavate Bedrock installed it's built through it, in seconds, without asking.)
    slot().is_none() && !BUILDING.load(std::sync::atomic::Ordering::SeqCst) && !app.paths.names().exists()
        && !engine::names::service_installed()
}

/// Being built from the drives' file tables, with administrator rights.
pub fn building() -> bool {
    BUILDING.load(std::sync::atomic::Ordering::SeqCst)
}

/// "Allow": `excavate-indexer --build-names` through Windows' prompt, writing the saved index from each drive's file
/// table (about 20 s on this PC against a minute and a half walking, and the ~500 folders a walk can't list); then
/// it's switched on, and loads what was built and catches up from the journal. "No" at Windows' prompt, or a build
/// that failed: switched on all the same, listing the drives the usual way. A sandbox (`EXCAVATE_PRETEND_EVERYTHING`
/// or `EXCAVATE_NO_ELEVATE`) never asks Windows: it logs what it would do and lists the usual way.
pub fn build_then_on(app: &App) {
    let file = app.paths.names();
    let exe = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join("excavate-indexer.exe")));
    let pretend = std::env::var_os("EXCAVATE_PRETEND_EVERYTHING").is_some() || std::env::var_os("EXCAVATE_NO_ELEVATE").is_some();
    BUILDING.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::thread::Builder::new().name("own index build".into()).spawn(move || {
        let started = Instant::now();
        let outcome = match exe.filter(|exe| exe.exists()) {
            _ if pretend => {
                log(&format!("own index: would ask Windows to run excavate-indexer --build-names \"{}\" (pretend)", file.display()));
                None
            }
            None => {
                log("own index: excavate-indexer.exe isn't beside excavate.exe: listed the usual way");
                None
            }
            Some(exe) => {
                log("own index: asking Windows for administrator rights to read the drives' file tables");
                Some(winsys::process::run_elevated(&exe.to_string_lossy(), &format!("--build-names \"{}\"", file.display())))
            }
        };
        match &outcome {
            Some(winsys::process::Elevated::Ended(0)) => log(&format!("own index: built from the drives' file tables in {:.1} s", started.elapsed().as_secs_f64())),
            Some(winsys::process::Elevated::Ended(code)) => log(&format!("own index: the build ended with {code} after {:.1} s: listed the usual way", started.elapsed().as_secs_f64())),
            Some(winsys::process::Elevated::Declined) => log("own index: Windows' prompt declined: listed the usual way"),
            Some(winsys::process::Elevated::Failed(code)) => log(&format!("own index: Windows couldn't start the build ({code}): listed the usual way")),
            None => {}
        }
        let _ = slint::invoke_from_event_loop(|| with_app(|app| {
            BUILDING.store(false, std::sync::atomic::Ordering::SeqCst);
            crate::setup::set_own_index(app, true);
        }));
    });
}

/// The drive whose record of changes was made larger here (Activity's "Fix this"), so it isn't offered again.
static JOURNAL_FIXED: Mutex<Option<String>> = Mutex::new(None);

/// Whether "Fix this" made the record of changes on `drive` larger.
pub fn journal_fixed(drive: &str) -> bool {
    JOURNAL_FIXED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_deref() == Some(drive)
}

/// How large the record of changes (the USN journal) is made by "Fix this": 1 GB, growing 128 MB at a time. About 8
/// million changes: days of a busy PC with Excavate closed (Windows' default, 32 MB, held ~320k on a busy PC, less
/// than an afternoon of builds).
pub const JOURNAL_SIZE: u64 = 1 << 30;

/// Activity's "Fix this", agreed to: Windows' `fsutil usn createjournal` through its administrator prompt makes the
/// drive's record of changes larger (an existing journal keeps its id and records: the saved position stays good).
/// Then Bedrock catches up from it after Excavate was closed for a long while too. A sandbox
/// (`EXCAVATE_PRETEND_EVERYTHING`, `EXCAVATE_NO_ELEVATE`, `EXCAVATE_PRETEND_RELIST`) only logs it.
pub fn fix_journal(drive: String) {
    set_journal(drive, JOURNAL_SIZE, JOURNAL_SIZE / 8, true);
}

/// Windows' own size for a drive's record of changes: 32 MB, growing 8 MB at a time.
pub const JOURNAL_DEFAULT: u64 = 32 << 20;

/// A record of changes this large or larger was made so by "Fix this" (Windows' own is 32 MB): Settings offers to put
/// it back.
const JOURNAL_LARGE: u64 = 256 << 20;

/// The drives whose record of changes is far larger than Windows' own ("C:"), for Settings' "Back to Windows' size".
pub fn large_journals() -> Vec<String> {
    engine::names::indexed_drives().into_iter()
        .filter(|drive| engine::journal::size(drive).is_some_and(|size| size >= JOURNAL_LARGE))
        .map(|drive| drive.trim_end_matches('\\').to_string()).collect()
}

/// Settings' "Back to Windows' size", agreed to: each drive's record of changes made Windows' own size again, through
/// its administrator prompt (the journal keeps its id: the saved position stays good, though a long while closed may
/// mean listing a drive again, as before "Fix this").
pub fn journal_to_default(drives: Vec<String>) {
    for drive in drives {
        set_journal(format!("{drive}\\"), JOURNAL_DEFAULT, 8 << 20, false);
    }
}

fn set_journal(drive: String, size: u64, growth: u64, larger: bool) {
    let pretend = ["EXCAVATE_PRETEND_EVERYTHING", "EXCAVATE_NO_ELEVATE", "EXCAVATE_PRETEND_RELIST"].iter().any(|name| std::env::var_os(name).is_some());
    let letter = drive.trim_end_matches('\\').to_string();
    let args = format!("usn createjournal m={size} a={growth} {letter}");
    let _ = std::thread::Builder::new().name("journal size".into()).spawn(move || {
        let fsutil = std::path::Path::new(&std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\fsutil.exe");
        let fixed = if pretend {
            log(&format!("journal: would ask Windows to run fsutil {args} (pretend)"));
            true
        } else {
            log(&format!("journal: asking Windows for administrator rights to run fsutil {args}"));
            match winsys::process::run_elevated(&fsutil.to_string_lossy(), &args) {
                winsys::process::Elevated::Ended(0) => {
                    log(&format!("journal: {letter} now keeps up to {} MB of changes", engine::journal::size(&drive).unwrap_or(0) >> 20));
                    true
                }
                winsys::process::Elevated::Ended(code) => {
                    log(&format!("journal: fsutil ended with {code}"));
                    false
                }
                winsys::process::Elevated::Declined => {
                    log("journal: Windows' prompt declined");
                    false
                }
                winsys::process::Elevated::Failed(code) => {
                    log(&format!("journal: Windows couldn't start fsutil ({code})"));
                    false
                }
            }
        };
        if fixed && larger {
            *JOURNAL_FIXED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(drive);
        } else if fixed {
            JOURNAL_FIXED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        }
        let _ = slint::invoke_from_event_loop(|| with_app(|app| crate::setup::refill(app)));
    });
}

/// The index, once it answers (the welcome page measures its first scene on it).
pub fn answering() -> Option<Arc<engine::names::Names>> {
    slot().clone().filter(|names| names.ready())
}

/// How the index is doing, when it's on.
pub fn state() -> Option<engine::names::State> {
    slot().as_ref().map(|names| names.state())
}

/// Saves it as it is, now (Windows is shutting down; from any thread).
pub fn save_now() {
    let names = slot().clone();
    if let Some(names) = names {
        names.save_now();
        log("own index saved: Windows is shutting down");
    }
}

/// Saves it, at quitting.
pub fn stop() {
    let Some(names) = slot().take() else { return };
    everything::set_local(None);
    names.stop();
    // The follower saves on its way out (a second or two for millions of names): it's waited for, up to a point.
    let started = Instant::now();
    while !names.stopped() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
}
