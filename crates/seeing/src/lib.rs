//! Images by what they show: each model turns an image, and a description, into a vector; the closer two vectors point,
//! the better the description fits. The reader stores each image's vector (`Writer`), the search helper loads them all
//! (`Vectors`) and scores a description against every one, in milliseconds.
//!
//! Stored at full length as 8-bit integers with one scale per vector: within 0.1 of full precision, where shortening to
//! 256 numbers lost 2 to 3 points. 121,000 images: 93 MB for a 768-number model.

pub mod facts;
pub mod places;
pub mod query;

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A model that looks at images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    /// SigLIP 2 base, on the CPU (Quick, and Thorough's lighter partner).
    Siglip,
    /// EmbeddingGemma 2 at 140 image tokens, on the graphics card (Balanced, and Thorough's first).
    Gemma,
    /// Qwen3-VL-Embedding-8B, on the graphics card (Thorough's most accurate partner).
    Qwen,
    /// EmbeddingGemma 2 again, on documents' text: documents by what they mean. Its own vectors, the same model.
    GemmaText,
}

impl Engine {
    pub const ALL: [Engine; 4] = [Engine::Siglip, Engine::Gemma, Engine::Qwen, Engine::GemmaText];

    /// What the catalog and the vectors' file are named after. A new form of a model gets a new name, so what the old
    /// one made is looked at again.
    pub fn name(self) -> &'static str {
        match self {
            Engine::Siglip => "siglip2-b16",
            Engine::Gemma => "eg2-140",
            Engine::Qwen => "qwen3vl-8b",
            Engine::GemmaText => "eg2-text",
        }
    }

    pub fn dims(self) -> usize {
        match self {
            Engine::Siglip | Engine::Gemma | Engine::GemmaText => 768,
            Engine::Qwen => 4096,
        }
    }

    /// The model behind it: documents' meaning is EmbeddingGemma 2's.
    pub fn model(self) -> Engine {
        if self == Engine::GemmaText { Engine::Gemma } else { self }
    }

    pub fn on_gpu(self) -> bool {
        self != Engine::Siglip
    }
}

/// The models a choice in Settings → Images → What they show uses, the one that goes first first. `seeing` is
/// "quick", "balanced" or "deep" (Thorough), `deep` its second model ("siglip" or "qwen"); nothing while it's off.
pub fn engines(on: bool, seeing: &str, deep: &str) -> Vec<Engine> {
    if !on {
        return Vec::new();
    }
    match seeing {
        "quick" => vec![Engine::Siglip],
        "deep" if deep == "qwen" => vec![Engine::Gemma, Engine::Qwen],
        "deep" => vec![Engine::Gemma, Engine::Siglip],
        _ => vec![Engine::Gemma],
    }
}

const MAGIC: &[u8; 4] = b"EXVS";
const VERSION: u32 = 1;
const HEADER: u64 = 16;
const ADD: u8 = 1;
const REMOVE: u8 = 2;

pub fn file(folder: &Path, engine: Engine) -> PathBuf {
    folder.join(format!("{}.vectors", engine.name()))
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

/// A vector made unit length, then 8-bit integers and the scale that brings them back.
pub fn quantize(vector: &[f32]) -> (f32, Vec<i8>) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt().max(1e-12);
    let largest = vector.iter().fold(0f32, |largest, value| largest.max(value.abs())) / norm;
    let scale = (largest / 127.0).max(1e-12);
    (scale, vector.iter().map(|value| (value / norm / scale).round().clamp(-127.0, 127.0) as i8).collect())
}

/// A vector made unit length (a description's, before scoring).
pub fn normalized(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt().max(1e-12);
    vector.iter().map(|value| value / norm).collect()
}

/// Adds images' vectors to a model's file, which only grows: a changed image's new vector is added and the old one
/// left behind, a gone one marked. Rewritten when what's left behind outweighs what's there.
pub struct Writer {
    path: PathBuf,
    file: BufWriter<File>,
    dims: usize,
    /// Each image there now (by lowercase path) and its date when looked at.
    live: HashMap<String, i64>,
    records: usize,
}

impl Writer {
    pub fn open(folder: &Path, engine: Engine) -> std::io::Result<Writer> {
        std::fs::create_dir_all(folder)?;
        let path = file(folder, engine);
        let dims = engine.dims();
        let mut live = HashMap::new();
        let mut records = 0;
        let mut good_to = 0;
        if let Ok(existing) = File::open(&path) {
            let mut reader = BufReader::new(existing);
            if read_header(&mut reader).is_some_and(|(found, _)| found == dims) {
                good_to = HEADER;
                while let Some((record, length)) = read_record(&mut reader, dims) {
                    match record {
                        Record::Add { path, modified, .. } => {
                            live.insert(key(&path), modified);
                        }
                        Record::Remove { path } => {
                            live.remove(&key(&path));
                        }
                    }
                    records += 1;
                    good_to += length;
                }
            }
        }
        let file = if good_to >= HEADER {
            let file = std::fs::OpenOptions::new().write(true).open(&path)?;
            // A record cut short (the reader was stopped mid-write) goes.
            file.set_len(good_to)?;
            let mut file = BufWriter::new(file);
            file.seek(SeekFrom::End(0))?;
            file
        } else {
            let mut file = BufWriter::new(File::create(&path)?);
            write_header(&mut file, dims)?;
            file.flush()?;
            file
        };
        Ok(Writer { path, file, dims, live, records })
    }

    /// Whether this image was looked at, as it is now.
    pub fn has(&self, path: &str, modified: i64) -> bool {
        self.live.get(&key(path)) == Some(&modified)
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub fn add(&mut self, path: &str, modified: i64, vector: &[f32]) -> std::io::Result<()> {
        if vector.len() != self.dims {
            return Err(std::io::Error::other(format!("{} numbers, not {}", vector.len(), self.dims)));
        }
        let (scale, values) = quantize(vector);
        write_record(&mut self.file, &Record::Add { path: path.to_string(), modified, scale, values })?;
        self.file.flush()?;
        self.live.insert(key(path), modified);
        self.records += 1;
        self.compact_if_worth()
    }

    pub fn remove(&mut self, path: &str) -> std::io::Result<()> {
        if self.live.remove(&key(path)).is_none() {
            return Ok(());
        }
        write_record(&mut self.file, &Record::Remove { path: path.to_string() })?;
        self.file.flush()?;
        self.records += 1;
        self.compact_if_worth()
    }

    /// Every image looked at (lowercase paths): for leaving out what's gone.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.live.keys()
    }

    fn compact_if_worth(&mut self) -> std::io::Result<()> {
        if self.records < 2 * self.live.len() + 1000 {
            return Ok(());
        }
        // The latest of each, in a new file under a new generation, put in place of the old one: the helper sees the
        // generation change and loads it afresh.
        let temporary = self.path.with_extension("compacting");
        {
            let mut reader = BufReader::new(File::open(&self.path)?);
            read_header(&mut reader).ok_or_else(|| std::io::Error::other("bad header"))?;
            let mut latest: HashMap<String, Record> = HashMap::new();
            while let Some((record, _)) = read_record(&mut reader, self.dims) {
                match &record {
                    Record::Add { path, .. } => {
                        latest.insert(key(path), record);
                    }
                    Record::Remove { path } => {
                        latest.remove(&key(path));
                    }
                }
            }
            let mut out = BufWriter::new(File::create(&temporary)?);
            write_header(&mut out, self.dims)?;
            for record in latest.values() {
                write_record(&mut out, record)?;
            }
            out.flush()?;
            self.records = latest.len();
        }
        std::fs::rename(&temporary, &self.path)?;
        let mut file = BufWriter::new(std::fs::OpenOptions::new().write(true).open(&self.path)?);
        file.seek(SeekFrom::End(0))?;
        self.file = file;
        Ok(())
    }
}

enum Record {
    Add { path: String, modified: i64, scale: f32, values: Vec<i8> },
    Remove { path: String },
}

fn write_header(out: &mut impl Write, dims: usize) -> std::io::Result<()> {
    let generation = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|time| time.subsec_nanos() ^ time.as_secs() as u32).unwrap_or(1);
    out.write_all(MAGIC)?;
    out.write_all(&VERSION.to_le_bytes())?;
    out.write_all(&(dims as u32).to_le_bytes())?;
    out.write_all(&generation.to_le_bytes())
}

/// (dims, generation)
fn read_header(input: &mut impl Read) -> Option<(usize, u32)> {
    let mut header = [0u8; HEADER as usize];
    input.read_exact(&mut header).ok()?;
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    (&header[..4] == MAGIC && word(4) == VERSION).then(|| (word(8) as usize, word(12)))
}

fn write_record(out: &mut impl Write, record: &Record) -> std::io::Result<()> {
    match record {
        Record::Add { path, modified, scale, values } => {
            out.write_all(&[ADD])?;
            out.write_all(&(path.len() as u16).to_le_bytes())?;
            out.write_all(path.as_bytes())?;
            out.write_all(&modified.to_le_bytes())?;
            out.write_all(&scale.to_le_bytes())?;
            out.write_all(&values.iter().map(|&value| value as u8).collect::<Vec<u8>>())
        }
        Record::Remove { path } => {
            out.write_all(&[REMOVE])?;
            out.write_all(&(path.len() as u16).to_le_bytes())?;
            out.write_all(path.as_bytes())
        }
    }
}

/// The next whole record and its length in bytes; none at the end, or where one is cut short.
fn read_record(input: &mut impl Read, dims: usize) -> Option<(Record, u64)> {
    let mut head = [0u8; 3];
    input.read_exact(&mut head).ok()?;
    let length = u16::from_le_bytes([head[1], head[2]]) as usize;
    let mut path = vec![0u8; length];
    input.read_exact(&mut path).ok()?;
    let path = String::from_utf8(path).ok()?;
    match head[0] {
        ADD => {
            let mut fixed = [0u8; 12];
            input.read_exact(&mut fixed).ok()?;
            let mut values = vec![0u8; dims];
            input.read_exact(&mut values).ok()?;
            let modified = i64::from_le_bytes(fixed[..8].try_into().unwrap());
            let scale = f32::from_le_bytes(fixed[8..].try_into().unwrap());
            let values = values.into_iter().map(|value| value as i8).collect();
            Some((Record::Add { path, modified, scale, values }, (3 + length + 12 + dims) as u64))
        }
        REMOVE => Some((Record::Remove { path }, (3 + length) as u64)),
        _ => None,
    }
}

/// A model's vectors, all in memory, for scoring. `refresh` takes in what the reader added since.
pub struct Vectors {
    pub engine: Engine,
    file: PathBuf,
    generation: u32,
    read_to: u64,
    paths: Vec<String>,
    scales: Vec<f32>,
    data: Vec<i8>,
    slots: HashMap<String, usize>,
    live: usize,
}

impl Vectors {
    /// None when the model hasn't looked at anything yet.
    pub fn load(folder: &Path, engine: Engine) -> Option<Vectors> {
        let mut vectors = Vectors {
            engine,
            file: file(folder, engine),
            generation: 0,
            read_to: 0,
            paths: Vec::new(),
            scales: Vec::new(),
            data: Vec::new(),
            slots: HashMap::new(),
            live: 0,
        };
        vectors.refresh();
        (vectors.read_to > 0).then_some(vectors)
    }

    /// Takes in what was added since; everything again when the file was rewritten.
    pub fn refresh(&mut self) {
        let Ok(file) = File::open(&self.file) else { return };
        let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let mut reader = BufReader::with_capacity(1 << 20, file);
        let Some((dims, generation)) = read_header(&mut reader) else { return };
        if dims != self.engine.dims() {
            return;
        }
        if generation != self.generation || length < self.read_to {
            *self = Vectors { engine: self.engine, file: self.file.clone(), generation, read_to: HEADER, paths: Vec::new(), scales: Vec::new(),
                              data: Vec::new(), slots: HashMap::new(), live: 0 };
        }
        if length == self.read_to || reader.seek(SeekFrom::Start(self.read_to)).is_err() {
            return;
        }
        while let Some((record, size)) = read_record(&mut reader, dims) {
            self.read_to += size;
            match record {
                Record::Add { path, scale, values, .. } => {
                    let key = key(&path);
                    match self.slots.get(&key) {
                        Some(&slot) => {
                            if self.scales[slot] == 0.0 {
                                self.live += 1;
                            }
                            self.paths[slot] = path;
                            self.scales[slot] = scale;
                            self.data[slot * dims..(slot + 1) * dims].copy_from_slice(&values);
                        }
                        None => {
                            self.slots.insert(key, self.paths.len());
                            self.paths.push(path);
                            self.scales.push(scale);
                            self.data.extend_from_slice(&values);
                            self.live += 1;
                        }
                    }
                }
                Record::Remove { path } => {
                    if let Some(&slot) = self.slots.get(&key(&path)) {
                        if self.scales[slot] != 0.0 {
                            self.live -= 1;
                        }
                        self.scales[slot] = 0.0;
                    }
                }
            }
        }
    }

    /// How many images it has looked at.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How well `query` (unit length) fits each image, -1 to 1; gone ones get nothing (NaN).
    pub fn scores(&self, query: &[f32]) -> Vec<f32> {
        let dims = self.engine.dims();
        let count = self.paths.len();
        let mut out = vec![f32::NAN; count];
        if query.len() != dims || count == 0 {
            return out;
        }
        let threads = std::thread::available_parallelism().map(|threads| threads.get()).unwrap_or(4).clamp(1, 8);
        let chunk = count.div_ceil(threads).max(2048);
        std::thread::scope(|scope| {
            for (index, slice) in out.chunks_mut(chunk).enumerate() {
                let start = index * chunk;
                scope.spawn(move || {
                    for (offset, score) in slice.iter_mut().enumerate() {
                        let slot = start + offset;
                        let scale = self.scales[slot];
                        if scale == 0.0 {
                            continue;
                        }
                        let row = &self.data[slot * dims..(slot + 1) * dims];
                        let dot: f32 = row.iter().zip(query).map(|(&value, &weight)| f32::from(value) * weight).sum();
                        *score = dot * scale;
                    }
                });
            }
        });
        out
    }

    pub fn path(&self, slot: usize) -> &str {
        &self.paths[slot]
    }

    /// Changes whenever what's stored does (a vector added, removed or replaced, or the file written anew).
    pub fn version(&self) -> (u32, u64) {
        (self.generation, self.read_to)
    }

    /// Each video's frames averaged into one vector (unit length), by the video's path (lowercased, without its
    /// moment), for ranking videos (`video_bonus`).
    pub fn video_means(&self) -> Vec<(String, Vec<f32>)> {
        let dims = self.engine.dims();
        let mut sums: HashMap<String, Vec<f32>> = HashMap::new();
        for (slot, path) in self.paths.iter().enumerate() {
            let scale = self.scales[slot];
            let Some((video, _)) = path.split_once("#t=") else { continue };
            if scale == 0.0 {
                continue;
            }
            let sum = sums.entry(key(video)).or_insert_with(|| vec![0.0; dims]);
            for (total, &value) in sum.iter_mut().zip(&self.data[slot * dims..(slot + 1) * dims]) {
                *total += f32::from(value) * scale;
            }
        }
        sums.into_iter().map(|(video, sum)| (video, normalized(&sum))).collect()
    }
}

/// How much a video's frames together add to its best frame: a quarter of
/// how well its averaged frames fit `query`, above or below the other videos', on the scale `scores` (every vector's,
/// as `combine` standardizes them) is on. By video path (lowercased); nothing when there are fewer than two videos.
pub fn video_bonus(means: &[(String, Vec<f32>)], scores: &[f32], query: &[f32]) -> HashMap<String, f32> {
    let valid: Vec<f32> = scores.iter().copied().filter(|score| !score.is_nan()).collect();
    if means.len() < 2 || valid.len() < 2 {
        return HashMap::new();
    }
    let mean = valid.iter().sum::<f32>() / valid.len() as f32;
    let deviation = (valid.iter().map(|score| (score - mean).powi(2)).sum::<f32>() / valid.len() as f32).sqrt().max(1e-6);
    let fits: Vec<f32> = means.iter().map(|(_, vector)| (vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>() - mean) / deviation).collect();
    let average = fits.iter().sum::<f32>() / fits.len() as f32;
    means.iter().zip(fits).map(|((video, _), fit)| (video.clone(), VIDEO_MEAN_WEIGHT * (fit - average))).collect()
}

/// Images and videos by what they show, as the search helper answers: `combine`, every frame ranked so each video's
/// best is there however many frames come before it; a video kept once, at its best moment, with what its frames
/// together add (`video_bonus`, one map a model, averaged); the best `limit`.
pub fn rank(results: &[(&Vectors, Vec<f32>)], bonuses: &[HashMap<String, f32>], limit: usize) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    let mut hits = combine(results, usize::MAX);
    hits.retain(|hit| seen.insert(key(hit.path.split("#t=").next().unwrap_or(&hit.path))));
    if bonuses.iter().any(|bonus| !bonus.is_empty()) {
        for hit in &mut hits {
            let Some((video, _)) = hit.path.split_once("#t=") else { continue };
            let video = key(video);
            let added: Vec<f32> = bonuses.iter().filter_map(|bonus| bonus.get(&video).copied()).collect();
            if !added.is_empty() {
                hit.score += added.iter().sum::<f32>() / added.len() as f32;
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    }
    hits.truncate(limit);
    hits
}

/// Measured on MSR-VTT with EmbeddingGemma 2: 0.25 gained 1.3 R@1 at Balanced and 1.0 at Thorough (R@10 -0.2 and
/// +1.5); 0.5 and 0.75 gained less at R@1 and lost at Balanced's R@10.
pub const VIDEO_MEAN_WEIGHT: f32 = 0.25;

/// One image found by what it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: String,
    /// How far above the rest it stands: each model's similarities standardized over every image (mean 0, deviation 1)
    /// and averaged over the models that have looked at it.
    pub score: f32,
    /// The first model's own similarity (-1 to 1).
    pub similarity: f32,
}

/// Combines each model's scores (`Vectors::scores`) into the best `limit` images, as the benchmark did: standardized per
/// search, then together. Averaged, so an image only one model has looked at yet isn't held back.
pub fn combine(results: &[(&Vectors, Vec<f32>)], limit: usize) -> Vec<Hit> {
    let mut by_path: HashMap<String, (f32, usize, f32, String)> = HashMap::new();
    for (index, (vectors, scores)) in results.iter().enumerate() {
        let valid: Vec<f32> = scores.iter().copied().filter(|score| !score.is_nan()).collect();
        if valid.len() < 2 {
            continue;
        }
        let mean = valid.iter().sum::<f32>() / valid.len() as f32;
        let deviation = (valid.iter().map(|score| (score - mean).powi(2)).sum::<f32>() / valid.len() as f32).sqrt().max(1e-6);
        for (slot, &score) in scores.iter().enumerate() {
            if score.is_nan() {
                continue;
            }
            let path = vectors.path(slot);
            let entry = by_path.entry(key(path)).or_insert((0.0, 0, f32::NAN, path.to_string()));
            entry.0 += (score - mean) / deviation;
            entry.1 += 1;
            if index == 0 || entry.2.is_nan() {
                entry.2 = score;
            }
        }
    }
    let mut hits: Vec<Hit> = by_path.into_values().map(|(sum, count, similarity, path)| Hit { path, score: sum / count as f32, similarity }).collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("excavate-seeing-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        folder
    }

    fn vector(seed: usize) -> Vec<f32> {
        (0..768).map(|index| (((index * 31 + seed * 17) % 97) as f32 - 48.0) / 48.0).collect()
    }

    #[test]
    fn stored_close_to_full_precision() {
        let original = vector(3);
        let (scale, values) = quantize(&original);
        let unit = normalized(&original);
        let back: Vec<f32> = values.iter().map(|&value| f32::from(value) * scale).collect();
        let cosine: f32 = back.iter().zip(&unit).map(|(a, b)| a * b).sum::<f32>() / back.iter().map(|a| a * a).sum::<f32>().sqrt();
        assert!(cosine > 0.9999, "{cosine}");
    }

    #[test]
    fn written_then_read_and_scored() {
        let folder = folder("store");
        let mut writer = Writer::open(&folder, Engine::Gemma).unwrap();
        writer.add(r"C:\Pictures\pie.jpg", 5, &vector(1)).unwrap();
        writer.add(r"C:\Pictures\shirt.jpg", 6, &vector(2)).unwrap();
        writer.add(r"C:\Pictures\beach.jpg", 7, &vector(3)).unwrap();
        assert!(writer.has(r"c:\pictures\PIE.jpg", 5));
        assert!(!writer.has(r"C:\Pictures\pie.jpg", 6));
        let mut vectors = Vectors::load(&folder, Engine::Gemma).unwrap();
        assert_eq!(vectors.len(), 3);
        let query = normalized(&vector(2));
        let hits = combine(&[(&vectors, vectors.scores(&query))], 2);
        assert_eq!(hits[0].path, r"C:\Pictures\shirt.jpg");
        assert!((hits[0].similarity - 1.0).abs() < 0.01, "{}", hits[0].similarity);

        // Changed, gone, and added while the helper has it loaded: taken in by refresh.
        writer.add(r"C:\Pictures\pie.jpg", 9, &vector(2)).unwrap();
        writer.remove(r"C:\Pictures\shirt.jpg").unwrap();
        writer.add(r"C:\Pictures\new.jpg", 9, &vector(4)).unwrap();
        vectors.refresh();
        assert_eq!(vectors.len(), 3);
        let hits = combine(&[(&vectors, vectors.scores(&query))], 5);
        assert_eq!(hits[0].path, r"C:\Pictures\pie.jpg");
        assert!(hits.iter().all(|hit| hit.path != r"C:\Pictures\shirt.jpg"));

        // Opened again, it knows what it has; a record cut short is dropped.
        drop(writer);
        let mut file = std::fs::OpenOptions::new().append(true).open(super::file(&folder, Engine::Gemma)).unwrap();
        file.write_all(&[ADD, 9, 0, b'C']).unwrap();
        drop(file);
        let writer = Writer::open(&folder, Engine::Gemma).unwrap();
        assert_eq!(writer.len(), 3);
        assert!(writer.has(r"C:\Pictures\pie.jpg", 9));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn rewritten_when_mostly_left_behind() {
        let folder = folder("compact");
        let mut writer = Writer::open(&folder, Engine::Siglip).unwrap();
        for round in 0..5 {
            for index in 0..400 {
                writer.add(&format!(r"C:\x\{index}.png"), round, &vector(index + round as usize)).unwrap();
            }
        }
        // 2,000 records for 400 images: rewritten once past 2 × 400 + 1,000.
        assert!(std::fs::metadata(super::file(&folder, Engine::Siglip)).unwrap().len() < 1000 * 800);
        let vectors = Vectors::load(&folder, Engine::Siglip).unwrap();
        assert_eq!(vectors.len(), 400);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn two_models_combined() {
        let (one, two) = (folder("one"), folder("two"));
        let mut gemma = Writer::open(&one, Engine::Gemma).unwrap();
        let mut siglip = Writer::open(&two, Engine::Siglip).unwrap();
        for index in 0..20 {
            gemma.add(&format!(r"C:\x\{index}.png"), 1, &vector(index)).unwrap();
            // The second model has looked at half so far.
            if index % 2 == 0 {
                siglip.add(&format!(r"C:\x\{index}.png"), 1, &vector(index)).unwrap();
            }
        }
        let (a, b) = (Vectors::load(&one, Engine::Gemma).unwrap(), Vectors::load(&two, Engine::Siglip).unwrap());
        let query = normalized(&vector(7));
        let hits = combine(&[(&a, a.scores(&query)), (&b, b.scores(&query))], 3);
        assert_eq!(hits[0].path, r"C:\x\7.png");
        let _ = std::fs::remove_dir_all(&one);
        let _ = std::fs::remove_dir_all(&two);
    }

    #[test]
    fn choices() {
        assert_eq!(engines(true, "quick", "qwen"), [Engine::Siglip]);
        assert_eq!(engines(true, "balanced", "siglip"), [Engine::Gemma]);
        assert_eq!(engines(true, "deep", "siglip"), [Engine::Gemma, Engine::Siglip]);
        assert_eq!(engines(true, "deep", "qwen"), [Engine::Gemma, Engine::Qwen]);
        assert!(engines(false, "balanced", "siglip").is_empty());
    }
}
