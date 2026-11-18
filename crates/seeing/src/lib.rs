//! Images by what they show: each model turns an image, and a description, into a vector; the closer two vectors point,
//! the better the description fits. The reader stores each image's vector (`Writer`), the search helper loads them all
//! (`Vectors`) and scores a description against every one, in milliseconds.
//!
//! Stored at full length as 8-bit integers with one scale per vector: within 0.1 of full precision, where shortening to
//! 256 numbers lost 2 to 3 points. 121,000 images: 93 MB for a 768-number model.

pub mod facts;
pub mod places;
pub mod query;
pub mod sensitive;

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
/// Windows' answer to cutting short a file another process has mapped.
const ERROR_USER_MAPPED_FILE: i32 = 1224;

pub fn file(folder: &Path, engine: Engine) -> PathBuf {
    folder.join(format!("{}.vectors", engine.name()))
}

fn key(path: &str) -> String {
    path.to_lowercase()
}

/// A number for a lowercase path (FNV-1a, 64 bits): the same in every process and every run.
fn hashed(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// A lowercase path's file, and what names a part of it ("#c=3", a document's piece, or "#t=12.0", a video's moment).
fn split_part(key: &str) -> (&str, &str) {
    match key.rfind("#c=").or_else(|| key.rfind("#t=")) {
        Some(at) => (&key[..at], &key[at..]),
        None => (key, ""),
    }
}

/// What a writer has now, by lowercase path. A document's pieces and a video's frames share their file's name, kept
/// once, with the others in one block of text: with a quarter of a million pieces, a name kept for each one held 55 MB.
#[derive(Default)]
struct Live {
    /// Each file's name (lowercase): where it is in `text` and how long it is.
    files: Vec<(u32, u32)>,
    text: String,
    /// The file each name's number stands for.
    file_ids: HashMap<u64, u32>,
    /// Each path here now, by its number: its file, its date when looked at, and its part.
    items: HashMap<u64, Item>,
}

struct Item {
    file: u32,
    modified: i64,
    part: Box<str>,
}

impl Live {
    fn file(&self, id: u32) -> &str {
        let (start, length) = self.files[id as usize];
        &self.text[start as usize..(start + length) as usize]
    }

    fn get(&self, key: &str) -> Option<i64> {
        self.items.get(&hashed(key)).map(|item| item.modified)
    }

    fn insert(&mut self, key: &str, modified: i64) {
        let (file, part) = split_part(key);
        let file = match self.file_ids.get(&hashed(file)) {
            Some(&id) => id,
            None => {
                let id = self.files.len() as u32;
                self.files.push((self.text.len() as u32, file.len() as u32));
                self.text.push_str(file);
                self.file_ids.insert(hashed(file), id);
                id
            }
        };
        self.items.insert(hashed(key), Item { file, modified, part: part.into() });
    }

    fn remove(&mut self, key: &str) -> bool {
        self.items.remove(&hashed(key)).is_some()
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn paths(&self) -> impl Iterator<Item = String> + '_ {
        self.items.values().map(|item| format!("{}{}", self.file(item.file), item.part))
    }

    /// The parts of this file here now ("…#c=0", "…#c=1"), as lowercase paths.
    fn parts_of(&self, file: &str) -> Vec<String> {
        let Some(&id) = self.file_ids.get(&hashed(file)) else { return Vec::new() };
        self.items.values().filter(|item| item.file == id && !item.part.is_empty()).map(|item| format!("{file}{}", item.part)).collect()
    }
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
    live: Live,
    /// The images there now that are blurred whole (`sensitive`; not a video's frames), by lowercase path: each one's
    /// path as written and what it shows.
    sensitive: HashMap<String, (String, sensitive::Shows)>,
    engine: Engine,
    records: usize,
}

impl Writer {
    pub fn open(folder: &Path, engine: Engine) -> std::io::Result<Writer> {
        std::fs::create_dir_all(folder)?;
        let path = file(folder, engine);
        let dims = engine.dims();
        let mut live = Live::default();
        let mut sensitive = HashMap::new();
        let mut records = 0;
        let mut good_to = 0;
        if let Ok(existing) = File::open(&path) {
            let mut reader = BufReader::new(existing);
            if read_header(&mut reader).is_some_and(|(found, _)| found == dims) {
                good_to = HEADER;
                while let Some((record, length)) = read_record(&mut reader, dims) {
                    match record {
                        Record::Add { path, modified, scale, values } => {
                            let vector: Vec<f32> = values.iter().map(|&value| f32::from(value) * scale).collect();
                            judge(&mut sensitive, engine, &path, &vector);
                            live.insert(&key(&path), modified);
                        }
                        Record::Remove { path } => {
                            live.remove(&key(&path));
                            sensitive.remove(&key(&path));
                        }
                    }
                    records += 1;
                    good_to += length;
                }
            }
        }
        let file = if good_to >= HEADER {
            let file = std::fs::OpenOptions::new().write(true).open(&path)?;
            // A record cut short (the reader was stopped mid-write) goes. Windows refuses that while the search helper
            // has the file mapped (`Vectors`): it lets go a minute after its last search.
            let mut tries = 0;
            while file.metadata()?.len() != good_to {
                match file.set_len(good_to) {
                    Err(error) if error.raw_os_error() == Some(ERROR_USER_MAPPED_FILE) && tries < 14 => {
                        tries += 1;
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                    other => other?,
                }
            }
            let mut file = BufWriter::new(file);
            file.seek(SeekFrom::End(0))?;
            file
        } else {
            let mut file = BufWriter::new(File::create(&path)?);
            write_header(&mut file, dims)?;
            file.flush()?;
            file
        };
        Ok(Writer { path, file, dims, live, sensitive, engine, records })
    }

    /// Whether this image was looked at, as it is now.
    pub fn has(&self, path: &str, modified: i64) -> bool {
        self.live.get(&key(path)) == Some(modified)
    }

    pub fn len(&self) -> usize {
        self.live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.live.len() == 0
    }

    pub fn add(&mut self, path: &str, modified: i64, vector: &[f32]) -> std::io::Result<()> {
        if vector.len() != self.dims {
            return Err(std::io::Error::other(format!("{} numbers, not {}", vector.len(), self.dims)));
        }
        let (scale, values) = quantize(vector);
        write_record(&mut self.file, &Record::Add { path: path.to_string(), modified, scale, values })?;
        self.file.flush()?;
        judge(&mut self.sensitive, self.engine, path, vector);
        self.live.insert(&key(path), modified);
        self.records += 1;
        self.compact_if_worth()
    }

    pub fn remove(&mut self, path: &str) -> std::io::Result<()> {
        self.sensitive.remove(&key(path));
        if !self.live.remove(&key(path)) {
            return Ok(());
        }
        write_record(&mut self.file, &Record::Remove { path: path.to_string() })?;
        self.file.flush()?;
        self.records += 1;
        self.compact_if_worth()
    }

    /// Every image looked at (lowercase paths): for leaving out what's gone.
    pub fn paths(&self) -> impl Iterator<Item = String> + '_ {
        self.live.paths()
    }

    /// A file's parts here now, a document's pieces or a video's frames (lowercase paths): for taking them out before
    /// it's looked at again.
    pub fn parts_of(&self, path: &str) -> Vec<String> {
        self.live.parts_of(&key(path))
    }

    /// What this image shows that blurs it whole (`sensitive`), as last looked at.
    pub fn shows(&self, path: &str) -> Option<sensitive::Shows> {
        self.sensitive.get(&key(path)).map(|(_, shows)| *shows)
    }

    /// The images blurred whole, and what each shows.
    pub fn sensitive(&self) -> impl Iterator<Item = (&str, sensitive::Shows)> {
        self.sensitive.values().map(|(path, shows)| (path.as_str(), *shows))
    }

    fn compact_if_worth(&mut self) -> std::io::Result<()> {
        if self.records < 2 * self.live.len() + 1000 {
            return Ok(());
        }
        // The latest of each, in a new file under a new generation, put in place of the old one: the helper sees the
        // generation change and loads it afresh. Streamed, in two passes: the first notes where each file's latest
        // record is (a number a file, not its vector: ~200 MB held for a moment before, for 240,000 pieces), the second
        // copies those records across as they come.
        let temporary = self.path.with_extension("compacting");
        {
            let mut reader = BufReader::with_capacity(1 << 20, File::open(&self.path)?);
            read_header(&mut reader).ok_or_else(|| std::io::Error::other("bad header"))?;
            let start = HEADER;
            // Each file's latest record: where it starts, and whether it adds (a removal leaves the file out).
            let mut latest: HashMap<u64, (u64, bool, i64)> = HashMap::new();
            let mut at = start;
            while let Some((record, size)) = read_record(&mut reader, self.dims) {
                match &record {
                    Record::Add { path, modified, .. } => latest.insert(hashed(&key(path)), (at, true, *modified)),
                    Record::Remove { path } => latest.insert(hashed(&key(path)), (at, false, 0)),
                };
                at += size;
            }
            // The files' list is made again as the records are copied (the names of what's gone go from memory too):
            // the old one goes first, so the two are never held at once. If the copy fails, it's read back from the
            // file as it was.
            self.live = Live::default();
            let copied = (|| -> std::io::Result<(Live, usize)> {
                let mut out = BufWriter::with_capacity(1 << 20, File::create(&temporary)?);
                write_header(&mut out, self.dims)?;
                let mut reader = BufReader::with_capacity(1 << 20, File::open(&self.path)?);
                read_header(&mut reader).ok_or_else(|| std::io::Error::other("bad header"))?;
                let mut live = Live::default();
                let mut kept = 0usize;
                let mut at = start;
                while let Some((record, size)) = read_record(&mut reader, self.dims) {
                    if let Record::Add { path, modified, .. } = &record {
                        let k = key(path);
                        if latest.get(&hashed(&k)) == Some(&(at, true, *modified)) {
                            write_record(&mut out, &record)?;
                            live.insert(&k, *modified);
                            kept += 1;
                        }
                    }
                    at += size;
                }
                out.flush()?;
                Ok((live, kept))
            })();
            match copied {
                Ok((live, kept)) => {
                    self.live = live;
                    self.records = kept;
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary);
                    self.live = read_live(&self.path, self.dims)?;
                    return Err(error);
                }
            }
        }
        std::fs::rename(&temporary, &self.path)?;
        let mut file = BufWriter::new(std::fs::OpenOptions::new().write(true).open(&self.path)?);
        file.seek(SeekFrom::End(0))?;
        self.file = file;
        Ok(())
    }
}

/// The files a vectors file has now, read from it.
fn read_live(path: &Path, dims: usize) -> std::io::Result<Live> {
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    read_header(&mut reader).ok_or_else(|| std::io::Error::other("bad header"))?;
    let mut live = Live::default();
    while let Some((record, _)) = read_record(&mut reader, dims) {
        match record {
            Record::Add { path, modified, .. } => live.insert(&key(&path), modified),
            Record::Remove { path } => {
                live.remove(&key(&path));
            }
        }
    }
    Ok(live)
}

/// Notes whether an image (not a video's frame) is one to blur whole.
fn judge(sensitive: &mut HashMap<String, (String, sensitive::Shows)>, engine: Engine, path: &str, vector: &[f32]) {
    match sensitive::shows(engine, vector).filter(|_| !path.contains("#t=") && !path.contains("#c=")) {
        Some(shows) => sensitive.insert(key(path), (path.to_string(), shows)),
        None => sensitive.remove(&key(path)),
    };
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

/// One record where it lies in a mapped file: its kind, where its path is, and its size; an Add's scale. None at the
/// end, or where one is cut short (the reader may be writing it now).
struct Mapped {
    add: bool,
    path: std::ops::Range<usize>,
    scale: f32,
    size: usize,
}

fn mapped_record(bytes: &[u8], at: usize, dims: usize) -> Option<Mapped> {
    let head = bytes.get(at..at + 3)?;
    let length = u16::from_le_bytes([head[1], head[2]]) as usize;
    let path = at + 3..at + 3 + length;
    std::str::from_utf8(bytes.get(path.clone())?).ok()?;
    match head[0] {
        ADD => {
            let fixed = bytes.get(path.end..path.end + 12)?;
            bytes.get(path.end + 12..path.end + 12 + dims)?;
            let scale = f32::from_le_bytes(fixed[8..].try_into().ok()?);
            Some(Mapped { add: true, path, scale, size: 3 + length + 12 + dims })
        }
        REMOVE => Some(Mapped { add: false, path, scale: 0.0, size: 3 + length }),
        _ => None,
    }
}

/// A model's vectors, for scoring, read where they lie in its file: the file is mapped, not copied, so they take none
/// of the helper's own memory (a quarter of a million documents' pieces held 270 MB), and Windows keeps them in memory
/// while they're used. `refresh` takes in what the reader added since.
pub struct Vectors {
    pub engine: Engine,
    file: PathBuf,
    generation: u32,
    read_to: u64,
    /// The file as it was at the last `refresh`. Its bytes never change while mapped: the reader only adds to the
    /// end, a rewrite is a new file put in its place (this one stays readable), and cutting the file short, the one
    /// change that would touch them, is refused by Windows while it's mapped (`Writer::open` waits for it).
    map: Option<memmap2::Mmap>,
    /// Where each one's record starts in the file, and its scale (none: it's gone).
    at: Vec<u64>,
    scales: Vec<f32>,
    /// Where each is, by its lowercase path's number (`hashed`).
    slots: HashMap<u64, usize>,
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
            map: None,
            at: Vec::new(),
            scales: Vec::new(),
            slots: HashMap::new(),
            live: 0,
        };
        vectors.refresh();
        (vectors.read_to > 0).then_some(vectors)
    }

    /// Takes in what was added since; everything again when the file was rewritten.
    pub fn refresh(&mut self) {
        let Ok(mut file) = File::open(&self.file) else { return };
        let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let Some((dims, generation)) = read_header(&mut file) else { return };
        if dims != self.engine.dims() {
            return;
        }
        if generation != self.generation || length < self.read_to {
            *self = Vectors { engine: self.engine, file: self.file.clone(), generation, read_to: HEADER, map: None, at: Vec::new(), scales: Vec::new(),
                              slots: HashMap::new(), live: 0 };
        }
        if length == self.read_to {
            return;
        }
        // SAFETY: the mapped bytes aren't changed while mapped (see `map`), and every record is checked as it's read.
        let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { return };
        let mut at = self.read_to as usize;
        while let Some(record) = mapped_record(&map, at, dims) {
            let key = hashed(&key(std::str::from_utf8(&map[record.path.clone()]).unwrap_or_default()));
            if record.add {
                match self.slots.get(&key) {
                    Some(&slot) => {
                        if self.scales[slot] == 0.0 {
                            self.live += 1;
                        }
                        self.at[slot] = at as u64;
                        self.scales[slot] = record.scale;
                    }
                    None => {
                        self.slots.insert(key, self.at.len());
                        self.at.push(at as u64);
                        self.scales.push(record.scale);
                        self.live += 1;
                    }
                }
            } else if let Some(&slot) = self.slots.get(&key) {
                if self.scales[slot] != 0.0 {
                    self.live -= 1;
                }
                self.scales[slot] = 0.0;
            }
            at += record.size;
        }
        self.read_to = at as u64;
        self.map = Some(map);
    }

    fn bytes(&self) -> &[u8] {
        self.map.as_deref().unwrap_or_default()
    }

    /// One's 8-bit values, where they lie in the file.
    fn row(&self, slot: usize) -> &[i8] {
        let (bytes, at) = (self.bytes(), self.at[slot] as usize);
        let length = u16::from_le_bytes([bytes[at + 1], bytes[at + 2]]) as usize;
        let start = at + 3 + length + 12;
        let row = &bytes[start..start + self.engine.dims()];
        // SAFETY: i8 and u8 have the same size and alignment, and every bit pattern is valid for both.
        unsafe { std::slice::from_raw_parts(row.as_ptr().cast::<i8>(), row.len()) }
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
        let count = self.at.len();
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
                        let dot: f32 = self.row(slot).iter().zip(query).map(|(&value, &weight)| f32::from(value) * weight).sum();
                        *score = dot * scale;
                    }
                });
            }
        });
        out
    }

    pub fn path(&self, slot: usize) -> &str {
        let (bytes, at) = (self.bytes(), self.at[slot] as usize);
        let length = u16::from_le_bytes([bytes[at + 1], bytes[at + 2]]) as usize;
        std::str::from_utf8(&bytes[at + 3..at + 3 + length]).unwrap_or_default()
    }

    fn paths(&self) -> impl Iterator<Item = &str> {
        (0..self.at.len()).map(|slot| self.path(slot))
    }

    /// Each image (or video frame) here now and its vector as stored (unit length, from its 8-bit values): for
    /// comparing two stores (`examples/clips.rs`).
    pub fn stored(&self) -> impl Iterator<Item = (&str, Vec<f32>)> {
        self.paths().enumerate().filter(|(slot, _)| self.scales[*slot] != 0.0).map(move |(slot, path)| {
            let scale = self.scales[slot];
            (path, self.row(slot).iter().map(|&value| f32::from(value) * scale).collect())
        })
    }

    /// How alike two images' vectors are (-1 to 1), when both are here.
    pub fn alike(&self, one: &str, other: &str) -> Option<f32> {
        self.alike_slots(*self.slots.get(&hashed(&key(one)))?, *self.slots.get(&hashed(&key(other)))?)
    }

    /// The same, by where each is stored.
    fn alike_slots(&self, a: usize, b: usize) -> Option<f32> {
        let (scale_a, scale_b) = (self.scales[a], self.scales[b]);
        if scale_a == 0.0 || scale_b == 0.0 {
            return None;
        }
        let rows = self.row(a).iter().zip(self.row(b));
        Some(rows.map(|(&x, &y)| i32::from(x) * i32::from(y)).sum::<i32>() as f32 * scale_a * scale_b)
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
        for (slot, path) in self.paths().enumerate() {
            let scale = self.scales[slot];
            let Some((video, _)) = path.split_once("#t=") else { continue };
            if scale == 0.0 {
                continue;
            }
            let sum = sums.entry(key(video)).or_insert_with(|| vec![0.0; dims]);
            for (total, &value) in sum.iter_mut().zip(self.row(slot)) {
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
    rank_where(results, bonuses, limit, |_| true)
}

/// The same, keeping only those `keep` wants. Only those near the top have their paths spelled out.
pub fn rank_where(results: &[(&Vectors, Vec<f32>)], bonuses: &[HashMap<String, f32>], limit: usize, mut keep: impl FnMut(&str) -> bool) -> Vec<Hit> {
    let with_bonuses = bonuses.iter().any(|bonus| !bonus.is_empty());
    // A video once, by its best moment.
    let mut seen = std::collections::HashSet::new();
    let mut ranked: Vec<(f32, f32, usize, usize)> = Vec::new();
    for (mut score, similarity, result, slot) in standings(results) {
        let path = results[result].0.path(slot);
        let (file, moment) = path.split_once("#t=").map_or((path, false), |(file, _)| (file, true));
        if !seen.insert(hashed_lower(file)) {
            continue;
        }
        if moment && with_bonuses {
            let video = key(file);
            let added: Vec<f32> = bonuses.iter().filter_map(|bonus| bonus.get(&video).copied()).collect();
            if !added.is_empty() {
                score += added.iter().sum::<f32>() / added.len() as f32;
            }
        }
        ranked.push((score, similarity, result, slot));
    }
    if with_bonuses {
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    }
    // Enough for the copies to be set aside among them (`copies_last`).
    let enough = limit.saturating_mul(4).max(2000);
    let hits: Vec<Hit> = ranked.into_iter().filter_map(|(score, similarity, result, slot)| {
        let path = results[result].0.path(slot);
        keep(path).then(|| Hit { path: path.to_string(), score, similarity })
    }).take(enough).collect();
    let mut hits = copies_last(hits, results.first().map(|(vectors, _)| *vectors), limit);
    hits.truncate(limit);
    hits
}

/// Two images whose vectors are at least this alike are the same picture: the same file in two folders.
/// Measured on COCO val2017: no two of its 5,000 photos are past 0.983 with SigLIP 2 base or 0.986 with EmbeddingGemma 2
/// (two frames of one scene), and 12 photos copied to a second folder, looked at on the graphics card, came out 1.000.
pub const SAME_PICTURE: f32 = 0.995;

/// The same picture in several folders (one vector, the same score, side by side) would fill the list: the first copy
/// stays where it is, the others go after every other image. Looked for among the best `limit` (at most 1,000).
fn copies_last(hits: Vec<Hit>, vectors: Option<&Vectors>, limit: usize) -> Vec<Hit> {
    let Some(vectors) = vectors else { return hits };
    let limit = limit.min(1000);
    let (mut kept, mut copies): (Vec<(Hit, Option<usize>)>, Vec<Hit>) = (Vec::new(), Vec::new());
    let mut rest = hits.into_iter();
    for hit in rest.by_ref() {
        // Where its vector is (none for a video's moment: they're kept once already).
        let slot = if hit.path.contains("#t=") { None } else { vectors.slots.get(&hashed(&key(&hit.path))).copied() };
        // Copies score alike: only those within a hair of it are compared.
        let copy = slot.is_some_and(|slot| kept.iter().rev().take(50).take_while(|(other, _)| other.score - hit.score < 0.05).any(|(_, other)| {
            other.and_then(|other| vectors.alike_slots(other, slot)).is_some_and(|alike| alike >= SAME_PICTURE)
        }));
        if copy { copies.push(hit) } else { kept.push((hit, slot)) }
        if kept.len() >= limit {
            break;
        }
    }
    let mut kept: Vec<Hit> = kept.into_iter().map(|(hit, _)| hit).collect();
    kept.extend(copies);
    kept.extend(rest);
    kept
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
    best_where(results, limit, |_| true)
}

/// The same, keeping only those `keep` wants, in order until there are `limit`: only those have their paths spelled
/// out (every one of a quarter of a million documents' pieces, twice, held 95 MB for each search).
pub fn best_where(results: &[(&Vectors, Vec<f32>)], limit: usize, mut keep: impl FnMut(&str) -> bool) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (score, similarity, result, slot) in standings(results) {
        if hits.len() >= limit {
            break;
        }
        let path = results[result].0.path(slot);
        if keep(path) {
            hits.push(Hit { path: path.to_string(), score, similarity });
        }
    }
    hits
}

/// Each path's standing, best first: (standing, similarity, which result it's from, where in it), its paths not
/// spelled out. A path in several results (one per model) is one, by its lowercase path's number.
fn standings(results: &[(&Vectors, Vec<f32>)]) -> Vec<(f32, f32, usize, usize)> {
    // (sum of standings, how many, similarity, first result and slot)
    let mut by_path: HashMap<u64, (f32, usize, f32, usize, usize)> = HashMap::new();
    let mut alone: Vec<(f32, usize, f32, usize, usize)> = Vec::new();
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
            let standing = (score - mean) / deviation;
            // One model's paths are each there once: nothing to bring together.
            if results.len() == 1 {
                alone.push((standing, 1, score, index, slot));
                continue;
            }
            let entry = by_path.entry(hashed_lower(vectors.path(slot))).or_insert((0.0, 0, f32::NAN, index, slot));
            entry.0 += standing;
            entry.1 += 1;
            if index == 0 || entry.2.is_nan() {
                entry.2 = score;
            }
        }
    }
    let mut standings: Vec<(f32, f32, usize, usize)> = alone.into_iter().chain(by_path.into_values())
        .map(|(sum, count, similarity, result, slot)| (sum / count as f32, similarity, result, slot)).collect();
    standings.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| results[a.2].0.path(a.3).cmp(results[b.2].0.path(b.3))));
    standings
}

/// `hashed(&key(path))`, without making the lowercase path.
fn hashed_lower(path: &str) -> u64 {
    let mut buffer = [0u8; 4];
    path.chars().flat_map(char::to_lowercase).fold(0xcbf2_9ce4_8422_2325u64, |hash, letter| {
        letter.encode_utf8(&mut buffer).bytes().fold(hash, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
    })
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

        // Opened again, it knows what it has; a record cut short is dropped (once the helper lets go of the file).
        drop((writer, vectors));
        let mut file = std::fs::OpenOptions::new().append(true).open(super::file(&folder, Engine::Gemma)).unwrap();
        file.write_all(&[ADD, 9, 0, b'C']).unwrap();
        drop(file);
        let writer = Writer::open(&folder, Engine::Gemma).unwrap();
        assert_eq!(writer.len(), 3);
        assert!(writer.has(r"C:\Pictures\pie.jpg", 9));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn copies_after_the_rest() {
        let folder = folder("copies");
        let mut writer = Writer::open(&folder, Engine::Siglip).unwrap();
        for index in 0..30 {
            writer.add(&format!(r"C:\x\{index}.png"), 1, &vector(index)).unwrap();
        }
        // The same picture in two more folders.
        writer.add(r"C:\Backup\2.png", 1, &vector(2)).unwrap();
        writer.add(r"C:\Downloads\2 (1).png", 1, &vector(2)).unwrap();
        let vectors = Vectors::load(&folder, Engine::Siglip).unwrap();
        let query = normalized(&vector(2));
        let hits = rank(&[(&vectors, vectors.scores(&query))], &[], 10);
        assert_eq!(hits.len(), 10);
        assert_eq!(hits.iter().filter(|hit| hit.path.ends_with(r"\2.png") || hit.path.ends_with("2 (1).png")).count(), 1, "{hits:?}");
        assert!(vectors.alike(r"C:\x\2.png", r"c:\backup\2.png").unwrap() > 0.999);
        // Asked for them all, the copies come last.
        let all = rank(&[(&vectors, vectors.scores(&query))], &[], 100);
        assert_eq!(all.len(), 32);
        assert!(all[30..].iter().all(|hit| hit.score == all[0].score) && all[1].score < all[0].score, "{:?}", &all[29..]);
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
        // Some gone, and one of those back again.
        for index in 0..50 {
            writer.remove(&format!(r"C:\x\{index}.png")).unwrap();
        }
        writer.add(r"C:\x\7.png", 9, &vector(700)).unwrap();
        for round in 5..8 {
            for index in 50..400 {
                writer.add(&format!(r"C:\x\{index}.png"), round, &vector(index + round as usize)).unwrap();
            }
        }
        // 3,101 records written for 351 images: rewritten on the way (past twice the images and 1,000 more), the
        // latest of each kept.
        assert!(writer.records < 2 * 351 + 1000, "{} records", writer.records);
        assert!(writer.has(r"C:\x\7.png", 9) && writer.has(r"C:\x\399.png", 7) && !writer.has(r"C:\x\8.png", 4));
        let vectors = Vectors::load(&folder, Engine::Siglip).unwrap();
        assert_eq!(vectors.len(), 351);
        // Each with its latest vector (the test's vectors repeat every 97 seeds, so it's each one's own score that
        // tells, not which comes first).
        for (path, seed) in [(r"C:\x\7.png", 700), (r"C:\x\399.png", 399 + 7), (r"C:\x\60.png", 60 + 7)] {
            let query = normalized(&vector(seed));
            let hits = combine(&[(&vectors, vectors.scores(&query))], 400);
            let hit = hits.iter().find(|hit| hit.path == path).unwrap();
            assert!((hit.similarity - 1.0).abs() < 0.01, "{path}: {}", hit.similarity);
        }
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
