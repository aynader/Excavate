//! Starting and stopping the indexer (excavate-indexer.exe, beside excavate.exe). It runs in a job object, so it
//! ends with Excavate even if Excavate is killed. Settings changes reach it through a named event.

use crate::settings::log;
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

pub struct Indexer {
    job: Option<winsys::process::Job>,
    child: RefCell<Option<std::process::Child>>,
    /// When it was last started.
    started_at: Cell<Option<Instant>>,
    /// When it last ended without being asked to, and how many times in a row it has, each soon after starting.
    ended_at: Cell<Option<Instant>>,
    endings: Cell<u32>,
    /// Asked to stop (quitting): not started again.
    stopped: Cell<bool>,
    /// A change it hasn't been told of yet (`settings_changed_soon`).
    pending: Cell<bool>,
    /// Asked to end so it starts again ("Restart reading"): started at once when it has, not counted as a crash.
    restarting: Cell<bool>,
    /// Being started, on a thread of its own (`spawned` takes it from there).
    spawning: Cell<bool>,
}

/// It ended this soon after starting: counted as one more time in a row (the next start waits longer).
const SOON: Duration = Duration::from_secs(600);

/// How long after it ended without being asked (a crash) it's started again: at once the first time, then longer each
/// time it ends again soon after, so a file that takes it down each time doesn't keep the PC busy.
fn restart_after(endings: u32) -> Duration {
    match endings {
        0 | 1 => Duration::ZERO,
        2 => Duration::from_secs(120),
        3 => Duration::from_secs(600),
        _ => Duration::from_secs(1800),
    }
}

impl Indexer {
    pub fn new() -> Indexer {
        Indexer {
            job: winsys::process::Job::new(),
            child: RefCell::new(None),
            started_at: Cell::new(None),
            ended_at: Cell::new(None),
            endings: Cell::new(0),
            stopped: Cell::new(false),
            pending: Cell::new(false),
            restarting: Cell::new(false),
            spawning: Cell::new(false),
        }
    }

    /// Looks whether it's still running (once a minute, and each second while Settings is open): one that ended
    /// without being asked is started again. Before, nothing noticed: its status.json stayed as it last wrote it
    /// ("reading", the same counts), nothing was read, and only restarting Excavate started it again.
    pub fn watch(&self) {
        if self.stopped.get() {
            return;
        }
        {
            let mut child = self.child.borrow_mut();
            if let Some(running) = child.as_mut() {
                let Ok(Some(status)) = running.try_wait() else { return };
                if self.restarting.replace(false) {
                    log(&format!("the indexer ended ({status}) to start again, as asked"));
                    *child = None;
                    drop(child);
                    self.start();
                    return;
                }
                let lived = self.started_at.get().map_or(Duration::MAX, |at| at.elapsed());
                let endings = if lived < SOON { self.endings.get() + 1 } else { 1 };
                log(&format!("the indexer ended by itself ({status}) after {} s; starting it again in {} s", lived.as_secs(),
                             restart_after(endings).as_secs()));
                self.endings.set(endings);
                self.ended_at.set(Some(Instant::now()));
                *child = None;
            }
        }
        if let Some(ended) = self.ended_at.get()
            && ended.elapsed() >= restart_after(self.endings.get()) {
                self.ended_at.set(None);
                self.start();
            }
    }

    fn exe() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?.parent()?.join("excavate-indexer.exe");
        exe.exists().then_some(exe)
    }

    /// Starts the indexer if it isn't running.
    pub fn start(&self) {
        if self.running() || self.spawning.get() {
            return;
        }
        // For screenshots of how reading looks: the indexer isn't started, and its status file is as the test wrote it.
        if std::env::var_os("EXCAVATE_NO_INDEXER").is_some() {
            log("indexer not started (EXCAVATE_NO_INDEXER)");
            return;
        }
        let Some(exe) = Self::exe() else {
            log("excavate-indexer.exe isn't beside excavate.exe: nothing will be read inside files");
            return;
        };
        // Started on a thread of its own: Windows can take seconds to start a program it hasn't seen run (Defender reads
        // a new one through first), and the window would stop answering meanwhile.
        self.spawning.set(true);
        let started = std::thread::Builder::new().name("start indexer".into()).spawn(move || {
            let result = std::process::Command::new(&exe).spawn().map_err(|error| error.to_string());
            let _ = slint::invoke_from_event_loop(move || crate::with_app(|app| app.indexer.spawned(result)));
        });
        if let Err(error) = started {
            self.spawning.set(false);
            log(&format!("can't start the indexer: {error}"));
        }
    }

    /// The indexer's process, started (`start`): kept, in the job that ends it with Excavate; or ended at once when
    /// Excavate was asked to stop it meanwhile.
    fn spawned(&self, result: Result<std::process::Child, String>) {
        self.spawning.set(false);
        match result {
            Ok(mut child) => {
                if self.stopped.get() {
                    let _ = child.kill();
                    return;
                }
                if let Some(job) = &self.job {
                    job.add(child.id());
                }
                log(&format!("indexer started (process {})", child.id()));
                *self.child.borrow_mut() = Some(child);
                self.started_at.set(Some(Instant::now()));
                self.ended_at.set(None);
            }
            Err(error) => log(&format!("can't start the indexer: {error}")),
        }
    }

    pub fn running(&self) -> bool {
        let mut child = self.child.borrow_mut();
        match child.as_mut().map(|child| child.try_wait()) {
            Some(Ok(None)) => true,
            Some(_) => {
                *child = None;
                false
            }
            None => false,
        }
    }

    /// Tells it the settings changed; starts it when it isn't running. A pause or "Full speed" alone is taken in as
    /// it reads (nothing is listed again, its models stay loaded); anything else has it start over with them.
    pub fn settings_changed(&self) {
        self.pending.set(false);
        if !winsys::process::signal(&config::indexer_settings_event()) {
            self.start();
        }
    }

    /// A change to tell it of in a moment (Settings waits for a burst of changes to settle): `tell_pending` tells it.
    pub fn settings_changed_soon(&self) {
        self.pending.set(true);
    }

    /// Tells it of a change still waiting, if there is one: when the moment comes, and when Settings closes before it
    /// does (its timer goes with the window: the change was saved, but the indexer only saw it once Excavate started
    /// again).
    pub fn tell_pending(&self) {
        if self.take_pending() {
            self.settings_changed();
        }
    }

    fn take_pending(&self) -> bool {
        self.pending.replace(false)
    }

    /// Ends it and starts it again ("Restart reading" in Activity): asked to quit, and started again by `watch` once it
    /// has (it saves what it read first); started at once if it isn't running.
    pub fn restart(&self) {
        if !self.running() {
            self.start();
            return;
        }
        self.restarting.set(true);
        winsys::process::signal(&config::indexer_quit_event());
    }

    /// Asks it to let go of a file it's reading now ("Skip" in Activity): the path in `Paths::skip`, then the signal.
    pub fn skip(&self, paths: &config::Paths, path: &str) {
        if std::fs::write(paths.skip(), path).is_ok() {
            winsys::process::signal(&config::indexer_skip_event());
        }
    }

    /// Asks it to read again what couldn't be read ("Try again", "Read again", "Download and read" in Activity): the
    /// requests added to `Paths::retry`, then the signal. Not running, it's started, and takes them as it begins.
    pub fn retry(&self, paths: &config::Paths, retries: &[config::Retry]) {
        if config::Retry::ask(paths, retries) && !winsys::process::signal(&config::indexer_retry_event()) {
            self.start();
        }
    }

    /// Asks it to finish (it commits what it read), waiting a moment; the job ends it either way.
    pub fn stop(&self) {
        self.stopped.set(true);
        winsys::process::signal(&config::indexer_quit_event());
        if let Some(mut child) = self.child.borrow_mut().take() {
            for _ in 0..30 {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_told_once() {
        let indexer = Indexer::new();
        assert!(!indexer.take_pending());
        indexer.settings_changed_soon();
        indexer.settings_changed_soon();
        assert!(indexer.take_pending());
        assert!(!indexer.take_pending(), "told once");
    }

    #[test]
    fn started_again_sooner_or_later() {
        assert_eq!(restart_after(1), Duration::ZERO);
        assert!(restart_after(2) > restart_after(1) && restart_after(3) > restart_after(2));
        assert_eq!(restart_after(4), restart_after(40));
    }
}
