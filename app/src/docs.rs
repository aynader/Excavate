//! "Learn more": the pages open on GitHub, at the release this copy is (so they match what it does).

use crate::app::App;

const REPOSITORY: &str = "https://github.com/aynader/Excavate";

const PAGES: [(&str, &str); 4] = [
    ("everything", "docs/EVERYTHING.md"),
    ("privacy", "docs/PRIVACY.md"),
    ("models", "docs/MODELS.md"),
    // The Indexers page's figures: where and how they were measured.
    ("measured", "benchmarks/README.md"),
];

/// A page's address: the file at this version's release tag.
fn address(page: &str) -> Option<String> {
    let (_, file) = PAGES.iter().find(|(name, _)| *name == page)?;
    Some(format!("{REPOSITORY}/blob/v{}/{file}", env!("CARGO_PKG_VERSION")))
}

/// Opens a page by its short name, or a web address as it is, in the browser.
pub fn open(_app: &App, page: &str) {
    let address = if page.starts_with("https://") { Some(page.to_string()) } else { address(page) };
    if let Some(address) = address {
        let _ = winsys::shell::open(0, &address);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_open_at_this_release() {
        let privacy = super::address("privacy").unwrap();
        assert_eq!(privacy, format!("https://github.com/aynader/Excavate/blob/v{}/docs/PRIVACY.md", env!("CARGO_PKG_VERSION")));
        assert!(super::address("nothing").is_none());
    }
}
