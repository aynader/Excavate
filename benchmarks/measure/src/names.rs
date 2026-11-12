//! `measure names`: Bedrock against Everything, on this PC's drives.
//!
//! Bedrock runs here as it runs in Excavate (the same service, `engine::names`, in this process); Everything is asked
//! through its SDK, as Excavate asks it. Each figure is measured `--runs` or `--repeat` times and summed up with its
//! spread. Counts only: no file names are written.

use crate::process::{self, cost};
use crate::stats::{self, summary};
use engine::names::{Names, Phase};
use everything::{Everything, Query, Sort};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Options {
    pub index: PathBuf,
    pub queries: PathBuf,
    pub scratch: PathBuf,
    pub runs: usize,
    pub repeat: usize,
    pub changes: usize,
    pub idle_seconds: u64,
    /// Everything's program and database, to time its own building and opening (it's ended and started for that).
    pub everything: Option<(PathBuf, PathBuf)>,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

fn ask(everything: &Everything, query: Query) -> Option<(u64, Duration)> {
    let started = Instant::now();
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let mut pages = everything.search_blocking(generation, vec![query]).ok()?;
    Some((pages.remove(0).total, started.elapsed()))
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn wait_live(names: &Names, limit: Duration) -> Duration {
    let started = Instant::now();
    while names.state().phase != Phase::Live && started.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
    started.elapsed()
}

fn stop(names: Arc<Names>) {
    names.stop();
    let started = Instant::now();
    while !names.stopped() && started.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn remove_index(file: &Path) {
    let _ = std::fs::remove_file(file);
    let _ = std::fs::remove_file(file.with_extension("journal"));
}

/// Everything's own process (its window and its index), not its service (session 0), which reads the drives for it.
fn everything_client() -> Option<u32> {
    process::pids("Everything.exe").into_iter().find(|&pid| process::session(pid) != Some(0))
}

/// What Everything costs, both processes together (each needs the other): CPU seconds, private bytes; and the
/// private bytes of each, its own process first.
fn everything_cost() -> Option<(f64, u64, Vec<u64>)> {
    let mut pids = process::pids("Everything.exe");
    pids.sort_by_key(|&pid| process::session(pid) == Some(0));
    let costs: Vec<(f64, u64)> = pids.into_iter().filter_map(cost).collect();
    (!costs.is_empty()).then(|| (costs.iter().map(|c| c.0).sum(), costs.iter().map(|c| c.1).sum(), costs.iter().map(|c| c.1).collect()))
}

fn mb(bytes: u64) -> f64 {
    stats::round(bytes as f64 / 1_048_576.0)
}

fn their_memory() -> Value {
    everything_cost().map_or(Value::Null, |(_, total, each)| json!({ "both": mb(total), "each": each.into_iter().map(mb).collect::<Vec<_>>() }))
}

pub fn run(options: &Options) -> Value {
    let everything = Everything::shared();
    let status = everything.status();
    if !status.available || !status.db_loaded {
        eprintln!("Everything isn't running (or hasn't loaded its database): it's needed to compare against");
        std::process::exit(2);
    }
    println!("Everything {} | Bedrock: {}", status.version, options.index.display());
    let mut report = serde_json::Map::new();
    report.insert("everything_version".into(), json!(status.version));

    // Building from nothing from the drives' own tables, as Excavate's helper does when Windows allows it (administrator
    // rights): first, as the folder walk below leaves its index for the opening runs.
    let mut from_tables = Vec::new();
    let mut tables = json!(null);
    for run in 0..options.runs {
        remove_index(&options.index);
        let started = Instant::now();
        match engine::names::build_from_tables(&options.index, None) {
            Ok(built) => {
                from_tables.push(started.elapsed().as_secs_f64());
                println!("from the drives' tables {}/{}: {:.2} s, {} entries", run + 1, options.runs, started.elapsed().as_secs_f64(), built.entries);
                tables = json!({ "entries": built.entries, "drives_read": built.read, "drives_walked": built.walked });
            }
            Err(error) => {
                println!("from the drives' tables: {error}");
                tables = json!({ "error": error });
                break;
            }
        }
    }

    // Building from nothing folder by folder (as Excavate does without administrator rights), then opening what was saved.
    let mut builds = Vec::new();
    let mut build_cpu = Vec::new();
    let mut memory = Vec::new();
    let mut entries = 0;
    let mut facts = json!({});
    for run in 0..options.runs {
        remove_index(&options.index);
        let (cpu, private) = (engine::cpu_seconds(), engine::private_bytes());
        let names = Names::start_over(options.index.clone(), None);
        let took = wait_live(&names, Duration::from_secs(3600));
        builds.push(took.as_secs_f64());
        build_cpu.push(engine::cpu_seconds() - cpu);
        memory.push(engine::private_bytes().saturating_sub(private) as f64 / 1_048_576.0);
        let state = names.state();
        entries = state.entries;
        facts = json!({ "entries": state.entries, "drives": state.volumes, "refused_folders": state.refused });
        println!("build {}/{}: {:.2} s, {} entries", run + 1, options.runs, took.as_secs_f64(), state.entries);
        stop(names);
    }
    let size = std::fs::metadata(&options.index).map(|m| m.len()).unwrap_or(0);
    let mut opens = Vec::new();
    let mut answering = Vec::new();
    for _ in 0..options.runs {
        let started = Instant::now();
        let names = Names::start(options.index.clone());
        while !names.ready() && started.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_millis(1));
        }
        answering.push(ms(started.elapsed()));
        wait_live(&names, Duration::from_secs(600));
        opens.push(started.elapsed().as_secs_f64());
        stop(names);
    }
    // Kept running for the rest, as Excavate keeps it.
    let names = Names::start(options.index.clone());
    wait_live(&names, Duration::from_secs(600));
    report.insert("bedrock".into(), json!({
        "facts": facts,
        "from_the_drives_tables": tables,
        "build_from_the_drives_tables_seconds": summary(&from_tables),
        "build_seconds": summary(&builds),
        "build_cpu_seconds": summary(&build_cpu),
        "memory_mb": summary(&memory),
        "bytes_per_entry": stats::round(stats::median(&memory) * 1_048_576.0 / entries.max(1) as f64),
        "size_on_disk_mb": stats::round(size as f64 / 1_048_576.0),
        "answering_after_ms": summary(&answering),
        "open_seconds": summary(&opens),
    }));
    let mut theirs = serde_json::Map::new();
    // (Both processes: its own, then its service.)
    theirs.insert("memory_mb".into(), their_memory());
    if let Some((exe, db)) = &options.everything {
        theirs.insert("size_on_disk_mb".into(), json!(std::fs::metadata(db).map(|m| stats::round(m.len() as f64 / 1_048_576.0)).ok()));
        let (built, opened) = everything_cycles(&everything, exe, db, options.runs);
        theirs.insert("build_seconds".into(), summary(&built));
        theirs.insert("open_seconds".into(), summary(&opened));
        theirs.insert("memory_mb_after_its_restarts".into(), their_memory());
    }
    report.insert("everything".into(), Value::Object(theirs));

    report.insert("queries".into(), queries(&names, &everything, &options.queries, options.repeat));
    report.insert("new_file_found_ms".into(), new_files(&names, &everything, &options.scratch, 50));
    report.insert("idle".into(), idle(options.idle_seconds));
    report.insert("changes".into(), changes(&names, &everything, &options.scratch, options.changes, options.runs));
    stop(names);
    remove_index(&options.index);
    let _ = std::fs::remove_dir_all(&options.scratch);
    Value::Object(report)
}

/// Everything ended, its database removed (to build) or kept (to open), started again: how long until it answers with
/// every entry. Building is timed from its start to the moment its count settles.
fn everything_cycles(everything: &Everything, exe: &Path, db: &Path, runs: usize) -> (Vec<f64>, Vec<f64>) {
    let total = || ask(everything, Query::new("", Sort::NameAscending, 0)).map(|(t, _)| t);
    let expected = total().unwrap_or(0);
    let mut built = Vec::new();
    let mut opened = Vec::new();
    for fresh in [true, false] {
        for run in 0..runs {
            let _ = std::process::Command::new(exe).arg("-exit").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            let ended = Instant::now();
            while everything_client().is_some() && ended.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(100));
            }
            if fresh {
                let _ = std::fs::remove_file(db);
            }
            let started = Instant::now();
            // Without this program's output: Everything keeps running, and a reader of that output would wait for it.
            if std::process::Command::new(exe).arg("-startup").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().is_err() {
                break;
            }
            // Until it holds as many entries as before (within 1%: a few change meanwhile).
            loop {
                if let Some(now) = total() {
                    if now as f64 >= expected as f64 * 0.99 && everything.status().db_loaded {
                        break;
                    }
                }
                if started.elapsed() > Duration::from_secs(3600) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let took = started.elapsed().as_secs_f64();
            println!("Everything {} {}/{runs}: {took:.2} s", if fresh { "build" } else { "open" }, run + 1);
            if fresh { built.push(took) } else { opened.push(took) }
        }
    }
    (built, opened)
}

/// Each query of the list, `repeat` times on each (after three to warm up), alternating: the time until the first 100
/// results are in hand, and both counts.
fn queries(names: &Names, everything: &Everything, list: &Path, repeat: usize) -> Value {
    let text = std::fs::read_to_string(list).unwrap_or_else(|error| panic!("{}: {error}", list.display()));
    let mut rows = Vec::new();
    let (mut same, mut asked) = (0, 0);
    println!("{:<34} {:>10} {:>10} {:>9} {:>9}", "query", "Bedrock", "Everything", "ms", "ms");
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let theirs_text = line;
        let (mut ours, mut theirs) = (Vec::new(), Vec::new());
        let (mut our_count, mut their_count) = (0, None);
        for round in 0..repeat + 3 {
            let started = Instant::now();
            let page = names.query(&Query::new(line, Sort::NameAscending, 100));
            let took = started.elapsed();
            our_count = page.total;
            let answer = ask(everything, Query::new(theirs_text, Sort::NameAscending, 100));
            if round >= 3 {
                ours.push(ms(took));
                if let Some((count, took)) = answer {
                    theirs.push(ms(took));
                    their_count = Some(count);
                }
            }
        }
        asked += 1;
        if their_count == Some(our_count) {
            same += 1;
        }
        println!("{line:<34} {our_count:>10} {:>10} {:>9.2} {:>9.2}", their_count.map_or("-".into(), |c| c.to_string()),
                 stats::median(&ours), stats::median(&theirs));
        rows.push(json!({ "query": line, "bedrock_count": our_count, "everything_count": their_count,
                          "bedrock_ms": summary(&ours), "everything_ms": summary(&theirs) }));
    }
    json!({ "repeat": repeat, "same_count": same, "queries": asked, "each": rows })
}

/// A file made, then each engine asked until it's found (each on its own thread, from the moment it's written).
fn new_files(names: &Arc<Names>, everything: &Arc<Everything>, scratch: &Path, count: usize) -> Value {
    let folder = scratch.join("new");
    let _ = std::fs::create_dir_all(&folder);
    std::thread::sleep(Duration::from_secs(2));
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    for n in 0..count {
        let name = format!("measure-new-{}-{n}.bin", std::process::id());
        let started = Instant::now();
        std::fs::write(folder.join(&name), b"x").expect("can't write in the scratch folder");
        let find = |found: &dyn Fn() -> bool| -> Option<f64> {
            while started.elapsed() < Duration::from_secs(10) {
                if found() {
                    return Some(ms(started.elapsed()));
                }
                std::thread::sleep(Duration::from_micros(500));
            }
            None
        };
        let quoted = format!("\"{name}\"");
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| find(&|| names.query(&Query::new(quoted.as_str(), Sort::NameAscending, 1)).total > 0));
            let b = scope.spawn(|| find(&|| ask(everything, Query::new(quoted.as_str(), Sort::NameAscending, 1)).is_some_and(|(t, _)| t > 0)));
            (a.join().unwrap(), b.join().unwrap())
        });
        ours.extend(a);
        theirs.extend(b);
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("a new file found: Bedrock {:.1} ms, Everything {:.1} ms (medians)", stats::median(&ours), stats::median(&theirs));
    json!({ "files": count, "bedrock": summary(&ours), "everything": summary(&theirs) })
}

/// Nothing asked, nothing changed by this tool: each one's CPU over the time.
fn idle(seconds: u64) -> Value {
    let (ours, theirs) = (engine::cpu_seconds(), everything_cost());
    std::thread::sleep(Duration::from_secs(seconds));
    let ours = engine::cpu_seconds() - ours;
    let theirs = theirs.zip(everything_cost()).map(|((a, _, _), (b, _, _))| b - a);
    println!("idle for {seconds} s: Bedrock {:.3} s of CPU, Everything {}", ours, shown(theirs, " s"));
    json!({ "seconds": seconds, "bedrock_cpu_seconds": stats::round(ours), "everything_cpu_seconds": theirs.map(stats::round) })
}

/// `count` files made, half renamed, half deleted (2 x count changes) by another process (`measure churn`), then a
/// pause for both to catch up: each one's CPU per change, and whether both then agree.
fn changes(names: &Names, everything: &Everything, scratch: &Path, count: usize, runs: usize) -> Value {
    let (mut ours, mut theirs, mut agree) = (Vec::new(), Vec::new(), 0);
    for run in 0..runs {
        let folder = scratch.join(format!("churn-{run}"));
        let (our_cpu, their_cpu) = (engine::cpu_seconds(), everything_cost());
        let exe = std::env::current_exe().expect("this program");
        let done = std::process::Command::new(exe).arg("churn").arg(&folder).arg(count.to_string()).status();
        if !done.is_ok_and(|status| status.success()) {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
        let changes = (2 * count) as f64;
        ours.push((engine::cpu_seconds() - our_cpu) * 1e6 / changes);
        let their_run = their_cpu.zip(everything_cost()).map(|((a, _, _), (b, _, _))| (b - a) * 1e6 / changes);
        theirs.extend(their_run);
        let scope = format!("\"{}\\\" measure-churn-", folder.display());
        let our_count = names.query(&Query::new(scope.as_str(), Sort::NameAscending, 0)).total;
        let their_count = ask(everything, Query::new(scope.as_str(), Sort::NameAscending, 0)).map(|(t, _)| t);
        if their_count == Some(our_count) && our_count == count as u64 / 2 {
            agree += 1;
        }
        println!("changes {}/{runs}: {} each; Bedrock {:.1} us of CPU a change, found {our_count}; Everything {}, found {}",
                 run + 1, 2 * count, ours.last().unwrap(), shown(their_run, " us"),
                 their_count.map_or("-".to_string(), |c| c.to_string()));
        let _ = std::fs::remove_dir_all(&folder);
        std::thread::sleep(Duration::from_secs(3));
    }
    json!({ "changes_each_run": 2 * count, "bedrock_cpu_us": summary(&ours), "everything_cpu_us": summary(&theirs),
            "runs_where_both_agree": agree })
}

/// `measure churn <folder> <count>`: the changes, made from a process of their own so their cost isn't counted as
/// Bedrock's. Half the files renamed (and kept), the other half deleted.
pub fn churn(folder: &Path, count: usize) {
    std::fs::create_dir_all(folder).expect("can't make the folder");
    for n in 0..count {
        std::fs::write(folder.join(format!("measure-file-{n}.txt")), b"x").expect("write");
    }
    for n in (0..count).step_by(2) {
        std::fs::rename(folder.join(format!("measure-file-{n}.txt")), folder.join(format!("measure-churn-{n}.txt"))).expect("rename");
    }
    for n in (1..count).step_by(2) {
        std::fs::remove_file(folder.join(format!("measure-file-{n}.txt"))).expect("delete");
    }
}

/// A figure for the summary lines, or "-" when it couldn't be measured.
fn shown(value: Option<f64>, unit: &str) -> String {
    value.map_or("-".to_string(), |v| format!("{v:.3}{unit}"))
}
