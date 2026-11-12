//! When and where each image was taken (its EXIF metadata, and the place named from its position), in a small file
//! beside the vectors (`facts.tsv`): the reader adds to it, the search helper reads it to answer "last winter" and
//! "Prague". It only grows, a line an image (`A`) or a gone one (`R`); the latest line for a path counts.

use crate::places::Place;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fact {
    /// When it was taken by the camera's clock (seconds since 1970 counted as if UTC), or failing that when it was last
    /// changed, in the same local reckoning.
    pub when: i64,
    /// Whether `when` came from the camera rather than the file.
    pub taken: bool,
    pub place: Option<Place>,
}

pub fn file(folder: &Path) -> PathBuf {
    folder.join("facts.tsv")
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

/// Adds facts; knows which images it has, to fill in only what's missing.
pub struct Writer {
    file: std::fs::File,
    known: std::collections::HashSet<String>,
}

impl Writer {
    pub fn open(folder: &Path) -> std::io::Result<Writer> {
        std::fs::create_dir_all(folder)?;
        let path = file(folder);
        let known = Facts::read(&path).map(|facts| facts.by_path.into_keys().collect()).unwrap_or_default();
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Writer { file, known })
    }

    pub fn has(&self, path: &str) -> bool {
        self.known.contains(&key(path))
    }

    pub fn add(&mut self, path: &str, fact: &Fact) -> std::io::Result<()> {
        let place = fact.place.as_ref().map(Place::joined).unwrap_or_default();
        writeln!(self.file, "A\t{path}\t{}\t{}\t{place}", fact.when, u8::from(fact.taken))?;
        self.known.insert(key(path));
        Ok(())
    }

    pub fn remove(&mut self, path: &str) -> std::io::Result<()> {
        if self.known.remove(&key(path)) {
            writeln!(self.file, "R\t{path}")?;
        }
        Ok(())
    }
}

/// Every image's facts, for the search helper; `refresh` takes in what was added since.
#[derive(Default)]
pub struct Facts {
    path: PathBuf,
    read_to: u64,
    pub by_path: HashMap<String, Fact>,
}

impl Facts {
    pub fn load(folder: &Path) -> Facts {
        Facts::read(&file(folder)).unwrap_or_else(|| Facts { path: file(folder), ..Facts::default() })
    }

    fn read(path: &Path) -> Option<Facts> {
        let mut facts = Facts { path: path.to_path_buf(), ..Facts::default() };
        std::fs::metadata(path).ok()?;
        facts.refresh();
        Some(facts)
    }

    pub fn refresh(&mut self) {
        let Ok(mut file) = std::fs::File::open(&self.path) else { return };
        let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if length < self.read_to {
            self.read_to = 0;
            self.by_path.clear();
        }
        if length == self.read_to || file.seek(SeekFrom::Start(self.read_to)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let Ok(read) = reader.read_line(&mut line) else { break };
            // Only whole lines: one being written is read next time.
            if read == 0 || !line.ends_with('\n') {
                break;
            }
            self.read_to += read as u64;
            let fields: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
            match fields[..] {
                ["A", path, when, taken, place] => {
                    if let Ok(when) = when.parse() {
                        self.by_path.insert(key(path), Fact { when, taken: taken == "1", place: Place::split(place) });
                    }
                }
                ["R", path] => {
                    self.by_path.remove(&key(path));
                }
                _ => {}
            }
        }
    }

    pub fn get(&self, path: &str) -> Option<&Fact> {
        self.by_path.get(&key(path))
    }

    /// Every name a place in these images goes by (lowercase), for telling a place in a search from other words.
    pub fn place_names(&self) -> std::collections::HashSet<String> {
        self.by_path.values().filter_map(|fact| fact.place.as_ref()).flat_map(Place::names).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_and_read_back() {
        let folder = std::env::temp_dir().join(format!("excavate-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let mut writer = Writer::open(&folder).unwrap();
        let prague = Place::split("Prague|Prague|Czechia");
        writer.add(r"C:\Photos\a.jpg", &Fact { when: 100, taken: true, place: prague.clone() }).unwrap();
        writer.add(r"C:\Photos\b.jpg", &Fact { when: 200, taken: false, place: None }).unwrap();
        let mut facts = Facts::load(&folder);
        assert_eq!(facts.get(r"c:\photos\A.JPG").unwrap().place, prague);
        assert!(facts.place_names().contains("czechia"));
        writer.remove(r"C:\Photos\a.jpg").unwrap();
        facts.refresh();
        assert!(facts.get(r"C:\Photos\a.jpg").is_none());
        assert_eq!(facts.get(r"C:\Photos\b.jpg").unwrap().when, 200);
        let writer = Writer::open(&folder).unwrap();
        assert!(writer.has(r"C:\Photos\b.jpg") && !writer.has(r"C:\Photos\a.jpg"));
        let _ = std::fs::remove_dir_all(&folder);
    }
}
