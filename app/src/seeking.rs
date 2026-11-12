//! Images by what they show, for the popup. The popup never loads a model: the search helper (`excavate-helper --seek`, "Excavate Helper")
//! holds them, started here when someone pauses typing and ending itself a few minutes after the last search. It's in
//! Excavate's job object, so it also ends with Excavate.

use crate::settings::log;
use config::Paths;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// One image it found.
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    /// How far above the rest it stands, in deviations (`seeing::combine`).
    pub score: f32,
    /// For a video: the moment that fits best, in seconds.
    pub at: Option<f64>,
    pub size: Option<u64>,
    /// Seconds since 1970.
    pub modified: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub hits: Vec<Hit>,
    /// A model's text half is still loading: asking again soon adds its view.
    pub waiting: bool,
    /// How many images it scored.
    pub images: usize,
    /// A time or a place in the description narrowed them down: they all fit that.
    pub filtered: bool,
    /// Documents by what they mean, each by its best piece, and how many pieces there are.
    pub documents: Vec<Hit>,
    pub pieces: usize,
    /// The kind the description asks for by the helper's classifier ("image", "video", "audio", "document", "none"),
    /// and how sure it is (0 to 1), when it had EmbeddingGemma 2's vector of it.
    pub kind: Option<(String, f32)>,
    pub ms: f64,
}

impl Answer {
    /// How far above the rest an image has to stand to join names in the ranking (the toggle shows them all). Measured
    /// on 752 images: file names' best images stood 2.1 to 3.5 deviations above the rest, descriptions'
    /// 3.4 to 9.0 (median 5.8), and every right image found first 4.3 or more. In a larger library more images stand out
    /// by chance (about √(2 ln n) for the luckiest of n), so the line rises with it: 4.0 here, 5.2 for 121,000.
    pub fn line(&self) -> f32 {
        // "Prague", "last winter": every one of them is taken there, then.
        if self.filtered {
            return f32::MIN;
        }
        line_for(self.images)
    }

    /// The same for documents, by their pieces.
    pub fn documents_line(&self) -> f32 {
        line_for(self.pieces)
    }
}

fn line_for(count: usize) -> f32 {
    let count = count.max(2) as f32;
    ((2.0 * count.ln()).sqrt() + 0.4).max(4.0)
}

pub struct Seeker {
    job: Option<winsys::process::Job>,
    child: RefCell<Option<std::process::Child>>,
}

impl Seeker {
    pub fn new() -> Seeker {
        Seeker { job: winsys::process::Job::new(), child: RefCell::new(None) }
    }

    /// Starts it if this Excavate hasn't, or it ended. (One already running for these settings keeps going: a second
    /// one ends at once.)
    fn ensure(&self) {
        let mut child = self.child.borrow_mut();
        if matches!(child.as_mut().map(|child| child.try_wait()), Some(Ok(None))) {
            return;
        }
        // Excavate Helper (`excavate-helper.exe`, Task Manager's name for it); the indexer's own program where it's missing.
        let beside = |name: &str| std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join(name))).filter(|exe| exe.exists());
        let Some(exe) = beside("excavate-helper.exe").or_else(|| beside("excavate-indexer.exe")) else {
            return;
        };
        match std::process::Command::new(exe).arg("--seek").spawn() {
            Ok(started) => {
                if let Some(job) = &self.job {
                    job.add(started.id());
                }
                *child = Some(started);
            }
            Err(error) => log(&format!("can't start the search helper: {error}")),
        }
    }

    /// Ends it: the choice of models changed, and the next search starts one with the new choice.
    pub fn stop(&self) {
        if let Some(mut child) = self.child.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Started ahead of the first search (the popup opened): loading its models takes 1 to 3 s, which used to come after
    /// the typing paused, with nothing to show meanwhile for a search only it could answer.
    pub fn warm(&self) {
        self.ensure();
    }

    /// Asks for images that fit `text`, on a thread of its own; `done` runs there.
    pub fn ask(&self, paths: &Paths, text: String, limit: usize, wanted: Wanted, done: impl FnOnce(Result<Answer, String>) + Send + 'static) {
        self.ensure();
        let file = paths.seeker();
        std::thread::spawn(move || done(ask(&file, &text, limit, wanted)));
    }
}

/// VLC, when it's installed: it can open a video at a moment ("--start-time"), which Windows' own players can't be
/// asked to.
pub fn vlc() -> Option<String> {
    let path = winsys::registry::string(winsys::registry::Root::LocalMachine, r"SOFTWARE\VideoLAN\VLC", "")?;
    std::path::Path::new(&path).is_file().then_some(path)
}

/// What a search wants from the helper.
#[derive(Clone, Debug)]
pub struct Wanted {
    pub images: bool,
    pub documents: bool,
    /// Only files with these extensions (none: any), picked before the best are cut off.
    pub extensions: Vec<&'static str>,
}

fn ask(file: &std::path::Path, text: &str, limit: usize, wanted: Wanted) -> Result<Answer, String> {
    let body = serde_json::to_vec(&serde_json::json!({ "text": text, "limit": limit, "images": wanted.images, "documents": wanted.documents,
                                                       "extensions": wanted.extensions }))
        .map_err(|error| error.to_string())?;
    // A helper just started writes where it listens within a moment; one that ended left a stale port behind.
    let started = Instant::now();
    loop {
        let port = std::fs::read(file).ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|said| said["port"].as_u64());
        if let Some(port) = port {
            if let Ok((200, reply)) = winsys::http::local(&format!("http://127.0.0.1:{port}/search"), &body, Duration::from_secs(20)) {
                let reply: serde_json::Value = serde_json::from_slice(&reply).map_err(|error| error.to_string())?;
                // What's gone since it was looked at is left out (the reader forgets it within minutes).
                let found = |key: &str| reply[key].as_array().map(|hits| hits.iter().filter_map(|hit| {
                    let path = hit["path"].as_str()?.to_string();
                    let metadata = std::fs::metadata(&path).ok().filter(std::fs::Metadata::is_file)?;
                    let modified = metadata.modified().ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|since| since.as_secs() as i64);
                    Some(Hit { path, score: hit["score"].as_f64()? as f32, at: hit["at"].as_f64(), size: Some(metadata.len()), modified })
                }).collect::<Vec<Hit>>()).unwrap_or_default();
                let (hits, documents) = (found("hits"), found("documents"));
                let pieces = reply["pieces"].as_u64().unwrap_or(0) as usize;
                let waiting = reply["waiting"].as_array().is_some_and(|waiting| !waiting.is_empty());
                let images = reply["images"].as_u64().unwrap_or(0) as usize;
                let filtered = reply["filtered"].as_bool().unwrap_or(false);
                let kind = reply["kind"]["kind"].as_str().zip(reply["kind"]["p"].as_f64()).map(|(kind, sure)| (kind.to_string(), sure as f32));
                return Ok(Answer { hits, waiting, images, filtered, documents, pieces, kind, ms: reply["ms"].as_f64().unwrap_or(0.0) });
            }
        }
        if started.elapsed() > Duration::from_secs(8) {
            return Err("the search helper didn't answer".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_rises_with_the_library() {
        let line = |images| Answer { images, ..Answer::default() }.line();
        assert!((line(752) - 4.04).abs() < 0.01, "{}", line(752));
        assert_eq!(line(50), 4.0);
        assert!((line(121_000) - 5.24).abs() < 0.05, "{}", line(121_000));
        assert!(line(10_000) > 4.0 && line(10_000) < line(121_000));
    }
}
