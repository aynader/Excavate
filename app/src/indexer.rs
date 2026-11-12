//! Starting and stopping the indexer (excavate-indexer.exe, beside excavate.exe). It runs in a job object, so it
//! ends with Excavate even if Excavate is killed. Settings changes reach it through a named event.

use crate::settings::log;
use std::cell::RefCell;

pub struct Indexer {
    job: Option<winsys::process::Job>,
    child: RefCell<Option<std::process::Child>>,
}

impl Indexer {
    pub fn new() -> Indexer {
        Indexer { job: winsys::process::Job::new(), child: RefCell::new(None) }
    }

    fn exe() -> Option<std::path::PathBuf> {
        let exe = std::env::current_exe().ok()?.parent()?.join("excavate-indexer.exe");
        exe.exists().then_some(exe)
    }

    /// Starts the indexer if it isn't running.
    pub fn start(&self) {
        if self.running() {
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
        match std::process::Command::new(&exe).spawn() {
            Ok(child) => {
                if let Some(job) = &self.job {
                    job.add(child.id());
                }
                log(&format!("indexer started (process {})", child.id()));
                *self.child.borrow_mut() = Some(child);
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

    /// Tells it the settings changed (it starts over with them); starts it when it isn't running.
    pub fn settings_changed(&self) {
        if !winsys::process::signal(&config::indexer_settings_event()) {
            self.start();
        }
    }

    /// Asks it to let go of a file it's reading now ("Skip" in Activity): the path in `Paths::skip`, then the signal.
    pub fn skip(&self, paths: &config::Paths, path: &str) {
        if std::fs::write(paths.skip(), path).is_ok() {
            winsys::process::signal(&config::indexer_skip_event());
        }
    }

    /// Asks it to finish (it commits what it read), waiting a moment; the job ends it either way.
    pub fn stop(&self) {
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
