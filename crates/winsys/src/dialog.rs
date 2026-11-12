//! Windows' own folder picker (the Explorer-style one, with Quick access and search).

use crate::wide;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::core::PCWSTR;

/// Asks for a folder. None when the person cancels. Call on a COM (STA) thread: the UI thread.
pub fn pick_folder(owner: isize, title: &str) -> Option<String> {
    // SAFETY: the dialog is created, shown modally over `owner`, read and released here.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let title = wide(title);
        let _ = dialog.SetTitle(PCWSTR(title.as_ptr()));
        let owner = (owner != 0).then(|| HWND(owner as *mut _));
        dialog.Show(owner).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}
