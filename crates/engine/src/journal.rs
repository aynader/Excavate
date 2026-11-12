//! Catching up after a restart without admin rights, from the drive's change journal.
//!
//! Windows lets any user read the journal (`FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`, Windows 10 1709 and later) through a
//! handle to the drive's root folder; the volume handle `mft.rs` uses needs admin rights. The journal's position is
//! saved beside the index (`save` / `load`). At the next start, the records since then name each folder something
//! changed in (a record's parent), and each folder made or moved there (its own record). Only those are listed again,
//! instead of every folder on the drive. A record gives a file id, not a path; `OpenFileById` and
//! `GetFinalPathNameByHandleW` turn it into one, for folders the user can open (the others couldn't be listed anyway).
//!
//! When the journal can't answer (another journal since, written past the saved position, or no journal at all), the
//! caller lists everything again, as before.

use crate::walk::{from_wide, wide};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdType, GetFinalPathNameByHandleW, GetVolumePathNameW, OpenFileById, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, READ_USN_JOURNAL_DATA_V1, USN_JOURNAL_DATA_V0,
    USN_REASON_FILE_CREATE, USN_REASON_RENAME_NEW_NAME, USN_RECORD_V2,
};

/// Where a drive's journal was: which journal, and the position of its next record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub journal: u64,
    pub usn: i64,
}

/// What changed since a position.
#[derive(Debug, Default)]
pub struct Changed {
    /// Folders whose own entries changed: listed again, their own entries only.
    pub folders: Vec<String>,
    /// Folders made or moved in: listed again with everything inside.
    pub whole: Vec<String>,
    pub records: usize,
    /// Folders named by the records that couldn't be opened (gone since, or not the user's to open).
    pub unopened: usize,
    /// Where the journal was when it was read: the position to save once these are listed.
    pub now: Position,
}

/// The drive a path is on, as its root folder ("C:\").
pub fn volume_of(path: &str) -> Option<String> {
    let path = wide(path);
    let mut out = vec![0u16; 1024];
    let ok = unsafe { GetVolumePathNameW(path.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    if ok == 0 {
        return None;
    }
    let len = out.iter().position(|&c| c == 0).unwrap_or(out.len());
    Some(String::from_utf16_lossy(&out[..len]))
}

/// A handle to the drive's root folder: enough for the journal, without admin rights.
struct Root(HANDLE);

// Windows handles may be used from any thread; ids are turned into paths in parallel through this one.
unsafe impl Send for Root {}
unsafe impl Sync for Root {}

impl Root {
    fn open(volume: &str) -> Result<Root, u32> {
        let path = wide(volume);
        let handle = unsafe {
            CreateFileW(path.as_ptr(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut())
        };
        if handle == INVALID_HANDLE_VALUE { Err(unsafe { GetLastError() }) } else { Ok(Root(handle)) }
    }

    fn query(&self) -> Result<USN_JOURNAL_DATA_V0, u32> {
        let mut data: USN_JOURNAL_DATA_V0 = unsafe { std::mem::zeroed() };
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(self.0, FSCTL_QUERY_USN_JOURNAL, std::ptr::null(), 0, &mut data as *mut _ as *mut c_void,
                            std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32, &mut got, std::ptr::null_mut())
        };
        if ok == 0 { Err(unsafe { GetLastError() }) } else { Ok(data) }
    }

    /// The folder (or file) with this id, as a path ("C:\Users\…"), when it still exists and the user can open it.
    fn path_of(&self, id: u64) -> Option<String> {
        let descriptor = FILE_ID_DESCRIPTOR { dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32, Type: FileIdType,
                                              Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id as i64 } };
        unsafe {
            let handle = OpenFileById(self.0, &descriptor, FILE_READ_ATTRIBUTES,
                                      FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(),
                                      FILE_FLAG_BACKUP_SEMANTICS);
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut out = vec![0u16; 1024];
            let mut len = GetFinalPathNameByHandleW(handle, out.as_mut_ptr(), out.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
            if len as usize >= out.len() {
                out = vec![0u16; len as usize + 1];
                len = GetFinalPathNameByHandleW(handle, out.as_mut_ptr(), out.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS);
            }
            CloseHandle(handle);
            if len == 0 || len as usize >= out.len() {
                return None;
            }
            let path = from_wide(&out[..len as usize]);
            // "\\?\C:\…" (and "\\?\UNC\…" never: these are local drives).
            Some(path.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(path))
        }
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// Where the journal of the drive `path` is on now.
pub fn position(path: &str) -> Option<Position> {
    let root = Root::open(&volume_of(path)?).ok()?;
    let data = root.query().ok()?;
    Some(Position { journal: data.UsnJournalID, usn: data.NextUsn })
}

/// How large the journal of the drive `path` may grow, in bytes (Windows' default is 32 MB; then the oldest records go).
pub fn size(path: &str) -> Option<u64> {
    let root = Root::open(&volume_of(path)?).ok()?;
    root.query().ok().map(|data| data.MaximumSize)
}

/// Why the journal can't say what changed: then everything is listed again.
#[derive(Debug)]
pub enum Why {
    /// No saved position for this drive.
    NotSaved,
    /// The drive's root couldn't be opened, or has no journal (Windows' error).
    NoJournal(u32),
    /// The journal was made again since (its id changed).
    Replaced,
    /// The journal no longer holds the saved position: more changed than it keeps.
    Overwritten,
    /// Windows refused the read (its error).
    Refused(u32),
}

/// What changed on the drive of `path` since `from`, as the folders to list again (only those at or under `path`).
pub fn since(path: &str, from: Option<Position>) -> Result<Changed, Why> {
    let from = from.ok_or(Why::NotSaved)?;
    let volume = volume_of(path).ok_or(Why::NoJournal(0))?;
    let root = Root::open(&volume).map_err(Why::NoJournal)?;
    let data = root.query().map_err(Why::NoJournal)?;
    if data.UsnJournalID != from.journal {
        return Err(Why::Replaced);
    }
    if from.usn < data.FirstUsn || from.usn > data.NextUsn {
        return Err(Why::Overwritten);
    }
    let now = Position { journal: data.UsnJournalID, usn: data.NextUsn };
    // Read up to where the journal was a moment ago; what comes after is the watches' to report.
    let mut parents: HashSet<u64> = HashSet::new();
    let mut made: HashSet<u64> = HashSet::new();
    let mut records = 0usize;
    let mut buffer = vec![0u64; 1 << 14]; // 128 KB
    let mut next = from.usn;
    while next < now.usn {
        let input = READ_USN_JOURNAL_DATA_V1 { StartUsn: next, ReasonMask: u32::MAX, ReturnOnlyOnClose: 0, Timeout: 0,
                                               BytesToWaitFor: 0, UsnJournalID: from.journal, MinMajorVersion: 2,
                                               MaxMajorVersion: 2 };
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(root.0, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, &input as *const _ as *const c_void,
                            std::mem::size_of_val(&input) as u32, buffer.as_mut_ptr() as *mut c_void,
                            (buffer.len() * 8) as u32, &mut got, std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(Why::Refused(unsafe { GetLastError() }));
        }
        let after = buffer[0] as i64;
        let base = buffer.as_ptr() as *const u8;
        let mut at = 8usize;
        while at + std::mem::size_of::<USN_RECORD_V2>() <= got as usize {
            let record = unsafe { &*(base.add(at) as *const USN_RECORD_V2) };
            if record.RecordLength == 0 {
                break;
            }
            if record.MajorVersion != 2 {
                return Err(Why::Refused(0));
            }
            if record.Usn >= now.usn {
                break;
            }
            records += 1;
            parents.insert(record.ParentFileReferenceNumber);
            if record.FileAttributes & 0x10 != 0 && record.Reason & (USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME) != 0 {
                made.insert(record.FileReferenceNumber);
            }
            at += record.RecordLength as usize;
        }
        if got <= 8 || after <= next {
            break;
        }
        next = after;
    }
    // Ids to paths, in parallel (each is a few system calls); only what's at or under `path`.
    use rayon::prelude::*;
    let under = path.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let inside = |p: &String| {
        let lower = p.to_lowercase();
        lower == under || lower.starts_with(&format!("{under}\\"))
    };
    let resolve = |ids: &HashSet<u64>| -> (Vec<String>, usize) {
        let found: Vec<Option<String>> = ids.par_iter().map(|&id| root.path_of(id)).collect();
        let unopened = found.iter().filter(|p| p.is_none()).count();
        let mut paths: Vec<String> = found.into_iter().flatten().map(|p| p.trim_end_matches('\\').to_string()).filter(inside).collect();
        paths.sort();
        paths.dedup();
        (paths, unopened)
    };
    let (folders, unopened_a) = resolve(&parents);
    let (whole, unopened_b) = resolve(&made);
    Ok(Changed { folders, whole, records, unopened: unopened_a + unopened_b, now })
}

/// The file beside the index that keeps each drive's journal position.
fn file_for(index: &Path) -> std::path::PathBuf {
    index.with_extension("journal")
}

/// What's kept per drive beside the index: the journal's position, and how many folders couldn't be listed (the
/// journal doesn't say, and they aren't listed again).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Saved {
    pub position: Position,
    pub refused: usize,
}

/// What was saved with the index at `index`, by drive ("C:\").
pub fn load(index: &Path) -> HashMap<String, Saved> {
    let Ok(text) = std::fs::read_to_string(file_for(index)) else { return HashMap::new() };
    text.lines().filter_map(|line| {
        let mut parts = line.rsplitn(4, '\t');
        let refused = parts.next()?.parse().ok()?;
        let usn = parts.next()?.parse().ok()?;
        let journal = parts.next()?.parse().ok()?;
        Some((parts.next()?.to_string(), Saved { position: Position { journal, usn }, refused }))
    }).collect()
}

/// Saves it (after the index itself: a crash between the two leaves older positions, which only means more is read
/// again).
pub fn save(index: &Path, saved: &HashMap<String, Saved>) {
    let mut text = String::new();
    for (drive, s) in saved {
        text.push_str(&format!("{drive}\t{}\t{}\t{}\n", s.position.journal, s.position.usn, s.refused));
    }
    let file = file_for(index);
    let temporary = file.with_extension("journal-saving");
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, &file);
    }
}

/// Removes the saved positions (when the index is built again from nothing, or isn't saved).
pub fn forget(index: &Path) {
    let _ = std::fs::remove_file(file_for(index));
}
