//! Words typed in the search that are really filters ("pdfs from last week invoice"): each becomes a chip in the field
//! (`PDF` `Last week`) and searches as its syntax (`ext:pdf dm:lastweek`), the rest stays words. Only file formats and
//! times (a format's own name, "pdf", counts), and only once the phrase is finished (a space after it): kinds such as
//! "photos" or "invoice" stay words, as they tell a search by meaning what's wanted. A file whose name holds the word
//! ("lastfinalpdf.pdf") is still found: only a whole word is taken, and quotes or Backspace keep the words as words.

/// A filter made from typed words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    /// What the chip says ("PDF", "Last week").
    pub label: String,
    /// What it searches as ("ext:pdf", "dm:lastweek").
    pub syntax: String,
    /// The words as they were typed ("from last week"): Backspace on an empty search gives them back.
    pub words: String,
}

/// Phrases, most words first so "word documents" wins over a shorter match: (words, label, syntax).
const PHRASES: &[(&[&str], &str, &str)] = &[
    (&["last", "week"], "Last week", "dm:lastweek"),
    (&["this", "week"], "This week", "dm:thisweek"),
    (&["last", "month"], "Last month", "dm:lastmonth"),
    (&["this", "month"], "This month", "dm:thismonth"),
    (&["last", "year"], "Last year", "dm:lastyear"),
    (&["this", "year"], "This year", "dm:thisyear"),
    (&["pdf", "files"], "PDF", "ext:pdf"),
    (&["pdf", "documents"], "PDF", "ext:pdf"),
    (&["word", "documents"], "Word", "ext:doc;docx"),
    (&["word", "docs"], "Word", "ext:doc;docx"),
    (&["word", "files"], "Word", "ext:doc;docx"),
    (&["excel", "files"], "Spreadsheets", "ext:xls;xlsx;xlsm;csv;ods"),
    (&["excel", "sheets"], "Spreadsheets", "ext:xls;xlsx;xlsm;csv;ods"),
    (&["powerpoint", "files"], "Presentations", "ext:ppt;pptx;odp"),
    (&["zip", "files"], "Archives", "ext:zip;7z;rar"),
    (&["today"], "Today", "dm:today"),
    (&["yesterday"], "Yesterday", "dm:yesterday"),
    (&["pdfs"], "PDF", "ext:pdf"),
    (&["pdf"], "PDF", "ext:pdf"),
    (&["doc"], "Word", "ext:doc;docx"),
    (&["xls"], "Spreadsheets", "ext:xls;xlsx;xlsm;csv;ods"),
    (&["ppt"], "Presentations", "ext:ppt;pptx;odp"),
    (&["mp3"], "MP3", "ext:mp3"),
    (&["mp4"], "MP4", "ext:mp4"),
    (&["jpg"], "JPEG", "ext:jpg;jpeg"),
    (&["jpeg"], "JPEG", "ext:jpg;jpeg"),
    (&["png"], "PNG", "ext:png"),
    (&["docx"], "Word", "ext:doc;docx"),
    (&["spreadsheets"], "Spreadsheets", "ext:xls;xlsx;xlsm;csv;ods"),
    (&["xlsx"], "Spreadsheets", "ext:xls;xlsx;xlsm;csv;ods"),
    (&["powerpoints"], "Presentations", "ext:ppt;pptx;odp"),
    (&["pptx"], "Presentations", "ext:ppt;pptx;odp"),
    (&["zips"], "Archives", "ext:zip;7z;rar"),
    (&["mp3s"], "MP3", "ext:mp3"),
    (&["mp4s"], "MP4", "ext:mp4"),
    (&["jpgs"], "JPEG", "ext:jpg;jpeg"),
    (&["jpegs"], "JPEG", "ext:jpg;jpeg"),
    (&["pngs"], "PNG", "ext:png"),
];

/// Words that only join a time to the rest ("from last week", "modified today"): they go into the chip with it.
const JOINING: &[&str] = &["from", "since", "in", "during", "modified", "changed", "edited", "made", "created", "saved"];

/// The first filter phrase in `text`, and the text without it, once the phrase is finished (a space after it). None
/// for syntax, quotes or regular expressions, which say exactly what they mean already.
pub fn take(text: &str) -> Option<(Chip, String)> {
    take_except(text, &[])
}

/// As `take`, leaving alone phrases in `declined` (lowercase): ones taken back into words, which are meant as words.
pub fn take_except(text: &str, declined: &[String]) -> Option<(Chip, String)> {
    if !text.ends_with(char::is_whitespace) || text.contains([':', '"', '<', '>', '|', '\\', '*', '?']) {
        return None;
    }
    // The words, with where each starts and ends in `text`.
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(at),
            (true, Some(from)) => {
                words.push((from, at));
                start = None;
            }
            _ => {}
        }
    }
    let lower: Vec<String> = words.iter().map(|&(from, to)| text[from..to].to_lowercase()).collect();
    for first in 0..lower.len() {
        for &(phrase, label, syntax) in PHRASES {
            let end = first + phrase.len();
            if end > lower.len() || !lower[first..end].iter().zip(phrase).all(|(word, wanted)| word == wanted) {
                continue;
            }
            // "from last week": the joining word goes with a time.
            let from = if syntax.starts_with("dm:") && first > 0 && JOINING.contains(&lower[first - 1].as_str()) { first - 1 } else { first };
            let (cut_from, cut_to) = (words[from].0, words[end - 1].1);
            if declined.iter().any(|words| *words == text[cut_from..cut_to].to_lowercase() || *words == lower[first..end].join(" ")) {
                continue;
            }
            let chip = Chip { label: label.to_string(), syntax: syntax.to_string(), words: text[cut_from..cut_to].to_string() };
            let rest = format!("{} {}", text[..cut_from].trim_end(), text[cut_to..].trim_start());
            let rest = rest.trim_start().to_string();
            let rest = if rest.trim().is_empty() { String::new() } else { format!("{} ", rest.trim_end()) };
            return Some((chip, rest));
        }
    }
    None
}

/// The search the chips and the words make together: the chips' syntax first.
pub fn compose(chips: &[Chip], words: &str) -> String {
    if chips.is_empty() {
        return words.to_string();
    }
    let syntax: Vec<&str> = chips.iter().map(|chip| chip.syntax.as_str()).collect();
    format!("{} {}", syntax.join(" "), words).trim_end().to_string() + if words.ends_with(' ') { " " } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> (Vec<String>, String) {
        let mut text = text.to_string();
        let mut labels = Vec::new();
        while let Some((chip, rest)) = take(&text) {
            labels.push(chip.label);
            text = rest;
        }
        (labels, text)
    }

    #[test]
    fn the_mock() {
        assert_eq!(all("pdfs from last week invoice "), (vec!["PDF".to_string(), "Last week".to_string()], "invoice ".into()));
    }

    #[test]
    fn only_once_finished() {
        assert_eq!(take("pdfs"), None, "still typing");
        assert_eq!(take("invoice last wee "), None);
        let (chip, rest) = take("report today ").unwrap();
        assert_eq!((chip.syntax.as_str(), rest.as_str()), ("dm:today", "report "));
    }

    #[test]
    fn words_that_stay_words() {
        for text in ["family photos ", "invoice ", "word ", "excel ", "ext:pdf last week ", "\"last week\" ",
                     "the week before ", "weekly report "] {
            assert_eq!(take(text), None, "{text}");
        }
    }

    #[test]
    fn the_typed_words_come_back() {
        let (chip, rest) = take("budget modified this month ").unwrap();
        assert_eq!((chip.words.as_str(), rest.as_str()), ("modified this month", "budget "));
        let (chip, rest) = take("Word Documents ").unwrap();
        assert_eq!((chip.label.as_str(), chip.words.as_str(), rest.as_str()), ("Word", "Word Documents", ""));
    }

    #[test]
    fn a_formats_own_name() {
        assert_eq!(all("pdf last week "), (vec!["PDF".to_string(), "Last week".to_string()], String::new()));
        assert_eq!(all("pdf reader "), (vec!["PDF".to_string()], "reader ".into()));
    }

    #[test]
    fn taken_back_stays_words() {
        assert_eq!(take_except("pdfs report ", &["pdfs".into()]), None);
        let (chip, _) = take_except("pdfs from last week ", &["pdfs".into()]).unwrap();
        assert_eq!(chip.label, "Last week");
        assert_eq!(take("lastfinalpdf "), None, "one word, not a filter");
    }

    #[test]
    fn composed() {
        let (pdf, rest) = take("pdfs tax ").unwrap();
        assert_eq!(compose(&[pdf], &rest), "ext:pdf tax ");
        assert_eq!(compose(&[], "tax"), "tax");
    }
}
