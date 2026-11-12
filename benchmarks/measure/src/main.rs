//! The benchmarks on this page's machine, each written to a JSON file (and summed up as it goes):
//!
//! ```text
//! measure names   [--runs 5] [--repeat 30] [--changes 10000] [--idle 60] [--queries names-queries.txt]
//!                 [--everything <Everything.exe> --everything-db <Everything.db>] [--out names.json]
//! measure reading --excavate <release folder> --kind <kind> --choice quick|balanced|deep [--files 300] [--runs 5]
//!                 [--seed 1] [--keep <home>] [--out reading.json]
//! measure finding --home <home kept by reading> --excavate <release folder> [--queries 1000] [--seed 1] [--out finding.json]
//! ```
//!
//! Kinds: documents, image-text, image-shown, speech, speech-fleurs, video-shown. The corpus: `--data <folder>` as
//! `make_corpus.py` makes it, or each part on its own with `--documents`, `--images`, `--audio`, `--videos`, `--truth`.
//! `names` needs Everything running, and administrator rights to build Bedrock from the drives' tables as Excavate
//! does when allowed (without them it lists folders, as Excavate does when it isn't).

mod corpus;
mod finding;
mod names;
mod process;
mod reading;
mod stats;

use std::collections::HashMap;
use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("{}", include_str!("main.rs").lines().take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")
        .strip_prefix(' ').unwrap_or("")).filter(|l| !l.starts_with("```")).collect::<Vec<_>>().join("\n"));
    std::process::exit(2);
}

/// This program's input and output aren't handed down: a program it starts and leaves running (Everything) would
/// otherwise hold them open, and whoever reads this program's output would wait for ever.
fn keep_output_to_itself() {
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain calls on this process's own standard handles; a missing one is refused, and that's fine.
        unsafe { SetHandleInformation(GetStdHandle(which), HANDLE_FLAG_INHERIT, 0) };
    }
}

fn main() {
    keep_output_to_itself();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else { usage() };
    if command == "churn" {
        names::churn(&PathBuf::from(&args[1]), args[2].parse().unwrap());
        return;
    }
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let Some(name) = flag.strip_prefix("--") else { usage() };
        flags.insert(name.to_string(), rest.next().cloned().unwrap_or_else(|| usage()));
    }
    let text = |name: &str, default: &str| flags.get(name).cloned().unwrap_or_else(|| default.to_string());
    let number = |name: &str, default: usize| flags.get(name).map_or(default, |v| v.parse().unwrap_or_else(|_| usage()));
    let path = |name: &str| flags.get(name).map(PathBuf::from).unwrap_or_else(|| { eprintln!("--{name} is needed"); usage() });
    let here = std::env::current_dir().unwrap();
    let data = flags.get("data").map(PathBuf::from);
    corpus::set(corpus::NAMES.iter().filter_map(|&part| {
        flags.get(part).map(PathBuf::from).or_else(|| data.as_ref().map(|d| d.join(part))).map(|p| (part.to_string(), p))
    }).collect());

    let work = flags.get("work").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("measure"));
    let report = match command.as_str() {
        "names" => names::run(&names::Options {
            index: work.join("names.idx"),
            queries: flags.get("queries").map(PathBuf::from).unwrap_or_else(|| here.join("names-queries.txt")),
            scratch: work.join("scratch"),
            runs: number("runs", 5),
            repeat: number("repeat", 30),
            changes: number("changes", 10_000),
            idle_seconds: number("idle", 60) as u64,
            everything: flags.get("everything").map(|exe| (PathBuf::from(exe), path("everything-db"))),
        }),
        "reading" => reading::run(&reading::Options {
            excavate: path("excavate"),
            work: work.clone(),
            kind: text("kind", ""),
            choice: text("choice", "balanced"),
            files: number("files", 300),
            runs: number("runs", 5),
            seed: number("seed", 1) as u64,
            hours: flags.get("hours").map_or(24.0, |h| h.parse().unwrap_or(24.0)),
            keep: flags.get("keep").map(PathBuf::from),
        }),
        "finding" => finding::run(&finding::Options {
            home: path("home"),
            excavate: path("excavate"),
            queries: number("queries", 1000),
            seed: number("seed", 1) as u64,
        }),
        _ => usage(),
    };
    let report = serde_json::json!({
        "measured": command,
        "at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
        "machine": { "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
                     "processor": std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default() },
        "result": report,
    });
    let out = flags.get("out").map(PathBuf::from).unwrap_or_else(|| here.join(format!("{command}.json")));
    std::fs::write(&out, serde_json::to_vec_pretty(&report).unwrap()).expect("can't write the result");
    println!("written: {}", out.display());
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // Ended at once: after Everything's SDK has been used, a normal exit can hang (seen in the test machine).
    winsys::process::exit_now(0);
}
