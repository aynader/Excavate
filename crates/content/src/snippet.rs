//! The line shown under a result: the part of the file where the typed words are closest together, with them marked.
//! Made here rather than by tantivy's generator so unfinished words ("renov…") and misread ones are marked too.

use crate::tokenizer;
use std::ops::Range;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snippet {
    pub text: String,
    /// Byte ranges of `text` that matched.
    pub highlights: Vec<Range<usize>>,
    /// Where in the file's text the first match shown is (bytes), for a recording's moment.
    pub at: Option<usize>,
}

/// The typed words, analyzed once for matching against many texts.
pub struct Words {
    /// For each typed word, its whole words as the index sees them and as they were typed.
    words: Vec<Vec<(String, String)>>,
    prefix: bool,
    case: bool,
    whole_word: bool,
}

impl Words {
    pub fn new(typed: &[String], prefix: bool) -> Words {
        let words = typed.iter().map(|word| tokenizer::typed_wholes(word)).collect();
        Words { words, prefix, case: false, whole_word: false }
    }

    /// Only exact matches, which the index can't tell apart: its words are lowercase, and the parts of joined words are words too.
    pub fn exactly(mut self, case: bool, whole_word: bool) -> Words {
        self.case = case;
        self.whole_word = whole_word;
        self
    }

    fn strict(&self) -> bool {
        self.case || self.whole_word
    }

    /// Which typed word a text word matches, if any. `written` is how the text has it, `alone` whether it stands on its own.
    fn matches(&self, word: &str, written: &str, alone: bool) -> Option<usize> {
        if self.whole_word && !alone {
            return None;
        }
        let last = self.words.len().saturating_sub(1);
        for (index, wholes) in self.words.iter().enumerate() {
            for (whole, typed) in wholes {
                if self.case && !written.starts_with(typed.as_str()) {
                    continue;
                }
                if word == whole {
                    return Some(index);
                }
                if self.prefix && index == last && whole.chars().count() >= 2 && word.starts_with(whole.as_str()) {
                    return Some(index);
                }
                if !self.strict() && whole.len() >= 5 && one_edit(word, whole) {
                    return Some(index);
                }
            }
        }
        None
    }
}

/// Whether what's at `range` stands as a word of its own. As in Everything's `ww:`: `_` joins, like a letter or a digit.
fn alone(text: &str, range: &Range<usize>) -> bool {
    let joins = |c: char| c.is_alphanumeric() || c == '_';
    !text[..range.start].chars().next_back().is_some_and(joins) && !text[range.end..].chars().next().is_some_and(joins)
}

/// Whether two words are at most one letter apart (a substitution, insertion, deletion or swap).
fn one_edit(a: &str, b: &str) -> bool {
    // Nearly every word of a file, without counting its letters.
    if a.is_ascii() && b.is_ascii() && a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if prefix == a.len() && prefix == b.len() {
        return true;
    }
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (rest_a, rest_b) = (a.len() - prefix - suffix.min(a.len() - prefix), b.len() - prefix - suffix.min(b.len() - prefix));
    if rest_a <= 1 && rest_b <= 1 {
        return true;
    }
    // A swap of neighbours: "teh" / "the".
    a.len() == b.len() && rest_a == 2 && a[prefix] == b[prefix + 1] && a[prefix + 1] == b[prefix] && a[prefix + 2..] == b[prefix + 2..]
}

/// How much of a text is looked through first, in bytes. Stored texts run to 256 KB, and telling their words apart takes
/// milliseconds, for each of many files on every keystroke; what's looked for is usually early on.
const FIRST_LOOK: usize = 16 << 10;

/// The start of `text`, about `length` bytes, ending between words.
fn start_of(text: &str, length: usize) -> &str {
    if length >= text.len() {
        return text;
    }
    let mut end = length;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match text[..end].rfind(char::is_whitespace) {
        Some(space) if space > 0 => &text[..space],
        _ => text,
    }
}

/// What in `text` matches.
fn matched_in(text: &str, words: &Words) -> Vec<(usize, Range<usize>)> {
    tokenizer::spans(text).into_iter().filter_map(|(word, range)| {
        let stands = !words.whole_word || alone(text, &range);
        words.matches(&word, &text[range.clone()], stands).map(|index| (index, range))
    }).collect()
}

/// The window with the most different words, then the earliest: how many, and its first match.
fn densest(matched: &[(usize, Range<usize>)], width_bytes: usize) -> (usize, usize) {
    let mut best = (0usize, 0usize);
    for start in 0..matched.len() {
        let from = matched[start].1.start;
        let mut seen = Vec::new();
        for (index, range) in &matched[start..] {
            if range.end - from > width_bytes {
                break;
            }
            if !seen.contains(index) {
                seen.push(*index);
            }
        }
        if seen.len() > best.0 {
            best = (seen.len(), start);
        }
    }
    best
}

/// Up to about `width` characters of `text` around the densest cluster of matches; the start of the text when nothing
/// matches (the name matched).
pub fn make(text: &str, words: &Words, width: usize) -> Snippet {
    let nothing = || Snippet { text: clean(&excerpt(text, 0, width)), highlights: Vec::new(), at: None };
    // With case, what isn't written that way anywhere can't match: no need to tell its words apart.
    if words.case && !words.words.iter().flatten().all(|(_, typed)| text.contains(typed.as_str())) {
        return nothing();
    }
    // Nothing to look for: a preview with nothing typed, or a regular expression, or a typed "&".
    let wanted = words.words.iter().filter(|wholes| !wholes.is_empty()).count();
    if wanted == 0 {
        return nothing();
    }
    let width_bytes = width * 2;
    let mut look = FIRST_LOOK;
    let (matched, best) = loop {
        let part = start_of(text, look);
        let matched = matched_in(part, words);
        let best = densest(&matched, width_bytes);
        // Enough: a place with every word, and all of the line it'll show in view.
        let seen_enough = best.0 == wanted && matched[best.1].1.start + width_bytes <= part.len();
        if part.len() == text.len() || seen_enough {
            break (matched, best);
        }
        look *= 4;
    };
    // An exact match is all of the words: the index only knows they're there in some form. (A typed "&" is no word to it.)
    let incomplete = words.strict() && words.words.iter().enumerate().any(|(index, wholes)| {
        !wholes.is_empty() && !matched.iter().any(|(word, _)| *word == index)
    });
    if matched.is_empty() || incomplete {
        return nothing();
    }
    let first = matched[best.1].1.start;
    // Some words before the first match, from the start of its line when that's close.
    let line_start = text[..first].rfind('\n').map(|at| at + 1).unwrap_or(0);
    let lead = if first - line_start <= width / 3 { line_start } else { back(text, first, width / 4) };
    let piece = excerpt(text, lead, width);
    let end = lead + piece.len();
    let ellipsis = if lead > 0 { "…" } else { "" };
    let mut highlights: Vec<Range<usize>> = matched
        .iter()
        .filter(|(_, range)| range.start >= lead && range.end <= end)
        .map(|(_, range)| (range.start - lead + ellipsis.len())..(range.end - lead + ellipsis.len()))
        .collect();
    // A joined word and its part can both match ("report2024" and "report"): one mark.
    highlights.sort_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
    highlights.dedup_by(|next, kept| next.start < kept.end && {
        kept.end = kept.end.max(next.end);
        true
    });
    let mut snippet = format!("{ellipsis}{}", clean(&piece));
    if end < text.len() {
        snippet.push('…');
    }
    Snippet { text: snippet, highlights, at: Some(first) }
}

/// About `width` characters of `text` from byte `from`, ending at a word boundary.
fn excerpt(text: &str, from: usize, width: usize) -> String {
    let rest = &text[from..];
    let mut end = rest.char_indices().nth(width).map(|(at, _)| at).unwrap_or(rest.len());
    if end < rest.len() {
        if let Some(space) = rest[..end].rfind(char::is_whitespace).filter(|&space| space > end / 2) {
            end = space;
        }
    }
    rest[..end].to_string()
}

/// A byte position about `chars` characters before `at`, at a word's start.
fn back(text: &str, at: usize, chars: usize) -> usize {
    let start = text[..at].char_indices().rev().nth(chars).map(|(index, _)| index).unwrap_or(0);
    // (Past the whole space: some are more than a byte, as a no-break space.)
    match text[start..at].char_indices().find(|(_, c)| c.is_whitespace()) {
        Some((space, c)) => start + space + c.len_utf8(),
        None => start,
    }
}

/// Line breaks and tabs as spaces, byte for byte (so highlight ranges stay right).
fn clean(text: &str) -> String {
    text.chars().map(|c| if c.is_ascii_control() { ' ' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_space_before_the_match() {
        // A no-break space (two bytes) where the excerpt starts: it once cut that space in half.
        let text = "aaaa\u{a0}budget figures";
        let from = back(text, text.find("budget").unwrap(), 3);
        assert_eq!(&text[from..], "budget figures");
    }

    fn marked(snippet: &Snippet) -> Vec<&str> {
        snippet.highlights.iter().map(|range| &snippet.text[range.clone()]).collect()
    }

    #[test]
    fn marks_words_prefixes_and_misreads() {
        let words = Words::new(&["kitchen".into(), "renov".into()], true);
        let snippet = make("Invoice 2291\nfor the kitchen renovation, due March 3", &words, 160);
        assert_eq!(marked(&snippet), ["kitchen", "renovation"]);
        let words = Words::new(&["password".into()], false);
        let snippet = make("Wi-Fi passw0rd: x", &words, 160);
        assert_eq!(marked(&snippet), ["passw0rd"]);
    }

    #[test]
    fn exact_matches() {
        let typed = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
        let marks = |text: &str, words: &Words| make(text, words, 160).highlights.len();

        let case = Words::new(&typed("Report"), false).exactly(true, false);
        assert_eq!(marks("the report was late", &case), 0);
        assert_eq!(marks("The Report was late", &case), 1);
        assert_eq!(marks("MonthlyReport2024", &case), 1, "a part of a joined word is still matched by case alone");
        // Unfinished, with case.
        let unfinished = Words::new(&typed("Repo"), true).exactly(true, false);
        assert_eq!(marks("The Report", &unfinished), 1);
        assert_eq!(marks("The report", &unfinished), 0);
        // Not one letter off.
        let misread = Words::new(&typed("password"), false).exactly(true, false);
        assert_eq!(marks("Wi-Fi passw0rd: x", &misread), 0);

        let whole = Words::new(&typed("user"), false).exactly(false, true);
        assert_eq!(marks("the User name", &whole), 1);
        assert_eq!(marks("getUserName", &whole), 0);
        assert_eq!(marks("get_user_name", &whole), 0);
        assert_eq!(marks("users", &whole), 0);
        assert_eq!(marks("get-user-name", &whole), 1);
        // Every word.
        let both = Words::new(&typed("Budget Lisbon"), false).exactly(true, false);
        assert_eq!(marks("Budget for lisbon", &both), 0);
        assert_eq!(marks("Budget for Lisbon", &both), 2);
        // Punctuation in what's typed.
        let mail = Words::new(&typed("E-Mail"), false).exactly(true, false);
        assert_eq!(marks("Your E-Mail", &mail), 2);
        assert_eq!(marks("your e-mail", &mail), 0);
    }

    #[test]
    fn picks_the_dense_part() {
        let filler = "lorem ipsum dolor ".repeat(40);
        let text = format!("budget once. {filler} the budget for the trip to Lisbon {filler}");
        let words = Words::new(&["budget".into(), "lisbon".into()], false);
        let snippet = make(&text, &words, 80);
        assert!(snippet.text.starts_with('…'));
        assert_eq!(marked(&snippet), ["budget", "Lisbon"]);
    }

    #[test]
    fn nothing_to_look_for() {
        let text = "Some text read from an image";
        // A preview with nothing typed: this once took the whole app down.
        let snippet = make(text, &Words::new(&[], true), 900);
        assert_eq!((snippet.text.as_str(), snippet.highlights.len()), (text, 0));
        assert!(make("", &Words::new(&[], true), 900).highlights.is_empty());
        assert!(make(text, &Words::new(&[], false).exactly(true, true), 900).highlights.is_empty());
        // Typed, but no word to the index.
        assert!(make(text, &Words::new(&["&".to_string()], true), 900).highlights.is_empty());
        assert!(make(text, &Words::new(&["&".to_string()], true).exactly(true, false), 900).highlights.is_empty());
        assert!(make("", &Words::new(&["budget".to_string()], true), 900).highlights.is_empty());
    }

    #[test]
    fn looks_only_as_far_as_it_takes() {
        let typed = |text: &str| text.split(' ').map(String::from).collect::<Vec<_>>();
        let filler = "lorem ipsum dolor ".repeat(4000);
        assert!(filler.len() > 4 * FIRST_LOOK);

        // Early on, and far along: the same either way.
        let early = format!("the budget first. {filler}");
        assert_eq!(marked(&make(&early, &Words::new(&typed("budget"), false), 80)), ["budget"]);
        let late = format!("{filler} the budget last. {filler}");
        let snippet = make(&late, &Words::new(&typed("budget"), false), 80);
        assert_eq!(marked(&snippet), ["budget"]);
        assert!(snippet.text.contains("the budget last"));

        // A place with both words is worth looking on for, past one that has only one.
        let apart = format!("budget once. {filler} the budget for Lisbon. {filler}");
        let snippet = make(&apart, &Words::new(&typed("budget lisbon"), false), 80);
        assert_eq!(marked(&snippet), ["budget", "Lisbon"]);

        // All that's on the line shown is marked, at the very edge of what's first looked through too.
        let at_the_edge = format!("{} budget budget budget {filler}", "x ".repeat(FIRST_LOOK / 2 - 12));
        let snippet = make(&at_the_edge, &Words::new(&typed("budget"), false), 80);
        assert_eq!(marked(&snippet), ["budget", "budget", "budget"]);

        // Nothing there at all.
        assert!(make(&filler, &Words::new(&typed("budget"), false), 80).highlights.is_empty());
        assert!(make(&early, &Words::new(&typed("Budget"), false).exactly(true, false), 80).highlights.is_empty());
    }

    #[test]
    fn edits() {
        assert!(one_edit("password", "passwerd"));
        assert!(one_edit("password", "passwod"));
        assert!(one_edit("password", "passwords"));
        assert!(one_edit("receipt", "reciept"));
        assert!(!one_edit("password", "passwxyz"));
    }
}
