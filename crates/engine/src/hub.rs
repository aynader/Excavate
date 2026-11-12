//! Many folder watches on one thread: each folder is opened for overlapped `ReadDirectoryChangesExW`, all of them on
//! one I/O completion port, so a hundred watches cost one sleeping thread.
//!
//! Watching a drive as a few dozen pieces instead of one is what lets an overflow (Windows' change buffer filling up
//! before it's read) be repaired by listing just that piece again: a drive's root is watched for its own entries only,
//! each folder in it for everything inside, and a folder too big to list again quickly is split the same way.
//!
//! Changes come out as (watch, change) with paths relative to the watched folder; `Watches` keeps where each watched
//! folder is, so they can be put together with the folder's path when they're applied (a watched folder that's renamed
//! keeps its handle and goes on reporting).

use crate::watch::{Change, Facts};
use crate::walk::wide;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesExW, ReadDirectoryNotifyExtendedInformation, FILE_ACTION_ADDED,
    FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_EXTENDED_INFORMATION, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::{CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, PostQueuedCompletionStatus, OVERLAPPED};

/// What a watch reports.
pub enum Event {
    /// A change, its path relative to the watched folder.
    Change(Change),
    /// The watch is running: anything from now on is reported.
    Started,
    /// Windows' buffer overflowed: something was lost under this folder.
    Overflowed,
    /// The folder went (or couldn't be opened): the watch is over.
    Ended,
}

const WAKE: usize = usize::MAX;

struct Watch {
    handle: HANDLE,
    buffer: Vec<u64>,
    overlapped: Box<OVERLAPPED>,
    recursive: bool,
    closing: bool,
}

/// Requests to the hub's thread (which owns the watches).
enum Request {
    Add { id: u32, path: String, recursive: bool, kb: usize },
    Remove(u32),
    Stop,
}

pub struct Hub {
    port: usize,
    requests: Arc<Mutex<Vec<Request>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Hub {
    /// Starts the hub; `events` gets (watch id, event).
    pub fn start(events: crossbeam_channel::Sender<(u32, Event)>) -> Option<Hub> {
        let port = unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return None;
        }
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));
        let (raw, shared) = (port as usize, requests.clone());
        let thread = std::thread::Builder::new().name("watches".into()).spawn(move || run(raw, shared, events)).ok()?;
        Some(Hub { port: raw, requests, thread: Mutex::new(Some(thread)) })
    }

    /// Watches `path` (recursively, or its own entries only), with a change buffer of `kb` KB.
    pub fn add(&self, id: u32, path: &str, recursive: bool, kb: usize) {
        self.request(Request::Add { id, path: path.to_string(), recursive, kb });
    }

    pub fn remove(&self, id: u32) {
        self.request(Request::Remove(id));
    }

    fn request(&self, request: Request) {
        self.requests.lock().unwrap().push(request);
        unsafe { PostQueuedCompletionStatus(self.port as HANDLE, 0, WAKE, std::ptr::null()) };
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.request(Request::Stop);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
        unsafe { CloseHandle(self.port as HANDLE) };
    }
}

fn issue(watch: &mut Watch) -> bool {
    unsafe {
        *watch.overlapped = std::mem::zeroed();
        ReadDirectoryChangesExW(watch.handle, watch.buffer.as_mut_ptr() as *mut c_void, (watch.buffer.len() * 8) as u32,
                                watch.recursive as i32,
                                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE
                                    | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_ATTRIBUTES,
                                std::ptr::null_mut(), &mut *watch.overlapped, None, ReadDirectoryNotifyExtendedInformation) != 0
    }
}

fn run(port: usize, requests: Arc<Mutex<Vec<Request>>>, events: crossbeam_channel::Sender<(u32, Event)>) {
    let port = port as HANDLE;
    let mut watches: HashMap<u32, Watch> = HashMap::new();
    let send = |id: u32, event: Event| events.send((id, event)).is_ok();
    loop {
        let (mut bytes, mut key, mut overlapped) = (0u32, 0usize, std::ptr::null_mut::<OVERLAPPED>());
        let ok = unsafe { GetQueuedCompletionStatus(port, &mut bytes, &mut key, &mut overlapped, u32::MAX) } != 0;
        if key == WAKE {
            let pending: Vec<Request> = std::mem::take(&mut *requests.lock().unwrap());
            for request in pending {
                match request {
                    Request::Stop => {
                        for watch in watches.values_mut() {
                            watch.closing = true;
                            unsafe { CancelIoEx(watch.handle, std::ptr::null()) };
                        }
                        // Each cancelled read still completes: wait for them before the buffers go.
                        while !watches.is_empty() {
                            let (mut b, mut k, mut o) = (0u32, 0usize, std::ptr::null_mut::<OVERLAPPED>());
                            unsafe { GetQueuedCompletionStatus(port, &mut b, &mut k, &mut o, 2000) };
                            if o.is_null() && k != WAKE {
                                break;
                            }
                            if let Some(watch) = watches.remove(&(k as u32)) {
                                unsafe { CloseHandle(watch.handle) };
                            }
                        }
                        return;
                    }
                    Request::Remove(id) => {
                        if let Some(watch) = watches.get_mut(&id) {
                            watch.closing = true;
                            unsafe { CancelIoEx(watch.handle, std::ptr::null()) };
                        }
                    }
                    Request::Add { id, path, recursive, kb } => {
                        let name = wide(&path);
                        let handle = unsafe {
                            CreateFileW(name.as_ptr(), FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                        std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                                        std::ptr::null_mut())
                        };
                        if handle == INVALID_HANDLE_VALUE {
                            send(id, Event::Ended);
                            continue;
                        }
                        unsafe { CreateIoCompletionPort(handle, port, id as usize, 0) };
                        let mut watch = Watch { handle, buffer: vec![0u64; kb * 1024 / 8], overlapped: Box::new(unsafe { std::mem::zeroed() }),
                                                recursive, closing: false };
                        if issue(&mut watch) {
                            watches.insert(id, watch);
                            send(id, Event::Started);
                        } else {
                            unsafe { CloseHandle(handle) };
                            send(id, Event::Ended);
                        }
                    }
                }
            }
            continue;
        }
        let id = key as u32;
        let Some(watch) = watches.get_mut(&id) else { continue };
        if watch.closing || !ok {
            // Cancelled, or the folder went: the read completes with an error.
            let closing = watch.closing;
            if let Some(watch) = watches.remove(&id) {
                unsafe { CloseHandle(watch.handle) };
            }
            if !closing && !send(id, Event::Ended) {
                return;
            }
            continue;
        }
        if bytes == 0 {
            if !send(id, Event::Overflowed) {
                return;
            }
        } else {
            for change in parse(&watch.buffer, bytes as usize) {
                if !send(id, Event::Change(change)) {
                    return;
                }
            }
        }
        // For tests: a hub that falls behind, so Windows' buffers overflow.
        if let Some(ms) = std::env::var("ENGINE_HUB_STALL_MS").ok().and_then(|v| v.parse().ok()) {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
        if !issue(watch) {
            if let Some(watch) = watches.remove(&id) {
                unsafe { CloseHandle(watch.handle) };
            }
            if !send(id, Event::Ended) {
                return;
            }
        }
    }
}

/// The changes in a buffer Windows filled. A rename comes as an old name and a new one, but with many renames at once
/// in different folders the pairs interleave: they're matched by file id. An old name left unmatched (moved out, or
/// its new name in the next buffer) counts as removed, a new one as added: the end state is the same.
fn parse(buffer: &[u64], len: usize) -> Vec<Change> {
    let base = buffer.as_ptr() as *const u8;
    let mut offset = 0usize;
    let mut out: Vec<Option<Change>> = Vec::new();
    let mut olds: HashMap<i64, (usize, String)> = HashMap::new();
    while offset + std::mem::size_of::<FILE_NOTIFY_EXTENDED_INFORMATION>() <= len {
        // Windows puts each entry on a 4-byte boundary, and the entry holds 8-byte fields: it's copied out rather than
        // referred to in place (a misaligned reference; debug builds stop on it).
        let at = unsafe { base.add(offset) };
        let info = unsafe { std::ptr::read_unaligned(at as *const FILE_NOTIFY_EXTENDED_INFORMATION) };
        let name_at = unsafe { at.add(std::mem::offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileName)) } as *const u16;
        let name_length = (info.FileNameLength as usize / 2).min(len.saturating_sub(offset + std::mem::offset_of!(FILE_NOTIFY_EXTENDED_INFORMATION, FileName)) / 2);
        let name: Vec<u16> = (0..name_length).map(|i| unsafe { std::ptr::read_unaligned(name_at.add(i)) }).collect();
        let name = crate::walk::from_wide(&name);
        let meta = Facts::of(&info);
        let change = match info.Action {
            FILE_ACTION_ADDED => Some(Change::Added(name, Some(meta))),
            FILE_ACTION_REMOVED => Some(Change::Removed(name)),
            FILE_ACTION_MODIFIED => Some(Change::Modified(name, Some(meta))),
            FILE_ACTION_RENAMED_OLD_NAME => {
                olds.insert(info.FileId, (out.len(), name.clone()));
                Some(Change::Removed(name))
            }
            FILE_ACTION_RENAMED_NEW_NAME => match olds.remove(&info.FileId) {
                Some((at, from)) => {
                    out[at] = None;
                    Some(Change::Renamed(from, name))
                }
                None => Some(Change::Added(name, Some(meta))),
            },
            _ => None,
        };
        out.push(change);
        if info.NextEntryOffset == 0 {
            break;
        }
        offset += info.NextEntryOffset as usize;
    }
    out.into_iter().flatten().collect()
}
