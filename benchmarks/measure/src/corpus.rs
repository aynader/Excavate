//! Where the corpus is: one folder as `make_corpus.py` makes it (documents, images, audio, videos, truth), or its parts
//! wherever they were put (on the test machine: Documents, Pictures, Music, Videos).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

static PARTS: RwLock<BTreeMap<String, PathBuf>> = RwLock::new(BTreeMap::new());

pub const NAMES: [&str; 5] = ["documents", "images", "audio", "videos", "truth"];

/// The parts given (on the command line, or by a kept home's sample.json), over any set before.
pub fn set(parts: BTreeMap<String, PathBuf>) {
    PARTS.write().unwrap().extend(parts);
}

pub fn parts() -> BTreeMap<String, PathBuf> {
    PARTS.read().unwrap().clone()
}

/// A path under the corpus as the truth files name it ("images/coco-val2017/1.jpg"), on this machine.
pub fn path(relative: &str) -> PathBuf {
    let (top, rest) = relative.split_once('/').unwrap_or((relative, ""));
    let base = PARTS.read().unwrap().get(top).cloned().unwrap_or_else(|| PathBuf::from(top));
    if rest.is_empty() { base } else { base.join(rest.replace('/', "\\")) }
}

pub fn truth() -> PathBuf {
    path("truth")
}
