//! Acting on files as Explorer does: to the Recycle Bin and back again, renamed, and Windows' own right-click menu
//! (with its submenus, Send to and the like, which need the menu's messages passed on to it). COM must be ready on
//! the calling thread (`com_sta`).

use crate::wide;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, PROPERTYKEY, WPARAM};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, CMF_NORMAL, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, DefSubclassProc, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
    FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FOFX_RECYCLEONDELETE, FOLDERID_RecycleBinFolder, FileOperation, IContextMenu,
    IContextMenu3, IEnumShellItems, IFileOperation, IShellFolder, IShellItem, IShellItem2, ILFree, KF_FLAG_DEFAULT,
    RemoveWindowSubclass, SHBindToParent, SHCreateItemFromParsingName, SHGetKnownFolderItem, SHParseDisplayName,
    SIGDN_PARENTRELATIVEEDITING, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, PostMessageW, SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx,
    WM_NULL,
};
use windows::core::{GUID, Interface, PCSTR, PCWSTR};

pub type Result<T> = std::result::Result<T, String>;

fn text(error: windows::core::Error) -> String {
    error.message().to_string()
}

fn item(path: &str) -> Result<IShellItem> {
    let path = wide(path);
    // SAFETY: the path outlives the call.
    unsafe { SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None) }.map_err(text)
}

/// Sends `paths` to the Recycle Bin, as Explorer's Delete does, without asking first. Windows still asks before
/// deleting for good one that can't go there (too large, or a drive without a bin): never silently. Err when nothing
/// was done (or it was cancelled at that question).
pub fn recycle(owner: isize, paths: &[String]) -> Result<()> {
    // SAFETY: COM objects used here and dropped here.
    unsafe {
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(text)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE)
            .map_err(text)?;
        if owner != 0 {
            let _ = operation.SetOwnerWindow(HWND(owner as *mut _));
        }
        for path in paths {
            operation.DeleteItem(&item(path)?, None).map_err(text)?;
        }
        operation.PerformOperations().map_err(text)?;
        if operation.GetAnyOperationsAborted().map_err(text)?.as_bool() {
            return Err("it was cancelled".into());
        }
    }
    Ok(())
}

/// Where a file in the Recycle Bin was deleted from ("C:\Users\me\Documents"): System.Recycle.DeletedFrom.
const DELETED_FROM: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0x9b174b33_40ff_11d2_a27e_00c04fc30871), pid: 2 };

/// Brings `paths` back from the Recycle Bin to where they were (Undo after `recycle`). Returns how many came back; a
/// path taken again meanwhile (by another file of its name) is left in the bin.
pub fn restore(owner: isize, paths: &[String]) -> Result<usize> {
    let wanted: Vec<(String, String)> = paths.iter().filter_map(|path| {
        let path = std::path::Path::new(path);
        Some((path.parent()?.display().to_string().to_lowercase(), path.file_name()?.to_string_lossy().to_lowercase()))
    }).collect();
    // SAFETY: COM objects used here and dropped here; strings from the shell freed once read.
    unsafe {
        let bin: IShellItem = SHGetKnownFolderItem(&FOLDERID_RecycleBinFolder, KF_FLAG_DEFAULT, None).map_err(text)?;
        let items: IEnumShellItems = bin.BindToHandler(None, &BHID_EnumItems).map_err(text)?;
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(text)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO | FOF_SILENT | FOF_NOERRORUI).map_err(text)?;
        if owner != 0 {
            let _ = operation.SetOwnerWindow(HWND(owner as *mut _));
        }
        let mut found = 0;
        let mut taken = vec![false; wanted.len()];
        loop {
            let mut one = [None];
            let mut fetched = 0u32;
            if items.Next(&mut one, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(entry) = one[0].take() else { break };
            let Ok(entry2) = entry.cast::<IShellItem2>() else { continue };
            let Ok(from) = entry2.GetString(&DELETED_FROM) else { continue };
            let folder = from.to_string().unwrap_or_default().to_lowercase();
            CoTaskMemFree(Some(from.0 as *const _));
            let Ok(name) = entry.GetDisplayName(SIGDN_PARENTRELATIVEEDITING) else { continue };
            let original = name.to_string().unwrap_or_default();
            CoTaskMemFree(Some(name.0 as *const _));
            let key = (folder.trim_end_matches('\\').to_string(), original.to_lowercase());
            let Some(at) = wanted.iter().enumerate().position(|(n, w)| !taken[n] && w.0.trim_end_matches('\\') == key.0 && w.1 == key.1) else { continue };
            if std::path::Path::new(&paths[at]).exists() {
                continue;
            }
            let Ok(destination) = item(&from_folder(&paths[at])) else { continue };
            let new_name = wide(&original);
            if operation.MoveItem(&entry, &destination, PCWSTR(new_name.as_ptr()), None).is_ok() {
                taken[at] = true;
                found += 1;
            }
        }
        if found > 0 {
            operation.PerformOperations().map_err(text)?;
        }
        Ok(found)
    }
}

fn from_folder(path: &str) -> String {
    std::path::Path::new(path).parent().map(|folder| folder.display().to_string()).unwrap_or_default()
}

/// Gives `path` the name `name` in the same folder. Never over another file: Err when one has that name already.
/// Returns the new path.
pub fn rename(path: &str, name: &str) -> Result<String> {
    use windows::Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW};
    let name = name.trim();
    if name.is_empty() || name.ends_with('.') || name.ends_with(' ') || name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32) {
        return Err("A name can't have \\ / : * ? \" < > | in it, or end with a dot or a space.".into());
    }
    let to = std::path::Path::new(path).with_file_name(name).display().to_string();
    if to.eq_ignore_ascii_case(path) && to != path {
        // Only the case changes: Windows takes it as the same name.
    } else if std::path::Path::new(&to).exists() {
        return Err(format!("There's already a “{name}” in that folder."));
    }
    let (from, into) = (wide(path), wide(&to));
    // SAFETY: both paths outlive the call. No MOVEFILE_REPLACE_EXISTING: an existing file is never replaced.
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(into.as_ptr()), MOVE_FILE_FLAGS(0)) }.map_err(text)?;
    Ok(to)
}

thread_local! {
    /// The menu showing now, for its messages (`pass_on`).
    static SHOWING: RefCell<Option<IContextMenu3>> = const { RefCell::new(None) };
}

unsafe extern "system" fn pass_on(window: HWND, message: u32, w: WPARAM, l: LPARAM, _id: usize, _data: usize) -> LRESULT {
    const WM_DRAWITEM: u32 = 0x2B;
    const WM_MEASUREITEM: u32 = 0x2C;
    const WM_INITMENUPOPUP: u32 = 0x117;
    const WM_MENUCHAR: u32 = 0x120;
    if matches!(message, WM_DRAWITEM | WM_MEASUREITEM | WM_INITMENUPOPUP | WM_MENUCHAR) {
        let handled = SHOWING.with(|showing| {
            showing.borrow().as_ref().and_then(|menu| {
                let mut result = LRESULT(0);
                // SAFETY: the menu is alive while it shows; the message is the window's own.
                unsafe { menu.HandleMenuMsg2(message, w, l, Some(&mut result)) }.ok().map(|()| result)
            })
        });
        if let Some(result) = handled {
            return result;
        }
    }
    // SAFETY: the window's own message, on to what was there before.
    unsafe { DefSubclassProc(window, message, w, l) }
}

/// Windows' own right-click menu for `path` (what Explorer shows, its extensions' items too), at (x, y) on the screen,
/// owned by `owner`. Whatever is chosen is done by Windows. Ok(false) when nothing was chosen.
pub fn shell_menu(owner: isize, path: &str, x: i32, y: i32) -> Result<bool> {
    let window = HWND(owner as *mut _);
    let name = wide(path);
    // SAFETY: the ID list, the menu and the subclass are made, used and undone here, on the window's own thread.
    unsafe {
        let mut list: *mut ITEMIDLIST = std::ptr::null_mut();
        SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut list, 0, None).map_err(text)?;
        let mut last: *mut ITEMIDLIST = std::ptr::null_mut();
        let parent: std::result::Result<IShellFolder, _> = SHBindToParent(list, Some(&mut last));
        let result = (|| -> Result<bool> {
            let parent = parent.map_err(text)?;
            let menu: IContextMenu = parent.GetUIObjectOf(window, &[last as *const ITEMIDLIST], None).map_err(text)?;
            let popup = CreatePopupMenu().map_err(text)?;
            const FIRST: u32 = 1;
            const LAST: u32 = 0x7FFF;
            let _ = menu.QueryContextMenu(popup, 0, FIRST, LAST, CMF_NORMAL);
            SHOWING.with(|showing| *showing.borrow_mut() = menu.cast::<IContextMenu3>().ok());
            let subclassed = SetWindowSubclass(window, Some(pass_on), 0x4558_4341, 0).as_bool();
            let _ = SetForegroundWindow(window);
            let chosen = TrackPopupMenuEx(popup, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, x, y, window, None).0 as u32;
            let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
            if subclassed {
                let _ = RemoveWindowSubclass(window, Some(pass_on), 0x4558_4341);
            }
            SHOWING.with(|showing| showing.borrow_mut().take());
            let done = if chosen >= FIRST {
                let folder = wide(&from_folder(path));
                let info = CMINVOKECOMMANDINFOEX {
                    cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                    // CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE
                    fMask: 0x4000 | 0x2000_0000,
                    hwnd: window,
                    lpVerb: PCSTR((chosen - FIRST) as usize as *const u8),
                    lpVerbW: PCWSTR((chosen - FIRST) as usize as *const u16),
                    lpDirectoryW: PCWSTR(folder.as_ptr()),
                    nShow: SW_SHOWNORMAL.0,
                    ptInvoke: POINT { x, y },
                    ..Default::default()
                };
                menu.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO).map_err(text)?;
                true
            } else {
                false
            };
            let _ = DestroyMenu(popup);
            Ok(done)
        })();
        ILFree(Some(list));
        result
    }
}

/// Drags `paths` out of `owner`, as from Explorer: into another app, a folder or the desktop, with Windows' own picture
/// of them under the pointer. Returns once they're dropped (true) or the drag is cancelled (false). They're offered to
/// be copied or linked, never moved: a search's results stay where they are.
pub fn drag_out(owner: isize, paths: &[String]) -> Result<bool> {
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE, OleInitialize};
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHDoDragDrop};
    // SAFETY: the ID lists are made here and freed once the item array has its own; COM objects are dropped here.
    unsafe {
        // Usually done already on the UI thread (a second call says so and changes nothing).
        let _ = OleInitialize(None);
        let mut lists: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
        let mut failed = None;
        for path in paths {
            let name = wide(path);
            let mut list: *mut ITEMIDLIST = std::ptr::null_mut();
            match SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut list, 0, None) {
                Ok(()) => lists.push(list),
                Err(error) => {
                    failed = Some(text(error));
                    break;
                }
            }
        }
        let array: Option<std::result::Result<IShellItemArray, _>> = failed.is_none().then(|| {
            let shown: Vec<*const ITEMIDLIST> = lists.iter().map(|list| list.cast_const()).collect();
            SHCreateShellItemArrayFromIDLists(&shown)
        });
        for list in lists {
            CoTaskMemFree(Some(list.cast()));
        }
        if let Some(error) = failed {
            return Err(error);
        }
        let array = array.expect("made when nothing failed").map_err(text)?;
        let data: IDataObject = array.BindToHandler(None, &BHID_DataObject).map_err(text)?;
        let effect = SHDoDragDrop(Some(HWND(owner as *mut _)), &data, None, DROPEFFECT_COPY | DROPEFFECT_LINK).map_err(text)?;
        Ok(effect != DROPEFFECT_NONE)
    }
}
