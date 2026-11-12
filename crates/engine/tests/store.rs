use engine::search::Search;
use engine::store::{Index, Meta, DIR, NONE};

fn meta(flags: u8) -> Meta {
    Meta { size: Some(1), modified: 0, flags }
}

#[test]
fn rename_case_only() {
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    let a = index.push(root, "before.txt", meta(0));
    let b = index.push(root, "case only.txt", meta(0));
    let c = index.push(root, "after.txt", meta(0));
    assert_eq!(index.child(root, "case only.txt"), Some(b));
    let new = index.rename(b, root, "CASE ONLY.txt");
    assert_eq!(index.child(root, "CASE ONLY.txt"), Some(new));
    assert_eq!(index.child(root, "before.txt"), Some(a));
    assert_eq!(index.child(root, "after.txt"), Some(c));
    assert_eq!(Search::new("case only").run(&index, None), vec![new]);
}

#[test]
fn folder_rename_keeps_children() {
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    let folder = index.push(root, "moving", meta(DIR));
    let file = index.push(folder, "inside.txt", meta(0));
    index.rename(folder, root, "moved");
    assert_eq!(index.path(file), r"C:\root\moved\inside.txt");
    index.remove(index.child(root, "moved").unwrap());
    assert!(index.gone(file));
    index.settle();
    assert!(Search::new("inside").run(&index, None).is_empty());
}

#[test]
fn folding() {
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    let nfc = index.push(root, "café.txt", meta(0));
    let nfd = index.push(root, "cafe\u{301}.txt", meta(0));
    let plain = index.push(root, "CAFE.txt", meta(0));
    assert_eq!(Search::new("cafe").run(&index, None), vec![nfc, nfd, plain]);
    assert_eq!(Search::new("CAFÉ").run(&index, None), vec![nfc, nfd, plain]);
    let big = index.push(root, "big.bin", Meta { size: Some(3 << 40), modified: 0, flags: 0 });
    assert_eq!(index.size(big), Some(3 << 40));
    let old = index.push(root, "old.txt", Meta { size: Some(1), modified: -2_207_520_000, flags: 0 });
    let far = index.push(root, "far.txt", Meta { size: Some(1), modified: 7_253_280_000, flags: 0 });
    assert_eq!(index.modified(old), -2_207_520_000);
    assert_eq!(index.modified(far), 7_253_280_000);
}

#[test]
fn same_name_twice_in_a_row() {
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    let one = index.push(root, "x", meta(DIR));
    let two = index.push(root, "y", meta(DIR));
    let a = index.push(one, "0", meta(0));
    let b = index.push(two, "0", meta(0));
    assert_eq!(index.child(one, "0"), Some(a));
    assert_eq!(index.child(two, "0"), Some(b));
}

#[test]
fn dupes_and_depths() {
    use engine::search::Search;
    use engine::store::{Index, Meta, DIR, NONE};
    let mut index = Index::new();
    let root = index.push(NONE, "C:", Meta { size: None, modified: 0, flags: DIR });
    let a = index.push(root, "a", Meta { size: None, modified: 0, flags: DIR });
    let x1 = index.push(root, "x.txt", Meta { size: Some(1), modified: 0, flags: 0 });
    let x2 = index.push(a, "X.TXT", Meta { size: Some(2), modified: 0, flags: 0 });
    let y = index.push(a, "y.txt", Meta { size: Some(1), modified: 0, flags: 0 });
    assert_eq!(Search::new("dupe:").run(&index, None), vec![x1, x2], "unsorted");
    index.sort_names();
    assert_eq!(Search::new("dupe:").run(&index, None), vec![x1, x2], "sorted");
    assert_eq!(Search::new("sizedupe:").run(&index, None), vec![x1, y]);
    assert_eq!(Search::new("depth:2").run(&index, None), vec![x2, y]);
    assert_eq!(Search::new("empty:").run(&index, None), Vec::<u32>::new());
    assert_eq!(Search::new("childfilecount:2").run(&index, None), vec![a]);
    assert_eq!(Search::new("child:y").run(&index, None), vec![a]);
}

/// The index mapped from its file: changes go to the file, a flush makes it whole and clean, it opens again as it
/// was; one that outgrew its room in the file is written anew; one left half-changed isn't trusted.
#[test]
fn mapped_from_its_file() {
    let folder = std::env::temp_dir().join(format!("excavate-mapped-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("names.idx");
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    let docs = index.push(root, "docs", meta(DIR));
    for i in 0..2000 {
        index.push(docs, &format!("report {i}.pdf"), Meta { size: Some(i), modified: 1_700_000_000 + i as i64, flags: 0 });
    }
    index.push(root, "Café.txt", meta(0));
    index.build_table();
    index.sort_names();
    index.build_table();
    index.save(&file).unwrap();

    let mut mapped = Index::open(&file).unwrap();
    assert!(mapped.mapped());
    assert_eq!(mapped.len(), index.len());
    assert_eq!(Search::new("report 1999").run(&mapped, None), Search::new("report 1999").run(&index, None));
    // Changed as the copy in memory is, then flushed.
    for copy in [&mut index, &mut mapped] {
        let gone = copy.child(docs, "report 5.pdf").unwrap();
        copy.remove(gone);
        let moved = copy.child(docs, "report 6.pdf").unwrap();
        copy.rename(moved, root, "moved report.pdf");
        copy.set_facts(copy.child(docs, "report 7.pdf").unwrap(), Some(99), 1_800_000_000);
        copy.push(docs, "new one.txt", meta(0));
        copy.push(docs, "nouvelle façade.txt", meta(0));
    }
    assert!(mapped.mapped(), "small changes fit in the room left");
    assert!(mapped.flush().unwrap());
    drop(mapped);
    let again = Index::open(&file).unwrap();
    for query in ["report", "moved", "new one", "facade", "cafe", "size:99"] {
        assert_eq!(Search::new(query).run(&again, None), Search::new(query).run(&index, None), "{query}");
    }
    assert_eq!(again.child(docs, "report 5.pdf"), None);
    assert_eq!(again.dead, index.dead);

    // Changed and not flushed, Excavate gone between two changes, Windows still running: what it wrote is trusted.
    let mut changed = again;
    changed.push(docs, "unsaved.txt", meta(0));
    drop(changed);
    let trusted = Index::open(&file).unwrap();
    assert_eq!(Search::new("unsaved").run(&trusted, None).len(), 1);
    drop(trusted);
    // Gone in the middle of a change (its busy mark still on): not trusted.
    let mut bytes = std::fs::read(&file).unwrap();
    bytes[9] = 1;
    std::fs::write(&file, &bytes).unwrap();
    assert!(Index::open(&file).is_err());

    // Outgrowing the room: written anew, then mapped again.
    let grown = Index::load(&file.with_extension("missing")).unwrap_or_else(|_| index.compact().0);
    grown.save(&file).unwrap();
    let mut grown = Index::open(&file).unwrap();
    let started = std::time::Instant::now();
    for i in 0..70_000 {
        grown.push(docs, &format!("more {i}.log"), meta(0));
    }
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    assert!(!grown.mapped());
    assert!(!grown.flush().unwrap());
    let fresh = grown.write_new(&file).unwrap();
    let reopened = Index::open(&fresh).unwrap();
    drop(grown);
    std::fs::rename(&fresh, &file).unwrap();
    assert!(reopened.mapped());
    assert_eq!(Search::new("more 69999").run(&reopened, None).len(), 1);
    drop(reopened);
    let _ = std::fs::remove_dir_all(&folder);
}

/// More of Everything's functions: count:, sort:, type:, filelist:, character escapes, the other dupes, dates made and opened, every
/// attribute, shell:.
#[test]
fn accents_when_they_matter() {
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    for name in ["Résumé.pdf", "resume.txt", "RÉSUMÉ old.doc", "Café.md"] {
        index.push(root, name, Meta { size: Some(1), modified: 1_700_000_000, flags: 0 });
    }
    let found = |q: &str| -> Vec<String> {
        let mut names: Vec<String> = Search::new(q).run(&index, None).iter().map(|&e| index.name(e).to_string()).collect();
        names.sort();
        names
    };
    // Without it, accents and case don't matter (as Everything).
    assert_eq!(found("resume").len(), 3);
    // With it, accents do and case still doesn't.
    assert_eq!(found("diacritics:résumé"), ["RÉSUMÉ old.doc", "Résumé.pdf"]);
    assert_eq!(found("diacritics:resume"), ["resume.txt"]);
    assert_eq!(found("diacritics:case:Résumé"), ["Résumé.pdf"]);
    assert_eq!(found("nodiacritics:resume").len(), 3);
    assert_eq!(found("diacritics:caf*").len(), 1, "a wildcard keeps folding (only words and exact names check accents)");
}

#[test]
fn w9_functions() {
    use engine::store::More;
    let mut index = Index::new();
    let root = index.push(NONE, r"C:\root", meta(DIR));
    let day = 86_400i64;
    let base = 1_700_000_000i64;
    let files = [("a.txt", 10u64, 1), ("b.txt", 30, 2), ("c.png", 20, 3), ("a.png", 20, 4), ("say \"hi\".txt", 5, 5), ("it's.txt", 7, 6)];
    let mut ids = Vec::new();
    for (name, size, k) in files {
        let e = index.push(root, name, Meta { size: Some(size), modified: base + k * day, flags: 0 });
        index.set_more(e, More { created: base - k * day, accessed: base + 100 * day, attributes: if k % 2 == 0 { 0x21 } else { 0x20 } });
        ids.push(e);
    }
    let found = |q: &str| Search::new(q).run(&index, None);
    let names = |q: &str| -> Vec<String> {
        let search = Search::new(q);
        let (sort, descending) = search.asked_order().unwrap_or((engine::search::Sort::Name, false));
        search.page(&index, None, sort, descending, 0, 100).page.iter().map(|&e| index.name(e).to_string()).collect()
    };
    assert_eq!(names("count:2 .txt").len(), 2);
    assert_eq!(Search::new("count:2 .txt").page(&index, None, engine::search::Sort::Name, false, 0, 100).count, 2);
    assert_eq!(names("sort:size .png|a.txt"), vec!["c.png", "a.png", "a.txt"], "largest first (the whole order turned round)");
    assert_eq!(names("sort:size-ascending .txt").first().map(String::as_str), Some("say \"hi\".txt"));
    assert_eq!(names("sort:size-descending .txt").first().map(String::as_str), Some("b.txt"));
    assert_eq!(names("sort:dc .txt").first().map(String::as_str), Some("a.txt"), "newest made first");
    assert_eq!(found("filelist:a.txt|c.png").len(), 2);
    assert_eq!(found("quot:hi").len(), 1);
    assert_eq!(found("it apos:s").len(), 1);
    assert_eq!(found("#39:s").len(), 1);
    assert_eq!(found("namepartdupe:").len(), 2, "a.txt and a.png");
    assert_eq!(found("dadupe:").len(), 6);
    assert_eq!(found("dc:2023").len(), 6);
    assert_eq!(found("dc:2024").len(), 0);
    assert_eq!(found("da:2024").len(), 6);
    assert_eq!(found("dc:2023 attrib:r").len(), 3);
    assert_eq!(found("attrib:r").len(), 3);
    assert_eq!(found("attrib:ra").len(), 3);
    assert_eq!(found("attrib:a").len(), 6);
    assert!(found("type:png").len() >= 2, "Windows names .png something with PNG in it");
    assert!(engine::known::folder("downloads").is_some_and(|path| path.contains(':')));
    assert!(Search::new("shell:nonsense").unsupported.len() == 1);
}

/// Sorted names are found by halving, with no table of them: each name once, whatever its case or
/// accents, and names added after the sort through the small table of names since.
#[test]
fn names_found_by_halving() {
    let mut index = Index::new();
    let root = index.push(NONE, "C:", meta(DIR));
    let names = ["b.txt", "B.txt", "Café.txt", "CAFÉ.txt", "cafe.txt", "a", "Z", "ä", "Ä", "zz", "A b c"];
    for name in names {
        index.push(root, name, meta(0));
    }
    index.sort_names();
    let count = index.name_count();
    for name in names {
        let id = index.id_of(name.as_bytes()).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(index.id_bytes(id), name.as_bytes());
        index.push(root, name, meta(0));
    }
    assert_eq!(index.name_count(), count, "no name twice");
    assert_eq!(index.id_of(b"missing"), None);
    assert_eq!(index.id_of(b"CAFE.txt"), None);
    index.push(root, "new.txt", meta(0));
    index.push(root, "new.txt", meta(0));
    assert_eq!(index.name_count(), count + 1);
    assert!(index.id_of(b"new.txt").is_some());
}

/// The index built by the helper Windows asks permission for (`build_from_tables`): without those rights (as here) a
/// folder is listed instead of read from the MFT; either way it's saved with its journal note, and opens as it was.
#[test]
fn built_from_tables_opens() {
    let folder = std::env::temp_dir().join(format!("excavate-tables-{}", std::process::id()));
    let tree = folder.join("tree");
    std::fs::create_dir_all(tree.join("a")).unwrap();
    std::fs::write(tree.join("a").join("Café.txt"), "x").unwrap();
    std::fs::write(tree.join("notes.md"), "y").unwrap();
    let file = folder.join("out").join("names.idx");
    let root = tree.to_string_lossy().to_string();
    let built = engine::names::build_from_tables(&file, Some(vec![root.clone()])).unwrap();
    assert_eq!(built.walked, vec![root.clone()]);
    assert_eq!(built.entries, 4);
    let opened = Index::open(&file).unwrap();
    assert_eq!(Search::new("cafe").run(&opened, None).len(), 1);
    assert_eq!(Search::new("notes").run(&opened, None).len(), 1);
    assert!(engine::journal::load(&file).contains_key(&root));
    drop(opened);
    let _ = std::fs::remove_dir_all(&folder);
}
