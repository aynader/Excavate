//! Everything's search syntax, read into a tree: what Excavate sends (its scope, kind filters, toggles and Leave out
//! list) and what people type.
//!
//! - A space is AND, `|` is OR (it binds tighter: `a b|c` is `a (b or c)`), `!` is NOT, `< >` group, `"…"` quotes.
//! - A word is looked for in the name, ignoring case and accents. With a `\` in it, in the full path; one that starts
//!   with a drive and ends with `\` (`"C:\Users\"`) keeps the search inside that folder.
//! - `*` and `?` match the whole name (or path).
//! - Modifiers go in front of a word, in any order: `case:` `ww:` (whole word) `path:` `diacritics:` `nocase:` `noww:` `nopath:` `nodiacritics:`
//!   `file:` `folder:`. On their own (`file: ext:pdf`), `file:` and `folder:` keep only files or folders.
//! - Functions: `ext:a;b`, `regex:…`, `startwith:` `endwith:` `wfn:` (whole name), `dm:` `dc:` `da:` (dates
//!   modified, made, opened), `size:`, `attrib:`, `len:`, `parent:` (directly in a folder), `depth:`, `childcount:`,
//!   `root:`, `empty:`, the `dupe:` kinds, `count:`, `sort:`, and Everything's kind macros (`audio:` `pic:` `video:`
//!   `doc:` `zip:` `exe:`): `FUNCTIONS` has them all. Those this index can't answer (`content:`, `width:`…, which the
//!   app answers itself) match nothing, and say so in `Parsed::unsupported`.

use crate::store::fold;

#[derive(Debug)]
pub enum Node {
    All,
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Leaf(Test),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Any,
    Start,
    End,
    Whole,
}

#[derive(Debug)]
pub struct Text {
    /// Folded: what the scan looks for, and what's compared without case.
    pub folded: Vec<u8>,
    /// As typed, when case matters.
    pub exact: Option<Vec<u8>>,
    /// Lowercase with its accents, when they matter (`diacritics:`): "resume" no longer finds "Résumé".
    pub accents: Option<Vec<u8>>,
    pub whole_word: bool,
    pub anchor: Anchor,
    /// Against the full path.
    pub path: bool,
}

impl Text {
    /// Found by the scan of folded names alone: nothing more to check on the name as it is.
    pub fn plain(&self) -> bool {
        self.exact.is_none() && self.accents.is_none()
    }

    /// Whether `name` (or a path) has it with its accents, when they matter.
    pub fn accents_in(&self, name: &[u8]) -> bool {
        self.accents.as_ref().is_none_or(|accents| {
            crate::search::find(String::from_utf8_lossy(name).to_lowercase().as_bytes(), accents, self.anchor, self.whole_word)
        })
    }
}

#[derive(Debug)]
pub enum Test {
    Text(Text),
    /// `*` any run, `?` one character, over the whole (folded) name or path; `exact` when case matters.
    Glob { folded: Vec<u8>, exact: Option<Vec<u8>>, path: bool },
    Regex { regex: regex::bytes::Regex, path: bool },
    /// Extensions, lowercased.
    Ext(Vec<String>),
    Files,
    Folders,
    /// Seconds since 1970, both ends included.
    Modified(i64, i64),
    /// When it was made (`dc:`) or last opened (`da:`), the same way.
    Created(i64, i64),
    Accessed(i64, i64),
    /// Bytes, both ends included (files only).
    Size(u64, u64),
    /// Inside this folder (the path as typed, found when the search runs).
    Under(String),
    /// Directly in this folder.
    Parent(String),
    /// With no parent folder: the drives, as Everything has `root:`.
    Root,
    /// The name's length in characters, both ends included (`len:`).
    Len(u64, u64),
    /// Every one of these attributes (`attrib:`, Windows' FILE_ATTRIBUTE_* bits: r read-only, h hidden, s system, d
    /// folder, a archive, n normal, t temporary, p sparse, l link, c compressed, o offline, i not indexed, e
    /// encrypted, v integrity, as Everything's letters).
    Attrib(u32),
    /// How many folders are above it (`depth:`, `parents:`; a drive's is 0), both ends included.
    Depth(u64, u64),
    /// A folder holding this many entries (`childcount:`), files (`childfilecount:`) or folders
    /// (`childfoldercount:`): 0, 1 or 2; `empty:` is a folder holding none.
    Children(u8, u64, u64),
    /// A folder holding something that matches (`child:`).
    Child(Box<Node>),
    /// A name (ignoring case) that more than one entry has (`dupe:`), or a file size (`sizedupe:`).
    Dupe,
    SizeDupe,
    /// Files sharing something else with another: `namepartdupe:` (the name without its extension), `dmdupe:`,
    /// `dcdupe:`, `dadupe:` (a date), `attribdupe:` (the attributes).
    KeyDupe(DupeKey),
    /// Windows' name for its type contains this, lowercased (`type:`: "text document", "png file", "file folder").
    Type(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DupeKey {
    NamePart,
    Modified,
    Created,
    Accessed,
    Attributes,
}

/// An order asked for inside the search (`sort:`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Path,
    Size,
    Extension,
    Modified,
    Created,
    Accessed,
}

pub struct Parsed {
    pub root: Node,
    /// `count:`: no more results than this. `sort:`, `ascending:`, `descending:`: the order asked for in the search.
    pub count: Option<usize>,
    pub sort: Option<SortKey>,
    pub descending: Option<bool>,
    /// Functions Everything has that this index can't answer: they match nothing (matching everything would
    /// show every entry for them).
    pub unsupported: Vec<String>,
}

#[derive(Clone, Copy, Default)]
struct Mods {
    case: bool,
    whole_word: bool,
    path: bool,
    files: bool,
    folders: bool,
    diacritics: bool,
}

#[derive(Debug, PartialEq)]
enum Token {
    /// A term: its text, and which characters came quoted.
    Term(String, Vec<bool>),
    Or,
    Not,
    Open,
    Close,
    Space,
}

fn lex(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut term = String::new();
    let mut quoted_mask: Vec<bool> = Vec::new();
    let mut quoted = false;
    let flush = |tokens: &mut Vec<Token>, term: &mut String, mask: &mut Vec<bool>| {
        if !term.is_empty() {
            tokens.push(Token::Term(std::mem::take(term), std::mem::take(mask)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            quoted = !quoted;
            i += 1;
            // "" stands for an empty phrase: keep the term alive.
            continue;
        }
        if quoted {
            term.push(c);
            quoted_mask.push(true);
            i += 1;
            continue;
        }
        match c {
            c if c.is_whitespace() => {
                flush(&mut tokens, &mut term, &mut quoted_mask);
                if tokens.last() != Some(&Token::Space) {
                    tokens.push(Token::Space);
                }
            }
            // (`filelist:a.txt|b.txt`: the names are its value, not an OR.)
            '|' if term.to_ascii_lowercase().starts_with("filelist:") => {
                term.push(c);
                quoted_mask.push(false);
            }
            '|' => {
                flush(&mut tokens, &mut term, &mut quoted_mask);
                tokens.push(Token::Or);
            }
            '!' if term.is_empty() => tokens.push(Token::Not),
            // A comparison right after a function's colon: `size:>1mb`, `dm:<=2024`.
            '<' | '>' | '=' if term.ends_with(':') && term.len() > 1 => {
                term.push(c);
                quoted_mask.push(false);
            }
            '<' | '>' | '=' if term.ends_with(['<', '>']) && term.contains(':') => {
                term.push(c);
                quoted_mask.push(false);
            }
            '<' => {
                flush(&mut tokens, &mut term, &mut quoted_mask);
                tokens.push(Token::Open);
            }
            '>' => {
                flush(&mut tokens, &mut term, &mut quoted_mask);
                tokens.push(Token::Close);
            }
            c => {
                term.push(c);
                quoted_mask.push(false);
            }
        }
        i += 1;
    }
    flush(&mut tokens, &mut term, &mut quoted_mask);
    tokens
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    unsupported: Vec<String>,
    count: Option<usize>,
    sort: Option<SortKey>,
    descending: Option<bool>,
}

impl Parser {
    fn peek_past_space(&self) -> Option<&Token> {
        let mut at = self.at;
        while self.tokens.get(at) == Some(&Token::Space) {
            at += 1;
        }
        self.tokens.get(at)
    }

    fn skip_space(&mut self) {
        while self.tokens.get(self.at) == Some(&Token::Space) {
            self.at += 1;
        }
    }

    fn and(&mut self, mods: Mods) -> Node {
        let mut items = Vec::new();
        loop {
            self.skip_space();
            match self.tokens.get(self.at) {
                None | Some(Token::Close) => break,
                Some(Token::Or) => {
                    // A stray `|`: nothing to join.
                    self.at += 1;
                }
                _ => items.push(self.or(mods)),
            }
        }
        match items.len() {
            0 => Node::All,
            1 => items.pop().unwrap(),
            _ => Node::And(items),
        }
    }

    fn or(&mut self, mods: Mods) -> Node {
        let mut items = vec![self.unary(mods)];
        while self.peek_past_space() == Some(&Token::Or) {
            self.skip_space();
            self.at += 1;
            self.skip_space();
            match self.tokens.get(self.at) {
                None | Some(Token::Close) => break,
                _ => items.push(self.unary(mods)),
            }
        }
        if items.len() == 1 { items.pop().unwrap() } else { Node::Or(items) }
    }

    fn unary(&mut self, mods: Mods) -> Node {
        match self.tokens.get(self.at) {
            Some(Token::Not) => {
                self.at += 1;
                Node::Not(Box::new(self.unary(mods)))
            }
            Some(Token::Open) => {
                self.at += 1;
                let inner = self.and(mods);
                if self.tokens.get(self.at) == Some(&Token::Close) {
                    self.at += 1;
                }
                inner
            }
            Some(Token::Term(..)) => {
                let Some(Token::Term(text, mask)) = self.tokens.get(self.at) else { unreachable!() };
                let (text, mask) = (text.clone(), mask.clone());
                self.at += 1;
                // Modifiers in front of a group: `case:<a|b>`.
                if let Some(mods) = modifiers_only(&text, mods) {
                    if self.tokens.get(self.at) == Some(&Token::Open) {
                        return self.unary(mods);
                    }
                }
                self.term(&text, &mask, mods)
            }
            _ => {
                self.at += 1;
                Node::All
            }
        }
    }

    fn term(&mut self, text: &str, mask: &[bool], mut mods: Mods) -> Node {
        let (text, mask) = escapes(text, mask);
        let (text, mask) = (text.as_str(), mask.as_slice());
        let mut rest = text;
        let mut rest_mask = mask;
        // Functions and modifiers, as long as they're unquoted.
        loop {
            let Some(colon) = rest.find(':') else { break };
            let name_len = rest[..colon].chars().count();
            if rest_mask[..name_len + 1].iter().any(|&q| q) {
                break;
            }
            let name = rest[..colon].to_ascii_lowercase();
            let value = &rest[colon + 1..];
            let value_mask = &rest_mask[name_len + 1..];
            match name.as_str() {
                "case" => mods.case = true,
                "nocase" => mods.case = false,
                "ww" | "wholeword" | "wholewords" => mods.whole_word = true,
                "noww" | "nowholeword" | "nowholewords" => mods.whole_word = false,
                "path" => mods.path = true,
                "nopath" => mods.path = false,
                "file" | "files" => mods.files = true,
                "folder" | "folders" => mods.folders = true,
                "diacritics" => mods.diacritics = true,
                "nodiacritics" => mods.diacritics = false,
                "ascii" | "utf8" | "noregex" => {}
                _ => {
                    if FUNCTIONS.contains(&name.as_str()) {
                        return self.function(&name, value, mods);
                    }
                    // Not a function: "re:" or "10:30" is text.
                    break;
                }
            }
            rest = value;
            rest_mask = value_mask;
        }
        let kind = kind_of(mods);
        if rest.is_empty() {
            return kind.unwrap_or(Node::All);
        }
        let leaf = self.text(rest, mods);
        match kind {
            Some(kind) => Node::And(vec![kind, leaf]),
            None => leaf,
        }
    }

    /// A word (or wildcard, or path) with its modifiers.
    fn text(&mut self, text: &str, mods: Mods) -> Node {
        let has_slash = text.contains('\\');
        let path = mods.path || has_slash;
        if text.contains(['*', '?']) {
            let folded = fold(text).into_bytes();
            return Node::Leaf(Test::Glob { folded, exact: mods.case.then(|| text.as_bytes().to_vec()), path });
        }
        // "C:\Users\": inside that folder (a path that starts with a drive can only be found at a path's start).
        let bytes = text.as_bytes();
        if !mods.case && !mods.whole_word && bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
            && bytes[2] == b'\\' && text.ends_with('\\') {
            return Node::Leaf(Test::Under(text.trim_end_matches('\\').to_string()));
        }
        let anchor = if !mods.path && bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && has_slash {
            // A full path without its trailing backslash: the path starts with it.
            Anchor::Start
        } else {
            Anchor::Any
        };
        Node::Leaf(Test::Text(Text { folded: fold(text).into_bytes(), exact: mods.case.then(|| text.as_bytes().to_vec()),
                                     accents: mods.diacritics.then(|| text.to_lowercase().into_bytes()), whole_word: mods.whole_word, anchor, path }))
    }

    fn function(&mut self, name: &str, value: &str, mods: Mods) -> Node {
        let kind = kind_of(mods);
        let node = match name {
            "ext" => Node::Leaf(Test::Ext(value.split(';').map(|e| e.trim().trim_start_matches('.').to_lowercase()).filter(|e| !e.is_empty()).collect())),
            "regex" => {
                let regex = regex::bytes::RegexBuilder::new(value).case_insensitive(!mods.case).build();
                match regex {
                    Ok(regex) => Node::Leaf(Test::Regex { regex, path: mods.path }),
                    // As Everything: a regex that doesn't make sense matches nothing.
                    Err(_) => Node::Not(Box::new(Node::All)),
                }
            }
            "startwith" | "endwith" | "wfn" | "wholefilename" | "exact" => {
                if value.is_empty() {
                    Node::All
                } else {
                    let anchor = match name { "startwith" => Anchor::Start, "endwith" => Anchor::End, _ => Anchor::Whole };
                    Node::Leaf(Test::Text(Text { folded: fold(value).into_bytes(), exact: mods.case.then(|| value.as_bytes().to_vec()),
                                                 accents: mods.diacritics.then(|| value.to_lowercase().into_bytes()), whole_word: false, anchor, path: mods.path }))
                }
            }
            "dm" | "datemodified" => match dates(value, crate::when::now(), crate::when::utc_offset()) {
                Some((from, to)) => Node::Leaf(Test::Modified(from, to)),
                None => Node::Not(Box::new(Node::All)),
            },
            "dc" | "datecreated" => match dates(value, crate::when::now(), crate::when::utc_offset()) {
                Some((from, to)) => Node::Leaf(Test::Created(from, to)),
                None => Node::Not(Box::new(Node::All)),
            },
            "da" | "dateaccessed" => match dates(value, crate::when::now(), crate::when::utc_offset()) {
                Some((from, to)) => Node::Leaf(Test::Accessed(from, to)),
                None => Node::Not(Box::new(Node::All)),
            },
            "size" => match sizes(value) {
                Some((from, to)) => Node::Leaf(Test::Size(from, to)),
                None => Node::Not(Box::new(Node::All)),
            },
            "parent" | "infolder" | "nosubfolders" => Node::Leaf(Test::Parent(value.trim_end_matches('\\').to_string())),
            "root" => Node::Leaf(Test::Root),
            "depth" | "parents" => match counts(value) {
                Some((from, to)) => Node::Leaf(Test::Depth(from, to)),
                None => Node::Not(Box::new(Node::All)),
            },
            "childcount" | "childfilecount" | "childfoldercount" => match counts(value) {
                Some((from, to)) => {
                    let what = match name { "childfilecount" => 1, "childfoldercount" => 2, _ => 0 };
                    Node::And(vec![Node::Leaf(Test::Folders), Node::Leaf(Test::Children(what, from, to))])
                }
                None => Node::Not(Box::new(Node::All)),
            },
            "empty" => Node::And(vec![Node::Leaf(Test::Folders), Node::Leaf(Test::Children(0, 0, 0))]),
            "child" if !value.is_empty() => Node::And(vec![Node::Leaf(Test::Folders), Node::Leaf(Test::Child(Box::new(self.text(value, mods))))]),
            "dupe" | "sizedupe" => {
                let dupe = Node::Leaf(if name == "dupe" { Test::Dupe } else { Test::SizeDupe });
                if value.is_empty() { dupe } else { Node::And(vec![dupe, self.text(value, mods)]) }
            }
            "count" => {
                self.count = value.trim().parse().ok();
                Node::All
            }
            "sort" => {
                let value = value.to_ascii_lowercase();
                let (key, order) = match value.rsplit_once('-') {
                    Some((key, "ascending")) => (key.to_string(), Some(false)),
                    Some((key, "descending")) => (key.to_string(), Some(true)),
                    _ => (value.clone(), None),
                };
                self.sort = match key.as_str() {
                    "name" => Some(SortKey::Name),
                    "path" => Some(SortKey::Path),
                    "size" => Some(SortKey::Size),
                    "ext" | "extension" | "type" => Some(SortKey::Extension),
                    "dm" | "date-modified" | "datemodified" | "date_modified" => Some(SortKey::Modified),
                    "dc" | "date-created" | "datecreated" | "date_created" => Some(SortKey::Created),
                    "da" | "date-accessed" | "dateaccessed" | "date_accessed" => Some(SortKey::Accessed),
                    _ => {
                        self.unsupported.push(format!("sort:{value}"));
                        self.sort
                    }
                };
                if order.is_some() {
                    self.descending = order;
                }
                Node::All
            }
            "ascending" | "descending" => {
                self.descending = Some(name == "descending");
                Node::All
            }
            "type" if !value.is_empty() => Node::Leaf(Test::Type(value.to_lowercase())),
            "shell" => match crate::known::folder(value) {
                Some(path) => Node::Leaf(Test::Under(path)),
                None => {
                    self.unsupported.push(format!("shell:{value}"));
                    Node::Not(Box::new(Node::All))
                }
            },
            "filelist" => {
                let names: Vec<Node> = value.split('|').map(str::trim).filter(|name| !name.is_empty()).map(|name| {
                    Node::Leaf(Test::Text(Text { folded: fold(name).into_bytes(), exact: mods.case.then(|| name.as_bytes().to_vec()), accents: None,
                                                 whole_word: false, anchor: Anchor::Whole, path: false }))
                }).collect();
                if names.is_empty() { Node::Not(Box::new(Node::All)) } else { Node::Or(names) }
            }
            "namepartdupe" | "dmdupe" | "dcdupe" | "dadupe" | "attribdupe" => {
                let key = match name {
                    "namepartdupe" => DupeKey::NamePart,
                    "dmdupe" => DupeKey::Modified,
                    "dcdupe" => DupeKey::Created,
                    "dadupe" => DupeKey::Accessed,
                    _ => DupeKey::Attributes,
                };
                let dupe = Node::Leaf(Test::KeyDupe(key));
                if value.is_empty() { dupe } else { Node::And(vec![dupe, self.text(value, mods)]) }
            }
            "len" => match counts(value) {
                Some((from, to)) => Node::Leaf(Test::Len(from, to)),
                None => Node::Not(Box::new(Node::All)),
            },
            "attrib" | "attributes" => {
                let mut wanted = 0u32;
                let mut known = true;
                for letter in value.chars() {
                    wanted |= match letter.to_ascii_lowercase() {
                        'r' => 0x1,
                        'h' => 0x2,
                        's' => 0x4,
                        'd' => 0x10,
                        'a' => 0x20,
                        'n' => 0x80,
                        't' => 0x100,
                        'p' => 0x200,
                        'l' => 0x400,
                        'c' => 0x800,
                        'o' => 0x1000,
                        'i' => 0x2000,
                        'e' => 0x4000,
                        'v' => 0x8000,
                        _ => {
                            known = false;
                            0
                        }
                    };
                }
                if known && wanted != 0 {
                    Node::Leaf(Test::Attrib(wanted))
                } else {
                    self.unsupported.push(format!("{name}:{value}"));
                    Node::Not(Box::new(Node::All))
                }
            }
            "audio" | "zip" | "doc" | "exe" | "pic" | "video" => {
                let exts = macro_exts(name).iter().map(|e| e.to_string()).collect();
                let macro_node = Node::Leaf(Test::Ext(exts));
                if value.is_empty() { macro_node } else { Node::And(vec![macro_node, self.text(value, mods)]) }
            }
            _ => {
                self.unsupported.push(format!("{name}:"));
                Node::Not(Box::new(Node::All))
            }
        };
        match kind {
            Some(kind) => Node::And(vec![kind, node]),
            None => node,
        }
    }
}

/// A count as `len:` takes it: "12", ">60", ">=5", "<10", "<=10", "=3", "5..10".
fn counts(value: &str) -> Option<(u64, u64)> {
    let value = value.trim();
    if let Some((a, b)) = value.split_once("..") {
        return Some((a.trim().parse().ok()?, b.trim().parse().ok()?));
    }
    let (op, number) = [">=", "<=", ">", "<", "="].iter().find_map(|op| value.strip_prefix(op).map(|rest| (*op, rest)))
        .unwrap_or(("=", value));
    let n: u64 = number.trim().parse().ok()?;
    Some(match op {
        ">=" => (n, u64::MAX),
        "<=" => (0, n),
        ">" => (n.checked_add(1)?, u64::MAX),
        "<" => (0, n.checked_sub(1)?),
        _ => (n, n),
    })
}

/// Everything's modifiers (`case:`, `ww:`…): what `Parser::term` takes before a term.
pub const MODIFIERS: &[&str] = &[
    "case", "nocase", "ww", "wholeword", "wholewords", "noww", "nowholeword", "nowholewords", "path", "nopath", "file",
    "files", "folder", "folders", "diacritics", "nodiacritics", "ascii", "utf8", "noregex",
];

/// Whether a term is only modifiers (`case:`, `case:ww:`), and the modifiers it makes.
fn modifiers_only(text: &str, mut mods: Mods) -> Option<Mods> {
    let mut rest = text;
    while let Some(colon) = rest.find(':') {
        match rest[..colon].to_ascii_lowercase().as_str() {
            "case" => mods.case = true,
            "nocase" => mods.case = false,
            "ww" | "wholeword" | "wholewords" => mods.whole_word = true,
            "noww" | "nowholeword" | "nowholewords" => mods.whole_word = false,
            "path" => mods.path = true,
            "nopath" => mods.path = false,
            "file" | "files" => mods.files = true,
            "folder" | "folders" => mods.folders = true,
            "diacritics" => mods.diacritics = true,
            "nodiacritics" => mods.diacritics = false,
            "ascii" | "utf8" | "noregex" => {}
            _ => return None,
        }
        rest = &rest[colon + 1..];
    }
    rest.is_empty().then_some(mods)
}

fn kind_of(mods: Mods) -> Option<Node> {
    match (mods.files, mods.folders) {
        (true, false) => Some(Node::Leaf(Test::Files)),
        (false, true) => Some(Node::Leaf(Test::Folders)),
        _ => None,
    }
}

/// Everything's functions. One this index doesn't answer matches nothing, and the query says so (`unsupported`).
/// The app's own list (`search::query::is_function`) is checked against this one and `MODIFIERS` by a test.
pub const FUNCTIONS: &[&str] = &[
    "ext", "regex", "startwith", "endwith", "wfn", "wholefilename", "exact", "dm", "datemodified", "size", "parent",
    "infolder", "nosubfolders", "root", "audio", "zip", "doc", "exe", "pic", "video",
    "type", "dc", "da", "dr", "rc", "attrib", "attributes", "parents", "child", "len", "count", "runcount", "daterun",
    "datecreated", "dateaccessed", "dupe", "sizedupe", "empty", "content", "utf8content", "width", "height", "length",
    "dimensions", "orientation", "bitdepth", "frontmatter", "shell", "filelist", "depth", "childcount",
    "childfilecount", "childfoldercount", "sort", "ascending", "descending", "namepartdupe", "dmdupe", "dcdupe",
    "dadupe", "attribdupe",
];

fn macro_exts(name: &str) -> &'static [&'static str] {
    match name {
        "audio" => &["aac", "ac3", "aif", "aifc", "aiff", "amr", "ape", "au", "cda", "dts", "fla", "flac", "it", "m1a", "m2a",
                     "m3u", "m4a", "m4b", "m4p", "mid", "midi", "mka", "mod", "mp2", "mp3", "mpa", "ogg", "opus", "ra", "rmi",
                     "snd", "spc", "umx", "voc", "wav", "wma", "xm"],
        "zip" => &["7z", "ace", "arj", "bz2", "cab", "gz", "gzip", "jar", "r00", "r01", "r02", "r03", "r04", "r05", "r06",
                   "r07", "r08", "r09", "r10", "r11", "r12", "r13", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21",
                   "r22", "r23", "r24", "r25", "r26", "r27", "r28", "r29", "rar", "tar", "tgz", "z", "zip", "xz", "zst"],
        "doc" => &["c", "chm", "cpp", "csv", "cxx", "doc", "docm", "docx", "dot", "dotm", "dotx", "h", "hpp", "htm", "html",
                   "hxx", "ini", "java", "lua", "mht", "mhtml", "odt", "pdf", "potx", "potm", "ppam", "ppsm", "ppsx", "pps",
                   "ppt", "pptm", "pptx", "rtf", "sldm", "sldx", "thmx", "txt", "vsd", "wpd", "wps", "wri", "xlam", "xls",
                   "xlsb", "xlsm", "xlsx", "xltm", "xltx", "xml"],
        "exe" => &["bat", "cmd", "exe", "msi", "msp", "scr"],
        "pic" => &["ani", "bmp", "gif", "ico", "jpe", "jpeg", "jpg", "pcx", "png", "psd", "tga", "tif", "tiff", "webp", "wmf",
                   "heic", "heif", "avif", "jxl", "svg", "raw", "dng", "cr2", "cr3", "nef", "arw"],
        _ => &["3g2", "3gp", "3gp2", "3gpp", "amr", "amv", "asf", "avi", "bdmv", "bik", "d2v", "divx", "drc", "dsa", "dsm",
               "dss", "dsv", "evo", "f4v", "flc", "fli", "flic", "flv", "hdmov", "ifo", "ivf", "m1v", "m2p", "m2t", "m2ts",
               "m2v", "m4b", "m4p", "m4v", "mkv", "mp2v", "mp4", "mp4v", "mpe", "mpeg", "mpg", "mpls", "mpv2", "mpv4",
               "mov", "mts", "ogm", "ogv", "pss", "pva", "qt", "ram", "ratdvd", "rm", "rmm", "rmvb", "roq", "rpm", "smil",
               "smk", "swf", "tp", "tpr", "ts", "vob", "vp6", "webm", "wm", "wmp", "wmv"],
    }
}

/// Reads Everything's syntax.
pub fn parse(text: &str) -> Parsed {
    let mut parser = Parser { tokens: lex(text), at: 0, unsupported: Vec::new(), count: None, sort: None, descending: None };
    let mut items = Vec::new();
    // A stray `>` doesn't end the search: what's after it still counts.
    loop {
        let node = parser.and(Mods::default());
        if !matches!(node, Node::All) {
            items.push(node);
        }
        if parser.at >= parser.tokens.len() {
            break;
        }
        parser.at += 1;
    }
    let root = match items.len() {
        0 => Node::All,
        1 => items.pop().unwrap(),
        _ => Node::And(items),
    };
    Parsed { root, unsupported: parser.unsupported, count: parser.count, sort: parser.sort, descending: parser.descending }
}

/// Everything's character escapes in a term: `quot:` a double quote, `apos:` an apostrophe, `#<n>:` the
/// character with that code. Each becomes the character, taken as quoted (plain text, never an operator).
fn escapes(text: &str, mask: &[bool]) -> (String, Vec<bool>) {
    let chars: Vec<char> = text.chars().collect();
    if !text.contains("quot:") && !text.contains("apos:") && !text.contains('#') {
        return (text.to_string(), mask.to_vec());
    }
    let (mut out, mut out_mask) = (String::new(), Vec::new());
    let mut i = 0;
    let starts = |i: usize, word: &str| -> bool {
        let w: Vec<char> = word.chars().collect();
        chars.len() >= i + w.len() && chars[i..i + w.len()].iter().zip(&w).all(|(a, b)| a.to_ascii_lowercase() == *b)
            && !mask[i..i + w.len()].iter().any(|&q| q)
    };
    while i < chars.len() {
        if starts(i, "quot:") {
            out.push('"');
            out_mask.push(true);
            i += 5;
            continue;
        }
        if starts(i, "apos:") {
            out.push('\'');
            out_mask.push(true);
            i += 5;
            continue;
        }
        if chars[i] == '#' && !mask[i] {
            let digits: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_digit()).collect();
            let end = i + 1 + digits.chars().count();
            if !digits.is_empty() && chars.get(end) == Some(&':') {
                if let Some(c) = digits.parse::<u32>().ok().and_then(char::from_u32) {
                    out.push(c);
                    out_mask.push(true);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        out_mask.push(mask[i]);
        i += 1;
    }
    (out, out_mask)
}

const DAY: i64 = 86_400;

/// A `dm:` value as seconds since 1970, both ends included, in local time (`offset` seconds east of UTC): `today`,
/// `yesterday`, `thisweek`, `lastweek`, `thismonth`, `lastmonth`, `thisyear`, `lastyear`, `last3days` (`days`,
/// `weeks`, `months`, `years`, `hours`, `minutes`; `past` for `last`), `2024`, `2024-05`, `2024-05-31`, and those
/// after `<` `<=` `>` `>=` `=`, or two of them with `..` between.
pub fn dates(value: &str, now: i64, offset: i64) -> Option<(i64, i64)> {
    dates_with(value, now, offset, crate::when::first_weekday())
}

/// `dates`, a week starting on `first` (0 Monday to 6 Sunday).
pub fn dates_with(value: &str, now: i64, offset: i64, first: i64) -> Option<(i64, i64)> {
    let value = value.trim().to_ascii_lowercase();
    if let Some((a, b)) = value.split_once("..") {
        let from = if a.is_empty() { i64::MIN } else { date_range(a, now, offset, first)?.0 };
        let to = if b.is_empty() { i64::MAX } else { date_range(b, now, offset, first)?.1 };
        return Some((from, to));
    }
    for (op, f) in [(">=", 0), ("<=", 1), (">", 2), ("<", 3), ("=", 4)] {
        if let Some(rest) = value.strip_prefix(op) {
            let (from, to) = date_range(rest, now, offset, first)?;
            return Some(match f {
                0 => (from, i64::MAX),
                1 => (i64::MIN, to),
                2 => (to + 1, i64::MAX),
                3 => (i64::MIN, from - 1),
                _ => (from, to),
            });
        }
    }
    date_range(&value, now, offset, first)
}

fn date_range(value: &str, now: i64, offset: i64, first: i64) -> Option<(i64, i64)> {
    let local = now + offset;
    let today = local.div_euclid(DAY) * DAY;
    let (year, month, _) = civil(today / DAY);
    // The week starts on the day Windows is set to start it (Monday 0 … Sunday 6; 1970-01-01 was a Thursday, 3).
    let into_week = (today / DAY + 3 - first).rem_euclid(7);
    let week = today - into_week * DAY;
    let month_start = days_from_civil(year, month, 1) * DAY;
    let year_start = days_from_civil(year, 1, 1) * DAY;
    let local_range = |from: i64, to: i64| Some((from - offset, to - offset - 1));
    match value {
        "today" => return local_range(today, today + DAY),
        "yesterday" => return local_range(today - DAY, today),
        "tomorrow" => return local_range(today + DAY, today + 2 * DAY),
        "thisweek" => return local_range(week, week + 7 * DAY),
        "lastweek" => return local_range(week - 7 * DAY, week),
        "thismonth" => return local_range(month_start, days_from_civil(year + (month == 12) as i64, month % 12 + 1, 1) * DAY),
        "lastmonth" => {
            let (y, m) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
            return local_range(days_from_civil(y, m, 1) * DAY, month_start);
        }
        "thisyear" => return local_range(year_start, days_from_civil(year + 1, 1, 1) * DAY),
        "lastyear" => return local_range(days_from_civil(year - 1, 1, 1) * DAY, year_start),
        _ => {}
    }
    for prefix in ["last", "past"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let n: i64 = if digits.is_empty() { 1 } else { digits.parse().ok()? };
            let unit = rest[digits.len()..].trim_end_matches('s');
            let span = match unit {
                "minute" | "min" => 60,
                "hour" => 3600,
                "day" => DAY,
                "week" => 7 * DAY,
                "month" => 30 * DAY,
                "year" => 365 * DAY,
                _ => return None,
            };
            return Some((now - n * span, now));
        }
    }
    // ISO: 2024, 2024-05, 2024-05-31 (also with / or .).
    let parts: Vec<&str> = value.split(['-', '/', '.']).collect();
    let numbers: Option<Vec<i64>> = parts.iter().map(|p| p.parse().ok()).collect();
    let numbers = numbers?;
    match numbers.as_slice() {
        [y] if parts[0].len() == 4 => local_range(days_from_civil(*y, 1, 1) * DAY, days_from_civil(y + 1, 1, 1) * DAY),
        [y, m] if parts[0].len() == 4 && (1..=12).contains(m) => {
            local_range(days_from_civil(*y, *m, 1) * DAY, days_from_civil(y + (*m == 12) as i64, m % 12 + 1, 1) * DAY)
        }
        [y, m, d] if parts[0].len() == 4 && (1..=12).contains(m) && (1..=31).contains(d) => {
            let day = days_from_civil(*y, *m, *d) * DAY;
            local_range(day, day + DAY)
        }
        _ => None,
    }
}

/// Days since 1970-01-01 for a date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day since 1970-01-01.
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A `size:` value as bytes, both ends included: `>1mb`, `<=10kb`, `1mb..2gb`, `empty` `tiny` `small` `medium`
/// `large` `huge` `gigantic` (Everything's), or a number of bytes.
pub fn sizes(value: &str) -> Option<(u64, u64)> {
    let value = value.trim().to_ascii_lowercase();
    const KB: u64 = 1024;
    let named = match value.as_str() {
        "empty" => Some((0, 0)),
        "tiny" => Some((1, 10 * KB)),
        "small" => Some((10 * KB + 1, 100 * KB)),
        "medium" => Some((100 * KB + 1, KB * KB)),
        "large" => Some((KB * KB + 1, 16 * KB * KB)),
        "huge" => Some((16 * KB * KB + 1, 128 * KB * KB)),
        "gigantic" => Some((128 * KB * KB + 1, u64::MAX)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if let Some((a, b)) = value.split_once("..") {
        let from = if a.is_empty() { 0 } else { bytes(a)? };
        let to = if b.is_empty() { u64::MAX } else { bytes(b)? };
        return Some((from, to));
    }
    for (op, f) in [(">=", 0), ("<=", 1), (">", 2), ("<", 3), ("=", 4)] {
        if let Some(rest) = value.strip_prefix(op) {
            let n = bytes(rest)?;
            return Some(match f {
                0 => (n, u64::MAX),
                1 => (0, n),
                2 => (n.saturating_add(1), u64::MAX),
                3 => (0, n.saturating_sub(1)),
                _ => (n, n),
            });
        }
    }
    let n = bytes(&value)?;
    Some((n, n))
}

fn bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let number: f64 = text[..digits].parse().ok()?;
    let scale: u64 = match text[digits..].trim() {
        "" | "b" | "bytes" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return None,
    };
    Some((number * scale as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(node: &Node) -> String {
        match node {
            Node::All => "*all*".into(),
            Node::And(xs) => format!("(and {})", xs.iter().map(show).collect::<Vec<_>>().join(" ")),
            Node::Or(xs) => format!("(or {})", xs.iter().map(show).collect::<Vec<_>>().join(" ")),
            Node::Not(x) => format!("(not {})", show(x)),
            Node::Leaf(Test::Text(t)) => format!("{}{}{}{}{}", if t.exact.is_some() { "case:" } else { "" }, if t.whole_word { "ww:" } else { "" },
                                                 if t.path { "path:" } else { "" }, match t.anchor { Anchor::Start => "^", Anchor::End => "$", Anchor::Whole => "=", Anchor::Any => "" },
                                                 String::from_utf8_lossy(&t.folded)),
            Node::Leaf(Test::Glob { folded, path, .. }) => format!("glob{}:{}", if *path { "-path" } else { "" }, String::from_utf8_lossy(folded)),
            Node::Leaf(Test::Regex { regex, path }) => format!("regex{}:{}", if *path { "-path" } else { "" }, regex.as_str()),
            Node::Leaf(Test::Ext(e)) => format!("ext:{}", e.join(";")),
            Node::Leaf(Test::Files) => "file:".into(),
            Node::Leaf(Test::Folders) => "folder:".into(),
            Node::Leaf(Test::Under(p)) => format!("under:{p}"),
            Node::Leaf(Test::Parent(p)) => format!("parent:{p}"),
            Node::Leaf(Test::Root) => "root:".into(),
            Node::Leaf(Test::Modified(a, b)) => format!("dm:{a}..{b}"),
            Node::Leaf(Test::Created(a, b)) => format!("dc:{a}..{b}"),
            Node::Leaf(Test::Type(t)) => format!("type:{t}"),
            Node::Leaf(Test::KeyDupe(key)) => format!("dupe:{key:?}"),
            Node::Leaf(Test::Accessed(a, b)) => format!("da:{a}..{b}"),
            Node::Leaf(Test::Size(a, b)) => format!("size:{a}..{b}"),
            Node::Leaf(Test::Len(a, b)) => format!("len:{a}..{b}"),
            Node::Leaf(Test::Attrib(f)) => format!("attrib:{f}"),
            Node::Leaf(Test::Depth(a, b)) => format!("depth:{a}..{b}"),
            Node::Leaf(Test::Children(what, a, b)) => format!("children{what}:{a}..{b}"),
            Node::Leaf(Test::Child(inner)) => format!("child:{}", show(inner)),
            Node::Leaf(Test::Dupe) => "dupe:".into(),
            Node::Leaf(Test::SizeDupe) => "sizedupe:".into(),
        }
    }

    #[test]
    fn w8_functions() {
        assert_eq!(p("len:>60"), "len:61..18446744073709551615");
        assert_eq!(p("len:5..10"), "len:5..10");
        assert_eq!(p("attrib:h"), "attrib:2");
        assert_eq!(p("attrib:hs"), "attrib:6");
        assert_eq!(p("attrib:r"), "attrib:1");
        assert_eq!(p("attrib:ac"), "attrib:2080");
        assert_eq!(p("depth:2"), "depth:2..2");
        assert_eq!(p("empty:"), "(and folder: children0:0..0)");
        assert_eq!(p("child:x"), "(and folder: child:x)");
        assert_eq!(p("childfilecount:>3"), "(and folder: children1:4..18446744073709551615)");
        // Dates made and opened are kept.
        assert!(p("dc:today").starts_with("dc:"));
        assert!(p("da:thisweek").starts_with("da:"));
        assert!(parse("dc:today").unsupported.is_empty());
        // A letter Everything doesn't have matches nothing, and is said.
        assert_eq!(p("attrib:q"), "(not *all*)");
    }

    fn p(text: &str) -> String {
        show(&parse(text).root)
    }

    #[test]
    fn what_excavate_sends() {
        assert_eq!(p("budget"), "budget");
        assert_eq!(p("q3 report"), "(and q3 report)");
        assert_eq!(p("startwith:budget"), "^budget");
        assert_eq!(p("file: ext:pdf;doc q3"), "(and file: ext:pdf;doc q3)");
        assert_eq!(p("folder: budget"), "(and folder: budget)");
        assert_eq!(p("case:Q3 case:Report"), "(and case:q3 case:report)");
        assert_eq!(p("case:ww:path:Q3"), "case:ww:path:q3");
        assert_eq!(p(r#"case:"re:" case:x"#), "(and case:re: case:x)");
        assert_eq!(p(r#"regex:"^IMG_\d+ ext:pdf""#), r"regex:^IMG_\d+ ext:pdf");
        assert_eq!(p(r#"case:path:regex:"^a""#), "regex-path:^a");
        assert_eq!(p("\"re:\" notes"), "(and re: notes)");
        assert_eq!(p(r#"budget !\node_modules\ !\.git\"#), r"(and budget (not path:\node_modules\) (not path:\.git\))");
        assert_eq!(p(r#"<<"C:\One\">|<ext:exe;lnk <"\Program Files"|"\Start Menu\">>> budget"#),
                   r"(and (or under:C:\One (and ext:exe;lnk (or path:\program files path:\start menu\))) budget)");
        assert_eq!(p(r#"file: "C:\Docs\" !"\node_modules\" ext:txt;md"#), r"(and file: under:C:\Docs (not path:\node_modules\) ext:txt;md)");
        assert_eq!(p(r#""C:\Users\me\probe.txt""#), r"path:^c:\users\me\probe.txt");
        assert_eq!(p(r#""Q3 Plan" | x"#), "(or q3 plan x)");
        assert_eq!(p("a b|c"), "(and a (or b c))");
        assert_eq!(p("!e"), "(not e)");
        assert_eq!(p("*.dll"), "glob:*.dll");
        assert_eq!(p("re: meeting"), "(and re: meeting)");
        assert_eq!(p("10:30 call"), "(and 10:30 call)");
        assert_eq!(p("ww:*.rs"), "glob:*.rs");
        assert_eq!(p("size:>1mb"), format!("size:{}..{}", (1u64 << 20) + 1, u64::MAX));
        assert_eq!(p(""), "*all*");
        assert!(p("dc:today x").starts_with("(and dc:"));
        assert!(matches!(parse("audio:").root, Node::Leaf(Test::Ext(_))));
    }

    #[test]
    fn dates_and_sizes() {
        // 2026-11-04 12:00 UTC, two hours east.
        let now = days_from_civil(2026, 11, 4) * DAY + 12 * 3600;
        let offset = 7200;
        let today = dates("today", now, offset).unwrap();
        assert_eq!(today, (days_from_civil(2026, 11, 4) * DAY - offset, days_from_civil(2026, 11, 5) * DAY - offset - 1));
        assert_eq!(dates("2026", now, offset).unwrap().0, days_from_civil(2026, 1, 1) * DAY - offset);
        assert_eq!(dates(">=2026-11", now, 0).unwrap(), (days_from_civil(2026, 11, 1) * DAY, i64::MAX));
        assert_eq!(dates("<2000", now, 0).unwrap(), (i64::MIN, days_from_civil(2000, 1, 1) * DAY - 1));
        assert_eq!(dates("2024..2025", now, 0).unwrap(), (days_from_civil(2024, 1, 1) * DAY, days_from_civil(2026, 1, 1) * DAY - 1));
        // Wednesday 2026-11-04: the week started Monday the 2nd.
        assert_eq!(dates_with("thisweek", now, 0, 0).unwrap().0, days_from_civil(2026, 11, 2) * DAY, "from Monday");
        assert_eq!(dates_with("thisweek", now, 0, 6).unwrap().0, days_from_civil(2026, 11, 1) * DAY, "from Sunday");
        assert_eq!(dates_with("thisweek", now, 0, 5).unwrap().0, days_from_civil(2026, 10, 31) * DAY, "from Saturday");
        assert_eq!(dates_with("lastweek", now, 0, 5).unwrap(), (days_from_civil(2026, 10, 24) * DAY, days_from_civil(2026, 10, 31) * DAY - 1));
        assert!((0..=6).contains(&crate::when::first_weekday()));
        assert_eq!(dates("lastmonth", now, 0).unwrap(), (days_from_civil(2026, 10, 1) * DAY, days_from_civil(2026, 11, 1) * DAY - 1));
        assert_eq!(dates("last2weeks", now, 0).unwrap(), (now - 14 * DAY, now));
        assert!(dates("someday", now, 0).is_none());
        assert_eq!(civil(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(sizes("1kb..2kb"), Some((1024, 2048)));
        assert_eq!(sizes("<10"), Some((0, 9)));
        assert_eq!(sizes("empty"), Some((0, 0)));
        assert_eq!(sizes("1.5mb"), Some((1_572_864, 1_572_864)));
        assert_eq!(sizes("lots"), None);
    }
}
