//! Processes: who else is running, background priority for the indexer, the power and attention state it pauses
//! for, a job that ends the indexer with Excavate, and named events the two use to talk.

use crate::wide;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenEventW, OpenProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    ProcessPowerThrottling, SetEvent, SetPriorityClass, SetProcessInformation, TerminateProcess, WaitForMultipleObjects,
    EVENT_MODIFY_STATE,
};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::core::PCWSTR;

/// The executable name of a running process whose name contains `part` (case-insensitive), if any.
pub fn running(part: &str) -> Option<String> {
    let part = part.to_lowercase();
    // SAFETY: a snapshot of the process list, walked and closed here.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
                if name.to_lowercase().contains(&part) {
                    found = Some(name);
                    break;
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

/// The processes `parent` started that still run (their ids).
pub fn children(parent: u32) -> Vec<u32> {
    let mut found = Vec::new();
    // SAFETY: a snapshot of the process list, walked and closed here.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return found };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ParentProcessID == parent && entry.th32ProcessID != parent {
                    found.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

/// Every running process: its id and its file's name ("python.exe").
pub fn all() -> Vec<(u32, String)> {
    let mut all = Vec::new();
    // SAFETY: a snapshot of the process list, walked and closed here.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return all };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let length = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                all.push((entry.th32ProcessID, String::from_utf16_lossy(&entry.szExeFile[..length])));
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    all
}

/// A program's name as Windows shows it in Task Manager: its file's description ("Python", "Google Chrome"), or else
/// its file's name without ".exe". None when the process can't be asked.
pub fn friendly_name(process: u32) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    use windows::Win32::System::Threading::{PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    // SAFETY: the process is opened to ask for its file's path and closed; the version block is read within its size.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process).ok()?;
        let mut buffer = vec![0u16; 1024];
        let mut length = buffer.len() as u32;
        let asked = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, windows::core::PWSTR(buffer.as_mut_ptr()), &mut length);
        let _ = CloseHandle(handle);
        asked.ok()?;
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        let stem = std::path::Path::new(&path).file_stem().map(|stem| stem.to_string_lossy().into_owned());
        let wide_path = wide(&path);
        let size = GetFileVersionInfoSizeW(PCWSTR(wide_path.as_ptr()), None);
        if size == 0 {
            return stem;
        }
        let mut block = vec![0u8; size as usize];
        if GetFileVersionInfoW(PCWSTR(wide_path.as_ptr()), Some(0), size, block.as_mut_ptr().cast()).is_err() {
            return stem;
        }
        let (mut pointer, mut bytes) = (std::ptr::null_mut(), 0u32);
        let translation = wide(r"\VarFileInfo\Translation");
        let languages: Vec<(u16, u16)> = if VerQueryValueW(block.as_ptr().cast(), PCWSTR(translation.as_ptr()), &mut pointer, &mut bytes).as_bool() && bytes >= 4 {
            std::slice::from_raw_parts(pointer as *const u16, (bytes / 2) as usize).chunks(2).map(|pair| (pair[0], pair[1])).collect()
        } else {
            Vec::new()
        };
        // Its own language first, then US English in Unicode, which most files have.
        for (language, page) in languages.into_iter().chain([(0x0409, 0x04B0)]) {
            let key = wide(&format!(r"\StringFileInfo\{language:04x}{page:04x}\FileDescription"));
            if VerQueryValueW(block.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut pointer, &mut bytes).as_bool() && bytes > 1 {
                let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer as *const u16, bytes as usize));
                let text = text.trim_end_matches('\0').trim();
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
        stem
    }
}

/// Runs this process behind everything the person does, without slowing it down when the PC is free:
///
/// - **idle CPU priority**: any other program's threads go first, but idle cores (performance ones included) run it
///   at full speed. (Windows' "background mode" would also do this, but on hybrid CPUs it confines the process to
///   the efficiency cores: OCR measured 4 times slower.)
/// - **low memory priority**: the pages it reads are the first to make room, so the person's file cache stays;
/// - **efficiency mode off** on AC power (see `efficiency_mode`).
///
/// I/O priority is lowered per thread, for the threads that read files (`background_thread`).
pub fn run_in_background() {
    use windows::Win32::System::Threading::{IDLE_PRIORITY_CLASS, MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW, ProcessMemoryPriority};
    // SAFETY: changing our own process's priorities from well-formed structs.
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), IDLE_PRIORITY_CLASS);
        let memory = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_LOW };
        let _ = SetProcessInformation(GetCurrentProcess(), ProcessMemoryPriority, &memory as *const _ as *const _,
                                      std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32);
    }
    efficiency_mode(false);
}

/// How far ahead of other programs this process (or one of its model servers) runs: idle (behind everything the
/// person does: Normal), below normal (ahead of what's merely in the background: Fast), or normal (alongside the
/// person's apps: Full speed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
}

impl Priority {
    fn class(self) -> windows::Win32::System::Threading::PROCESS_CREATION_FLAGS {
        use windows::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS};
        match self {
            Priority::Idle => IDLE_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
        }
    }
}

/// This process's priority, and its memory's: at normal priority the pages it reads are kept like any app's, else
/// they're the first to make room (`run_in_background`).
pub fn set_priority(priority: Priority) {
    use windows::Win32::System::Threading::{MEMORY_PRIORITY_INFORMATION, MEMORY_PRIORITY_LOW, MEMORY_PRIORITY_NORMAL, ProcessMemoryPriority};
    let memory = MEMORY_PRIORITY_INFORMATION { MemoryPriority: if priority == Priority::Normal { MEMORY_PRIORITY_NORMAL } else { MEMORY_PRIORITY_LOW } };
    // SAFETY: our own process's priorities, from well-formed structs.
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), priority.class());
        let _ = SetProcessInformation(GetCurrentProcess(), ProcessMemoryPriority, &memory as *const _ as *const _,
                                      std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32);
    }
}

/// The same for another process of Excavate's (a model's server, the listener). False when its priority couldn't be
/// changed.
pub fn set_process_priority(process_id: u32, priority: Priority) -> bool {
    use windows::Win32::System::Threading::PROCESS_SET_INFORMATION;
    // SAFETY: a handle opened, used and closed here.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SET_INFORMATION, false, process_id) else { return false };
        let done = SetPriorityClass(process, priority.class()).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(process);
        done
    }
}

/// Lowest CPU and I/O priority for the calling thread (one that mostly reads files).
pub fn background_thread() {
    background_thread_as(true);
}

/// The calling thread in Windows' background mode (lowest disk and memory priority) or out of it again.
pub fn background_thread_as(on: bool) {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END};
    // SAFETY: our own thread.
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), if on { THREAD_MODE_BACKGROUND_BEGIN } else { THREAD_MODE_BACKGROUND_END });
    }
}

/// EcoQoS (efficient cores, lower clocks) on or off for this process: on battery, work costs less power; on AC power,
/// it would only make the work take longer.
pub fn efficiency_mode(on: bool) {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
    };
    // SAFETY: setting our own process's power throttling from a well-formed struct.
    unsafe {
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

/// Local time minus UTC now, in seconds.
pub fn utc_offset() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut zone = TIME_ZONE_INFORMATION::default();
    // SAFETY: filled by the call.
    let state = unsafe { GetTimeZoneInformation(&mut zone) };
    // 2: daylight saving is in effect.
    let bias = zone.Bias + if state == 2 { zone.DaylightBias } else { zone.StandardBias };
    -i64::from(bias) * 60
}

/// Whether Windows shows times as "2:30 PM" rather than "14:30": its short time format, which is what the taskbar's clock
/// follows.
pub fn twelve_hour_clock() -> bool {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SSHORTTIME};
    let mut format = [0u16; 80];
    // SAFETY: the buffer is passed with its length. Null: this user's own format.
    let length = unsafe { GetLocaleInfoEx(windows::core::PCWSTR::null(), LOCALE_SSHORTTIME, Some(&mut format)) };
    format_is_twelve_hour(&String::from_utf16_lossy(&format[..length.max(0) as usize]))
}

/// "h" is the 12-hour clock's hour in Windows' time formats, "H" the 24-hour's.
fn format_is_twelve_hour(format: &str) -> bool {
    format.contains('h')
}

/// Whether the PC runs on its battery now.
pub fn on_battery() -> bool {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: filled by the call.
    unsafe { GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0 }
}

/// Whether the person is in a game, a full-screen app or presenting: heavy background work waits.
pub fn person_busy() -> bool {
    // SAFETY: a plain query.
    match unsafe { SHQueryUserNotificationState() } {
        Ok(state) => state == QUNS_BUSY || state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE,
        Err(_) => false,
    }
}

/// Whether the person is presenting (PowerPoint's slide show, Windows' presentation mode).
pub fn presenting() -> bool {
    // SAFETY: a plain query.
    unsafe { SHQueryUserNotificationState() }.is_ok_and(|state| state == QUNS_PRESENTATION_MODE)
}

/// A job object that ends every process in it when Excavate exits (even if it crashes).
pub struct Job(HANDLE);

// SAFETY: a job handle can be used from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> Option<Job> {
        // SAFETY: an anonymous job object with the kill-on-close limit, owned by the returned value.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limits as *const _ as *const _,
                                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32).ok()?;
            Some(Job(job))
        }
    }

    /// Puts a child process in the job.
    pub fn add(&self, process_id: u32) -> bool {
        // SAFETY: opening the child to assign it, then closing our handle to it.
        unsafe {
            let Ok(process) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, process_id) else { return false };
            let ok = AssignProcessToJobObject(self.0, process).is_ok();
            let _ = CloseHandle(process);
            ok
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: our own handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A named auto-reset event (`Local\…`): one process waits on it, another signals it.
pub struct Event(HANDLE);

// SAFETY: event handles can be waited on and signalled from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    pub fn create(name: &str) -> Option<Event> {
        let name = wide(name);
        // SAFETY: a named event owned by the returned value.
        unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())).ok().map(Event) }
    }

    pub fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: our own handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Signals another process's named event; false when nobody created it (the other process isn't running).
pub fn signal(name: &str) -> bool {
    let name = wide(name);
    // SAFETY: opening, setting and closing a named event.
    unsafe {
        let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) else { return false };
        let ok = SetEvent(event).is_ok();
        let _ = CloseHandle(event);
        ok
    }
}

/// Waits for any of the events, up to `timeout` (None: forever). The index of the one signalled, or None on time out.
pub fn wait_any(events: &[&Event], timeout: Option<std::time::Duration>) -> Option<usize> {
    let handles: Vec<HANDLE> = events.iter().map(|event| event.0).collect();
    let milliseconds = timeout.map(|timeout| timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32).unwrap_or(u32::MAX);
    // SAFETY: waiting on handles we own for the duration of the call.
    let result = unsafe { WaitForMultipleObjects(&handles, false, milliseconds) };
    if result == WAIT_TIMEOUT {
        return None;
    }
    let index = result.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    (index < handles.len()).then_some(index)
}

#[cfg(test)]
mod tests {

    /// Another process's priority, below normal, normal and back to idle.
    #[test]
    fn another_process_priority() {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::{GetPriorityClass, PROCESS_QUERY_LIMITED_INFORMATION};
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"]).creation_flags(0x0800_0040)
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let class = |id: u32| unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id).unwrap();
            let class = GetPriorityClass(process);
            let _ = windows::Win32::Foundation::CloseHandle(process);
            class
        };
        assert_eq!(class(child.id()), 0x40, "idle to start with");
        assert!(super::set_process_priority(child.id(), super::Priority::BelowNormal));
        assert_eq!(class(child.id()), 0x4000, "below normal");
        assert!(super::set_process_priority(child.id(), super::Priority::Normal));
        assert_eq!(class(child.id()), 0x20, "normal");
        assert!(super::set_process_priority(child.id(), super::Priority::Idle));
        assert_eq!(class(child.id()), 0x40, "idle again");
        let _ = child.kill();
        let _ = child.wait();
    }
    use super::*;

    #[test]
    fn windows_time_formats() {
        assert!(format_is_twelve_hour("h:mm tt"));
        assert!(format_is_twelve_hour("tt h:mm"));
        assert!(!format_is_twelve_hour("HH:mm"));
        assert!(!format_is_twelve_hour("H:mm"));
        // This PC's, whichever it is.
        let _ = twelve_hour_clock();
    }

    #[test]
    fn named_events_cross() {
        let name = format!(r"Local\Excavate.Test.{}", std::process::id());
        let event = Event::create(&name).expect("created");
        let other = Event::create(&format!("{name}.other")).expect("created");
        assert_eq!(wait_any(&[&event, &other], Some(std::time::Duration::from_millis(10))), None);
        assert!(signal(&format!("{name}.other")));
        assert_eq!(wait_any(&[&event, &other], Some(std::time::Duration::from_secs(1))), Some(1));
        assert!(signal(&name));
        assert_eq!(wait_any(&[&event, &other], None), Some(0));
        assert!(!signal(r"Local\Excavate.Test.Nobody"));
    }

    /// What `run_elevated` waits with: the exit code once it ends, nothing while it's still running.
    #[test]
    fn waits_for_a_process() {
        use std::os::windows::io::IntoRawHandle;
        let handle = |child: std::process::Child| HANDLE(child.into_raw_handle());
        let ended = std::process::Command::new("cmd").args(["/c", "exit 3"]).spawn().unwrap();
        assert_eq!(wait_and_close(handle(ended), 30_000), Some(3));
        // (It ends by itself a second later.)
        let running = std::process::Command::new("cmd").args(["/c", "ping -n 2 127.0.0.1 >nul"]).spawn().unwrap();
        assert_eq!(wait_and_close(handle(running), 0), None);
    }

    #[test]
    fn names_a_program() {
        // This test's own process: its file's description, or its name.
        assert!(friendly_name(std::process::id()).is_some_and(|name| !name.is_empty()));
        assert!(all().iter().any(|(process, _)| *process == std::process::id()));
    }
}

/// Ends this process at once, once everything that matters is saved: Windows' usual ending runs every library's own
/// clean-up after the other threads are gone, and a graphics driver's can then wait forever on one of them (Intel's
/// D3D11 driver spins a core at 100%, past any "end task", for a device Windows' PDF or video code kept). Ending it
/// this way skips that clean-up; the system frees what the process held either way.
pub fn exit_now(code: u32) -> ! {
    // SAFETY: ends this process; nothing runs after it.
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), code);
    }
    std::process::exit(code as i32)
}

/// How a program started with Windows' administrator prompt ended (`run_elevated`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevated {
    /// It ran and ended with this exit code.
    Ended(u32),
    /// The person said no at Windows' prompt.
    Declined,
    /// It couldn't be started (Windows' error code).
    Failed(u32),
}

/// Starts `exe` with `args` through Windows' administrator prompt (UAC) and waits for it to end (call it off the UI
/// thread). Nothing is elevated without the person saying yes at Windows' own prompt.
pub fn run_elevated(exe: &str, args: &str) -> Elevated {
    use windows::Win32::System::Threading::INFINITE;
    match start_elevated(exe, args, false) {
        Err(error) => {
            let code = (error.code().0 as u32) & 0xFFFF;
            if code == ERROR_CANCELLED { Elevated::Declined } else { Elevated::Failed(code) }
        }
        Ok(None) => Elevated::Failed(0),
        Ok(Some(process)) => Elevated::Ended(wait_and_close(process, INFINITE).unwrap_or(0)),
    }
}

/// Windows' error when the person said "No" at its prompt.
pub(crate) const ERROR_CANCELLED: u32 = 1223;

/// Starts `exe` with `args` through Windows' administrator prompt: `shown`, in a window of its own, the call waiting
/// until Windows has handed it over (an installer); else hidden. Its process, when Windows gives one back, to
/// `wait_and_close`.
pub(crate) fn start_elevated(exe: &str, args: &str, shown: bool) -> windows::core::Result<Option<HANDLE>> {
    use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNORMAL};
    let (verb, file, parameters) = (wide("runas"), wide(exe), wide(args));
    let info = &mut SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: if shown { SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC } else { SEE_MASK_NOCLOSEPROCESS },
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: if shown { SW_SHOWNORMAL.0 } else { SW_HIDE.0 },
        ..Default::default()
    };
    // SAFETY: every string outlives the call; the process handle it returns is the caller's to wait on and close.
    unsafe { ShellExecuteExW(info)? };
    Ok((!info.hProcess.is_invalid()).then_some(info.hProcess))
}

/// Waits up to `milliseconds` (`INFINITE`: for as long as it takes) for a process to end, and closes its handle: its
/// exit code, or None when it's still running.
pub(crate) fn wait_and_close(process: HANDLE, milliseconds: u32) -> Option<u32> {
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    // SAFETY: the handle is ours, waited on and closed here.
    unsafe {
        if WaitForSingleObject(process, milliseconds) == WAIT_TIMEOUT {
            let _ = CloseHandle(process);
            return None;
        }
        let mut code = 0u32;
        let _ = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
        Some(code)
    }
}
