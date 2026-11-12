//! A type of file asked for in what's typed:
//!
//! - named as a word of its own: "the pdf of my passport" puts PDFs first, "my passport photo" images;
//! - after `>` or `<`: ">v theory of computation" searches the text in videos, "<pdf lease" PDFs by what they mean.
//!   A letter for a kind (`i`, `v`, `a`, `d`, `c`) or any extension a filter knows (`pdf`, `py`, `html`).

use crate::describe::{self, JOINING, Kind};
use crate::filter::{self, Filter};
use crate::query::Terms;

/// The type a search puts first: one extension, or a kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum First {
    Extension(&'static str),
    Kind(Kind),
}

impl First {
    /// Whether a file by this name is of the type.
    pub fn holds(self, name: &str) -> bool {
        match self {
            First::Extension(extension) => filter::extension(name).is_some_and(|its| its.eq_ignore_ascii_case(extension)),
            First::Kind(kind) => kind.holds(name),
        }
    }

    /// Its extensions, as Everything's `ext:` and the search helper take them.
    pub fn extensions(self) -> Vec<&'static str> {
        match self {
            First::Extension(extension) => vec![extension],
            First::Kind(kind) => kind_filter(kind).extensions().to_vec(),
        }
    }

    /// The filter chip it belongs under.
    pub fn filter(self) -> Filter {
        match self {
            First::Extension(extension) => known(extension).map_or(Filter::All, |(filter, _)| filter),
            First::Kind(kind) => kind_filter(kind),
        }
    }
}

pub fn kind_filter(kind: Kind) -> Filter {
    match kind {
        Kind::Image => Filter::Pictures,
        Kind::Video => Filter::Video,
        Kind::Audio => Filter::Audio,
        Kind::Document => Filter::Documents,
    }
}

/// The filter an extension is under, and the extension as the filter lists it.
fn known(extension: &str) -> Option<(Filter, &'static str)> {
    [Filter::Documents, Filter::Pictures, Filter::Audio, Filter::Video, Filter::Code, Filter::Archives, Filter::Apps]
        .into_iter()
        .find_map(|filter| filter.extensions().iter().find(|known| known.eq_ignore_ascii_case(extension)).map(|known| (filter, *known)))
}

/// Extensions people type as a word to ask for that type. Not those that are everyday words too ("key", "pages",
/// "numbers", "one", "go", "log", "raw", "img", "lock", "ts").
const TYPED: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "csv", "rtf", "odt", "ods", "odp", "epub", "md", "eml", "msg",
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "heic", "heif", "tif", "tiff", "svg", "psd", "dng",
    "mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "wma",
    "mp4", "mkv", "mov", "avi", "wmv", "webm", "m4v",
    "py", "ipynb", "rs", "js", "jsx", "tsx", "cpp", "java", "html", "htm", "css", "json", "yaml", "yml", "xml", "sql", "ps1", "bat",
    "zip", "7z", "rar", "iso", "msi", "exe",
];

/// The type named in `terms` by a word of its own: an extension ("the pdf of my passport", "budget xlsx"), else a kind
/// that isn't the first word ("my passport photo", "lecture recording"; not "photo editor", an app). Not in a file's
/// name ("notes.txt"), nor in Everything's syntax.
pub fn named(terms: &Terms) -> Option<First> {
    if terms.syntax || terms.matching.regex {
        return None;
    }
    let words = words(&terms.text);
    if let Some(extension) = words.iter().find_map(|word| TYPED.iter().find(|typed| **typed == word.as_str())) {
        return Some(First::Extension(extension));
    }
    words.iter().skip(1).find_map(|word| describe::named(word)).map(First::Kind)
}

/// What's left to find by name once the type and the words joining a description are taken out: "passport", of
/// "the pdf of my passport". None when nothing would be left, or nothing was taken out.
pub fn core(terms: &Terms) -> Option<Terms> {
    if terms.syntax || terms.matching.regex {
        return None;
    }
    let all = words(&terms.text);
    let kept: Vec<&String> = all.iter()
        .filter(|word| !JOINING.contains(&word.as_str()) && !TYPED.contains(&word.as_str()) && describe::named(word).is_none())
        .collect();
    if kept.is_empty() || kept.len() == all.len() {
        return None;
    }
    let text = kept.iter().map(|word| word.as_str()).collect::<Vec<_>>().join(" ");
    Some(Terms::matching(&text, terms.matching))
}

/// The words without those that join them ("passport" of "my passport"): names with only those come after names with
/// all of them, and the words inside files are searched by those (no image's text says "my"). None when no word is a
/// joining one, or every word is.
pub fn relaxed(terms: &Terms) -> Option<Terms> {
    if terms.syntax || terms.matching.regex || terms.words.len() < 2 {
        return None;
    }
    let all = words(&terms.text);
    let kept: Vec<&String> = all.iter().filter(|word| !JOINING.contains(&word.as_str())).collect();
    if kept.is_empty() || kept.len() == all.len() {
        return None;
    }
    let text = kept.iter().map(|word| word.as_str()).collect::<Vec<_>>().join(" ");
    Some(Terms::matching(&text, terms.matching))
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.').trim_start_matches('.').to_lowercase())
        .filter(|word| !word.is_empty() && !word.trim_end_matches('.').contains('.'))
        .collect()
}

/// What `>` or `<` in front of the text asks: `>` the words inside (text in images and videos, what's said, a
/// document's text), `<` what it shows or means. A type right after it (no space) narrows it: ">v theory of
/// computation". Without one (or with `i`), images, as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefixed<'a> {
    pub inside: bool,
    /// The kind's filter, and one extension within it ("pdf"). None: images, as before.
    pub kind: Option<(Filter, Option<&'static str>)>,
    /// For videos' words, one of the two (`>vt` on screen only, `>vs` said only); None: both.
    pub part: Option<Part>,
    pub rest: &'a str,
}

/// Which of a video's words: the text it shows, or what's said in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Shown,
    Said,
}

impl Prefixed<'_> {
    /// The line under the search field saying what this search does. Markdown: the letters in bold.
    pub fn help(&self) -> String {
        let kind = self.kind.map(|(filter, _)| filter);
        let extension = self.kind.and_then(|(_, extension)| extension);
        let typed = |noun: &str| match extension {
            Some(extension) => format!("{} files", extension.to_uppercase()),
            None => noun.to_string(),
        };
        if self.inside {
            match (kind, self.part) {
                (Some(Filter::Video), Some(Part::Shown)) => r"Words shown on screen in videos. **\>vs** said only · **\>v** both".into(),
                (Some(Filter::Video), Some(Part::Said)) => r"Words said in videos. **\>vt** on screen only · **\>v** both".into(),
                (Some(Filter::Video), None) if extension.is_none() => r"Words in videos: on screen and said. **\>vt** on screen only · **\>vs** said only".into(),
                (Some(Filter::Video), None) => format!("Words in {}: on screen and said.", typed("videos")),
                (Some(Filter::Audio), _) => format!("Words said in {}.", typed("recordings")),
                (Some(Filter::Documents), _) => format!("Words in {}.", typed("documents")),
                (Some(Filter::Code), _) => format!("Words in {}.", typed("code")),
                _ => format!("Words in {}: the text they show.", typed("images")),
            }
        } else {
            match kind {
                Some(Filter::Video) => format!("What {} show.", typed("videos")),
                Some(Filter::Audio) => format!("{} by what they mean.", capital(&typed("recordings"))),
                Some(Filter::Documents) => format!("{} by what they mean.", capital(&typed("documents"))),
                Some(Filter::Code) => format!("{} by what they mean.", capital(&typed("code"))),
                _ => format!("What {} show.", typed("images")),
            }
        }
    }
}

fn capital(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map(|first| first.to_uppercase().chain(characters).collect()).unwrap_or_default()
}

pub fn prefixed(text: &str) -> Option<Prefixed<'_>> {
    let trimmed = text.trim_start();
    let inside = match trimmed.chars().next()? {
        '>' => true,
        // ("<a|b>" is Everything's group.)
        '<' if !trimmed.contains('>') => false,
        _ => return None,
    };
    let after = &trimmed[1..];
    let end = after.find(char::is_whitespace).unwrap_or(after.len());
    let token = after[..end].to_lowercase();
    // `>vt`, `>vs`: one of a video's two sorts of words.
    let part = match token.as_str() {
        "vt" if inside => Some(Part::Shown),
        "vs" if inside => Some(Part::Said),
        _ => None,
    };
    if part.is_some() {
        return Some(Prefixed { inside, kind: Some((Filter::Video, None)), part, rest: &after[end..] });
    }
    match code(&after[..end]) {
        Some(kind) => Some(Prefixed { inside, kind: Some(kind), part: None, rest: &after[end..] }),
        None => Some(Prefixed { inside, kind: None, part: None, rest: after }),
    }
}

/// A type after `>` or `<`: a kind's letter or name, or an extension.
fn code(token: &str) -> Option<(Filter, Option<&'static str>)> {
    let token = token.trim_start_matches('.').to_lowercase();
    let filter = match token.as_str() {
        "" => return None,
        "i" | "image" | "images" => Filter::Pictures,
        "v" | "video" | "videos" => Filter::Video,
        "a" | "audio" => Filter::Audio,
        "d" | "docs" | "document" | "documents" => Filter::Documents,
        "c" | "code" => Filter::Code,
        _ => return known(&token).filter(|(filter, _)| *filter != Filter::Apps).map(|(filter, extension)| (filter, Some(extension))),
    };
    Some((filter, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_named_in_the_text() {
        let named = |text: &str| named(&Terms::parse(text));
        assert_eq!(named("the pdf of my passport"), Some(First::Extension("pdf")));
        assert_eq!(named("pride and prejudice pdf"), Some(First::Extension("pdf")));
        assert_eq!(named("budget XLSX"), Some(First::Extension("xlsx")));
        assert_eq!(named("my passport photo"), Some(First::Kind(Kind::Image)));
        assert_eq!(named("lecture recording"), Some(First::Kind(Kind::Audio)));
        assert_eq!(named("photo editor"), None);
        assert_eq!(named("notes.txt"), None);
        assert_eq!(named("ext:pdf passport"), None);
        assert_eq!(named("passport"), None);
        assert!(First::Extension("pdf").holds("Passport - Recent.PDF"));
        assert!(!First::Extension("pdf").holds("Full name on Passport .png"));
        assert_eq!(First::Extension("py").filter(), Filter::Code);
    }

    #[test]
    fn what_is_left_to_find() {
        let core = |text: &str| core(&Terms::parse(text)).map(|terms| terms.words);
        assert_eq!(core("the pdf of my passport"), Some(vec!["passport".to_string()]));
        assert_eq!(core("my passport photo"), Some(vec!["passport".to_string()]));
        assert_eq!(core("passport"), None);
        let relaxed = |text: &str| relaxed(&Terms::parse(text)).map(|terms| terms.words);
        assert_eq!(relaxed("my passport"), Some(vec!["passport".to_string()]));
        assert_eq!(relaxed("the beatles abbey road"), Some(vec!["beatles".to_string(), "abbey".to_string(), "road".to_string()]));
        assert_eq!(relaxed("passport"), None);
        assert_eq!(relaxed("passport scan"), None);
        assert_eq!(relaxed("the one"), None);
        assert_eq!(relaxed("ext:pdf my passport"), None);
        assert_eq!(core("the pdf"), None);
        // "pdf that has my resume" once found nothing: "has" had to be in the name or the text.
        assert_eq!(core("pdf that has my resume"), Some(vec!["resume".to_string()]));
        assert_eq!(core("pdf that has my transcript"), Some(vec!["transcript".to_string()]));
        assert_eq!(relaxed("pdf that has my resume"), Some(vec!["pdf".to_string(), "resume".to_string()]));
        assert_eq!(core("the video where he talks about the budget"), Some(vec!["budget".to_string()]));
    }

    #[test]
    fn prefixes() {
        let at = |text: &str| prefixed(text).map(|prefixed| (prefixed.inside, prefixed.kind, prefixed.rest.trim().to_string()));
        assert_eq!(at(">v theory of computation"), Some((true, Some((Filter::Video, None)), "theory of computation".into())));
        assert_eq!(at("<pdf lease"), Some((false, Some((Filter::Documents, Some("pdf"))), "lease".into())));
        assert_eq!(at(">py parser"), Some((true, Some((Filter::Code, Some("py"))), "parser".into())));
        assert_eq!(at(">a sie das"), Some((true, Some((Filter::Audio, None)), "sie das".into())));
        assert_eq!(at("> receipt"), Some((true, None, "receipt".into())));
        assert_eq!(at(">receipt"), Some((true, None, "receipt".into())));
        assert_eq!(at("<a|b>"), None);
        assert_eq!(prefixed(">vt wifi").map(|prefixed| (prefixed.kind, prefixed.part, prefixed.rest.trim())), Some((Some((Filter::Video, None)), Some(Part::Shown), "wifi")));
        assert_eq!(prefixed(">vs wifi").map(|prefixed| prefixed.part), Some(Some(Part::Said)));
        assert_eq!(prefixed("<vs wifi").map(|prefixed| prefixed.part), Some(None));
        assert!(prefixed(">v wifi").unwrap().help().starts_with("Words in videos: on screen and said."));
        assert_eq!(prefixed("<pdf lease").unwrap().help(), "PDF files by what they mean.");
        assert_eq!(prefixed("<a lecture").unwrap().help(), "Recordings by what they mean.");
        assert_eq!(at("theory"), None);
    }
}
