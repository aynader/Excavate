//! What's in the chosen folders: asked of Everything (milliseconds for a whole profile, no disk walk) when its list is
//! loaded and kept up to date, or walked. Only files some reader handles, without what's left out.

use config::Exclusions;
use extract::Reader;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct FileInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub modified: i64,
    pub reader: Reader,
}

/// Files kept online only (OneDrive, Dropbox, iCloud placeholders): opening one would download it, so they're
/// never read. FILE_ATTRIBUTE_OFFLINE, _RECALL_ON_OPEN, _RECALL_ON_DATA_ACCESS.
pub const NOT_ON_THIS_PC: u32 = 0x1000 | 0x4_0000 | 0x40_0000;

pub fn on_this_pc(attributes: u32) -> bool {
    attributes & NOT_ON_THIS_PC == 0
}

/// Whether a file is kept online only, asked of Windows now (its attributes, without opening it). A listing from
/// Everything doesn't say: asking it would read every file's attributes from the disk.
pub fn online_only(path: &str) -> bool {
    use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated path that outlives the call.
    let attributes = unsafe { GetFileAttributesW(windows::core::PCWSTR(wide.as_ptr())) };
    attributes != INVALID_FILE_ATTRIBUTES && !on_this_pc(attributes)
}

pub fn wanted(path: &str, name: &str, exclusions: &Exclusions, pictures: bool) -> Option<Reader> {
    let reader = extract::reader_for(name)?;
    if reader == Reader::Picture && !pictures {
        return None;
    }
    if exclusions.excludes(path) {
        return None;
    }
    Some(reader)
}

/// Whether a path is in what's being read now: under one of the chosen folders, and not left out.
pub fn in_scope(path: &str, roots: &[String], exclusions: &Exclusions) -> bool {
    let lower = path.to_lowercase();
    let under = |root: &String| lower.strip_prefix(root.trim_end_matches('\\').to_lowercase().as_str()).is_some_and(|rest| rest.starts_with('\\'));
    roots.iter().any(under) && !exclusions.excludes(path)
}

/// Everything in `root` worth reading: through Everything when it's given (its list kept up to date), else walked. When
/// Everything has nothing there, the folder is walked too: it may be on a drive Everything leaves out.
pub fn list(root: &str, exclusions: &Exclusions, pictures: bool, everything: Option<&everything::Everything>) -> Vec<FileInfo> {
    if let Some(files) = everything.and_then(|everything| list_with_everything(root, exclusions, pictures, everything)) {
        if !files.is_empty() {
            return files;
        }
    }
    let mut files = Vec::new();
    walk(Path::new(root), exclusions, pictures, &mut files);
    files
}

/// Files kept online only are among these: `online_only` asks of those about to be read.
fn list_with_everything(root: &str, exclusions: &Exclusions, pictures: bool, everything: &everything::Everything) -> Option<Vec<FileInfo>> {
    let mut folder = root.trim_end_matches('\\').to_string();
    folder.push('\\');
    let extensions: Vec<&str> = extract::extensions()
        .into_iter()
        .filter(|extension| pictures || !extract::PICTURE_EXTENSIONS.contains(extension))
        .collect();
    let text = format!("file: \"{folder}\" ext:{}{}", extensions.join(";"), exclusions.everything());
    let page = everything.list(everything::Query::listing(text, everything::Sort::ModifiedDescending)).ok()?;
    let mut files = Vec::with_capacity(page.items.len());
    for item in page.items {
        let path = item.path();
        if !path.to_lowercase().starts_with(&folder.to_lowercase()) {
            continue;
        }
        if let Some(reader) = wanted(&path, &item.name, exclusions, pictures) {
            files.push(FileInfo { path, name: item.name, size: item.size.unwrap_or(0), modified: item.modified.unwrap_or(0), reader });
        }
    }
    Some(files)
}

/// Videos a model can look at frame by frame (what Windows' Media Foundation plays).
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm", "mpg", "mpeg", "3gp", "mts", "m2ts"];

/// Recordings the model that listens can hear (what Windows' Media Foundation plays; Ogg and Opus with Windows' own
/// extensions for them).
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "m4a", "m4b", "wav", "wma", "flac", "aac", "ogg", "opus", "amr", "aiff", "aif"];

fn has_extension(name: &str, extensions: &[&str]) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| extensions.contains(&extension.to_ascii_lowercase().as_str()))
}

/// The videos in `root`, as `list` finds files (Everything, else a walk): (path, size, modified).
pub fn list_videos(root: &str, exclusions: &Exclusions, everything: Option<&everything::Everything>) -> Vec<(String, u64, i64)> {
    list_media(root, VIDEO_EXTENSIONS, exclusions, everything, true)
}

/// The recordings in `root`, the same way; with `walk` false, only what Everything has (for looking again later: a walk
/// costs seconds, Everything's answer milliseconds).
pub fn list_recordings(root: &str, exclusions: &Exclusions, everything: Option<&everything::Everything>, walk: bool) -> Vec<(String, u64, i64)> {
    list_media(root, AUDIO_EXTENSIONS, exclusions, everything, walk)
}

fn list_media(root: &str, extensions: &[&str], exclusions: &Exclusions, everything: Option<&everything::Everything>, walk: bool) -> Vec<(String, u64, i64)> {
    let mut folder = root.trim_end_matches('\\').to_string();
    folder.push('\\');
    if let Some(everything) = everything {
        let text = format!("file: \"{folder}\" ext:{}{}", extensions.join(";"), exclusions.everything());
        if let Ok(page) = everything.list(everything::Query::listing(text, everything::Sort::ModifiedDescending)) {
            let found: Vec<(String, u64, i64)> = page.items.into_iter()
                .map(|item| (item.path(), item.size.unwrap_or(0), item.modified.unwrap_or(0)))
                .filter(|(path, ..)| path.to_lowercase().starts_with(&folder.to_lowercase()) && !exclusions.excludes(path))
                .collect();
            if !found.is_empty() || !walk {
                return found;
            }
        }
    }
    let mut found = Vec::new();
    if !walk {
        return found;
    }
    walk_files(Path::new(root), exclusions, &mut |path, name, metadata| {
        if has_extension(name, extensions) && !exclusions.excludes(&path) {
            found.push((path, metadata.len(), seconds(metadata)));
        }
    });
    found
}

/// The folder tree, without hidden and system folders, links (no loops) and what's left out.
pub fn walk(folder: &Path, exclusions: &Exclusions, pictures: bool, out: &mut Vec<FileInfo>) {
    walk_files(folder, exclusions, &mut |path, name, metadata| {
        if let Some(reader) = wanted(&path, name, exclusions, pictures) {
            out.push(FileInfo { path, name: name.to_string(), size: metadata.len(), modified: seconds(metadata), reader });
        }
    });
}

/// Each file in the folder tree, as `walk` goes through it: (path, name, its metadata).
fn walk_files(folder: &Path, exclusions: &Exclusions, each: &mut dyn FnMut(String, &str, &std::fs::Metadata)) {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const REPARSE: u32 = 0x400;
    let mut stack = vec![folder.to_path_buf()];
    while let Some(folder) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else { continue };
            let attributes = metadata.file_attributes();
            let name = entry.file_name().to_string_lossy().into_owned();
            if attributes & REPARSE != 0 || !on_this_pc(attributes) || exclusions.excludes_name(&name) {
                continue;
            }
            let path = entry.path();
            if metadata.is_dir() {
                if attributes & (HIDDEN | SYSTEM) == 0 {
                    stack.push(path);
                }
                continue;
            }
            each(path.display().to_string(), &name, &metadata);
        }
    }
}

pub fn seconds(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// One file's facts, now (after a change notification).
pub fn file(path: &Path, exclusions: &Exclusions, pictures: bool) -> Option<FileInfo> {
    use std::os::windows::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || !on_this_pc(metadata.file_attributes()) {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().into_owned();
    let path_text = path.display().to_string();
    let reader = wanted(&path_text, &name, exclusions, pictures)?;
    Some(FileInfo { path: path_text, name, size: metadata.len(), modified: seconds(&metadata), reader })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What keeps a removed folder's images from coming back: Deep asks this of each image just before reading it, so a
    /// batch taken from the catalog before the removal was saved, and held through a download, stays out.
    #[test]
    fn a_removed_folder_stays_out() {
        let roots = vec![r"C:\Kept".to_string(), "D:\\".to_string()];
        let exclusions = Exclusions::new(&["node_modules".to_string(), r"C:\Kept\private".to_string()]);
        assert!(in_scope(r"C:\Kept\a.png", &roots, &exclusions));
        assert!(in_scope(r"c:\kept\sub\b.PNG", &roots, &exclusions));
        assert!(in_scope(r"D:\c.png", &roots, &exclusions));
        // The folder that was removed, and one that only starts the same.
        assert!(!in_scope(r"C:\Removed\d.png", &roots, &exclusions));
        assert!(!in_scope(r"C:\Kept2\e.png", &roots, &exclusions));
        // Left out since.
        assert!(!in_scope(r"C:\Kept\node_modules\f.png", &roots, &exclusions));
        assert!(!in_scope(r"C:\Kept\Private\g.png", &roots, &exclusions));
    }

    /// Without an Everything that keeps up, the folder itself; and what's on this PC isn't taken for online only.
    #[test]
    fn walked_without_everything() {
        let folder = std::env::temp_dir().join(format!("excavate-scan-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("inside")).unwrap();
        std::fs::write(folder.join("a.txt"), "a").unwrap();
        std::fs::write(folder.join("inside").join("b.md"), "b").unwrap();
        std::fs::write(folder.join("c.unknown-kind"), "c").unwrap();
        let mut names: Vec<String> = list(&folder.display().to_string(), &Exclusions::new(&[]), true, None).into_iter().map(|file| file.name).collect();
        names.sort();
        assert_eq!(names, ["a.txt", "b.md"]);
        assert!(!online_only(&folder.join("a.txt").display().to_string()));
        assert!(!online_only(&folder.join("gone.txt").display().to_string()));
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
