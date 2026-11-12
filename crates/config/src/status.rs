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
    pub model: Model,
    /// Images by what they show: one for each model the choice uses, the one that goes first first.
    pub looking: Vec<Looking>,
    /// Recordings by what's said in them, when it's on.
    pub listening: Option<Listening>,
    /// Text in videos, read after the images (none when it's off).
    pub video_text: Option<VideoText>,
    /// The latest files that couldn't be read: (path, why).
    pub errors: Vec<(String, String)>,
    /// When this was written (seconds since 1970).
    pub updated: i64,
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
