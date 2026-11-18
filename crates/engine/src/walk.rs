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

/// Puts this lister in or out of background priority, when it isn't already (`now` is where it is): a paced walk
/// follows the speed control's changes between two folders.
fn lister_mode(want: bool, now: &mut bool) {
    if want == *now {
        return;
    }
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END};
    let mode = if want { THREAD_MODE_BACKGROUND_BEGIN } else { THREAD_MODE_BACKGROUND_END };
    unsafe { SetThreadPriority(GetCurrentThread(), mode) };
    *now = want;
}

/// How long a waiting lister sleeps before it looks at `stop` again (stopping doesn't wake it; a speed change does).
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(100);

/// How many listers a paced walk (`reconcile_paced`) may use now, and whether they list at background priority.
/// Excavate's speed control sets it at any time (`set_pace`); a walk going on follows between two folders: the
/// listers above the count wait (they aren't ended), and go on when it rises again.
pub struct Pace {
    listers: std::sync::Mutex<usize>,
    background: std::sync::atomic::AtomicBool,
    changed: std::sync::Condvar,
}

impl Pace {
    pub const fn new(listers: usize, background: bool) -> Pace {
        Pace { listers: std::sync::Mutex::new(listers), background: std::sync::atomic::AtomicBool::new(background),
               changed: std::sync::Condvar::new() }
    }

    /// At least one lister always lists.
    pub fn set(&self, listers: usize, background: bool) {
        self.background.store(background, std::sync::atomic::Ordering::Relaxed);
        *self.lock() = listers.max(1);
        self.changed.notify_all();
    }

    pub fn listers(&self) -> usize {
        *self.lock()
    }

    pub fn background(&self) -> bool {
        self.background.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.listers.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Waits while lister `place` (from 0) is above the count: true when its turn comes, false once `over` says the
    /// walk ended or was stopped.
    fn turn(&self, place: usize, over: impl Fn() -> bool) -> bool {
        let mut listers = self.lock();
        loop {
            if over() {
                return false;
            }
            if place < *listers {
                return true;
            }
            listers = self.changed.wait_timeout(listers, LOOK_AGAIN).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }

    /// Wakes the waiting listers to look at `over` again (a walk ended). Under the lock, so none misses it.
    fn wake(&self) {
        let _listers = self.lock();
        self.changed.notify_all();
    }
}

/// The pace of Bedrock's catch-up walks: 2 listers in the background until Excavate says otherwise.
pub static PACE: Pace = Pace::new(2, true);

/// Sets the pace of catch-up walks, going on or to come (`Pace`).
pub fn set_pace(listers: usize, background: bool) {
    PACE.set(listers, background);
}

/// The most listers a paced walk starts: up to 8, and one thread fewer than the processor has (at least 1). Those
/// above the pace's count only wait, so starting them all costs a few idle threads.
pub fn most_listers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get()).saturating_sub(1).clamp(1, 8)
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

/// `reconcile_until` at `pace`: up to `most` listers, as many listing as the pace allows at each moment, at its
/// priority (when listing in the background at all: `set_background`).
pub fn reconcile_paced(lock: &std::sync::RwLock<Index>, top: u32, path: &str, deep: bool, most: usize, pace: &Pace,
                       stop: &std::sync::atomic::AtomicBool) -> Reconciled {
    reconcile_with(lock, vec![(path.to_string(), top, deep)], most, Some(pace), stop)
}

/// `reconcile` for many folders at once, each `(path, entry, deep)`: listed in parallel, and a folder that appears in
/// one of them (new, or moved there) is listed with everything inside. What wasn't listed goes in one pass at the end:
/// an entry from before whose folder was listed and didn't have it. (Inside a folder that couldn't be listed, nothing
/// goes: that folder wasn't listed.) Given up as soon as `stop` is set: what was listed by then is applied, nothing
/// is taken out.
pub fn reconcile_many(lock: &std::sync::RwLock<Index>, folders: Vec<(String, u32, bool)>, threads: usize,
                      stop: &std::sync::atomic::AtomicBool) -> Reconciled {
    reconcile_with(lock, folders, threads, None, stop)
}

/// `reconcile_many`, at `pace` when there's one (`reconcile_paced`).
fn reconcile_with(lock: &std::sync::RwLock<Index>, folders: Vec<(String, u32, bool)>, threads: usize, pace: Option<&Pace>,
                  stop: &std::sync::atomic::AtomicBool) -> Reconciled {
    use std::sync::atomic::Ordering::Relaxed;
    // Folders made while the index is followed (a program's new folder, listed as it arrived) have everything inside
    // them after them: only the entries from the lowest of them on are looked at, not the whole index. (Only this
    // thread changes the index, and listing renames nothing that would undo it: a name's case changing is a new entry
    // after the folder, with its contents still after the folder too.)
    let (before, base) = {
        let index = lock.read().unwrap();
        let lowest = folders.iter().map(|(_, entry, _)| index.current(*entry)).min().unwrap_or(0);
        (index.len(), if index.fresh(lowest) { lowest as usize } else { 0 })
    };
    let bit = |set: &mut Vec<u64>, e: u32| {
        let e = e as usize;
        if e >= base && e < before {
            set[(e - base) / 64] |= 1 << ((e - base) % 64);
        }
    };
    let has = |set: &Vec<u64>, e: u32| {
        let e = e as usize;
        e >= base && e < before && set[(e - base) / 64] >> ((e - base) % 64) & 1 != 0
    };
    // Entries seen in a listing, and folders listed whole (their entries from before that weren't seen go).
    let mut seen = vec![0u64; (before - base).div_ceil(64)];
    let mut listed_ok = vec![0u64; (before - base).div_ceil(64)];
    let mut done = Reconciled::default();
    if folders.is_empty() {
        return done;
    }
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<(String, u32, bool)>();
    let (done_tx, done_rx) = crossbeam_channel::unbounded::<(String, u32, bool, Result<Vec<Found>, u32>, Option<Meta>)>();
    // Set when the walk is over (done or stopped), for listers waiting their turn.
    let finished = &std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        for place in 0..threads.max(1) {
            let jobs = job_rx.clone();
            let out = done_tx.clone();
            scope.spawn(move || {
                let mut background = false;
                loop {
                    if let Some(pace) = pace {
                        if !pace.turn(place, || stop.load(Relaxed) || finished.load(Relaxed)) {
                            break;
                        }
                    }
                    let Ok((path, entry, deep)) = jobs.recv() else { break };
                    if stop.load(Relaxed) {
                        break;
                    }
                    lister_mode(BACKGROUND.load(Relaxed) && pace.is_none_or(Pace::background), &mut background);
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
            // Not waiting for good: a lister that saw `stop` ends without its folder, and the others wait for jobs.
            let (folder_path, folder, deep, listed, own) = match done_rx.recv_timeout(LOOK_AGAIN) {
                Ok(listed) => listed,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            };
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
        finished.store(true, Relaxed);
        if let Some(pace) = pace {
            pace.wake();
        }
    });
    if done.stopped {
        return done;
    }
    let mut index = lock.write().unwrap();
    for e in base as u32..before as u32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Long enough to see that a waiting lister doesn't go (it would within microseconds).
    const A_MOMENT: Duration = Duration::from_millis(50);
    /// Far longer than a lister takes to go once it may (only reached when the test fails).
    const AT_MOST: Duration = Duration::from_secs(5);

    #[test]
    fn listers_above_the_pace_wait_and_go_on_when_it_rises() {
        let pace = Pace::new(2, true);
        let over = AtomicBool::new(false);
        let (turns, taken) = crossbeam_channel::unbounded();
        std::thread::scope(|scope| {
            // Listers 0, 2 and 5, each asking for its turn before each folder, as a paced walk's do; the test says when
            // each has listed its folder and asks again.
            let asks: Vec<_> = [0, 2, 5].into_iter().map(|place| {
                let (ask, asked) = crossbeam_channel::unbounded::<()>();
                let (pace, over, turns) = (&pace, &over, turns.clone());
                scope.spawn(move || loop {
                    let went = pace.turn(place, || over.load(Ordering::Relaxed));
                    turns.send((place, went)).unwrap();
                    if !went || asked.recv().is_err() {
                        return;
                    }
                });
                ask
            }).collect();
            let next = || taken.recv_timeout(AT_MOST).unwrap();
            let none = || taken.recv_timeout(A_MOMENT).is_err();
            // At 2 (Gentle, Normal): 0 lists, 2 and 5 wait.
            assert_eq!(next(), (0, true));
            assert!(none(), "nothing above 2 lists");
            // Raised to 4 (Fast): 2 goes on, 5 still waits.
            pace.set(4, true);
            assert_eq!(next(), (2, true));
            assert!(none(), "5 waits at 4");
            // Raised to 8 (Full, at normal priority): 5 goes on too.
            pace.set(8, false);
            assert_eq!(next(), (5, true));
            assert!(!pace.background());
            // Lowered to nothing, which is 1: after their folder, 2 and 5 wait; 0 still lists.
            pace.set(0, true);
            assert_eq!(pace.listers(), 1);
            asks[1].send(()).unwrap();
            asks[2].send(()).unwrap();
            assert!(none(), "nothing above 1 lists");
            asks[0].send(()).unwrap();
            assert_eq!(next(), (0, true));
            // Raised to 3: 2 goes on, 5 waits.
            pace.set(3, true);
            assert_eq!(next(), (2, true));
            assert!(none(), "5 waits at 3");
            // The walk ends: 5 stops waiting, and the others end when asked again.
            over.store(true, Ordering::Relaxed);
            pace.wake();
            assert_eq!(next(), (5, false));
            drop(asks);
        });
    }

    #[test]
    fn a_paced_walk_lists_everything_while_the_pace_changes() {
        let dir = std::env::temp_dir().join(format!("engine-paced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for a in 0..20 {
            for b in 0..5 {
                std::fs::create_dir_all(dir.join(format!("a{a}")).join(format!("b{b}"))).unwrap();
                std::fs::write(dir.join(format!("a{a}")).join(format!("b{b}")).join("f.txt"), b"x").unwrap();
            }
        }
        let root = dir.display().to_string();
        let mut index = Index::new();
        walk(&mut index, &root, 2);
        let top = index.roots()[0];
        let before = index.live();
        // Added since: listed again at a pace of 1 that rises and falls while it goes.
        for a in 0..20 {
            std::fs::write(dir.join(format!("a{a}")).join("new.txt"), b"x").unwrap();
        }
        let lock = std::sync::RwLock::new(index);
        let pace = Pace::new(1, false);
        let (stop, done) = (AtomicBool::new(false), AtomicBool::new(false));
        let listed = std::thread::scope(|scope| {
            scope.spawn(|| {
                for listers in [8, 1, 4, 2].into_iter().cycle() {
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    pace.set(listers, false);
                    std::thread::yield_now();
                }
            });
            let listed = reconcile_paced(&lock, top, &root, true, 8, &pace, &stop);
            done.store(true, Ordering::Relaxed);
            listed
        });
        assert_eq!((listed.added, listed.removed, listed.refused, listed.stopped), (20, 0, 0, false));
        assert_eq!(lock.read().unwrap().live(), before + 20);
        // Stopped: it ends, its waiting listers with it.
        stop.store(true, Ordering::Relaxed);
        assert!(reconcile_paced(&lock, top, &root, true, 8, &pace, &stop).stopped);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
