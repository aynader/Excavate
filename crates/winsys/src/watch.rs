//! Windows tells us about changes in a folder tree (ReadDirectoryChangesW): one thread per folder, each waiting in
//! the kernel until something changes. No polling. When changes come faster than they can be delivered, Windows
//! drops them and says so (`Change::Lost`), and the caller looks again some other way.

use crate::wide;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
};
use windows::Win32::System::IO::CancelIoEx;
use windows::core::PCWSTR;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Created, written or renamed to: look at it.
    Changed(PathBuf),
    /// Deleted or renamed away (a folder's contents go with it).
    Removed(PathBuf),
    /// Changes in this folder were dropped: look at the whole folder again.
    Lost(PathBuf),
}

struct Root {
    handle: isize,
}

/// Watches folder trees until dropped.
pub struct Watcher {
    roots: Vec<Root>,
    stop: Arc<AtomicBool>,
}

impl Watcher {
    /// Starts watching each folder and everything under it. `sink` gets each batch of changes, on the folder's own
    /// thread.
    pub fn start(folders: &[PathBuf], sink: impl Fn(Vec<Change>) + Send + Sync + 'static) -> Watcher {
        let sink: Arc<dyn Fn(Vec<Change>) + Send + Sync> = Arc::new(sink);
        let stop = Arc::new(AtomicBool::new(false));
        let mut roots = Vec::new();
        for folder in folders {
            let Some(handle) = open(folder) else { continue };
            roots.push(Root { handle });
            let (sink, stop, folder) = (sink.clone(), stop.clone(), folder.clone());
            let _ = std::thread::Builder::new().name("watch".into()).spawn(move || listen(handle, &folder, &*sink, &stop));
        }
        Watcher { roots, stop }
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for root in &self.roots {
            // SAFETY: cancels the blocked read on our handle; its thread then closes the handle and ends.
            unsafe {
                let _ = CancelIoEx(HANDLE(root.handle as *mut _), None);
            }
        }
    }
}

fn open(folder: &Path) -> Option<isize> {
    let path = wide(&folder.display().to_string());
    // SAFETY: opening a directory handle for change notifications (shared, so nothing else is blocked).
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .ok()
        .map(|handle| handle.0 as isize)
    }
}

fn listen(handle: isize, folder: &Path, sink: &(dyn Fn(Vec<Change>) + Send + Sync), stop: &AtomicBool) {
    let handle = HANDLE(handle as *mut _);
    // DWORD-aligned, as the API requires; 64 KB is the most a network share accepts.
    let mut buffer = vec![0u32; 16 * 1024];
    let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE;
    while !stop.load(Ordering::SeqCst) {
        let mut returned = 0u32;
        // SAFETY: a synchronous read into our buffer, which outlives the call.
        let result = unsafe {
            ReadDirectoryChangesW(handle, buffer.as_mut_ptr() as *mut _, (buffer.len() * 4) as u32, true, filter,
                                  Some(&mut returned), None, None)
        };
        if result.is_err() || stop.load(Ordering::SeqCst) {
            break;
        }
        if returned == 0 {
            sink(vec![Change::Lost(folder.to_path_buf())]);
            continue;
        }
        sink(parse(&buffer, returned as usize, folder));
    }
    // SAFETY: our own handle, no longer used.
    unsafe {
        let _ = CloseHandle(handle);
    }
}

fn parse(buffer: &[u32], length: usize, folder: &Path) -> Vec<Change> {
    let bytes = buffer.as_ptr() as *const u8;
    let mut changes = Vec::new();
    let mut offset = 0usize;
    loop {
        if offset + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() > length {
            break;
        }
        // SAFETY: Windows wrote a chain of FILE_NOTIFY_INFORMATION records into the first `length` bytes, each
        // DWORD-aligned; the name follows each header for FileNameLength bytes.
        let (next, action, name) = unsafe {
            let record = &*(bytes.add(offset) as *const FILE_NOTIFY_INFORMATION);
            let units = record.FileNameLength as usize / 2;
            let name = std::slice::from_raw_parts(record.FileName.as_ptr(), units);
            (record.NextEntryOffset as usize, record.Action, String::from_utf16_lossy(name))
        };
        let path = folder.join(&name);
        match action {
            FILE_ACTION_ADDED | FILE_ACTION_MODIFIED | FILE_ACTION_RENAMED_NEW_NAME => changes.push(Change::Changed(path)),
            FILE_ACTION_REMOVED | FILE_ACTION_RENAMED_OLD_NAME => changes.push(Change::Removed(path)),
            _ => {}
        }
        if next == 0 {
            break;
        }
        offset += next;
    }
    changes
}
