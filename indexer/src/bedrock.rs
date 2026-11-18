//! Excavate Bedrock, the service (`engine::service` has what it does and why). This program is the service itself, and
//! its own installer:
//!
//! - started by Windows (no arguments): runs as the service until it's stopped, or until a minute passes with nothing
//!   asked of it (Excavate starts it again when it needs it);
//! - `--install`, run with administrator rights (Excavate asks Windows, once, when the person allows it): copies
//!   itself to `%ProgramFiles%\Excavate Bedrock` (a folder only administrators may change: a service's program must
//!   never be one the person's own programs could replace) and registers the service there, started on demand, which
//!   any signed-in user may start; run again, it replaces the program with this one (an update);
//! - `--remove`, the same way: stops the service, unregisters it and deletes its folder;
//! - `--console`: answers on the pipe from a console window (for testing, as an administrator).
#![windows_subsystem = "windows"]

use engine::service::{self, Pc, IDLE, PIPE, SERVICE};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, NO_ERROR};
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService, CreateServiceW, DeleteService, OpenSCManagerW,
    OpenServiceW, QueryServiceStatus, RegisterServiceCtrlHandlerExW, SetServiceObjectSecurity, SetServiceStatus,
    StartServiceCtrlDispatcherW, SC_HANDLE, SC_MANAGER_ALL_ACCESS, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONTROL_STOP, SERVICE_DEMAND_START, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL, SERVICE_NO_CHANGE, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS,
};

/// What people see in Services, under the name.
const DESCRIPTION: &str = "Reads your drives' file tables for Excavate's own file-name index (Bedrock): every folder, in \
                           seconds. Sends Excavate names, sizes and dates only, never what's in a file, and never another \
                           account's files. Starts when Excavate asks and stops when it's done.";
/// Who may do what with the service: SYSTEM and administrators everything; any signed-in user may start it and ask how
/// it is (so Excavate starts it without administrator rights), nothing else.
const SERVICE_SECURITY: &str = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWRPLOCRRC;;;AU)";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// `%ProgramFiles%\Excavate Bedrock`.
fn folder() -> PathBuf {
    let programs = std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles")).unwrap_or_else(|| r"C:\Program Files".into());
    PathBuf::from(programs).join(service::DISPLAY_NAME)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a.eq_ignore_ascii_case(flag));
    let code = if has("--install") {
        report(install())
    } else if has("--remove") {
        report(remove())
    } else if has("--console") {
        let stop = Arc::new(AtomicBool::new(false));
        report(service::serve(Arc::new(Pc), PIPE, Duration::from_secs(24 * 3600), stop))
    } else {
        run_as_service()
    };
    std::process::exit(code);
}

/// An exit code for Excavate (which waits on this program): 0, or 1 with the reason written beside the program's
/// copy for Excavate to read (`bedrock-error.txt` in the temporary folder).
fn report(result: Result<(), String>) -> i32 {
    let note = std::env::temp_dir().join("excavate-bedrock-error.txt");
    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(note);
            0
        }
        Err(why) => {
            let _ = std::fs::write(note, &why);
            1
        }
    }
}

// ---- As a service ----

static STATUS: OnceLock<usize> = OnceLock::new();
static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn set_state(state: u32) {
    let Some(&handle) = STATUS.get() else { return };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP } else { 0 },
        dwWin32ExitCode: NO_ERROR,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_RUNNING { 0 } else { 5000 },
    };
    unsafe { SetServiceStatus(handle as SERVICE_STATUS_HANDLE, &status) };
}

unsafe extern "system" fn control(code: u32, _kind: u32, _data: *mut core::ffi::c_void, _context: *mut core::ffi::c_void) -> u32 {
    if code == SERVICE_CONTROL_STOP {
        set_state(SERVICE_STOP_PENDING);
        if let Some(stop) = STOP.get() {
            stop.store(true, Ordering::SeqCst);
            // The waiting connect is woken by connecting to it.
            let _ = std::fs::OpenOptions::new().read(true).write(true).open(PIPE);
        }
    }
    NO_ERROR
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    let name = wide(SERVICE);
    let handle = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), std::ptr::null()) };
    if handle.is_null() {
        return;
    }
    let _ = STATUS.set(handle as usize);
    let stop = Arc::new(AtomicBool::new(false));
    let _ = STOP.set(stop.clone());
    set_state(SERVICE_START_PENDING);
    set_state(SERVICE_RUNNING);
    let _ = service::serve(Arc::new(Pc), PIPE, IDLE, stop);
    set_state(SERVICE_STOPPED);
}

fn run_as_service() -> i32 {
    let name = wide(SERVICE);
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: name.as_ptr() as *mut u16, lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: std::ptr::null_mut(), lpServiceProc: None },
    ];
    // Not started by Windows (double-clicked): nothing to do.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 { 2 } else { 0 }
}

// ---- Installing and removing ----

struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

fn manager() -> Result<Handle, String> {
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if manager.is_null() {
        let code = unsafe { GetLastError() };
        return Err(if code == 5 { "it needs administrator rights".into() } else { format!("couldn't reach Windows' services (error {code})") });
    }
    Ok(Handle(manager))
}

/// Stops the service if it runs, waiting up to 20 s for it to end.
fn stop(service: &Handle) {
    unsafe {
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        if QueryServiceStatus(service.0, &mut status) == 0 || status.dwCurrentState == SERVICE_STOPPED {
            return;
        }
        ControlService(service.0, SERVICE_CONTROL_STOP, &mut status);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            if QueryServiceStatus(service.0, &mut status) == 0 || status.dwCurrentState == SERVICE_STOPPED {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Copies `from` over `to`, a few times if the old program is still letting go.
fn copy_in(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..25 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(error) => last = error.to_string(),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!("couldn't copy it into {} ({last})", to.display()))
}

fn install() -> Result<(), String> {
    let manager = manager()?;
    let here = std::env::current_exe().map_err(|e| e.to_string())?;
    let folder = folder();
    let program = folder.join("excavate-bedrock.exe");
    let name = wide(SERVICE);
    // Already there: stopped, so its program can be replaced.
    let existing = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_ALL_ACCESS) };
    let existing = (!existing.is_null()).then(|| Handle(existing));
    if let Some(service) = &existing {
        stop(service);
    }
    std::fs::create_dir_all(&folder).map_err(|e| format!("couldn't make {} ({e})", folder.display()))?;
    let same = std::fs::canonicalize(&here).ok() == std::fs::canonicalize(&program).ok();
    if !same {
        copy_in(&here, &program)?;
    }
    let command = wide(&format!("\"{}\"", program.display()));
    let service = match existing {
        Some(service) => {
            let ok = unsafe {
                ChangeServiceConfigW(service.0, SERVICE_WIN32_OWN_PROCESS, SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, command.as_ptr(),
                                     std::ptr::null(), std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(),
                                     wide(service::DISPLAY_NAME).as_ptr())
            };
            if ok == 0 {
                return Err(format!("couldn't change the service (error {})", unsafe { GetLastError() }));
            }
            let _ = SERVICE_NO_CHANGE;
            service
        }
        None => {
            // LocalSystem (no account given): only it may read a volume raw.
            let made = unsafe {
                CreateServiceW(manager.0, name.as_ptr(), wide(service::DISPLAY_NAME).as_ptr(), SERVICE_ALL_ACCESS, SERVICE_WIN32_OWN_PROCESS,
                               SERVICE_DEMAND_START, SERVICE_ERROR_NORMAL, command.as_ptr(), std::ptr::null(), std::ptr::null_mut(),
                               std::ptr::null(), std::ptr::null(), std::ptr::null())
            };
            if made.is_null() {
                return Err(format!("couldn't register the service (error {})", unsafe { GetLastError() }));
            }
            Handle(made)
        }
    };
    let mut description = wide(DESCRIPTION);
    let info = SERVICE_DESCRIPTIONW { lpDescription: description.as_mut_ptr() };
    unsafe { ChangeServiceConfig2W(service.0, SERVICE_CONFIG_DESCRIPTION, &info as *const _ as *const _) };
    unsafe {
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(SERVICE_SECURITY).as_ptr(), 1, &mut descriptor, std::ptr::null_mut()) == 0 {
            return Err("couldn't set who may start it".into());
        }
        let ok = SetServiceObjectSecurity(service.0, DACL_SECURITY_INFORMATION, descriptor);
        windows_sys::Win32::Foundation::LocalFree(descriptor as *mut _);
        if ok == 0 {
            return Err(format!("couldn't set who may start it (error {})", GetLastError()));
        }
    }
    Ok(())
}

fn remove() -> Result<(), String> {
    let manager = manager()?;
    let name = wide(SERVICE);
    let service = unsafe { OpenServiceW(manager.0, name.as_ptr(), SERVICE_ALL_ACCESS) };
    if !service.is_null() {
        let service = Handle(service);
        stop(&service);
        if unsafe { DeleteService(service.0) } == 0 {
            let code = unsafe { GetLastError() };
            // (Already marked for removal: 1072.)
            if code != 1072 {
                return Err(format!("couldn't remove the service (error {code})"));
            }
        }
    }
    let folder = folder();
    let here = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    let inside = here.as_ref().is_some_and(|p| std::fs::canonicalize(&folder).is_ok_and(|f| p.starts_with(f)));
    if inside {
        // Run from the folder itself: it goes once this program has ended.
        let script = format!("ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", folder.display());
        let _ = std::process::Command::new("cmd").args(["/c", &script]).spawn();
    } else if folder.exists() {
        let mut last = Ok(());
        for _ in 0..25 {
            last = std::fs::remove_dir_all(&folder);
            if last.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        last.map_err(|e| format!("couldn't delete {} ({e})", folder.display()))?;
    }
    Ok(())
}
