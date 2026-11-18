//! `measure conversing`: finding a photo by a description typed the way people type one ("a photo i took of a cake",
//! "pic of the zebras i saved", "dog beach", typos), through the popup's own path: the route the popup gives what was
//! typed (`search::describe::route`), the search helper's tidying, the model and its ranking, and what of the helper's
//! answer the popup then shows (`app.rs`, `seen_found` and `present`).
//!
//! - `measure conversing --prepare --excavate <release folder> --choice quick|balanced --work <folder>` makes a home that
//!   has looked at every COCO val2017 photo (`<work>/home-<choice>`). Excavate's own index is on in it, so its indexer
//!   walks the folder itself and never asks Everything.
//! - `measure conversing --home <home> --excavate <release folder> --queries <queries.jsonl>` asks each query, as typed
//!   and as its plain description (`core`, what a perfect tidying would leave), and scores both.
//!
//! A query's right answers are every photo whose COCO captions and objects have each of its `need` groups (a group is
//! words separated by `|`, any of which will do, each matched at a word's start): "show me photos from the beach" is
//! right with any beach. A query written from one photo also says which (`target`).

use crate::process::Job;
use crate::stats::round;
use config::status::Status;
use config::{Paths, Settings};
use search::describe::{self, Route};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Options {
    pub excavate: PathBuf,
    pub work: PathBuf,
    pub choice: String,
    pub prepare: bool,
    pub home: Option<PathBuf>,
    pub queries: Option<PathBuf>,
}

/// As the popup has them (`app.rs`): images with the toggle on, those that may join names, and the best shown when
/// nothing else is found; how far above the rest a description's images need to stand to join.
const SEEN_SHOWN: usize = 60;
const SEEN_JOINING: usize = 4;
const CLOSEST: usize = 12;
const DESCRIBED_LINE: f32 = 3.0;

/// `seeking::Answer::line`, as the popup has it.
fn line_for(count: usize) -> f32 {
    let count = count.max(2) as f32;
    ((2.0 * count.ln()).sqrt() + 0.4).max(4.0)
}

fn coco_folder() -> PathBuf {
    crate::corpus::path("images/coco-val2017")
}

pub fn run(options: &Options) -> Value {
    if options.prepare {
        return prepare(options);
    }
    ask_all(options)
}

/// A home that has looked at every photo with the choice's model.
fn prepare(options: &Options) -> Value {
    let home = options.work.join(format!("home-{}", options.choice));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("local").join("logs")).expect("can't make the home");
    std::fs::create_dir_all(home.join("roaming")).unwrap();
    let models = options.work.join("models");
    std::fs::create_dir_all(&models).unwrap();
    let windows = |path: &Path| path.to_string_lossy().replace('/', "\\");
    let linked = std::process::Command::new("cmd.exe").args(["/c", "mklink", "/J"]).arg(windows(&home.join("local").join("models")))
        .arg(windows(&models)).stdout(std::process::Stdio::null()).status();
    assert!(linked.is_ok_and(|s| s.success()), "can't link the models folder");
    let paths = Paths { roaming: home.join("roaming"), local: home.join("local") };
    let mut settings: Settings = crate::reading::settings("image-shown", &options.choice, vec![coco_folder().to_string_lossy().into_owned()]);
    // The folder walked here: Everything is left alone.
    settings.own_index = true;
    settings.save(&paths);
    let job = Job::new();
    let started = Instant::now();
    let child = std::process::Command::new(options.excavate.join("excavate-indexer.exe")).env("EXCAVATE_HOME", &home)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().expect("can't start excavate-indexer.exe");
    job.add(&child);
    let mut status = Status::default();
    let mut done_at: Option<Instant> = None;
    let mut printed = Instant::now();
    while started.elapsed() < Duration::from_secs(4 * 3600) {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(now) = Status::load(&paths) {
            status = now;
        }
        if crate::reading::finished("image-shown", &status) {
            if done_at.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(10) {
                break;
            }
        } else {
            done_at = None;
        }
        if printed.elapsed() > Duration::from_secs(60) {
            printed = Instant::now();
            let looked: u64 = status.looking.iter().map(|l| l.done).sum();
            println!("  {:.0} min: {looked} looked at, {}, {:?}", started.elapsed().as_secs_f64() / 60.0, status.state, status.errors);
        }
    }
    drop(job);
    let looked: u64 = status.looking.iter().map(|l| l.done).sum();
    println!("{}: {looked} photos looked at in {:.0} s", options.choice, started.elapsed().as_secs_f64());
    json!({ "choice": options.choice, "home": home, "looked_at": looked, "seconds": round(started.elapsed().as_secs_f64()) })
}

struct Query {
    style: String,
    typed: String,
    core: String,
    target: String,
    need: Vec<Vec<String>>,
}

fn load_queries(path: &Path) -> Vec<Query> {
    std::fs::read_to_string(path).expect("can't read the queries").lines().filter(|line| !line.trim().is_empty()).map(|line| {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}"));
        Query {
            style: v["style"].as_str().unwrap_or("").into(),
            typed: v["q"].as_str().unwrap_or("").into(),
            core: v["core"].as_str().unwrap_or("").into(),
            target: v["target"].as_str().unwrap_or("").into(),
            need: v["need"].as_array().into_iter().flatten().filter_map(Value::as_str)
                .map(|group| group.split('|').map(|w| w.trim().to_lowercase()).collect()).collect(),
        }
    }).collect()
}

/// Each photo's file name and its captions and objects, lowercased.
fn descriptions() -> HashMap<String, String> {
    let text = std::fs::read_to_string(crate::corpus::truth().join("coco.jsonl")).expect("no coco.jsonl");
    text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).map(|record| {
        let file = record["file"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").to_lowercase();
        let mut all: Vec<String> = record["captions"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase).collect();
        all.extend(record["objects"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_lowercase));
        (file, format!(" {} ", all.join(" ; ").replace(|c: char| !c.is_alphanumeric() && c != ';', " ")))
    }).collect()
}

/// Whether a description has a word starting with each group's words (any one of them).
fn fits(description: &str, need: &[Vec<String>]) -> bool {
    need.iter().all(|group| group.iter().any(|word| description.contains(&format!(" {word}"))))
}

struct Helper {
    port: u64,
    _job: Job,
}

fn helper(home: &Path, excavate: &Path) -> Helper {
    let said = home.join("local").join("seeker.json");
    let _ = std::fs::remove_file(&said);
    let job = Job::new();
    let child = std::process::Command::new(excavate.join("excavate-helper.exe")).arg("--seek").env("EXCAVATE_HOME", home)
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

/// The helper's answer, once its model is ready, as the popup asks with images alone (`documents` false: there are
/// none here).
fn seek(helper: &Helper, text: &str) -> Value {
    let body = json!({ "text": text, "limit": SEEN_SHOWN, "images": true, "documents": false, "extensions": [] }).to_string();
    let started = Instant::now();
    loop {
        let reply = winsys::http::local(&format!("http://127.0.0.1:{}/search", helper.port), body.as_bytes(), Duration::from_secs(120));
        let reply: Value = reply.ok().and_then(|(_, bytes)| serde_json::from_slice(&bytes).ok()).unwrap_or(Value::Null);
        let waiting = reply["waiting"].as_array().is_some_and(|w| !w.is_empty());
        if !waiting && !reply.is_null() || started.elapsed() > Duration::from_secs(600) {
            return reply;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// What the popup shows of the helper's answer for this route: with the photo toggle on (a description of an image),
/// every hit in order; otherwise, nothing having been found by name or by the words inside (COCO's names are numbers),
/// the few that stand out enough to join the names, or else the closest few.
fn shown(reply: &Value, route: Route) -> Vec<String> {
    let hits: Vec<(String, f32)> = reply["hits"].as_array().into_iter().flatten()
        .filter_map(|hit| Some((hit["path"].as_str()?.rsplit('\\').next()?.to_lowercase(), hit["score"].as_f64()? as f32))).collect();
    if route == Route::Image && !hits.is_empty() {
        return hits.into_iter().map(|(path, _)| path).collect();
    }
    let images = reply["images"].as_u64().unwrap_or(0) as usize;
    let filtered = reply["filtered"].as_bool().unwrap_or(false);
    let line = if filtered { f32::MIN } else { line_for(images) };
    let line = if route == Route::Name { line } else { line.min(DESCRIBED_LINE) };
    let joining: Vec<String> = hits.iter().filter(|(_, score)| *score >= line).take(SEEN_JOINING).map(|(path, _)| path.clone()).collect();
    if !joining.is_empty() {
        return joining;
    }
    hits.into_iter().take(CLOSEST).map(|(path, _)| path).collect()
}

#[derive(Default, Clone)]
struct Tally {
    asked: usize,
    /// Written from one photo: where that photo came.
    targeted: usize,
    target_first: usize,
    target_ten: usize,
    target_reciprocal: f64,
    /// Of the first ten shown, how many are right (fewer shown count as wrong), summed.
    precision: f64,
    image_route: usize,
}

impl Tally {
    fn add(&mut self, one: &One) {
        self.asked += 1;
        if let Some(target) = one.target_rank {
            self.targeted += 1;
            self.target_first += usize::from(target == Some(1));
            self.target_ten += usize::from(target.is_some_and(|r| r <= 10));
            self.target_reciprocal += target.map_or(0.0, |r| 1.0 / r as f64);
        }
        self.precision += one.right_in_ten as f64 / 10.0;
        self.image_route += usize::from(one.route == Route::Image);
    }

    fn numbers(&self) -> [f64; 5] {
        let share = |a: f64, n: usize| round(a * 100.0 / n.max(1) as f64);
        [share(self.target_first as f64, self.targeted), share(self.target_ten as f64, self.targeted), share(self.target_reciprocal, self.targeted),
         share(self.precision, self.asked), share(self.image_route as f64, self.asked)]
    }

    fn report(&self) -> Value {
        let [r1, r10, mrr, p10, image] = self.numbers();
        json!({ "queries": self.asked, "targeted": self.targeted, "target_r1": r1, "target_r10": r10, "target_mrr": mrr, "p10": p10, "image_route": image })
    }
}

/// One search, as the popup would show it.
struct One {
    route: Route,
    target_rank: Option<Option<usize>>,
    right_in_ten: usize,
    shown: usize,
    top: String,
    /// The helper's first 20: standing, similarity, right.
    hits: Vec<Value>,
}

fn ask_all(options: &Options) -> Value {
    let home = options.home.clone().expect("--home");
    let queries = load_queries(options.queries.as_ref().expect("--queries"));
    let described = descriptions();
    let helper = helper(&home, &options.excavate);
    let mut by_style: BTreeMap<String, (Tally, Tally)> = BTreeMap::new();
    let (mut all_typed, mut all_core) = (Tally::default(), Tally::default());
    let mut rows = Vec::new();
    for query in &queries {
        let right: HashSet<&String> = described.iter().filter(|(_, d)| fits(d, &query.need)).map(|(file, _)| file).collect();
        let target = (!query.target.is_empty()).then(|| query.target.to_lowercase());
        let one = |text: &str| {
            let route = describe::route(&search::Terms::parse(text));
            let reply = seek(&helper, text);
            let list = shown(&reply, route);
            let hits = reply["hits"].as_array().into_iter().flatten().take(20).filter_map(|hit| {
                let file = hit["path"].as_str()?.rsplit('\\').next()?.to_lowercase();
                Some(json!([round(hit["score"].as_f64()?), round(hit["similarity"].as_f64()?), right.contains(&file)]))
            }).collect();
            One {
                route,
                target_rank: target.as_ref().map(|target| list.iter().position(|file| file == target).map(|at| at + 1)),
                right_in_ten: list.iter().take(10).filter(|file| right.contains(file)).count(),
                shown: list.len(),
                top: list.first().cloned().unwrap_or_default(),
                hits,
            }
        };
        let typed = one(&query.typed);
        let core = one(&query.core);
        let entry = by_style.entry(query.style.clone()).or_default();
        entry.0.add(&typed);
        entry.1.add(&core);
        // (Controls with nothing right to find: only their scores count.)
        if query.style != "absent" {
            all_typed.add(&typed);
            all_core.add(&core);
        }
        rows.push(json!({
            "style": query.style, "typed": query.typed, "route": format!("{:?}", typed.route), "target_rank": typed.target_rank,
            "right_in_ten": typed.right_in_ten, "shown": typed.shown, "top": typed.top, "hits": typed.hits, "right": right.len(),
            "core_route": format!("{:?}", core.route), "core_target_rank": core.target_rank, "core_right_in_ten": core.right_in_ten,
        }));
    }
    println!("{:<11} {:>4} {:>4} | {:>6} {:>6} {:>6} {:>6} {:>6} | core {:>6} {:>6} {:>6} {:>6}", "style", "n", "tgt", "R@1", "R@10", "MRR", "P@10",
             "image", "R@1", "R@10", "MRR", "P@10");
    let line = |name: &str, typed: &Tally, core: &Tally| {
        let ([r1, r10, mrr, p10, image], [c1, c10, cmrr, cp10, _]) = (typed.numbers(), core.numbers());
        println!("{name:<11} {:>4} {:>4} | {r1:>6.1} {r10:>6.1} {mrr:>6.1} {p10:>6.1} {image:>6.0} | core {c1:>6.1} {c10:>6.1} {cmrr:>6.1} {cp10:>6.1}",
                 typed.asked, typed.targeted);
    };
    for (style, (typed, core)) in &by_style {
        line(style, typed, core);
    }
    line("all", &all_typed, &all_core);
    json!({
        "queries": queries.len(),
        "all": { "typed": all_typed.report(), "core": all_core.report() },
        "by_style": by_style.iter().map(|(style, (typed, core))| (style.clone(), json!({ "typed": typed.report(), "core": core.report() })))
            .collect::<serde_json::Map<_, _>>(),
        "rows": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_fit_at_word_starts() {
        let d = " a dog on the beach ; dogs ; surfboard ";
        assert!(fits(d, &[vec!["dog".into()], vec!["beach".into(), "sand".into()]]));
        assert!(!fits(d, &[vec!["og".into()]]));
        assert!(fits(d, &[vec!["surf".into()]]));
    }
}
