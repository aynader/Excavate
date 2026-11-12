//! Where each model runs fastest on this PC, as measured the first time it ran. Kept in the local folder, not
//! the settings: a measurement belongs to this PC, and the settings roam with the profile.
//!
//! A choice made on a card always wins (`Settings::reading_devices` and the others); without one, a measurement made
//! with the graphics card and driver there now does; without one, Excavate's own defaults (from the benchmarks).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The measurements, with the graphics card (and its driver) they were made with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Measured {
    /// "NVIDIA GeForce RTX 4080 Laptop GPU, driver 32.0.15.6094": a different one, and they're made again.
    pub adapter: String,
    /// By what was measured: "reading:balanced" (text in images, per image: no longer measured, nor followed), "seeing:siglip" or "seeing:gemma" (what
    /// images show, per image), "listening:balanced" (per minute of sound).
    pub runs: BTreeMap<String, Run>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Run {
    /// The faster: "cpu", "gpu", "vulkan" or "cuda".
    pub fastest: String,
    /// Each device's time per item (ms). One given up as clearly slower has its time so far.
    pub ms: BTreeMap<String, f64>,
    /// When (seconds since 1970).
    pub at: i64,
}

impl Measured {
    pub fn load(paths: &crate::Paths) -> Measured {
        std::fs::read(paths.devices()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self, paths: &crate::Paths) {
        let path = paths.devices();
        if let (Some(folder), Ok(json)) = (path.parent(), serde_json::to_vec_pretty(self)) {
            let _ = std::fs::create_dir_all(folder);
            let temporary = path.with_extension("json.new");
            if std::fs::write(&temporary, json).is_ok() {
                let _ = std::fs::rename(&temporary, &path);
            }
        }
    }

    /// The fastest device measured for `key`, if it was.
    pub fn fastest(&self, key: &str) -> Option<&str> {
        self.runs.get(key).map(|run| run.fastest.as_str()).filter(|fastest| !fastest.is_empty())
    }

    /// Forgets what was measured with another graphics card or driver; true when it did (to save).
    pub fn follow_adapter(&mut self, adapter: &str) -> bool {
        if self.adapter == adapter {
            return false;
        }
        let had = !self.runs.is_empty() || !self.adapter.is_empty();
        self.adapter = adapter.to_string();
        self.runs.clear();
        had
    }

    /// Records a measurement: each device's time per item. The fastest is the one with the least.
    pub fn record(&mut self, key: &str, ms: BTreeMap<String, f64>, at: i64) -> Option<String> {
        let fastest = ms.iter().filter(|(_, ms)| ms.is_finite() && **ms > 0.0).min_by(|a, b| a.1.total_cmp(b.1)).map(|(device, _)| device.clone())?;
        self.runs.insert(key.to_string(), Run { fastest: fastest.clone(), ms, at });
        Some(fastest)
    }

}

/// What a choice for images by what they show is measured as: the model it looks with first.
pub fn seeing_key(choice: &str) -> &'static str {
    if choice == "quick" { "seeing:siglip" } else { "seeing:gemma" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_fastest_for_this_card() {
        let mut measured = Measured::default();
        assert!(!measured.follow_adapter(""));
        assert!(!measured.follow_adapter("Card A, driver 1"));
        let ms = BTreeMap::from([("cpu".to_string(), 120.0), ("gpu".to_string(), 980.0)]);
        assert_eq!(measured.record("reading:balanced", ms, 5).as_deref(), Some("cpu"));
        assert_eq!(measured.fastest("reading:balanced"), Some("cpu"));
        assert_eq!(measured.fastest("reading:quick"), None);
        // A new driver: measured again.
        assert!(measured.follow_adapter("Card A, driver 2"));
        assert_eq!(measured.fastest("reading:balanced"), None);
    }

}
