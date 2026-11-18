//! A first build stopped partway (Excavate quit while it listed): what was listed is kept, found at the next start,
//! and that start lists the rest.
use engine::names::{Names, Phase};
use std::time::{Duration, Instant};

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(120), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_build_cut_short_is_kept() {
    let dir = std::env::temp_dir().join(format!("engine-cut-short-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Enough folders that the first listing is stopped partway.
    for a in 0..60 {
        for b in 0..20 {
            let folder = dir.join(format!("a{a}")).join(format!("b{b}"));
            std::fs::create_dir_all(&folder).unwrap();
            for c in 0..8 {
                std::fs::write(folder.join(format!("k3q-{a}-{b}-{c}.txt")), b"x").unwrap();
            }
        }
    }
    let total = 60 * 20 * 8;
    let file = dir.with_extension("idx");
    let _ = std::fs::remove_file(&file);
    let drives = Some(vec![dir.display().to_string()]);

    let names = Names::start_over(file.clone(), drives.clone());
    wait("building", || names.state().phase == Phase::Building);
    std::thread::sleep(Duration::from_millis(30));
    names.stop();
    wait("stopped", || names.stopped());
    let kept = file.exists();
    let first = names.read(|index| engine::search::Search::new("k3q-").run(index, None).len());

    eprintln!("kept: {kept}, {first} of {total} files listed before the stop");
    // The next start: what was kept answers at once, and the rest is listed.
    let again = Names::start_over(file.clone(), drives);
    wait("answering", || again.ready());
    if kept && first < total {
        assert!(again.state().phase != Phase::Building, "the kept part answered at once");
    }
    wait("live", || again.state().phase == Phase::Live);
    let found = again.read(|index| engine::search::Search::new("k3q-").run(index, None).len());
    assert_eq!(found, total, "every file once the rest was listed (kept: {kept}, {first} before)");
    again.stop();
    wait("stopped again", || again.stopped());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(file.with_extension("journal"));
}
