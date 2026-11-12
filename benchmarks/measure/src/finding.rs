//! `measure finding`: how often the right file comes first, and in the first ten, when it's searched for as a person
//! would, scored against each dataset's own ground truth. It searches a home that `measure reading --keep` made: text
//! through the content index the popup searches (`content::Reader`, with the popup's own query), images and videos
//! through the search helper the popup asks (`excavate-helper.exe --seek`).
//!
//! - documents: four words in a row from the file's own text (the text Excavate read: GovDocs has no other);
//! - image-text: the longest word TextVQA's reference reading found in the image; also, how many of those words
//!   Excavate read (recall);
//! - speech, speech-fleurs: four words in a row from the reference transcript; also the word error rate of what was heard;
//! - image-shown (COCO), video-shown (MSR-VTT): the first of the reference descriptions.

use crate::process::Job;
use crate::stats::{self, Rng, rate, summary};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Options {
    pub home: PathBuf,
    pub excavate: PathBuf,
    pub queries: usize,
    pub seed: u64,
}

struct Truth {
    /// What to search for, per kind (see above).
    query: Option<String>,
    /// The reference transcript (speech) or reading's words (image-text).
    reference: Option<Vec<String>>,
    language: String,
}

/// Lowercase words of letters and digits, any script.
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'').map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty()).collect()
}

/// Edits (substitutions, insertions, deletions) between two word lists.
pub fn edits(reference: &[String], heard: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=heard.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, h) in heard.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (previous + usize::from(r != h)).min(row[j] + 1).min(here + 1);
            previous = here;
        }
    }
    row[heard.len()]
}

/// Four words in a row, at a seeded place, from words of three letters or more.
fn phrase(words: &[String], rng: &mut Rng) -> Option<String> {
    let usable: Vec<&String> = words.iter().filter(|w| w.chars().count() >= 3).collect();
    if usable.len() < 8 {
        return None;
    }
    let at = rng.below(usable.len() - 3);
    Some(usable[at..at + 4].iter().map(|w| w.as_str()).collect::<Vec<_>>().join(" "))
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display())).lines().map(String::from).collect()
}

/// Each file's truth, by its path under the corpus.
fn truths(kind: &str, rng: &mut Rng) -> HashMap<String, Truth> {
    let truth = crate::corpus::truth();
    let mut found = HashMap::new();
    let jsonl = |name: &str| lines(&truth.join(name)).into_iter().filter_map(|line| serde_json::from_str::<Value>(&line).ok());
    match kind {
        "image-shown" | "video-shown" => {
            for record in jsonl(if kind == "image-shown" { "coco.jsonl" } else { "msrvtt.jsonl" }) {
                let caption = record["captions"][0].as_str().map(String::from);
                found.insert(record["file"].as_str().unwrap_or("").to_string(), Truth { query: caption, reference: None, language: String::new() });
            }
        }
        "image-text" => {
            for record in jsonl("textvqa.jsonl") {
                let tokens: Vec<String> = record["ocr_tokens"].as_array().into_iter().flatten().filter_map(Value::as_str)
                    .flat_map(words).filter(|w| w.chars().count() >= 3).collect();
                let query = tokens.iter().filter(|w| w.chars().all(char::is_alphabetic) && w.chars().count() >= 4)
                    .max_by_key(|w| w.chars().count()).cloned();
                found.insert(record["file"].as_str().unwrap_or("").to_string(), Truth { query, reference: Some(tokens), language: String::new() });
            }
        }
        "speech" | "speech-fleurs" => {
            for line in lines(&truth.join(if kind == "speech" { "librispeech.tsv" } else { "fleurs.tsv" })).into_iter().skip(1) {
                let mut columns = line.splitn(3, '\t');
                let (Some(file), Some(language), Some(text)) = (columns.next(), columns.next(), columns.next()) else { continue };
                let reference = words(text);
                found.insert(file.to_string(), Truth { query: phrase(&reference, rng), reference: Some(reference), language: language.to_string() });
            }
        }
        _ => {}
    }
    found
}

fn normal(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Where the right file came among `hits` (1 is first), if it did: by its path below the corpus's part ("images/"),
/// wherever that part is.
fn rank(hits: &[String], file: &str) -> Option<usize> {
    let file = format!("/{}", normal(file.split_once('/').map_or(file, |(_, rest)| rest)));
    hits.iter().position(|hit| normal(hit).ends_with(&file)).map(|at| at + 1)
}

struct Helper {
    port: u64,
    _job: Job,
}

/// The search helper, started on the home, as the popup starts it.
fn helper(options: &Options) -> Helper {
    let said = options.home.join("local").join("seeker.json");
    let _ = std::fs::remove_file(&said);
    let job = Job::new();
    let child = std::process::Command::new(options.excavate.join("excavate-helper.exe")).arg("--seek").env("EXCAVATE_HOME", &options.home)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().expect("can't start excavate-helper.exe");
    job.add(&child);
    let started = Instant::now();
    loop {
        let port = std::fs::read(&said).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["port"].as_u64());
        if let Some(port) = port {
            return Helper { port, _job: job };
        }
        assert!(started.elapsed() < Duration::from_secs(60), "the search helper didn't start");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The helper's answer once every model it holds is ready, as the popup's last answer.
fn seek(helper: &Helper, text: &str) -> (Vec<String>, f64) {
    let body = json!({ "text": text, "limit": 60 }).to_string();
    let started = Instant::now();
    loop {
        let asked = Instant::now();
        let reply = winsys::http::local(&format!("http://127.0.0.1:{}/search", helper.port), body.as_bytes(), Duration::from_secs(120));
        let reply: Value = reply.ok().and_then(|(_, bytes)| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
        let waiting = reply["waiting"].as_array().is_some_and(|w| !w.is_empty());
        if !waiting || started.elapsed() > Duration::from_secs(600) {
            let hits = reply["hits"].as_array().into_iter().flatten().filter_map(|h| h["path"].as_str().map(String::from)).collect();
            return (hits, asked.elapsed().as_secs_f64() * 1000.0);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn run(options: &Options) -> Value {
    let list: Value = serde_json::from_slice(&std::fs::read(options.home.join("sample.json")).expect("no sample.json: make the home with `measure reading --keep`"))
        .expect("sample.json");
    let kind = list["kind"].as_str().unwrap_or("").to_string();
    if let Some(parts) = list["corpus"].as_object() {
        crate::corpus::set(parts.iter().map(|(name, path)| (name.clone(), PathBuf::from(path.as_str().unwrap_or("")))).collect());
    }
    let files: Vec<String> = list["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).collect();
    let folders: Vec<String> = list["folders"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).collect();
    let mut rng = Rng::new(options.seed);
    let truths = truths(&kind, &mut rng);
    // Each file's path as it was read: each folder read is one of the kind's folders, in the same order.
    let read_path = |file: &str| -> Option<String> {
        crate::reading::folders(&kind).iter().zip(&folders).find(|(k, _)| file.starts_with(&format!("{k}/")))
            .map(|(k, folder)| format!("{folder}{}", file[k.len()..].replace('/', "\\")))
    };
    let reader = content::Reader::open(&options.home.join("local").join("index"));
    let picked = rng.sample(&files, options.queries.min(files.len()));
    println!("{kind}: {} queries over {} files", picked.len(), files.len());

    let (mut first, mut ten, mut asked, mut no_query) = (0, 0, 0, 0);
    let mut ranks = Vec::new();
    let mut times = Vec::new();
    // What was read against the reference: words (speech: errors; image-text: found).
    let (mut reference_words, mut errors, mut recalled) = (0usize, 0usize, 0usize);
    let mut by_language: HashMap<String, (usize, usize)> = HashMap::new();
    let mut with_text = 0;
    let helper = matches!(kind.as_str(), "image-shown" | "video-shown").then(|| helper(options));
    for file in &picked {
        let Some(path) = read_path(file) else { continue };
        let stored = reader.as_ref().ok().and_then(|r| r.stored(&path)).map(|s| s.text).unwrap_or_default();
        if !stored.trim().is_empty() {
            with_text += 1;
        }
        let truth = truths.get(file);
        if let Some(reference) = truth.and_then(|t| t.reference.as_ref()) {
            let heard = words(&stored);
            if kind.starts_with("speech") {
                let wrong = edits(reference, &heard);
                errors += wrong;
                reference_words += reference.len();
                let entry = by_language.entry(truth.unwrap().language.clone()).or_default();
                entry.0 += wrong;
                entry.1 += reference.len();
            } else {
                let read: std::collections::HashSet<&str> = heard.iter().map(String::as_str).collect();
                let text = stored.to_lowercase();
                recalled += reference.iter().filter(|w| read.contains(w.as_str()) || text.contains(w.as_str())).count();
                reference_words += reference.len();
            }
        }
        let query = match kind.as_str() {
            "documents" => phrase(&words(&stored), &mut rng),
            _ => truth.and_then(|t| t.query.clone()),
        };
        let Some(query) = query else { no_query += 1; continue };
        let (hits, ms) = if let Some(helper) = &helper {
            seek(helper, &query)
        } else {
            let started = Instant::now();
            let found = reader.as_ref().map(|r| r.search(&content::Query {
                words: query.split_whitespace().map(String::from).collect(), prefix: true, inside_only: kind == "image-text",
                ..content::Query::default()
            }, 10)).unwrap_or_default();
            (found.into_iter().map(|hit| hit.path).collect::<Vec<_>>(), started.elapsed().as_secs_f64() * 1000.0)
        };
        asked += 1;
        times.push(ms);
        match rank(&hits, file) {
            Some(at) => {
                ranks.push(at as f64);
                first += usize::from(at == 1);
                ten += usize::from(at <= 10);
            }
            None => {}
        }
        if asked % 100 == 0 {
            println!("  {asked}: first {first}, top ten {ten}");
        }
    }
    let mut report = json!({
        "kind": kind, "files_read": files.len(), "queries": asked, "seed": options.seed,
        "files_without_a_query": no_query,
        "files_with_text": rate(with_text, picked.len()),
        "first": rate(first, asked), "top_ten": rate(ten, asked),
        "rank_when_in_top_sixty": summary(&ranks),
        "search_ms": summary(&times),
    });
    if kind.starts_with("speech") {
        report["word_error_rate"] = json!(stats::round(errors as f64 / reference_words.max(1) as f64));
        report["word_error_rate_by_language"] = by_language.iter()
            .map(|(language, (e, n))| (language.clone(), json!(stats::round(*e as f64 / (*n).max(1) as f64)))).collect();
    } else if kind == "image-text" {
        report["reference_words_read"] = rate(recalled, reference_words);
    }
    println!("first {first} of {asked}, top ten {ten}");
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_errors() {
        let a = words("The cat sat on the mat.");
        assert_eq!(edits(&a, &words("the cat sat on a mat")), 1);
        assert_eq!(edits(&a, &words("cat sat on the mat")), 1);
        assert_eq!(edits(&a, &[]), 6);
        assert_eq!(words("تشكلت في المحيط"), vec!["تشكلت", "في", "المحيط"]);
    }

    #[test]
    fn ranks_by_path_ending() {
        let hits = vec![r"C:\Users\A\Pictures\coco-val2017\1.jpg".to_string(), r"C:\Users\A\Pictures\coco-val2017\2.jpg".to_string()];
        assert_eq!(rank(&hits, "images/coco-val2017/2.jpg"), Some(2));
        assert_eq!(rank(&hits, "images/coco-val2017/3.jpg"), None);
    }
}
