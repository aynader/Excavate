//! Rich Text Format (WordPad, older Word): the text, without control words, font tables or embedded pictures.

pub fn to_text(rtf: &[u8]) -> String {
    let mut out = String::new();
    // Per group: whether its text is skipped (font tables, pictures, `\*` destinations).
    let mut skipped: Vec<bool> = vec![false];
    let mut unicode_skip = 1usize;
    let mut pending_skip = 0usize;
    let mut index = 0;
    while index < rtf.len() {
        let byte = rtf[index];
        let skipping = *skipped.last().unwrap_or(&false);
        match byte {
            b'{' => {
                skipped.push(skipping);
                index += 1;
            }
            b'}' => {
                skipped.pop();
                if skipped.is_empty() {
                    skipped.push(false);
                }
                index += 1;
            }
            b'\\' => {
                index += 1;
                let Some(&next) = rtf.get(index) else { break };
                if next == b'\'' {
                    // \'hh: a byte in the document's code page (Windows-1252 nearly always).
                    let hex = rtf.get(index + 1..index + 3).and_then(|hex| std::str::from_utf8(hex).ok());
                    if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                        if !skipping && pending_skip == 0 {
                            out.push(windows_1252(value));
                        }
                        pending_skip = pending_skip.saturating_sub(1);
                    }
                    index += 3;
                    continue;
                }
                if !next.is_ascii_alphabetic() {
                    // \\, \{, \}, \~ and friends.
                    if !skipping {
                        match next {
                            b'\\' | b'{' | b'}' => out.push(next as char),
                            b'~' => out.push(' '),
                            b'*' => {
                                if let Some(last) = skipped.last_mut() {
                                    *last = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    index += 1;
                    continue;
                }
                let start = index;
                while index < rtf.len() && rtf[index].is_ascii_alphabetic() {
                    index += 1;
                }
                let word = std::str::from_utf8(&rtf[start..index]).unwrap_or("");
                let number_start = index;
                if index < rtf.len() && (rtf[index] == b'-' || rtf[index].is_ascii_digit()) {
                    index += 1;
                    while index < rtf.len() && rtf[index].is_ascii_digit() {
                        index += 1;
                    }
                }
                let number: Option<i32> = std::str::from_utf8(&rtf[number_start..index]).ok().and_then(|n| n.parse().ok());
                if rtf.get(index) == Some(&b' ') {
                    index += 1;
                }
                match word {
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "object" | "header" | "footer" | "themedata"
                    | "datastore" | "xmlnstbl" | "listtable" | "listoverridetable" | "rsidtbl" | "generator" => {
                        if let Some(last) = skipped.last_mut() {
                            *last = true;
                        }
                    }
                    "par" | "line" | "row" | "sect" | "page" if !skipping => out.push('\n'),
                    "tab" | "cell" if !skipping => out.push('\t'),
                    "uc" => unicode_skip = number.unwrap_or(1).max(0) as usize,
                    "u" => {
                        if let Some(code) = number {
                            let code = if code < 0 { code + 65536 } else { code } as u32;
                            if !skipping {
                                if let Some(c) = char::from_u32(code) {
                                    out.push(c);
                                }
                            }
                            pending_skip = unicode_skip;
                        }
                    }
                    _ => {}
                }
            }
            b'\r' | b'\n' => index += 1,
            _ => {
                if !skipping {
                    if pending_skip > 0 {
                        pending_skip -= 1;
                    } else {
                        out.push(windows_1252(byte));
                    }
                }
                index += 1;
            }
        }
    }
    out
}

fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’',
        '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[(byte - 0x80) as usize],
        _ => byte as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text() {
        let rtf = br"{\rtf1\ansi{\fonttbl{\f0 Calibri;}}{\*\generator Riched20;}\f0 Caf\'e9 menu\par Price: \u8364?12\par}";
        assert_eq!(to_text(rtf), "Café menu\nPrice: €12\n");
    }
}
