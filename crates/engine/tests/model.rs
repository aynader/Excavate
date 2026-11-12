//! The index against a plain model: random adds, deletes, renames, moves, size changes, dates made and opened and
//! attributes (v6's codes: `coded`), compaction, saving and loading, then every path, every (folder, name) lookup, every
//! entry's times, and searches and sorted pages compared with a naive answer.

use engine::search::{Search, Sort};
use engine::store::{fold, Index, Meta, More, DEAD, DIR, NONE};
use proptest::prelude::*;

const NAMES: &[&str] = &[
    "a", "A2", "ab", "Ab c", "café", "cafe\u{301}x", "CAFE", "σίσυφος", "ΣΊΣΥΦΟΣ2", "İstanbul", "istanbul2", "ß", "SS",
    "x.txt", "Y.TXT", "e", "é", "日本", "🦀", "👩\u{200d}💻", "ﬁle", "file", "report final", "final report", "שלום",
    "x", "0", "0 0", "[b]", "100%", "a.b.c", "z", "q.extensionover16chars", "a_b",
];

#[derive(Clone, Debug)]
enum Op {
    Add { parent: usize, name: usize, dir: bool, size: u64 },
    Remove(usize),
    Rename { node: usize, parent: usize, name: usize },
    Size { node: usize, size: u64 },
    /// New dates made and opened, and attributes: each the same as another time, not known, or its own.
    Times { node: usize, made: u8, opened: u8, attributes: u16 },
    Settle,
    Compact,
    SaveLoad,
    RankNames,
    RankPaths,
    Table,
    RankExts,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (any::<usize>(), 0..NAMES.len(), any::<bool>(), 0u64..5).prop_map(|(parent, name, dir, size)| Op::Add { parent, name, dir, size }),
        2 => any::<usize>().prop_map(Op::Remove),
        3 => (any::<usize>(), any::<usize>(), 0..NAMES.len()).prop_map(|(node, parent, name)| Op::Rename { node, parent, name }),
        2 => (any::<usize>(), 0u64..5).prop_map(|(node, size)| Op::Size { node, size }),
        2 => (any::<usize>(), 0u8..4, 0u8..4, prop_oneof![Just(0x20u16), Just(0x21), Just(0x2022), any::<u16>()])
            .prop_map(|(node, made, opened, attributes)| Op::Times { node, made, opened, attributes }),
        1 => Just(Op::Settle),
        1 => Just(Op::Compact),
        1 => Just(Op::SaveLoad),
        1 => Just(Op::RankNames),
        1 => Just(Op::RankPaths),
        1 => Just(Op::Table),
        1 => Just(Op::RankExts),
    ]
}

static FILES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct Node {
    name: String,
    parent: Option<usize>,
    dir: bool,
    alive: bool,
    size: u64,
    entry: u32,
    /// Its date modified as the index has it, and what `more` should say.
    modified: i64,
    more: More,
}

struct Model {
    nodes: Vec<Node>,
}

impl Model {
    fn live(&self) -> Vec<usize> {
        (0..self.nodes.len()).filter(|&n| self.nodes[n].alive).collect()
    }
    fn live_dirs(&self) -> Vec<usize> {
        self.live().into_iter().filter(|&n| self.nodes[n].dir).collect()
    }
    fn inside(&self, mut n: usize, root: usize) -> bool {
        while let Some(p) = self.nodes[n].parent {
            if p == root {
                return true;
            }
            n = p;
        }
        false
    }
    fn taken(&self, parent: usize, name: &str, except: Option<usize>) -> bool {
        self.live().into_iter().any(|n| {
            Some(n) != except && self.nodes[n].parent == Some(parent) && self.nodes[n].name.to_lowercase() == name.to_lowercase()
        })
    }
    fn path(&self, n: usize) -> String {
        let mut parts = vec![self.nodes[n].name.clone()];
        let mut at = self.nodes[n].parent;
        while let Some(p) = at {
            parts.push(self.nodes[p].name.clone());
            at = self.nodes[p].parent;
        }
        parts.reverse();
        parts.join("\\")
    }
    /// The (folded name, name, entry) of each level from the top, the order paths sort in.
    fn path_key(&self, n: usize) -> Vec<(String, String, u32)> {
        let mut chain = vec![n];
        let mut at = self.nodes[n].parent;
        while let Some(p) = at {
            chain.push(p);
            at = self.nodes[p].parent;
        }
        chain.iter().rev().map(|&c| (fold(&self.nodes[c].name), self.nodes[c].name.clone(), self.nodes[c].entry)).collect()
    }
}

/// An independent wildcard match on characters.
fn naive_glob(p: &[char], t: &[char]) -> bool {
    match (p.first(), t.first()) {
        (None, None) => true,
        (Some('*'), _) => naive_glob(&p[1..], t) || (!t.is_empty() && naive_glob(p, &t[1..])),
        (Some('?'), Some(_)) => naive_glob(&p[1..], &t[1..]),
        (Some(a), Some(b)) if a == b => naive_glob(&p[1..], &t[1..]),
        _ => false,
    }
}

/// Whether a whole word `w` is in `text` (bounded by what isn't a letter or digit).
fn naive_word(text: &str, w: &str) -> bool {
    text.match_indices(w).any(|(at, _)| {
        let before = text[..at].chars().next_back().map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
        let after = text[at + w.len()..].chars().next().map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
        before && after
    })
}

/// A count as typed after `depth:` and the like: "2", ">1", "<=3", "1..2".
fn range(w: &str) -> (u64, u64) {
    if let Some((a, b)) = w.split_once("..") {
        return (a.parse().unwrap(), b.parse().unwrap());
    }
    for (op, f) in [(">=", 0), ("<=", 1), (">", 2), ("<", 3)] {
        if let Some(n) = w.strip_prefix(op) {
            let n: u64 = n.parse().unwrap();
            return match f { 0 => (n, u64::MAX), 1 => (0, n), 2 => (n + 1, u64::MAX), _ => (0, n.saturating_sub(1)) };
        }
    }
    let n = w.parse().unwrap();
    (n, n)
}

fn naive_match(model: &Model, n: usize, query: &str) -> bool {
    let name = fold(&model.nodes[n].name);
    let node = &model.nodes[n];
    query.split_whitespace().all(|word| {
        let (not, word) = match word.strip_prefix('!') { Some(w) => (true, w), None => (false, word) };
        let functions = |word: &str| -> Option<bool> {
            Some(match word.split_once(':')? {
                ("file", "") => !node.dir,
                ("folder", "") => node.dir,
                ("startwith", w) => name.starts_with(&fold(w)),
                ("endwith", w) => name.ends_with(&fold(w)),
                ("wfn", w) => name == fold(w),
                ("case", w) => node.name.contains(w),
                ("ww", w) => naive_word(&name, &fold(w)),
                // As Everything: no parent folder (the drive).
                ("root", "") => node.parent.is_none(),
                ("depth", w) | ("parents", w) => {
                    let (lo, hi) = range(w);
                    let mut depth = 0u64;
                    let mut at = node.parent;
                    while let Some(p) = at {
                        depth += 1;
                        at = model.nodes[p].parent;
                    }
                    (lo..=hi).contains(&depth)
                }
                ("childcount", w) | ("childfilecount", w) | ("childfoldercount", w) => {
                    let (lo, hi) = range(w);
                    let kind = word.split(':').next().unwrap();
                    let count = model.live().into_iter().filter(|&m| model.nodes[m].parent == Some(n))
                        .filter(|&m| match kind { "childfilecount" => !model.nodes[m].dir, "childfoldercount" => model.nodes[m].dir, _ => true })
                        .count() as u64;
                    node.dir && (lo..=hi).contains(&count)
                }
                ("empty", "") => node.dir && !model.live().into_iter().any(|m| model.nodes[m].parent == Some(n)),
                ("dupe", "") => model.live().into_iter().filter(|&m| fold(&model.nodes[m].name) == name).count() > 1,
                ("child", w) => {
                    let w = fold(w);
                    node.dir && model.live().into_iter().any(|m| model.nodes[m].parent == Some(n) && fold(&model.nodes[m].name).contains(&w))
                }
                ("size", w) => {
                    let (lo, hi) = engine::syntax::sizes(w).unwrap();
                    !node.dir && (lo..=hi).contains(&node.size)
                }
                _ => return None,
            })
        };
        if let Some(yes) = functions(word) {
            return yes != not;
        }
        let ext: String;
        let word = match word.strip_prefix("ext:") {
            Some(list) => { ext = list.split(';').map(|e| format!("*.{e}")).collect::<Vec<_>>().join("|"); ext.as_str() }
            None => word,
        };
        let any = if let Some(pattern) = word.strip_prefix("regex:") {
            regex::RegexBuilder::new(pattern).case_insensitive(true).build().unwrap().is_match(&model.nodes[n].name)
        } else {
            word.split('|').filter(|w| !w.is_empty()).any(|w| {
                if w.len() >= 3 && w.as_bytes()[1] == b':' && w.contains('\\') {
                    // Inside that folder (not the folder itself).
                    let folder = w.trim_end_matches('\\').to_lowercase();
                    model.path(n).to_lowercase().starts_with(&format!("{folder}\\"))
                } else if w.contains('\\') {
                    fold(&model.path(n)).contains(&fold(w))
                } else {
                    let w = fold(w);
                    if w.contains('*') || w.contains('?') {
                        naive_glob(&w.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
                    } else {
                        name.contains(&w)
                    }
                }
            })
        };
        any != not
    })
}

const QUERIES: &[&str] = &[
    "a", "A", "cafe", "café", "CAFÉ", "σίσυφος", "ΣΊΣΥΦΟΣ", "istanbul", "ß", "ss", "txt", "*.txt", "?", "??", "*", "e",
    "é", "日", "🦀", "file", "ﬁ", "report final", "final !report", "a|z", "!a", "x*", "*2", "0 0", "[b]", "%", "a.b",
    "ab|cafe !x", "שלום", "💻", "regex:^a", "regex:x$", r"regex:^\d", "regex:.", r"\a", r"a\", r"c:\a\", r"C:\ab\ e",
    r"C:\x\a\", r"\ab c\", "ext:txt;c", "regex:é", "regex:^[^a]*$", "regex:$", "regex:^", "regex:", r"!\a",
    "startwith:a", "startwith:cafe", "endwith:txt", "endwith:2", "wfn:x", "wfn:CAFE", "file: a", "folder: a", "folder: !e",
    "case:A", "case:a", "case:Ab", "ww:a", "ww:0", "ww:report", "size:>2", "size:1..3", "size:0 file:", "ext:txt;zz;c",
    "ext:zz", "root:", "root: !a", "!ext:txt", "file: ext:c",
    "depth:1", "depth:>1", "depth:0", "parents:2", "childcount:0", "childcount:>1", "childfilecount:1", "childfoldercount:>=1",
    "empty:", "dupe:", "dupe: !e", "child:a", "child:txt file:", "ext:extensionover16chars", "ext:txt;extensionover16chars",
];

fn check(index: &Index, model: &Model) -> Result<(), TestCaseError> {
    let live = model.live();
    // Every path and every lookup.
    for &n in &live {
        let node = &model.nodes[n];
        prop_assert_eq!(index.path(node.entry), model.path(n));
        prop_assert!(index.flags[node.entry as usize] & DEAD == 0);
        if let Some(p) = node.parent {
            prop_assert_eq!(index.child(model.nodes[p].entry, &node.name), Some(node.entry), "child {}", model.path(n));
        }
        prop_assert_eq!(index.size(node.entry), if node.dir { None } else { Some(node.size) });
        prop_assert_eq!(index.modified(node.entry), node.modified, "modified {}", model.path(n));
        prop_assert_eq!(index.more(node.entry), node.more, "times {}", model.path(n));
    }
    prop_assert_eq!(index.live(), live.len());
    // Searches, and a sorted page of each.
    for query in QUERIES {
        let mut want: Vec<u32> = live.iter().filter(|&&n| naive_match(model, n, query)).map(|&n| model.nodes[n].entry).collect();
        want.sort_unstable();
        let got = Search::new(query).run(index, None);
        prop_assert_eq!(&got, &want, "query {:?}", query);
        let by_name = |e: u32| (fold(index.name(e)), index.name(e).to_string(), e);
        let mut named = want.clone();
        named.sort_by_key(|&e| by_name(e));
        let page = Search::new(query).page(index, None, Sort::Name, false, 1, 3);
        prop_assert_eq!(page.count, want.len());
        prop_assert_eq!(page.page, named.iter().skip(1).take(3).copied().collect::<Vec<_>>(), "name page {:?}", query);
        let mut sized = want.clone();
        sized.sort_by_key(|&e| (std::cmp::Reverse(index.size(e).unwrap_or(0)), std::cmp::Reverse(by_name(e))));
        let page = Search::new(query).page(index, None, Sort::Size, true, 0, 4);
        prop_assert_eq!(page.page, sized.iter().take(4).copied().collect::<Vec<_>>(), "size page {:?}", query);
    }
    Ok(())
}

fn run(ops: Vec<Op>, table_from_start: bool) -> Result<(), TestCaseError> {
    let mut index = Index::new();
    let root = index.push(NONE, "C:", Meta { size: None, modified: 0, flags: DIR });
    if table_from_start {
        index.build_table();
    }
    let mut model = Model { nodes: vec![Node { name: "C:".into(), parent: None, dir: true, alive: true, size: 0, entry: root, modified: 0,
                                               more: More { created: 0, accessed: 0, attributes: 0x10 } }] };
    // Times as the index keeps them: seconds since 1980, from 1981 (0 is "not known").
    const T0: i64 = 315_532_800 + 31_536_000;
    for op in ops {
        match op {
            Op::Add { parent, name, dir, size } => {
                let dirs = model.live_dirs();
                let p = dirs[parent % dirs.len()];
                let name = NAMES[name];
                if model.taken(p, name, None) {
                    continue;
                }
                let modified = T0 + size as i64;
                let entry = index.push(model.nodes[p].entry, name,
                                       Meta { size: if dir { None } else { Some(size) }, modified, flags: if dir { DIR } else { 0 } });
                let more = More { created: 0, accessed: 0, attributes: if dir { 0x10 } else { 0 } };
                model.nodes.push(Node { name: name.into(), parent: Some(p), dir, alive: true, size, entry, modified, more });
            }
            Op::Remove(pick) => {
                let live = model.live();
                let n = live[pick % live.len()];
                if n == 0 {
                    continue;
                }
                index.remove(model.nodes[n].entry);
                for m in model.live() {
                    if m == n || model.inside(m, n) {
                        model.nodes[m].alive = false;
                    }
                }
            }
            Op::Rename { node, parent, name } => {
                let live = model.live();
                let n = live[node % live.len()];
                if n == 0 {
                    continue;
                }
                let dirs: Vec<usize> = model.live_dirs().into_iter().filter(|&d| d != n && !model.inside(d, n)).collect();
                let p = dirs[parent % dirs.len()];
                let name = NAMES[name];
                if model.taken(p, name, Some(n)) {
                    continue;
                }
                let new = index.rename(model.nodes[n].entry, model.nodes[p].entry, name);
                let node = &mut model.nodes[n];
                node.entry = new;
                node.parent = Some(p);
                node.name = name.into();
            }
            Op::Size { node, size } => {
                let live = model.live();
                let n = live[node % live.len()];
                if model.nodes[n].dir {
                    continue;
                }
                // A file written to: its size and date move; its dates made and opened stay.
                index.set_facts(model.nodes[n].entry, Some(size), T0 + size as i64);
                model.nodes[n].size = size;
                model.nodes[n].modified = T0 + size as i64;
            }
            Op::Times { node, made, opened, attributes } => {
                let live = model.live();
                let n = live[node % live.len()];
                let modified = model.nodes[n].modified;
                let created = match made { 0 => modified, 1 => 0, 2 => T0 + 7, _ => T0 + 1000 + node as i64 % 5 };
                let accessed = match opened { 0 => modified, 1 => created, 2 => 0, _ => T0 + 2000 + node as i64 % 3 };
                let more = More { created, accessed, attributes: attributes as u32 };
                index.set_more(model.nodes[n].entry, more);
                model.nodes[n].more = more;
            }
            Op::Settle => index.settle(),
            Op::Compact => {
                index.settle();
                // In place, or into a new index, by turns.
                let map = if index.len() % 2 == 0 {
                    index.compact_in_place()
                } else {
                    let (fresh, map) = index.compact();
                    index = fresh;
                    map
                };
                for n in model.live() {
                    model.nodes[n].entry = map[model.nodes[n].entry as usize];
                }
            }
            Op::SaveLoad => {
                let file = std::env::temp_dir().join(format!("engine-model-{}-{}.idx", std::process::id(), FILES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
                index.save(&file).unwrap();
                index = Index::load(&file).unwrap();
                let _ = std::fs::remove_file(&file);
            }
            Op::RankNames => index.sort_names(),
            Op::RankPaths => {
                index.settle();
                index.rank_paths();
                // Path order right after ranking: a walk of the tree, siblings by name.
                let live = model.live();
                let mut want: Vec<usize> = live.clone();
                want.sort_by_key(|&n| model.path_key(n));
                let mut got: Vec<u32> = live.iter().map(|&n| model.nodes[n].entry).collect();
                got.sort_by(|&a, &b| index.path_order(a, b));
                prop_assert_eq!(got, want.iter().map(|&n| model.nodes[n].entry).collect::<Vec<_>>());
            }
            Op::Table => index.build_table(),
            Op::RankExts => index.rank_exts(),
        }
        index.settle();
        check(&index, &model)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]
    #[test]
    fn with_table(ops in prop::collection::vec(op(), 1..60)) {
        run(ops, true)?;
    }
    #[test]
    fn without_table_at_first(ops in prop::collection::vec(op(), 1..60)) {
        run(ops, false)?;
    }
}
