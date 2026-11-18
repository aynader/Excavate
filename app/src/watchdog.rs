//! Notices when the UI thread stops answering (a second or more), and logs how long: with `with_app`'s own timing,
//! that says what held it. A thread asks the event loop for a reply twice a second; it costs nothing measurable.

use crate::settings::log;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_millis(500);
const LATE: Duration = Duration::from_secs(1);

pub fn start() {
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(|| {
        loop {
            std::thread::sleep(EVERY);
            let (sender, receiver) = channel();
            let asked = Instant::now();
            if slint::invoke_from_event_loop(move || {
                let _ = sender.send(());
            })
            .is_err()
            {
                return;
            }
            match receiver.recv_timeout(LATE) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            // Late: wait for the answer, however long, then say how long it was.
            if receiver.recv().is_err() {
                return;
            }
            log(&format!("the UI thread didn't answer for {} ms", asked.elapsed().as_millis()));
        }
    });
}
