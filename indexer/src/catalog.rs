//! What the indexer knows about each file (SQLite): its size and date when last read, how that went, and which OCR
//! engine read it. Compared against what's on disk to read only what changed.

use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Read and indexed.
    Read = 1,
    /// Couldn't be read (the error is kept; tried again when the file changes).
    Failed = 2,
    /// Read, nothing in it (a photo without text).
    Empty = 3,
}

impl State {
    fn from(value: i64) -> State {
        match value {
            2 => State::Failed,
            3 => State::Empty,
            _ => State::Read,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub size: u64,
    pub modified: i64,
    pub state: State,
    pub read_at: i64,
    /// An image whose text wasn't read, its folder leaving that out (`SKIPPED`): read when that's turned back on.
    pub skipped: bool,
}

/// The engine recorded for an image whose text its folder leaves out.
pub const SKIPPED: &str = "left out";

/// The error recorded for a file "Skip" let go while its text was read (a scanned PDF's pages): it's listed with the
/// other skipped ones (`skipped`), not among the damaged.
pub const SKIPPED_ERROR: &str = "Skipped";

/// What couldn't be read, shown in a group of `config::why`: up to this many files each (the counts are of them all).
pub const PROBLEM_FILES: usize = 30;

#[derive(Clone, Debug)]
pub struct Update {
    pub path: String,
    pub size: u64,
    pub modified: i64,
    pub kind: &'static str,
    pub state: State,
    pub engine: String,
    pub read_at: i64,
    pub error: String,
    pub characters: u64,
    /// Whether Deep should read it: `deep::` values.
    pub deep: i64,
    /// From the first sweep: its group (0: none), and whether it comes after the sweep.
    pub group: u32,
    pub later: bool,
    /// Its contents' hash, when another image has its size (0: not known): copies are read once.
    pub hash: u64,
}

/// A picture Deep has nothing to do for (read by it, or nothing to read).
pub const DEEP_NO: i64 = 0;
/// A picture for Deep.
pub const DEEP_WANTED: i64 = 1;
/// A picture for Deep, before the others: the first pass couldn't make sense of it.
pub const DEEP_FIRST: i64 = 2;
/// A picture Deep couldn't read (tried again when the file changes).
pub const DEEP_FAILED: i64 = 3;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct Counts {
    pub files: u64,
    pub read: u64,
    pub failed: u64,
    pub pictures: u64,
}

pub struct Catalog {
    db: Mutex<Connection>,
    /// Chosen folders (lowercased) whose images aren't looked at for what they show.
    unshown: Mutex<Vec<String>>,
}

/// A file's id as a number (FNV-1a, 64 bits): the same in every run.
pub fn hashed(id: &str) -> u64 {
    id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

pub fn id(path: &str) -> String {
    path.to_lowercase()
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Catalog, String> {
        let db = Connection::open(path).map_err(|error| error.to_string())?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS files (
                 id TEXT PRIMARY KEY,
                 path TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 modified INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 state INTEGER NOT NULL,
                 engine TEXT NOT NULL DEFAULT '',
                 read_at INTEGER NOT NULL,
                 error TEXT NOT NULL DEFAULT '',
                 characters INTEGER NOT NULL DEFAULT 0,
                 deep INTEGER NOT NULL DEFAULT 0,
                 grp INTEGER NOT NULL DEFAULT 0,
                 later INTEGER NOT NULL DEFAULT 0,
                 hash INTEGER NOT NULL DEFAULT 0
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS seen (
                 id TEXT NOT NULL,
                 engine TEXT NOT NULL,
                 modified INTEGER NOT NULL,
                 at INTEGER NOT NULL,
                 ok INTEGER NOT NULL,
                 PRIMARY KEY (id, engine)
             ) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS videos (id TEXT PRIMARY KEY, path TEXT NOT NULL, size INTEGER NOT NULL, modified INTEGER NOT NULL,
                                                  kinds INTEGER NOT NULL DEFAULT 7) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS recordings (id TEXT PRIMARY KEY, path TEXT NOT NULL, size INTEGER NOT NULL, modified INTEGER NOT NULL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS skipped (id TEXT NOT NULL, kind TEXT NOT NULL, path TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (id, kind)) WITHOUT ROWID;",
        )
        .map_err(|error| error.to_string())?;
        db.execute_batch("CREATE INDEX IF NOT EXISTS files_deep ON files (deep, modified) WHERE deep IN (1, 2);").map_err(|error| error.to_string())?;
        // `errors()`: the failures alone, newest first, instead of every row sorted (21 ms for 20,000 files).
        db.execute_batch("CREATE INDEX IF NOT EXISTS files_failed ON files (read_at) WHERE state = 2;").map_err(|error| error.to_string())?;
        Ok(Catalog { db: Mutex::new(db), unshown: Mutex::new(Vec::new()) })
    }

    pub fn meta(&self, key: &str) -> i64 {
        self.db().query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0)).unwrap_or(0)
    }

    pub fn set_meta(&self, key: &str, value: i64) {
        let _ = self.db().execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value]);
    }

    /// Pictures the first pass read before Deep was chosen: Deep reads them too.
    pub fn deep_wants_all(&self, engine: &str) {
        let _ = self.db().execute(
            "UPDATE files SET deep = 1 WHERE kind = 'picture' AND state = 1 AND deep = 0 AND engine != ?1",
            [engine],
        );
    }

    /// The next pictures for Deep: (path, size, modified), the doubtful first, then the first sweep's, newest first.
    pub fn deep_next(&self, limit: usize) -> Vec<(String, u64, i64)> {
        let db = self.db();
        let Ok(mut statement) = db.prepare("SELECT path, size, modified FROM files WHERE deep IN (1, 2) ORDER BY deep DESC, later ASC, modified DESC LIMIT ?1") else {
            return Vec::new();
        };
        statement
            .query_map([limit as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn deep_count(&self) -> u64 {
        self.db().query_row("SELECT count(*) FROM files WHERE deep IN (1, 2)", [], |row| row.get::<_, i64>(0)).unwrap_or(0) as u64
    }

    /// Images Deep has finished with, from any session: read by it, or it couldn't.
    pub fn deep_done(&self, engine: &str) -> u64 {
        self.db().query_row("SELECT count(*) FROM files WHERE kind = 'picture' AND (engine = ?1 OR deep = 3)", [engine], |row| row.get::<_, i64>(0)).unwrap_or(0) as u64
    }

    /// Images already read that have a hash: the first of each.
    pub fn hashes(&self) -> HashMap<u64, String> {
        let db = self.db();
        let mut hashes = HashMap::new();
        let Ok(mut statement) = db.prepare("SELECT hash, path FROM files WHERE hash != 0 AND state IN (1, 3)") else { return hashes };
        if let Ok(rows) = statement.query_map([], |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))) {
            for (hash, path) in rows.flatten() {
                hashes.entry(hash).or_insert(path);
            }
        }
        hashes
    }

    /// Deep is done with a picture without changing what's indexed.
    pub fn deep_set(&self, path: &str, deep: i64) {
        let _ = self.db().execute("UPDATE files SET deep = ?2 WHERE id = ?1", params![id(path), deep]);
    }

    /// The next images for a model that looks at them: (path, size, modified), read by the first pass (with or without
    /// text) and not looked at as they are now since `relook_at`; the first sweep's first, newest first.
    pub fn seen_next(&self, engine: &str, relook_at: i64, limit: usize) -> Vec<(String, u64, i64)> {
        let shown = self.shown_only();
        let db = self.db();
        let Ok(mut statement) = db.prepare(&format!(
            "SELECT f.path, f.size, f.modified FROM files f LEFT JOIN seen s ON s.id = f.id AND s.engine = ?1
             WHERE f.kind = 'picture' AND f.state IN (1, 3) AND (s.id IS NULL OR s.modified != f.modified OR s.at < ?2){shown}
             ORDER BY f.later ASC, f.modified DESC LIMIT ?3",
        )) else {
            return Vec::new();
        };
        statement
            .query_map(params![engine, relook_at, limit as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The chosen folders whose images aren't looked at for what they show.
    pub fn set_unshown(&self, folders: Vec<String>) {
        *self.unshown.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = folders;
    }

    /// The images left out of looking, as SQL: `AND f.id NOT LIKE 'c:\photos\%'` for each such folder.
    fn shown_only(&self) -> String {
        let folders = self.unshown.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        folders.iter().map(|folder| {
            let like: String = folder.chars().flat_map(|c| match c {
                '^' | '%' | '_' => vec!['^', c],
                '\'' => vec!['\'', '\''],
                c => vec![c],
            }).collect();
            format!(" AND f.id NOT LIKE '{like}\\%' ESCAPE '^'")
        }).collect()
    }

    /// For a model that looks at images: (looked at, as they are now; waiting).
    pub fn seen_counts(&self, engine: &str, relook_at: i64) -> (u64, u64) {
        let shown = self.shown_only();
        self.db()
            .query_row(
                &format!("SELECT sum(s.id IS NOT NULL AND s.modified = f.modified AND s.at >= ?2), sum(s.id IS NULL OR s.modified != f.modified OR s.at < ?2)
                 FROM files f LEFT JOIN seen s ON s.id = f.id AND s.engine = ?1 WHERE f.kind = 'picture' AND f.state IN (1, 3){shown}"),
                params![engine, relook_at],
                |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64, row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64)),
            )
            .unwrap_or((0, 0))
    }

    /// `seen_counts`, for the images under one folder.
    pub fn seen_counts_in(&self, folder: &str, engine: &str, relook_at: i64) -> (u64, u64) {
        let shown = self.shown_only();
        let (low, high) = range(folder);
        self.db()
            .query_row(
                &format!("SELECT sum(s.id IS NOT NULL AND s.modified = f.modified AND s.at >= ?2), sum(s.id IS NULL OR s.modified != f.modified OR s.at < ?2)
                 FROM files f LEFT JOIN seen s ON s.id = f.id AND s.engine = ?1 WHERE f.id >= ?3 AND f.id < ?4 AND f.kind = 'picture' AND f.state IN (1, 3){shown}"),
                params![engine, relook_at, low, high],
                |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64, row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64)),
            )
            .unwrap_or((0, 0))
    }

    /// A model has looked at an image as it was at `modified` (or couldn't: `ok` false, tried again when it changes).
    pub fn seen_set(&self, path: &str, engine: &str, modified: i64, at: i64, ok: bool) {
        let _ = self.db().execute(
            "INSERT OR REPLACE INTO seen (id, engine, modified, at, ok) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id(path), engine, modified, at, ok],
        );
    }

    /// The videos in the chosen folders now, in place of what was known, each with the kinds its folder reads
    /// (`config::video_kind` bits: its text, what it shows, what's said in it).
    pub fn videos_replace(&self, videos: &[(String, u64, i64, u8)]) {
        let mut db = self.db();
        let Ok(transaction) = db.transaction() else { return };
        let _ = transaction.execute("DELETE FROM videos", []);
        {
            let Ok(mut insert) = transaction.prepare_cached("INSERT OR REPLACE INTO videos (id, path, size, modified, kinds) VALUES (?1, ?2, ?3, ?4, ?5)") else { return };
            for (path, size, modified, kinds) in videos {
                let _ = insert.execute(params![id(path), path, *size as i64, modified, kinds]);
            }
        }
        let _ = transaction.commit();
    }

    /// The next videos for a model, newest first (a large one later: `work::size_wait`), as `seen_next` has images: of
    /// those whose folder reads `kind` (a `config::video_kind` bit).
    pub fn video_next(&self, engine: &str, relook_at: i64, limit: usize, kind: u8) -> Vec<(String, u64, i64)> {
        let db = self.db();
        let Ok(mut statement) = db.prepare(&format!(
            "SELECT v.path, v.size, v.modified FROM videos v LEFT JOIN seen s ON s.id = v.id AND s.engine = ?1
             WHERE (v.kinds & ?4) != 0 AND (s.id IS NULL OR s.modified != v.modified OR s.at < ?2) ORDER BY v.modified - {} DESC LIMIT ?3",
            crate::work::size_wait_sql("v.size"),
        )) else {
            return Vec::new();
        };
        statement
            .query_map(params![engine, relook_at, limit as i64, kind], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// The next documents for their meaning (anything read that isn't an image), newest first, as `seen_next` has images.
    pub fn text_next(&self, engine: &str, relook_at: i64, limit: usize) -> Vec<(String, u64, i64)> {
        let db = self.db();
        let Ok(mut statement) = db.prepare(
            "SELECT f.path, f.size, f.modified FROM files f LEFT JOIN seen s ON s.id = f.id AND s.engine = ?1
             WHERE f.kind != 'picture' AND f.state = 1 AND (s.id IS NULL OR s.modified != f.modified OR s.at < ?2)
             ORDER BY f.modified DESC LIMIT ?3",
        ) else {
            return Vec::new();
        };
        statement
            .query_map(params![engine, relook_at, limit as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// For their meaning, documents (done as they are now; waiting).
    pub fn text_counts(&self, engine: &str, relook_at: i64) -> (u64, u64) {
        self.db()
            .query_row(
                "SELECT sum(s.id IS NOT NULL AND s.modified = f.modified AND s.at >= ?2), sum(s.id IS NULL OR s.modified != f.modified OR s.at < ?2)
                 FROM files f LEFT JOIN seen s ON s.id = f.id AND s.engine = ?1 WHERE f.kind != 'picture' AND f.state = 1",
                params![engine, relook_at],
                |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64, row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64)),
            )
            .unwrap_or((0, 0))
    }

    /// Every document read for its text (lowercase paths): what the meaning vectors may keep.
    pub fn text_ids(&self) -> std::collections::HashSet<String> {
        let db = self.db();
        let Ok(mut statement) = db.prepare("SELECT id FROM files WHERE kind != 'picture' AND state = 1") else { return Default::default() };
        statement.query_map([], |row| row.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// For a model, videos (looked at as they are now; waiting).
    pub fn video_counts(&self, engine: &str, relook_at: i64, kind: u8) -> (u64, u64) {
        self.db()
            .query_row(
                "SELECT sum(s.id IS NOT NULL AND s.modified = v.modified AND s.at >= ?2), sum(s.id IS NULL OR s.modified != v.modified OR s.at < ?2)
                 FROM videos v LEFT JOIN seen s ON s.id = v.id AND s.engine = ?1 WHERE (v.kinds & ?3) != 0",
                params![engine, relook_at, kind],
                |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64, row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64)),
            )
            .unwrap_or((0, 0))
    }

    /// The recordings in the chosen folders now, in place of what was known (only the model that listens reads them).
    pub fn recordings_replace(&self, recordings: &[(String, u64, i64)]) {
        self.recordings_put(recordings, true);
    }

    /// New and changed recordings, keeping the rest (a later look that may not see every folder).
    pub fn recordings_add(&self, recordings: &[(String, u64, i64)]) {
        self.recordings_put(recordings, false);
    }

    fn recordings_put(&self, recordings: &[(String, u64, i64)], replace: bool) {
        let mut db = self.db();
        let Ok(transaction) = db.transaction() else { return };
        if replace {
            let _ = transaction.execute("DELETE FROM recordings", []);
        }
        {
            let Ok(mut insert) = transaction.prepare_cached("INSERT OR REPLACE INTO recordings (id, path, size, modified) VALUES (?1, ?2, ?3, ?4)") else { return };
            for (path, size, modified) in recordings {
                let _ = insert.execute(params![id(path), path, *size as i64, modified]);
            }
        }
        let _ = transaction.commit();
    }

    /// The next recordings for the model that listens (`engine`), newest first (a long one later: `work::size_wait`), as
    /// `seen_next` has images: not heard as they are now since `relisten_at`.
    pub fn recording_next(&self, engine: &str, relisten_at: i64, limit: usize) -> Vec<(String, u64, i64)> {
        let db = self.db();
        let Ok(mut statement) = db.prepare(&format!(
            "SELECT r.path, r.size, r.modified FROM recordings r LEFT JOIN seen s ON s.id = r.id AND s.engine = ?1
             WHERE s.id IS NULL OR s.modified != r.modified OR s.at < ?2 ORDER BY r.modified - {} DESC LIMIT ?3",
            crate::work::size_wait_sql("r.size"),
        )) else {
            return Vec::new();
        };
        statement
            .query_map(params![engine, relisten_at, limit as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64, row.get::<_, i64>(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Recordings (heard as they are now; waiting).
    pub fn recording_counts(&self, engine: &str, relisten_at: i64) -> (u64, u64) {
        self.db()
            .query_row(
                "SELECT sum(s.id IS NOT NULL AND s.modified = r.modified AND s.at >= ?2), sum(s.id IS NULL OR s.modified != r.modified OR s.at < ?2)
                 FROM recordings r LEFT JOIN seen s ON s.id = r.id AND s.engine = ?1",
                params![engine, relisten_at],
                |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64, row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64)),
            )
            .unwrap_or((0, 0))
    }

    /// `recording_counts`, for the recordings under one folder.
    pub fn recording_counts_in(&self, folder: &str, engine: &str, relisten_at: i64) -> (u64, u64) {
        let (low, high) = range(folder);
        self.db()
            .query_row(
                "SELECT sum(s.id IS NOT NULL AND s.modified = r.modified AND s.at >= ?2), sum(s.id IS NULL OR s.modified != r.modified OR s.at < ?2)
                 FROM recordings r LEFT JOIN seen s ON s.id = r.id AND s.engine = ?1 WHERE r.id >= ?3 AND r.id < ?4",
                params![engine, relisten_at, low, high],
                |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64, row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64)),
            )
            .unwrap_or((0, 0))
    }

    /// Paths a listening model heard that are no longer recordings in the chosen folders (gone, moved, left out): their
    /// words go from the index. `engines`: every model that listens.
    pub fn heard_gone(&self, engines: &[&str]) -> Vec<String> {
        self.gone_from("recordings", engines)
    }

    /// Paths whose text or speech a model took (`engines`: reading their text, the listeners) that are no longer videos
    /// in the chosen folders: their words go from the index.
    pub fn videos_gone(&self, engines: &[&str]) -> Vec<String> {
        self.gone_from("videos", engines)
    }

    /// Videos still in the chosen folders that a model read, though their folder now leaves its kind out (a
    /// `config::video_kind` bit): their id, each once.
    pub fn videos_left_out(&self, engines: &[&str], kind: u8) -> Vec<String> {
        let db = self.db();
        let list = engines.iter().map(|engine| format!("'{engine}'")).collect::<Vec<_>>().join(",");
        let Ok(mut statement) = db.prepare(&format!(
            "SELECT DISTINCT s.id FROM seen s JOIN videos v ON v.id = s.id WHERE s.engine IN ({list}) AND (v.kinds & ?1) = 0"
        )) else {
            return Vec::new();
        };
        statement.query_map([kind], |row| row.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    fn gone_from(&self, table: &str, engines: &[&str]) -> Vec<String> {
        let db = self.db();
        // (Excavate's own names: nothing to quote.)
        let list = engines.iter().map(|engine| format!("'{engine}'")).collect::<Vec<_>>().join(",");
        let Ok(mut statement) = db.prepare(&format!(
            "SELECT DISTINCT s.id FROM seen s LEFT JOIN {table} r ON r.id = s.id WHERE s.engine IN ({list}) AND r.id IS NULL"
        )) else {
            return Vec::new();
        };
        statement.query_map([], |row| row.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Forgets a file was read: it's read again at the next listing.
    pub fn forget(&self, id: &str) {
        let _ = self.db().execute("DELETE FROM files WHERE id = ?1", params![id]);
    }

    /// Forgets what the listening models heard of a path.
    pub fn heard_forget(&self, id: &str, engines: &[&str]) {
        for engine in engines {
            let _ = self.db().execute("DELETE FROM seen WHERE id = ?1 AND engine = ?2", params![id, engine]);
        }
    }

    /// Forgets what one engine made of every video (`seen`): each is read by it again.
    pub fn seen_forget_engine(&self, engine: &str) -> usize {
        self.db().execute("DELETE FROM seen WHERE engine = ?1", params![engine]).unwrap_or(0)
    }

    /// Every image read by the first pass, and every video (lowercase paths): what a model's vectors may keep.
    pub fn picture_ids(&self) -> std::collections::HashSet<String> {
        let db = self.db();
        let Ok(mut statement) = db.prepare("SELECT id FROM files WHERE kind = 'picture' AND state IN (1, 3) UNION SELECT id FROM videos") else { return Default::default() };
        statement.query_map([], |row| row.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Changes whenever anything is written to the catalog: rows this connection inserted, updated or deleted, plus
    /// commits by any other connection to the file. Equal values mean the counts would come out the same.
    pub fn version(&self) -> (u64, i64) {
        let db = self.db();
        let others = db.query_row("PRAGMA data_version", [], |row| row.get(0)).unwrap_or(0);
        (db.total_changes(), others)
    }

    /// Forgets everything (the index was rebuilt).
    pub fn clear(&self) {
        let _ = self.db().execute("DELETE FROM files", []);
        let _ = self.db().execute("DELETE FROM seen", []);
    }

    /// Every file known, by `hashed` id: a number a file instead of its path (the reader's start compares a listing
    /// with this; 50 MB for 131,000 files kept as paths, a few as numbers).
    pub fn all_hashed(&self) -> HashMap<u64, Entry> {
        let db = self.db();
        let mut entries = HashMap::new();
        let Ok(mut statement) = db.prepare("SELECT id, size, modified, state, read_at, engine FROM files") else { return entries };
        let rows = statement.query_map([], |row| {
            Ok((hashed(&row.get::<_, String>(0)?), Entry {
                size: row.get::<_, i64>(1)? as u64,
                modified: row.get(2)?,
                state: State::from(row.get(3)?),
                read_at: row.get(4)?,
                skipped: row.get::<_, String>(5)? == SKIPPED,
            }))
        });
        if let Ok(rows) = rows {
            for (id, entry) in rows.flatten() {
                entries.insert(id, entry);
            }
        }
        entries
    }

    /// The ids of the files known for which `keep` says yes, read one by one.
    pub fn ids_where(&self, mut keep: impl FnMut(&str) -> bool) -> Vec<String> {
        let db = self.db();
        let mut out = Vec::new();
        let Ok(mut statement) = db.prepare("SELECT id FROM files") else { return out };
        if let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) {
            for id in rows.flatten() {
                if keep(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    pub fn get(&self, path: &str) -> Option<Entry> {
        self.db()
            .query_row("SELECT size, modified, state, read_at, engine FROM files WHERE id = ?1", [id(path)], |row| {
                Ok(Entry {
                    size: row.get::<_, i64>(0)? as u64,
                    modified: row.get(1)?,
                    state: State::from(row.get(2)?),
                    read_at: row.get(3)?,
                    skipped: row.get::<_, String>(4)? == SKIPPED,
                })
            })
            .ok()
    }

    /// Records what was read, in one transaction.
    pub fn apply(&self, updates: &[Update], removed: &[String]) {
        if updates.is_empty() && removed.is_empty() {
            return;
        }
        let mut db = self.db();
        let Ok(transaction) = db.transaction() else { return };
        {
            let mut insert = transaction
                .prepare_cached(
                    "INSERT OR REPLACE INTO files (id, path, size, modified, kind, state, engine, read_at, error, characters, deep, grp, later, hash)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )
                .expect("the insert statement");
            for update in updates {
                let _ = insert.execute(params![
                    id(&update.path), update.path, update.size as i64, update.modified, update.kind, update.state as i64,
                    update.engine, update.read_at, update.error, update.characters as i64, update.deep,
                    update.group, update.later, update.hash as i64
                ]);
            }
            // A file read now isn't online only any more; one that's gone isn't anything that couldn't be read.
            let mut online = transaction.prepare_cached("DELETE FROM skipped WHERE id = ?1 AND kind = ?2").expect("the online statement");
            for update in updates {
                let _ = online.execute(params![id(&update.path), config::SKIP_ONLINE]);
            }
            let mut delete = transaction.prepare_cached("DELETE FROM files WHERE id = ?1").expect("the delete statement");
            let mut unseen = transaction.prepare_cached("DELETE FROM seen WHERE id = ?1").expect("the unseen statement");
            let mut unskipped = transaction.prepare_cached("DELETE FROM skipped WHERE id = ?1").expect("the unskipped statement");
            for path in removed {
                let _ = delete.execute([id(path)]);
                let _ = unseen.execute([id(path)]);
                let _ = unskipped.execute([id(path)]);
            }
        }
        let _ = transaction.commit();
    }

    /// Every file under a folder (to forget them all when it's removed or deleted).
    pub fn under(&self, folder: &str) -> Vec<String> {
        let (low, high) = range(folder);
        let db = self.db();
        let Ok(mut statement) = db.prepare("SELECT path FROM files WHERE id >= ?1 AND id < ?2") else { return Vec::new() };
        statement.query_map([low, high], |row| row.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// How far along a folder is.
    pub fn counts(&self, folder: &str) -> Counts {
        let (low, high) = range(folder);
        self.db()
            .query_row(
                "SELECT count(*), sum(state = 1 OR state = 3), sum(state = 2), sum(kind = 'picture') FROM files WHERE id >= ?1 AND id < ?2",
                [low, high],
                |row| {
                    Ok(Counts {
                        files: row.get::<_, i64>(0)? as u64,
                        read: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                        failed: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                        pictures: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                    })
                },
            )
            .unwrap_or_default()
    }

    /// The latest files that couldn't be read, and why.
    /// A file let go by "Skip" while one kind of work read it ("text in videos", "what it shows", "what's said"):
    /// among what couldn't be read, and not tried again until it changes (each lane marks it seen).
    pub fn skipped_add(&self, path: &str, kind: &str) {
        let _ = self.db().execute("INSERT OR REPLACE INTO skipped (id, kind, path, at) VALUES (?1, ?2, ?3, ?4)",
                                  params![id(path), kind, path, crate::work::now()]);
    }

    /// Files kept online only, found while listing: among what couldn't be read (since they were first found), in one
    /// transaction.
    pub fn online_add(&self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let mut db = self.db();
        let Ok(transaction) = db.transaction() else { return };
        {
            let Ok(mut insert) = transaction.prepare_cached("INSERT OR IGNORE INTO skipped (id, kind, path, at) VALUES (?1, ?2, ?3, ?4)") else { return };
            let now = crate::work::now();
            for path in paths {
                let _ = insert.execute(params![id(path), config::SKIP_ONLINE, path, now]);
            }
        }
        let _ = transaction.commit();
    }

    /// What couldn't be read, the latest first: files that failed, and what "Skip" let go.
    pub fn errors(&self, limit: usize) -> Vec<(String, String)> {
        let db = self.db();
        let Ok(mut statement) = db.prepare("SELECT path, error, read_at FROM files WHERE state = 2 AND error != ?2
                                            UNION ALL SELECT path, 'Skipped (' || kind || ')', at FROM skipped
                                            ORDER BY 3 DESC LIMIT ?1") else {
            return Vec::new();
        };
        statement
            .query_map(params![limit as i64, SKIPPED_ERROR], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    /// Everything that couldn't be read, counted by why (`config::why`, in that order, the empty groups left out),
    /// and the latest files of each (up to `PROBLEM_FILES`): (group, path, what was said).
    pub fn problems(&self) -> (Vec<(String, u64)>, Vec<config::status::Problem>) {
        let mut counts: HashMap<&'static str, u64> = HashMap::new();
        let mut files: Vec<config::status::Problem> = Vec::new();
        let mut kept: HashMap<&'static str, usize> = HashMap::new();
        let db = self.db();
        let mut keep = |why: &'static str, path: String, said: String| {
            let kept = kept.entry(why).or_default();
            if *kept < PROBLEM_FILES {
                *kept += 1;
                files.push(config::status::Problem { why: why.into(), path, said });
            }
        };
        // (By the error's own words: password-protected, a reader that stopped on it, or damaged.)
        if let Ok(mut statement) = db.prepare("SELECT error, count(*) FROM files WHERE state = 2 AND error != ?1 GROUP BY error")
            && let Ok(rows) = statement.query_map([SKIPPED_ERROR], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))) {
                for (error, count) in rows.flatten() {
                    *counts.entry(config::why::of_error(&error)).or_default() += count;
                }
            }
        if let Ok(mut statement) = db.prepare("SELECT kind, count(*) FROM skipped GROUP BY kind")
            && let Ok(rows) = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))) {
                for (kind, count) in rows.flatten() {
                    *counts.entry(config::why::of_skip(&kind)).or_default() += count;
                }
            }
        // The latest of each: a few hundred rows at most, through the index of failures.
        if let Ok(mut statement) = db.prepare("SELECT path, error FROM files WHERE state = 2 AND error != ?1 ORDER BY read_at DESC LIMIT ?2")
            && let Ok(rows) = statement.query_map(params![SKIPPED_ERROR, (PROBLEM_FILES * 4) as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                for (path, error) in rows.flatten() {
                    keep(config::why::of_error(&error), path, error);
                }
            }
        if let Ok(mut statement) = db.prepare("SELECT path, kind FROM skipped ORDER BY at DESC LIMIT ?1")
            && let Ok(rows) = statement.query_map([(PROBLEM_FILES * 6) as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
                for (path, kind) in rows.flatten() {
                    keep(config::why::of_skip(&kind), path, kind);
                }
            }
        let counts = config::why::ALL.iter().filter_map(|why| counts.get(why).map(|count| (why.to_string(), *count))).filter(|(_, count)| *count > 0).collect();
        (counts, files)
    }

    /// Clears what keeps files from being read again: a group of what couldn't be read (`config::why`), all of it or
    /// one file. The lanes that ask the catalog what's next (what images show, what's said, Thorough's) find them at
    /// once; what's returned has to be listed again (its record is gone, so it's read as new). Files kept online only
    /// are returned without anything cleared: they're brought down first (`run`), and their row goes when they're read.
    pub fn retry(&self, why: &str, path: Option<&str>) -> Vec<String> {
        let one = path.map(id);
        let mut again = Vec::new();
        let db = self.db();
        let matches = |row_id: &str| one.as_deref().is_none_or(|one| one == row_id);
        // Files that failed, by their error (a document's reader that stopped on one is among "stopped" too).
        if matches!(why, config::why::DAMAGED | config::why::PASSWORD | config::why::STOPPED) {
            {
                let Ok(mut statement) = db.prepare("SELECT id, path, error FROM files WHERE state = 2 AND error != ?1") else { return again };
                let rows: Vec<(String, String, String)> = statement
                    .query_map([SKIPPED_ERROR], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .map(|rows| rows.flatten().collect()).unwrap_or_default();
                for (row_id, path, error) in rows {
                    if matches(&row_id) && config::why::of_error(&error) == why {
                        let _ = db.execute("DELETE FROM files WHERE id = ?1", [&row_id]);
                        again.push(path);
                    }
                }
            }
        }
        // Files let go: skipped, kept online only, a model's reader that stopped on them twice.
        if !matches!(why, config::why::DAMAGED | config::why::PASSWORD) {
            {
                let Ok(mut statement) = db.prepare("SELECT id, path, kind FROM skipped") else { return again };
                let rows: Vec<(String, String, String)> = statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .map(|rows| rows.flatten().collect()).unwrap_or_default();
                for (row_id, path, kind) in rows {
                    if !matches(&row_id) || config::why::of_skip(&kind) != why {
                        continue;
                    }
                    if why == config::why::ONLINE {
                        again.push(path);
                        continue;
                    }
                    let _ = db.execute("DELETE FROM skipped WHERE id = ?1 AND kind = ?2", params![row_id, kind]);
                    // What a model couldn't do, done again; Thorough's read wanted again.
                    let _ = db.execute("DELETE FROM seen WHERE id = ?1 AND ok = 0", [&row_id]);
                    let _ = db.execute("UPDATE files SET deep = ?2 WHERE id = ?1 AND deep = ?3", params![row_id, DEEP_WANTED, DEEP_FAILED]);
                    // Its text let go while it was read: read again as new.
                    if db.execute("DELETE FROM files WHERE id = ?1 AND state = 2 AND error = ?2", params![row_id, SKIPPED_ERROR]).unwrap_or(0) > 0 {
                        again.push(path);
                    }
                }
            }
        }
        again
    }

    /// A file kept online only, brought down to be read: it's no longer listed as online only.
    pub fn online_done(&self, path: &str) {
        let _ = self.db().execute("DELETE FROM skipped WHERE id = ?1 AND kind = ?2", params![id(path), config::SKIP_ONLINE]);
    }
}

/// The ids of everything under a folder: from "folder\" up to (not including) "folder]" (']' follows '\').
fn range(folder: &str) -> (String, String) {
    let mut low = id(folder);
    if !low.ends_with('\\') {
        low.push('\\');
    }
    let mut high = low.clone();
    high.pop();
    high.push(']');
    (low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_counts() {
        let path = std::env::temp_dir().join(format!("excavate-catalog-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let catalog = Catalog::open(&path).unwrap();
        let update = |path: &str, state: State, kind: &'static str| Update {
            path: path.into(), size: 1, modified: 2, kind, state, engine: String::new(), read_at: 3, error: String::new(), characters: 0, deep: DEEP_NO, group: 0, later: false, hash: 0,
        };
        catalog.apply(&[
            update(r"C:\Docs\a.txt", State::Read, "text"),
            update(r"C:\Docs\sub\b.png", State::Empty, "picture"),
            update(r"C:\Docs\c.pdf", State::Failed, "pdf"),
            update(r"C:\Docsother\d.txt", State::Read, "text"),
        ], &[]);
        let counts = catalog.counts(r"C:\Docs");
        assert_eq!((counts.files, counts.read, counts.failed, counts.pictures), (3, 2, 1, 1));
        assert_eq!(catalog.under(r"C:\docs\SUB").len(), 1);
        catalog.apply(&[], &[r"C:\DOCS\A.TXT".into()]);
        assert!(catalog.get(r"C:\Docs\a.txt").is_none());
        assert_eq!(catalog.all_hashed().len(), 3);
        // Deep: what was read before it's chosen is wanted, the doubtful first, and it's done with what it read.
        catalog.apply(&[Update { deep: DEEP_FIRST, modified: 1, ..update(r"C:\Docs\e.png", State::Read, "picture") },
                        update(r"C:\Docs\f.png", State::Read, "picture"),
                        Update { engine: "deep".into(), ..update(r"C:\Docs\g.png", State::Read, "picture") }], &[]);
        catalog.deep_wants_all("deep");
        assert_eq!(catalog.deep_count(), 2);
        assert_eq!(catalog.deep_next(1)[0].0, r"C:\Docs\e.png");
        catalog.deep_set(r"C:\Docs\e.png", DEEP_NO);
        assert_eq!(catalog.deep_next(5).len(), 1);
        // Counted from the catalog, so a new session goes on from there.
        assert_eq!(catalog.deep_done("deep"), 1);
        catalog.deep_set(r"C:\Docs\e.png", DEEP_FAILED);
        assert_eq!(catalog.deep_done("deep"), 2);
        // Copies: the first of each hash.
        catalog.apply(&[Update { hash: 9, ..update(r"C:\Docs\h.png", State::Read, "picture") }, update(r"C:\Docs\i.png", State::Read, "picture")], &[]);
        assert_eq!(catalog.hashes().get(&9).map(String::as_str), Some(r"C:\Docs\h.png"));
        assert_eq!(catalog.hashes().len(), 1);
        // Looking at images: what the first pass read, newest first, until looked at as it is now; again after "Look again".
        assert!(catalog.seen_next("eg2", 0, 50).iter().any(|(path, ..)| path == r"C:\Docs\sub\b.png"));
        let waiting = catalog.seen_counts("eg2", 0).1;
        catalog.seen_set(r"C:\Docs\sub\b.png", "eg2", 2, 100, true);
        assert_eq!(catalog.seen_counts("eg2", 0), (1, waiting - 1));
        assert!(catalog.seen_next("eg2", 0, 50).iter().all(|(path, ..)| path != r"C:\Docs\sub\b.png"));
        assert!(catalog.seen_next("eg2", 101, 50).iter().any(|(path, ..)| path == r"C:\Docs\sub\b.png"));
        assert_eq!(catalog.seen_counts("siglip", 0).0, 0);
        // A folder that leaves out what its images show: none of them are looked at (a "_" in its name is no wildcard).
        catalog.apply(&[update(r"C:\Docs\a_b\x.png", State::Read, "picture"), update(r"C:\Docs\axb\y.png", State::Read, "picture")], &[]);
        catalog.set_unshown(vec![r"c:\docs\a_b".into()]);
        let next = catalog.seen_next("eg2", 0, 50);
        assert!(next.iter().all(|(path, ..)| path != r"C:\Docs\a_b\x.png") && next.iter().any(|(path, ..)| path == r"C:\Docs\axb\y.png"));
        catalog.set_unshown(Vec::new());
        catalog.apply(&[], &[r"C:\Docs\a_b\x.png".into(), r"C:\Docs\axb\y.png".into()]);
        // An image whose text its folder left out says so, to be read when that's back on.
        catalog.apply(&[Update { engine: SKIPPED.into(), ..update(r"C:\Docs\z.png", State::Empty, "picture") }], &[]);
        assert!(catalog.get(r"C:\Docs\z.png").is_some_and(|entry| entry.skipped));
        catalog.apply(&[], &[r"C:\Docs\z.png".into()]);
        assert!(catalog.picture_ids().contains(r"c:\docs\sub\b.png"));
        catalog.apply(&[], &[r"C:\Docs\sub\b.png".into()]);
        assert_eq!(catalog.seen_counts("eg2", 0).0, 0);
        // Videos: their own list, looked at as images are.
        catalog.videos_replace(&[(r"C:\Docs\clip.mp4".into(), 9, 4, 7), (r"C:\Docs\old.mov".into(), 9, 3, 7)]);
        assert_eq!(catalog.video_next("eg2", 0, 5, 2)[0].0, r"C:\Docs\clip.mp4");
        catalog.seen_set(r"C:\Docs\clip.mp4", "eg2", 4, 100, true);
        assert_eq!(catalog.video_counts("eg2", 0, 2), (1, 1));
        assert!(catalog.picture_ids().contains(r"c:\docs\old.mov"));
        // What was said in a video, or the text it showed: gone with it.
        catalog.seen_set(r"C:\Docs\old.mov", "said-parakeet", 3, 100, true);
        assert!(catalog.videos_gone(&["video-text", "said-parakeet"]).is_empty());
        // Its folder no longer reads what's said in it: what was heard goes; the rest stays.
        catalog.videos_replace(&[(r"C:\Docs\clip.mp4".into(), 9, 4, 7), (r"C:\Docs\old.mov".into(), 9, 3, 3)]);
        assert_eq!(catalog.videos_left_out(&["said-parakeet"], 4), [r"c:\docs\old.mov"]);
        assert!(catalog.videos_left_out(&["eg2"], 2).is_empty());
        assert!(catalog.video_next("said-parakeet", 0, 5, 4).iter().all(|(path, ..)| path != r"C:\Docs\old.mov"));
        assert_eq!(catalog.video_counts("eg2", 0, 2), (1, 1));
        catalog.videos_replace(&[]);
        assert_eq!(catalog.video_counts("eg2", 0, 2), (0, 0));
        assert_eq!(catalog.videos_gone(&["video-text", "said-parakeet"]), [r"c:\docs\old.mov"]);
        // Recordings: their own list, heard as images are looked at; what's gone is found to take out of the index.
        catalog.recordings_replace(&[(r"C:\Docs\call.m4a".into(), 9, 7), (r"C:\Docs\memo.mp3".into(), 9, 3)]);
        assert_eq!(catalog.recording_next("whisper-turbo", 0, 5)[0].0, r"C:\Docs\call.m4a");
        catalog.seen_set(r"C:\Docs\call.m4a", "whisper-turbo", 7, 100, true);
        assert_eq!(catalog.recording_counts("whisper-turbo", 0), (1, 1));
        assert_eq!(catalog.recording_counts("whisper-turbo", 101), (0, 2));
        assert_eq!(catalog.recording_counts("parakeet", 0), (0, 2));
        catalog.recordings_replace(&[(r"C:\Docs\memo.mp3".into(), 9, 3)]);
        assert_eq!(catalog.heard_gone(&["parakeet", "whisper-turbo"]), [r"c:\docs\call.m4a"]);
        catalog.heard_forget(r"c:\docs\call.m4a", &["parakeet", "whisper-turbo"]);
        assert!(catalog.heard_gone(&["parakeet", "whisper-turbo"]).is_empty());
        // A long recording waits behind newer short ones (`work::size_wait`): a 100 MB one from today after a 1 MB
        // one from three weeks ago, before one from six weeks ago.
        let (today, day) = (1_800_000_000i64, 86_400i64);
        catalog.recordings_replace(&[(r"C:\Docs\long.wav".into(), 100 << 20, today), (r"C:\Docs\memo.mp3".into(), 1 << 20, today - 21 * day),
                                     (r"C:\Docs\old.mp3".into(), 1 << 20, today - 42 * day)]);
        let order: Vec<String> = catalog.recording_next("parakeet", 0, 5).into_iter().map(|(path, ..)| path).collect();
        assert_eq!(order, [r"C:\Docs\memo.mp3", r"C:\Docs\long.wav", r"C:\Docs\old.mp3"]);
        for size in [0u64, 8 << 20, (8 << 20) + 1, 100 << 20, 1 << 30, 1 << 50] {
            let sql: i64 = catalog.db().query_row(&format!("SELECT {}", crate::work::size_wait_sql(&size.to_string())), [], |row| row.get(0)).unwrap();
            assert_eq!(sql, crate::work::size_wait(size), "{size}");
        }
        catalog.recordings_replace(&[]);
        assert_eq!(catalog.meta("x"), 0);
        catalog.set_meta("x", 5);
        assert_eq!(catalog.meta("x"), 5);
        drop(catalog);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn what_couldnt_be_read_and_reading_it_again() {
        let path = std::env::temp_dir().join(format!("excavate-catalog-problems-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let catalog = Catalog::open(&path).unwrap();
        let failed = |path: &str, error: &str| Update {
            path: path.into(), size: 1, modified: 2, kind: "pdf", state: State::Failed, engine: String::new(), read_at: 3, error: error.into(),
            characters: 0, deep: DEEP_NO, group: 0, later: false, hash: 0,
        };
        catalog.apply(&[
            failed(r"C:\Docs\locked.pdf", "PdfiumLibraryInternalError(PasswordError)"),
            failed(r"C:\Docs\broken.pdf", "PdfiumLibraryInternalError(FormatError)"),
            failed(r"C:\Docs\odd.png", "The component cannot be found."),
            failed(r"C:\Docs\scan.pdf", SKIPPED_ERROR),
            failed(r"C:\Docs\deck.pptx", config::why::READER_STOPPED),
            Update { state: State::Read, deep: DEEP_FAILED, kind: "picture", ..failed(r"C:\Docs\photo.png", "") },
        ], &[]);
        catalog.skipped_add(r"C:\Docs\scan.pdf", "text in images");
        catalog.skipped_add(r"C:\Docs\talk.mp4", "what's said");
        catalog.seen_set(r"C:\Docs\talk.mp4", "said-parakeet", 4, 100, false);
        catalog.skipped_add(r"C:\Cloud\a.docx", config::SKIP_ONLINE);
        catalog.skipped_add(r"C:\Docs\photo.png", config::SKIP_STOPPED);
        catalog.seen_set(r"C:\Docs\photo.png", "eg2", 2, 100, false);
        let (counts, files) = catalog.problems();
        let counts: Vec<(&str, u64)> = counts.iter().map(|(why, count)| (why.as_str(), *count)).collect();
        assert_eq!(counts, [("password", 1), ("online", 1), ("damaged", 2), ("skipped", 2), ("stopped", 2)]);
        assert!(files.iter().any(|file| file.why == "stopped" && file.path == r"C:\Docs\deck.pptx"), "a document its reader stopped on");
        assert!(files.iter().any(|file| file.why == "online" && file.path == r"C:\Cloud\a.docx"));
        assert!(files.iter().all(|file| file.path != r"C:\Docs\scan.pdf" || file.why == "skipped"), "a skipped scan isn't damaged");
        // "Try again" for the damaged: their records go, to be listed again; the password-protected stay.
        let mut again = catalog.retry(config::why::DAMAGED, None);
        again.sort();
        assert_eq!(again, [r"C:\Docs\broken.pdf", r"C:\Docs\odd.png"]);
        assert!(catalog.get(r"C:\Docs\broken.pdf").is_none() && catalog.get(r"C:\Docs\locked.pdf").is_some());
        // "Read again" for one skipped file: what was said in the video is wanted again, at once.
        assert!(catalog.retry(config::why::SKIPPED, Some(r"c:\docs\TALK.mp4")).is_empty());
        catalog.videos_replace(&[(r"C:\Docs\talk.mp4".into(), 9, 4, 7)]);
        assert_eq!(catalog.video_next("said-parakeet", 0, 5, 4).len(), 1);
        // The skipped scan: listed again.
        assert_eq!(catalog.retry(config::why::SKIPPED, None), [r"C:\Docs\scan.pdf"]);
        // The reader stopped on it: the document listed again; the model's twice looked at again, and Thorough's read
        // wanted again.
        assert_eq!(catalog.retry(config::why::STOPPED, None), [r"C:\Docs\deck.pptx"]);
        assert!(catalog.get(r"C:\Docs\deck.pptx").is_none());
        assert!(catalog.seen_next("eg2", 0, 5).iter().any(|(path, ..)| path == r"C:\Docs\photo.png"));
        assert_eq!(catalog.deep_count(), 1);
        // Online only: handed back to be brought down; listed until it's read.
        assert_eq!(catalog.retry(config::why::ONLINE, None), [r"C:\Cloud\a.docx"]);
        let (counts, _) = catalog.problems();
        assert_eq!(counts, [("password".to_string(), 1), ("online".to_string(), 1)]);
        catalog.apply(&[Update { state: State::Read, ..failed(r"C:\Cloud\a.docx", "") }], &[]);
        assert_eq!(catalog.problems().0, [("password".to_string(), 1)]);
        drop(catalog);
        let _ = std::fs::remove_file(&path);
    }
}
