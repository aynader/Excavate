//! The queries a keystroke sends to Everything for the long tail. Measured on a PC whose Everything 1.4 indexes 10
//! million files:
//!
//! - a plain substring query costs ~85-115 ms whatever it matches; a `startwith:` query ~30 ms;
//! - excluding folders (`!\node_modules\`…) costs per match: nothing on 200 matches, +350 ms on 1M, +870 ms on 5M.
//!
//! Everything answers one query at a time and a running query can't be stopped, so an expensive query on an early
//! keystroke ("c") delays the last one. Hence:
//!
//! - one character: no query (the hot tier answers; five million names starting nowhere in particular help nobody);
//! - then a cheap prefix query, and the plain query (no exclusions: ranking pushes dependency folders down);
//! - and, only when the plain query was cut off and matches few enough files for exclusions to stay cheap, a
//!   refinement: the same words without dependency folders, newest first, to bring in what the window missed.
//!
//! "Show all" pages through the complete, unfiltered result set in a chosen order instead.
//!
//! The search field's toggles go on the typed words themselves (`case:`, `ww:`, `path:`, `regex:`), not through Everything's own
//! switches: those would reach the folder and kind filters in the same query too. Measured over those 10 million files, `path:` and
//! `regex:` take 0.7 to 0.85 s against about 0.1 s for a plain word, and about 75 ms with folders chosen.

use crate::filter::Filter;
use crate::query::{Matching, Terms, is_function};
use everything::{Query, Sort};

/// Dependency and junk folders left out of the refinement (they stay in "Show all").
pub const EXCLUDE: &str = r#" !\node_modules\ !\site-packages\ !\.git\ !\$Recycle.Bin\ !\__pycache__\ !\.cargo\registry\ !\Windows\WinSxS\"#;

/// The refinement runs when the plain query matched at most this many files (its cost grows with the matches).
pub const REFINE_UP_TO: u64 = 250_000;

/// Programs, wherever they're installed: they show whichever folders names are limited to. (Store apps like Notepad aren't
/// in the Start menu's shortcuts, so only Everything finds them.)
const PROGRAMS: &str = r#"<ext:exe;lnk;appref-ms <"\Program Files"|"\Start Menu\"|"\AppData\Local\Programs\"|"\Windows\">>"#;

/// Where names are searched: Everything's whole index, or only some folders. A filter on the queries and on the hot tier;
/// Everything goes on indexing everything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    /// Lowercased, ending in a backslash. Empty: everywhere.
    folders: Vec<String>,
    /// The same folders as Everything search syntax, with a space after it.
    syntax: String,
    /// Some folders only, and not even apps from anywhere else.
    no_apps: bool,
}

impl Scope {
    /// Apps show from anywhere.
    pub fn new(folders: &[String]) -> Scope {
        Scope::with_apps(folders, true)
    }

    pub fn with_apps(folders: &[String], apps: bool) -> Scope {
        let folders: Vec<String> = folders.iter().map(|folder| format!("{}\\", folder.trim_end_matches('\\'))).collect();
        let listed = folders.iter().map(|folder| format!("\"{folder}\"")).collect::<Vec<_>>().join("|");
        let syntax = match (folders.is_empty(), apps) {
            (true, _) => String::new(),
            (false, true) => format!("<<{listed}>|{PROGRAMS}> "),
            (false, false) => format!("<{listed}> "),
        };
        let no_apps = !folders.is_empty() && !apps;
        Scope { folders: folders.iter().map(|folder| folder.to_lowercase()).collect(), syntax, no_apps }
    }

    /// Whether apps are left out along with everything outside the folders.
    pub fn leaves_out_apps(&self) -> bool {
        self.no_apps
    }

    /// Whether names are searched everywhere, not only in some folders.
    pub fn everywhere(&self) -> bool {
        self.folders.is_empty()
    }

    pub fn accepts(&self, path: &str) -> bool {
        self.folders.is_empty() || {
            let path = path.to_lowercase();
            self.folders.iter().any(|folder| path.starts_with(folder.as_str()) || path == folder[..folder.len() - 1])
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Names starting with the word: cheap, sent first (single words).
    pub fast: Option<Query>,
    pub main: Query,
    /// Sent after `main` when it was cut off and matched at most `REFINE_UP_TO` files.
    pub refine: Option<Query>,
}

impl Plan {
    pub fn wants_refine(&self, main_total: u64) -> bool {
        self.refine.is_some() && main_total > u64::from(self.main.count) && main_total <= REFINE_UP_TO
    }
}

pub fn ranked(terms: &Terms, filter: Filter, scope: &Scope, count: u32) -> Option<Plan> {
    if terms.is_empty() {
        return None;
    }
    let prefix = format!("{}{}", scope.syntax, filter.everything());
    if terms.syntax {
        let main = Query::new(format!("{prefix}{}", asked(terms)), Sort::NameAscending, count);
        return Some(Plan { fast: None, main, refine: None });
    }
    if terms.text.chars().count() < 2 {
        return None;
    }
    let words = asked(terms);
    let main = Query::new(format!("{prefix}{words}"), Sort::NameAscending, count);
    // Slow already: not a second query on top, which nothing could stop.
    if terms.matching.only_everything() {
        return Some(Plan { fast: None, main, refine: None });
    }
    let fast = match terms.words.as_slice() {
        [word] if !terms.matching.whole_word => {
            let case = if terms.matching.case { "case:" } else { "" };
            Some(Query::new(format!("{prefix}{case}startwith:{}", quote(word)), Sort::NameAscending, count.min(150)))
        }
        _ => None,
    };
    let refine = Some(Query::new(format!("{prefix}{words}{EXCLUDE}"), Sort::ModifiedDescending, count));
    Some(Plan { fast, main, refine })
}

/// Files of the type a search names, by what's left of it once the type and the joining words are out: "ext:pdf
/// passport" for "the pdf of my passport" (`asked`), whose words together match few names.
pub fn typed(core: &Terms, extensions: &[&str], filter: Filter, scope: &Scope, count: u32) -> Option<Query> {
    if core.words.is_empty() || extensions.is_empty() {
        return None;
    }
    Some(Query::new(format!("{}{}file: ext:{} {}", scope.syntax, filter.everything(), extensions.join(";"), asked(core)), Sort::NameAscending, count))
}

/// Names with the words that aren't joining ones (`asked::relaxed`: "passport" of "my passport"), after those with all.
pub fn relaxed(relaxed: &Terms, filter: Filter, scope: &Scope, count: u32) -> Option<Query> {
    if relaxed.words.is_empty() || relaxed.matching.only_everything() {
        return None;
    }
    Some(Query::new(format!("{}{}{}", scope.syntax, filter.everything(), asked(relaxed)), Sort::NameAscending, count))
}

/// Names with any of the words, when none has them all: a last try, so a search finds something ("sie das" under
/// Audio). Ranked word by word.
pub fn loose(terms: &Terms, filter: Filter, scope: &Scope, count: u32) -> Option<Query> {
    if terms.syntax || terms.matching.only_everything() || terms.words.len() < 2 {
        return None;
    }
    let modifiers = modifiers(terms.matching);
    let any = terms.words.iter().map(|word| format!("{modifiers}{}", quote(word))).collect::<Vec<_>>().join("|");
    Some(Query::new(format!("{}{}<{any}>", scope.syntax, filter.everything()), Sort::NameAscending, count))
}

/// Names a letter off a single word nothing has (a letter changed, added, left out, or two swapped): what it was
/// likely meant to be ("invoce": invoice), as the last try. One query, the variants in a group.
pub fn fuzzy(terms: &Terms, filter: Filter, scope: &Scope, count: u32) -> Option<Query> {
    if terms.syntax || terms.matching.only_everything() || terms.matching.whole_word || terms.words.len() != 1 {
        return None;
    }
    let letters: Vec<char> = terms.words[0].chars().collect();
    if letters.len() < 4 || letters.len() > 24 || !letters.iter().all(|c| c.is_alphanumeric()) {
        return None;
    }
    let text = |chars: &[char]| chars.iter().collect::<String>();
    let mut variants: Vec<String> = Vec::new();
    for at in 0..letters.len() {
        // Changed, and one added before it.
        let mut changed = letters.clone();
        changed[at] = '?';
        variants.push(format!("*{}*", text(&changed)));
        let mut added = letters.clone();
        added.insert(at, '?');
        variants.push(format!("*{}*", text(&added)));
        // Left out: shorter words match more, so not below four letters.
        if letters.len() >= 5 {
            let mut left = letters.clone();
            left.remove(at);
            variants.push(text(&left));
        }
        if at + 1 < letters.len() && letters[at] != letters[at + 1] {
            let mut swapped = letters.clone();
            swapped.swap(at, at + 1);
            variants.push(text(&swapped));
        }
    }
    let mut added = letters.clone();
    added.push('?');
    variants.push(format!("*{}*", text(&added)));
    variants.sort();
    variants.dedup();
    let case = if terms.matching.case { "case:" } else { "" };
    let any = variants.iter().map(|variant| format!("{case}{variant}")).collect::<Vec<_>>().join("|");
    Some(Query::new(format!("{}{}<{any}>", scope.syntax, filter.everything()), Sort::NameAscending, count))
}

/// Every result, in `sort` order, a page at a time.
pub fn exhaustive(terms: &Terms, filter: Filter, scope: &Scope, sort: Sort, offset: u32, count: u32) -> Query {
    let mut query = Query::new(format!("{}{}{}", scope.syntax, filter.everything(), asked(terms)), sort, count);
    query.offset = offset;
    query
}

/// A plain word as Everything should see it: quoted when it holds a character Everything would read as syntax.
fn quote(word: &str) -> String {
    if word.contains([' ', ';', ':', '!', '|', '<', '>']) { format!("\"{word}\"") } else { word.to_string() }
}

/// The toggles, as they go in front of a word. Everything takes them in any order.
fn modifiers(matching: Matching) -> String {
    let mut modifiers = String::new();
    if matching.case {
        modifiers.push_str("case:");
    }
    if matching.whole_word {
        modifiers.push_str("ww:");
    }
    if matching.path {
        modifiers.push_str("path:");
    }
    modifiers
}

/// What was typed, as Everything is asked it, with the toggles on.
fn asked(terms: &Terms) -> String {
    let modifiers = modifiers(terms.matching);
    if terms.matching.regex {
        // Everything ignores `ww:` on a regular expression, and has no way to escape a quote. No file name holds one.
        let pattern = terms.text.replace('"', r"\x22");
        let pattern = if terms.matching.whole_word { format!(r"\b(?:{pattern})\b") } else { pattern };
        return format!("{}regex:\"{pattern}\"", modifiers.replace("ww:", ""));
    }
    if terms.syntax {
        return typed_syntax(&terms.text, &modifiers);
    }
    terms.words.iter().map(|word| format!("{modifiers}{}", quote(word))).collect::<Vec<_>>().join(" ")
}

/// Typed syntax with the toggles on its plain words. Functions (`ext:pdf`), groups and `|` stay as they are.
fn typed_syntax(text: &str, modifiers: &str) -> String {
    if modifiers.is_empty() {
        return text.to_string();
    }
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
        }
        if c.is_whitespace() && !quoted {
            pieces.push(std::mem::take(&mut piece));
        } else {
            piece.push(c);
        }
    }
    pieces.push(piece);
    pieces.iter().filter(|piece| !piece.is_empty()).map(|piece| {
        let (not, word) = match piece.strip_prefix('!') {
            Some(word) => ("!", word),
            None => ("", piece.as_str()),
        };
        if is_function(word) || word.contains(['<', '>', '|']) { piece.clone() } else { format!("{not}{modifiers}{word}") }
    }).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letter_off() {
        let everywhere = Scope::default();
        let query = fuzzy(&Terms::parse("invoce"), Filter::All, &everywhere, 50).unwrap();
        for variant in ["*invo?ce*", "*inv?ce*", "invce", "ivnoce", "*invoce?*"] {
            assert!(query.text.contains(variant), "{variant} in {}", query.text);
        }
        assert!(fuzzy(&Terms::parse("abc"), Filter::All, &everywhere, 50).is_none(), "too short");
        assert!(fuzzy(&Terms::parse("two words"), Filter::All, &everywhere, 50).is_none(), "loose does those");
    }

    #[test]
    fn plans() {
        let everywhere = Scope::default();
        let plan = ranked(&Terms::parse("Budget"), Filter::All, &everywhere, 300).unwrap();
        assert_eq!(plan.fast.as_ref().unwrap().text, "startwith:budget");
        assert_eq!(plan.main.text, "budget");
        assert_eq!(plan.refine.as_ref().unwrap().text, format!("budget{EXCLUDE}"));
        assert_eq!(plan.refine.as_ref().unwrap().sort, Sort::ModifiedDescending);
        assert!(!plan.wants_refine(212), "everything already came back");
        assert!(plan.wants_refine(23_387));
        assert!(!plan.wants_refine(1_000_000), "too broad: exclusions would cost more than they bring");

        let plan = ranked(&Terms::parse("q3 report"), Filter::Documents, &everywhere, 200).unwrap();
        assert!(plan.fast.is_none());
        assert!(plan.main.text.starts_with("file: ext:pdf;") && plan.main.text.ends_with(" q3 report"));

        let plan = ranked(&Terms::parse("ext:pdf dm:today"), Filter::All, &everywhere, 100).unwrap();
        assert_eq!((plan.main.text.as_str(), plan.refine.is_none()), ("ext:pdf dm:today", true));
        assert!(ranked(&Terms::parse(" "), Filter::All, &everywhere, 100).is_none());
        assert!(ranked(&Terms::parse("c"), Filter::All, &everywhere, 100).is_none(), "one letter: the hot tier alone");
        assert_eq!(ranked(&Terms::parse("re: notes"), Filter::All, &everywhere, 10).unwrap().main.text, "\"re:\" notes");

        let page = exhaustive(&Terms::parse("budget"), Filter::Folders, &everywhere, Sort::ModifiedDescending, 200, 100);
        assert_eq!((page.text.as_str(), page.offset), ("folder: budget", 200));
    }

    #[test]
    fn toggles() {
        let everywhere = Scope::default();
        let on = |case, whole_word, path, regex| Matching { case, whole_word, path, regex };
        let main = |text: &str, matching| ranked(&Terms::matching(text, matching), Filter::All, &everywhere, 10).unwrap().main.text;

        assert_eq!(main("Q3 Report", on(true, false, false, false)), "case:Q3 case:Report");
        assert_eq!(main("q3 report", on(false, true, false, false)), "ww:q3 ww:report");
        assert_eq!(main("q3", on(false, false, true, false)), "path:q3");
        assert_eq!(main("Q3", on(true, true, true, false)), "case:ww:path:Q3");
        assert_eq!(main("re: x", on(true, false, false, false)), "case:\"re:\" case:x");

        // One regular expression, whatever it looks like, and it can't be typed out of its quotes.
        assert_eq!(main(r"^IMG_\d+ ext:pdf", on(false, false, false, true)), r#"regex:"^IMG_\d+ ext:pdf""#);
        assert_eq!(main(r#"a"b"#, on(false, false, false, true)), r#"regex:"a\x22b""#);
        assert_eq!(main("^a", on(true, false, true, true)), r#"case:path:regex:"^a""#);
        assert_eq!(main("a|b", on(false, true, false, true)), r#"regex:"\b(?:a|b)\b""#);

        // Only the typed words: never the folders or the kinds.
        let one = Scope::new(&[r"C:\One".to_string()]);
        let plan = ranked(&Terms::matching("Plan", on(true, false, false, false)), Filter::Folders, &one, 10).unwrap();
        assert_eq!(plan.main.text, format!(r#"<<"C:\One\">|{PROGRAMS}> folder: case:Plan"#));
        assert_eq!(plan.fast.unwrap().text, format!(r#"<<"C:\One\">|{PROGRAMS}> folder: case:startwith:Plan"#));

        // What can use the cheap and the second query, and what can't.
        let plan = ranked(&Terms::matching("plan", on(false, true, false, false)), Filter::All, &everywhere, 10).unwrap();
        assert!(plan.fast.is_none() && plan.refine.is_some());
        let plan = ranked(&Terms::matching("plan", on(false, false, true, false)), Filter::All, &everywhere, 10).unwrap();
        assert!(plan.fast.is_none() && plan.refine.is_none());

        // In typed syntax, on the plain words.
        assert_eq!(main("ext:pdf Plan !Old", on(true, false, false, false)), "ext:pdf case:Plan !case:Old");
        assert_eq!(main("\"Q3 Plan\" | x", on(true, false, false, false)), "case:\"Q3 Plan\" | case:x");
        assert_eq!(main("*.rs", on(false, true, false, false)), "ww:*.rs");

        // And on every page of "Show all".
        let page = exhaustive(&Terms::matching("Plan", on(true, false, false, false)), Filter::All, &everywhere, Sort::NameAscending, 0, 10);
        assert_eq!(page.text, "case:Plan");
    }

    #[test]
    fn scopes() {
        let everywhere = Scope::default();
        assert!(everywhere.accepts(r"D:\anything\at\all.txt"));

        let one = Scope::new(&[r"C:\Users\me\Documents\".to_string()]);
        assert_eq!(ranked(&Terms::parse("budget"), Filter::All, &one, 10).unwrap().main.text, format!(r#"<<"C:\Users\me\Documents\">|{PROGRAMS}> budget"#));
        assert!(one.accepts(r"c:\users\ME\documents\a.pdf"));
        assert!(one.accepts(r"C:\Users\me\Documents"));
        assert!(!one.accepts(r"C:\Users\me\Documents2\a.pdf"));

        let two = Scope::new(&[r"C:\One".to_string(), r"D:\Two".to_string()]);
        let plan = ranked(&Terms::parse("q3 report"), Filter::Documents, &two, 10).unwrap();
        assert!(plan.main.text.starts_with(r#"<<"C:\One\"|"D:\Two\">|<ext:exe"#) && plan.main.text.ends_with(" q3 report"));
        assert_eq!(exhaustive(&Terms::parse("x"), Filter::All, &two, Sort::NameAscending, 0, 10).text, format!(r#"<<"C:\One\"|"D:\Two\">|{PROGRAMS}> x"#));
        assert!(two.accepts(r"D:\Two\sub\file.txt") && !two.accepts(r"E:\Two\file.txt"));
        assert!(!two.leaves_out_apps() && !everywhere.leaves_out_apps());

        // Apps left out too: only the folders.
        let only = Scope::with_apps(&[r"C:\One".to_string(), r"D:\Two".to_string()], false);
        assert!(only.leaves_out_apps());
        assert_eq!(ranked(&Terms::parse("x1"), Filter::All, &only, 10).unwrap().main.text, r#"<"C:\One\"|"D:\Two\"> x1"#);
        // Nowhere in particular: nothing is left out, whatever the switch says.
        let nowhere = Scope::with_apps(&[], false);
        assert!(nowhere.everywhere() && !nowhere.leaves_out_apps());
        assert_eq!(ranked(&Terms::parse("x1"), Filter::All, &nowhere, 10).unwrap().main.text, "x1");
    }
}
