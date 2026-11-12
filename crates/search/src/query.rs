//! What was typed. Plain words become terms matched against names; anything that uses Everything's search syntax
//! (functions like `ext:` or `dm:`, wildcards, `|`, `!`, quotes, paths) is passed to Everything untouched, so power
//! users keep every operator.

/// The search field's toggles for how names match, as in Everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Matching {
    pub case: bool,
    pub whole_word: bool,
    /// Against the full path, not only the name.
    pub path: bool,
    /// What's typed is one regular expression.
    pub regex: bool,
}

impl Matching {
    /// Whether anything but the name's letters decides: only Everything can answer, so the hot tier sits out.
    pub fn only_everything(self) -> bool {
        self.path || self.regex
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Terms {
    /// What was typed, trimmed.
    pub text: String,
    /// The words, in order: lowercased, unless matching case.
    pub words: Vec<String>,
    /// Uses Everything's syntax: the hot tier sits out and Everything gets the text as is.
    pub syntax: bool,
    pub matching: Matching,
}

impl Terms {
    pub fn parse(text: &str) -> Terms {
        Terms::matching(text, Matching::default())
    }

    pub fn matching(text: &str, matching: Matching) -> Terms {
        let text = text.trim().to_string();
        let syntax = !matching.regex && uses_syntax(&text);
        let words = if syntax || matching.regex {
            Vec::new()
        } else if matching.case {
            text.split_whitespace().map(str::to_string).collect()
        } else {
            text.split_whitespace().map(str::to_lowercase).collect()
        };
        Terms { text, words, syntax, matching }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether only Everything can answer this.
    pub fn only_everything(&self) -> bool {
        self.syntax || self.matching.only_everything()
    }
}

/// Whether a word is one of Everything's functions, like `ext:pdf`.
pub fn is_function(word: &str) -> bool {
    word.split_once(':').is_some_and(|(name, _)| FUNCTIONS.contains(&name.to_ascii_lowercase().as_str()))
}

/// Everything's functions and modifiers ("ext:pdf", "dm:today", "size:>1mb", "regex:…", "case:"): the engine's
/// `syntax::FUNCTIONS` and `syntax::MODIFIERS` (this crate doesn't depend on the engine; app's tests check they match).
const FUNCTIONS: &[&str] = &[
    "ext", "type", "dm", "dc", "da", "dr", "rc", "size", "attrib", "attributes", "file", "files", "folder", "folders",
    "path", "nopath", "parent", "parents", "child", "regex", "noregex", "case", "nocase", "wholeword", "wholewords", "ww",
    "noww", "nowholeword", "nowholewords", "wfn", "wholefilename", "exact", "startwith", "endwith", "len", "count",
    "runcount", "daterun", "datemodified", "datecreated", "dateaccessed", "dupe", "sizedupe", "namepartdupe", "dmdupe",
    "dcdupe", "dadupe", "attribdupe", "empty", "root", "infolder", "nosubfolders", "depth", "childcount",
    "childfilecount", "childfoldercount", "sort", "ascending", "descending", "audio", "zip", "doc", "exe", "pic",
    "video", "content", "utf8content", "width", "height", "dimensions", "length", "orientation", "bitdepth",
    "frontmatter", "shell", "filelist", "diacritics", "nodiacritics", "ascii", "utf8",
];

fn uses_syntax(text: &str) -> bool {
    if text.contains(['*', '?', '|', '"', '<', '>', '\\']) {
        return true;
    }
    if text.split_whitespace().any(|word| word.starts_with('!') && word.len() > 1) {
        return true;
    }
    text.split_whitespace().any(|word| is_function(word.trim_start_matches('!')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_syntax() {
        let terms = Terms::parse("  Q3 Report  ");
        assert_eq!(terms.words, ["q3", "report"]);
        assert!(!terms.syntax);
        for syntax in ["ext:pdf tax", "*.rs", "dm:today", "a | b", "!temp notes", r"C:\Users\x", "\"exact phrase\"", "size:>10mb"] {
            assert!(Terms::parse(syntax).syntax, "{syntax}");
        }
        for plain in ["re: meeting notes", "10:30 call", "c# guide", "what's new"] {
            assert!(!Terms::parse(plain).syntax, "{plain}");
        }
        assert!(Terms::parse("   ").is_empty());
    }

    #[test]
    fn toggles() {
        let case = Matching { case: true, ..Matching::default() };
        assert_eq!(Terms::matching("Q3 Report", case).words, ["Q3", "Report"]);
        let regex = Terms::matching(r"^IMG_\d+ ext:pdf", Matching { regex: true, ..Matching::default() });
        assert!(regex.words.is_empty() && !regex.syntax && regex.only_everything());
        let path = Terms::matching("Report", Matching { path: true, ..Matching::default() });
        assert_eq!(path.words, ["report"]);
        assert!(path.only_everything() && !path.syntax);
        assert!(!Terms::parse("report").only_everything() && Terms::parse("ext:pdf").only_everything());
    }
}
