//! What was opened through Excavate, and when: the strongest signal of what someone will look for again. Kept small
//! (the 2,000 most recent) in a JSON file beside the settings.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Opened {
    pub path: String,
    pub count: u32,
    /// Seconds since 1970.
    pub last: i64,
    /// Queries it was opened from, most recent first (a few), so a query can learn what it means.
    #[serde(default)]
    pub queries: Vec<String>,
}

#[derive(Debug, Default)]
pub struct History {
    file: Option<PathBuf>,
    items: HashMap<String, Opened>,
}

const KEEP: usize = 2_000;

impl History {
    pub fn load(file: &Path) -> History {
        let items: Vec<Opened> = std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        History {
            file: Some(file.to_path_buf()),
            items: items.into_iter().map(|item| (item.path.to_lowercase(), item)).collect(),
        }
    }

    pub fn record(&mut self, path: &str, query: &str, now: i64) {
        let entry = self.items.entry(path.to_lowercase()).or_insert_with(|| Opened {
            path: path.to_string(),
            count: 0,
            last: now,
            queries: Vec::new(),
        });
        entry.count += 1;
        entry.last = now;
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            entry.queries.retain(|known| *known != query);
            entry.queries.insert(0, query);
            entry.queries.truncate(5);
        }
        if self.items.len() > KEEP {
            let mut by_age: Vec<(i64, String)> = self.items.iter().map(|(key, item)| (item.last, key.clone())).collect();
            by_age.sort();
            for (_, key) in by_age.into_iter().take(self.items.len() - KEEP) {
                self.items.remove(&key);
            }
        }
    }

    pub fn get(&self, path: &str) -> Option<&Opened> {
        self.items.get(&path.to_lowercase())
    }

    pub fn items(&self) -> impl Iterator<Item = &Opened> {
        self.items.values()
    }

    /// Writes the file (atomically: a temporary file, then a rename).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        if let Some(folder) = file.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let mut items: Vec<&Opened> = self.items.values().collect();
        items.sort_by(|a, b| b.last.cmp(&a.last));
        let temporary = file.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(&items).map_err(std::io::Error::other)?)?;
        std::fs::rename(&temporary, file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_round_trips() {
        let folder = std::env::temp_dir().join(format!("excavate-history-{}", std::process::id()));
        let file = folder.join("history.json");
        let mut history = History::load(&file);
        history.record(r"C:\a\Report.pdf", "report", 100);
        history.record(r"c:\a\report.pdf", "Q3 Report", 200);
        let opened = history.get(r"C:\A\REPORT.PDF").unwrap();
        assert_eq!((opened.count, opened.last), (2, 200));
        assert_eq!(opened.queries, ["q3 report", "report"]);
        history.save().unwrap();
        let again = History::load(&file);
        assert_eq!(again.get(r"c:\a\report.pdf").unwrap().count, 2);
        let _ = std::fs::remove_dir_all(folder);
    }
}
