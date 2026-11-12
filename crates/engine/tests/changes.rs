//! Files added or changed while Excavate runs (the folder watches): they get their dates made and opened and their
//! attributes, as a listing gives them, and an attribute change (read-only here) reaches the index.
use engine::names::{Names, Phase};
use std::time::{Duration, Instant};

fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(30), "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn watched_changes_keep_dates_and_attributes() {
    let dir = std::env::temp_dir().join(format!("engine-changes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inside")).unwrap();
    let file = dir.with_extension("idx");
    let _ = std::fs::remove_file(&file);
    let names = Names::start_over(file.clone(), Some(vec![dir.display().to_string()]));
    wait("live", || names.state().phase == Phase::Live);

    let added = dir.join("inside").join("q7vz-added.txt");
    std::fs::write(&added, b"x").unwrap();
    let entry = || names.read(|index| engine::search::Search::new("q7vz-added").run(index, None).first().copied());
    wait("the new file is found", || entry().is_some());
    let e = entry().unwrap();
    let more = names.read(|index| index.more(e));
    assert!(more.created > 0 && more.accessed > 0, "dates made and opened: {more:?}");
    assert!(more.attributes & 0x20 != 0, "archive, as Windows sets it: {more:?}");

    let mut permissions = std::fs::metadata(&added).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&added, permissions.clone()).unwrap();
    wait("read-only reaches the index", || names.read(|index| index.more(e).attributes & 0x1 != 0));

    permissions.set_readonly(false);
    std::fs::set_permissions(&added, permissions).unwrap();
    names.stop();
    wait("stopped", || names.stopped());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(file.with_extension("journal"));
}
