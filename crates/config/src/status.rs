//! What the indexer is doing, as it writes it to status.json for Excavate's settings window and popup.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Status {
    /// "waiting" (for setup), "listing", "reading", "paused" (with why, in `detail`), "idle", "stopped".
    pub state: String,
    /// Why it's paused ("Paused on battery power", "Waiting for a quiet moment: the graphics card is busy"), or what it's doing.
    pub detail: String,
    /// When this session of reading began (seconds since 1970).
    pub started: i64,
    pub folders: Vec<Folder>,
    /// Files known in the chosen folders that a reader handles.
    pub files: u64,
    pub read: u64,
    pub failed: u64,
    /// Of those files, the images listed, and the images with a record so far (read, empty or failed).
    pub images: u64,
    pub images_read: u64,
    /// Why each kind of reading waits, empty when it doesn't: documents and code, images, Deep.
    pub waits: [String; 3],
    /// Waiting to be read: documents and code, pictures (and scanned pages).
    pub queued_text: u64,
    pub queued_pictures: u64,
    /// Of those, scanned PDFs (their pages to read; found as documents are read, so not among `images`).
    pub queued_scanned: u64,
    /// Of those images, how many wait for the first sweep to be over.
    pub queued_later: u64,
    /// In one sitting, without waiting for a quiet moment ("Read everything now").
    pub reading_all: bool,
    /// New pictures wait for their time (the schedule, or "Read new pictures now"): when, in seconds since 1970, or 0
    /// when only a person's click will start them.
    pub pictures_wait: bool,
    pub pictures_at: i64,
    /// The same for looking at what new images show, and whether everything is being looked at in one sitting.
    pub seeing_wait: bool,
    pub seeing_at: i64,
    pub seeing_all: bool,
    /// And for listening to new recordings.
    pub listen_wait: bool,
    pub listen_at: i64,
    pub listen_all: bool,
    /// Deep's second pass: whether it's chosen, pictures waiting for it, and pictures it has read (from the catalog, so
    /// they count across sessions).
    pub deep: bool,
    pub deep_queued: u64,
    pub deep_done: u64,
    /// How long a picture takes it: the reading alone (median, ms), and everything included (mean of the last 64, ms).
    pub deep_ms: Option<f64>,
    pub deep_pace_ms: Option<f64>,
    /// Its server is up, and the picture it's on.
    pub deep_running: bool,
    pub deep_current: String,
    /// In the content index.
    pub documents: u64,
    pub current: String,
    /// How text in pictures is read now ("PP-OCRv6 small"), and how long a picture takes on this PC (median, ms).
    pub engine: String,
    pub picture_ms: Option<f64>,
    /// Whether it reads them on the graphics card (else the processor): as chosen, or where it was measured fastest.
    pub reading_gpu: bool,
    /// The memory the indexer holds, in megabytes.
    pub memory_mb: u64,
    /// How many of the processor's threads the indexer and its model servers keep busy, lately (their CPU time over
    /// the last few seconds), and how many the processor has.
    pub cpu_threads: u32,
    pub threads: u32,
    pub model: Model,
    /// Images by what they show: one for each model the choice uses, the one that goes first first.
    pub looking: Vec<Looking>,
    /// Recordings by what's said in them, when it's on.
    pub listening: Option<Listening>,
    /// Text in videos, read after the images (none when it's off).
    pub video_text: Option<VideoText>,
    /// The latest files that couldn't be read: (path, why).
    pub errors: Vec<(String, String)>,
    /// Everything that couldn't be read, counted by why (`config::why`), in that order; the groups with none left out.
    pub problems: Vec<(String, u64)>,
    /// The latest files of each of those groups (a few dozen each).
    pub problem_files: Vec<Problem>,
    /// How each lane is going, by its name (`LANES`): for its time left, and to tell slow from stalled.
    pub paces: std::collections::BTreeMap<String, Pace>,
    /// When this was written (seconds since 1970).
    pub updated: i64,
}

/// The lanes whose pace is kept: documents, text in images, Thorough's second read, what images and videos show,
/// documents by what they mean, and what's said in recordings and videos.
pub const LANES: [&str; 6] = ["documents", "images", "deep", "shown", "meaning", "said"];

/// A file that couldn't be read: its group (`config::why`), and what the reader said.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Problem {
    pub why: String,
    pub path: String,
    pub said: String,
}

/// Said instead of why a kind of reading waits when the person paused it on its own (`Settings::paused_kinds`).
pub const KIND_PAUSED: &str = "Paused";

/// A lane's pace.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Pace {
    /// Items a minute, lately (an average weighted to the last few minutes of work; the time it waited, was paused or
    /// had nothing to do isn't counted). 0 until it's known.
    pub per_minute: f64,
    /// The same weighted to the last quarter of an hour or so: with `per_minute`, how steady it is (Activity's time
    /// left is a range between the two).
    pub per_minute_long: f64,
    /// When an item was last done (seconds since 1970; 0: none yet).
    pub last_progress: i64,
    /// When the oldest item it's on now was started (0: none).
    pub current_since: i64,
    /// How many items it has in hand at once now (its workers side by side).
    pub at_once: u32,
    /// The oldest item in hand: its path (empty when the lane doesn't say), and how far into it, where the reader can
    /// tell: `done` of `total` `unit` ("pages", "seconds"; empty: not known), and when that last moved (0: never).
    pub item: String,
    pub item_done: u64,
    pub item_total: u64,
    pub item_unit: String,
    pub item_moved: i64,
}

impl Pace {
    /// How long `left` items take at this pace, in milliseconds; None until the pace is known.
    pub fn time_for(&self, left: u64) -> Option<f64> {
        (self.per_minute > 0.0).then(|| left as f64 / self.per_minute * 60_000.0)
    }

    /// When the item it's on last moved: a piece of it read, or its start (0: none in hand).
    pub fn moved_at(&self) -> i64 {
        if self.current_since == 0 { 0 } else { self.current_since.max(self.item_moved) }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Folder {
    pub path: String,
    pub files: u64,
    pub read: u64,
    pub failed: u64,
    pub pictures: u64,
    /// Its images looked at for what they show (as they are now), and waiting; its recordings heard, and waiting
    /// (0 and 0 when that's off): the folder card says which step it's on.
    pub seen: u64,
    pub to_see: u64,
    pub heard: u64,
    pub to_hear: u64,
    /// The folder isn't there (deleted, moved, or its drive unplugged): its counts are of nothing, not "0 files".
    pub missing: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Looking {
    /// `seeing::Engine::name`.
    pub engine: String,
    /// Images looked at as they are now, and waiting (from the catalog, so they count across sessions).
    pub done: u64,
    pub queued: u64,
    /// The same for videos.
    pub videos_done: u64,
    pub videos_queued: u64,
    /// And documents, by what they mean (EmbeddingGemma 2's lane only).
    pub documents_done: u64,
    pub documents_queued: u64,
    /// How long an image takes it (median, ms).
    pub ms: Option<f64>,
    /// Its model is loaded, and the image it's on.
    pub running: bool,
    /// Its model runs on the graphics card (else the processor).
    pub gpu: bool,
    /// Why it waits, if it does ("Waiting for a quiet moment: the graphics card is busy").
    pub waiting: String,
    pub current: String,
    /// Its download.
    pub model: Model,
}

/// Reading the text videos show: videos read as they are now, and waiting (from the catalog); the one being read.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct VideoText {
    pub done: u64,
    pub queued: u64,
    pub current: String,
}

/// The model listening to recordings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Listening {
    /// "parakeet", "whisper-turbo" or "whisper-large".
    pub engine: String,
    /// Recordings listened to as they are now, and waiting (from the catalog).
    pub done: u64,
    pub queued: u64,
    /// Whether recordings are listened to, and videos (what's said in them, after the recordings); the videos heard
    /// as they are now, and waiting.
    pub recordings: bool,
    pub videos: bool,
    pub videos_done: u64,
    pub videos_queued: u64,
    /// How long a minute of sound takes it (median of the last ones, ms), and where it runs ("Vulkan", "CUDA", "CPU").
    pub ms_a_minute: Option<f64>,
    pub device: String,
    pub running: bool,
    pub waiting: String,
    pub current: String,
    pub model: Model,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Model {
    /// "", "downloading", "ready", "failed".
    pub state: String,
    /// What's being downloaded ("the image reader", "the thorough reader").
    pub name: String,
    pub received: u64,
    pub total: u64,
    pub error: String,
}

impl Status {
    pub fn load(paths: &crate::Paths) -> Option<Status> {
        serde_json::from_slice(&std::fs::read(paths.status()).ok()?).ok()
    }

    pub fn save(&self, paths: &crate::Paths) {
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = crate::write_atomic(&paths.status(), &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_folder_in_the_status() {
        // An indexer from before says nothing of it: there.
        let old: Folder = serde_json::from_str(r#"{ "path": "C:\\Gone", "files": 0 }"#).unwrap();
        assert!(!old.missing);
        let gone = Folder { path: r"C:\Gone".into(), missing: true, ..Folder::default() };
        let text = serde_json::to_string(&gone).unwrap();
        assert!(text.contains(r#""missing":true"#));
        assert_eq!(serde_json::from_str::<Folder>(&text).unwrap(), gone);
    }

    #[test]
    fn a_status_from_before_the_item_in_hand() {
        let old: Status = serde_json::from_str(r#"{ "state": "reading", "paces": { "documents": { "per-minute": 30.0, "last-progress": 5, "current-since": 4 } } }"#).unwrap();
        let pace = &old.paces["documents"];
        assert_eq!((pace.per_minute_long, pace.at_once, pace.item.as_str(), pace.moved_at()), (0.0, 0, "", 4));
        assert!(old.problems.is_empty());
        let moving = Pace { current_since: 100, item_moved: 150, ..Pace::default() };
        assert_eq!(moving.moved_at(), 150);
        assert_eq!(Pace::default().moved_at(), 0);
    }
}
