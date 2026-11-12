//! Bedrock: a file-name index of Excavate's own, measured against Everything.
pub mod cases;
pub mod coded;
pub mod fuzzy;
pub mod column;
pub mod hub;
pub mod ids;
pub mod journal;
pub mod known;
pub mod mft;
pub mod names;
pub mod search;
pub mod store;
pub mod syntax;
pub mod walk;
pub mod watch;
pub mod when;

/// This process's private memory, in bytes.
pub fn private_bytes() -> usize {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(GetCurrentProcess(), &mut counters as *mut _ as *mut _, counters.cb);
        counters.PrivateUsage
    }
}

/// This process's CPU time (user and kernel), in seconds.
pub fn cpu_seconds() -> f64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let mut t = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        GetProcessTimes(GetCurrentProcess(), &mut t[0], &mut t[1], &mut t[2], &mut t[3]);
        let f = |t: &FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e7;
        f(&t[2]) + f(&t[3])
    }
}
