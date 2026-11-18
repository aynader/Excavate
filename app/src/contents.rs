//! Searching the content index from the popup, on a thread of its own so typing never waits: requests superseded by a
//! newer keystroke are dropped before they run. The index is opened when it first exists (the indexer creates it) and
//! refreshed at most once a second, so what was just read shows up. The same thread asks the indexer's catalog whether
//! a picture or a video is still waiting to be read: until it's read, its secrets aren't known, so it's covered whole.

use content::{Hit, Query, Reader, Stored};
use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a file will be read for, as far as covering it goes: the text in a picture, the text a video shows, or
/// nothing (not read at all: outside the chosen folders, left out, its kind turned off, or not a picture or a video).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Nothing,
    PictureText,
    VideoText,
}

/// What the settings say will be read, to tell which files to ask the catalog about (`expect`).
pub struct ToRead {
    on: bool,
    video_text: bool,
    /// The chosen folders, lowercased, each ending in a backslash.
    folders: Vec<String>,
    /// Excavate's own folders, never read, the same way.
    own: Vec<String>,
    exclusions: config::Exclusions,
    kinds: config::FolderKinds,
}

impl ToRead {
    /// With secrets shown (blurring them off) nothing is covered, so nothing is waited for either.
    pub fn new(settings: &config::Settings, paths: &config::Paths) -> ToRead {
        let folder = |path: &str| format!("{}\\", path.trim_end_matches('\\').to_lowercase());
        ToRead {
            on: settings.blur_secrets && settings.setup_done && settings.reading() != config::Reading::Off,
            video_text: settings.video_text,
            folders: settings.folders.iter().map(|path| folder(path)).collect(),
            own: [&paths.local, &paths.roaming].iter().map(|path| folder(&path.display().to_string())).collect(),
            exclusions: settings.exclusions(),
            kinds: settings.kinds(),
        }
    }

    pub fn expect(&self, path: &str, picture: bool, video: bool) -> Expect {
        if !self.on || !(picture || video) {
            return Expect::Nothing;
        }
        let lower = path.to_lowercase();
        if !self.folders.iter().any(|folder| lower.starts_with(folder.as_str())) || self.own.iter().any(|folder| lower.starts_with(folder.as_str()))
            || self.exclusions.excludes(path) {
            return Expect::Nothing;
        }
        if picture && self.kinds.on(path, "image-text") {
            Expect::PictureText
        } else if video && self.video_text && self.kinds.on(path, "video-text") {
            Expect::VideoText
        } else {
            Expect::Nothing
        }
    }
}

/// What the index knows about a file to cover it: whether it has secrets, and whether it's still waiting to be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cover {
    pub secrets: bool,
    pub waiting: bool,
}

type StoredReply = Box<dyn FnOnce(Option<Stored>, bool) + Send>;

enum Request {
    Search { generation: u64, query: Query, limit: usize, reply: Box<dyn FnOnce(u64, Vec<Hit>, Duration) + Send> },
    Stored { path: String, expect: Expect, reply: StoredReply },
    Covers { files: Vec<(String, Expect)>, reply: Box<dyn FnOnce(Vec<Cover>) + Send> },
    Whole { path: String, reply: Box<dyn FnOnce(Vec<Stored>) + Send> },
}

pub struct Contents {
    sender: Sender<Request>,
    copies: Arc<Mutex<Arc<Copies>>>,
}

/// Images with the same contents in several places, as the indexer's catalog knows them (it hashes an image when
/// another has its size, to read copies once). Reloaded at most every half minute.
#[derive(Default)]
pub struct Copies {
    hash_of: HashMap<String, u64>,
    places: HashMap<u64, Vec<String>>,
}

impl Copies {
    /// From the catalog's (path, hash) pairs: only contents found in more than one place are kept.
    fn from_pairs(pairs: impl IntoIterator<Item = (String, u64)>) -> Copies {
        let mut places: HashMap<u64, Vec<String>> = HashMap::new();
        for (path, hash) in pairs {
            places.entry(hash).or_default().push(path);
        }
        places.retain(|_, paths| paths.len() > 1);
        let hash_of = places.iter().flat_map(|(hash, paths)| paths.iter().map(move |path| (path.to_lowercase(), *hash))).collect();
        Copies { hash_of, places }
    }

    /// What `path`'s contents are known by, when it has copies.
    pub fn hash(&self, path: &str) -> Option<u64> {
        self.hash_of.get(&path.to_lowercase()).copied()
    }

    /// The other places with the same contents as `path`.
    pub fn others(&self, path: &str) -> Vec<String> {
        let lower = path.to_lowercase();
        self.hash(path).and_then(|hash| self.places.get(&hash))
            .map(|paths| paths.iter().filter(|other| other.to_lowercase() != lower).cloned().collect())
            .unwrap_or_default()
    }
}

impl Contents {
    pub fn start(index: PathBuf, catalog: PathBuf) -> Contents {
        let (sender, receiver) = channel();
        let copies = Arc::new(Mutex::new(Arc::new(Copies::default())));
        let shared = copies.clone();
        std::thread::Builder::new()
            .name("contents".into())
            .spawn(move || serve(index, catalog, receiver, shared))
            .expect("can't start the content search thread");
        Contents { sender, copies }
    }

    /// The copies known now (folded into one result each: "+3 copies").
    pub fn copies(&self) -> Arc<Copies> {
        self.copies.lock().map(|copies| copies.clone()).unwrap_or_default()
    }

    pub fn search(&self, generation: u64, query: Query, limit: usize, reply: impl FnOnce(u64, Vec<Hit>, Duration) + Send + 'static) {
        let _ = self.sender.send(Request::Search { generation, query, limit, reply: Box::new(reply) });
    }

    /// The index's copy of a file, and whether it's still waiting to be read for `expect`.
    pub fn stored(&self, path: String, expect: Expect, reply: impl FnOnce(Option<Stored>, bool) + Send + 'static) {
        let _ = self.sender.send(Request::Stored { path, expect, reply: Box::new(reply) });
    }

    /// Everything the index keeps of a file (a video's text and its speech are two), for showing all of it.
    pub fn whole(&self, path: String, reply: impl FnOnce(Vec<Stored>) + Send + 'static) {
        let _ = self.sender.send(Request::Whole { path, reply: Box::new(reply) });
    }

    /// For each file, whether it has secrets and whether it's still waiting to be read (rows' thumbnails).
    pub fn covers(&self, files: Vec<(String, Expect)>, reply: impl FnOnce(Vec<Cover>) + Send + 'static) {
        let _ = self.sender.send(Request::Covers { files, reply: Box::new(reply) });
    }
}

fn serve(index: PathBuf, catalog: PathBuf, requests: Receiver<Request>, copies: Arc<Mutex<Arc<Copies>>>) {
    let mut reader: Option<Reader> = None;
    let mut tried = Instant::now() - Duration::from_secs(60);
    let mut catalog = Catalog { path: catalog, db: None, tried: Instant::now() - Duration::from_secs(60) };
    let mut copies_at: Option<Instant> = None;
    while let Ok(first) = requests.recv() {
        if copies_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(30)) {
            catalog.open();
            if let Some(db) = catalog.db.as_ref() {
                copies_at = Some(Instant::now());
                if let Ok(mut slot) = copies.lock() {
                    *slot = Arc::new(load_copies(db));
                }
            }
        }
        // Only the newest search matters; previews are all answered.
        let mut search = None;
        let mut stored = Vec::new();
        for request in std::iter::once(first).chain(requests.try_iter()) {
            match request {
                Request::Search { .. } => search = Some(request),
                Request::Stored { .. } | Request::Covers { .. } | Request::Whole { .. } => stored.push(request),
            }
        }
        // The catalog first: a file it says is read has its words committed already, so the index is looked at
        // after it, and fresh then (else its copy could be from before, without the marks that cover its secrets).
        let mut read_now = false;
        let waits: Vec<Vec<bool>> = stored.iter().map(|request| {
            let asked: Vec<(&str, Expect)> = match request {
                Request::Stored { path, expect, .. } => vec![(path.as_str(), *expect)],
                Request::Covers { files, .. } => files.iter().map(|(path, expect)| (path.as_str(), *expect)).collect(),
                Request::Search { .. } | Request::Whole { .. } => Vec::new(),
            };
            asked.into_iter().map(|(path, expect)| {
                let waiting = catalog.waiting(path, expect);
                read_now |= expect != Expect::Nothing && !waiting;
                waiting
            }).collect()
        }).collect();
        if reader.is_none() && (read_now || tried.elapsed() >= Duration::from_secs(2)) {
            tried = Instant::now();
            reader = Reader::open(&index).ok();
        }
        if let Some(reader) = reader.as_mut() {
            if read_now { reader.refresh_now() } else { reader.refresh() }
        }
        if let Some(Request::Search { generation, query, limit, reply }) = search {
            let started = Instant::now();
            let hits = reader.as_ref().map(|reader| reader.search(&query, limit)).unwrap_or_default();
            reply(generation, hits, started.elapsed());
        }
        for (request, waits) in stored.into_iter().zip(waits) {
            match request {
                Request::Stored { path, reply, .. } => {
                    reply(reader.as_ref().and_then(|reader| reader.stored(&path)), waits.first().copied().unwrap_or(false));
                }
                Request::Covers { files, reply } => {
                    let covers = files.iter().zip(waits).map(|((path, _), waiting)| Cover {
                        secrets: reader.as_ref().and_then(|reader| reader.stored(path)).is_some_and(|stored| has_secrets(&stored.marks)),
                        waiting,
                    }).collect();
                    reply(covers);
                }
                Request::Whole { path, reply } => reply(reader.as_ref().map(|reader| reader.stored_all(&path)).unwrap_or_default()),
                Request::Search { .. } => {}
            }
        }
    }
}

/// Whether a file's privacy marks name any secret.
fn has_secrets(marks: &[u8]) -> bool {
    !marks.is_empty() && serde_json::from_slice::<serde_json::Value>(marks).is_ok_and(|json| json["kinds"].as_array().is_some_and(|kinds| !kinds.is_empty()))
}

/// The indexer's catalog (SQLite), opened read-only once it exists.
struct Catalog {
    path: PathBuf,
    db: Option<rusqlite::Connection>,
    tried: Instant,
}

impl Catalog {
    /// Whether a file is still waiting to be read for `expect`. Anything unsure (the catalog can't be read, the file
    /// is gone or kept online only) counts as not waiting: nothing is covered for good by mistake.
    /// Opens it once it exists (tried at most every two seconds).
    fn open(&mut self) {
        if self.db.is_none() && self.tried.elapsed() >= Duration::from_secs(2) {
            self.tried = Instant::now();
            if self.path.exists() {
                use rusqlite::OpenFlags;
                self.db = rusqlite::Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                    .map_err(|error| config::log(&format!("the catalog, to tell what's read: {error}")))
                    .ok();
            }
        }
    }

    fn waiting(&mut self, path: &str, expect: Expect) -> bool {
        if expect == Expect::Nothing {
            return false;
        }
        self.open();
        match expect {
            Expect::PictureText => {
                use std::os::windows::fs::MetadataExt;
                let Ok(metadata) = std::fs::metadata(path) else { return false };
                // Kept online only (OneDrive's "Free up space"): never read, so never waited for.
                if metadata.file_attributes() & NOT_ON_THIS_PC != 0 {
                    return false;
                }
                let modified = metadata.modified().ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_secs() as i64);
                match &self.db {
                    Some(db) => picture_waiting(db, path, metadata.len(), modified),
                    // Nothing read yet at all (the indexer hasn't made its catalog).
                    None => !self.path.exists(),
                }
            }
            Expect::VideoText => self.db.as_ref().is_some_and(|db| video_waiting(db, path)),
            Expect::Nothing => false,
        }
    }
}

/// The images read (the catalog's states 1 and 3, as its own `hashes`) that have a contents hash.
fn load_copies(db: &rusqlite::Connection) -> Copies {
    let Ok(mut statement) = db.prepare("SELECT path, hash FROM files WHERE hash != 0 AND state IN (1, 3)") else { return Copies::default() };
    let Ok(rows) = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))) else { return Copies::default() };
    Copies::from_pairs(rows.flatten())
}

/// Files kept online only (the indexer's `scan::NOT_ON_THIS_PC`).
pub const NOT_ON_THIS_PC: u32 = 0x1000 | 0x4_0000 | 0x40_0000;

/// A picture waits when the catalog has no record of it as it is now (its size and date), or recorded it with its text
/// left out (`catalog::SKIPPED`), which its folder now reads. A failed read is a record: shown, not waited for.
fn picture_waiting(db: &rusqlite::Connection, path: &str, size: u64, modified: i64) -> bool {
    let row = db.query_row("SELECT size, modified, engine FROM files WHERE id = ?1", [path.to_lowercase()], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
    });
    match row {
        Ok((known_size, known_modified, engine)) => known_size as u64 != size || known_modified != modified || engine == "left out",
        Err(rusqlite::Error::QueryReturnedNoRows) => true,
        Err(error) => {
            config::log(&format!("the catalog, to tell what's read: {error}"));
            false
        }
    }
}

/// A video waits when it's listed with its text to read and that hasn't been read as it is now (`video_text::ENGINE`).
fn video_waiting(db: &rusqlite::Connection, path: &str) -> bool {
    db.query_row(
        "SELECT s.modified IS NULL OR s.modified != v.modified FROM videos v LEFT JOIN seen s ON s.id = v.id AND s.engine = 'video-text'
         WHERE v.id = ?1 AND (v.kinds & 1) != 0",
        [path.to_lowercase()],
        |row| row.get::<_, bool>(0),
    ).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(folder: &str) -> config::Settings {
        config::Settings {
            setup_done: true,
            blur_secrets: true,
            pictures_on: true,
            reading: "balanced".into(),
            folders: vec![folder.into()],
            exclude: vec!["Private".into()],
            video_text: false,
            ..config::Settings::default()
        }
    }

    #[test]
    fn waits_only_for_what_will_be_read() {
        let paths = config::Paths { roaming: r"C:\Users\pat\AppData\Roaming\Excavate".into(), local: r"C:\Users\pat\AppData\Local\Excavate".into() };
        let mut chosen = settings(r"C:\Users\pat\Pictures");
        chosen.folder_kinds.insert(r"c:\users\pat\pictures\scans".into(), vec!["image-text".into()]);
        let to_read = ToRead::new(&chosen, &paths);
        assert_eq!(to_read.expect(r"C:\Users\pat\Pictures\board.png", true, false), Expect::PictureText);
        assert_eq!(to_read.expect(r"C:\Users\pat\Downloads\board.png", true, false), Expect::Nothing, "outside the chosen folders");
        assert_eq!(to_read.expect(r"C:\Users\pat\Pictures2\board.png", true, false), Expect::Nothing, "a folder named like it");
        assert_eq!(to_read.expect(r"C:\Users\pat\Pictures\Private\id.png", true, false), Expect::Nothing, "left out");
        assert_eq!(to_read.expect(r"C:\Users\pat\Pictures\clip.mp4", false, true), Expect::Nothing, "text in videos off");
        assert_eq!(to_read.expect(r"C:\Users\pat\Pictures\notes.txt", false, false), Expect::Nothing);
        let videos = ToRead::new(&config::Settings { video_text: true, ..chosen.clone() }, &paths);
        assert_eq!(videos.expect(r"C:\Users\pat\Pictures\clip.mp4", false, true), Expect::VideoText);
        let shown = ToRead::new(&config::Settings { blur_secrets: false, ..chosen.clone() }, &paths);
        assert_eq!(shown.expect(r"C:\Users\pat\Pictures\board.png", true, false), Expect::Nothing, "secrets shown: nothing covered");
        let off = ToRead::new(&config::Settings { pictures_on: false, ..chosen.clone() }, &paths);
        assert_eq!(off.expect(r"C:\Users\pat\Pictures\board.png", true, false), Expect::Nothing, "text in images off");
        // Its text left out by a folder chosen inside the other.
        let mut inner = chosen;
        inner.folders.push(r"C:\Users\pat\Pictures\Scans".into());
        inner.folder_kinds.insert(r"c:\users\pat\pictures\scans".into(), vec!["image-text".into()]);
        let inner = ToRead::new(&inner, &paths);
        assert_eq!(inner.expect(r"C:\Users\pat\Pictures\Scans\id.png", true, false), Expect::Nothing, "its text turned off there");
        assert_eq!(inner.expect(r"C:\Users\pat\Pictures\board.png", true, false), Expect::PictureText);
    }

    #[test]
    fn copies_are_the_same_contents_in_more_than_one_place() {
        let copies = Copies::from_pairs([(r"C:\Pictures\beach.jpg".to_string(), 7), (r"C:\Desktop\IMG_1.jpg".to_string(), 7),
                                          (r"C:\Pictures\alone.jpg".to_string(), 9)]);
        assert_eq!(copies.others(r"c:\pictures\BEACH.jpg"), [r"C:\Desktop\IMG_1.jpg"]);
        assert_eq!(copies.hash(r"C:\Desktop\IMG_1.jpg"), Some(7));
        assert!(copies.others(r"C:\Pictures\alone.jpg").is_empty() && copies.hash(r"C:\Pictures\alone.jpg").is_none());
    }

    #[test]
    fn the_catalog_says_what_waits() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        // The indexer's tables, as far as these questions go.
        db.execute_batch(
            "CREATE TABLE files (id TEXT PRIMARY KEY, path TEXT NOT NULL, size INTEGER NOT NULL, modified INTEGER NOT NULL, kind TEXT NOT NULL,
                                 state INTEGER NOT NULL, engine TEXT NOT NULL DEFAULT '', read_at INTEGER NOT NULL);
             CREATE TABLE seen (id TEXT NOT NULL, engine TEXT NOT NULL, modified INTEGER NOT NULL, at INTEGER NOT NULL, ok INTEGER NOT NULL,
                                PRIMARY KEY (id, engine));
             CREATE TABLE videos (id TEXT PRIMARY KEY, path TEXT NOT NULL, size INTEGER NOT NULL, modified INTEGER NOT NULL, kinds INTEGER NOT NULL);
             INSERT INTO files VALUES ('d:\\read.png', 'D:\\read.png', 100, 50, 'picture', 1, 'PP-OCRv6 small', 60);
             INSERT INTO files VALUES ('d:\\failed.png', 'D:\\failed.png', 100, 50, 'picture', 2, 'PP-OCRv6 small', 60);
             INSERT INTO files VALUES ('d:\\left.png', 'D:\\left.png', 100, 50, 'picture', 3, 'left out', 60);
             INSERT INTO videos VALUES ('d:\\read.mp4', 'D:\\read.mp4', 9, 70, 7);
             INSERT INTO videos VALUES ('d:\\new.mp4', 'D:\\new.mp4', 9, 70, 7);
             INSERT INTO videos VALUES ('d:\\changed.mp4', 'D:\\changed.mp4', 9, 80, 7);
             INSERT INTO videos VALUES ('d:\\shown.mp4', 'D:\\shown.mp4', 9, 70, 2);
             INSERT INTO seen VALUES ('d:\\read.mp4', 'video-text', 70, 1, 1);
             INSERT INTO seen VALUES ('d:\\changed.mp4', 'video-text', 70, 1, 1);",
        ).unwrap();
        assert!(!picture_waiting(&db, r"D:\Read.png", 100, 50));
        assert!(picture_waiting(&db, r"D:\read.png", 120, 55), "changed since it was read");
        assert!(picture_waiting(&db, r"D:\new.png", 100, 50), "not read yet");
        assert!(!picture_waiting(&db, r"D:\failed.png", 100, 50), "a failed read isn't waited for");
        assert!(picture_waiting(&db, r"D:\left.png", 100, 50), "its text left out before, read now");
        assert!(!video_waiting(&db, r"D:\read.mp4"));
        assert!(video_waiting(&db, r"D:\new.mp4"));
        assert!(video_waiting(&db, r"D:\changed.mp4"));
        assert!(!video_waiting(&db, r"D:\shown.mp4"), "its text isn't read in its folder");
        assert!(!video_waiting(&db, r"D:\unlisted.mp4"));
    }

    #[test]
    fn secrets_in_marks() {
        assert!(!has_secrets(b""));
        assert!(!has_secrets(br#"{"kinds":[],"similar":3}"#));
        assert!(has_secrets(br#"{"kinds":["password"]}"#));
    }
}
