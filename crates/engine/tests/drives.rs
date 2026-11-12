//! The saved index when the drives differ at the next start: a drive that's gone leaves it, a new one is
//! listed, and those it has are caught up, not listed from scratch. Folders stand in for drives.
use engine::names::{Names, Phase};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("engine-drives-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inside")).unwrap();
    std::fs::write(dir.join("inside").join(format!("{name}-file.txt")), b"x").unwrap();
    dir
}

fn live(file: &Path, drives: &[&PathBuf]) -> std::sync::Arc<Names> {
    let names = Names::start_over(file.to_path_buf(), Some(drives.iter().map(|d| d.display().to_string()).collect()));
    let started = Instant::now();
    while names.state().phase != Phase::Live {
        assert!(started.elapsed() < Duration::from_secs(60), "not live: {:?}", names.state().phase);
        std::thread::sleep(Duration::from_millis(20));
    }
    names
}

fn stop(names: std::sync::Arc<Names>) {
    names.stop();
    let started = Instant::now();
    while !names.stopped() {
        assert!(started.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn finds(names: &Names, word: &str) -> usize {
    names.read(|index| engine::search::Search::new(word).run(index, None).len())
}

#[test]
fn drives_that_differ() {
    let (a, b, c) = (folder("alpha"), folder("bravo"), folder("charlie"));
    let file = std::env::temp_dir().join(format!("engine-drives-{}.idx", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let names = live(&file, &[&a, &b]);
    assert_eq!((finds(&names, "alpha-file"), finds(&names, "bravo-file")), (1, 1));
    stop(names);
    let names = live(&file, &[&a, &c]);
    let state = names.state();
    assert!(state.loaded_in.is_some() && state.built_in.is_none(), "the saved index was used: {state:?}");
    assert_eq!((finds(&names, "alpha-file"), finds(&names, "bravo-file"), finds(&names, "charlie-file")), (1, 0, 1));
    stop(names);
    for dir in [a, b, c] {
        let _ = std::fs::remove_dir_all(dir);
    }
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(file.with_extension("journal"));
}
