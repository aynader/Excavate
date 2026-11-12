//! "Learn more": the pages travel with the app (they read offline and match this version) and open as Markdown files.

use crate::app::App;
use crate::settings::log;

const PAGES: [(&str, &str, &str); 4] = [
    ("everything", "EVERYTHING.md", include_str!("../../docs/EVERYTHING.md")),
    ("privacy", "PRIVACY.md", include_str!("../../docs/PRIVACY.md")),
    ("models", "MODELS.md", include_str!("../../docs/MODELS.md")),
    // The Indexers page's figures: where and how they were measured.
    ("measured", "BENCHMARKS.md", include_str!("../../benchmarks/README.md")),
];

/// Opens a page by its short name, with whatever opens Markdown here (Notepad when nothing does).
pub fn open(app: &App, page: &str) {
    // A page on the web, as it is.
    if page.starts_with("https://") {
        let _ = winsys::shell::open(0, page);
        return;
    }
    let Some((_, file, text)) = PAGES.iter().find(|(name, ..)| *name == page) else { return };
    let folder = app.paths.local.join("docs");
    let path = folder.join(file);
    if std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&path, text)).is_err() {
        log(&format!("can't write {}", path.display()));
        return;
    }
    let path = path.display().to_string();
    if winsys::shell::open(0, &path).is_err() {
        let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
    }
}
