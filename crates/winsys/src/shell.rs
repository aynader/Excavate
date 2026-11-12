//! What a result can do: open (as Explorer would), run as administrator, properties, Open with, show in its folder
//! with the file selected, and the clipboard (the file itself, or its path).

use crate::wide;
use std::path::Path;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OPENASINFO, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems, SHOpenWithDialog, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

pub type Result<T> = std::result::Result<T, String>;

fn hwnd(owner: isize) -> Option<HWND> {
    (owner != 0).then(|| HWND(owner as *mut _))
}

/// A shell verb: "open" (the default action, in the file's folder), "runas", "properties", "edit"…
pub fn verb(owner: isize, path: &str, verb: &str) -> Result<()> {
    let file = wide(path);
    let verb_text = wide(verb);
    let folder = Path::new(path).parent().map(|folder| wide(&folder.display().to_string()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | if verb == "properties" { SEE_MASK_INVOKEIDLIST } else { Default::default() },
        hwnd: hwnd(owner).unwrap_or_default(),
        lpVerb: PCWSTR(verb_text.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpDirectory: folder.as_ref().map(|folder| PCWSTR(folder.as_ptr())).unwrap_or(PCWSTR::null()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call.
    unsafe { ShellExecuteExW(&mut info) }.map_err(|error| error.message().to_string())
}

pub fn open(owner: isize, path: &str) -> Result<()> {
    verb(owner, path, "open")
}

/// Runs a program with administrator rights (Windows asks for approval), waits until it ends, and returns its exit code:
/// none if it's still running when `patience` runs out. Saying no comes back as an error.
pub fn run_elevated(path: &str, parameters: &str, patience: std::time::Duration) -> Result<Option<u32>> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Foundation::WAIT_TIMEOUT;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::SEE_MASK_NOCLOSEPROCESS;
    let (file, verb, parameters) = (wide(path), wide("runas"), wide(parameters));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: every string outlives the call; the process handle is ours to wait on and close.
    unsafe {
        ShellExecuteExW(&mut info).map_err(|error| {
            // ERROR_CANCELLED: the approval was declined.
            if error.code().0 as u32 & 0xFFFF == 1223 { "Windows' approval was declined.".to_string() } else { error.message().to_string() }
        })?;
        if info.hProcess.is_invalid() {
            return Ok(Some(0));
        }
        if WaitForSingleObject(info.hProcess, patience.as_millis().min(u32::MAX as u128 - 1) as u32) == WAIT_TIMEOUT {
            let _ = CloseHandle(info.hProcess);
            return Ok(None);
        }
        let mut code = 0u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        Ok(Some(code))
    }
}

/// Windows' "Open with" dialog.
pub fn open_with(owner: isize, path: &str) -> Result<()> {
    let file = wide(path);
    let info = OPENASINFO { pcszFile: PCWSTR(file.as_ptr()), pcszClass: PCWSTR::null(), oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC };
    // SAFETY: the name outlives the call.
    unsafe { SHOpenWithDialog(hwnd(owner), &info) }.map_err(|error| error.message().to_string())
}

/// Opens the folder in Explorer with the item selected (reusing an open window of that folder). COM must be ready.
pub fn reveal(path: &str) -> Result<()> {
    let file = wide(path);
    // SAFETY: the ID list is created from the path, used, and freed here.
    unsafe {
        let list = ILCreateFromPathW(PCWSTR(file.as_ptr()));
        if list.is_null() {
            return Err(format!("{path} isn't there"));
        }
        let result = SHOpenFolderAndSelectItems(list, None, 0);
        ILFree(Some(list));
        result.map_err(|error| error.message().to_string())
    }
}

/// Text on the clipboard.
pub fn copy_text(owner: isize, text: &str) -> Result<()> {
    const CF_UNICODETEXT: u32 = 13;
    let data = wide(text);
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
    set_clipboard(owner, &[(CF_UNICODETEXT, bytes.to_vec())])
}

/// The files themselves on the clipboard, to paste in Explorer, mail or chat (CF_HDROP, as Explorer's Copy).
pub fn copy_files(owner: isize, paths: &[String]) -> Result<()> {
    const CF_HDROP: u32 = 15;
    // DROPFILES: offset of the list (20), a point, fNC, fWide = 1; then NUL-separated paths and a final NUL.
    let mut drop = Vec::with_capacity(20 + paths.iter().map(|path| path.len() * 2 + 2).sum::<usize>() + 2);
    drop.extend_from_slice(&20u32.to_le_bytes());
    drop.extend_from_slice(&[0u8; 12]);
    drop.extend_from_slice(&1u32.to_le_bytes());
    for path in paths {
        for unit in path.encode_utf16().chain(std::iter::once(0)) {
            drop.extend_from_slice(&unit.to_le_bytes());
        }
    }
    drop.extend_from_slice(&[0, 0]);
    let effect_format = unsafe { RegisterClipboardFormatW(windows::core::w!("Preferred DropEffect")) };
    let copy = 1u32.to_le_bytes().to_vec(); // DROPEFFECT_COPY
    set_clipboard(owner, &[(CF_HDROP, drop), (effect_format, copy)])
}

fn set_clipboard(owner: isize, formats: &[(u32, Vec<u8>)]) -> Result<()> {
    // SAFETY: each block is sized for its data and written within it; the clipboard owns a block once it takes it.
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(hwnd(owner)).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10)); // another app has it open for a moment
        }
        if !opened {
            return Err("the clipboard is busy".into());
        }
        let _ = EmptyClipboard();
        let mut result = Ok(());
        for (format, data) in formats {
            let memory: HGLOBAL = match GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, data.len().max(1)) {
                Ok(memory) => memory,
                Err(error) => {
                    result = Err(error.message().to_string());
                    break;
                }
            };
            let start = GlobalLock(memory) as *mut u8;
            std::ptr::copy_nonoverlapping(data.as_ptr(), start, data.len());
            let _ = GlobalUnlock(memory);
            if SetClipboardData(*format, Some(HANDLE(memory.0))).is_err() {
                let _ = GlobalFree(Some(memory));
                result = Err("the clipboard didn't take it".into());
                break;
            }
        }
        let _ = CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::DataExchange::GetClipboardData;

    fn clipboard_text() -> String {
        // SAFETY: the clipboard's text, read while it's open.
        unsafe {
            OpenClipboard(None).unwrap();
            let pointer = GetClipboardData(13).unwrap().0 as *const u16;
            let mut length = 0;
            while *pointer.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
            let _ = CloseClipboard();
            text
        }
    }

    #[test]
    fn clipboard() {
        copy_text(0, "Excavate clipboard test ✓").unwrap();
        assert_eq!(clipboard_text(), "Excavate clipboard test ✓");
        let windows = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        copy_files(0, &[format!(r"{windows}\win.ini")]).unwrap();
        // SAFETY: reading the drop list back.
        unsafe {
            OpenClipboard(None).unwrap();
            let handle = GetClipboardData(15).unwrap();
            let mut name = [0u16; 260];
            let length = windows::Win32::UI::Shell::DragQueryFileW(windows::Win32::UI::Shell::HDROP(handle.0), 0, Some(&mut name));
            let _ = CloseClipboard();
            assert!(String::from_utf16_lossy(&name[..length as usize]).ends_with("win.ini"));
        }
    }
}
