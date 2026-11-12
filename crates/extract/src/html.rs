//! The readable text of HTML: tags dropped, scripts and styles skipped, block ends as line breaks, entities decoded.
//! A small scanner rather than a browser engine: search needs the words, not the layout.

pub fn to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut index = 0;
    let mut skip_until: Option<&str> = None;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            let start = index;
            let Some(close) = html[index..].find('>').map(|at| index + at) else { break };
            let tag = &html[index + 1..close];
            let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '!').collect();
            let name = name.to_ascii_lowercase();
            let closing = tag.starts_with('/');
            index = close + 1;
            if let Some(until) = skip_until {
                if closing && name == until {
                    skip_until = None;
                }
                continue;
            }
            if name.starts_with("!--") || tag.starts_with("!--") {
                // A comment: skip to its own end, looked for after its "<!--" (its text can hold a '>'). One that
                // never ends runs to the end, as in a browser.
                index = match html.get(start + 4..).and_then(|rest| rest.find("-->")) {
                    Some(end) => start + 4 + end + 3,
                    None => bytes.len(),
                };
                continue;
            }
            match name.as_str() {
                "script" | "style" | "head" | "noscript" | "svg" | "template" if !closing && !tag.ends_with('/') => {
                    skip_until = Some(match name.as_str() {
                        "script" => "script",
                        "style" => "style",
                        "head" => "head",
                        "noscript" => "noscript",
                        "svg" => "svg",
                        _ => "template",
                    });
                }
                "br" | "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "section" | "article"
                | "blockquote" | "pre" | "table" | "ul" | "ol" | "title" | "header" | "footer" => {
                    if !out.ends_with('\n') && !out.is_empty() {
                        out.push('\n');
                    }
                }
                "td" | "th" => out.push('\t'),
                _ => {}
            }
            continue;
        }
        if skip_until.is_some() {
            index += 1;
            continue;
        }
        if bytes[index] == b'&' {
            if let Some(end) = html[index..].find(';').filter(|&end| end <= 10) {
                if let Some(c) = entity(&html[index + 1..index + end]) {
                    out.push(c);
                    index += end + 1;
                    continue;
                }
            }
        }
        let c = html[index..].chars().next().unwrap_or(' ');
        // Runs of whitespace as one space, as a browser shows them.
        if c.is_whitespace() {
            if !out.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        index += c.len_utf8();
    }
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "rsquo" | "lsquo" => '\'',
        "rdquo" | "ldquo" => '"',
        "copy" => '©',
        "eacute" => 'é',
        "egrave" => 'è',
        "agrave" => 'à',
        "uuml" => 'ü',
        "ouml" => 'ö',
        "auml" => 'ä',
        "szlig" => 'ß',
        "ccedil" => 'ç',
        "euro" => '€',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_text() {
        let html = "<html><head><title>x</title><style>p{color:red}</style></head><body><h1>Trip&nbsp;plan</h1>\
                    <p>Lisbon &amp; Porto</p><script>var a = 1;</script><!-- note --><ul><li>caf&#233;</li></ul></body></html>";
        let text = to_text(html);
        assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["Trip", "plan", "Lisbon", "&", "Porto", "café"]);
        assert!(!text.contains("color") && !text.contains("var a"));
    }

    #[test]
    fn text_between_comments_is_kept() {
        assert_eq!(to_text("<p>one<!-- a -->two<!-- b -->three</p>").trim(), "onetwothree");
        assert_eq!(to_text("<p>x<!-- a > b -->y</p>").trim(), "xy");
        assert_eq!(to_text("<p>x<!---->y<!-- open").trim(), "xy");
    }
}
