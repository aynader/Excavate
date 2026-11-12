//! Fuzzy matching of names, for when the exact name is forgotten: each word typed is found in a name exactly, or with
//! a letter or two off (wrong, missing, extra or swapped: "mikro11" and "micor11" find "micro11"), or with letters skipped between its own
//! ("mcr11" finds "micro11"). Every word has to be found; the closer each fits, the better the name. The index scans
//! each distinct name once, across cores, and keeps the best (`Index`'s users: `names::Names::fuzzy`).
//!
//! The letters off are counted by Myers' bit-parallel edit distance (Hyyrö's form for a word anywhere in a text): one
//! pass over the name, a few operations per byte. Letters are compared without case (ASCII); accents and other cases are
//! folded first (`store::fold`: "cafe" finds "Café"), the query's words always, a name beyond ASCII as well as it is.

use std::ops::Range;

/// A query's words, prepared once.
pub struct Matcher {
    words: Vec<Word>,
}

struct Word {
    bytes: Vec<u8>,
    /// For each byte value, the bits of the word's positions that hold it (Myers' pattern masks).
    masks: Box<[u64; 256]>,
    /// Letters off allowed: none up to 3 letters, one up to 7, two from 8.
    most_off: u32,
}

/// How well one word fit a name, and where.
#[derive(Clone, Debug, PartialEq)]
struct Fit {
    score: f32,
    at: Range<usize>,
}

impl Matcher {
    /// None when nothing is left to match (only spaces and punctuation).
    pub fn new(text: &str) -> Option<Matcher> {
        let words: Vec<Word> = text
            .split(|c: char| c.is_whitespace() || c == '*' || c == '"')
            .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|word| !word.is_empty())
            .map(|word| {
                let bytes: Vec<u8> = crate::store::fold(word).into_bytes().into_iter().take(64).collect();
                let mut masks = Box::new([0u64; 256]);
                for (at, &byte) in bytes.iter().enumerate() {
                    masks[byte as usize] |= 1 << at;
                }
                let most_off = match bytes.len() {
                    0..=3 => 0,
                    4..=7 => 1,
                    _ => 2,
                };
                Word { bytes, masks, most_off }
            })
            .collect();
        (!words.is_empty()).then_some(Matcher { words })
    }

    /// How well `name` fits, higher is better (None: a word isn't in it, even loosely). Exact words score most, then
    /// a letter off, then letters skipped; a word at the start of the name or of one of its parts, a little more; and a
    /// name much longer than what matched, a little less.
    pub fn score(&self, name: &[u8]) -> Option<f32> {
        let mut total = 0.0;
        let mut matched = 0;
        for word in &self.words {
            let fit = fit(word, name)?;
            matched += fit.at.len();
            total += fit.score + if starts_part(name, fit.at.start) { 8.0 } else { 0.0 };
        }
        let extra = name.len().saturating_sub(matched) as f32;
        Some(total / self.words.len() as f32 - (extra / 4.0).min(12.0))
    }

    /// `score` of a name as text: beyond ASCII, folded too ("Café" as "cafe"), whichever fits better.
    pub fn score_name(&self, name: &str) -> Option<f32> {
        let raw = self.score(name.as_bytes());
        if name.is_ascii() {
            return raw;
        }
        match (raw, self.score(crate::store::fold(name).as_bytes())) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The bytes of `name` each word matched, for highlighting.
    pub fn highlights(&self, name: &[u8]) -> Vec<Range<usize>> {
        let mut ranges: Vec<Range<usize>> = self.words.iter().filter_map(|word| fit(word, name)).map(|fit| fit.at).collect();
        ranges.sort_by_key(|range| range.start);
        ranges
    }
}

/// Whether `at` starts the name or one of its parts ("micro" in "my_micro11.iso", "11" in "micro 11").
fn starts_part(name: &[u8], at: usize) -> bool {
    at == 0 || matches!(name[at - 1], b' ' | b'.' | b'_' | b'-' | b'(' | b'[' | b'+' | b',')
        || (name[at - 1].is_ascii_lowercase() && name[at].is_ascii_uppercase())
}

fn lower(byte: u8) -> u8 {
    byte.to_ascii_lowercase()
}

fn fit(word: &Word, name: &[u8]) -> Option<Fit> {
    let m = word.bytes.len();
    if m == 0 || name.is_empty() {
        return None;
    }
    // Exactly.
    if m <= name.len() {
        if let Some(at) = name.windows(m).position(|window| window.iter().zip(&word.bytes).all(|(&a, &b)| lower(a) == b)) {
            return Some(Fit { score: 100.0, at: at..at + m });
        }
    }
    // A letter or two off.
    if word.most_off > 0 {
        if let Some((off, end)) = nearest(word, name) {
            if off <= word.most_off {
                let start = end.saturating_sub(m);
                return Some(Fit { score: 100.0 - 25.0 * off as f32, at: start..end });
            }
            // Two letters swapped ("micor11") count as one off: looked for only when the word is that close.
            if off == word.most_off + 1 && m >= 4 && m <= name.len() {
                for swap in 0..m - 1 {
                    let mut swapped = word.bytes.clone();
                    swapped.swap(swap, swap + 1);
                    if let Some(at) = name.windows(m).position(|window| window.iter().zip(&swapped).all(|(&a, &b)| lower(a) == b)) {
                        return Some(Fit { score: 100.0 - 25.0 * word.most_off as f32, at: at..at + m });
                    }
                }
            }
        }
    }
    // Letters skipped: found in order, in a stretch at most three times the word's length.
    let span = skipping(&word.bytes, name)?;
    (span.len() <= 3 * m && m >= 2).then(|| Fit { score: 55.0 * m as f32 / span.len() as f32, at: span })
}

/// The fewest letters off the word is anywhere in `name`, and where that match ends (exclusive).
fn nearest(word: &Word, name: &[u8]) -> Option<(u32, usize)> {
    let m = word.bytes.len();
    let high = 1u64 << (m - 1);
    let mut vp: u64 = if m == 64 { !0 } else { (1u64 << m) - 1 };
    let mut vn: u64 = 0;
    let mut score = m as u32;
    let mut best = (score, 0usize);
    for (at, &byte) in name.iter().enumerate() {
        let eq = word.masks[lower(byte) as usize];
        let xv = eq | vn;
        let xh = (((eq & vp).wrapping_add(vp)) ^ vp) | eq;
        let mut hp = vn | !(xh | vp);
        let hn = vp & xh;
        if hp & high != 0 {
            score += 1;
        } else if hn & high != 0 {
            score -= 1;
        }
        // (The text side's row 0 is all zeros: a match may start anywhere.)
        hp <<= 1;
        let hn = hn << 1;
        vp = hn | !(xv | hp);
        vn = hp & xv;
        if score < best.0 {
            best = (score, at + 1);
        }
    }
    (best.0 < m as u32).then_some(best)
}

/// The shortest stretch of `name` holding the word's letters in order, as fzf finds it: forward to where the last
/// letter is first reached, then back from there to the latest start.
fn skipping(word: &[u8], name: &[u8]) -> Option<Range<usize>> {
    let mut next = 0;
    let mut end = None;
    for (at, &byte) in name.iter().enumerate() {
        if lower(byte) == word[next] {
            next += 1;
            if next == word.len() {
                end = Some(at + 1);
                break;
            }
        }
    }
    let end = end?;
    let mut left = word.len();
    let mut start = end;
    while left > 0 {
        start -= 1;
        if lower(name[start]) == word[left - 1] {
            left -= 1;
        }
    }
    Some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, name: &str) -> Option<f32> {
        Matcher::new(query).unwrap().score(name.as_bytes())
    }

    #[test]
    fn exact_off_and_skipped() {
        // An ISO whose exact name was forgotten.
        assert!(score("micro11", "Micro11.iso").is_some());
        assert!(score("mikro11", "Micro11.iso").is_some());
        assert!(score("micro 11", "Micro11.iso").is_some());
        assert!(score("mcr11", "Micro11.iso").is_some());
        assert!(score("micor11", "X-Lite Micro11 22621.iso").is_some());
        assert!(score("windows lite", "Windows X-Lite.iso").is_some());
        // Exact beats a letter off beats skipped letters.
        assert!(score("micro11", "Micro11.iso") > score("mikro11", "Micro11.iso"));
        assert!(score("mikro11", "Micro11.iso") > score("mcr11", "Micro11.iso"));
        // A shorter name fits better than a long one with the same match.
        assert!(score("budget", "budget.xlsx") > score("budget", "old copies of the budget from last year.xlsx"));
    }

    /// Accents folded, both ways: the query's always, a name's beyond ASCII too.
    #[test]
    fn accents_folded() {
        // As bytes, each é is a letter off (two bytes for one): "Résumé" is two off "resume", one too many for six
        // letters; "été" one off "ete", one too many for three.
        let matcher = Matcher::new("resume").unwrap();
        assert!(matcher.score("Résumé 2026.docx".as_bytes()).is_none());
        assert!(matcher.score_name("Résumé 2026.docx").is_some());
        assert!(Matcher::new("ete").unwrap().score("été.jpg".as_bytes()).is_none());
        assert!(Matcher::new("ete").unwrap().score_name("été.jpg").is_some());
        assert!(Matcher::new("cafe").unwrap().score_name("Café.pdf").is_some());
        assert!(Matcher::new("café").unwrap().score_name("Cafe menu.pdf").is_some());
        assert!(Matcher::new("resume").unwrap().score_name("Résumé 2026.docx").is_some());
        // Through the index's own fuzzy search, which scores its folded copies of names beyond ASCII.
        let mut index = crate::store::Index::new();
        let root = index.push(crate::store::NONE, "C:", crate::store::Meta { size: None, modified: 0, flags: crate::store::DIR });
        for name in ["Café.pdf", "notes.txt", "Résumé 2026.docx", "été.jpg"] {
            index.push(root, name, crate::store::Meta { size: Some(1), modified: 1_700_000_000, flags: 0 });
        }
        index.sort_names();
        let names = crate::names::over(index);
        let found = |text: &str| -> Vec<String> {
            let query = everything::Query { fuzzy: true, ..everything::Query::new(text.to_string(), everything::Sort::NameAscending, 10) };
            names.fuzzy(&query).items.into_iter().map(|item| item.name).collect()
        };
        assert_eq!(found("cafe"), ["Café.pdf"]);
        assert_eq!(found("resume"), ["Résumé 2026.docx"]);
        assert_eq!(found("notes"), ["notes.txt"]);
        assert_eq!(found("ete"), ["été.jpg"]);
    }

    #[test]
    fn not_too_loose() {
        assert!(score("micro11", "notes.txt").is_none());
        assert!(score("abc", "abd.txt").is_none());
        assert!(score("passport", "pastry shop.jpg").is_none());
        // Every word has to be found.
        assert!(score("micro11 vmware", "Micro11.iso").is_none());
        // Skipped letters only within three times the word's length.
        assert!(score("mr", "m_________________________r").is_none());
    }

    #[test]
    fn letters_off_counted() {
        let word = |text: &str| Matcher::new(text).unwrap().words.into_iter().next().unwrap();
        assert_eq!(nearest(&word("micro"), b"xxmicroxx").map(|n| n.0), Some(0));
        assert_eq!(nearest(&word("micro"), b"xxmikroxx").map(|n| n.0), Some(1));
        assert_eq!(nearest(&word("micro"), b"xxmcroxx").map(|n| n.0), Some(1));
        assert_eq!(nearest(&word("microsoft"), b"mircosoft").map(|n| n.0), Some(2));
        assert_eq!(skipping(b"mcr", b"a micro b"), Some(2..6));
    }

    #[test]
    fn highlights_where_words_fit() {
        let matcher = Matcher::new("micro 11").unwrap();
        assert_eq!(matcher.highlights(b"Micro11.iso"), vec![0..5, 5..7]);
    }
}
