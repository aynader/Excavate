//! The hot tier: the places people open most, held in memory and matched in-process on every keystroke, so the first
//! frame of results never waits on anything. Its sources:
//!
//! - Start menu shortcuts (apps),
//! - Windows' Recent Items (what was opened lately, in any app),
//! - what was opened through Excavate (its history),
//! - the person's own folders (Desktop, Documents, Downloads, Pictures…), a few levels deep, skipping build output
//!   and dependency folders, up to a cap.
//!
//! Everything still answers for the rest of the disk a moment later; this tier makes the likely answers instant and
//! carries what Everything doesn't know (Recent Items, Excavate's own history).

use crate::filter::Filter;
use crate::query::Terms;
use crate::rank::{Candidate, Context, Origin, Ranked, name_match, score};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Entry {
    pub candidate: Candidate,
}

/// Where the hot tier comes from (resolved by the caller, which owns COM for shortcuts).
#[derive(Clone, Debug, Default)]
pub struct Sources {
    /// Start menu program folders.
    pub programs: Vec<PathBuf>,
    /// Recent Items, resolved: (target, when it was opened).
    pub recent: Vec<(String, i64)>,
    /// Excavate's history: (path, opens, last opened).
    pub history: Vec<(String, u32, i64)>,
    /// The person's folders, walked `depth` levels deep.
    pub folders: Vec<PathBuf>,
    pub depth: usize,
    /// At most this many entries from the folders.
    pub cap: usize,
}

pub struct HotIndex {
    entries: Vec<Entry>,
    pub built: Instant,
    pub build_time: Duration,
}

/// Folders not walked: dependencies, build output, caches, version control.
const SKIP: &[&str] = &[
    "node_modules", ".git", "target", "build", "dist", "out", "obj", "bin", "__pycache__", ".venv", "venv",
    "site-packages", ".gradle", ".idea", ".vs", ".vscode", ".cache", "vendor", "Vendor", "third_party", ".build",
    "upstream", "packages", "$RECYCLE.BIN", "System Volume Information",
];

fn seconds(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH).ok().map(|duration| duration.as_secs() as i64)
}

fn split(path: &str) -> (String, String) {
    let path = path.trim_end_matches('\\');
    match path.rfind('\\') {
        Some(slash) if slash > 0 => {
            let folder = &path[..slash];
            let folder = if folder.ends_with(':') { format!("{folder}\\") } else { folder.to_string() };
            (path[slash + 1..].to_string(), folder)
        }
        _ => (path.to_string(), String::new()),
    }
}

impl HotIndex {
    pub fn empty() -> HotIndex {
        HotIndex { entries: Vec::new(), built: Instant::now(), build_time: Duration::ZERO }
    }

    pub fn build(sources: &Sources) -> HotIndex {
        let started = Instant::now();
        let mut entries: HashMap<String, Candidate> = HashMap::new();
        let mut add = |candidate: Candidate| {
            let key = candidate.key();
            match entries.get_mut(&key) {
                Some(existing) => {
                    existing.origin = existing.origin.min(candidate.origin);
                    existing.opens = existing.opens.max(candidate.opens);
                    existing.opened = existing.opened.max(candidate.opened);
                }
                None => {
                    entries.insert(key, candidate);
                }
            }
        };

        for folder in &sources.programs {
            walk(folder, 6, usize::MAX, &mut |path, metadata| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                let shortcut = ["lnk", "url", "appref-ms"].iter().any(|ext| {
                    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
                });
                if !shortcut || name.to_lowercase().starts_with("uninstall") {
                    return None;
                }
                Some(Candidate {
                    name,
                    folder: path.parent()?.display().to_string(),
                    modified: metadata.modified().ok().and_then(seconds),
                    origin: Some(Origin::App),
                    ..Candidate::default()
                })
            }, &mut add);
        }

        for (target, opened) in &sources.recent {
            let Ok(metadata) = std::fs::metadata(target) else { continue };
            let (name, folder) = split(target);
            add(Candidate {
                name,
                folder,
                is_folder: metadata.is_dir(),
                size: metadata.is_file().then(|| metadata.len()),
                modified: metadata.modified().ok().and_then(seconds),
                opened: Some(*opened),
                origin: Some(Origin::Recent),
                ..Candidate::default()
            });
        }

        for (path, opens, last) in &sources.history {
            let Ok(metadata) = std::fs::metadata(path) else { continue };
            let (name, folder) = split(path);
            add(Candidate {
                name,
                folder,
                is_folder: metadata.is_dir(),
                size: metadata.is_file().then(|| metadata.len()),
                modified: metadata.modified().ok().and_then(seconds),
                opens: *opens,
                opened: Some(*last),
                origin: Some(Origin::History),
                ..Candidate::default()
            });
        }

        let mut remaining = sources.cap;
        for folder in &sources.folders {
            remaining -= walk(folder, sources.depth, remaining, &mut |path, metadata| {
                Some(Candidate {
                    name: path.file_name()?.to_string_lossy().into_owned(),
                    folder: path.parent()?.display().to_string(),
                    is_folder: metadata.is_dir(),
                    size: metadata.is_file().then(|| metadata.len()),
                    modified: metadata.modified().ok().and_then(seconds),
                    hidden: is_hidden(metadata),
                    origin: Some(Origin::Folder),
                    ..Candidate::default()
                })
            }, &mut add);
        }

        let entries = entries.into_values().map(|candidate| Entry { candidate }).collect();
        HotIndex { entries, built: Instant::now(), build_time: started.elapsed() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries whose names are within `most` letters of `word` (`rank::closeness`), nearest first: what a word
    /// nothing has was likely meant to be.
    pub fn closest(&self, word: &str, filter: Filter, most: usize) -> Vec<(usize, Candidate)> {
        let mut near: Vec<(usize, Candidate)> = self.entries.iter()
            .filter(|entry| filter.accepts(&entry.candidate.name, entry.candidate.is_folder))
            .filter_map(|entry| {
                let distance = crate::rank::closeness(word, &entry.candidate.name);
                (distance <= most).then(|| (distance, entry.candidate.clone()))
            })
            .collect();
        near.sort_by_key(|(distance, _)| *distance);
        near
    }

    /// Every entry of a kind (for a match of the app's own, as the fuzzy search's).
    pub fn each(&self, filter: Filter) -> impl Iterator<Item = &Candidate> {
        self.entries.iter().map(|entry| &entry.candidate).filter(move |candidate| filter.accepts(&candidate.name, candidate.is_folder))
    }

    /// The best `limit` entries for the words. With no words: the most recently opened and the apps, for the
    /// popup's first view.
    pub fn search(&self, terms: &Terms, filter: Filter, context: &Context, limit: usize) -> Vec<Ranked> {
        // (With nothing typed, the popup's first view stands.)
        if !terms.is_empty() && terms.only_everything() {
            return Vec::new();
        }
        let mut ranked: Vec<Ranked> = self
            .entries
            .iter()
            .filter(|entry| filter.accepts(&entry.candidate.name, entry.candidate.is_folder))
            .filter(|entry| !terms.words.is_empty() || entry.candidate.opened.is_some())
            .filter_map(|entry| {
                let (quality, highlights) = name_match(&entry.candidate.name, terms)?;
                Some(Ranked { score: score(&entry.candidate, quality, context), candidate: entry.candidate.clone(), highlights })
            })
            .collect();
        if terms.words.is_empty() {
            ranked.sort_by(|a, b| b.candidate.opened.cmp(&a.candidate.opened));
        } else {
            ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
        }
        ranked.truncate(limit);
        ranked
    }
}

#[cfg(windows)]
fn is_hidden(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x6 != 0 // hidden or system
}

#[cfg(not(windows))]
fn is_hidden(_: &std::fs::Metadata) -> bool {
    false
}

/// Walks `root` breadth-first up to `depth` levels and `cap` entries, skipping dependency/build folders and hidden
/// ones; `make` turns an entry into a candidate. Returns how many were added.
fn walk(root: &Path, depth: usize, cap: usize, make: &mut dyn FnMut(&Path, &std::fs::Metadata) -> Option<Candidate>,
        add: &mut dyn FnMut(Candidate)) -> usize {
    let mut added = 0;
    let mut level = vec![root.to_path_buf()];
    for _ in 0..depth {
        let mut next = Vec::new();
        for folder in level {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries.flatten() {
                if added >= cap {
                    return added;
                }
                let Ok(metadata) = entry.metadata() else { continue };
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if metadata.is_dir() {
                    if name.starts_with('.') || SKIP.iter().any(|skip| skip.eq_ignore_ascii_case(&name)) || is_hidden(&metadata) {
                        continue;
                    }
                    next.push(path.clone());
                }
                if let Some(candidate) = make(&path, &metadata) {
                    add(candidate);
                    added += 1;
                }
            }
        }
        level = next;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_searches() {
        let root = std::env::temp_dir().join(format!("excavate-hot-{}", std::process::id()));
        let programs = root.join("Programs");
        let documents = root.join("Documents");
        std::fs::create_dir_all(programs.join("Tools")).unwrap();
        std::fs::create_dir_all(documents.join("node_modules").join("x")).unwrap();
        std::fs::create_dir_all(documents.join("Taxes")).unwrap();
        std::fs::write(programs.join("Tools").join("Budget Planner.lnk"), b"").unwrap();
        std::fs::write(programs.join("Uninstall Budget Planner.lnk"), b"").unwrap();
        std::fs::write(documents.join("Taxes").join("budget 2026.xlsx"), b"x").unwrap();
        std::fs::write(documents.join("node_modules").join("x").join("budget.js"), b"x").unwrap();
        let notes = documents.join("notes.txt");
        std::fs::write(&notes, b"x").unwrap();

        let sources = Sources {
            programs: vec![programs],
            recent: vec![(notes.display().to_string(), 1_800_000_000)],
            folders: vec![documents.clone()],
            depth: 3,
            cap: 100,
            ..Sources::default()
        };
        let index = HotIndex::build(&sources);
        let context = Context { now: 1_800_000_100, profile: String::new() };
        let found = index.search(&Terms::parse("budget"), Filter::All, &context, 10);
        let names: Vec<_> = found.iter().map(|ranked| ranked.candidate.name.as_str()).collect();
        assert_eq!(names, ["Budget Planner.lnk", "budget 2026.xlsx"], "no uninstaller, nothing from node_modules");

        let documents_only = index.search(&Terms::parse("budget"), Filter::Documents, &context, 10);
        assert_eq!(documents_only.len(), 1);

        // Nothing typed: what was opened lately.
        let recent = index.search(&Terms::parse(""), Filter::All, &context, 10);
        assert_eq!(recent[0].candidate.name, "notes.txt");
        assert_eq!(recent[0].candidate.origin, Some(Origin::Recent), "a file both recent and in a folder is recent");
        let _ = std::fs::remove_dir_all(root);
    }
}
