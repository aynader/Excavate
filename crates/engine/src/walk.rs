//! The way in without admin rights: list every folder, many at once. Each folder is one handle and a few
//! `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` calls of 64 KB, which return names, sizes, dates and
//! attributes together (no call per file). Workers list, one builder thread owns the index and hands out folders.
//! Junctions and symbolic links are kept as entries but not followed, as Everything does.

use crate::store::{seconds, Index, Meta, DIR, HIDDEN, NONE, REPARSE, SYSTEM};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};

pub struct Found {
    pub name: String,
    pub meta: Meta,
    pub file_id: i64,
    /// When it was made and last opened, and every attribute (`dc:`, `da:`, `attrib:`).
    pub more: crate::store::More,
}

/// A name from Windows, kept whole: NTFS names are UTF-16 that needn't be valid, and lossy decoding would turn an
/// unpaired surrogate into U+FFFD, merging names and losing the way back to the file. Instead an unpaired surrogate
/// becomes U+10FFFF (a noncharacter, used as an escape) followed by U+10F000 + (surrogate - 0xD800), and a real
/// U+10FFFF becomes two of them. Every other name is its plain UTF-8; `to_wide` undoes it exactly.
pub fn from_wide(name: &[u16]) -> String {
    let mut out = String::with_capacity(name.len());
    for c in char::decode_utf16(name.iter().copied()) {
        match c {
            Ok(ESCAPE) => {
                out.push(ESCAPE);
                out.push(ESCAPE);
            }
            Ok(c) => out.push(c),
            Err(e) => {
                out.push(ESCAPE);
                out.push(char::from_u32(0x10F000 + (e.unpaired_surrogate() as u32 - 0xD800)).unwrap());
            }
        }
    }
    out
}

const ESCAPE: char = '\u{10FFFF}';

/// The UTF-16 Windows gets for a name or path (see `from_wide`).
pub fn to_wide(text: &str) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        let c = if c == ESCAPE {
            match chars.next() {
                Some(ESCAPE) | None => ESCAPE,
                Some(code) => {
                    out.push((code as u32 - 0x10F000 + 0xD800) as u16);
                    continue;
                }
            }
        } else {
            c
        };
        let mut buf = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut buf));
    }
    out
}

pub fn wide(path: &str) -> Vec<u16> {
    // \\?\ lifts the 260-character limit and keeps names Windows would otherwise trim (trailing dots and spaces).
    // Past \\?\ Windows takes the path as it is, so forward slashes must become backslashes first.
    let path = path.replace('/', "\\");
    let long = if path.starts_with(r"\\?\") { path } else { format!(r"\\?\{path}") };
    let mut wide = to_wide(&long);
    wide.push(0);
    wide
}

pub fn flags(attributes: u32) -> u8 {
    let mut flags = 0;
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 { flags |= DIR; }
    if attributes & FILE_ATTRIBUTE_HIDDEN != 0 { flags |= HIDDEN; }
    if attributes & FILE_ATTRIBUTE_SYSTEM != 0 { flags |= SYSTEM; }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 { flags |= REPARSE; }
    flags
}

/// One folder's entries, or the Windows error that stopped it.
pub fn list(path: &str) -> Result<Vec<Found>, u32> {
    let wide = wide(path);
    unsafe {
        let handle = CreateFileW(wide.as_ptr(), FILE_LIST_DIRECTORY, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                 std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
        if handle == INVALID_HANDLE_VALUE {
            return Err(GetLastError());
        }
        let mut out = Vec::new();
        // u64 for the alignment the records need.
        let mut buffer = vec![0u64; 64 * 1024 / 8];
        let mut class = FileIdBothDirectoryRestartInfo;
        let mut error = 0;
        loop {
            let ok = GetFileInformationByHandleEx(handle, class, buffer.as_mut_ptr() as *mut c_void, (buffer.len() * 8) as u32);
            class = FileIdBothDirectoryInfo;
            if ok == 0 {
                let e = GetLastError();
                if e != ERROR_NO_MORE_FILES {
                    error = e;
                }
                break;
            }
            let base = buffer.as_ptr() as *const u8;
            let mut offset = 0usize;
            loop {
                let info = &*(base.add(offset) as *const FILE_ID_BOTH_DIR_INFO);
                let name_ptr = std::ptr::addr_of!(info.FileName) as *const u16;
                let name = std::slice::from_raw_parts(name_ptr, info.FileNameLength as usize / 2);
                if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                    let is_dir = info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                    out.push(Found {
                        name: from_wide(name),
                        meta: Meta { size: if is_dir { None } else { Some(info.EndOfFile as u64) },
                                     modified: seconds(info.LastWriteTime), flags: flags(info.FileAttributes) },
                        file_id: info.FileId,
                        more: crate::store::More { created: seconds(info.CreationTime), accessed: seconds(info.LastAccessTime),
                                                   attributes: info.FileAttributes },
                    });
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                offset += info.NextEntryOffset as usize;
            }
        }
        CloseHandle(handle);
        if error != 0 && out.is_empty() { Err(error) } else { Ok(out) }
    }
}

/// One file's own facts, from the file itself (what the folder listing holds can lag behind them).
pub fn stat(path: &str) -> Option<Meta> {
    stat_facts(path).map(|facts| facts.meta)
}

/// `stat`, with the dates made and opened and the attributes.
pub fn stat_facts(path: &str) -> Option<crate::watch::Facts> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA};
    let wide = wide(path);
    unsafe {
        let mut data: WIN32_FILE_ATTRIBUTE_DATA = std::mem::zeroed();
        if GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, &mut data as *mut _ as *mut c_void) == 0 {
            return None;
        }
        let is_dir = data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let size = (data.nFileSizeHigh as u64) << 32 | data.nFileSizeLow as u64;
        let time = |t: windows_sys::Win32::Foundation::FILETIME| (t.dwHighDateTime as i64) << 32 | t.dwLowDateTime as i64;
        Some(crate::watch::Facts {
            meta: Meta { size: if is_dir { None } else { Some(size) }, modified: seconds(time(data.ftLastWriteTime)),
                         flags: flags(data.dwFileAttributes) },
            more: crate::store::More { created: seconds(time(data.ftCreationTime)),
                                       accessed: seconds(time(data.ftLastAccessTime)), attributes: data.dwFileAttributes },
        })
    }
}

pub fn join(folder: &str, name: &str) -> String {
    if folder.ends_with('\\') { format!("{folder}{name}") } else { format!("{folder}\\{name}") }
}

#[derive(Default, Debug)]
pub struct Walked {
    pub folders: usize,
    /// Folders that couldn't be listed (access denied, mostly), by Windows error.
    pub refused: std::collections::BTreeMap<u32, usize>,
    pub refused_paths: Vec<String>,
    /// Stopped before the end (`walk_until`): what's in the index is part of it only.
    pub stopped: bool,
}

/// Adds `root` and everything under it to `index`, with `threads` listing at once.
pub fn walk(index: &mut Index, root: &str, threads: usize) -> Walked {
    walk_until(index, root, threads, &std::sync::atomic::AtomicBool::new(false))
}

/// `walk`, given up as soon as `stop` is set (switched off meanwhile).
pub fn walk_until(index: &mut Index, root: &str, threads: usize, stop: &std::sync::atomic::AtomicBool) -> Walked {
    use std::sync::atomic::Ordering::Relaxed;
    let root = root.trim_end_matches('\\');
    let root_name = root.to_string();
    let top = index.push(NONE, &root_name, Meta { size: None, modified: 0, flags: DIR });
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<(String, u32)>();
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<(String, u32, Result<Vec<Found>, u32>)>();
    let mut walked = Walked::default();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let jobs = job_rx.clone();
            let done = done_tx.clone();
            scope.spawn(move || {
                lister_thread();
                for (path, entry) in jobs {
                    if stop.load(Relaxed) {
                        break;
                    }
                    let listed = list(&path);
                    if done.send((path, entry, listed)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        let first = if root.ends_with(':') { format!("{root}\\") } else { root.to_string() };
        job_tx.send((first, top)).unwrap();
        let mut pending = 1usize;
        while pending > 0 {
            if stop.load(Relaxed) {
                walked.stopped = true;
                break;
            }
            let Ok((path, parent, listed)) = done_rx.recv() else { break };
            pending -= 1;
            walked.folders += 1;
            match listed {
                Err(error) => {
                    *walked.refused.entry(error).or_default() += 1;
                    walked.refused_paths.push(path);
                }
                Ok(found) => {
                    for item in found {
                        let follow = item.meta.flags & DIR != 0 && item.meta.flags & REPARSE == 0;
                        let child_path = if follow { Some(join(&path, &item.name)) } else { None };
                        let entry = index.push(parent, &item.name, item.meta);
                        index.set_more(entry, item.more);
                        if let Some(child_path) = child_path {
                            job_tx.send((child_path, entry)).unwrap();
                            pending += 1;
                        }
                    }
                }
            }
        }
        drop(job_tx);
    });
    index.rank_exts();
    index.build_table();
    walked
}

static BACKGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Entries listed by `reconcile_many` since this process started: how far a drive listed again has come (Activity).
static LISTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Entries listed again so far (see `LISTED`).
pub fn listed() -> usize {
    LISTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Lists at background priority from now on (CPU and disk), as Excavate does: a walk then gives way to everything
/// else the PC is doing. Off for measuring.
pub fn set_background(on: bool) {
    BACKGROUND.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn lister_thread() {
    if BACKGROUND.load(std::sync::atomic::Ordering::Relaxed) {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN};
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }
}

#[derive(Default, Debug, Clone)]
pub struct Reconciled {
    pub folders: usize,
    pub refused: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// Stopped before the end (`reconcile_until`): nothing was taken out for not being listed.
    pub stopped: bool,
}

/// Lists `path` (whose entry is `top`) again: everything under it when `deep`, else its own entries. Then the index
/// agrees with what's there: what's listed and missing is added, what changed is updated, what's in the index and
/// wasn't listed goes (but not inside a folder that couldn't be listed). Folders are listed in parallel without the
/// lock; each listing is applied under the write lock on its own, so a search waits a moment at most. The caller is
/// the only one changing the index meanwhile.
pub fn reconcile(lock: &std::sync::RwLock<Index>, top: u32, path: &str, deep: bool, threads: usize) -> Reconciled {
    reconcile_until(lock, top, path, deep, threads, &std::sync::atomic::AtomicBool::new(false))
}

/// `reconcile`, given up as soon as `stop` is set: what was listed by then is applied, nothing is taken out.
pub fn reconcile_until(lock: &std::sync::RwLock<Index>, top: u32, path: &str, deep: bool, threads: usize,
                       stop: &std::sync::atomic::AtomicBool) -> Reconciled {
    reconcile_many(lock, vec![(path.to_string(), top, deep)], threads, stop)
}

/// `reconcile` for many folders at once, each `(path, entry, deep)`: listed in parallel, and a folder that appears in
/// one of them (new, or moved there) is listed with everything inside. What wasn't listed goes in one pass at the end:
/// an entry from before whose folder was listed and didn't have it. (Inside a folder that couldn't be listed, nothing
/// goes: that folder wasn't listed.) Given up as soon as `stop` is set: what was listed by then is applied, nothing
/// is taken out.
pub fn reconcile_many(lock: &std::sync::RwLock<Index>, folders: Vec<(String, u32, bool)>, threads: usize,
                      stop: &std::sync::atomic::AtomicBool) -> Reconciled {
    use std::sync::atomic::Ordering::Relaxed;
    let before = lock.read().unwrap().len();
    let bit = |set: &mut Vec<u64>, e: u32| {
        if (e as usize) < before {
            set[e as usize / 64] |= 1 << (e % 64);
        }
    };
    let has = |set: &Vec<u64>, e: u32| (e as usize) < before && set[e as usize / 64] >> (e % 64) & 1 != 0;
    // Entries seen in a listing, and folders listed whole (their entries from before that weren't seen go).
    let mut seen = vec![0u64; before.div_ceil(64)];
    let mut listed_ok = vec![0u64; before.div_ceil(64)];
    let mut done = Reconciled::default();
    if folders.is_empty() {
        return done;
    }
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<(String, u32, bool)>();
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<(String, u32, bool, Result<Vec<Found>, u32>, Option<Meta>)>();
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            let jobs = job_rx.clone();
            let out = done_tx.clone();
            scope.spawn(move || {
                lister_thread();
                for (path, entry, deep) in jobs {
                    if stop.load(Relaxed) {
                        break;
                    }
                    let listed = list(&path);
                    // A folder listed on its own: its own date changed with what's in it, and its folder isn't listed.
                    let own = if deep { None } else { stat(&path) };
                    if out.send((path, entry, deep, listed, own)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        let mut pending = 0usize;
        for (path, entry, deep) in folders {
            let first = if path.ends_with(':') { format!("{path}\\") } else { path };
            job_tx.send((first, entry, deep)).unwrap();
            pending += 1;
        }
        while pending > 0 {
            if stop.load(Relaxed) {
                done.stopped = true;
                break;
            }
            let Ok((folder_path, folder, deep, listed, own)) = done_rx.recv() else { break };
            pending -= 1;
            done.folders += 1;
            let mut index = lock.write().unwrap();
            let folder = index.current(folder);
            if index.gone(folder) {
                continue;
            }
            bit(&mut seen, folder);
            if let Some(own) = own.filter(|own| own.flags & DIR != 0 && index.parent(folder) != NONE) {
                if index.modified(folder) != own.modified {
                    index.set_facts(folder, None, own.modified);
                    done.updated += 1;
                }
            }
            let found = match listed {
                Err(_) => {
                    done.refused += 1;
                    continue;
                }
                Ok(found) => found,
            };
            LISTED.fetch_add(found.len(), Relaxed);
            bit(&mut listed_ok, folder);
            for item in found {
                let is_folder = item.meta.flags & DIR != 0 && item.meta.flags & REPARSE == 0;
                let (entry, new) = match index.child(folder, &item.name) {
                    Some(c) if index.flags[c as usize] & DIR == item.meta.flags & DIR => {
                        // The same name in another case: the name as it is now.
                        let c = if index.name(c) != item.name { index.rename(c, folder, &item.name) } else { c };
                        let had = index.meta(c);
                        if had.size != item.meta.size || had.modified != item.meta.modified {
                            index.set_facts(c, item.meta.size, item.meta.modified);
                            done.updated += 1;
                        }
                        if had.flags & (DIR | crate::store::HIDDEN | crate::store::SYSTEM | REPARSE) != item.meta.flags {
                            index.set_flags(c, item.meta.flags);
                        }
                        if index.more(c) != item.more {
                            index.set_more(c, item.more);
                        }
                        bit(&mut seen, c);
                        (c, false)
                    }
                    other => {
                        // A folder where a file was (or the other way round): the old one goes, with what was in it.
                        if let Some(c) = other {
                            index.remove(c);
                            done.removed += 1;
                        }
                        done.added += 1;
                        let entry = index.push(folder, &item.name, item.meta);
                        index.set_more(entry, item.more);
                        (entry, true)
                    }
                };
                if is_folder && (deep || new) {
                    job_tx.send((join(&folder_path, &item.name), entry, true)).unwrap();
                    pending += 1;
                }
            }
        }
        drop(job_tx);
    });
    if done.stopped {
        return done;
    }
    let mut index = lock.write().unwrap();
    for e in 0..before as u32 {
        if index.flags[e as usize] & crate::store::DEAD != 0 || has(&seen, e) {
            continue;
        }
        let parent = index.parent(e);
        if parent != NONE && has(&listed_ok, parent) {
            index.remove(e);
            done.removed += 1;
        }
    }
    index.settle();
    done
}
