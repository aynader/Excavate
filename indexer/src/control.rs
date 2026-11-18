//! How Excavate tells the indexer what to do: named events, one to quit, one for "settings changed", one for "Skip" and
//! one for reading again what couldn't be read (`config::Retry`).
//! Every worker checks `stopping()` between files, and waits through `sleep()`, which a signal cuts short.
//!
//! "Settings changed" is taken in two ways. When only the pace changed (a pause, "Full speed", a kind's "everything
//! now": `Settings::same_reading`), the session running takes it in as it goes (`on_pace`): nothing is listed again,
//! and the downloads and model servers stay up. Anything else (folders, kinds, models) starts the session over with it.

use config::Settings;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// What a session does with a new pace: the settings as they are now.
pub type Pacer = Arc<dyn Fn(&Settings) + Send + Sync>;

/// What a session does with files to read again.
pub type Retrier = Arc<dyn Fn(Vec<config::Retry>) + Send + Sync>;

#[derive(Default)]
pub struct Control {
    quit: AtomicBool,
    restart: AtomicBool,
    /// Counts the changes of pace: a `sleep()` ends at one, so what waits looks again at once.
    paces: AtomicU64,
    /// The settings the session running reads by (None between sessions), and what it does with a new pace.
    reading: Mutex<Option<Settings>>,
    pacer: Mutex<Option<Pacer>>,
    /// The file "Skip" asked to let go, lowercased (empty: none).
    skip: Mutex<String>,
    /// What the session does with files to read again, and what was asked between sessions (done when the next starts).
    retrier: Mutex<Option<Retrier>>,
    retries: Mutex<Vec<config::Retry>>,
    lock: Mutex<()>,
    wake: Condvar,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        let _guard = locked(&self.lock);
        self.wake.notify_all();
    }

    pub fn restart(&self) {
        self.restart.store(true, Ordering::SeqCst);
        let _guard = locked(&self.lock);
        self.wake.notify_all();
    }

    /// Clears a pending restart (the new session has begun).
    pub fn restarted(&self) {
        self.restart.store(false, Ordering::SeqCst);
    }

    /// Waits up to `duration`, or until the pace changes; false when cut short by a stop.
    pub fn sleep(&self, duration: Duration) -> bool {
        let guard = locked(&self.lock);
        let paces = self.paces.load(Ordering::SeqCst);
        let _ = self.wake.wait_timeout_while(guard, duration, |_| !self.stopping() && self.paces.load(Ordering::SeqCst) == paces);
        !self.stopping()
    }

    /// A session starts, reading by `settings`: a change of pace alone is taken in as it goes from now on.
    pub fn begin(&self, settings: &Settings) {
        *locked(&self.reading) = Some(settings.clone());
    }

    /// The session has ended: until the next begins, any change starts it over.
    pub fn end(&self) {
        *locked(&self.reading) = None;
        *locked(&self.pacer) = None;
        *locked(&self.retrier) = None;
    }

    /// What the session does with files to read again; done at once with what was asked before it began.
    pub fn on_retry(&self, retrier: Retrier) {
        *locked(&self.retrier) = Some(retrier.clone());
        let waiting = std::mem::take(&mut *locked(&self.retries));
        if !waiting.is_empty() {
            retrier(waiting);
        }
    }

    /// The session is ending: what's asked from now on waits for the next one (and the session's own handler goes, with
    /// what it holds: the change follower's channel ends only once nothing else can send on it).
    pub fn stop_retrying(&self) {
        *locked(&self.retrier) = None;
    }

    /// Files to read again: to the session, or kept for the next.
    pub fn retry(&self, retries: Vec<config::Retry>) {
        if retries.is_empty() {
            return;
        }
        let retrier = locked(&self.retrier).clone();
        match retrier {
            Some(retrier) => retrier(retries),
            None => locked(&self.retries).extend(retries),
        }
        self.paced();
    }

    /// What the session does with a new pace; done at once with the latest (it may have changed since `begin`).
    pub fn on_pace(&self, pacer: Pacer) {
        *locked(&self.pacer) = Some(pacer.clone());
        let latest = locked(&self.reading).clone();
        if let Some(settings) = latest {
            pacer(&settings);
        }
    }

    /// The settings were saved again: taken in as reading goes when only the pace changed (true), else reading
    /// starts over with them.
    pub fn settings_changed(&self, settings: Settings) -> bool {
        let live = {
            let mut reading = locked(&self.reading);
            match reading.as_mut() {
                Some(current) if current.same_reading(&settings) => {
                    *current = settings.clone();
                    true
                }
                _ => false,
            }
        };
        if !live {
            self.restart();
            return false;
        }
        let pacer = locked(&self.pacer).clone();
        if let Some(pacer) = pacer {
            pacer(&settings);
        }
        self.paced();
        true
    }

    /// The pace changed: every `sleep()` ends, so what waits looks again.
    pub fn paced(&self) {
        self.paces.fetch_add(1, Ordering::SeqCst);
        let _guard = locked(&self.lock);
        self.wake.notify_all();
    }

    /// Wakes every `sleep()`.
    pub fn nudge(&self) {
        self.wake.notify_all();
    }

    /// Whether "Skip" asked to let this file go.
    pub fn skip_asked(&self, path: &str) -> bool {
        let skip = locked(&self.skip);
        !skip.is_empty() && *skip == path.to_lowercase()
    }

    /// It's been let go: the request is done.
    pub fn skipped(&self) {
        locked(&self.skip).clear();
    }

    fn ask_skip(&self, path: &str) {
        *locked(&self.skip) = path.trim().to_lowercase();
    }
}

/// Listens for Excavate's signals on a thread of its own.
pub fn listen(control: &'static Control) {
    let (Some(quit), Some(settings), Some(skip), Some(retry)) = (winsys::process::Event::create(&config::indexer_quit_event()),
        winsys::process::Event::create(&config::indexer_settings_event()), winsys::process::Event::create(&config::indexer_skip_event()),
        winsys::process::Event::create(&config::indexer_retry_event())) else {
        config::log("can't create the indexer's events");
        return;
    };
    let paths = config::Paths::new();
    // Asked while it wasn't running: the first session does it.
    control.retry(config::Retry::take(&paths));
    let _ = std::thread::Builder::new().name("signals".into()).spawn(move || {
        loop {
            match winsys::process::wait_any(&[&quit, &settings, &skip, &retry], None) {
                Some(0) => {
                    config::log("asked to quit");
                    control.quit();
                    return;
                }
                Some(1) => {
                    if control.settings_changed(config::Settings::load(&paths)) {
                        config::log("settings changed: the pace, taken in as reading goes");
                    } else {
                        config::log("settings changed: starting over");
                    }
                }
                Some(2) => {
                    let path = std::fs::read_to_string(paths.skip()).unwrap_or_default();
                    config::log(&format!("asked to skip {}", path.trim()));
                    control.ask_skip(&path);
                }
                Some(3) => {
                    let retries = config::Retry::take(&paths);
                    config::log(&format!("asked to read again: {}", retries.iter().map(config::Retry::line).collect::<Vec<_>>().join(", ").replace('\t', " ")));
                    control.retry(retries);
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI64;

    fn watched(control: &Control) -> Arc<AtomicI64> {
        let paused = Arc::new(AtomicI64::new(0));
        let seen = paused.clone();
        control.on_pace(Arc::new(move |settings: &Settings| seen.store(settings.paused_until, Ordering::SeqCst)));
        paused
    }

    #[test]
    fn a_pause_is_taken_in_without_starting_over() {
        let control = Control::default();
        let settings = Settings { folders: vec![r"C:\Docs".into()], ..Settings::default() };
        // Between sessions, anything starts over.
        assert!(!control.settings_changed(settings.clone()));
        assert!(control.stopping());
        control.restarted();
        control.begin(&settings);
        let paused = watched(&control);
        // A pause: taken in, no restart.
        assert!(control.settings_changed(Settings { paused_until: -1, ..settings.clone() }));
        assert!(!control.stopping());
        assert_eq!(paused.load(Ordering::SeqCst), -1);
        // Full speed, then resumed: the same.
        assert!(control.settings_changed(Settings { read_all_at: 9, look_all_at: 9, listen_all_at: 9, ..settings.clone() }));
        assert!(control.settings_changed(settings.clone()));
        assert_eq!(paused.load(Ordering::SeqCst), 0);
        assert!(!control.stopping());
        // A folder: starts over.
        assert!(!control.settings_changed(Settings { folders: vec![r"C:\Other".into()], ..settings.clone() }));
        assert!(control.stopping());
        control.end();
        control.restarted();
        assert!(!control.settings_changed(settings));
    }

    #[test]
    fn a_pace_set_before_the_session_listens_is_taken_in() {
        let control = Control::default();
        let settings = Settings::default();
        control.begin(&settings);
        assert!(control.settings_changed(Settings { paused_until: 77, ..settings }));
        assert_eq!(watched(&control).load(Ordering::SeqCst), 77);
    }

    #[test]
    fn what_to_read_again_waits_for_a_session() {
        let control = Control::default();
        let asked = Arc::new(Mutex::new(Vec::new()));
        control.retry(vec![config::Retry { why: config::why::DAMAGED.into(), path: None }]);
        let seen = asked.clone();
        control.on_retry(Arc::new(move |retries: Vec<config::Retry>| seen.lock().unwrap().extend(retries)));
        assert_eq!(asked.lock().unwrap().len(), 1, "what was asked between sessions, done as it begins");
        control.retry(vec![config::Retry { why: config::why::ONLINE.into(), path: Some(r"C:\a.docx".into()) }]);
        assert_eq!(asked.lock().unwrap().len(), 2);
        control.end();
        control.retry(vec![config::Retry { why: config::why::SKIPPED.into(), path: None }]);
        assert_eq!(asked.lock().unwrap().len(), 2, "kept for the next session");
    }

    #[test]
    fn a_change_of_pace_ends_a_sleep() {
        let control: &'static Control = Box::leak(Box::default());
        let started = std::time::Instant::now();
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            control.paced();
        });
        assert!(control.sleep(Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(10));
        waker.join().unwrap();
    }
}
