//! The content index: the words inside files (documents, code, text in pictures), in tantivy (BM25 ranking,
//! phrases, prefixes; memory-mapped). The indexer writes it; the popup reads it directly, read-only, so a search
//! never waits on the indexer.

pub mod snippet;
pub mod tokenizer;

use std::path::Path;
use std::time::{Duration, Instant};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query as TantivyQuery, TermQuery, TermSetQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value,
};
use tantivy::store::{Compressor, ZstdCompressor};
use tantivy::{Index, IndexReader, IndexSettings, IndexWriter, ReloadPolicy, Term};

pub use snippet::Snippet;

/// Bumped when the schema or the tokenizer changes: an index of another version is rebuilt.
pub const VERSION: u32 = 1;

/// Words indexed per file at most (a 2 MB text is a long book).
pub const BODY_LIMIT: usize = 2 << 20;
/// Text kept per file for snippets and previews (compressed on disk).
pub const STORED_LIMIT: usize = 256 << 10;
/// For an exact search, how many files are looked at for each one wanted.
const EXACT_CONSIDERED: usize = 5;

/// What a file is, for filters and labels.
pub mod kind {
    pub const TEXT: &str = "text";
    pub const CODE: &str = "code";
    pub const PDF: &str = "pdf";
    pub const WORD: &str = "word";
    pub const SHEET: &str = "sheet";
    pub const SLIDES: &str = "slides";
    pub const EBOOK: &str = "ebook";
    pub const EMAIL: &str = "email";
    pub const WEB: &str = "web";
    pub const PICTURE: &str = "picture";
    /// A recording, by what's said in it.
    pub const AUDIO: &str = "audio";
    /// A video, by the text it shows.
    pub const VIDEO: &str = "video";

    pub const DOCUMENTS: &[&str] = &[TEXT, PDF, WORD, SHEET, SLIDES, EBOOK, EMAIL, WEB];
}

/// Where a file's words came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Source {
    /// Its own text (a document's text layer, code, an email).
    #[default]
    Text = 0,
    /// Read from a picture (OCR).
    Picture = 1,
    /// Both: a PDF with scanned pages.
    Mixed = 2,
    /// Heard: what's said in a recording.
    Speech = 3,
    /// Shown: the text a video shows, each line at the moment it first appears (`marks` as Speech's).
    Video = 4,
}

impl Source {
    fn from_u64(value: u64) -> Source {
        match value {
            1 => Source::Picture,
            2 => Source::Mixed,
            3 => Source::Speech,
            4 => Source::Video,
            _ => Source::Text,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Fields {
    /// The lowercased path: the key to replace or remove a file.
    pub id: Field,
    pub path: Field,
    pub name: Field,
    /// Everything read from the file, indexed (not stored).
    pub body: Field,
    /// The first STORED_LIMIT of it, stored for snippets and previews.
    pub text: Field,
    pub kind: Field,
    pub modified: Field,
    pub size: Field,
    pub source: Field,
    /// Privacy marks (secrets found, and where they are in a picture), JSON.
    pub marks: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let words = TextFieldIndexing::default().set_tokenizer(tokenizer::NAME).set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let id = builder.add_text_field("id", STRING);
    let path = builder.add_text_field("path", STORED);
    let name = builder.add_text_field("name", TextOptions::default().set_indexing_options(words.clone()).set_stored());
    let body = builder.add_text_field("body", TextOptions::default().set_indexing_options(words));
    let text = builder.add_text_field("text", STORED);
    let kind = builder.add_text_field("kind", STRING | STORED | FAST);
    let modified = builder.add_i64_field("modified", STORED | FAST);
    let size = builder.add_u64_field("size", STORED);
    let source = builder.add_u64_field("source", STORED | FAST | INDEXED);
    let marks = builder.add_bytes_field("marks", STORED);
    (builder.build(), Fields { id, path, name, body, text, kind, modified, size, source, marks })
}

fn register(index: &Index) {
    index.tokenizers().register(tokenizer::NAME, tokenizer::analyzer());
}

fn version_file(dir: &Path) -> std::path::PathBuf {
    dir.join("excavate-version.txt")
}

fn current_version(dir: &Path) -> Option<u32> {
    std::fs::read_to_string(version_file(dir)).ok()?.trim().parse().ok()
}

/// What's said in a video is kept beside what it shows, as an entry of its own: its id is the path's and this.
const SAID: &str = "\u{1}said";

/// The id of a file's entry in the index (`Writer::remove_entry`): what it shows or reads, or (`said`) what's said in a
/// video.
pub fn entry_id(path: &str, said: bool) -> String {
    let id = path.to_lowercase();
    if said { id + SAID } else { id }
}

/// A file as the indexer hands it over.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub path: String,
    pub name: String,
    pub kind: &'static str,
    pub modified: i64,
    pub size: u64,
    pub source: Source,
    pub body: String,
    pub marks: Vec<u8>,
}

pub struct Writer {
    index: Index,
    writer: IndexWriter,
    fields: Fields,
    pub uncommitted: usize,
}

impl Writer {
    /// Opens the index for writing, creating it (or recreating it, when it's from another version). The flag says
    /// it was created empty, so everything must be read again.
    pub fn open(dir: &Path, memory_budget: usize) -> Result<(Writer, bool), String> {
        let (schema, fields) = schema();
        let fresh = current_version(dir) != Some(VERSION) || !dir.join("meta.json").exists();
        if fresh {
            let _ = std::fs::remove_dir_all(dir);
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let index = if fresh {
            let settings = IndexSettings { docstore_compression: Compressor::Zstd(ZstdCompressor::default()), ..IndexSettings::default() };
            let index = Index::builder().schema(schema).settings(settings).create_in_dir(dir).map_err(|error| error.to_string())?;
            std::fs::write(version_file(dir), VERSION.to_string()).map_err(|error| error.to_string())?;
            index
        } else {
            Index::open_in_dir(dir).map_err(|error| error.to_string())?
        };
        register(&index);
        let writer = index.writer_with_num_threads(1, memory_budget.max(15_000_000)).map_err(|error| error.to_string())?;
        Ok((Writer { index, writer, fields, uncommitted: 0 }, fresh))
    }

    /// Adds a file, replacing what was there for its path (a video's speech: what was said in it).
    pub fn put(&mut self, doc: &Doc) -> Result<(), String> {
        let fields = self.fields;
        let id = entry_id(&doc.path, doc.source == Source::Speech && doc.kind == kind::VIDEO);
        self.writer.delete_term(Term::from_field_text(fields.id, &id));
        let mut document = TantivyDocument::default();
        document.add_text(fields.id, &id);
        document.add_text(fields.path, &doc.path);
        document.add_text(fields.name, &doc.name);
        document.add_text(fields.body, cut(&doc.body, BODY_LIMIT));
        document.add_text(fields.text, cut(&doc.body, STORED_LIMIT));
        document.add_text(fields.kind, doc.kind);
        document.add_i64(fields.modified, doc.modified);
        document.add_u64(fields.size, doc.size);
        document.add_u64(fields.source, doc.source as u64);
        if !doc.marks.is_empty() {
            document.add_bytes(fields.marks, &doc.marks);
        }
        self.writer.add_document(document).map_err(|error| error.to_string())?;
        self.uncommitted += 1;
        Ok(())
    }

    /// Takes a file out, all of it (a video: what it shows and what's said in it).
    pub fn remove(&mut self, path: &str) {
        self.remove_entry(&entry_id(path, false));
        self.remove_entry(&entry_id(path, true));
    }

    /// Takes one entry out (`entry_id`).
    pub fn remove_entry(&mut self, id: &str) {
        self.writer.delete_term(Term::from_field_text(self.fields.id, id));
        self.uncommitted += 1;
    }

    /// Makes what was added visible to readers (the popup).
    pub fn commit(&mut self) -> Result<(), String> {
        self.writer.commit().map_err(|error| error.to_string())?;
        self.uncommitted = 0;
        Ok(())
    }

    pub fn documents(&self) -> u64 {
        self.index.reader().map(|reader| reader.searcher().num_docs()).unwrap_or(0)
    }

    /// Ends the writer, waiting for its merges.
    pub fn close(self) {
        let _ = self.writer.wait_merging_threads();
    }
}

/// When the words at byte `at` of a recording's text are said, in seconds: its marks keep where each piece starts and
/// its moment (`{"times": [[byte, ms], ...]}`, in order).
pub fn moment_of(marks: &[u8], at: usize) -> Option<f64> {
    let marks: serde_json::Value = serde_json::from_slice(marks).ok()?;
    let times = marks["times"].as_array()?;
    let mut moment = None;
    for time in times {
        let (byte, ms) = (time[0].as_u64()? as usize, time[1].as_i64()?);
        if byte > at {
            break;
        }
        moment = Some(ms as f64 / 1000.0);
    }
    moment
}

/// At most `limit` bytes of `text`, cut at a character boundary.
fn cut(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// What was typed, for the content index.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub words: Vec<String>,
    /// The last word may be unfinished: match words starting with it.
    pub prefix: bool,
    /// Only these kinds (empty: all).
    pub kinds: Vec<&'static str>,
    /// Exactly as typed. The index's words are lowercase, so each hit's stored text is checked.
    pub case: bool,
    /// Whole words only, not unfinished, not one letter off, not the part of a joined word. Checked the same way.
    pub whole_word: bool,
    /// Only what's inside: a file's name doesn't count.
    pub inside_only: bool,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub modified: i64,
    pub size: u64,
    pub source: Source,
    /// BM25.
    pub score: f32,
    /// Matched in the file's name only (no words inside matched).
    pub in_name_only: bool,
    pub snippet: Snippet,
    /// A recording: when what matched is said, in seconds.
    pub moment: Option<f64>,
}

/// What the index keeps of a file, for its preview.
#[derive(Clone, Debug, Default)]
pub struct Stored {
    pub text: String,
    pub kind: String,
    pub source: Option<Source>,
    pub marks: Vec<u8>,
}

/// The popup's view of the index: read-only, memory-mapped, refreshed when the indexer has committed.
pub struct Reader {
    reader: IndexReader,
    fields: Fields,
    refreshed: Instant,
}

impl Reader {
    pub fn open(dir: &Path) -> Result<Reader, String> {
        if current_version(dir) != Some(VERSION) {
            return Err("no index yet".into());
        }
        let index = Index::open_in_dir(dir).map_err(|error| error.to_string())?;
        register(&index);
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().map_err(|error| error.to_string())?;
        let (_, fields) = schema();
        Ok(Reader { reader, fields, refreshed: Instant::now() })
    }

    /// Picks up what the indexer committed since (cheap when nothing changed). At most once a second.
    pub fn refresh(&mut self) {
        if self.refreshed.elapsed() >= Duration::from_secs(1) {
            let _ = self.reader.reload();
            self.refreshed = Instant::now();
        }
    }

    pub fn documents(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// The best `limit` files for the words, with a snippet each.
    pub fn search(&self, query: &Query, limit: usize) -> Vec<Hit> {
        let Some(tantivy_query) = self.build(query) else { return Vec::new() };
        let searcher = self.reader.searcher();
        // Exact matches are only known once the stored text has been looked at: more are looked at, until there are enough.
        let exact = query.case || query.whole_word;
        let considered = if exact { limit * EXACT_CONSIDERED } else { limit };
        let Ok(top) = searcher.search(&tantivy_query, &TopDocs::with_limit(considered).order_by_score()) else { return Vec::new() };
        let words = snippet::Words::new(&query.words, query.prefix).exactly(query.case, query.whole_word);
        // A video can match twice, by what it shows and by what's said: the better one.
        let mut found = std::collections::HashSet::new();
        top.into_iter()
            .filter_map(|(score, address)| {
                let document: TantivyDocument = searcher.doc(address).ok()?;
                let text = |field: Field| document.get_first(field).and_then(|value| value.as_str()).unwrap_or("").to_string();
                let stored = text(self.fields.text);
                let snippet = snippet::make(&stored, &words, 160);
                let inside = !snippet.highlights.is_empty();
                if !inside && (query.inside_only || (exact && snippet::make(&text(self.fields.name), &words, 160).highlights.is_empty())) {
                    return None;
                }
                if !found.insert(text(self.fields.path).to_lowercase()) {
                    return None;
                }
                let source = Source::from_u64(document.get_first(self.fields.source).and_then(|value| value.as_u64()).unwrap_or(0));
                let moment = match (source, snippet.at) {
                    (Source::Speech | Source::Video, Some(at)) => document.get_first(self.fields.marks).and_then(|value| value.as_bytes()).and_then(|marks| moment_of(marks, at)),
                    _ => None,
                };
                Some(Hit {
                    path: text(self.fields.path),
                    name: text(self.fields.name),
                    kind: text(self.fields.kind),
                    modified: document.get_first(self.fields.modified).and_then(|value| value.as_i64()).unwrap_or(0),
                    size: document.get_first(self.fields.size).and_then(|value| value.as_u64()).unwrap_or(0),
                    source,
                    score,
                    in_name_only: !inside,
                    snippet,
                    moment,
                })
            })
            .take(limit)
            .collect()
    }

    /// What the index keeps of one file (a video: what it shows, or else what's said in it).
    pub fn stored(&self, path: &str) -> Option<Stored> {
        self.stored_entry(&entry_id(path, false)).or_else(|| self.stored_entry(&entry_id(path, true)))
    }

    fn stored_entry(&self, id: &str) -> Option<Stored> {
        let searcher = self.reader.searcher();
        let term = Term::from_field_text(self.fields.id, id);
        let top = searcher.search(&TermQuery::new(term, IndexRecordOption::Basic), &TopDocs::with_limit(1).order_by_score()).ok()?;
        let (_, address) = top.into_iter().next()?;
        let document: TantivyDocument = searcher.doc(address).ok()?;
        Some(Stored {
            text: document.get_first(self.fields.text).and_then(|value| value.as_str()).unwrap_or("").to_string(),
            kind: document.get_first(self.fields.kind).and_then(|value| value.as_str()).unwrap_or("").to_string(),
            source: document.get_first(self.fields.source).and_then(|value| value.as_u64()).map(Source::from_u64),
            marks: document.get_first(self.fields.marks).and_then(|value| value.as_bytes()).map(<[u8]>::to_vec).unwrap_or_default(),
        })
    }

    fn build(&self, query: &Query) -> Option<Box<dyn TantivyQuery>> {
        let fields = self.fields;
        let mut musts: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();
        let last = query.words.len().saturating_sub(1);
        for (index, word) in query.words.iter().enumerate() {
            let wholes = tokenizer::analyze(word);
            if wholes.is_empty() {
                continue;
            }
            let prefix = query.prefix && index == last;
            let mut alternatives: Vec<(Occur, Box<dyn TantivyQuery>)> = Vec::new();
            let searched: &[Field] = if query.inside_only { &[fields.body] } else { &[fields.body, fields.name] };
            for &field in searched {
                // The name counts, but less: names are what Everything is for, and a file named after the word
                // already ranks there.
                let weight = if field == fields.name { 0.5 } else { 1.0 };
                for alternative in word_queries(field, &wholes, prefix, query.case || query.whole_word) {
                    alternatives.push((Occur::Should, Box::new(BoostQuery::new(alternative, weight))));
                }
            }
            if alternatives.is_empty() {
                continue;
            }
            musts.push((Occur::Must, Box::new(BooleanQuery::new(alternatives))));
        }
        if musts.is_empty() {
            return None;
        }
        if !query.kinds.is_empty() {
            let terms = query.kinds.iter().map(|kind| Term::from_field_text(fields.kind, kind));
            musts.push((Occur::Must, Box::new(TermSetQuery::new(terms))));
        }
        Some(Box::new(BooleanQuery::new(musts)))
    }
}

/// The ways one typed word can match a field. `exact`: not one letter off.
fn word_queries(field: Field, wholes: &[(String, Vec<String>)], prefix: bool, exact: bool) -> Vec<Box<dyn TantivyQuery>> {
    let mut queries: Vec<Box<dyn TantivyQuery>> = Vec::new();
    let term = |text: &str| Term::from_field_text(field, text);
    if wholes.len() > 1 {
        // "e-mail", "東京都": the words in a row.
        queries.push(Box::new(PhraseQuery::new(wholes.iter().map(|(whole, _)| term(whole)).collect())));
        return queries;
    }
    let (whole, parts) = &wholes[0];
    let length = whole.chars().count();
    queries.push(Box::new(TermQuery::new(term(whole), IndexRecordOption::WithFreqs)));
    if parts.len() > 1 {
        // getUserName also finds get_user_name.
        queries.push(Box::new(PhraseQuery::new(parts.iter().map(|part| term(part)).collect())));
    }
    if prefix && length >= 2 {
        queries.push(Box::new(BoostQuery::new(Box::new(FuzzyTermQuery::new_prefix(term(whole), 0, true)), 0.8)));
    } else if !exact && length >= 5 && whole.chars().all(char::is_alphabetic) {
        // One letter off: text read from pictures has the odd misread letter ("rn" for "m").
        queries.push(Box::new(BoostQuery::new(Box::new(FuzzyTermQuery::new(term(whole), 1, true)), 0.3)));
    }
    queries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_with(docs: &[(&str, &str, &str)]) -> (tempdir::Dir, Reader) {
        let dir = tempdir::Dir::new();
        let (mut writer, fresh) = Writer::open(&dir.0, 20_000_000).unwrap();
        assert!(fresh);
        for (path, kind, body) in docs {
            let name = path.rsplit('\\').next().unwrap().to_string();
            writer.put(&Doc { path: path.to_string(), name, kind: Box::leak(kind.to_string().into_boxed_str()), modified: 1,
                              size: body.len() as u64, body: body.to_string(), ..Doc::default() }).unwrap();
        }
        writer.commit().unwrap();
        writer.close();
        let reader = Reader::open(&dir.0).unwrap();
        (dir, reader)
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new() -> Dir {
                let dir = std::env::temp_dir().join(format!("excavate-test-{}-{:?}", std::process::id(), std::thread::current().id()));
                let _ = std::fs::remove_dir_all(&dir);
                Dir(dir)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn paths(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.path.as_str()).collect()
    }

    #[test]
    fn moments_in_a_recording() {
        let marks = br#"{"times": [[0, 0], [13, 2400], [40, 65000]]}"#;
        assert_eq!(moment_of(marks, 0), Some(0.0));
        assert_eq!(moment_of(marks, 20), Some(2.4));
        assert_eq!(moment_of(marks, 900), Some(65.0));
        assert_eq!(moment_of(b"{}", 5), None);
    }

    #[test]
    fn a_video_shows_and_says() {
        let dir = tempdir::Dir::new();
        let (mut writer, _) = Writer::open(&dir.0, 20_000_000).unwrap();
        let video = |source, body: &str| Doc {
            path: r"C:\v\Talk.mp4".into(), name: "Talk.mp4".into(), kind: kind::VIDEO, modified: 1, size: 9, source,
            body: body.into(), marks: br#"{"times": [[0, 61000]]}"#.to_vec(),
        };
        writer.put(&video(Source::Video, "Quarterly budget review")).unwrap();
        writer.put(&video(Source::Speech, "the budget is due on Friday")).unwrap();
        writer.commit().unwrap();
        // Both kept, neither in place of the other; a search finds the video once, by the better of them.
        let reader = Reader::open(&dir.0).unwrap();
        assert_eq!(reader.documents(), 2);
        let budget = reader.search(&Query { words: vec!["budget".into()], ..Query::default() }, 10);
        assert_eq!(paths(&budget), [r"C:\v\Talk.mp4"]);
        let friday = reader.search(&Query { words: vec!["friday".into()], ..Query::default() }, 10);
        assert_eq!((friday[0].source, friday[0].moment), (Source::Speech, Some(61.0)));
        assert_eq!(reader.stored(r"C:\v\Talk.mp4").unwrap().source, Some(Source::Video));
        // What's said, out on its own; then the whole video.
        writer.remove_entry(&entry_id(r"C:\v\Talk.mp4", true));
        writer.commit().unwrap();
        assert_eq!(Reader::open(&dir.0).unwrap().documents(), 1);
        writer.put(&video(Source::Speech, "the budget is due on Friday")).unwrap();
        writer.remove(r"C:\v\talk.mp4");
        writer.commit().unwrap();
        writer.close();
        assert_eq!(Reader::open(&dir.0).unwrap().documents(), 0);
    }

    #[test]
    fn finds_words_inside() {
        let (_dir, reader) = index_with(&[
            (r"C:\a\invoice.txt", kind::TEXT, "Invoice 2291 for the kitchen renovation, due March 3"),
            (r"C:\a\notes.md", kind::TEXT, "Shopping list: milk, eggs"),
            (r"C:\a\shot.png", kind::PICTURE, "Wi-Fi password: hunter2 network Excavate"),
        ]);
        let query = |text: &str, prefix: bool| Query { words: text.split(' ').map(String::from).collect(), prefix, ..Query::default() };
        assert_eq!(paths(&reader.search(&query("kitchen renovation", false), 10)), [r"C:\a\invoice.txt"]);
        // An unfinished last word.
        assert_eq!(paths(&reader.search(&query("renov", true), 10)), [r"C:\a\invoice.txt"]);
        // Punctuation inside a word: a phrase.
        assert_eq!(paths(&reader.search(&query("wi-fi", false), 10)), [r"C:\a\shot.png"]);
        // One letter misread.
        assert_eq!(paths(&reader.search(&query("passwerd", false), 10)), [r"C:\a\shot.png"]);
        // Kinds.
        let pictures = Query { kinds: vec![kind::PICTURE], ..query("password", false) };
        assert_eq!(paths(&reader.search(&pictures, 10)), [r"C:\a\shot.png"]);
        let documents = Query { kinds: kind::DOCUMENTS.to_vec(), ..query("password", false) };
        assert!(reader.search(&documents, 10).is_empty());
        // The snippet marks the match.
        let hit = &reader.search(&query("kitchen", false), 10)[0];
        let marked: Vec<&str> = hit.snippet.highlights.iter().map(|range| &hit.snippet.text[range.clone()]).collect();
        assert_eq!(marked, ["kitchen"]);
    }

    #[test]
    fn exact_searches() {
        let (_dir, reader) = index_with(&[
            (r"C:\a\upper.txt", kind::TEXT, "The Budget for March"),
            (r"C:\a\lower.txt", kind::TEXT, "the budget for march"),
            (r"C:\a\joined.txt", kind::TEXT, "see budgetPlan and getBudgetTotal"),
            (r"C:\a\Budget.txt", kind::TEXT, "nothing about it inside"),
            (r"C:\a\shot.png", kind::PICTURE, "Budget in an image"),
            (r"C:\a\budget shot.png", kind::PICTURE, "nothing about it inside"),
        ]);
        let words = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
        let found = |query: Query| {
            let mut paths: Vec<String> = reader.search(&query, 10).into_iter().map(|hit| hit.path).collect();
            paths.sort();
            paths
        };

        // Case: inside or in the name, exactly.
        let case = Query { words: words("Budget"), case: true, ..Query::default() };
        assert_eq!(found(case.clone()), [r"C:\a\Budget.txt", r"C:\a\joined.txt", r"C:\a\shot.png", r"C:\a\upper.txt"]);
        let hit = reader.search(&case, 10).into_iter().find(|hit| hit.path.ends_with("Budget.txt")).unwrap();
        assert!(hit.in_name_only);

        // Whole words: not the parts of joined words, and not unfinished ones.
        let whole = Query { words: words("budget"), whole_word: true, ..Query::default() };
        assert!(!found(whole).iter().any(|path| path.ends_with("joined.txt")));
        let unfinished = Query { words: words("budg"), whole_word: true, ..Query::default() };
        assert!(found(unfinished).is_empty());
        // Not one letter off.
        let misread = Query { words: words("budgat"), whole_word: true, ..Query::default() };
        assert!(found(misread).is_empty());

        // Only what's inside images: a name doesn't count.
        let images = Query { words: words("budget"), kinds: vec![kind::PICTURE], inside_only: true, ..Query::default() };
        assert_eq!(found(images), [r"C:\a\shot.png"]);
    }

    #[test]
    fn replaces_and_removes() {
        let dir = tempdir::Dir::new();
        let (mut writer, _) = Writer::open(&dir.0, 20_000_000).unwrap();
        let doc = Doc { path: r"C:\x\a.txt".into(), name: "a.txt".into(), kind: kind::TEXT, body: "alpha".into(), ..Doc::default() };
        writer.put(&doc).unwrap();
        writer.put(&Doc { body: "beta".into(), ..doc.clone() }).unwrap();
        writer.commit().unwrap();
        let reader = Reader::open(&dir.0).unwrap();
        let query = |word: &str| Query { words: vec![word.into()], ..Query::default() };
        assert!(reader.search(&query("alpha"), 10).is_empty());
        assert_eq!(reader.search(&query("beta"), 10).len(), 1);
        writer.remove(r"C:\X\A.TXT");
        writer.commit().unwrap();
        writer.close();
        let mut reader = reader;
        reader.refreshed = Instant::now() - Duration::from_secs(5);
        reader.refresh();
        assert!(reader.search(&query("beta"), 10).is_empty());
        // Reopening keeps the index.
        let (_, fresh) = Writer::open(&dir.0, 20_000_000).unwrap();
        assert!(!fresh);
    }
}
