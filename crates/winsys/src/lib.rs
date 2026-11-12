//! Excavate's Windows layer: the shell's verbs, icons and thumbnails as Explorer draws them, the clipboard, where the
//! taskbar is, the theme, the tray icon and the global hotkey. Everything here is plain Win32/COM through the `windows`
//! crate, so there is no runtime besides Windows itself.

pub mod audio;
pub mod dialog;
pub mod display;
pub mod files;
pub mod gpu;
pub mod hello;
pub mod http;
pub mod icons;
pub mod imaging;
pub mod layered;
pub mod load;
pub mod menu;
pub mod pdf;
pub mod process;
pub mod registry;
pub mod shell;
pub mod signature;
pub mod sound;
pub mod theme;
pub mod tray;
pub mod video;
pub mod watch;
pub mod window;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};

/// A NUL-terminated UTF-16 copy of `text`.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// COM for this thread, single-threaded apartment, as the shell's objects want it. Harmless to call again.
pub fn com_sta() {
    // SAFETY: initializes COM for the calling thread only.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
}

/// COM for this thread, multithreaded apartment (worker threads using WIC).
pub fn com_mta() {
    use windows::Win32::System::Com::COINIT_MULTITHREADED;
    // SAFETY: initializes COM for the calling thread only.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// Claims the name for this process: false when another process holds it (Excavate is running already).
pub fn single_instance(name: &str) -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    let name = wide(name);
    // SAFETY: a named mutex this process keeps (deliberately leaked) for its lifetime.
    unsafe {
        let handle = CreateMutexW(None, true, windows::core::PCWSTR(name.as_ptr()));
        let exists = GetLastError() == ERROR_ALREADY_EXISTS;
        std::mem::forget(handle);
        !exists
    }
}

/// Like `single_instance`, but waits up to `wait` for another to end first (a copy started by its update, while the
/// one it replaces quits).
pub fn single_instance_after(name: &str, wait: std::time::Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    let wide_name = wide(name);
    let started = std::time::Instant::now();
    loop {
        // SAFETY: the mutex is kept (leaked) once this process owns it; a handle to another's is closed.
        unsafe {
            if let Ok(handle) = CreateMutexW(None, true, windows::core::PCWSTR(wide_name.as_ptr())) {
                if GetLastError() != ERROR_ALREADY_EXISTS {
                    // (Kept open for the process's life: the handle is never closed.)
                    return true;
                }
                let _ = CloseHandle(handle);
            }
        }
        if started.elapsed() >= wait {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// This process's memory: (working set, private bytes).
pub fn process_memory() -> (u64, u64) {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
    // SAFETY: the counters struct is sized and filled by the call.
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                                     std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32);
    }
    (counters.WorkingSetSize as u64, counters.PrivateUsage as u64)
}
