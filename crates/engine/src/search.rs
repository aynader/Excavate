//! Search by name, in Everything's syntax (`syntax.rs`).
//!
//! One part of the query drives the search, and every other part is checked on what it finds:
//!
//! - **A word** (or a wildcard's plain piece): one SIMD scan of the distinct names (each kept once), split across
//!   cores, at most one hit per name, its rarest choice first, into a bitset of names; then one pass over the entries'
//!   name ids. A wildcard's literal start or end is anchored to the name's edge through the 0 byte between names
//!   (`*.dll` looks for `.dll\0`).
//! - **Extensions** (`ext:`, or a kind filter): a scan of the 1-byte extension column, when they're rare enough.
//! - **A lone regex**: the arena scan, as a filter.
//! - **Otherwise** every entry is checked; each name word in the query (`!e`, say) is found once by an arena scan
//!   first, into a bitset, so checking it is a lookup.
//!
//! A search returns the count and one sorted page, keeping only the best `offset + limit` per core, so a query with
//! millions of hits costs a scan and a count, not a list.

use crate::store::{ext_key, extension, fold, Index, DEAD, DIR, EXT_OTHER, NONE, WIDE};
use crate::syntax::{self, Anchor, Node, Test, Text};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    /// Index order: the order entries came in.
    None,
    Name,
    Path,
    Size,
    Modified,
    Extension,
    /// By date made or last opened.
    Created,
    Accessed,
}

pub struct Search {
    root: Node,
    /// What highlights in a result's name: the words of the query that aren't negated, folded.
    marks: Vec<Vec<u8>>,
    /// Functions Everything has that this index can't answer (they match nothing).
    pub unsupported: Vec<String>,
    /// `count:` (no more results than this), and `sort:`/`ascending:`/`descending:` (the order the search asks
    /// for, over the one it's given).
    pub count: Option<usize>,
    pub sort: Option<Sort>,
    pub descending: Option<bool>,
}

pub struct Results {
    pub count: usize,
    pub page: Vec<u32>,
}

/// A query prepared against an index: folders found, extension tables filled, name words scanned into bitsets.
enum Q<'a> {
    All,
    Nothing,
    And(Vec<Q<'a>>),
    Or(Vec<Q<'a>>),
    Not(Box<Q<'a>>),
    /// A word, and when the scan found it already, which names hold it (exact when `exact` is unset).
    Text(&'a Text, Option<Bits>),
    Glob(&'a [u8], Option<&'a [u8]>, bool),
    Regex(&'a regex::bytes::Regex, bool),
    /// The extension ids wanted, and extensions without an id (looked for in "another"'s names).
    Ext(Box<[bool; 256]>, Vec<Vec<u8>>),
    Files,
    Folders,
    Modified(i64, i64),
    Created(i64, i64),
    Accessed(i64, i64),
    Size(u64, u64),
    /// Inside a folder (NONE: there's no such folder), with its cache slot.
    Under(u32, usize),
    Parent(u32),
    Root,
    Len(u64, u64),
    Attrib(u32),
    Depth(u64, u64),
    /// Worked out over the whole index when the query was prepared (`child:`, `childcount:`, `dupe:`…): which entries.
    Set(Bits),
}

/// A bitset over entries, or over names.
struct Bits(Vec<u64>);

impl Bits {
    fn get(&self, e: u32) -> bool {
        self.0[e as usize / 64] >> (e % 64) & 1 != 0
    }
}

impl Search {
    pub fn new(text: &str) -> Search {
        let parsed = syntax::parse(text);
        let mut marks = Vec::new();
        collect_marks(&parsed.root, false, &mut marks);
        let sort = parsed.sort.map(|key| match key {
            syntax::SortKey::Name => Sort::Name,
            syntax::SortKey::Path => Sort::Path,
            syntax::SortKey::Size => Sort::Size,
            syntax::SortKey::Extension => Sort::Extension,
            syntax::SortKey::Modified => Sort::Modified,
            syntax::SortKey::Created => Sort::Created,
            syntax::SortKey::Accessed => Sort::Accessed,
        });
        Search { root: parsed.root, marks, unsupported: parsed.unsupported, count: parsed.count, sort, descending: parsed.descending }
    }

    /// Every live entry that matches (index order); `within` keeps only those under that entry.
    pub fn run(&self, index: &Index, within: Option<u32>) -> Vec<u32> {
        self.page(index, within, Sort::None, false, 0, usize::MAX).page
    }

    /// The count of matches, and `limit` of them from `offset` on in `sort` order.
    pub fn page(&self, index: &Index, within: Option<u32>, sort: Sort, descending: bool, offset: usize, limit: usize) -> Results {
        pool().install(|| self.page_here(index, within, sort, descending, offset, limit))
    }

    fn page_here(&self, index: &Index, within: Option<u32>, sort: Sort, descending: bool, offset: usize, limit: usize) -> Results {
        // The order asked for in the search wins (`sort:size`); `descending:` alone turns the one given round.
        let (sort, descending) = match (self.sort, self.descending) {
            (Some(sort), order) => (sort, order.unwrap_or(matches!(sort, Sort::Size | Sort::Modified | Sort::Created | Sort::Accessed))),
            (None, Some(order)) => (sort, order),
            (None, None) => (sort, descending),
        };
        match self.count {
            // `count:`: only the first so many, in that order.
            Some(count) => {
                let limit = limit.min(count.saturating_sub(offset));
                let mut found = page_of(&self.root, index, within, sort, descending, offset, limit);
                found.count = found.count.min(count);
                found
            }
            None => page_of(&self.root, index, within, sort, descending, offset, limit),
        }
    }

    /// The order the search itself asks for (`sort:`), if any, and which way.
    pub fn asked_order(&self) -> Option<(Sort, bool)> {
        self.sort.map(|sort| (sort, self.descending.unwrap_or(matches!(sort, Sort::Size | Sort::Modified | Sort::Created | Sort::Accessed))))
    }

    /// Where the query's words are in a result's name: byte ranges of `index.name(e)`, ignoring case and accents as
    /// the search does.
    pub fn highlights(&self, index: &Index, e: u32) -> Vec<Range<usize>> {
        highlight(index.name(e), &self.marks)
    }
}

/// The count of the entries matching `root`, and `limit` of them from `offset` on in `sort` order.
#[allow(clippy::too_many_arguments)]
fn page_of(root: &Node, index: &Index, within: Option<u32>, sort: Sort, descending: bool, offset: usize, limit: usize) -> Results {
    let mut unders = 0;
    let q = prepare(root, index, &mut unders);

    let keep = offset.saturating_add(limit);
    let order = |a: u32, b: u32| -> Ordering {
        let o = match sort {
            Sort::None => a.cmp(&b),
            Sort::Name => index.name_order(a, b),
            Sort::Path => index.path_order(a, b),
            Sort::Size => index.size(a).unwrap_or(0).cmp(&index.size(b).unwrap_or(0)).then_with(|| index.name_order(a, b)),
            Sort::Modified => index.modified(a).cmp(&index.modified(b)).then_with(|| index.name_order(a, b)),
            Sort::Extension => index.ext_order(a, b),
            Sort::Created => index.created(a).cmp(&index.created(b)).then_with(|| index.name_order(a, b)),
            Sort::Accessed => index.accessed(a).cmp(&index.accessed(b)).then_with(|| index.name_order(a, b)),
        };
        if descending { o.reverse() } else { o }
    };
    let threads = rayon::current_num_threads();
    // Made only of tests on a name (words, wildcards, regular expressions, lengths): checked once for each distinct
    // name the scan finds, not for each entry that has it. One word and nothing else: the scan's own hit is the
    // whole test.
    let by_name = name_only(&q);
    let word = matches!(&q, Q::Text(t, _) if t.plain() && !t.whole_word && !t.path && t.anchor == Anchor::Any);
    let plain = by_name || word;
    let check = |e: u32, start: usize, cache: &mut Cache| -> bool {
        index.flags[e as usize] & DEAD == 0
            && within.map_or(true, |root| under(index, e, root))
            && (plain || {
                let mut c = Cand { index, e, start, hay: None, path: None };
                eval(&q, &mut c, cache)
            })
    };
    let keep_name = |id: u32| word || !by_name || eval_name(&q, index, id);
    let parts: Vec<(usize, Top)> = match driver(&q, index) {
        Driver::Pieces { main, folded } => {
            let names = names_with(index, main.as_ref(), &folded, &keep_name);
            entries_named(index, &names, threads, &|e, start, cache| check(e, start, cache), &order, keep, unders)
        }
        Driver::Filter(filter) => {
            // The arena scan only finds candidates (a regex can run across a name's end): each name is checked.
            let Q::Regex(regex, _) = &q else { unreachable!() };
            let names = scan_names(index, &Finder::Regex(filter), true, &|id| regex.is_match(index.id_bytes(id)) && keep_name(id));
            entries_named(index, &names, threads, &|e, start, cache| check(e, start, cache), &order, keep, unders)
        }
        Driver::Ext(lut, rare) => each_entry(index, threads, &|e, start, cache| {
            let ext = index.exts[e as usize];
            (lut[ext as usize] || (ext == EXT_OTHER && !rare.is_empty() && rare_ext(index.name_bytes(e), &rare)))
                && check(e, start, cache)
        }, &order, keep, unders),
        // Made only of name words, extensions and files or folders: whole words of a bitset at a time.
        Driver::Every => match bits_of(&q, index) {
            Some(bits) => each_set(index, &bits, threads, &|e| within.map_or(true, |root| under(index, e, root)), &order, keep),
            // Every name checked once, then their entries.
            None if by_name => {
                let names = every_name(index, &keep_name);
                entries_named(index, &names, threads, &|e, start, cache| check(e, start, cache), &order, keep, unders)
            }
            None => each_entry(index, threads, &|e, start, cache| check(e, start, cache), &order, keep, unders),
        },
    };
    let count = parts.iter().map(|(c, _)| c).sum();
    let mut all: Vec<u32> = parts.into_iter().flat_map(|(_, top)| top.items).collect();
    all.par_sort_unstable_by(|&a, &b| order(a, b));
    let page = all.into_iter().skip(offset).take(limit).collect();
    Results { count, page }
}

/// The byte ranges of `name` where `marks` (folded words) are found, merged.
pub fn highlight(name: &str, marks: &[Vec<u8>]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    if marks.is_empty() {
        return ranges;
    }
    if name.is_ascii() {
        let lower = name.to_ascii_lowercase();
        for mark in marks {
            for at in memchr::memmem::find_iter(lower.as_bytes(), mark) {
                ranges.push(at..at + mark.len());
            }
        }
    } else {
        // The folded name, each folded byte knowing which character of the name it came from.
        let mut folded = Vec::new();
        let mut from = Vec::new();
        for (at, c) in name.char_indices() {
            let mut buffer = [0u8; 4];
            for b in fold(c.encode_utf8(&mut buffer)).bytes() {
                folded.push(b);
                from.push(at..at + c.len_utf8());
            }
        }
        for mark in marks {
            for at in memchr::memmem::find_iter(&folded, mark) {
                if mark.is_empty() {
                    continue;
                }
                ranges.push(from[at].start..from[at + mark.len() - 1].end);
            }
        }
    }
    ranges.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

fn collect_marks(node: &Node, negated: bool, marks: &mut Vec<Vec<u8>>) {
    match node {
        Node::And(xs) | Node::Or(xs) => xs.iter().for_each(|x| collect_marks(x, negated, marks)),
        Node::Not(x) => collect_marks(x, !negated, marks),
        Node::Leaf(Test::Text(t)) if !negated && !t.path && !t.folded.is_empty() => marks.push(t.folded.clone()),
        _ => {}
    }
}

/// The query against this index: folders found by their paths, extensions as ids, and (when no word drives the
/// search) each name word scanned for once.
fn prepare<'a>(node: &'a Node, index: &Index, unders: &mut usize) -> Q<'a> {
    let mut q = prepare_node(node, index, unders);
    if driver_kind(&q, index) == Kind::Every {
        prescan(&mut q, index);
    }
    q
}

fn prepare_node<'a>(node: &'a Node, index: &Index, unders: &mut usize) -> Q<'a> {
    match node {
        Node::All => Q::All,
        Node::And(xs) => Q::And(xs.iter().map(|x| prepare_node(x, index, unders)).collect()),
        Node::Or(xs) => Q::Or(xs.iter().map(|x| prepare_node(x, index, unders)).collect()),
        Node::Not(x) => match &**x {
            Node::All => Q::Nothing,
            x => Q::Not(Box::new(prepare_node(x, index, unders))),
        },
        Node::Leaf(test) => match test {
            Test::Text(t) => Q::Text(t, None),
            Test::Glob { folded, exact, path } => Q::Glob(folded, exact.as_deref(), *path),
            Test::Regex { regex, path } => Q::Regex(regex, *path),
            Test::Ext(exts) => {
                let mut lut = Box::new([false; 256]);
                let mut rare = Vec::new();
                for ext in exts {
                    match index.ext_id(ext) {
                        Some(id) => lut[id as usize] = true,
                        None => rare.push(ext_key(ext.as_bytes())),
                    }
                }
                Q::Ext(lut, rare)
            }
            Test::Files => Q::Files,
            Test::Folders => Q::Folders,
            Test::Modified(a, b) => Q::Modified(*a, *b),
            Test::Created(a, b) => Q::Created(*a, *b),
            Test::Accessed(a, b) => Q::Accessed(*a, *b),
            Test::Size(a, b) => Q::Size(*a, *b),
            Test::Under(path) => {
                *unders += 1;
                Q::Under(resolve(index, path).unwrap_or(NONE), *unders - 1)
            }
            Test::Parent(path) => Q::Parent(resolve(index, path).unwrap_or(NONE)),
            Test::Root => Q::Root,
            Test::Len(a, b) => Q::Len(*a, *b),
            Test::Attrib(flags) => Q::Attrib(*flags),
            Test::Depth(a, b) => Q::Depth(*a, *b),
            Test::Children(what, from, to) => Q::Set(children(index, *what, *from, *to)),
            Test::Child(inner) => {
                // The folders of what matches.
                let mut out = vec![0u64; index.len().div_ceil(64)];
                for e in page_of(inner, index, None, Sort::None, false, 0, usize::MAX).page {
                    let parent = index.parent(e);
                    if parent != NONE {
                        out[parent as usize / 64] |= 1 << (parent % 64);
                    }
                }
                Q::Set(Bits(out))
            }
            Test::Dupe => Q::Set(dupes(index)),
            Test::SizeDupe => Q::Set(size_dupes(index)),
            Test::KeyDupe(key) => Q::Set(key_dupes(index, *key)),
            Test::Type(text) => {
                // Each extension with an id whose type name says it (files only), and folders ("File folder").
                let mut lut = Box::new([false; 256]);
                for (i, ext) in index.ext_names().iter().enumerate() {
                    if crate::known::type_name(&String::from_utf8_lossy(ext)).to_lowercase().contains(text.as_str()) {
                        lut[i + 1] = true;
                    }
                }
                lut[crate::store::EXT_NONE as usize] = "file".contains(text.as_str());
                let files = Q::And(vec![Q::Files, Q::Ext(lut, Vec::new())]);
                if "file folder".contains(text.as_str()) { Q::Or(vec![files, Q::Folders]) } else { files }
            }
        },
    }
}

/// Scans the arena once for each name word in the query, into a bitset each (at most a few: past that, checking each
/// name costs less than more scans).
fn prescan(q: &mut Q, index: &Index) {
    let mut budget = 4;
    fn walk(q: &mut Q, index: &Index, budget: &mut usize) {
        match q {
            Q::And(xs) | Q::Or(xs) => xs.iter_mut().for_each(|x| walk(x, index, budget)),
            Q::Not(x) => walk(x, index, budget),
            Q::Text(t, bits) if !t.path && *budget > 0 && !t.folded.is_empty() => {
                *budget -= 1;
                *bits = Some(scan_bits(index, &text_piece(t)));
            }
            _ => {}
        }
    }
    walk(q, index, &mut budget);
}

/// The names that hold `piece` (folded), as a bitset over names.
fn scan_bits(index: &Index, piece: &[u8]) -> Bits {
    names_with(index, finder(&[piece.to_vec()]).as_ref(), &folded_finder(&[piece.to_vec()]), &|_| true)
}

/// A candidate being checked: its folded name and path, made when first needed.
struct Cand<'i> {
    index: &'i Index,
    e: u32,
    start: usize,
    hay: Option<std::borrow::Cow<'i, [u8]>>,
    path: Option<(Vec<u8>, String)>,
}

impl<'i> Cand<'i> {
    fn name(&self) -> &'i [u8] {
        // (`usize::MAX`: not looked up yet; most checks never need it.)
        if self.start == usize::MAX { self.index.name_bytes(self.e) } else { self.index.name_at(self.e, self.start) }
    }

    /// The name folded (lowercased when it's ASCII).
    fn hay(&mut self) -> &[u8] {
        if self.hay.is_none() {
            let index = self.index;
            self.hay = Some(if index.flags[self.e as usize] & WIDE != 0 {
                index.folded_name(self.e).into()
            } else {
                self.name().to_ascii_lowercase().into()
            });
        }
        self.hay.as_deref().unwrap()
    }

    /// The full path, folded and as it is, the folder's part kept in `cache` while hits stay in one folder.
    fn path(&mut self, cache: &mut Cache) -> &(Vec<u8>, String) {
        if self.path.is_none() {
            let index = self.index;
            let parent = index.parent(self.e);
            if cache.folder != parent || !cache.filled {
                cache.folder = parent;
                cache.filled = true;
                cache.raw = if parent == NONE { String::new() } else { index.path(parent) };
                cache.folded = fold(&cache.raw).into_bytes();
            }
            let name = index.name(self.e);
            let (mut folded, mut raw) = (cache.folded.clone(), cache.raw.clone());
            if !raw.is_empty() && !raw.ends_with('\\') {
                raw.push('\\');
                folded.push(b'\\');
            }
            raw.push_str(name);
            folded.extend_from_slice(fold(name).as_bytes());
            self.path = Some((folded, raw));
        }
        self.path.as_ref().unwrap()
    }
}

#[derive(Default)]
struct Cache {
    folder: u32,
    filled: bool,
    raw: String,
    folded: Vec<u8>,
    /// For each `Under` test: the last folder asked about, and whether it was inside.
    unders: Vec<(u32, bool)>,
    /// The last folder whose depth was worked out, its depth, and whether that's set.
    depth: (u32, u64, bool),
}

fn eval(q: &Q, c: &mut Cand, cache: &mut Cache) -> bool {
    let index = c.index;
    let e = c.e;
    match q {
        Q::All => true,
        Q::Nothing => false,
        Q::And(xs) => xs.iter().all(|x| eval(x, c, cache)),
        Q::Or(xs) => xs.iter().any(|x| eval(x, c, cache)),
        Q::Not(x) => !eval(x, c, cache),
        Q::Text(t, bits) => {
            if let Some(bits) = bits {
                if !bits.get(index.name_of.get(e as usize)) {
                    return false;
                }
                if t.plain() && !t.whole_word && matches!(t.anchor, Anchor::Any | Anchor::End) {
                    return true;
                }
            }
            if t.path {
                let (folded, raw) = c.path(cache);
                find(folded, &t.folded, t.anchor, t.whole_word)
                    && t.exact.as_ref().map_or(true, |exact| find(raw.as_bytes(), exact, t.anchor, t.whole_word))
                    && t.accents_in(raw.as_bytes())
            } else {
                let name = c.name();
                find(c.hay(), &t.folded, t.anchor, t.whole_word)
                    && t.exact.as_ref().map_or(true, |exact| find(name, exact, t.anchor, t.whole_word))
                    && t.accents_in(name)
            }
        }
        Q::Glob(pattern, exact, path) => {
            if *path {
                let (folded, raw) = c.path(cache);
                glob(pattern, folded) && exact.map_or(true, |exact| glob(exact, raw.as_bytes()))
            } else {
                let name = c.name();
                glob(pattern, c.hay()) && exact.map_or(true, |exact| glob(exact, name))
            }
        }
        Q::Regex(regex, path) => {
            if *path { regex.is_match(c.path(cache).1.as_bytes()) } else { regex.is_match(c.name()) }
        }
        Q::Ext(lut, rare) => {
            let ext = index.exts[e as usize];
            lut[ext as usize] || (ext == EXT_OTHER && !rare.is_empty() && rare_ext(c.name(), rare))
        }
        Q::Files => index.flags[e as usize] & DIR == 0,
        Q::Folders => index.flags[e as usize] & DIR != 0,
        Q::Modified(from, to) => (*from..=*to).contains(&index.modified(e)),
        // (0: not known, never a match.)
        Q::Created(from, to) => index.created(e) != 0 && (*from..=*to).contains(&index.created(e)),
        Q::Accessed(from, to) => index.accessed(e) != 0 && (*from..=*to).contains(&index.accessed(e)),
        Q::Size(from, to) => index.flags[e as usize] & DIR == 0 && index.size(e).is_some_and(|s| (*from..=*to).contains(&s)),
        Q::Under(folder, slot) => {
            if *folder == NONE {
                return false;
            }
            let parent = index.parent(e);
            if cache.unders.len() <= *slot {
                cache.unders.resize(*slot + 1, (NONE, false));
            }
            let (last, inside) = cache.unders[*slot];
            if last == parent && parent != NONE {
                return inside;
            }
            let inside = parent == *folder || (parent != NONE && under(index, parent, *folder));
            cache.unders[*slot] = (parent, inside);
            inside
        }
        Q::Parent(folder) => *folder != NONE && index.parent(e) == *folder,
        // As Everything: no parent folder (the drives).
        Q::Root => index.parent(e) == NONE,
        Q::Len(from, to) => (*from..=*to).contains(&(c.name().iter().filter(|&&b| b & 0xC0 != 0x80).count() as u64)),
        Q::Attrib(bits) => index.attributes(e) & bits == *bits,
        Q::Depth(from, to) => {
            // The folder's own depth is kept while hits stay in one folder.
            let parent = index.parent(e);
            let depth = if parent == NONE {
                0
            } else if cache.depth.0 == parent && cache.depth.2 {
                cache.depth.1 + 1
            } else {
                let mut above = 0u64;
                let mut at = parent;
                while at != NONE {
                    above += 1;
                    at = index.parent(at);
                }
                cache.depth = (parent, above - 1, true);
                above
            };
            (*from..=*to).contains(&depth)
        }
        Q::Set(bits) => bits.get(e),
    }
}

/// The folders holding `from..=to` live entries (`what`: 0 any, 1 files, 2 folders), one pass over the index.
fn children(index: &Index, what: u8, from: u64, to: u64) -> Bits {
    let n = index.len();
    let mut count = vec![0u32; n];
    for e in 0..n as u32 {
        let flags = index.flags[e as usize];
        if flags & DEAD != 0 || (what == 1 && flags & DIR != 0) || (what == 2 && flags & DIR == 0) {
            continue;
        }
        let parent = index.parent(e);
        if parent != NONE {
            count[parent as usize] += 1;
        }
    }
    let mut out = vec![0u64; n.div_ceil(64)];
    out.par_iter_mut().enumerate().for_each(|(w, word)| {
        for k in 0..64.min(n.saturating_sub(w * 64)) {
            let e = w * 64 + k;
            if index.flags[e] & DIR != 0 && (from..=to).contains(&(count[e] as u64)) {
                *word |= 1 << k;
            }
        }
    });
    Bits(out)
}

/// The live entries whose name (ignoring case and accents, as names compare) another live entry has too.
fn dupes(index: &Index) -> Bits {
    // Names that compare the same are next to each other in name order: each name's group is its first.
    let groups = index.name_groups();
    let mut count = vec![0u32; index.name_count()];
    for e in 0..index.len() {
        if index.flags[e] & DEAD == 0 {
            let group = groups[index.name_of.get(e) as usize] as usize;
            count[group] = count[group].saturating_add(1);
        }
    }
    let n = index.len();
    let mut out = vec![0u64; n.div_ceil(64)];
    out.par_iter_mut().enumerate().for_each(|(w, word)| {
        for k in 0..64.min(n.saturating_sub(w * 64)) {
            let e = w * 64 + k;
            if count[groups[index.name_of.get(e) as usize] as usize] > 1 {
                *word |= 1 << k;
            }
        }
    });
    Bits(out)
}

/// The live files whose size another live file has too.
fn size_dupes(index: &Index) -> Bits {
    let file = |e: usize| index.flags[e] & (DEAD | DIR) == 0;
    let mut sizes: Vec<u64> = (0..index.len()).filter(|&e| file(e)).filter_map(|e| index.size(e as u32)).collect();
    sizes.par_sort_unstable();
    let mut twice: Vec<u64> = sizes.windows(2).filter(|pair| pair[0] == pair[1]).map(|pair| pair[0]).collect();
    twice.dedup();
    let n = index.len();
    let mut out = vec![0u64; n.div_ceil(64)];
    out.par_iter_mut().enumerate().for_each(|(w, word)| {
        for k in 0..64.min(n.saturating_sub(w * 64)) {
            let e = w * 64 + k;
            if file(e) && index.size(e as u32).is_some_and(|size| twice.binary_search(&size).is_ok()) {
                *word |= 1 << k;
            }
        }
    });
    Bits(out)
}

/// Files that share a key with another file (`namepartdupe:`, `dmdupe:`, `dcdupe:`, `dadupe:`, `attribdupe:`).
fn key_dupes(index: &Index, key: syntax::DupeKey) -> Bits {
    use syntax::DupeKey;
    let file = |e: usize| index.flags[e] & (DEAD | DIR) == 0;
    let key_of = |e: u32| -> Option<u64> {
        match key {
            DupeKey::NamePart => {
                // (Folded beyond ASCII; lowercased otherwise.)
                let name: std::borrow::Cow<[u8]> = if index.flags[e as usize] & crate::store::WIDE != 0 {
                    index.folded_name(e).into()
                } else {
                    index.name_bytes(e).to_ascii_lowercase().into()
                };
                let part = match name.iter().rposition(|&b| b == b'.') { Some(dot) if dot > 0 => &name[..dot], _ => &name[..] };
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hasher::write(&mut hasher, part);
                Some(std::hash::Hasher::finish(&hasher))
            }
            DupeKey::Modified => Some(index.modified(e) as u64),
            DupeKey::Created => Some(index.created(e) as u64).filter(|&t| t != 0),
            DupeKey::Accessed => Some(index.accessed(e) as u64).filter(|&t| t != 0),
            DupeKey::Attributes => Some(index.attributes(e) as u64),
        }
    };
    let mut keys: Vec<u64> = (0..index.len() as u32).into_par_iter().filter(|&e| file(e as usize)).filter_map(key_of).collect();
    keys.par_sort_unstable();
    let mut twice: Vec<u64> = keys.windows(2).filter(|pair| pair[0] == pair[1]).map(|pair| pair[0]).collect();
    twice.dedup();
    let n = index.len();
    let mut out = vec![0u64; n.div_ceil(64)];
    out.par_iter_mut().enumerate().for_each(|(w, word)| {
        for k in 0..64.min(n.saturating_sub(w * 64)) {
            let e = w * 64 + k;
            if file(e) && key_of(e as u32).is_some_and(|key| twice.binary_search(&key).is_ok()) {
                *word |= 1 << k;
            }
        }
    });
    Bits(out)
}

/// Whether the query only tests a name (not where it is, nor its size, date or kind): then it holds for every entry
/// with that name, and is checked once for each.
fn name_only(q: &Q) -> bool {
    match q {
        Q::All | Q::Nothing | Q::Len(..) => true,
        Q::And(xs) | Q::Or(xs) => xs.iter().all(name_only),
        Q::Not(x) => name_only(x),
        Q::Text(t, _) => !t.path,
        Q::Glob(_, _, path) | Q::Regex(_, path) => !path,
        _ => false,
    }
}

/// `eval` for a name-only query (`name_only`), on distinct name `id`.
fn eval_name(q: &Q, index: &Index, id: u32) -> bool {
    let name = index.id_bytes(id);
    // Folded once: lowercased (borrowed when it already is), or the folded copy beyond ASCII.
    let folded: std::borrow::Cow<[u8]> = if !name.is_ascii() {
        index.id_folded(id).into()
    } else if name.iter().any(u8::is_ascii_uppercase) {
        name.to_ascii_lowercase().into()
    } else {
        name.into()
    };
    eval_name_with(q, id, name, &folded)
}

fn eval_name_with(q: &Q, id: u32, name: &[u8], folded: &[u8]) -> bool {
    match q {
        Q::All => true,
        Q::Nothing => false,
        Q::And(xs) => xs.iter().all(|x| eval_name_with(x, id, name, folded)),
        Q::Or(xs) => xs.iter().any(|x| eval_name_with(x, id, name, folded)),
        Q::Not(x) => !eval_name_with(x, id, name, folded),
        Q::Text(t, bits) => {
            if let Some(bits) = bits {
                if !bits.get(id) {
                    return false;
                }
                if t.plain() && !t.whole_word && matches!(t.anchor, Anchor::Any | Anchor::End) {
                    return true;
                }
            }
            find(folded, &t.folded, t.anchor, t.whole_word) && t.exact.as_ref().map_or(true, |exact| find(name, exact, t.anchor, t.whole_word))
                && t.accents_in(name)
        }
        Q::Glob(pattern, exact, _) => glob(pattern, folded) && exact.map_or(true, |exact| glob(exact, name)),
        Q::Regex(regex, _) => regex.is_match(name),
        Q::Len(from, to) => (*from..=*to).contains(&(name.iter().filter(|&&b| b & 0xC0 != 0x80).count() as u64)),
        _ => unreachable!("name_only"),
    }
}

/// Every distinct name for which `keep` holds, in parallel, as a bitset over names.
fn every_name(index: &Index, keep: &(dyn Fn(u32) -> bool + Sync)) -> Bits {
    let count = index.name_count();
    let mut words = vec![0u64; count.div_ceil(64)];
    words.par_chunks_mut(64).enumerate().for_each(|(c, out)| {
        for (w, word) in out.iter_mut().enumerate() {
            let base = (c * 64 + w) * 64;
            for k in 0..64.min(count.saturating_sub(base)) {
                if keep((base + k) as u32) {
                    *word |= 1 << k;
                }
            }
        }
    });
    Bits(words)
}

fn rare_ext(name: &[u8], rare: &[Vec<u8>]) -> bool {
    let Some(ext) = extension(name) else { return false };
    if ext.is_ascii() {
        rare.iter().any(|r| r.eq_ignore_ascii_case(ext))
    } else {
        rare.contains(&ext_key(ext))
    }
}

/// Whether `needle` is in `hay` where `anchor` says, at word boundaries when `whole_word`.
pub(crate) fn find(hay: &[u8], needle: &[u8], anchor: Anchor, whole_word: bool) -> bool {
    let fits = |at: usize| !whole_word || (boundary_before(hay, at) && boundary_after(hay, at + needle.len()));
    match anchor {
        Anchor::Start => hay.starts_with(needle) && fits(0),
        Anchor::End => hay.ends_with(needle) && fits(hay.len() - needle.len()),
        Anchor::Whole => hay == needle,
        Anchor::Any if !whole_word => memchr::memmem::find(hay, needle).is_some(),
        Anchor::Any => memchr::memmem::find_iter(hay, needle).any(fits),
    }
}

fn boundary_before(hay: &[u8], at: usize) -> bool {
    if at == 0 {
        return true;
    }
    let mut from = at - 1;
    while from > 0 && hay[from] & 0xC0 == 0x80 {
        from -= 1;
    }
    std::str::from_utf8(&hay[from..at]).ok().and_then(|s| s.chars().next()).map_or(true, |c| !in_word(c))
}

/// What a whole word (`ww:`) can't be next to: letters, digits and `_`, as Everything has it (`ww:config` 128,844
/// that way against Everything's 129,142; 136,610 without `_`).
fn in_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn boundary_after(hay: &[u8], at: usize) -> bool {
    if at >= hay.len() {
        return true;
    }
    let len = match hay[at] { 0xF0.. => 4, 0xE0.. => 3, 0xC0.. => 2, _ => 1 };
    std::str::from_utf8(&hay[at..(at + len).min(hay.len())]).ok().and_then(|s| s.chars().next()).map_or(true, |c| !in_word(c))
}

/// The folder at an absolute path, by its names, from the root that holds it (a drive, or a folder walked).
pub fn resolve(index: &Index, path: &str) -> Option<u32> {
    let path = path.trim_end_matches('\\');
    let wanted = path.to_ascii_lowercase();
    let mut best: Option<(u32, usize)> = None;
    for &root in index.roots() {
        if index.flags[root as usize] & DEAD != 0 {
            continue;
        }
        let name = index.name(root).trim_end_matches('\\').to_ascii_lowercase();
        let fits = wanted == name || wanted.starts_with(&format!("{name}\\"));
        if fits && best.map_or(true, |(_, len)| name.len() > len) {
            best = Some((root, name.len()));
        }
    }
    let (mut at, len) = best?;
    for part in path[len..].split('\\').filter(|p| !p.is_empty()) {
        at = index.child(at, part)?;
    }
    Some(at)
}

/// Searches run on their own few threads: a scan is bound by memory, and past about 8 threads each one only adds the
/// CPU time its spinning costs (measured: 4.8 ms and 35 ms of CPU a query on 8, 4.4 ms and 64 ms on 16). `ENGINE_THREADS`
/// overrides it.
pub fn pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::env::var("ENGINE_THREADS").ok().and_then(|t| t.parse().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()).min(8));
        rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|n| format!("search-{n}")).build().unwrap()
    })
}

enum Finder {
    Byte(u8, u8),
    Regex(regex::bytes::Regex),
}

impl Finder {
    fn find(&self, hay: &[u8], at: usize) -> Option<usize> {
        match self {
            Finder::Byte(lo, up) => memchr::memchr2(*lo, *up, &hay[at..]).map(|n| at + n),
            Finder::Regex(r) => r.find_at(hay, at).map(|m| m.start()),
        }
    }
}

enum Driver {
    Pieces { main: Option<Finder>, folded: regex::bytes::Regex },
    Filter(regex::bytes::Regex),
    Ext(Box<[bool; 256]>, Vec<Vec<u8>>),
    Every,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Pieces,
    Filter,
    Ext,
    Every,
}

/// What would drive the search, before any bitsets are made.
fn driver_kind(q: &Q, index: &Index) -> Kind {
    match choose(q, index) {
        Choice::Pieces(_) => Kind::Pieces,
        Choice::Filter => Kind::Filter,
        Choice::Ext(..) => Kind::Ext,
        Choice::Every => Kind::Every,
    }
}

enum Choice {
    Pieces(Vec<Vec<u8>>),
    Filter,
    Ext(Box<[bool; 256]>, Vec<Vec<u8>>),
    Every,
}

fn choose(q: &Q, index: &Index) -> Choice {
    let rarity = |piece: &[u8]| piece.iter().filter(|&&b| b != 0).map(|&b| index.frequency(b)).min().unwrap_or(u32::MAX) as u64;
    let pieces = pieces(q, &rarity);
    let exts = ext_set(q);
    if let Some((lut, rare)) = &exts {
        // Extensions drive when they're rare enough, or when nothing else would: a 1-byte column is a quick scan,
        // and checking each of a few percent of the entries costs less than a scan of the names.
        let count: u64 = lut.iter().enumerate().filter(|(_, &on)| on).map(|(id, _)| index.ext_count(id as u8) as u64).sum::<u64>()
            + if rare.is_empty() { 0 } else { index.ext_count(EXT_OTHER) as u64 };
        let small = count * 25 < index.live().max(1) as u64;
        let short_words = pieces.as_ref().is_some_and(|p| p.iter().any(|p| p.iter().filter(|&&b| b != 0).count() < 3));
        if pieces.is_none() || small || short_words && count * 8 < index.live() as u64 {
            return Choice::Ext(lut.clone(), rare.clone());
        }
    }
    if let Some(pieces) = pieces {
        return Choice::Pieces(pieces);
    }
    if matches!(q, Q::Regex(_, false)) {
        return Choice::Filter;
    }
    Choice::Every
}

fn driver(q: &Q, index: &Index) -> Driver {
    match choose(q, index) {
        Choice::Pieces(pieces) => {
            let main = finder(&pieces);
            Driver::Pieces { main, folded: folded_finder(&pieces) }
        }
        Choice::Filter => {
            let Q::Regex(regex, _) = q else { unreachable!() };
            let filter = regex::bytes::RegexBuilder::new(regex.as_str()).case_insensitive(true).multi_line(true)
                .line_terminator(0).build();
            match filter {
                Ok(filter) => Driver::Filter(filter),
                Err(_) => Driver::Every,
            }
        }
        Choice::Ext(lut, rare) => Driver::Ext(lut, rare),
        Choice::Every => Driver::Every,
    }
}

/// The ASCII scan for some pieces: both cases of one character with memchr2 (which beats any engine when hits come
/// every few bytes), else a regex of their alternation, ignoring ASCII case.
fn finder(pieces: &[Vec<u8>]) -> Option<Finder> {
    let ascii: Vec<String> = pieces.iter().filter(|p| p.is_ascii()).map(|p| piece_pattern(p)).collect();
    match ascii.as_slice() {
        [] => None,
        [one] if (one.len() == 1 && one.as_bytes()[0].is_ascii_alphanumeric()) || (one.len() == 2 && one.starts_with('\\') && !one.ends_with('x')) => {
            let b = *one.as_bytes().last().unwrap();
            Some(Finder::Byte(b.to_ascii_lowercase(), b.to_ascii_uppercase()))
        }
        _ => regex::bytes::Regex::new(&format!("(?i-u)(?:{})", ascii.join("|"))).ok().map(Finder::Regex),
    }
}

fn folded_finder(pieces: &[Vec<u8>]) -> regex::bytes::Regex {
    let all: Vec<String> = pieces.iter().map(|p| piece_pattern(p)).collect();
    regex::bytes::Regex::new(&format!("(?:{})", all.join("|"))).unwrap()
}

/// The pieces any match must hold in its (folded) name: a word's, a wildcard's plain piece; for AND the best part's,
/// for OR all of its parts' (when each has some).
fn pieces(q: &Q, rarity: &impl Fn(&[u8]) -> u64) -> Option<Vec<Vec<u8>>> {
    match q {
        Q::Text(t, _) if !t.path && !t.folded.is_empty() => Some(vec![text_piece(t)]),
        Q::Glob(pattern, _, false) => Some(glob_piece(pattern, rarity)).filter(|p| p.iter().any(|&b| b != 0)).map(|p| vec![p]),
        Q::And(xs) => xs.iter().filter_map(|x| pieces(x, rarity)).min_by_key(|p| p.iter().map(|p| rarity(p)).sum::<u64>()),
        Q::Or(xs) => {
            let mut all = Vec::new();
            for x in xs {
                all.extend(pieces(x, rarity)?);
            }
            Some(all)
        }
        _ => None,
    }
}

/// What a word is scanned for: the word, and the 0 after it when it must end the name (every name has a 0 before
/// it, so a leading one would make the scan stop at every name: a word that must start the name is checked on the hit).
fn text_piece(t: &Text) -> Vec<u8> {
    let mut piece = Vec::with_capacity(t.folded.len() + 1);
    piece.extend_from_slice(&t.folded);
    if matches!(t.anchor, Anchor::End | Anchor::Whole) {
        piece.push(0);
    }
    piece
}

/// The extensions any match must have: an `ext:`'s, for AND the smallest part's, for OR all of its parts' (when each
/// has some).
fn ext_set(q: &Q) -> Option<(Box<[bool; 256]>, Vec<Vec<u8>>)> {
    match q {
        Q::Ext(lut, rare) => Some((lut.clone(), rare.clone())),
        Q::And(xs) => xs.iter().filter_map(ext_set).min_by_key(|(lut, rare)| lut.iter().filter(|&&on| on).count() + rare.len() * 4),
        Q::Or(xs) => {
            let mut lut = Box::new([false; 256]);
            let mut rare = Vec::new();
            for x in xs {
                let (l, r) = ext_set(x)?;
                for (a, b) in lut.iter_mut().zip(l.iter()) {
                    *a |= *b;
                }
                rare.extend(r);
            }
            Some((lut, rare))
        }
        _ => None,
    }
}

/// The names a scan finds: `main` over the names as they are (those beyond ASCII left to the folded scan), `folded`
/// over the folded ones; each name found checked by `keep`.
fn names_with(index: &Index, main: Option<&Finder>, folded: &regex::bytes::Regex, keep: &(dyn Fn(u32) -> bool + Sync)) -> Bits {
    let mut bits = match main {
        Some(main) => scan_names(index, main, false, keep),
        None => Bits(vec![0u64; index.name_count().div_ceil(64)]),
    };
    for id in scan_folded(index, folded, keep) {
        bits.0[id as usize / 64] |= 1 << (id % 64);
    }
    bits
}

/// Scans the distinct names for `finder`, at most one hit per name, into a bitset over names; `wide` says whether
/// names beyond ASCII count (they're searched folded, separately). Split into runs of names, each run filling its own
/// words of the bitset, so nothing is collected or shared.
fn scan_names(index: &Index, finder: &Finder, wide: bool, keep: &(dyn Fn(u32) -> bool + Sync)) -> Bits {
    let count = index.name_count();
    let mut words = vec![0u64; count.div_ceil(64)];
    let threads = rayon::current_num_threads();
    let per = words.len().div_ceil(threads * 8).max(1);
    words.par_chunks_mut(per).enumerate().for_each(|(c, out)| {
        let (first, last) = (c * per * 64, ((c + 1) * per * 64).min(count));
        if first >= last {
            return;
        }
        let to = index.id_start(last as u32);
        let hay = &index.names[..to];
        // From the 0 before the first name, so an anchored piece finds it.
        let mut at = index.id_start(first as u32) - 1;
        let mut id = first as u32;
        while at < to {
            let Some(hit) = finder.find(hay, at) else { break };
            if hit >= to { break; }
            // A piece anchored to a name's start matches on the 0 before it.
            let hit = if index.names[hit] == 0 { hit + 1 } else { hit };
            if hit >= to { break; }
            // Forward to the name that holds it (hits come in order).
            while index.id_start(id + 1) <= hit {
                id += 1;
            }
            if (wide || index.id_bytes(id).is_ascii()) && keep(id) {
                let k = id as usize - first;
                out[k / 64] |= 1 << (k % 64);
            }
            // On from this name's terminator: the next name's anchored piece starts there.
            at = (index.id_start(id + 1) - 1).max(hit + 1);
        }
    });
    Bits(words)
}

/// The same over the folded names beyond ASCII: the ids found.
fn scan_folded(index: &Index, folded: &regex::bytes::Regex, keep: &(dyn Fn(u32) -> bool + Sync)) -> Vec<u32> {
    let slots = index.folded_at.len();
    if slots == 0 {
        return Vec::new();
    }
    let threads = rayon::current_num_threads();
    let per = slots.div_ceil(threads).max(1);
    (0..slots.div_ceil(per)).into_par_iter().flat_map_iter(|c| {
        let mut found = Vec::new();
        let (s0, s1) = (c * per, ((c + 1) * per).min(slots));
        let to = if s1 < slots { index.folded_at[s1].0 as usize } else { index.folded.len() };
        let hay = &index.folded[..to];
        let mut slot = s0;
        let mut at = index.folded_at[s0].0 as usize - 1;
        while at < to {
            let Some(m) = folded.find_at(hay, at) else { break };
            if m.start() >= to { break; }
            let hit = if index.folded[m.start()] == 0 { m.start() + 1 } else { m.start() };
            if hit >= to { break; }
            while slot + 1 < s1 && index.folded_at[slot + 1].0 as usize <= hit {
                slot += 1;
            }
            let id = index.folded_at[slot].1;
            let next = if slot + 1 < slots { index.folded_at[slot + 1].0 as usize } else { index.folded.len() + 1 };
            if keep(id) {
                found.push(id);
            }
            at = (next - 1).max(hit + 1);
        }
        found
    }).collect()
}

/// The live entries with a name in `names` that pass `check`, counted and offered, in parallel runs of entries.
#[allow(clippy::too_many_arguments)]
fn entries_named(index: &Index, names: &Bits, threads: usize, check: &(dyn Fn(u32, usize, &mut Cache) -> bool + Sync),
                 order: &(dyn Fn(u32, u32) -> Ordering + Sync), keep: usize, unders: usize) -> Vec<(usize, Top)> {
    let n = index.len();
    let per = n.div_ceil(threads * 4).max(4096);
    (0..n.div_ceil(per)).into_par_iter().map(|c| {
        let mut top = Top::new(keep);
        let mut count = 0;
        let mut cache = Cache { unders: vec![(NONE, false); unders], ..Cache::default() };
        // The names' ids a block at a time (`Ids::fill`).
        let mut ids = [0u32; 256];
        let end = ((c + 1) * per).min(n);
        let mut from = c * per;
        while from < end {
            let got = index.name_of.fill(from, &mut ids[..(end - from).min(256)]);
            for (k, &id) in ids[..got].iter().enumerate() {
                let e = from + k;
                if names.get(id) && index.flags[e] & DEAD == 0 && check(e as u32, usize::MAX, &mut cache) {
                    count += 1;
                    top.offer(e as u32, order);
                }
            }
            from += got;
        }
        (count, top)
    }).collect()
}

/// Every entry, checked in parallel runs.
fn each_entry(index: &Index, threads: usize, check: &(dyn Fn(u32, usize, &mut Cache) -> bool + Sync),
              order: &(dyn Fn(u32, u32) -> Ordering + Sync), keep: usize, unders: usize) -> Vec<(usize, Top)> {
    let n = index.len();
    let per = n.div_ceil(threads * 4).max(4096);
    (0..n.div_ceil(per)).into_par_iter().map(|c| {
        let mut top = Top::new(keep);
        let mut count = 0;
        let mut cache = Cache { unders: vec![(NONE, false); unders], ..Cache::default() };
        for e in c * per..((c + 1) * per).min(n) {
            if index.flags[e] & DEAD == 0 && check(e as u32, usize::MAX, &mut cache) {
                count += 1;
                top.offer(e as u32, order);
            }
        }
        (count, top)
    }).collect()
}

/// The query as a bitset over entries, when every part of it can be one: a name word already scanned (`prescan`), an
/// extension (its column scanned), files or folders, dates made or opened, attributes, and AND, OR and NOT of those. Dead entries may be set: the caller
/// leaves them out.
fn bits_of(q: &Q, index: &Index) -> Option<Bits> {
    let words = index.len().div_ceil(64);
    let column = |test: &(dyn Fn(usize) -> bool + Sync)| -> Bits {
        let mut out = vec![0u64; words];
        out.par_iter_mut().enumerate().for_each(|(w, word)| {
            let base = w * 64;
            for k in 0..64.min(index.len().saturating_sub(base)) {
                if test(base + k) {
                    *word |= 1 << k;
                }
            }
        });
        Bits(out)
    };
    Some(match q {
        Q::All => Bits(vec![u64::MAX; words]),
        Q::Nothing => Bits(vec![0; words]),
        Q::Text(t, Some(bits)) if t.plain() && !t.whole_word && matches!(t.anchor, Anchor::Any | Anchor::End) => {
            let mut out = vec![0u64; words];
            out.par_iter_mut().enumerate().for_each(|(w, word)| {
                let mut ids = [0u32; 64];
                let got = index.name_of.fill(w * 64, &mut ids);
                for (k, &id) in ids[..got].iter().enumerate() {
                    *word |= (bits.get(id) as u64) << k;
                }
            });
            Bits(out)
        }
        Q::Ext(lut, rare) if rare.is_empty() => column(&|e| lut[index.exts[e] as usize]),
        Q::Set(bits) => Bits(bits.0.clone()),
        Q::Files => column(&|e| index.flags[e] & DIR == 0),
        // v6's dates made and opened, read in order (`Index::times_in`).
        Q::Created(from, to) => Bits(index.times_in(false, *from, *to)),
        Q::Accessed(from, to) => Bits(index.times_in(true, *from, *to)),
        Q::Attrib(bits) => Bits(index.attributes_with(*bits)),
        Q::Folders => column(&|e| index.flags[e] & DIR != 0),
        Q::Not(x) => {
            let mut b = bits_of(x, index)?;
            b.0.par_iter_mut().for_each(|w| *w = !*w);
            b
        }
        Q::And(xs) | Q::Or(xs) => {
            let and = matches!(q, Q::And(_));
            let mut parts = xs.iter().map(|x| bits_of(x, index)).collect::<Option<Vec<_>>>()?.into_iter();
            let mut acc = parts.next().unwrap_or_else(|| Bits(vec![if and { u64::MAX } else { 0 }; words]));
            for part in parts {
                acc.0.par_iter_mut().zip(part.0.par_iter()).for_each(|(a, b)| if and { *a &= *b } else { *a |= *b });
            }
            acc
        }
        _ => return None,
    })
}

/// The live entries whose bit is set (and that pass `keep_it`), counted and offered, word by word in parallel.
fn each_set(index: &Index, bits: &Bits, threads: usize, keep_it: &(dyn Fn(u32) -> bool + Sync),
            order: &(dyn Fn(u32, u32) -> Ordering + Sync), keep: usize) -> Vec<(usize, Top)> {
    let words = bits.0.len();
    let per = words.div_ceil(threads * 4).max(1);
    (0..words.div_ceil(per)).into_par_iter().map(|c| {
        let mut top = Top::new(keep);
        let mut count = 0;
        for w in c * per..((c + 1) * per).min(words) {
            let mut word = bits.0[w];
            while word != 0 {
                let k = word.trailing_zeros() as usize;
                word &= word - 1;
                let e = w * 64 + k;
                if e < index.len() && index.flags[e] & DEAD == 0 && keep_it(e as u32) {
                    count += 1;
                    top.offer(e as u32, order);
                }
            }
        }
        (count, top)
    }).collect()
}

/// What a whole-name pattern is scanned for. Its literal end, when it has one, is anchored to the name's end with the 0
/// byte that ends each name in the arena ("*.dll" looks for ".dll\0", which only a name's end can hold), its literal
/// start is looked for as it is, as long as either is two bytes or more; otherwise its rarest piece.
fn glob_piece(g: &[u8], rarity: &impl Fn(&[u8]) -> u64) -> Vec<u8> {
    let wild = |b: &u8| *b == b'*' || *b == b'?';
    let head: Vec<u8> = g.iter().take_while(|b| !wild(b)).copied().collect();
    let tail: Vec<u8> = g.iter().rev().take_while(|b| !wild(b)).copied().collect::<Vec<_>>().into_iter().rev().collect();
    if head.len() == g.len() {
        // No wildcard at all: the whole name (its end anchored; its start checked on the hit).
        let mut whole = g.to_vec();
        whole.push(0);
        return whole;
    }
    let best_tail = (tail.len() >= 2).then(|| { let mut t = tail.clone(); t.push(0); t });
    let best_head = (head.len() >= 2).then(|| head.clone());
    match (best_head, best_tail) {
        (Some(h), Some(t)) => if rarity(&head) <= rarity(&tail) { h } else { t },
        (Some(h), None) => h,
        (None, Some(t)) => t,
        (None, None) => g.split(wild).max_by_key(|p| rarity_len(p, rarity)).map(<[u8]>::to_vec).unwrap_or_default(),
    }
}

/// A piece as a regex: its text escaped, its 0 bytes (a name's edge) as \x00.
fn piece_pattern(piece: &[u8]) -> String {
    piece.split(|&b| b == 0).map(|part| regex::escape(std::str::from_utf8(part).unwrap_or(""))).collect::<Vec<_>>().join(r"\x00")
}

/// A glob piece's worth as a scan target: rarer, then longer, is better.
fn rarity_len(piece: &[u8], rarity: &impl Fn(&[u8]) -> u64) -> (std::cmp::Reverse<u64>, usize) {
    (std::cmp::Reverse(if piece.is_empty() { u64::MAX } else { rarity(piece) }), piece.len())
}

/// Whole-name match: `*` any run of bytes, `?` one character.
pub fn glob(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (usize::MAX, 0);
    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'?' {
            p += 1;
            t += utf8_len(text[t]);
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = p;
            p += 1;
            mark = t;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    }
}

/// The best `keep` entries one core has seen, as a heap with the worst on top.
struct Top {
    keep: usize,
    items: Vec<u32>,
}

impl Top {
    fn new(keep: usize) -> Top {
        Top { keep, items: Vec::new() }
    }

    fn offer(&mut self, e: u32, order: &(dyn Fn(u32, u32) -> Ordering + Sync)) {
        if self.keep == 0 {
            return;
        }
        if self.keep == usize::MAX {
            self.items.push(e);
            return;
        }
        if self.items.len() < self.keep {
            self.items.push(e);
            // Sift up: the worst rises to the top.
            let mut i = self.items.len() - 1;
            while i > 0 {
                let parent = (i - 1) / 2;
                if order(self.items[i], self.items[parent]) == Ordering::Greater {
                    self.items.swap(i, parent);
                    i = parent;
                } else {
                    break;
                }
            }
        } else if order(e, self.items[0]) == Ordering::Less {
            self.items[0] = e;
            let n = self.items.len();
            let mut i = 0;
            loop {
                let (l, r) = (2 * i + 1, 2 * i + 2);
                let mut worst = i;
                if l < n && order(self.items[l], self.items[worst]) == Ordering::Greater {
                    worst = l;
                }
                if r < n && order(self.items[r], self.items[worst]) == Ordering::Greater {
                    worst = r;
                }
                if worst == i {
                    break;
                }
                self.items.swap(i, worst);
                i = worst;
            }
        }
    }
}

pub fn under(index: &Index, entry: u32, root: u32) -> bool {
    let mut at = index.parent(entry);
    while at != NONE {
        if at == root {
            return true;
        }
        at = index.parent(at);
    }
    false
}
