//! The text behind the preview's "Where it matched" and "Text in this image", opened large: all the index keeps of a
//! file (secrets already replaced by their labels when it was read), with what was searched for marked, a recording's
//! and a video's pieces with their moments. Made on a thread of its own; a long document is cut to its start and the
//! places around its matches, so what the window lays out stays small.

use content::snippet::Words;
use content::{Source, Stored};
use std::ops::Range;

/// What's shown from a document's start at most, in bytes, and in all.
const HEAD: usize = 48 << 10;
const MOST: usize = 160 << 10;
/// Pieces laid out at most (a long recording's moments are a piece each).
const PIECES_MOST: usize = 800;
/// Lines kept on each side of a line with a match past the start.
const AROUND: usize = 2;
/// A piece (one item the window lays out) gathers lines up to about this many bytes; a longer line is cut between words.
const PIECE: usize = 700;
/// The matched words listed at most.
const WORDS_LISTED: usize = 8;

/// One part of the text as the window shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Piece {
    /// A heading over it ("Said"), where a part starts.
    pub label: String,
    /// When it's said or shown ("1:23"), for a recording's or a video's.
    pub time: String,
    /// Markdown for a styled text: escaped, with the matches colored.
    pub markdown: String,
    /// The same words without markup: the window measures with it how tall the piece is once wrapped.
    pub plain: String,
    /// Lines left out before it.
    pub gap: bool,
    /// The index of its first match among all, and how many it has.
    pub first: usize,
    pub matches: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    pub pieces: Vec<Piece>,
    pub matches: usize,
    /// The words that matched, as written in the text (each once).
    pub words: Vec<String>,
}

/// What a part of the index's copy is called over it.
fn label(stored: &Stored) -> &'static str {
    match stored.source.unwrap_or_default() {
        Source::Picture => "Text in this image",
        Source::Speech => "Said",
        Source::Video => "In video",
        Source::Text | Source::Mixed => "Where it matched",
    }
}

/// A line of a part: its byte range, and its moment for a timed one.
struct Line {
    range: Range<usize>,
    time: Option<f64>,
}

fn lines(stored: &Stored) -> Vec<Line> {
    let text = &stored.text;
    let timed = matches!(stored.source, Some(Source::Speech | Source::Video));
    let times: Vec<(usize, f64)> = if timed {
        serde_json::from_slice::<serde_json::Value>(&stored.marks).ok()
            .and_then(|marks| marks["times"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|time| Some((time[0].as_u64()? as usize, time[1].as_i64()? as f64 / 1000.0)))
            .filter(|(at, _)| *at <= text.len() && text.is_char_boundary(*at))
            .collect()
    } else {
        Vec::new()
    };
    if !times.is_empty() {
        // Each piece from where it starts to where the next does.
        let mut out = Vec::new();
        if times[0].0 > 0 {
            out.push(Line { range: 0..times[0].0, time: None });
        }
        for (index, (at, seconds)) in times.iter().enumerate() {
            let end = times.get(index + 1).map_or(text.len(), |next| next.0.max(*at));
            out.push(Line { range: *at..end, time: Some(*seconds) });
        }
        return out;
    }
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        out.push(Line { range: at..at + line.len(), time: None });
        at += line.len();
    }
    out
}

/// A line cut in pieces of about `PIECE` bytes, between words.
fn cut(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = range.start;
    while range.end - start > PIECE * 2 {
        let mut end = start + PIECE;
        while !text.is_char_boundary(end) {
            end += 1;
        }
        end = text[end..range.end].find(char::is_whitespace).map_or(range.end, |space| end + space);
        out.push(start..end);
        start = end;
    }
    out.push(start..range.end);
    out
}

/// The markdown of `text[range]`: its lines' leading spaces gone (markdown would take them for code), every character
/// escaped, and the matches in it colored.
fn markdown(text: &str, range: Range<usize>, marks: &[Range<usize>], color: &str) -> String {
    let mut out = String::with_capacity(range.len() + 32);
    let mut first = true;
    for line in text[range].trim_end_matches(['\n', '\r']).split('\n') {
        let line_start = line.as_ptr() as usize - text.as_ptr() as usize;
        let trimmed = line.trim_start().trim_end_matches('\r');
        let start = line_start + (line.len() - line.trim_start().len());
        let end = start + trimmed.len();
        if !first {
            out.push('\n');
        }
        first = false;
        let mut at = start;
        for mark in marks.iter().filter(|mark| mark.start >= start && mark.end <= end) {
            escape_into(&mut out, &text[at..mark.start]);
            out.push_str("<font color=\"");
            out.push_str(color);
            out.push_str("\">");
            escape_into(&mut out, &text[mark.clone()]);
            out.push_str("</font>");
            at = mark.end;
        }
        escape_into(&mut out, &text[at..end]);
    }
    out
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        // (Tabs and other controls as spaces.)
        out.push(if c.is_control() { ' ' } else { c });
    }
}

/// The view of a file's parts in the index (`content::Reader::stored_all`), with `words` marked in `color`.
pub fn build(parts: &[Stored], words: &Words, color: &str) -> View {
    let mut view = View::default();
    let mut shown = 0usize;
    let mut listed: Vec<String> = Vec::new();
    for stored in parts.iter().filter(|stored| !stored.text.trim().is_empty()) {
        let text = &stored.text;
        let marks = content::snippet::matches(text, words);
        for mark in &marks {
            let word = &text[mark.clone()];
            if listed.len() < WORDS_LISTED && !listed.iter().any(|seen| seen.to_lowercase() == word.to_lowercase()) {
                listed.push(word.to_string());
            }
        }
        let lines = lines(stored);
        // Which lines show: the start, and those around a match, as far as the whole allows.
        let matched: Vec<bool> = lines.iter().map(|line| marks.iter().any(|mark| mark.start < line.range.end && mark.end > line.range.start)).collect();
        let mut keep = vec![false; lines.len()];
        for (index, line) in lines.iter().enumerate() {
            if line.range.end <= HEAD {
                keep[index] = true;
            } else if matched[index] {
                for near in index.saturating_sub(AROUND)..(index + AROUND + 1).min(lines.len()) {
                    keep[near] = true;
                }
            }
        }
        // The heading goes over the part's first piece.
        let mut heading = label(stored).to_string();
        let mut gap = false;
        let mut current: Option<(Range<usize>, Option<f64>)> = None;
        let mut pieces: Vec<(Range<usize>, Option<f64>, bool)> = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if !keep[index] || shown >= MOST || view.pieces.len() + pieces.len() + usize::from(current.is_some()) >= PIECES_MOST {
                if let Some((range, time)) = current.take() {
                    pieces.push((range, time, gap));
                }
                gap = true;
                continue;
            }
            shown += line.range.len();
            // A timed line is a piece of its own; others gather up to `PIECE`. A line with a match is one too, so the
            // window marks just its line.
            let joins = line.time.is_none() && !matched[index] && !index.checked_sub(1).is_some_and(|before| matched[before])
                && current.as_ref().is_some_and(|(range, time)| time.is_none() && range.len() + line.range.len() <= PIECE);
            if joins {
                if let Some((range, _)) = current.as_mut() {
                    range.end = line.range.end;
                }
                continue;
            }
            if let Some((range, time)) = current.take() {
                pieces.push((range, time, gap));
                gap = false;
            }
            current = Some((line.range.clone(), line.time));
        }
        if let Some((range, time)) = current.take() {
            pieces.push((range, time, gap));
        }
        for (range, time, gap) in pieces {
            for (index, part) in cut(text, range).into_iter().enumerate() {
                let inside: Vec<Range<usize>> = marks.iter().filter(|mark| mark.start >= part.start && mark.end <= part.end).cloned().collect();
                let markdown = markdown(text, part.clone(), &inside, color);
                if markdown.trim().is_empty() {
                    continue;
                }
                view.pieces.push(Piece {
                    label: std::mem::take(&mut heading),
                    time: if index == 0 { time.map(crate::format::moment).unwrap_or_default() } else { String::new() },
                    markdown,
                    plain: text[part.clone()].trim_end_matches(['\n', '\r']).to_string(),
                    gap: gap && index == 0,
                    first: view.matches,
                    matches: inside.len(),
                });
                view.matches += inside.len();
            }
        }
    }
    view.words = listed;
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(text: &str, source: Source, marks: &str) -> Stored {
        Stored { text: text.into(), kind: String::new(), source: Some(source), marks: marks.as_bytes().to_vec() }
    }

    fn words(typed: &str) -> Words {
        Words::new(&typed.split(' ').map(String::from).collect::<Vec<_>>(), false)
    }

    #[test]
    fn marks_every_match_and_lists_the_words() {
        let text = "Invoice 2291\nfor the kitchen renovation\n  - kitchen tiles: 12 * 4";
        let view = build(&[stored(text, Source::Picture, "")], &words("kitchen"), "#ff0000");
        assert_eq!(view.matches, 2);
        assert_eq!(view.words, ["kitchen"]);
        // A line with a match starts a piece.
        let markdowns: Vec<&str> = view.pieces.iter().map(|piece| piece.markdown.as_str()).collect();
        assert_eq!(view.pieces[0].label, "Text in this image");
        // Escaped (no list, no emphasis), its leading spaces gone, both matches colored.
        assert_eq!(markdowns, ["Invoice 2291", "for the <font color=\"#ff0000\">kitchen</font> renovation",
                               "\\- <font color=\"#ff0000\">kitchen</font> tiles\\: 12 \\* 4"]);
        assert_eq!(view.pieces.iter().map(|piece| (piece.first, piece.matches)).collect::<Vec<_>>(), [(0, 0), (0, 1), (1, 1)]);
    }

    #[test]
    fn a_recording_with_its_moments() {
        let text = "hello there the IBAN is ‹IBAN› okay thanks";
        let marks = r#"{"times": [[0, 0], [12, 2400], [35, 65000]], "kinds": ["iban"]}"#;
        let view = build(&[stored(text, Source::Speech, marks)], &words("iban"), "#f00");
        let times: Vec<&str> = view.pieces.iter().map(|piece| piece.time.as_str()).collect();
        assert_eq!(times, ["0:00", "0:02", "1:05"]);
        assert_eq!(view.pieces[0].label, "Said");
        // The secret stays its label (the word in it marked, as it matches too).
        assert!(view.pieces[1].markdown.contains("‹<font color=\"#f00\">IBAN</font>›"), "{}", view.pieces[1].markdown);
        assert_eq!((view.pieces[1].first, view.pieces[1].matches), (0, 2));
    }

    #[test]
    fn a_long_document_is_cut_to_its_start_and_its_matches() {
        let filler = "lorem ipsum dolor sit amet\n".repeat(20_000);
        let text = format!("start\n{filler}the budget for Lisbon\n{filler}end");
        let view = build(&[stored(&text, Source::Text, "")], &words("budget"), "#f00");
        let shown: usize = view.pieces.iter().map(|piece| piece.markdown.len()).sum();
        assert!(shown < 60 << 10, "{shown}");
        assert_eq!(view.matches, 1);
        let at = view.pieces.iter().position(|piece| piece.matches == 1).unwrap();
        assert!(view.pieces[1..=at].iter().any(|piece| piece.gap), "lines left out before it");
        assert!(view.pieces[at].markdown.contains("Lisbon"));
        assert_eq!(view.pieces[0].label, "Where it matched");
        assert!(view.pieces.iter().skip(1).all(|piece| piece.label.is_empty()));
    }

    #[test]
    fn a_long_recording_is_cut_too() {
        let text = "a few words said here ".repeat(5000);
        let times: Vec<String> = (0..5000).map(|index| format!("[{}, {}]", index * 22, index * 1000)).collect();
        let marks = format!(r#"{{"times": [{}]}}"#, times.join(", "));
        let view = build(&[stored(&text, Source::Speech, &marks)], &words("said"), "#f00");
        assert!(view.pieces.len() <= PIECES_MOST, "{}", view.pieces.len());
    }

    #[test]
    fn a_video_has_both_parts() {
        let shown = stored("SALE 50% OFF", Source::Video, r#"{"times": [[0, 4000]]}"#);
        let said = stored("come in for the sale", Source::Speech, r#"{"times": [[0, 1000]]}"#);
        let view = build(&[shown, said], &words("sale"), "#f00");
        let labels: Vec<&str> = view.pieces.iter().map(|piece| piece.label.as_str()).collect();
        assert_eq!(labels, ["In video", "Said"]);
        assert_eq!(view.matches, 2);
        assert_eq!(view.words, ["SALE"]);
    }
}
