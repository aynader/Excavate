//! How Excavate tells the indexer what to do: two named events, one to quit and one for "settings changed" (start
//! over with the new folders and reading choice). Every worker checks `stopping()` between files, and waits through
//! `sleep()`, which a signal cuts short.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;


#[derive(Default)]
pub struct Control {
    quit: AtomicBool,
    restart: AtomicBool,
    /// The file "Skip" asked to let go, lowercased (empty: none).
    skip: Mutex<String>,
    lock: Mutex<()>,
    wake: Condvar,
}

impl Control {
    pub fn quitting(&self) -> bool {
        self.quit.load(Ordering::SeqCst)
    }

    /// Quitting, or about to start over with new settings.
    pub fn stopping(&self) -> bool {
        self.quitting() || self.restart.load(Ordering::SeqCst)
    }

    pub fn quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    pub fn restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    /// Clears a pending restart (the new session has begun).
    pub fn restarted(&self) {
        self.restart.store(false, Ordering::SeqCst);
    }

    /// Waits up to `duration`; false when cut short by a stop.
    pub fn sleep(&self, duration: Duration) -> bool {
        let guard = self.lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = self.wake.wait_timeout_while(guard, duration, |_| !self.stopping());
        !self.stopping()
    }

    /// Wakes every `sleep()`.
    pub fn nudge(&self) {
        self.wake.notify_all();
    }

    /// Whether "Skip" asked to let this file go.
    pub fn skip_asked(&self, path: &str) -> bool {
        let skip = self.skip.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        !skip.is_empty() && *skip == path.to_lowercase()
    }

    /// It's been let go: the request is done.
    pub fn skipped(&self) {
        self.skip.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }

    fn ask_skip(&self, path: &str) {
        *self.skip.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = path.trim().to_lowercase();
    }
}

/// Listens for Excavate's signals on a thread of its own.
pub fn listen(control: &'static Control) {
    let (Some(quit), Some(settings), Some(skip)) = (winsys::process::Event::create(&config::indexer_quit_event()),
        winsys::process::Event::create(&config::indexer_settings_event()), winsys::process::Event::create(&config::indexer_skip_event())) else {
        config::log("can't create the indexer's events");
        return;
    };
    let paths = config::Paths::new();
    let _ = std::thread::Builder::new().name("signals".into()).spawn(move || {
        loop {
            match winsys::process::wait_any(&[&quit, &settings, &skip], None) {
                Some(0) => {
                    config::log("asked to quit");
                    control.quit();
                    return;
                }
                Some(1) => {
                    config::log("settings changed");
                    control.restart();
                }
                Some(2) => {
                    let path = std::fs::read_to_string(paths.skip()).unwrap_or_default();
                    config::log(&format!("asked to skip {}", path.trim()));
                    control.ask_skip(&path);
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
}
