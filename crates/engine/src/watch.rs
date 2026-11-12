//! Changes a folder watch reports (`ReadDirectoryChangesW`: paths, not file records), applied to the index: each finds
//! its entry by name, folder by folder. Its buffer can overflow under a burst of changes; then the only safe answer is
//! listing that root again (`Overflowed`).

use crate::store::{Index, Meta, More, DIR, REPARSE};
use crate::store::seconds;
use crate::walk::{flags, join, list, stat_facts};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_NOTIFY_EXTENDED_INFORMATION};

/// What a change says of the file: its size, date and flags, and its dates made and opened and attributes.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    pub meta: Meta,
    pub more: More,
}

impl Facts {
    /// From the extended record Windows sends with each change.
    pub(crate) fn of(info: &FILE_NOTIFY_EXTENDED_INFORMATION) -> Facts {
        let is_dir = info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        Facts {
            meta: Meta { size: if is_dir { None } else { Some(info.FileSize as u64) },
                         modified: seconds(info.LastModificationTime), flags: flags(info.FileAttributes) },
            more: More { created: seconds(info.CreationTime), accessed: seconds(info.LastAccessTime),
                         attributes: info.FileAttributes },
        }
    }
}

pub enum Change {
    /// With the file's facts when Windows sent them along (it does since Windows 10 1709), so no call is needed.
    Added(String, Option<Facts>),
    Removed(String),
    /// Written to, or its attributes changed.
    Modified(String, Option<Facts>),
    Renamed(String, String),
    Overflowed,
}

/// The entry at `relative` (a path under `root`'s entry), by its names.
pub fn find(index: &Index, root: u32, relative: &str) -> Option<u32> {
    let mut at = root;
    for part in relative.split('\\').filter(|p| !p.is_empty()) {
        at = index.child(at, part)?;
    }
    Some(at)
}

fn split(relative: &str) -> (&str, &str) {
    match relative.rfind('\\') {
        Some(n) => (&relative[..n], &relative[n + 1..]),
        None => ("", relative),
    }
}

#[derive(Default, Debug)]
pub struct Applied {
    /// Folders that arrived whole (moved in from elsewhere): left for the caller to list when `defer_folders` is set.
    pub new_folders: Vec<u32>,
    pub defer_folders: bool,
    pub added: usize,
    pub removed: usize,
    pub renamed: usize,
    pub modified: usize,
    pub missed: usize,
    pub overflowed: usize,
}

/// Applies one change under `root_path` (whose entry is `root`).
pub fn apply(index: &mut Index, root: u32, root_path: &str, change: Change, applied: &mut Applied) {
    if std::env::var_os("ENGINE_DEBUG").is_some() {
        let missed = applied.missed;
        let shown = match &change {
            Change::Added(p, _) => format!("added {p:?}"),
            Change::Removed(p) => format!("removed {p:?}"),
            Change::Modified(p, _) => format!("modified {p:?}"),
            Change::Renamed(a, b) => format!("renamed {a:?} -> {b:?}"),
            Change::Overflowed => "overflowed".into(),
        };
        apply_change(index, root, root_path, change, applied);
        if applied.missed > missed {
            eprintln!("  missed: {shown}");
        }
        return;
    }
    apply_change(index, root, root_path, change, applied)
}

fn apply_change(index: &mut Index, root: u32, root_path: &str, change: Change, applied: &mut Applied) {
    match change {
        Change::Overflowed => applied.overflowed += 1,
        Change::Removed(relative) => match find(index, root, &relative) {
            Some(entry) => { index.remove(entry); applied.removed += 1; }
            None => applied.missed += 1,
        },
        // Written to: a new size and date (a file is often added empty and written after, so this matters), or its
        // attributes changed (hidden, read-only…). Whether it's a folder is the index's to say, not the change's.
        Change::Modified(relative, facts) => {
            let Some(entry) = find(index, root, &relative) else { return };
            if let Some(Facts { meta, more }) = facts.or_else(|| stat_facts(&join(root_path, &relative))) {
                index.set_facts(entry, meta.size, meta.modified);
                let dir = index.meta(entry).flags & DIR;
                index.set_flags(entry, dir | (meta.flags & !DIR));
                index.set_more(entry, more);
                applied.modified += 1;
            }
        }
        Change::Renamed(from, to) => {
            let (folder, name) = split(&to);
            match (find(index, root, &from), find(index, root, folder)) {
                (Some(entry), Some(parent)) => { index.rename(entry, parent, name); applied.renamed += 1; }
                // Windows reports a case-only rename (with replace-existing, as most programs rename) as the
                // name removed and then renamed from it: the source is gone by then. What's on disk decides.
                _ => apply_change(index, root, root_path, Change::Added(to, None), applied),
            }
        }
        Change::Added(relative, meta) => {
            let (folder, name) = split(&relative);
            let Some(parent) = find(index, root, folder) else { applied.missed += 1; return };
            if index.child(parent, name).is_some() {
                return;
            }
            // Its facts come from the file itself.
            let folder_path = join(root_path, folder);
            let Some(Facts { meta, more }) = meta.or_else(|| stat_facts(&join(&folder_path, name))) else { applied.missed += 1; return };
            let follow = meta.flags & DIR != 0 && meta.flags & REPARSE == 0;
            let entry = index.push(parent, name, meta);
            index.set_more(entry, more);
            applied.added += 1;
            if follow {
                // A folder moved in from elsewhere arrives whole, with one event for itself only: listed here, or by
                // the caller outside its lock.
                if applied.defer_folders {
                    applied.new_folders.push(entry);
                } else {
                    add_tree(index, entry, &join(&folder_path, name), applied);
                }
            }
        }
    }
}

fn add_tree(index: &mut Index, parent: u32, path: &str, applied: &mut Applied) {
    let Ok(found) = list(path) else { return };
    for item in found {
        let follow = item.meta.flags & DIR != 0 && item.meta.flags & REPARSE == 0;
        let child_path = join(path, &item.name);
        let entry = index.push(parent, &item.name, item.meta);
        index.set_more(entry, item.more);
        applied.added += 1;
        if follow {
            add_tree(index, entry, &child_path, applied);
        }
    }
}
