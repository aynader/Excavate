//! What a process costs, seen from outside: its CPU time and private memory, or a whole tree's through a job object
//! (the indexer and the model servers it starts).

use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

/// The ids of the running processes with this file name ("Everything.exe"), any case.
pub fn pids(name: &str) -> Vec<u32> {
    let mut found = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..length]).eq_ignore_ascii_case(name) {
                found.push(entry.th32ProcessID);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    found
}

/// The Windows session a process runs in: 0 for services, the signed-in person's otherwise.
pub fn session(pid: u32) -> Option<u32> {
    let mut session = 0;
    (unsafe { windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId(pid, &mut session) } != 0).then_some(session)
}

fn seconds(time: &FILETIME) -> f64 {
    ((time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64) as f64 / 1e7
}

/// A process's CPU time (user and kernel), in seconds, and its private memory, in bytes.
pub fn cost(pid: u32) -> Option<(f64, u64)> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut t = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
        let timed = GetProcessTimes(process, &mut t[0], &mut t[1], &mut t[2], &mut t[3]) != 0;
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let sized = GetProcessMemoryInfo(process, &mut counters as *mut _ as *mut _, counters.cb) != 0;
        CloseHandle(process);
        (timed && sized).then(|| (seconds(&t[2]) + seconds(&t[3]), counters.PrivateUsage as u64))
    }
}

/// A job holding a process and everything it starts; closed (and so ended) with this one.
pub struct Job(HANDLE);

unsafe impl Send for Job {}

impl Job {
    pub fn new() -> Job {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as *const _,
                                    std::mem::size_of_val(&limits) as u32);
            Job(job)
        }
    }

    pub fn add(&self, child: &std::process::Child) -> bool {
        unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) != 0 }
    }

    /// The CPU time of every process in it so far, ended ones too, in seconds.
    pub fn cpu_seconds(&self) -> f64 {
        unsafe {
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
            QueryInformationJobObject(self.0, JobObjectBasicAccountingInformation, &mut accounting as *mut _ as *mut _,
                                      std::mem::size_of_val(&accounting) as u32, std::ptr::null_mut());
            (accounting.TotalUserTime + accounting.TotalKernelTime) as f64 / 1e7
        }
    }

    /// The most memory its processes held at once (committed), in bytes.
    pub fn peak_bytes(&self) -> u64 {
        unsafe {
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            QueryInformationJobObject(self.0, JobObjectExtendedLimitInformation, &mut limits as *mut _ as *mut _,
                                      std::mem::size_of_val(&limits) as u32, std::ptr::null_mut());
            limits.PeakJobMemoryUsed as u64
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
