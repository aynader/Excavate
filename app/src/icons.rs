//! Icons and thumbnails, made off the UI thread by a small pool of COM threads and cached. Newest requests first:
//! what's on screen now matters more than what scrolled past. Generic types share one icon per extension; files with
//! their own icon (programs, shortcuts) and pictures and videos (thumbnails) are cached per file.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use winsys::icons::{Bitmap, Kind};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    pub key: String,
    pub path: String,
    pub size: u32,
    pub kind: KindKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KindKey {
    Icon,
    Preview,
}

struct Queue {
    pending: Mutex<(VecDeque<Request>, HashSet<String>)>,
    ready: Condvar,
}

pub struct Loader {
    queue: Arc<Queue>,
}

/// File types whose icon is each file's own (a program's, a shortcut's target's), not one for the whole type.
fn per_file(extension: &str) -> bool {
    matches!(extension, "exe" | "lnk" | "ico" | "url" | "appref-ms" | "msc" | "cpl" | "scr" | "dll" | "cur" | "ani")
}

/// Images and videos get thumbnails in rows.
pub fn has_thumbnail(name: &str) -> bool {
    let filter = search::Filter::Pictures;
    filter.accepts(name, false) || search::Filter::Video.accepts(name, false)
}

/// Folders with icons of their own (the profile, Desktop, Downloads…), lowercased.
fn special_folders() -> &'static HashSet<String> {
    static SPECIAL: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    SPECIAL.get_or_init(|| {
        use winsys::files::Known;
        let mut folders: HashSet<String> = [Known::Profile, Known::Desktop, Known::Documents, Known::Downloads,
                                            Known::Pictures, Known::Music, Known::Videos]
            .into_iter()
            .filter_map(winsys::files::known)
            .map(|path| path.display().to_string().to_lowercase())
            .collect();
        if let Some(onedrive) = std::env::var_os("OneDrive") {
            folders.insert(std::path::PathBuf::from(onedrive).display().to_string().to_lowercase());
        }
        folders
    })
}

/// A folder certain to have the plain folder icon, to draw that icon from.
pub fn plain_folder() -> String {
    let windows = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{windows}\Web")
}

/// The cache key of a row's icon. Plain folders share one key, drawn from `plain_folder()`.
pub fn key(path: &str, name: &str, is_folder: bool, size: u32) -> (String, KindKey) {
    if is_folder {
        // Drives and the special folders have their own icons; plain folders share one.
        let lower = path.trim_end_matches('\\').to_lowercase();
        if path.len() <= 3 || special_folders().contains(&lower) {
            return (format!("{size}|{lower}"), KindKey::Icon);
        }
        return (format!("{size}|<folder>"), KindKey::Icon);
    }
    let extension = search::filter::extension(name).unwrap_or("").to_ascii_lowercase();
    if has_thumbnail(name) {
        return (format!("{size}|thumb|{}", path.to_lowercase()), KindKey::Preview);
    }
    if per_file(&extension) {
        return (format!("{size}|{}", path.to_lowercase()), KindKey::Icon);
    }
    (format!("{size}|.{extension}"), KindKey::Icon)
}

impl Loader {
    /// `threads` COM threads; `done` runs on them with each finished image (None when there's none to show).
    pub fn start(threads: usize, done: impl Fn(Request, Option<Bitmap>) + Send + Sync + 'static) -> Loader {
        let queue = Arc::new(Queue { pending: Mutex::new((VecDeque::new(), HashSet::new())), ready: Condvar::new() });
        let done = Arc::new(done);
        for index in 0..threads {
            let queue = queue.clone();
            let done = done.clone();
            std::thread::Builder::new()
                .name(format!("icons-{index}"))
                .spawn(move || {
                    winsys::com_sta();
                    loop {
                        let request = {
                            let mut pending = queue.pending.lock().unwrap();
                            loop {
                                if let Some(request) = pending.0.pop_front() {
                                    pending.1.remove(&request.key);
                                    break request;
                                }
                                pending = queue.ready.wait(pending).unwrap();
                            }
                        };
                        let kind = match request.kind {
                            KindKey::Icon => Kind::Icon,
                            KindKey::Preview => Kind::Preview,
                        };
                        let path = if request.key.ends_with("|<folder>") { plain_folder() } else { request.path.clone() };
                        let image = winsys::icons::image(&path, request.size, kind);
                        done(request, image);
                    }
                })
                .expect("can't start an icon thread");
        }
        Loader { queue }
    }

    /// Asks for an image; asking again for one already waiting moves it to the front.
    pub fn request(&self, request: Request) {
        let mut pending = self.queue.pending.lock().unwrap();
        if !pending.1.insert(request.key.clone()) {
            if let Some(index) = pending.0.iter().position(|waiting| waiting.key == request.key) {
                let waiting = pending.0.remove(index).unwrap();
                pending.0.push_front(waiting);
            }
            return;
        }
        pending.0.push_front(request);
        // A long scroll shouldn't leave hundreds of stale requests: the oldest go.
        while pending.0.len() > 400 {
            if let Some(dropped) = pending.0.pop_back() {
                pending.1.remove(&dropped.key);
            }
        }
        self.queue.ready.notify_one();
    }

    /// Drops everything still waiting (a new search makes it stale).
    pub fn clear(&self) {
        let mut pending = self.queue.pending.lock().unwrap();
        pending.0.clear();
        pending.1.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(key(r"C:\a\Report.PDF", "Report.PDF", false, 32), ("32|.pdf".into(), KindKey::Icon));
        assert_eq!(key(r"C:\a\app.exe", "app.exe", false, 32).0, r"32|c:\a\app.exe");
        assert_eq!(key(r"C:\a\photo.jpg", "photo.jpg", false, 48), (r"48|thumb|c:\a\photo.jpg".into(), KindKey::Preview));
        assert_eq!(key(r"C:\folder", "folder", true, 32).0, "32|<folder>");
        assert_eq!(key(r"D:\", "D:", true, 32).0, r"32|d:");
    }
}
