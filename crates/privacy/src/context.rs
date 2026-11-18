//! Secrets told by what's around them rather than by a label and a colon (measured on synthetic screenshots and texts):
//!
//! - **one-time codes**: 4 to 8 digits in a line that speaks of a code ("Your verification code is 482913",
//!   "482913 is your login code", "G-482913"), not an order's, a ticket's, an error's or a receipt's;
//! - **said aloud or written as a sentence**: "my PIN is 4821", "the password is tulip-river-77" (transcripts);
//! - **CVV and PIN without a colon** ("CVV 485", "PIN 4821"), and on a card's back, the three digits by its signature
//!   strip;
//! - **backup codes** in a text about them, a **2FA setup key** (base32, in groups) in a text about an authenticator,
//!   `otpauth://` links, **bearer tokens**, the values of variables named `…_KEY`, `…_SECRET`, `…_TOKEN`;
//! - **unlabelled tokens**: long and random-looking (letters of both cases and digits), within two lines of a key,
//!   token, secret or password word;
//! - a **booking's reference** (it opens the booking), and in a text about Wi-Fi, the value after a bare "Key".

use crate::{Kind, Secret, is_placeholder, password_like};
use regex_lite::Regex;
use std::ops::Range;
use std::sync::OnceLock;

struct Rules {
    number: Regex,
    spaced_digits: Regex,
    code_words: Regex,
    not_code: Regex,
    said: Regex,
    cvv_pin: Regex,
    card_back: Regex,
    backup_words: Regex,
    backup_code: Regex,
    setup_words: Regex,
    base32_groups: Regex,
    base32_run: Regex,
    otpauth: Regex,
    bearer: Regex,
    variable: Regex,
    token: Regex,
    key_words: Regex,
    booking: Regex,
    wifi_key: Regex,
    code_label: Regex,
    wallet_words: Regex,
    /// What a QR code is a secret in besides those: signing in on another screen, paying, moving an authenticator.
    qr_words: Regex,
    function_call: Regex,
    /// A code introduced on the line before it ("Enter this code…", "…the code below"), and a PIN's word.
    code_above: Regex,
    pin_word: Regex,
}

/// Words that say a number is a code to type in (several languages).
/// (Not "code" alone: documents number many things codes, a classification's, a form's.)
const CODE_WORDS: &str = r"(?i)\b(?:verification|verify|one[- ]time|otp|passcode|2fa|two[- ]factor|security code|login code|log-?in code|sign[- ]?in|log[- ]?in|confirmation code|access code|authentication code|your code|(?-i:the code)|code is|code was|code:|is your \w+ code|use code|enter code|codice|best(?:ä|a)tigungscode|sicherheitscode|anmeldecode|einmalcode|c(?:ó|o)digo|v(?:é|e)rification)\b|код";
/// Numbers that a code word is near and aren't one: an order's, a receipt's approval, an error's, a postcode.
const NOT_CODE: &str = r"(?i)\b(?:order|ticket|room|flight|tel|phone|call|zip|postal|post|postcode|error|status|auth(?:orization)? code|auth|approval|ref|reference|invoice|terminal|discount|promo|coupon|voucher|track|tracking|area|country|dial|product|item|sku|article|qr|bar ?code|source code|exit code|http|line|page|version|model|serial|s/n)\b";
/// A line that is a code's or a PIN's label alone: its value is beside or under it.
const CODE_LABEL: &str = r"(?i)^[^A-Za-z0-9]*(?:your\s+|card\s+|new\s+)?(?:one[- ]time (?:password|passcode|code)|otp|totp|verification code|authenticator code|authentication code|security code|login code|sign-in code|2fa code|access code|backup codes?|recovery codes?|booking ?(?:reference|ref\.?|code)|confirmation code|pnr|record locator|reservation code|(?:pin|puk)[12il]?(?: code)?|cvv2?|cvc2?)(?:\s*\([^)]*\))?\s*:?\s*$";

/// An image's words that make its QR code a secret, besides Wi-Fi's, a 2FA setup's and a wallet's: signing in on another
/// screen (a messenger's web app, a TV: whoever scans it first is in), paying (an address, a payment app's code, an
/// invoice), and an authenticator's export (every account's secret at once).
const QR_WORDS: &str = r"(?i)\b(?:log|sign)[- ]?in with (?:a |the |your )?(?:qr|phone|mobile)|\bto (?:log|sign)[- ]?in\b|\blink(?:ed)? (?:a |this |your )?(?:devices?|computer)\b|\banmelden mit qr|\bum sich anzumelden\b|\b(?:upi|paypal|venmo|cash ?app|zelle|lightning|bitcoin|btc|ethereum|usdt|litecoin|monero)\b|\bscan (?:&|and|to) pay\b|\bpay (?:me|with)\b|\bpayment (?:link|request)\b|\b(?:transfer|export) accounts\b|\bmove to a new phone\b|\bon your new (?:device|phone)\b";

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| Rules {
        number: Regex::new(r"\b(?:\d{3}[ -]\d{3}|\d{4,8})\b").unwrap(),
        spaced_digits: Regex::new(r"\b\d(?: \d){3,7}\b").unwrap(),
        code_words: Regex::new(CODE_WORDS).unwrap(),
        not_code: Regex::new(NOT_CODE).unwrap(),
        said: Regex::new(r"(?i)\b(wi-?fi password|wifi key|network key|password|passwort|kennwort|passphrase|passcode|pass code|pin|code)\s+(?:is|was|ist|lautet|est|es)\s*:?\s+").unwrap(),
        cvv_pin: Regex::new(r"(?i)\b(?:(?:cvv2?|cvc2?|cid)\b[ \t]*:?[ \t]*|cvv|cvc)(\d{3,4})\b|\b(?:pin|puk)[12il]?\b[ \t]*:?[ \t]*(\d{4,8})\b").unwrap(),
        // (Leniently: OCR reads "Authorized" as "Authortzed".)
        card_back: Regex::new(r"(?i)\bauth\w{0,8}\s+signat|not valid unless signed").unwrap(),
        backup_words: Regex::new(r"(?i)\b(?:backup|recovery|reserve|emergency|wiederherstellungs|sicherungs)[ -]?codes?\b").unwrap(),
        backup_code: Regex::new(r"^(?:\d{4}[ -]?\d{4}|\d{8,10}|[a-z0-9]{4,5}-[a-z0-9]{4,5}|[A-Z0-9]{4,5}-[A-Z0-9]{4,5})$").unwrap(),
        setup_words: Regex::new(r"(?i)authenticator|two[- ]factor|two[- ]step|2-step|\b2fa\b|setup key|secret key|otpauth|\btotp\b|enter this key").unwrap(),
        // (As OCR reads it: groups run together or split, and base32's letters read as 0, 1 or 8.)
        base32_groups: Regex::new(r"\b[A-Z0-9]{4,16}(?: [A-Z0-9]{4,16}){2,15}\b").unwrap(),
        base32_run: Regex::new(r"\b[A-Z2-7]{16,64}\b").unwrap(),
        otpauth: Regex::new(r"otpauth://\S+").unwrap(),
        bearer: Regex::new(r"(?i)\bbearer[ \t]+([A-Za-z0-9._~+/=-]{16,})").unwrap(),
        variable: Regex::new(r#"\b[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)*_(?:KEY|SECRET|TOKEN|PASSWORD|PASS|PWD|CREDENTIALS?)[ \t]*[:=][ \t]*["']?([^\s"']{8,})"#).unwrap(),
        token: Regex::new(r"[A-Za-z0-9+/_-]{20,}={0,2}").unwrap(),
        // Whole words (or a variable's part: `SESSION_KEY`, `apiKey`), not letters inside random ones.
        key_words: Regex::new(r"(?:\b|_)(?i:key|keys|token|tokens|secret|password|passwort|credentials?|bearer|auth|private)(?:\b|_)|[a-z](?:Key|Token|Secret|Password)\b").unwrap(),
        booking: Regex::new(r"(?i)\b(?:booking ?(?:reference|ref\.?|code)|confirmation (?:code|number)|pnr|record locator|reservation code|buchungscode|buchungsnummer)\b[ \t]*[:#]?[ \t]*([A-Z0-9]{5,8})\b").unwrap(),
        wifi_key: Regex::new(r"(?im)^[ \t]*(?:key|schl(?:ü|ue)ssel|clave|cl(?:é|e))[ \t]*[:=][ \t]*(\S+)").unwrap(),
        code_label: Regex::new(CODE_LABEL).unwrap(),
        wallet_words: Regex::new(r"(?i)\b(?:bitcoin|ethereum|wallet|crypto|private key|seed phrase|recovery phrase|scan to pay|payment code|xpub)\b").unwrap(),
        qr_words: Regex::new(QR_WORDS).unwrap(),
        function_call: Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$.]*\s*[\(\[]").unwrap(),
        code_above: Regex::new(r"(?i)\b(?:this|the|following)\s+(?:\w+\s+)?code\b|\bcode below\b").unwrap(),
        pin_word: Regex::new(r"(?i)\b(?:pin|puk)\b").unwrap(),
    })
}

/// The secrets these rules find in `text` (may overlap others: `find` joins them).
/// `lower`: the text with its ASCII letters lowercased, for `crate::any_of`: each expression below that has words it
/// can't match without is skipped when the text has none of them (the same secrets are found, sooner).
pub(crate) fn find(text: &str, lower: &str) -> Vec<Secret> {
    use crate::any_of;
    let rules = rules();
    let mut found = Vec::new();
    let lines = lines(text);
    // (Lines in order: the one holding a place, by halving.)
    let index_of = |at: usize| lines.partition_point(|(range, _)| range.start <= at).saturating_sub(1);
    let line_of = |at: usize| lines.get(index_of(at)).map(|(range, _)| range.clone()).unwrap_or(0..text.len());

    // One-time codes, by the words around them in their line.
    for hit in rules.number.find_iter(text).chain(rules.spaced_digits.find_iter(text)) {
        let digits = hit.as_str().bytes().filter(u8::is_ascii_digit).count();
        if !(4..=8).contains(&digits) {
            continue;
        }
        if digits == 4 && hit.as_str().len() == 4 && (1900..=2099).contains(&hit.as_str().parse::<u32>().unwrap_or(0)) {
            continue;
        }
        // Part of something longer: a decimal, a time, an amount, a phone number in pieces.
        let before = text[..hit.start()].chars().next_back();
        let after = text[hit.end()..].chars().next();
        if before.is_some_and(|c| matches!(c, '.' | ',' | ':' | '#' | '/' | '+' | '$' | '€' | '£' | '=' | '&' | '?')) || after.is_some_and(|c| matches!(c, ',' | ':' | '/' | '%' | '€' | '&')) {
            continue;
        }
        // One digit over and over: a colour, a placeholder ("000000").
        if hit.as_str().bytes().filter(u8::is_ascii_digit).all(|b| Some(b) == hit.as_str().bytes().next()) {
            continue;
        }
        if after == Some('.') && text[hit.end() + 1..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let line = line_of(hit.start());
        let from = floor(text, hit.start().saturating_sub(60).max(line.start));
        let to = ceil(text, (hit.end() + 60).min(line.end));
        let window = &text[from..to];
        // A message with a code has one number like it; a log or a table has many.
        let numbers = rules.number.find_iter(window).count();
        if numbers <= 1 && rules.code_words.is_match(window) && !rules.not_code.is_match(window) {
            found.push(Secret { kind: Kind::Code, range: hit.range() });
        }
    }

    // A code on a line of its own ("482913", "482 913"), what says it's one on the two lines above it: an e-mail's or a
    // sign-in page's layout. (Not below it: "Customer number / 48291374 / Log in to pay online" isn't one.)
    for (index, (range, line)) in lines.iter().enumerate() {
        if let Some(kind) = code_alone(line, &lines[index.saturating_sub(3)..index]) {
            let start = range.start + (line.len() - line.trim_start().len());
            found.push(Secret { kind, range: start..start + line.trim().len() });
        }
    }

    // Said aloud, or written as a sentence: "my PIN is 4821", "the password is tulip-river-77".
    // ("wi-fi password", "pass code", "network key"…: each has one of these.)
    let said = any_of(lower, &["pass", "key", "kennwort", "pin", "code"]).then(|| rules.said.captures_iter(text));
    for captures in said.into_iter().flatten() {
        let (whole, label) = (captures.get(0).unwrap(), captures[1].to_lowercase());
        let rest_end = text[whole.end()..].find('\n').map_or(text.len(), |at| whole.end() + at);
        let rest = &text[whole.end()..rest_end];
        let numeric = label == "pin" || label.contains("code");
        if numeric {
            // ("the zip code is 90210", "the error code was 4012")
            let line = &text[text[..whole.start()].rfind('\n').map_or(0, |at| at + 1)..rest_end];
            if rules.not_code.is_match(line) {
                continue;
            }
            let run: usize = rest.char_indices().take_while(|(_, c)| c.is_ascii_digit() || *c == ' ' || *c == '-').map(|(_, c)| c.len_utf8()).sum();
            let value = rest[..run].trim_end();
            let count = value.bytes().filter(u8::is_ascii_digit).count();
            if (4..=8).contains(&count) {
                let kind = if label == "pin" { Kind::Password } else { Kind::Code };
                found.push(Secret { kind, range: whole.end()..whole.end() + value.len() });
            }
        } else {
            let word = rest.split_whitespace().next().unwrap_or("");
            let value = word.trim_end_matches(['.', ',', '!', '?', ';']);
            if !value.is_empty() && password_like(value) && rest.starts_with(word) {
                found.push(Secret { kind: Kind::Password, range: whole.end()..whole.end() + value.len() });
            }
        }
    }

    // CVV and PIN without a colon ("CVV 485", "PIN 4821").
    let cvv_pin = any_of(lower, &["cvv", "cvc", "cid", "pin", "puk"]).then(|| rules.cvv_pin.captures_iter(text));
    for captures in cvv_pin.into_iter().flatten() {
        if let Some(value) = captures.get(1).or_else(|| captures.get(2)) {
            found.push(Secret { kind: Kind::Password, range: value.range() });
        }
    }
    // A card's back: the three or four digits alone on a line by the signature strip.
    if any_of(lower, &["signat", "not valid unless signed"]) && rules.card_back.is_match(text) {
        for (range, line) in &lines {
            let value = line.trim();
            if (3..=4).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit()) {
                let start = range.start + (line.len() - line.trim_start().len());
                found.push(Secret { kind: Kind::Password, range: start..start + value.len() });
            }
        }
    }

    // Backup codes, in a text about them: every line that is one.
    if lower.contains("code") && any_of(lower, &["backup", "recovery", "reserve", "emergency", "wiederherstellungs", "sicherungs"])
        && rules.backup_words.is_match(text) {
        for (range, line) in &lines {
            for (offset, piece) in pieces(line) {
                if rules.backup_code.is_match(piece) && piece.bytes().any(|b| b.is_ascii_digit()) {
                    let start = range.start + offset;
                    found.push(Secret { kind: Kind::Code, range: start..start + piece.len() });
                }
            }
        }
    }

    // A 2FA setup key: base32, in groups of four or in one run, in a text about an authenticator.
    // (The words found once, and each key looks for one between where its lines start and end: the same as looking
    // through each line, as the words never span lines, and not once per key: a megabyte line of binary read as text
    // had hundreds of thousands of key-like runs, each looking through the line again.)
    let setup_at: Vec<std::ops::Range<usize>> = if any_of(lower, SETUP_WORDS) { rules.setup_words.find_iter(text).map(|hit| hit.range()).collect() } else { Vec::new() };
    if !setup_at.is_empty() {
        for hit in rules.base32_groups.find_iter(text).chain(rules.base32_run.find_iter(text)) {
            // Within three lines of what says it's one (on lines of megabytes, within `NEAR` of it).
            let index = index_of(hit.start());
            let first = &lines[index.saturating_sub(3)];
            let own = &lines[index.min(lines.len() - 1)];
            let long = lines[index.saturating_sub(3)..(index + 1).min(lines.len())].iter().any(|(_, line)| line.len() > 2 * NEAR);
            let (from, to) = if long {
                (first.0.start.max(hit.start().saturating_sub(NEAR)), own.0.end.min(hit.end() + NEAR))
            } else {
                (first.0.start, own.0.end)
            };
            let after = setup_at.partition_point(|word| word.start < from);
            let near = setup_at.get(after).is_some_and(|word| word.end <= to);
            let digits = hit.as_str().bytes().filter(u8::is_ascii_digit).count();
            let letters = hit.as_str().bytes().filter(u8::is_ascii_uppercase).count();
            // (A key may have no digit: then four groups of exactly four, as authenticators print them.)
            let fours = hit.as_str().split(' ').count() >= 4 && hit.as_str().split(' ').all(|group| group.len() == 4);
            if near && letters >= 4 && digits + letters >= 16 && (digits >= 1 || fours) {
                found.push(Secret { kind: Kind::Key, range: hit.range() });
            }
        }
    }
    if text.contains("otpauth://") {
        for hit in rules.otpauth.find_iter(text) {
            found.push(Secret { kind: Kind::Key, range: hit.range() });
        }
    }
    if lower.contains("bearer") {
        for captures in rules.bearer.captures_iter(text) {
            let value = captures.get(1).unwrap();
            found.push(Secret { kind: Kind::Key, range: value.range() });
        }
    }
    let variables = any_of(text, &["_KEY", "_SECRET", "_TOKEN", "_PASS", "_PWD", "_CREDENTIAL"]).then(|| rules.variable.captures_iter(text));
    for captures in variables.into_iter().flatten() {
        let value = captures.get(1).unwrap();
        let shown = value.as_str().trim_end_matches([',', ';']);
        if !is_placeholder(shown) && !code_not_value(shown) && !shown.bytes().all(|b| b.is_ascii_digit()) {
            found.push(Secret { kind: Kind::Key, range: value.start()..value.start() + shown.len() });
        }
    }

    // Unlabelled tokens: long and random-looking, within two lines of a key, token, secret or password word.
    let tokens = token_run(text).then(|| rules.token.find_iter(text));
    for hit in tokens.into_iter().flatten() {
        let token = hit.as_str();
        if !random_looking(token) || token.contains("//") || token.starts_with('/') {
            continue;
        }
        // Part of something longer ("…dA=Debian:pool/…": a file's checksum in a list).
        if text[hit.end()..].chars().next().is_some_and(char::is_alphanumeric) {
            continue;
        }
        // A line of a base64 block (an attachment, an embedded picture): long, and all of its line.
        let index = index_of(hit.start());
        if token.len() >= 60 && lines.get(index).is_some_and(|(_, line)| line.trim() == token) {
            continue;
        }
        // An identifier in pieces ("ctl00_cntMain_rpSections", "Menu1-menuItem001-subMenu"): few digits among its letters.
        // (Not random groups, "CmkXhDjW-Y8xxtZws-hydtKnCi": their letters change case all the way.)
        let digits = token.bytes().filter(u8::is_ascii_digit).count();
        if (token.contains('_') || token.contains('-')) && digits * 7 < token.len() && !random_case(token) {
            continue;
        }
        // In a link's query ("?id=…&auth=…&ref=…"): a page's own, not something kept.
        // (Past the space, however wide: a no-break space is two bytes, and one past its start cut it in half,
        // a panic that ended the documents' reading for good on a slide deck.)
        // (Within `NEAR` of it: a file of one line with no spaces, 2 MB of binary read as text, had every token scan the
        // whole file, minutes in all; ordinary text has a space long before that.)
        let from = floor(text, hit.start().saturating_sub(NEAR));
        let to = ceil(text, (hit.end() + NEAR).min(text.len()));
        let chunk_start = text[from..hit.start()].char_indices().rev().find(|(_, c)| c.is_whitespace()).map_or(from, |(at, c)| from + at + c.len_utf8());
        let chunk_end = text[hit.end()..to].find(char::is_whitespace).map_or(to, |at| hit.end() + at);
        let chunk = &text[chunk_start..chunk_end];
        if chunk.contains('?') && chunk.contains('=') || chunk.contains("&amp;") || text[hit.end()..].starts_with('&') {
            continue;
        }
        // In a link, a path or an address: not a token on its own.
        let before = text[..hit.start()].chars().next_back();
        let after = text[hit.end()..].chars().next();
        if before.is_some_and(|c| matches!(c, '.' | '@' | '\\' | ':')) && !text[..hit.start()].ends_with(": ") || after.is_some_and(|c| matches!(c, '.' | '@' | '\\')) && text[hit.end()..].chars().nth(1).is_some_and(char::is_alphanumeric) {
            continue;
        }
        let near = lines[index.saturating_sub(2)..(index + 3).min(lines.len())].iter().any(|(range, line)| {
            // (The token's own letters don't count: "…Key…" inside it. A line longer than `NEAR` counts by its part
            // nearest the token, for the same reason as the chunk above; shorter ones whole, as ever.)
            let own = range.start <= hit.start() && hit.end() <= range.end;
            if own {
                let local = hit.start() - range.start..hit.end() - range.start;
                let before = &line[floor(line, local.start.saturating_sub(NEAR))..local.start];
                let after = &line[local.end..ceil(line, (local.end + NEAR).min(line.len()))];
                rules.key_words.is_match(before) || rules.key_words.is_match(after)
            } else if range.end <= hit.start() {
                rules.key_words.is_match(&line[floor(line, line.len().saturating_sub(NEAR))..])
            } else {
                rules.key_words.is_match(&line[..ceil(line, NEAR.min(line.len()))])
            }
        });
        if near {
            found.push(Secret { kind: Kind::Key, range: hit.range() });
        }
    }

    // A booking's reference, on its label's line; and a bare "Key: …" on a Wi-Fi card or sticker.
    let bookings = any_of(lower, &["booking", "confirmation ", "pnr", "record locator", "reservation code", "buchungs"]).then(|| rules.booking.captures_iter(text));
    for captures in bookings.into_iter().flatten() {
        let value = captures.get(1).unwrap();
        if value.as_str().bytes().any(|b| b.is_ascii_uppercase()) {
            found.push(Secret { kind: Kind::Code, range: value.range() });
        }
    }
    if crate::about_wifi_lower(text, lower) {
        for captures in rules.wifi_key.captures_iter(text) {
            let value = captures.get(1).unwrap();
            if password_like(value.as_str()) {
                found.push(Secret { kind: Kind::Password, range: value.range() });
            }
        }
        for pair in lines.windows(2) {
            let label = pair[0].1.trim().trim_end_matches([':', '：']).trim();
            let value = pair[1].1.trim();
            if ["key", "schlüssel", "clave", "clé"].contains(&label.to_lowercase().as_str()) && password_like(value) {
                let start = pair[1].0.start + (pair[1].1.len() - pair[1].1.trim_start().len());
                found.push(Secret { kind: Kind::Password, range: start..start + value.len() });
            }
        }
    }
    found
}

/// The kind of a `line` that is a code alone (4 to 8 digits, maybe in groups), when the nearest two lines with
/// something on them `above` it say it's one: a code's words ("Use the code below to verify your email address:",
/// "Your one-time passcode"), or a PIN's ("Scratch here to reveal your PIN"); and nothing that makes it an order's, a
/// phone's or a version's. None for any other line.
fn code_alone(line: &str, above: &[(Range<usize>, &str)]) -> Option<Kind> {
    let value = line.trim();
    let digits = value.bytes().filter(u8::is_ascii_digit).count();
    if !(4..=8).contains(&digits) || !value.bytes().all(|b| b.is_ascii_digit() || b == b' ') || value.contains("  ") {
        return None;
    }
    // A year; one digit over and over (a placeholder).
    if (value.len() == 4 && (1900..=2099).contains(&value.parse::<u32>().unwrap_or(0))) || value.bytes().all(|b| b == value.as_bytes()[0]) {
        return None;
    }
    let rules = rules();
    // (Lines of a sentence or two: a line of megabytes, binary read as text, says nothing about the next.)
    let near: Vec<&str> = above.iter().rev().map(|(_, line)| line.trim()).filter(|line| !line.is_empty()).take(2).collect();
    if near.is_empty() || near.iter().any(|line| line.len() > 300 || rules.not_code.is_match(line)) {
        return None;
    }
    if near.iter().any(|line| rules.pin_word.is_match(line)) {
        Some(Kind::Password)
    } else if near.iter().any(|line| rules.code_words.is_match(line) || rules.code_above.is_match(line)) {
        Some(Kind::Code)
    } else {
        None
    }
}

/// What every match of `setup_words` holds ("two-factor", "two step": "two").
const SETUP_WORDS: &[&str] = &["authenticator", "two", "2-step", "2fa", "setup key", "secret key", "otpauth", "totp", "enter this key"];

/// Whether `text` has a run of 20 of `token`'s characters (letters, digits, `+/_-`): every match of it starts with one.
fn token_run(text: &str) -> bool {
    let mut run = 0;
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'_' | b'-') {
            run += 1;
            if run >= 20 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// The kind of a value under (or beside) a code's or a PIN's label alone on its line: digits ("482 913", "4 8 2 1"), or a
/// booking's reference ("X7K9QP"). None for anything else.
pub(crate) fn value_for_label(label: &str, value: &str) -> Option<Kind> {
    let rules = rules();
    let label = label.trim();
    if label.chars().count() > 48 || !rules.code_label.is_match(label) {
        return None;
    }
    let lower = label.to_lowercase();
    let value = value.trim();
    let digits = value.bytes().filter(u8::is_ascii_digit).count();
    let numeric = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit() || b == b' ' || b == b'-');
    let pin = lower.contains("pin") || lower.contains("puk") || lower.contains("cvv") || lower.contains("cvc");
    let least = if lower.contains("cvv") || lower.contains("cvc") { 3 } else { 4 };
    if numeric && (least..=10).contains(&digits) {
        return Some(if pin { Kind::Password } else { Kind::Code });
    }
    let booking = ["booking", "confirmation", "pnr", "locator", "reservation"].iter().any(|word| lower.contains(word));
    // (Not a flight's number, "XY318", read next to it.)
    let flight = value.len() >= 3 && value.is_char_boundary(2) && value[..2].bytes().all(|b| b.is_ascii_uppercase()) && value[2..].bytes().all(|b| b.is_ascii_digit());
    if booking && !flight && (5..=8).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) && value.bytes().any(|b| b.is_ascii_uppercase()) {
        return Some(Kind::Code);
    }
    // Backup codes in letters and digits ("k7qp-2mx9").
    if (lower.contains("backup") || lower.contains("recovery")) && rules.backup_code.is_match(value) && digits > 0 {
        return Some(Kind::Code);
    }
    None
}

/// Whether a line is a code's or a PIN's label alone.
pub(crate) fn code_label(line: &str) -> bool {
    line.chars().count() <= 48 && rules().code_label.is_match(line.trim())
}

/// Whether a QR code in an image with this text is a secret, and of what kind: a Wi-Fi network's password (the text is
/// about Wi-Fi), a 2FA secret (about an authenticator or two-factor sign-in, or moving one), a wallet's or a payment's
/// code, or one that signs in on another screen (`QR_WORDS`).
pub fn qr_is_secret(text: &str) -> Option<Kind> {
    let rules = rules();
    if crate::about_wifi(text) {
        Some(Kind::Password)
    } else if rules.setup_words.is_match(text) || rules.wallet_words.is_match(text) || rules.qr_words.is_match(text) {
        Some(Kind::Key)
    } else {
        None
    }
}

/// A value after a label that is code, not a secret: `form.get("password")`, `os.environ['API_KEY']`, `${DB_PASSWORD}`,
/// `process.env.API_KEY`, `null`, "(the one you chose)".
pub(crate) fn code_not_value(value: &str) -> bool {
    let value = value.trim_start_matches("await ").trim_start_matches("new ");
    let lower = value.to_lowercase();
    rules().function_call.is_match(value)
        || value.contains("</") || value.contains("/>") || value.get(..3).is_some_and(|head| head.contains('>'))
        || value.starts_with('$') || value.starts_with('(') || value.starts_with('{') || value.starts_with('<') || value.starts_with('%')
        || lower.starts_with("process.env") || lower.starts_with("os.environ") || lower.starts_with("env[") || lower.contains("getenv")
        || ["null", "none", "nil", "true", "false", "undefined", "\"\"", "''", "required", "optional"].contains(&lower.as_str())
        || prose(&lower)
}

/// Words of a sentence ("see the dashboard", "the one you chose"), not a passphrase: three words or more, letters only,
/// one of them a little word a sentence has.
fn prose(lower: &str) -> bool {
    let words: Vec<&str> = lower.split_whitespace().collect();
    words.len() >= 3 && words.iter().all(|word| word.chars().all(char::is_alphabetic))
        && words.iter().any(|word| ["the", "a", "an", "in", "on", "at", "your", "my", "see", "is", "are", "to", "of", "from", "with", "you"].contains(word))
}

/// A token that looks random: letters of both cases and digits; or long, with letters and digits and not only hex (a
/// commit's or a file's hash, a colour, a UUID aren't secrets).
fn random_looking(token: &str) -> bool {
    let token = token.trim_end_matches('=');
    // A hash as OCR reads it: hex with a word run into it ("commit9198f3e1…") or a letter misread ("cl906fe…").
    let hex = token.bytes().filter(u8::is_ascii_hexdigit).count();
    if !token.bytes().any(|b| b.is_ascii_uppercase()) && hex * 100 >= token.len() * 85 {
        return false;
    }
    // A slug or an anchor's id ("i1536-2442-5-42-1-smith1"): no capitals, in pieces.
    if !token.bytes().any(|b| b.is_ascii_uppercase()) && token.contains('-') {
        return false;
    }
    let lower = token.bytes().any(|b| b.is_ascii_lowercase());
    let upper = token.bytes().any(|b| b.is_ascii_uppercase());
    let digits = token.bytes().filter(u8::is_ascii_digit).count();
    let letters = token.bytes().filter(u8::is_ascii_alphabetic).count();
    if token.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return false;
    }
    // An identifier in words ("getTokenFromStorage", "MAX_RETRY_COUNT_FOR_UPLOADS"): few digits, long runs of letters.
    // (Random letters with one digit or none, "CmkXhDjW-Y8xxtZws-hydtKnCi", change case about every other letter.)
    if digits < 2 && !random_case(token) {
        return false;
    }
    let changes = token.as_bytes().windows(2).filter(|pair| class(pair[0]) != class(pair[1])).count();
    let mixed = changes * 4 >= token.len();
    (lower && upper && mixed) || (token.len() >= 32 && letters > 0 && mixed) || random_lowercase(token, letters, mixed)
}

/// Small letters and digits in no order words have ("kpvqef0jk1mn69ftwampbvn4i", as many services print tokens): all of
/// it alphanumeric, and few vowels among the letters (random letters have about one in five, words two in five) or
/// letters and digits mixed through it.
fn random_lowercase(token: &str, letters: usize, mixed: bool) -> bool {
    let vowels = token.bytes().filter(|b| b"aeiou".contains(b)).count();
    token.len() >= 20 && letters >= 8 && token.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) && (vowels * 10 < letters * 3 || mixed)
}

/// Whether a token's letters are in random case ("CmkXhDjW", not "GetProcessMemoryInfo"): they go from small to capital
/// or back at least every third letter, and at least a third of them are capitals (words in camel case have one in five
/// or six, each a change both ways).
fn random_case(token: &str) -> bool {
    let letters = token.bytes().filter(u8::is_ascii_alphabetic).count();
    let capitals = token.bytes().filter(u8::is_ascii_uppercase).count();
    let changes = token.as_bytes().windows(2).filter(|pair| pair[0].is_ascii_alphabetic() && pair[1].is_ascii_alphabetic()
        && pair[0].is_ascii_uppercase() != pair[1].is_ascii_uppercase()).count();
    letters > 0 && changes * 3 >= letters && capitals * 3 >= letters
}

fn class(byte: u8) -> u8 {
    if byte.is_ascii_lowercase() { 0 } else if byte.is_ascii_uppercase() { 1 } else if byte.is_ascii_digit() { 2 } else { 3 }
}

/// The text's lines, each with its byte range (without the line break).
fn lines(text: &str) -> Vec<(Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        out.push((at..at + trimmed.len(), trimmed));
        at += line.len();
    }
    out
}

/// A line's pieces split at wide gaps (two columns read as one line), each with its offset.
fn pieces(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for piece in line.split("  ") {
        let trimmed = piece.trim();
        if !trimmed.is_empty() {
            out.push((at + (piece.len() - piece.trim_start().len()), trimmed));
        }
        at += piece.len() + 2;
    }
    out
}

/// How far around a token the words that make it a secret are looked for, at most (bytes): far past any ordinary
/// line, so only a line of megabytes (binary read as text) is cut.
const NEAR: usize = 4096;

fn floor(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil(text: &str, mut at: usize) -> usize {
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use crate::{Kind, find};

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        find(text).into_iter().map(|secret| (secret.kind, &text[secret.range])).collect()
    }

    #[test]
    fn a_line_of_megabytes_takes_seconds_not_minutes() {
        // Binary read as text: one line, with an authenticator's word, a key word and ~19,000 key-like runs. Each run
        // looked through the whole line again: a 2.2 MB file took over five minutes, and a reader's time limit ended it.
        let mut text = String::from("authenticator api key ");
        while text.len() < 400_000 {
            text.push_str("JBSWY3DPEHPK3PXP7Q2Z ");
        }
        let started = std::time::Instant::now();
        let found = find(&text);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
        // Near the words, as before: the first runs are keys; far from them on the line, not.
        assert!(found.first().is_some_and(|secret| secret.range.start < 100));
        assert!(found.iter().all(|secret| secret.range.start < super::NEAR + 100), "{} found", found.len());
    }

    #[test]
    fn a_token_after_a_wide_space_is_no_panic() {
        // A no-break space (two bytes) right before a random-looking token: the chunk around the token began one byte
        // into it, and the documents' reading ended on the slide deck that had it.
        for space in ['\u{a0}', '\u{2003}', '\u{3000}'] {
            let text = format!("API key{space}Kj8sD9fLq2Zx7Wm4Rt6Yp1Nb3Vc5\nsecond line");
            let found = kinds(&text);
            assert!(found.iter().all(|(_, value)| !value.is_empty()), "{found:?}");
        }
        // And with anything at all around such a token, from any byte on.
        let text = "x\u{a0}Kj8sD9fLq2Zx7Wm4Rt6Yp1Nb3Vc5\u{a0}y token\u{a0}Q9wE8rT7yU6iO5pA4sD3fG2hJ1k password";
        for (at, _) in text.char_indices() {
            let _ = find(&text[at..]);
        }
    }

    #[test]
    fn one_time_codes_by_their_words() {
        assert_eq!(kinds("Your Contoso verification code is 482913. It expires in 10 minutes."), [(Kind::Code, "482913")]);
        assert_eq!(kinds("482913 is your login code. Don't share it."), [(Kind::Code, "482913")]);
        assert_eq!(kinds("G-582913 is your Google verification code."), [(Kind::Code, "582913")]);
        assert_eq!(kinds("Use 7391 to sign in. Your code expires soon"), [(Kind::Code, "7391")]);
        assert_eq!(kinds("Dein Bestätigungscode: 902114"), [(Kind::Code, "902114")]);
        // Codes that aren't secrets, and numbers near the word "code" that aren't codes.
        for text in ["99-099-15:44:57.8    6372 nrt group uplinked with verification on :CDS    49167",
                     "Code, see Short Title note set out under section 15001 of Title 42", "[FR Doc. 91-18028 Filed 7-25-91 3:06 pm]", "blank or not classifiable code 99999.", "the zip code is 90210",
                     "AUTH CODE: 123456  Terminal 00418823", "Approval code 482913", "Error code 4012: try again", "Zip code 90210, phone 555-0142",
                     "Order #482211 shipped. Track it with code ABC123", "Use discount code SAVE20 for 20% off", "the code review is at 3 pm",
                     "Payment of 24.99 EUR received. Reference 1234567890", "See you at 7:30!"] {
            assert!(kinds(text).is_empty(), "{text}: {:?}", kinds(text));
        }
    }

    #[test]
    fn said_aloud() {
        assert_eq!(kinds("yeah my PIN is 4821 don't tell anyone"), [(Kind::Password, "4821")]);
        assert_eq!(kinds("so the password is pepperCopper39 all one word"), [(Kind::Password, "pepperCopper39")]);
        assert_eq!(kinds("the verification code is 4 8 2 1 9 3 okay"), [(Kind::Code, "4 8 2 1 9 3")]);
        assert_eq!(kinds("der Code lautet 551902 bitte eingeben"), [(Kind::Code, "551902")]);
        assert!(kinds("I forgot my password again so I reset it").is_empty());
        assert!(kinds("the password is in the drawer").is_empty());
    }

    #[test]
    fn cvv_and_pin_without_a_colon() {
        assert_eq!(kinds("CVV 485"), [(Kind::Password, "485")]);
        assert_eq!(kinds("CVV485"), [(Kind::Password, "485")], "as Quick's image reader reads it");
        assert_eq!(kinds("Card PIN 4821"), [(Kind::Password, "4821")]);
        assert_eq!(kinds("Authorized signature\n391\nCustomer service +1 800 555 0199"), [(Kind::Password, "391")]);
        assert!(kinds("Pin 2 of the connector").is_empty());
    }

    #[test]
    fn codes_under_their_labels() {
        assert_eq!(kinds("Verification code\n954 631"), [(Kind::Code, "954 631")]);
        assert_eq!(kinds("One-time password (TOTP)\n305 070"), [(Kind::Code, "305 070")]);
        assert_eq!(kinds("Your PIN\n2 5 4 7"), [(Kind::Password, "2 5 4 7")]);
        assert_eq!(kinds("Booking reference\nX7K9QP"), [(Kind::Code, "X7K9QP")]);
        assert_eq!(kinds("Bookingref.\nK4KMCB"), [(Kind::Code, "K4KMCB")]);
        assert!(kinds("Booking reference\nXY318").is_empty(), "a flight's number");
        assert_eq!(kinds("Confirmation code: K2M8PQ"), [(Kind::Code, "K2M8PQ")]);
        assert!(kinds("PIN\nPlease memorise it").is_empty());
    }

    #[test]
    fn backup_codes_and_setup_keys() {
        let codes = kinds("Backup codes\nEach code can be used once.\n8492 1837\n77310042\nk7qp-2mx9");
        assert_eq!(codes, [(Kind::Code, "8492 1837"), (Kind::Code, "77310042"), (Kind::Code, "k7qp-2mx9")]);
        assert_eq!(kinds("Set up your authenticator app\nOr enter this key:\nJBSW Y3DP EHPK 3PXP"), [(Kind::Key, "JBSW Y3DP EHPK 3PXP")]);
        assert_eq!(kinds("otpauth://totp/Example:alex?secret=JBSWY3DPEHPK3PXP&issuer=Example")[0].0, Kind::Key);
        assert!(kinds("PLEASE READ THIS CARE FULL").is_empty());
    }

    #[test]
    fn tokens_and_variables() {
        assert_eq!(kinds("curl -H \"Authorization: Bearer Zk3mP9qL2xV8nR4tW7yB1cD5\" https://x"), [(Kind::Key, "Zk3mP9qL2xV8nR4tW7yB1cD5")]);
        assert_eq!(kinds("SESSION_KEY=GcftyTrEBMHoJ6bT4csfZzYN8khYBA6H"), [(Kind::Key, "GcftyTrEBMHoJ6bT4csfZzYN8khYBA6H")]);
        assert_eq!(kinds("AWS Secret Access Key [None]: wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"), [(Kind::Password, "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY")]);
        assert_eq!(kinds("  \"apiKey\": \"Q9xW2mLp7RtV4nB8kZ3cY6hJ\","), [(Kind::Password, "Q9xW2mLp7RtV4nB8kZ3cY6hJ")]);
        // Near a token word, but not random: hashes, identifiers, code.
        for text in ["<a accesskey=\"M\" href=\"MouseSNP.cgi\">Mouse Strains</a>",
                     "href=\"redirect3.cgi?&amp;&amp;auth=0jqKYPHhHM5YZF7HGT9cQ70BxgSMh7IzKr6v5l54p&amp;reftype=pubmed\"",
                     "<h3 id=\"ctl00_cntMain_rpSections_ctl00_rpContent_ctl01_hgcHtmlHeader\">Auth</h3>",
                     "getElementById('PageHeader_menurow_Menu1-menuItem001-subMenu') // token",
                     "ckzydZ2+VS+Wt9lj3k/UbshdWN30a1wbNIVVax/OzmJfeRd6WPUCoGmaZjSaK/nKcy/89LmT75EE\nkXBoZnp8Znq8sLaxkr/lrQ6ymenxF+cm6DeBVVZHR/t7xk1m",
                     "Use two-factor sign-in.\n\n\n\nFEDERAL EMERGENCY MANAGEMENT AGENCY FEMA 308",
                     "\"FilterTab.Pin\":1, \"EntrezSystem2.PEntrez.Pubmed\":2",
                     "commit9198f3e1456de41c1e95b5701c28a6f28656626a\nconst token = await getToken(user.id);",
                     "<LABEL FOR=searchtype ACCESSKEY=A> as </LABEL>", "<font color=000000 face=sans-serif> Login</font>",
                     "libapache2-mod-auth-pam_1.1.1-6_sparc.deb\nY0uvQfbgfDid0ok3Jyn9dQ=Debian:pool/main/liba/libapache2-mod-auth",
                     "commit 810dfdecf3a0fa3de9ee79dada2033cc680c2e58\nFix token refresh", "const token = await getTokenFromStorage(user);",
                     "headers.Authorization = `Bearer ${token}`;", "const password = form.get(\"password\");", "api_key = os.environ['API_KEY']",
                     "password: ${DB_PASSWORD}\napiKey: process.env.API_KEY", "PASSWORD_MIN_LENGTH = 12\nTOKEN_TTL_SECONDS = 3600",
                     "id: 51ec77d3-64e2-40a5-a6de-d44663ce50ca\ntoken_type: bearer"] {
            assert!(kinds(text).is_empty(), "{text}: {:?}", kinds(text));
        }
    }

    #[test]
    fn dots_read_as_zeros_hide_nothing() {
        assert!(kinds("Password\n00000000\nForgot password?").is_empty());
        assert!(kinds("Password: oooooooo").is_empty());
        assert!(kinds("Password\n00800080\nForgot password?").is_empty());
    }

    #[test]
    fn a_bare_key_on_a_wifi_card() {
        assert_eq!(kinds("Guest WiFi\nNetwork: Cafe-4F2A\nKey: velvet-lunar-39"), [(Kind::Password, "velvet-lunar-39")]);
        assert_eq!(kinds("Wi-Fi Zugang\nKey\nHarbor2023%"), [(Kind::Password, "Harbor2023%")]);
        assert!(kinds("Key: velvet-lunar-39").is_empty(), "not about Wi-Fi");
    }

    #[test]
    fn qr_codes_that_are_secrets() {
        assert_eq!(crate::qr_is_secret("Scan to connect\nGuest WiFi"), Some(Kind::Password));
        assert_eq!(crate::qr_is_secret("Scan this QR code with your authenticator app"), Some(Kind::Key));
        assert_eq!(crate::qr_is_secret("Receive Bitcoin\nYour wallet address"), Some(Kind::Key));
        assert_eq!(crate::qr_is_secret("Scan for our menu\nTable 12"), None);
        // Signing in on another screen, paying, moving an authenticator.
        assert_eq!(crate::qr_is_secret("Log in with QR Code\nScan this with the mobile app to log in instantly."), Some(Kind::Key));
        assert_eq!(crate::qr_is_secret("Use WhatsApp on your computer\n2. Tap Menu, then Linked devices"), Some(Kind::Key));
        assert_eq!(crate::qr_is_secret("Scan & Pay with any UPI app"), Some(Kind::Key));
        assert_eq!(crate::qr_is_secret("Lightning invoice\nExpires in 10 minutes"), Some(Kind::Key));
        assert_eq!(crate::qr_is_secret("Transfer accounts\nScan this QR code on your new device"), Some(Kind::Key));
        for text in ["Get the app\nScan with your phone's camera to download", "Enjoyed your stay?\nScan to leave us a review",
                     "Track your parcel\nOrder 48291374", "Order confirmed\nSign in to track your order."] {
            assert_eq!(crate::qr_is_secret(text), None, "{text}");
        }
    }

    #[test]
    fn a_code_on_a_line_of_its_own() {
        assert_eq!(kinds("Use the code below to verify your email address:\n61553527\nThis code expires in 10 minutes."), [(Kind::Code, "61553527")]);
        assert_eq!(kinds("Your one-time passcode\nEnter this code to finish signing in:\n731 432\n"), [(Kind::Code, "731 432")]);
        assert_eq!(kinds("We noticed a sign-in from a new device. Your code:\n\n3648129"), [(Kind::Code, "3648129")]);
        assert_eq!(kinds("Scratch here to reveal your PIN\n4821 9930"), [(Kind::Password, "4821 9930")]);
        // Numbers alone on their line that aren't codes: an order's, a customer's (the words are below it), a year.
        for text in ["Order confirmed\nOrder number\n48291374\nSign in to track your order.",
                     "Invoice INV-482913\nCustomer number\n48291374\nPlease log in to pay online.",
                     "Use the code below to verify your email address:\n2026", "Verification\nPage 3 of 12\n4821"] {
            assert!(kinds(text).is_empty(), "{text}: {:?}", kinds(text));
        }
    }

    #[test]
    fn pins_and_puks_on_a_sim_card() {
        assert_eq!(kinds("SIM card\nPIN1 6708\nPUK1 40361045"), [(Kind::Password, "6708"), (Kind::Password, "40361045")]);
        assert_eq!(kinds("PIN: 1626   PUK: 40851163"), [(Kind::Password, "1626"), (Kind::Password, "40851163")]);
        assert_eq!(kinds("Your SIM card\nPIN1\n3710\nPUKI\n41742791"), [(Kind::Password, "3710"), (Kind::Password, "41742791")], "as OCR reads it");
    }

    #[test]
    fn unlabelled_tokens_in_small_letters_or_in_groups() {
        assert_eq!(kinds("here's the new API token for staging\nkpvqef0jk1mn69ftwampbvn4i\nkeep it out of the repo"), [(Kind::Key, "kpvqef0jk1mn69ftwampbvn4i")]);
        assert_eq!(kinds("y34iwb66u8lz4oue1le53y52aew7o\n^ that's the webhook secret"), [(Kind::Key, "y34iwb66u8lz4oue1le53y52aew7o")]);
        assert_eq!(kinds("the deploy key is below\nhuFhHKnG-iIjhtypb-fN5Km1rJ"), [(Kind::Key, "huFhHKnG-iIjhtypb-fN5Km1rJ")]);
        assert_eq!(kinds("Password for the shared drive (temporary)\nCmkXhDjW-Y8xxtZws-hydtKnCi"), [(Kind::Key, "CmkXhDjW-Y8xxtZws-hydtKnCi")], "one digit");
        // Words and numbers run together aren't random.
        // Nor a name in camel case near a key's word.
        for text in ["the key is in screenshot20240513093412", "token refresh in releasenotes2024version3",
                     "/// Private bytes.\nuse windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};"] {
            assert!(kinds(text).is_empty(), "{text}: {:?}", kinds(text));
        }
    }

    #[test]
    fn router_stickers() {
        assert_eq!(kinds("SSID: NET-9E6F
Network Key (WPA2-PSK)
hJiG82c5ywnA"), [(Kind::Password, "hJiG82c5ywnA")]);
        assert_eq!(kinds("WLAN-Name: NET-88C2
Password
1280 5489 2466"), [(Kind::Password, "1280 5489 2466")]);
        assert_eq!(kinds("Wi-Fi properties
Network security key
0w8au2fni4"), [(Kind::Password, "0w8au2fni4")]);
        assert!(kinds("Password
12 34").is_empty());
    }

    #[test]
    fn setup_keys_as_ocr_reads_them() {
        let text = "Scan this QR code with your authenticator app
Can't scan? Enter this key
OB52 BSZXL5PNHPKD BOOS WRC2 D4FX MNX4";
        assert_eq!(kinds(text), [(Kind::Key, "OB52 BSZXL5PNHPKD BOOS WRC2 D4FX MNX4")]);
    }

    #[test]
    fn more_as_ocr_reads_them() {
        assert_eq!(kinds("Enable 2FA
Can't scan? Enter this key
LMSJ ZAIK DNXL REXY"), [(Kind::Key, "LMSJ ZAIK DNXL REXY")]);
        assert_eq!(kinds("Authortzed signature
245
Customer service"), [(Kind::Password, "245")]);
        // A passport's number that starts like a document's code ("I", then letters).
        let page = "Passport No.
ILFBD2XMI
P<UTOERIKSSON<<ANNA<MARIA<<<<<<<<<<<<<<<<<<<
ILFBD2XMI7UT08402183M3208162<<<<<<<<<<<<<<0";
        assert!(kinds(page).contains(&(Kind::Document, "ILFBD2XMI")), "{:?}", kinds(page));
    }

    #[test]
    fn a_passports_number_as_ocr_reads_it() {
        // The country with a 0 for its O, the number printed above with an O for the line's 0.
        let page = "Passport No.
P9W72MOYX
P<UT0RIVERA<<SAM<<<<<<<<<<<<<<<<<<<<<<<<<<<<
P9W72M0YX4UT06509217M3305197<<<<<<<<<<<<<<7";
        let found = kinds(page);
        assert!(found.contains(&(Kind::Document, "P9W72MOYX")), "{found:?}");
        assert!(found.contains(&(Kind::Document, "P<UT0RIVERA<<SAM<<<<<<<<<<<<<<<<<<<<<<<<<<<<")), "{found:?}");
    }

    #[test]
    fn a_secret_said_across_two_pieces() {
        let pieces = ["yeah so my PIN is", "4821 don't tell anyone", "the code review is at 3 pm"];
        let found = crate::find_in_pieces(&pieces);
        let shown: Vec<Vec<&str>> = found.iter().zip(pieces).map(|(secrets, piece)| secrets.iter().map(|secret| &piece[secret.range.clone()]).collect()).collect();
        assert_eq!(shown, [vec![], vec!["4821"], vec![]]);
        // A card number cut in two: each piece its part.
        let pieces = ["my card number is 4111 1111", "1111 1111 and it expires in march"];
        let found = crate::find_in_pieces(&pieces);
        assert_eq!((&pieces[0][found[0][0].range.clone()], &pieces[1][found[1][0].range.clone()]), ("4111 1111", "1111 1111"));
        assert!(crate::find_in_pieces(&["the meeting code", "is on the invite"]).iter().all(Vec::is_empty));
    }

    #[test]
    fn a_private_key_cut_off() {
        let head = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA1xU3\nqk2k9";
        assert_eq!(kinds(head), [(Kind::PrivateKey, head)]);
    }
}
