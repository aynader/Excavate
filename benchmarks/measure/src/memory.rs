//! `measure memory`: the private memory of Excavate's processes at rest and while the search helper answers, on a home
//! `measure reading --keep` made, against limits, so a change that makes it creep back is caught (the run fails).
//!
//! - At rest: Excavate started as Windows starts it (`--hidden`, the reader with it), measured over the last 30 s of
//!   `--rest` seconds (the median of a sample every 5 s), each program on its own.
//! - Searching: the search helper (`excavate-helper --seek`, as the popup starts it) asked `--searches` times, its
//!   memory and its model server's sampled every 200 ms (the peak), then again once it has let the documents' vectors
//!   go (a minute after the last search).
//!
//! Only this run's processes are counted (they're in its job object), never the person's own Excavate. Everything is
//! never started or closed (`EXCAVATE_PRETEND_EVERYTHING`).

use crate::process::Job;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Options {
    pub home: PathBuf,
    pub excavate: PathBuf,
    pub rest: u64,
    pub searches: usize,
    /// Limits in MB: Excavate, its reader, the helper at rest, the helper with its model server while searching.
    pub limits: [u64; 4],
}

/// What searches look like: a few words, as typed.
const SEARCHES: [&str; 12] = ["a dog on the beach", "invoice from last year", "birthday cake", "mountains at sunset",
    "meeting notes", "a red car", "the lease agreement", "people at a wedding", "a cat asleep", "tax return",
    "a city at night", "recipe for bread"];

const PROGRAMS: [&str; 4] = ["excavate.exe", "excavate-indexer.exe", "excavate-helper.exe", "llama-server.exe"];

/// Each of the job's programs' private memory now, in MB (summed when one runs twice).
fn sample(job: &Job) -> [f64; 4] {
    let mut out = [0.0; 4];
    for (i, name) in PROGRAMS.iter().enumerate() {
        for pid in crate::process::pids(name) {
            if let Some((_, bytes)) = job.contains(pid).then(|| crate::process::cost(pid)).flatten() {
                out[i] += bytes as f64 / (1 << 20) as f64;
            }
        }
    }
    out
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

pub fn run(options: &Options) -> Value {
    let job = std::sync::Arc::new(Job::new());
    let child = std::process::Command::new(options.excavate.join("excavate.exe")).arg("--hidden")
        .env("EXCAVATE_HOME", &options.home).env("EXCAVATE_PRETEND_EVERYTHING", "running")
        .spawn().expect("can't start excavate.exe");
    job.add(&child);
    let started = Instant::now();
    let rest = Duration::from_secs(options.rest.max(40));
    let mut rest_samples: Vec<[f64; 4]> = Vec::new();
    while started.elapsed() < rest {
        std::thread::sleep(Duration::from_secs(5));
        if started.elapsed() + Duration::from_secs(30) >= rest {
            rest_samples.push(sample(&job));
        }
    }
    let at_rest: Vec<f64> = (0..3).map(|i| median(rest_samples.iter().map(|s| s[i]).collect())).collect();
    println!("at rest: Excavate {:.0} MB, reader {:.0} MB, helper {:.0} MB", at_rest[0], at_rest[1], at_rest[2]);

    // The helper, as the popup starts it, in this job too.
    let helper = crate::finding::start_helper(&options.home, &options.excavate, Some(&job));
    std::thread::sleep(Duration::from_secs(10));
    let helper_rest = sample(&job)[2];
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let (done, job) = (done.clone(), job.clone());
        std::thread::spawn(move || {
            let mut peak = [0.0f64; 4];
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let now = sample(&job);
                for i in 0..4 {
                    peak[i] = peak[i].max(now[i]);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            peak
        })
    };
    let mut times = Vec::new();
    for n in 0..options.searches {
        let (_, ms) = crate::finding::ask(&helper, SEARCHES[n % SEARCHES.len()]);
        times.push(ms);
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let peak = watcher.join().unwrap();
    let searching = peak[2] + peak[3];
    println!("searching: helper {:.0} MB at its peak, its model server {:.0} MB", peak[2], peak[3]);
    // A minute after the last search, the documents' vectors are let go.
    std::thread::sleep(Duration::from_secs(70));
    let after = sample(&job);
    println!("after: helper {:.0} MB, its model server {:.0} MB", after[2], after[3]);

    let [app_limit, reader_limit, helper_limit, searching_limit] = options.limits;
    let checks = [
        ("Excavate at rest", at_rest[0], app_limit),
        ("the reader at rest", at_rest[1], reader_limit),
        ("the helper at rest", helper_rest.max(after[2]), helper_limit),
        ("the helper and its model server while searching", searching, searching_limit),
    ];
    let mut failed = Vec::new();
    for (what, mb, limit) in checks {
        let ok = mb <= limit as f64;
        println!("{} {what}: {mb:.0} MB (limit {limit})", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failed.push(what);
        }
    }
    drop(helper);
    drop(job);
    let report = json!({
        "at_rest_mb": { "excavate": at_rest[0], "reader": at_rest[1], "helper": helper_rest },
        "searching_peak_mb": { "helper": peak[2], "model_server": peak[3] },
        "after_searching_mb": { "helper": after[2], "model_server": after[3] },
        "search_ms_median": median(times),
        "limits_mb": options.limits,
        "over": failed,
    });
    if !failed.is_empty() {
        eprintln!("over the limit: {}", failed.join(", "));
    }
    report
}
