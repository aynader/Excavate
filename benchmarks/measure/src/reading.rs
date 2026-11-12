//! `measure reading`: Excavate's own indexer (the release's `excavate-indexer.exe`) reading a seeded sample of one
//! kind with one choice, from nothing, `--runs` times. Each run has a home of its own (`EXCAVATE_HOME`); the models are
//! shared between runs and downloaded by a short warm-up first, so no run is timed with a download in it.
//!
//! Per run: the time until everything is read, per file, the CPU time and peak memory of the indexer and every model
//! server it starts (a job object holds them), and how late a 10 ms timer fires meanwhile (how the PC stays responsive).

use crate::corpus;
use crate::process::Job;
use crate::stats::{self, Rng, summary};
use config::status::Status;
use config::{Paths, Settings};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct Options {
    pub excavate: PathBuf,
    pub work: PathBuf,
    pub kind: String,
    pub choice: String,
    pub files: usize,
    pub runs: usize,
    pub seed: u64,
    pub hours: f64,
    /// Where the last run's home is kept, for `measure finding`.
    pub keep: Option<PathBuf>,
}

/// The corpus folders each kind reads.
pub fn folders(kind: &str) -> &'static [&'static str] {
    match kind {
        "documents" => &["documents/govdocs"],
        "image-text" => &["images/textvqa-val"],
        "image-shown" => &["images/coco-val2017"],
        // (test-clean: the set word error rates are usually given for. dev-clean is in the corpus as more files.)
        "speech" => &["audio/librispeech-test-clean"],
        "speech-fleurs" => &["audio/fleurs-ar-eg", "audio/fleurs-de-de", "audio/fleurs-en-us"],
        "video-shown" => &["videos/msrvtt-test"],
        // Every kind at once, as a PC holds them, with Excavate's own settings (a test more than a benchmark).
        "mixed" => &["documents/govdocs", "images/textvqa-val", "images/coco-val2017", "audio/librispeech-test-clean", "videos/msrvtt-test"],
        _ => {
            eprintln!("--kind: documents, image-text, image-shown, speech, speech-fleurs, video-shown or mixed");
            std::process::exit(2);
        }
    }
}

fn files_under(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() { files_under(&path, out) } else { out.push(path) }
    }
}

/// The sample: `files` of the kind's files (all of them when that's more), each as its path under the corpus
/// ("images/coco-val2017/000000000139.jpg", as the truth files name it). Spread evenly over the kind's folders.
pub fn sample(kind: &str, files: usize, seed: u64) -> Vec<String> {
    let folders = folders(kind);
    let mut rng = Rng::new(seed);
    let mut picked = Vec::new();
    for (i, folder) in folders.iter().enumerate() {
        let base = corpus::path(folder);
        let mut all = Vec::new();
        files_under(&base, &mut all);
        all.sort();
        let all: Vec<String> = all.iter().filter_map(|path| path.strip_prefix(&base).ok())
            .map(|path| format!("{folder}/{}", path.to_string_lossy().replace('\\', "/"))).collect();
        let share = if files == 0 { all.len() } else { files / folders.len() + usize::from(i < files % folders.len()) };
        picked.extend(rng.sample(&all, share.min(all.len())));
    }
    picked.sort();
    picked
}

/// The sample, copied under `root` with the corpus's own layout (or the corpus itself, when the sample is all of it).
fn place(kind: &str, sample: &[String], root: &Path, whole: bool) -> Vec<String> {
    if whole {
        return folders(kind).iter().map(|folder| corpus::path(folder).to_string_lossy().into_owned()).collect();
    }
    for file in sample {
        let target = root.join(file);
        if !target.exists() {
            std::fs::create_dir_all(target.parent().unwrap()).expect("can't make the sample's folders");
            std::fs::copy(corpus::path(file), &target).expect("can't copy the sample");
        }
    }
    folders(kind).iter().map(|folder| root.join(folder).to_string_lossy().into_owned()).collect()
}

/// What Excavate's settings say for the kind and choice, as if "Read everything now" had just been pressed.
fn settings(kind: &str, choice: &str, folders: Vec<String>) -> Settings {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut s = Settings { setup_done: true, first_run_done: true, folders, exclude: Vec::new(), ..Settings::default() };
    (s.pictures_on, s.seeing_on, s.listening_on, s.videos_on, s.video_text, s.video_speech) = (false, false, false, false, false, false);
    (s.schedule, s.seeing_schedule, s.listen_schedule) = ("arrive".into(), "arrive".into(), "arrive".into());
    (s.read_on_battery, s.look_on_battery) = (true, true);
    (s.read_all_at, s.look_all_at, s.listen_all_at) = (now, now, now);
    match kind {
        "image-text" => { s.pictures_on = true; s.reading = choice.into(); }
        "image-shown" => { s.seeing_on = true; s.seeing = choice.into(); }
        "speech" | "speech-fleurs" => { s.listening_on = true; s.listening = choice.into(); }
        // Videos are looked at by the models chosen for images.
        "video-shown" => { s.seeing_on = true; s.seeing = choice.into(); s.videos_on = true; s.video = choice.into(); }
        // Excavate's own defaults for every kind; or every choice set to `choice` (other than "default").
        "mixed" => {
            let d = Settings::default();
            (s.pictures_on, s.seeing_on, s.listening_on, s.videos_on, s.video_text, s.video_speech) =
                (d.pictures_on, d.seeing_on, d.listening_on, d.videos_on, d.video_text, d.video_speech);
            if choice != "default" {
                (s.reading, s.seeing, s.listening, s.video) = (choice.into(), choice.into(), choice.into(), choice.into());
            }
        }
        _ => {}
    }
    s
}

/// What the kind's work is counted in, done and in all: files read (documents, images), recordings heard, videos
/// looked at. (Recordings and videos aren't files to the indexer.)
fn counted(kind: &str, status: &Status) -> (u64, u64) {
    match kind {
        "speech" | "speech-fleurs" => status.listening.as_ref().map_or((0, 0), |l| (l.done, l.done + l.queued)),
        "video-shown" => status.looking.first().map_or((0, 0), |l| (l.videos_done, l.videos_done + l.videos_queued)),
        // All three, added up.
        "mixed" => [counted("documents", status), counted("speech", status), counted("video-shown", status)]
            .iter().fold((0, 0), |(d, a), (dd, aa)| (d + dd, a + aa)),
        _ => (status.read + status.failed, status.files),
    }
}

/// Nothing left to do of any kind, the kind's work counted, and the indexer saying so.
fn finished(kind: &str, status: &Status) -> bool {
    let (done, all) = counted(kind, status);
    status.state == "idle" && all > 0 && done >= all && status.read + status.failed >= status.files && status.deep_queued == 0
        && status.looking.iter().all(|l| l.queued + l.videos_queued + l.documents_queued == 0)
        && status.listening.as_ref().is_none_or(|l| l.queued + l.videos_queued == 0)
        && status.video_text.as_ref().is_none_or(|v| v.queued == 0)
}

struct Run {
    seconds: f64,
    status: Status,
    cpu: f64,
    peak_mb: f64,
    timer_late_ms: Vec<f64>,
    finished: bool,
    /// How the indexer ended, when it ended by itself.
    ended: Option<String>,
}

/// One reading from nothing in `home`, until finished (held for 10 s) or `limit`.
fn read_once(options: &Options, home: &Path, folders: Vec<String>, limit: Duration) -> Run {
    let _ = std::fs::remove_dir_all(home);
    std::fs::create_dir_all(home.join("local").join("logs")).expect("can't make the home");
    std::fs::create_dir_all(home.join("roaming")).unwrap();
    let models = options.work.join("models");
    std::fs::create_dir_all(&models).unwrap();
    // The models shared, through a junction (no rights needed).
    let windows = |path: &Path| path.to_string_lossy().replace('/', "\\");
    let linked = std::process::Command::new("cmd.exe").args(["/c", "mklink", "/J"]).arg(windows(&home.join("local").join("models")))
        .arg(windows(&models)).stdout(std::process::Stdio::null()).status();
    assert!(linked.is_ok_and(|s| s.success()), "can't link the models folder");
    let paths = Paths { roaming: home.join("roaming"), local: home.join("local") };
    settings(&options.kind, &options.choice, folders).save(&paths);

    let stop = Arc::new(AtomicBool::new(false));
    let timer = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut late = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                std::thread::sleep(Duration::from_millis(10));
                late.push((started.elapsed().as_secs_f64() - 0.010) * 1000.0);
            }
            late
        })
    };
    let job = Job::new();
    let started = Instant::now();
    let mut child = std::process::Command::new(options.excavate.join("excavate-indexer.exe")).env("EXCAVATE_HOME", home)
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
        .expect("can't start excavate-indexer.exe");
    job.add(&child);
    let mut done_at: Option<Instant> = None;
    let mut status = Status::default();
    let mut last_print = Instant::now();
    let mut ended = None;
    while started.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(250));
        // An indexer that ended by itself won't finish: said, not waited for.
        if let Ok(Some(code)) = child.try_wait() {
            ended = Some(code.code().map_or("killed".to_string(), |c| format!("0x{:08X}", c as u32)));
            println!("  the indexer ended by itself: {}", ended.as_deref().unwrap());
            break;
        }
        if let Some(now) = Status::load(&paths) {
            status = now;
        }
        if finished(&options.kind, &status) {
            let at = *done_at.get_or_insert_with(Instant::now);
            if at.elapsed() >= Duration::from_secs(10) {
                break;
            }
        } else {
            done_at = None;
        }
        if last_print.elapsed() > Duration::from_secs(60) {
            last_print = Instant::now();
            let (done, all) = counted(&options.kind, &status);
            println!("  {:.0} min: {done} of {all} done, {}", started.elapsed().as_secs_f64() / 60.0, status.state);
        }
    }
    let finished = done_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(10));
    let seconds = done_at.map_or(started.elapsed(), |at| at - started).as_secs_f64();
    let (cpu, peak_mb) = (job.cpu_seconds(), job.peak_bytes() as f64 / 1_048_576.0);
    stop.store(true, Ordering::Relaxed);
    drop(job);
    let _ = std::fs::copy(paths.log(), home.with_extension("log"));
    Run { seconds, status, cpu, peak_mb, timer_late_ms: timer.join().unwrap(), finished, ended }
}

pub fn run(options: &Options) -> Value {
    let all = sample(&options.kind, 0, options.seed).len();
    let sample = sample(&options.kind, options.files, options.seed);
    let whole = sample.len() == all;
    let root = options.work.join(format!("sample-{}-{}-{}", options.kind, sample.len(), options.seed));
    println!("{}: {} of {all} files ({}), the {} choice, {} runs", options.kind, sample.len(),
             if whole { "all" } else { "a seeded sample" }, options.choice, options.runs);
    let folders = place(&options.kind, &sample, &root, whole);
    let limit = Duration::from_secs_f64(options.hours * 3600.0);

    // The warm-up: the models downloaded (and Windows' caches filled for the programs), not timed.
    let warm_root = options.work.join(format!("warm-{}", options.kind));
    let warm_folders = place(&options.kind, &sample[..sample.len().min(4)], &warm_root, false);
    println!("warm-up (downloads the models the first time)");
    let warm = read_once(options, &options.work.join("home-warm"), warm_folders, limit.min(Duration::from_secs(3600)));
    let _ = std::fs::remove_dir_all(options.work.join("home-warm"));
    let _ = std::fs::remove_dir_all(&warm_root);
    if !warm.finished {
        eprintln!("the warm-up didn't finish: {:?}", warm.status.errors);
        if warm.ended.is_some() {
            return json!({ "kind": options.kind, "choice": options.choice, "indexer_ended": warm.ended });
        }
    }

    let mut runs = Vec::new();
    for run in 0..options.runs {
        let home = options.work.join(format!("home-{}-{}", options.kind, run));
        let read = read_once(options, &home, folders.clone(), limit);
        let (done, all) = counted(&options.kind, &read.status);
        println!("run {}/{}: {:.1} s, {done} of {all} done ({} failed), {:.1} s of CPU, peak {:.0} MB{}", run + 1, options.runs, read.seconds,
                 read.status.failed, read.cpu, read.peak_mb, if read.finished { "" } else { ", NOT FINISHED" });
        let last = run + 1 == options.runs;
        if last && let Some(keep) = &options.keep {
            let _ = std::fs::remove_dir_all(keep);
            // (Until the ended indexer's files are let go.)
            let kept = (0..40).any(|_| std::fs::rename(&home, keep).is_ok() || { std::thread::sleep(Duration::from_millis(500)); false });
            if !kept {
                eprintln!("couldn't keep the home as {}", keep.display());
            } else {
                let list = json!({ "kind": options.kind, "corpus": corpus::parts(), "files": sample, "folders": folders });
                let _ = std::fs::write(keep.join("sample.json"), serde_json::to_vec_pretty(&list).unwrap());
            }
        } else {
            let _ = std::fs::remove_dir_all(&home);
        }
        runs.push(read);
    }
    if !whole {
        let _ = std::fs::remove_dir_all(&root);
    }
    let each = |f: &dyn Fn(&Run) -> f64| -> Vec<f64> { runs.iter().map(f).collect() };
    let late: Vec<f64> = runs.iter().flat_map(|r| r.timer_late_ms.iter().cloned()).collect();
    json!({
        "kind": options.kind, "choice": options.choice, "files": sample.len(), "of": all, "seed": options.seed,
        "runs": runs.len(), "finished_runs": runs.iter().filter(|r| r.finished).count(),
        "engine": runs.last().map(|r| r.status.engine.clone()),
        "seconds": summary(&each(&|r| r.seconds)),
        // Per file Excavate reads (a sample of documents holds some types it leaves alone), recording heard, or video
        // looked at.
        "files_read": summary(&each(&|r| counted(&options.kind, &r.status).1 as f64)),
        "ms_per_file": summary(&each(&|r| r.seconds * 1000.0 / counted(&options.kind, &r.status).1.max(1) as f64)),
        "cpu_seconds": summary(&each(&|r| r.cpu)),
        "peak_memory_mb": summary(&each(&|r| r.peak_mb)),
        "failed_files": summary(&each(&|r| r.status.failed as f64)),
        "timer_late_ms": { "p50": stats::round(stats::median(&late)), "p99": stats::round(stats::quantile(&late, 0.99)),
                           "max": stats::round(stats::quantile(&late, 1.0)) },
        "indexer_ended": runs.iter().filter_map(|r| r.ended.clone()).collect::<Vec<_>>(),
        "errors": runs.last().map(|r| r.status.errors.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>()),
    })
}
