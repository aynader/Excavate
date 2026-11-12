//! Finding secrets in text (what was read from a picture, or a file's own text), so previews can blur them and the
//! index never keeps them:
//!
//! - **validated numbers**: card numbers (Luhn), IBANs (mod 97), US social security numbers;
//! - **credentials**: API keys and tokens by their published prefixes, private-key blocks, JWTs, and the values
//!   next to labels such as Password, PIN, CVV, Recovery code (in several languages);
//! - **identity documents**: the machine-readable lines of passports, visas and ID cards (read leniently: OCR puts
//!   spaces in them and reads `<` as `«` or `K`), the document's number wherever else it's printed, and in a short text
//!   that is an identity document (a scan of one), every number shaped like a document's;
//! - **seed phrases**: 12 or more words of the BIP-39 list in a row.
//!
//! Each secret is replaced in the index by a label ("‹password›"), so "the screenshot with my Wi-Fi password" is
//! still found, but the password isn't in Excavate's index for anything to read.

use regex_lite::Regex;
use std::ops::Range;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Card,
    Iban,
    NationalId,
    Password,
    Key,
    PrivateKey,
    Document,
    SeedPhrase,
}

impl Kind {
    /// What the index keeps in the secret's place (searchable words).
    pub fn label(self) -> &'static str {
        match self {
            Kind::Card => "‹card number›",
            Kind::Iban => "‹IBAN›",
            Kind::NationalId => "‹ID number›",
            Kind::Password => "‹password›",
            Kind::Key => "‹API key›",
            Kind::PrivateKey => "‹private key›",
            Kind::Document => "‹passport›",
            Kind::SeedPhrase => "‹seed phrase›",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Card => "card",
            Kind::Iban => "iban",
            Kind::NationalId => "id",
            Kind::Password => "password",
            Kind::Key => "key",
            Kind::PrivateKey => "private-key",
            Kind::Document => "document",
            Kind::SeedPhrase => "seed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret {
    pub kind: Kind,
    /// Byte range of the secret itself in the text (not its label).
    pub range: Range<usize>,
}

struct Patterns {
    card: Regex,
    iban: Regex,
    ssn: Regex,
    keys: Vec<Regex>,
    private_key: Regex,
    labeled: Regex,
    labeled_other_scripts: Regex,
    /// A line that is a label alone ("Password", "Wi-Fi password:"): its value shows beside or under it.
    label_only: Regex,
    /// Words that say a text is an identity document, or a visa in a passport.
    document_words: Regex,
    /// A number shaped like a document's: 8 to 10 capitals and digits (checked further in `document_number`).
    document_number: Regex,
    /// A label somewhere in a line ("Password of the router:"), for a line that ends in a colon.
    label_in: Regex,
    /// What an identity number follows: "National ID:", "ID No.", "Personal number" (its value has 6 digits or more).
    id_label: Regex,
}

/// What a secret's value follows: "Password: …", "PIN = …" (several languages).
const LABELS: &str = r"password|passwort|passwd|pwd|passcode|pass code|mot de passe|contrase(?:ñ|n)a|senha|wachtwoord|l(?:ö|o)senord|parola|has(?:ł|l)o|pin(?: code)?|cvv2?|cvc2?|security code|recovery code|recovery key|backup codes?|secret(?:[ _]access)?(?:[ _]key)?|client[ _]secret|private[ _]key|api[ _]key|access[ _]key|(?:auth|access|refresh|bearer)[ _]token|token|wi-?fi (?:key|password|passwort|kennwort|schl(?:ü|ue)ssel)|network key|network password|wpa2? key|wpa2?-psk|wpa2? passphrase|passphrase|psk|wlan[ -]?(?:key|password|passwort|kennwort|schl(?:ü|ue)ssel)|wireless (?:key|password)|security key|netzwerkschl(?:ü|ue)ssel|kennwort|cl(?:é|e) (?:wi-?fi|wpa|de s(?:é|e)curit(?:é|e) r(?:é|e)seau)";
/// The same in other scripts.
const LABELS_OTHER_SCRIPTS: &str = r"[Пп]ароль|ПАРОЛЬ|[Пп]ин-код|PIN-код|[Кк]од безопасности|كلمة المرور|كلمة السر|الرقم السري|رمز عبور|گذرواژه|סיסמה|סיסמא|पासवर्ड|कूटशब्द|[Κκ]ωδικός|ΚΩΔΙΚΟΣ|[şŞ]ifre|ŞİFRE|密码|密碼|口令|パスワード|暗証番号|비밀번호|암호|รหัสผ่าน|[Mm]ật khẩu";

/// What an identity number follows (the value is checked to have six digits or more).
const ID_LABELS: &str = r"national id(?: number| no\.?)?|national number|nid(?:[ -]no\.?)?|id[ -]?(?:no\.?|number|nr\.?)|personal (?:no\.?|number|id)|personalnummer|identity number|social security number|ssn|nin|nic(?: no\.?)?|الرقم القومي|رقم الهوية";

/// "Passport", "Visum", "Personalausweis"…: the text is an identity document (or a visa in one). Not "visa" alone: card
/// statements say it too.
const DOCUMENT_WORDS: &str = r"passport|passeport|pasaporte|passaporte|passaporto|paspoort|paszport|reisepass|pass-?nr|visum|visto|wiza|vize|identity card|id card|personalausweis|carte d'identit[eé]|carte nationale d'identit[eé]|documento de identidad|residence permit|aufenthaltstitel|aufenthaltserlaubnis|titre de s[eé]jour|permesso di soggiorno|duration of stay|dauer des aufenthalts|number of entries|anzahl der einreisen|nombre d'entr[eé]es";
/// Longer than this, a text that names an identity document is about one (a form, a guide), not a scan of one: only its
/// machine-readable lines and the number in them count then.
const DOCUMENT_TEXT_MOST: usize = 4000;

fn patterns() -> &'static Patterns {
    static PATTERNS: OnceLock<Patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Patterns {
        card: Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").unwrap(),
        iban: Regex::new(r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]){11,30}\b").unwrap(),
        ssn: Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(),
        keys: [
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            r"\bgh[pousr]_[A-Za-z0-9]{30,}\b",
            r"\bgithub_pat_[A-Za-z0-9_]{50,}\b",
            r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b",
            r"\bsk-[A-Za-z0-9_-]{20,}",
            r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}\b",
            r"\bAIza[0-9A-Za-z_-]{35}\b",
            r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            r"\bglpat-[A-Za-z0-9_-]{20,}\b",
            r"\bnpm_[A-Za-z0-9]{36}\b",
            r"\bhf_[A-Za-z0-9]{30,}\b",
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).unwrap())
        .collect(),
        private_key: Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----").unwrap(),
        // `_` before the label too: DB_PASSWORD=…, AWS_SECRET_ACCESS_KEY=….
        // (A note in brackets may come between: "Network Key (WPA2-PSK): …".)
        labeled: Regex::new(&format!(r"(?i)(?:\b|_)(?:{LABELS})\b(?:[ \t]*\([^)\r\n]{{0,24}}\))?[ \t]*[:=][ \t]*([^\s][^\r\n\t]{{0,63}})")).unwrap(),
        // What Deep reads and the first pass can't. This regex engine's `\b` and `(?i)` are ASCII only: no boundaries
        // here, and the usual ways of writing each. Colons include the full-width one.
        labeled_other_scripts: Regex::new(&format!(r"(?:{LABELS_OTHER_SCRIPTS})[ \t]*[:=：][ \t]*([^\s][^\r\n\t]{{0,63}})")).unwrap(),
        label_only: Regex::new(&format!(r"(?i)^[^A-Za-z0-9]*(?:(?:{LABELS})|(?:{LABELS_OTHER_SCRIPTS}))[ \t]*[:=：]?[^A-Za-z0-9]*$")).unwrap(),
        document_words: Regex::new(&format!(r"(?i)\b(?:{DOCUMENT_WORDS})\b")).unwrap(),
        document_number: Regex::new(r"\b[A-Z0-9]{8,10}\b").unwrap(),
        label_in: Regex::new(&format!(r"(?i)(?:\b|_)(?:{LABELS})\b|(?:{LABELS_OTHER_SCRIPTS})")).unwrap(),
        id_label: Regex::new(&format!(r"(?i)(?:^|[^A-Za-z])(?:{ID_LABELS})(?:[^A-Za-z]|$)")).unwrap(),
    })
}

fn seed_words() -> &'static std::collections::HashSet<&'static str> {
    static WORDS: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| include_str!("bip39-english.txt").lines().map(str::trim).filter(|word| !word.is_empty()).collect())
}

/// The secrets in `text`, in order, not overlapping.
pub fn find(text: &str) -> Vec<Secret> {
    let patterns = patterns();
    let mut found = Vec::new();
    for hit in patterns.private_key.find_iter(text) {
        found.push(Secret { kind: Kind::PrivateKey, range: hit.range() });
    }
    for key in &patterns.keys {
        for hit in key.find_iter(text) {
            found.push(Secret { kind: Kind::Key, range: hit.range() });
        }
    }
    for captures in patterns.labeled.captures_iter(text).chain(patterns.labeled_other_scripts.captures_iter(text)) {
        if let Some(value) = captures.get(1) {
            // The value ends at a wide gap (the next column of a screenshot) or the end of the line.
            let value_text = value.as_str();
            let end = value_text.find("  ").unwrap_or(value_text.len());
            let trimmed = value_text[..end].trim_end();
            if !trimmed.is_empty() && !is_placeholder(trimmed) {
                found.push(Secret { kind: Kind::Password, range: value.start()..value.start() + trimmed.len() });
            }
        }
    }
    for hit in patterns.card.find_iter(text) {
        let digits: Vec<u32> = hit.as_str().chars().filter_map(|c| c.to_digit(10)).collect();
        if (13..=19).contains(&digits.len()) && (2..=6).contains(&digits[0]) && luhn(&digits) {
            found.push(Secret { kind: Kind::Card, range: hit.range() });
        }
    }
    for hit in patterns.iban.find_iter(text) {
        if iban_valid(hit.as_str()) {
            found.push(Secret { kind: Kind::Iban, range: hit.range() });
        }
    }
    for hit in patterns.ssn.find_iter(text) {
        let area: u32 = hit.as_str()[..3].parse().unwrap_or(0);
        if area != 0 && area != 666 && area < 900 && &hit.as_str()[4..6] != "00" && &hit.as_str()[7..] != "0000" {
            found.push(Secret { kind: Kind::NationalId, range: hit.range() });
        }
    }
    found.extend(values_under_labels(text));
    found.extend(documents(text));
    found.extend(seed_phrases(text));
    found.sort_by_key(|secret| (secret.range.start, std::cmp::Reverse(secret.range.end)));
    // Overlaps (a key that is also a labeled value): the first, longest one wins.
    let mut kept: Vec<Secret> = Vec::new();
    for secret in found {
        match kept.last_mut() {
            Some(last) if secret.range.start < last.range.end => last.range.end = last.range.end.max(secret.range.end),
            _ => kept.push(secret),
        }
    }
    kept
}

/// Whether a line is only a secret's label ("Password", "Wi-Fi password:", "PIN"), with its value elsewhere: beside it
/// or under it, as a screen or a slide shows it. (A label with its value on the same line is `find`'s.)
pub fn label_only(line: &str) -> bool {
    line.chars().count() <= 40 && patterns().label_only.is_match(line.trim())
}

/// Whether a text is about a Wi-Fi network (a router's sticker, a "share Wi-Fi" screen, a note with its name and
/// key): a QR code in an image with such text is taken for the network's password.
pub fn about_wifi(text: &str) -> bool {
    static WIFI: OnceLock<Regex> = OnceLock::new();
    // (This regex engine's `\b` is ASCII only: the Arabic needs none.)
    WIFI.get_or_init(|| Regex::new(r"(?i)\b(?:wi-?fi|wlan|ssid|wpa2?|wpa3|wireless|network key|network name|netzwerkschl(?:ü|ue)ssel|netzwerkname|r(?:é|e)seau wi-?fi|scan to (?:connect|join)|join (?:the )?network)\b|واي ?فاي|شبكة").unwrap())
        .is_match(text)
}

/// Values on the line after their label, as a screen, a form or a sticker read line by line shows them: a label alone
/// ("Password", "Wi-Fi key:"), or a line ending in one and a colon ("Password of the router:"), then the value; an
/// identity number's label ("National ID:", "ID No.") then a number. Only a value that looks like one: a single word with a
/// digit, a symbol or a capital inside it ("Hx7kq2931", "tulip-river-77", "purplePlum"), not a button's or a field's
/// words ("Sign in", "Forgot password?"), an email address or a link.
fn values_under_labels(text: &str) -> Vec<Secret> {
    let patterns = patterns();
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        lines.push((at, line.trim_end_matches(['\r', '\n'])));
        at += line.len();
    }
    let mut found = Vec::new();
    for pair in lines.windows(2) {
        let (label, (start, next)) = (pair[0].1.trim(), pair[1]);
        let value = next.trim();
        if value.is_empty() {
            continue;
        }
        let offset = start + (next.len() - next.trim_start().len());
        let range = offset..offset + value.len();
        let colon = label.ends_with(':') || label.ends_with('：');
        let password_label = label_only(label) || (colon && label.chars().count() <= 60 && patterns.label_in.is_match(label));
        if password_label && password_like(value) {
            found.push(Secret { kind: Kind::Password, range });
        } else if label.chars().count() <= 40 && patterns.id_label.is_match(label) && !patterns.id_label.is_match(value) {
            let digits = value.chars().filter(char::is_ascii_digit).count();
            let shaped = value.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-');
            if digits >= 6 && shaped && value.chars().count() <= 24 {
                found.push(Secret { kind: Kind::NationalId, range });
            }
        }
    }
    found
}

/// One word that reads as a password, not as a word of the screen around it.
fn password_like(value: &str) -> bool {
    let count = value.chars().count();
    if !(4..=64).contains(&count) || value.contains(char::is_whitespace) || is_placeholder(value) {
        return false;
    }
    if value.ends_with(['?', ':', '：', '!']) && !value.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let lower = value.to_lowercase();
    // A button's or a field's word written with a hyphen ("Sign-in", "E-mail").
    let bare: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    const SCREEN_WORDS: &[&str] = &["signin", "signup", "login", "logon", "logout", "signout", "email", "wifi", "show", "hide",
        "submit", "continue", "next", "ok", "cancel", "required", "optional", "forgot", "reset", "change", "password",
        "username", "enter", "register", "save", "edit", "copy", "share", "connect"];
    if SCREEN_WORDS.contains(&bare.as_str()) {
        return false;
    }
    if lower.contains("://") || lower.starts_with("www.") || (lower.contains('@') && lower.contains('.')) {
        return false;
    }
    let digit = value.chars().any(|c| c.is_ascii_digit());
    let symbol = value.trim_end_matches(['.', ',']).chars().any(|c| !c.is_alphanumeric());
    let inner_capital = value.chars().skip(1).any(char::is_uppercase) && value.chars().any(char::is_lowercase);
    digit || symbol || inner_capital
}

/// "Password: ********", "PIN: ____": nothing to hide.
fn is_placeholder(value: &str) -> bool {
    // A secret's label, where the index put it in its place ("Password: ‹password›").
    if value.starts_with('‹') && value.contains('›') {
        return true;
    }
    value.chars().all(|c| matches!(c, '*' | '•' | '·' | '_' | '.' | 'x' | 'X' | '-' | ' '))
}

/// An identity document's secrets: its machine-readable lines, its number wherever else it's printed, and, in a short text
/// that is one, every number shaped like a document's (a visa's own number, the passport's it's in, a personal number).
fn documents(text: &str) -> Vec<Secret> {
    let mut found = Vec::new();
    let mut numbers: Vec<String> = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Some(mrz) = machine_readable(trimmed) else { continue };
        found.push(Secret { kind: Kind::Document, range: start..start + trimmed.len() });
        // The document's number: a passport's or a visa's second line starts with it, an ID card's first line has it
        // after the code and the country (ICAO 9303).
        let number = if mrz.len() >= 34 && !starts_a_document(&mrz) {
            &mrz[..9]
        } else if (28..=32).contains(&mrz.len()) && starts_a_document(&mrz) {
            &mrz[5..14]
        } else {
            ""
        };
        let number = number.trim_end_matches('<');
        if number.len() >= 6 && number.bytes().any(|b| b.is_ascii_digit()) && !number.contains('<') {
            numbers.push(number.to_string());
        }
    }
    // The number printed elsewhere (as OCR reads it: a space may come between its characters).
    for number in &numbers {
        let pattern: Vec<String> = number.chars().map(|c| regex_lite::escape(&c.to_string())).collect();
        if let Ok(printed) = Regex::new(&format!(r"\b{}\b", pattern.join(" ?"))) {
            found.extend(printed.find_iter(text).map(|hit| Secret { kind: Kind::Document, range: hit.range() }));
        }
    }
    let patterns = patterns();
    let scan = !found.is_empty() || (text.len() <= DOCUMENT_TEXT_MOST && patterns.document_words.is_match(text));
    if scan {
        for hit in patterns.document_number.find_iter(text) {
            if document_number(hit.as_str()) {
                found.push(Secret { kind: Kind::Document, range: hit.range() });
            }
        }
    }
    found
}

/// A machine-readable line, as read: without spaces, `«` as two fillers and `‹` as one, and a run of three `K`s or more
/// as fillers (OCR's usual misreadings). Some when it is one: 28 to 46 capitals, digits and fillers, with at least three
/// fillers, and either the start of a document ("P<UTO…", "VCD<<…", "IDD<<…") or six digits or more.
fn machine_readable(line: &str) -> Option<String> {
    let mut read = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            c if c.is_whitespace() => {}
            '«' => read.push_str("<<"),
            '‹' | '＜' | 'く' => read.push('<'),
            c => read.push(c),
        }
    }
    if !(28..=46).contains(&read.len()) || !read.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'<') {
        return None;
    }
    let read = fillers_for_ks(&read);
    if read.matches('<').count() < 3 || !read.contains("<<") {
        return None;
    }
    let digits = read.bytes().filter(u8::is_ascii_digit).count();
    (starts_a_document(&read) || digits >= 6).then_some(read)
}

/// Runs of three `K`s or more are fillers misread.
fn fillers_for_ks(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        let run = bytes[i..].iter().take_while(|&&b| b == b'K').count();
        if run >= 3 {
            out.extend(std::iter::repeat_n('<', run));
            i += run;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// The first line of a document: its code (P, V, I, A or C, then a capital or a filler) and the issuing country (three
/// capitals or fillers).
fn starts_a_document(line: &str) -> bool {
    let b = line.as_bytes();
    b.len() >= 5 && b"PVIAC".contains(&b[0]) && (b[1].is_ascii_uppercase() || b[1] == b'<')
        && b[2..5].iter().all(|&c| c.is_ascii_uppercase() || c == b'<')
}

/// Shaped like a document's number: at least five digits (a word of capitals isn't one), and not a date written as
/// digits alone (20251231, 31122025).
fn document_number(token: &str) -> bool {
    let digits = token.bytes().filter(u8::is_ascii_digit).count();
    if digits < 5 {
        return false;
    }
    if token.len() == 8 && digits == 8 {
        let n = |range: std::ops::Range<usize>| token[range].parse::<u32>().unwrap_or(0);
        let date = |year: u32, month: u32, day: u32| (1900..=2100).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day);
        if date(n(0..4), n(4..6), n(6..8)) || date(n(4..8), n(2..4), n(0..2)) {
            return false;
        }
    }
    true
}

fn luhn(digits: &[u32]) -> bool {
    let sum: u32 = digits.iter().rev().enumerate().map(|(index, &digit)| {
        if index % 2 == 1 {
            let doubled = digit * 2;
            if doubled > 9 { doubled - 9 } else { doubled }
        } else {
            digit
        }
    }).sum();
    sum % 10 == 0
}

fn iban_valid(text: &str) -> bool {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let (head, tail) = compact.split_at(4);
    let mut remainder = 0u64;
    for c in tail.chars().chain(head.chars()) {
        let value = match c {
            '0'..='9' => c as u64 - '0' as u64,
            'A'..='Z' => c as u64 - 'A' as u64 + 10,
            _ => return false,
        };
        remainder = if value >= 10 { (remainder * 100 + value) % 97 } else { (remainder * 10 + value) % 97 };
    }
    remainder == 1
}

/// 12 or more BIP-39 words in a row (a crypto wallet's recovery phrase).
fn seed_phrases(text: &str) -> Vec<Secret> {
    let words = seed_words();
    let mut found = Vec::new();
    let mut run: Vec<Range<usize>> = Vec::new();
    let mut flush = |run: &mut Vec<Range<usize>>| {
        if run.len() >= 12 {
            found.push(Secret { kind: Kind::SeedPhrase, range: run[0].start..run[run.len() - 1].end });
        }
        run.clear();
    };
    let mut index = 0;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        if bytes[index].is_ascii_alphabetic() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            let word = &text[start..index];
            if word.len() >= 3 && word.bytes().all(|b| b.is_ascii_lowercase()) && words.contains(word) {
                run.push(start..index);
            } else {
                flush(&mut run);
            }
        } else if bytes[index].is_ascii_digit() || bytes[index] == b'.' || bytes[index] == b')' || bytes[index].is_ascii_whitespace() || bytes[index] == b',' {
            // Numbered lists ("1. abandon 2. ability") and line breaks keep a phrase together.
            index += 1;
        } else {
            flush(&mut run);
            index += 1;
        }
    }
    flush(&mut run);
    found
}

/// The text with every secret replaced by its label: what the index keeps.
pub fn redact(text: &str, secrets: &[Secret]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for secret in secrets {
        if secret.range.start < at || secret.range.end > text.len() {
            continue;
        }
        out.push_str(&text[at..secret.range.start]);
        out.push_str(secret.kind.label());
        at = secret.range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// The text with every secret's characters covered (for a text preview before Reveal); same length in characters.
pub fn mask(text: &str, secrets: &[Secret]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for secret in secrets {
        if secret.range.start < at || secret.range.end > text.len() {
            continue;
        }
        out.push_str(&text[at..secret.range.start]);
        for c in text[secret.range.clone()].chars() {
            out.push(if c.is_whitespace() { c } else { '•' });
        }
        at = secret.range.end;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_alone() {
        for line in ["Password", "Wi-Fi password:", "PIN", "  Recovery code :", "Пароль", "パスワード", "API key"] {
            assert!(label_only(line), "{line}");
        }
        for line in ["Password: hunter22", "Forgot your password?", "Change password settings", "Wi-Fi", "Network"] {
            assert!(!label_only(line), "{line}");
        }
    }

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        find(text).into_iter().map(|secret| (secret.kind, &text[secret.range])).collect()
    }

    #[test]
    fn cards_need_luhn() {
        assert_eq!(kinds("Visa 4111 1111 1111 1111 exp 12/29"), [(Kind::Card, "4111 1111 1111 1111")]);
        assert!(kinds("order 4111 1111 1111 1112").is_empty());
        assert!(kinds("phone 0612345678").is_empty());
    }

    #[test]
    fn ibans_need_mod_97() {
        assert_eq!(kinds("IBAN: GB82 WEST 1234 5698 7654 32 thanks")[0], (Kind::Iban, "GB82 WEST 1234 5698 7654 32"));
        assert!(kinds("GB82 WEST 1234 5698 7654 33").iter().all(|(kind, _)| *kind != Kind::Iban));
    }

    #[test]
    fn wifi_labels() {
        // Router stickers and phone screens.
        assert_eq!(kinds("WLAN key: 8f3kd92mxq"), [(Kind::Password, "8f3kd92mxq")]);
        assert_eq!(kinds("WLAN-Schlüssel: 4492 1837 2290"), [(Kind::Password, "4492 1837 2290")]);
        assert_eq!(kinds("Network Key (WPA2-PSK): Xk29dLm4"), [(Kind::Password, "Xk29dLm4")]);
        assert_eq!(kinds("Passphrase: tulip-river-77"), [(Kind::Password, "tulip-river-77")]);
        assert_eq!(kinds("Kennwort: Haus2024!"), [(Kind::Password, "Haus2024!")]);
        assert_eq!(kinds("Wireless password: purpleplum9"), [(Kind::Password, "purpleplum9")]);
        assert!(label_only("WLAN key"));
        assert!(kinds("Key features: fast and light").is_empty());
    }

    #[test]
    fn wifi_context() {
        assert!(about_wifi("Scan to connect
Guest WiFi"));
        assert!(about_wifi("SSID: home-5G  WPA2"));
        assert!(about_wifi("WLAN-Schlüssel"));
        assert!(!about_wifi("Scan this code to see our menu"));
        assert!(!about_wifi("Boarding pass  Gate 12"));
    }

    #[test]
    fn values_on_the_next_line() {
        // As a screenshot or a sticker reads, line by line.
        assert_eq!(kinds("Password of the router:\nHx7kq2931\nEmail address :"), [(Kind::Password, "Hx7kq2931")]);
        assert_eq!(kinds("Wi-Fi key\ntulip-river-77"), [(Kind::Password, "tulip-river-77")]);
        assert_eq!(kinds("Password\n  purplePlum "), [(Kind::Password, "purplePlum")]);
        assert_eq!(kinds("National ID:\n29801011234567"), [(Kind::NationalId, "29801011234567")]);
        // A sign-in screen: the field's label, then its button or its link, has nothing to hide.
        for screen in ["Email\nPassword\nSign in\nForgot password?", "Password\nForgot password?", "Password\nShow",
                       "Password\n••••••••", "Change Password\nSecurity", "Password:\nname@example.com",
                       "Password\nEnter your password here", "Password recovery\nDo you have a login and password?",
                       "ID number\nRequired", "Password\nLOGIN", "Password\nSign-in"] {
            assert!(kinds(screen).is_empty(), "{screen:?}: {:?}", kinds(screen));
        }
    }

    #[test]
    fn labeled_values() {
        assert_eq!(kinds("Wi-Fi password: hunter2   Network: Home"), [(Kind::Password, "hunter2")]);
        assert_eq!(kinds("PIN = 4821"), [(Kind::Password, "4821")]);
        assert_eq!(kinds("DB_PASSWORD = s3cr3t-Pa55"), [(Kind::Password, "s3cr3t-Pa55")]);
        assert_eq!(kinds("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG"), [(Kind::Password, "wJalrXUtnFEMI/K7MDENG")]);
        assert_eq!(kinds("Mot de passe : Tr0ub4dor&3"), [(Kind::Password, "Tr0ub4dor&3")]);
        assert!(kinds("Password: ********").is_empty());
        assert!(kinds("Password: ‹password›").is_empty());
        assert!(kinds("Password
‹password›").is_empty());
        assert!(kinds("Forgot your password? Reset it").is_empty());
    }

    #[test]
    fn labels_in_other_scripts() {
        assert_eq!(kinds("Пароль: hunter2"), [(Kind::Password, "hunter2")]);
        assert_eq!(kinds("كلمة المرور: hunter2"), [(Kind::Password, "hunter2")]);
        assert_eq!(kinds("密码：hunter2"), [(Kind::Password, "hunter2")]);
        assert_eq!(kinds("パスワード: hunter2"), [(Kind::Password, "hunter2")]);
        assert_eq!(kinds("비밀번호 = hunter2"), [(Kind::Password, "hunter2")]);
        assert_eq!(kinds("पासवर्ड: hunter2"), [(Kind::Password, "hunter2")]);
        assert!(kinds("Забыли пароль? Восстановите").is_empty());
    }

    #[test]
    fn keys_and_blocks() {
        assert_eq!(kinds("key AKIAIOSFODNN7EXAMPLE end")[0], (Kind::Key, "AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(kinds("token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789")[0].0, Kind::Key);
        let block = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(kinds(block), [(Kind::PrivateKey, block)]);
    }

    #[test]
    fn seed_phrase() {
        let phrase = "abandon ability able about above absent absorb abstract absurd abuse access accident";
        assert_eq!(kinds(&format!("Recovery words:\n{phrase}\n")), [(Kind::SeedPhrase, phrase)]);
        assert!(kinds("I am able to go above and about the absent").is_empty());
    }

    #[test]
    fn passports() {
        let mrz = "P<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<\nL898902C36UTO7408122F1204159ZE184226B<<<<<10";
        assert_eq!(kinds(mrz).len(), 2);
        // The number printed above, as OCR reads the page (a visa's own number).
        let page = format!("PASSPORT  Passport No. L898902C3\nERIKSSON\n{mrz}");
        assert!(kinds(&page).contains(&(Kind::Document, "L898902C3")), "{:?}", kinds(&page));
    }

    #[test]
    fn machine_readable_lines_as_ocr_reads_them() {
        // Spaces, `«` for two fillers, `K`s for fillers; a visa's first line starts with V and its type.
        let read = "VCD<<ERIKSSON<<ANNA<MARIA ««««««««« <\n070815161 6UTO7408122F2512311KKKKKKKKKKKKKKKK";
        let found = kinds(read);
        assert_eq!(found.iter().filter(|(kind, _)| *kind == Kind::Document).count(), 2, "{found:?}");
        // Its number on the sticker, above.
        let page = format!("VISUM / VISA   070815161\nGULTIG FUR SCHENGENER STAATEN\n{read}");
        assert!(kinds(&page).contains(&(Kind::Document, "070815161")), "{:?}", kinds(&page));
        let spaced = format!("0708 15161\n{read}");
        assert!(kinds(&spaced).contains(&(Kind::Document, "0708 15161")), "{:?}", kinds(&spaced));
    }

    #[test]
    fn numbers_of_a_document_without_its_lines() {
        // A scan whose machine-readable lines didn't read: the words say what it is.
        let scan = "REISEPASS PASSPORT\nC01X00T47\nMUSTERMANN\nERIKA\n12.08.1983\nBERLIN";
        assert_eq!(kinds(scan), [(Kind::Document, "C01X00T47")]);
        assert!(kinds("VISUM 070815161 GULTIG 01.06.2025").contains(&(Kind::Document, "070815161")));
        // Not a document: nothing of the kind, and dates and words aren't numbers.
        assert!(kinds("Order 123456789 shipped to you").is_empty());
        assert!(kinds("Card statement VISA 20251231 total 12345678").is_empty());
        assert!(kinds("PASSPORT office opens 20251231, ask for BERLINER").is_empty());
        // A long text about passports (a guide): only lines and numbers from them.
        let guide = format!("How to renew your passport. {}Reference 123456789", "Bring two photos. ".repeat(300));
        assert!(kinds(&guide).is_empty());
    }

    #[test]
    fn redaction_keeps_the_label() {
        let text = "Wi-Fi password: hunter2";
        let secrets = find(text);
        assert_eq!(redact(text, &secrets), "Wi-Fi password: ‹password›");
        assert_eq!(mask(text, &secrets), "Wi-Fi password: •••••••");
    }
}
