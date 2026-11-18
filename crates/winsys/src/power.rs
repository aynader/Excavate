//! Keeping Windows awake while there's work it was asked to finish (a power request, as a video player or a download
//! makes one: the PC doesn't go to sleep on its own, the screen still turns off). Windows lets go of it when the
//! handle closes: on `Drop`, and when the process ends however it ends, so it can't outlive the program.

use crate::wide;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Power::{PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest};
use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
use windows::core::PWSTR;

/// A request to keep the system (not the screen) awake, on or off. Off when made, and when dropped.
pub struct KeepAwake {
    handle: HANDLE,
    on: bool,
}

// (The handle is a kernel object's, usable from any thread.)
unsafe impl Send for KeepAwake {}

impl KeepAwake {
    /// A request with the reason Windows shows for it (`powercfg /requests`). None when Windows refuses one.
    pub fn new(reason: &str) -> Option<KeepAwake> {
        let mut text = wide(reason);
        let context = REASON_CONTEXT {
            Version: 0, // POWER_REQUEST_CONTEXT_VERSION
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(text.as_mut_ptr()) },
        };
        // SAFETY: the context and its string live through the call (Windows copies the string).
        let handle = unsafe { PowerCreateRequest(&context) }.ok()?;
        Some(KeepAwake { handle, on: false })
    }

    /// Keeps the system awake (true) or lets it sleep again (false). Says whether it's now as asked.
    pub fn set(&mut self, on: bool) -> bool {
        if on == self.on {
            return true;
        }
        // SAFETY: a request handle owned by self.
        let done = unsafe {
            if on { PowerSetRequest(self.handle, PowerRequestSystemRequired) } else { PowerClearRequest(self.handle, PowerRequestSystemRequired) }
        };
        if done.is_ok() {
            self.on = on;
        }
        done.is_ok()
    }

    pub fn is_on(&self) -> bool {
        self.on
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.set(false);
        // SAFETY: closed once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_off_and_dropped() {
        let mut awake = KeepAwake::new("Excavate's test").expect("a power request");
        assert!(!awake.is_on());
        assert!(awake.set(true) && awake.is_on());
        assert!(awake.set(true), "asked twice: still on");
        assert!(awake.set(false) && !awake.is_on());
        assert!(awake.set(true));
        // Dropped while on: cleared and closed.
        drop(awake);
    }
}
