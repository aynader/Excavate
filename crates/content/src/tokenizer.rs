//! How text becomes searchable words, the same way for what's indexed and for what's typed:
//!
//! - words are runs of letters and digits, lowercased, accents folded ("Café" → "cafe");
//! - joined words are also split where they join, so `getUserName`, `get_user_name` and "user name" find each other,
//!   and so do `Report2024` and "report 2024";
//! - Chinese, Japanese and Korean, written without spaces, are indexed a character at a time and searched as phrases;
//! - very long runs (hashes, base64) aren't words anyone types, and are skipped.

use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, TextAnalyzer, Token, TokenStream, Tokenizer};

/// The name the analyzer is registered under, in the index and in every reader.
pub const NAME: &str = "excavate";

/// Words longer than this aren't indexed.
const LONGEST: usize = 48;

pub fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(Words).filter(LowerCaser).filter(AsciiFoldingFilter).build()
}

/// The words of `text` as the index sees them.
pub fn words(text: &str) -> Vec<String> {
    let mut analyzer = analyzer();
    let mut stream = analyzer.token_stream(text);
    let mut words = Vec::new();
    while let Some(token) = stream.next() {
        words.push(token.text.clone());
    }
    words
}

/// The words of `text` with where each is (byte ranges in `text`), joined words and their parts both, for finding
/// and highlighting matches.
pub fn spans(text: &str) -> Vec<(String, std::ops::Range<usize>)> {
    let mut analyzer = analyzer();
    let mut stream = analyzer.token_stream(text);
    let mut spans: Vec<(String, std::ops::Range<usize>)> = Vec::new();
    while let Some(token) = stream.next() {
        spans.push((token.text.clone(), token.offset_from..token.offset_to));
    }
    spans
}

/// A typed word as the index sees it: its whole words (several when it holds punctuation, "e-mail", or is written
/// without spaces, "東京都"), each with the parts it joins ("getUserName" → get, user, name).
pub fn analyze(word: &str) -> Vec<(String, Vec<String>)> {
    wholes(word).into_iter().map(|(whole, parts, _)| (whole, parts)).collect()
}

/// A typed word's whole words as the index sees them and as they were typed, "E-Mail" → (e, E), (mail, Mail).
pub fn typed_wholes(word: &str) -> Vec<(String, String)> {
    wholes(word).into_iter().map(|(whole, _, range)| (whole, word[range].to_string())).collect()
}

fn wholes(word: &str) -> Vec<(String, Vec<String>, std::ops::Range<usize>)> {
    let mut analyzer = analyzer();
    let mut stream = analyzer.token_stream(word);
    let mut wholes: Vec<(String, Vec<String>, std::ops::Range<usize>)> = Vec::new();
    while let Some(token) = stream.next() {
        let range = token.offset_from..token.offset_to;
        match wholes.last_mut() {
            Some((_, parts, whole)) if range.start >= whole.start && range.end <= whole.end => parts.push(token.text.clone()),
            _ => wholes.push((token.text.clone(), Vec::new(), range)),
        }
    }
    wholes
}

#[derive(Clone, Default)]
pub struct Words;

pub struct WordStream {
    tokens: Vec<Token>,
    next: usize,
}

impl Tokenizer for Words {
    type TokenStream<'a> = WordStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> WordStream {
        WordStream { tokens: tokenize(text), next: 0 }
    }
}

impl TokenStream for WordStream {
    fn advance(&mut self) -> bool {
        self.next += 1;
        self.next <= self.tokens.len()
    }

    fn token(&self) -> &Token {
        &self.tokens[self.next - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.next - 1]
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // Hiragana, Katakana
        | 0x3400..=0x4DBF   // CJK extension A
        | 0x4E00..=0x9FFF   // CJK unified ideographs
        | 0xAC00..=0xD7AF   // Hangul syllables
        | 0xF900..=0xFAFF   // compatibility ideographs
        | 0x20000..=0x2FA1F // extensions B–F
    )
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut position = 0usize;
    let mut start: Option<usize> = None;
    let push = |tokens: &mut Vec<Token>, position: &mut usize, from: usize, to: usize| {
        let word = &text[from..to];
        if word.chars().count() > LONGEST {
            return;
        }
        let parts = parts(word);
        tokens.push(Token { offset_from: from, offset_to: to, position: *position, text: word.to_string(), position_length: 1 });
        if parts.len() > 1 {
            for (index, part) in parts.iter().enumerate() {
                tokens.push(Token {
                    offset_from: from + part.start,
                    offset_to: from + part.end,
                    position: *position + index,
                    text: word[part.clone()].to_string(),
                    position_length: 1,
                });
            }
            *position += parts.len();
        } else {
            *position += 1;
        }
    };
    for (index, c) in text.char_indices() {
        if is_cjk(c) {
            if let Some(from) = start.take() {
                push(&mut tokens, &mut position, from, index);
            }
            let end = index + c.len_utf8();
            tokens.push(Token { offset_from: index, offset_to: end, position, text: c.to_string(), position_length: 1 });
            position += 1;
        } else if c.is_alphanumeric() {
            if start.is_none() {
                start = Some(index);
            }
        } else if let Some(from) = start.take() {
            push(&mut tokens, &mut position, from, index);
        }
    }
    if let Some(from) = start {
        push(&mut tokens, &mut position, from, text.len());
    }
    tokens
}

/// Where a word joins: lower→Upper ("getUser"), UPPER→Upper+lower ("HTTPServer" → HTTP, Server), letter↔digit.
fn parts(word: &str) -> Vec<std::ops::Range<usize>> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut from = 0usize;
    for window in 1..chars.len() {
        let (index, c) = chars[window];
        let previous = chars[window - 1].1;
        let next = chars.get(window + 1).map(|&(_, c)| c);
        let split = (previous.is_lowercase() && c.is_uppercase())
            || (previous.is_uppercase() && c.is_uppercase() && next.is_some_and(char::is_lowercase))
            || (previous.is_alphabetic() && c.is_numeric())
            || (previous.is_numeric() && c.is_alphabetic());
        if split {
            parts.push(from..index);
            from = index;
        }
    }
    parts.push(from..word.len());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_fold_and_lowercase() {
        assert_eq!(words("Café au LAIT, s'il vous plaît"), ["cafe", "au", "lait", "s", "il", "vous", "plait"]);
    }

    #[test]
    fn joined_words_split_too() {
        assert_eq!(words("getUserName"), ["getusername", "get", "user", "name"]);
        assert_eq!(words("get_user_name"), ["get", "user", "name"]);
        assert_eq!(words("HTTPServer"), ["httpserver", "http", "server"]);
        assert_eq!(words("Report2024"), ["report2024", "report", "2024"]);
    }

    #[test]
    fn cjk_by_character() {
        assert_eq!(words("東京都の地図"), ["東", "京", "都", "の", "地", "図"]);
        assert_eq!(words("会議notes"), ["会", "議", "notes"]);
    }

    #[test]
    fn typed_words() {
        assert_eq!(analyze("getUserName"), [("getusername".to_string(), vec!["get".to_string(), "user".into(), "name".into()])]);
        assert_eq!(analyze("e-mail"), [("e".to_string(), vec![]), ("mail".to_string(), vec![])]);
        assert_eq!(analyze("Budget"), [("budget".to_string(), vec![])]);
        assert_eq!(typed_wholes("E-Mail"), [("e".to_string(), "E".to_string()), ("mail".to_string(), "Mail".to_string())]);
    }

    #[test]
    fn long_runs_skipped() {
        let hash = "a".repeat(64);
        assert_eq!(words(&format!("sha {hash} done")), ["sha", "done"]);
    }

    #[test]
    fn positions_follow_parts() {
        let tokens = tokenize("my getUser call");
        let positions: Vec<(String, usize)> = tokens.iter().map(|t| (t.text.clone(), t.position)).collect();
        assert_eq!(positions, [("my".into(), 0), ("getUser".into(), 1), ("get".into(), 1), ("User".into(), 2), ("call".into(), 3)]);
    }
}
