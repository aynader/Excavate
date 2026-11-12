//! One score for every candidate, from the hot tier or from Everything: how well the name matches (the whole name,
//! its start, the start of a word in it, initials, anywhere in it), how much the person uses it (opens, with time
//! decay), how fresh it is, and where it lives (apps and their own folders up; dependency folders, caches and system
//! files down). Path and size never outweigh a clearly better name match.

use crate::query::Terms;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    /// A Start menu shortcut: an app.
    App,
    /// In Windows' Recent Items: opened lately in any app.
    Recent,
    /// Opened through Excavate before.
    History,
    /// In one of the person's own folders (Desktop, Documents, Downloads…).
    Folder,
    /// Found by Everything.
    Everything,
    /// Found by its contents (the content index), not its name.
    Content,
}

#[derive(Clone, Debug, Default)]
pub struct Candidate {
    pub name: String,
    pub folder: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    /// Seconds since 1970.
    pub modified: Option<i64>,
    /// Times opened (Everything's run count, Excavate's history).
    pub opens: u32,
    /// When last opened (Everything's date run, Recent Items, Excavate's history).
    pub opened: Option<i64>,
    pub hidden: bool,
    pub origin: Option<Origin>,
    /// How well its contents matched (0 to 1), when the content index found it.
    pub content: Option<f32>,
}

impl Candidate {
    pub fn path(&self) -> String {
        if self.folder.is_empty() {
            return if self.name.ends_with(':') { format!("{}\\", self.name) } else { self.name.clone() };
        }
        if self.folder.ends_with('\\') { format!("{}{}", self.folder, self.name) } else { format!("{}\\{}", self.folder, self.name) }
    }

    pub fn key(&self) -> String {
        self.path().to_lowercase()
    }

    /// Hidden or system, as Windows marks it, or by its name when that wasn't asked: Everything's results don't say
    /// (asking reads each file's attributes from the disk), and what Windows hides that the person comes across is
    /// mostly these. AppData and dot-folders are pushed down in `score` anyway.
    pub fn looks_hidden(&self) -> bool {
        let name = self.name.to_lowercase();
        let at_root = self.folder.len() <= 3;
        self.hidden
            || name.starts_with('$')
            || name.starts_with("ntuser.")
            || HIDDEN_NAMES.contains(&name.as_str())
            || at_root && HIDDEN_AT_ROOT.contains(&name.as_str())
    }
}

const HIDDEN_NAMES: &[&str] = &["desktop.ini", "thumbs.db", "iconcache.db", "system volume information"];
const HIDDEN_AT_ROOT: &[&str] = &[
    "programdata", "recovery", "config.msi", "msocache", "documents and settings", "pagefile.sys", "hiberfil.sys",
    "swapfile.sys", "dumpstack.log", "dumpstack.log.tmp", "bootmgr", "bootnxt",
];

#[derive(Clone, Debug)]
pub struct Ranked {
    pub candidate: Candidate,
    pub score: f32,
    /// Byte ranges of the name that matched.
    pub highlights: Vec<Range<usize>>,
}

/// How well `name` matches the words: 0 (no) to about 1.25 (the whole name), and which parts matched.
pub fn name_match(name: &str, terms: &Terms) -> Option<(f32, Vec<Range<usize>>)> {
    if terms.words.is_empty() {
        return Some((0.0, Vec::new()));
    }
    let matching = terms.matching;
    let lower = if matching.case { Lowered::exact(name) } else { Lowered::new(name) };
    let stem_end = name.rfind('.').filter(|&dot| dot > 0).unwrap_or(name.len());
    let boundaries = word_starts(name);
    let mut total = 0.0;
    let mut highlights = Vec::new();
    let mut previous_end = 0usize;
    let mut in_order = true;
    for word in &terms.words {
        let mut best: Option<(f32, Range<usize>)> = None;
        for start in lower.find_all(word) {
            let end = lower.original_end(start, word.len());
            let start_original = lower.original(start);
            if matching.whole_word && !whole_word_at(name, start_original, end) {
                continue;
            }
            let quality = if start_original == 0 && end >= stem_end && terms.words.len() == 1 {
                1.0 // the whole name (without its extension)
            } else if start_original == 0 {
                0.9
            } else if boundaries.contains(&start_original) {
                0.75
            } else {
                0.5
            };
            if best.as_ref().is_none_or(|(q, _)| quality > *q) {
                best = Some((quality, start_original..end));
            }
        }
        let (quality, range) = match best {
            Some(found) => found,
            // Initials are a guess: asking for exact matches asks for no guessing.
            None if matching.case || matching.whole_word => return None,
            None => match initials(name, &boundaries, word) {
                Some(ranges) => {
                    highlights.extend(ranges);
                    total += 0.7;
                    continue;
                }
                None => return None,
            },
        };
        if range.start < previous_end {
            in_order = false;
        }
        previous_end = range.end;
        total += quality;
        highlights.push(range);
    }
    let mut quality = total / terms.words.len() as f32;
    if terms.words.len() > 1 && in_order {
        quality += 0.1; // "q3 report" in "Q3 report final.docx"
    }
    if name[..stem_end].eq_ignore_ascii_case(&terms.text) || name.eq_ignore_ascii_case(&terms.text) {
        quality += 0.15; // exactly what was typed
    }
    highlights.sort_by_key(|range| range.start);
    Some((quality, merge(highlights)))
}

/// Whether a match stands as a word of its own. As in Everything (measured): `_` joins, like a letter or a digit.
fn whole_word_at(name: &str, start: usize, end: usize) -> bool {
    let joins = |c: char| c.is_alphanumeric() || c == '_';
    !name[..start].chars().next_back().is_some_and(joins) && !name[end..].chars().next().is_some_and(joins)
}

/// "vsc" in "Visual Studio Code", "gc" in "GoogleChrome": the word's letters are the initials of the name's words.
fn initials(name: &str, boundaries: &[usize], word: &str) -> Option<Vec<Range<usize>>> {
    if word.chars().count() < 2 || boundaries.len() < word.chars().count() {
        return None;
    }
    let mut ranges = Vec::new();
    let mut starts = boundaries.iter();
    for letter in word.chars() {
        let start = starts.find(|&&start| name[start..].chars().next().is_some_and(|c| c.to_lowercase().eq(letter.to_lowercase())))?;
        ranges.push(*start..*start + name[*start..].chars().next().unwrap().len_utf8());
    }
    Some(ranges)
}

/// Where words start in a name: after a space, _ - . ( [ or digits, and at camelCase humps.
fn word_starts(name: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous: Option<char> = None;
    for (index, c) in name.char_indices() {
        let start = match previous {
            None => true,
            Some(p) => {
                (!p.is_alphanumeric() && c.is_alphanumeric())
                    || (p.is_lowercase() && c.is_uppercase())
                    || (p.is_ascii_digit() && c.is_alphabetic())
                    || (p.is_alphabetic() && c.is_ascii_digit())
            }
        };
        if start && c.is_alphanumeric() {
            starts.push(index);
        }
        previous = Some(c);
    }
    starts
}

fn merge(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// A lowercased copy of a name that remembers where each of its bytes came from.
struct Lowered {
    text: String,
    origin: Vec<usize>,
    original_len: usize,
}

impl Lowered {
    fn new(name: &str) -> Lowered {
        if name.is_ascii() {
            return Lowered { text: name.to_ascii_lowercase(), origin: Vec::new(), original_len: name.len() };
        }
        let mut text = String::with_capacity(name.len());
        let mut origin = Vec::with_capacity(name.len());
        for (index, c) in name.char_indices() {
            for lower in c.to_lowercase() {
                let before = text.len();
                text.push(lower);
                origin.extend(std::iter::repeat_n(index, text.len() - before));
            }
        }
        Lowered { text, origin, original_len: name.len() }
    }

    /// Exactly as it's written, for matching case.
    fn exact(name: &str) -> Lowered {
        Lowered { text: name.to_string(), origin: Vec::new(), original_len: name.len() }
    }

    fn original(&self, index: usize) -> usize {
        if self.origin.is_empty() { index } else { self.origin.get(index).copied().unwrap_or(self.original_len) }
    }

    /// The original end of a match starting at `start` of `length` lowered bytes.
    fn original_end(&self, start: usize, length: usize) -> usize {
        if self.origin.is_empty() {
            return start + length;
        }
        let end = start + length;
        if end >= self.origin.len() { self.original_len } else { self.origin[end] }
    }

    fn find_all<'a>(&'a self, word: &'a str) -> impl Iterator<Item = usize> + 'a {
        memchr::memmem::find_iter(self.text.as_bytes(), word.as_bytes())
            .filter(|&start| self.text.is_char_boundary(start))
            .take(8)
    }
}

/// Folders whose contents are rarely what anyone is looking for: other people's code, caches, build output, Windows'
/// component store. Matched against the lowercased path.
const BURIED: &[&str] = &[
    r"\node_modules\", r"\site-packages\", r"\dist-packages\", r"\.git\", r"\__pycache__\", r"\.cargo\registry\",
    r"\.rustup\", r"\appdata\local\temp\", r"\windows\winsxs\", r"\windows\servicing\", r"\windows\installer\",
    r"\$recycle.bin\", r"\.vs\", r"\target\debug\", r"\target\release\", r"\obj\debug\", r"\obj\release\", r"\.venv\",
    r"\venv\", r"\.gradle\", r"\.m2\", r"\.nuget\packages\", r"\appdata\local\packages\", r"\.cache\",
    r"\appdata\local\microsoft\", r"\appdata\roaming\microsoft\windows\recent\", r"\windows\system32\driverstore\",
    r"\programdata\package cache\", r"\vendor\", r"\third_party\", r"\.build\", r"\build\", r"\dist\",
];

/// The person's own places (matched against the lowercased path after the profile folder).
const OWN: &[&str] = &[r"\desktop\", r"\documents\", r"\downloads\", r"\pictures\", r"\videos\", r"\music\", r"\onedrive"];

pub struct Context {
    /// Now, in seconds since 1970.
    pub now: i64,
    /// The profile folder, lowercased ("c:\users\me").
    pub profile: String,
}

fn half_life(age_seconds: i64, days: f32) -> f32 {
    let age_days = (age_seconds.max(0) as f32) / 86_400.0;
    0.5f32.powf(age_days / days)
}

/// How far `word` is from the closest part of `name`: the fewest letters changed, added or left out to find it in the
/// name, case aside (0: the name has it). Sellers' approximate substring match, one row at a time.
pub fn closeness(word: &str, name: &str) -> usize {
    let word: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
    let mut previous: Vec<usize> = (0..=word.len()).collect();
    let mut current = vec![0; word.len() + 1];
    let mut best = word.len();
    for letter in name.chars().flat_map(char::to_lowercase) {
        // (A match may start anywhere in the name.)
        current[0] = 0;
        for at in 1..=word.len() {
            let changed = usize::from(word[at - 1] != letter);
            current[at] = (previous[at - 1] + changed).min(previous[at] + 1).min(current[at - 1] + 1);
        }
        best = best.min(current[word.len()]);
        std::mem::swap(&mut previous, &mut current);
    }
    best
}

/// The score of a candidate whose name matched with `quality`.
pub fn score(candidate: &Candidate, quality: f32, context: &Context) -> f32 {
    let mut score = quality * 100.0;
    score += match candidate.origin {
        Some(Origin::App) => 25.0,
        Some(Origin::Recent) => 6.0,
        Some(Origin::History) => 4.0,
        Some(Origin::Folder) => 3.0,
        Some(Origin::Everything) | Some(Origin::Content) | None => 0.0,
    };
    if candidate.opens > 0 || candidate.opened.is_some() {
        let decay = candidate.opened.map(|at| half_life(context.now - at, 14.0)).unwrap_or(0.4);
        score += (12.0 * (1.0 + candidate.opens as f32).ln() + 8.0) * decay;
    }
    if let Some(modified) = candidate.modified {
        score += 6.0 * half_life(context.now - modified, 30.0);
    }
    let path = candidate.path().to_lowercase();
    if BURIED.iter().any(|buried| path.contains(buried)) {
        score -= 30.0;
    }
    if candidate.looks_hidden() {
        score -= 15.0;
    }
    // App data and dot-folders (.codex, .vscode…) are where programs keep their things: rarely what's looked for,
    // though sometimes (a config file), so a mild push down rather than burial.
    let folder = candidate.folder.to_lowercase();
    if folder.contains(r"\appdata\") || folder.ends_with(r"\appdata")
        || folder.split('\\').any(|part| part.len() > 1 && part.starts_with('.'))
    {
        score -= 12.0;
    }
    if let Some(rest) = path.strip_prefix(&context.profile) {
        if OWN.iter().any(|own| rest.starts_with(own)) {
            score += 5.0;
        }
    }
    let depth = path.matches('\\').count() as f32;
    score -= 0.4 * (depth - 4.0).max(0.0);
    score -= 0.05 * candidate.name.chars().count() as f32;
    score
}

/// Ranks candidates (deduplicated by path, keeping what each source knew) for `terms`, best first, at most `limit`.
pub fn rank(candidates: impl IntoIterator<Item = Candidate>, terms: &Terms, context: &Context, limit: usize) -> Vec<Ranked> {
    let mut by_path: std::collections::HashMap<String, Candidate> = std::collections::HashMap::new();
    for candidate in candidates {
        match by_path.entry(candidate.key()) {
            std::collections::hash_map::Entry::Occupied(mut existing) => combine(existing.get_mut(), candidate),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(candidate);
            }
        }
    }
    let mut ranked: Vec<Ranked> = by_path
        .into_values()
        .filter_map(|candidate| {
            let named = name_match(&candidate.name, terms).or_else(|| {
                // Everything found it in the path: its name needn't say so.
                (terms.matching.path && candidate.origin == Some(Origin::Everything)).then(|| (IN_PATH, Vec::new()))
            });
            let (quality, highlights) = match (named, candidate.content) {
                // The name, and the contents too: a little more sure.
                (Some((quality, highlights)), Some(content)) => (quality + 0.15 * content, highlights),
                (Some(named), None) => named,
                (None, Some(content)) => (content_quality(content), Vec::new()),
                (None, None) => return None,
            };
            Some(Ranked { score: score(&candidate, quality, context), candidate, highlights })
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| a.candidate.name.len().cmp(&b.candidate.name.len()))
            .then_with(|| a.candidate.folder.cmp(&b.candidate.folder))
    });
    ranked.truncate(limit);
    ranked
}

/// A match in the path and not the name, on the name-match scale: below a name that merely contains the words.
const IN_PATH: f32 = 0.4;

/// A match by contents only, on the name-match scale: from 0.35 (one word, barely) to 0.8 (a strong match). Below a
/// name that starts with the words (0.9), above one that merely contains them (0.5) when the contents match well.
pub fn content_quality(content: f32) -> f32 {
    0.35 + 0.45 * content.clamp(0.0, 1.0)
}

/// A content match's BM25 score as 0 to 1, saturating: about 0.75 for a clear match of every word.
pub fn content_strength(bm25: f32, words: usize) -> f32 {
    (bm25 / (4.0 * words.max(1) as f32)).tanh()
}

/// Merges what two sources know about the same file.
fn combine(into: &mut Candidate, other: Candidate) {
    into.origin = match (into.origin, other.origin) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    into.opens = into.opens.max(other.opens);
    into.opened = into.opened.max(other.opened);
    into.modified = into.modified.or(other.modified);
    into.size = into.size.or(other.size);
    into.hidden |= other.hidden;
    into.content = match (into.content, other.content) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(text: &str) -> Terms {
        Terms::parse(text)
    }

    fn file(name: &str, folder: &str) -> Candidate {
        Candidate { name: name.into(), folder: folder.into(), origin: Some(Origin::Everything), ..Candidate::default() }
    }

    fn context() -> Context {
        Context { now: 1_800_000_000, profile: r"c:\users\me".into() }
    }

    #[test]
    fn how_close() {
        assert_eq!(closeness("invoice", "Invoice 2024.pdf"), 0);
        assert_eq!(closeness("invoce", "Invoice 2024.pdf"), 1);
        assert_eq!(closeness("ivnoice", "Invoice 2024.pdf"), 2);
        assert_eq!(closeness("chrome", "Google Chrme.lnk"), 1);
        assert_eq!(closeness("zzzz", "abc"), 4);
    }

    #[test]
    fn match_quality() {
        let quality = |name: &str, text: &str| name_match(name, &terms(text)).map(|(q, _)| q);
        assert!(quality("budget.xlsx", "budget").unwrap() > quality("budget 2026.xlsx", "budget").unwrap());
        assert!(quality("budget 2026.xlsx", "budget").unwrap() > quality("2026 budget.xlsx", "budget").unwrap());
        assert!(quality("2026 budget.xlsx", "budget").unwrap() > quality("mybudget.xlsx", "budget").unwrap());
        assert!(quality("Visual Studio Code.lnk", "vsc").is_some());
        assert!(quality("notes.txt", "budget").is_none());
        assert!(quality("Q3 report final.docx", "q3 report").unwrap() > quality("report for q3.docx", "q3 report").unwrap());
        assert_eq!(name_match("QuarterlyReport.pdf", &terms("report")).unwrap().1, vec![9..15]);
        assert_eq!(name_match("Café Menu.pdf", &terms("menu")).unwrap().1, vec![6..10]);
        assert_eq!(name_match("ÉTÉ.txt", &terms("été")).unwrap().1, vec![0..5]);
    }

    #[test]
    fn toggles() {
        use crate::query::Matching;
        let exact = |text: &str, case, whole_word| Terms::matching(text, Matching { case, whole_word, ..Matching::default() });
        let matches = |name: &str, terms: &Terms| name_match(name, terms).is_some();

        let case = exact("Report", true, false);
        assert!(matches("Q3 Report.pdf", &case) && !matches("q3 report.pdf", &case));
        assert_eq!(name_match("REPORT Report.pdf", &case).unwrap().1, vec![7..13]);
        // Exact matches don't guess.
        assert!(matches("Visual Studio Code.lnk", &terms("vsc")));
        assert!(!matches("Visual Studio Code.lnk", &exact("VSC", true, false)));
        assert!(!matches("Visual Studio Code.lnk", &exact("vsc", false, true)));

        // What Everything's `ww:` takes on this PC: an end, a space, `.`, `-`. Not `_`, a letter or a digit.
        let word = exact("alpha", false, true);
        for yes in ["alpha.txt", "Alpha", "alpha space.txt", "alpha-dash.txt", "the alpha", "(alpha)"] {
            assert!(matches(yes, &word), "{yes}");
        }
        for no in ["alpha_under.txt", "alpha2.txt", "alphaMore.txt", "xalpha.txt", "é_alpha"] {
            assert!(!matches(no, &word), "{no}");
        }
        assert_eq!(name_match("alphabet alpha.txt", &word).unwrap().1, vec![9..14]);

        // The path: Everything's say-so is enough, and nothing else's.
        let context = context();
        let path = Terms::matching("project", Matching { path: true, ..Matching::default() });
        let found = file("notes.txt", r"C:\project");
        let mut read = file("other.txt", r"C:\project");
        read.origin = Some(Origin::Content);
        let named = file("project.txt", r"C:\a");
        let ranked = rank([found, read, named], &path, &context, 10);
        let names: Vec<_> = ranked.iter().map(|r| r.candidate.name.as_str()).collect();
        assert_eq!(names, ["project.txt", "notes.txt"]);
        assert!(ranked[1].highlights.is_empty());
    }

    #[test]
    fn ranking() {
        let context = context();
        let mut app = file("Visual Studio Code.lnk", r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs");
        app.origin = Some(Origin::App);
        let buried = file("code.js", r"C:\Users\me\project\node_modules\x");
        let own = file("code notes.md", r"C:\Users\me\Documents");
        let ranked = rank([buried, own, app], &terms("code"), &context, 10);
        let names: Vec<_> = ranked.iter().map(|r| r.candidate.name.as_str()).collect();
        assert_eq!(names, ["Visual Studio Code.lnk", "code notes.md", "code.js"]);

        // Something opened often and lately beats an equally good match that never was.
        let mut used = file("report.docx", r"C:\Users\me\Documents");
        used.opens = 6;
        used.opened = Some(context.now - 3_600);
        let unused = file("report.pdf", r"C:\Users\me\Documents");
        let ranked = rank([unused, used], &terms("report"), &context, 10);
        assert_eq!(ranked[0].candidate.name, "report.docx");

        // The same file from two sources is one result that keeps both sides' knowledge.
        let mut recent = file("plan.md", r"C:\Users\me\Desktop");
        recent.origin = Some(Origin::Recent);
        recent.opened = Some(context.now);
        let mut found = file("plan.md", r"C:\Users\me\Desktop");
        found.size = Some(42);
        let ranked = rank([recent, found], &terms("plan"), &context, 10);
        assert_eq!(ranked.len(), 1);
        assert_eq!((ranked[0].candidate.origin, ranked[0].candidate.size), (Some(Origin::Recent), Some(42)));

        // Hidden by name, without attributes: below the person's own, and only the place itself, not what's in it.
        let ranked = rank([file("desktop.ini", r"C:\Users\me\Documents"), file("desktop.txt", r"C:\Users\me\Documents")],
                          &terms("desktop"), &context, 10);
        assert_eq!(ranked[0].candidate.name, "desktop.txt");
        assert!(file("ProgramData", r"C:\").looks_hidden());
        assert!(file("$Recycle.Bin", r"C:\").looks_hidden());
        assert!(!file("Microsoft", r"C:\ProgramData").looks_hidden());
        assert!(!file("recovery plan.docx", r"C:\Users\me\Documents").looks_hidden());
    }

    #[test]
    fn contents() {
        let context = context();
        // Found only by what's inside: ranked, without name highlights.
        let mut inside = file("IMG_2291.png", r"C:\Users\me\Pictures");
        inside.origin = Some(Origin::Content);
        inside.content = Some(0.8);
        let starts = file("invoice 2026.pdf", r"C:\Users\me\Documents");
        let contains = file("myinvoices.zip", r"C:\Users\me\Documents");
        let ranked = rank([contains, inside, starts], &terms("invoice"), &context, 10);
        let names: Vec<_> = ranked.iter().map(|r| r.candidate.name.as_str()).collect();
        assert_eq!(names, ["invoice 2026.pdf", "IMG_2291.png", "myinvoices.zip"]);
        assert!(ranked[1].highlights.is_empty());
        // Name and contents both: above the name alone.
        let mut both = file("budget.txt", r"C:\Users\me\Documents");
        both.content = Some(0.7);
        let name_only = file("budget.md", r"C:\Users\me\Documents");
        assert_eq!(rank([name_only, both], &terms("budget"), &context, 10)[0].candidate.name, "budget.txt");
        assert!(content_strength(4.0, 1) > 0.7 && content_strength(1.0, 1) < 0.3);
    }
}
