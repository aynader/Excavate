//! Email saved as files: .eml (MIME) and Outlook's .msg (a compound file of MAPI properties). Who, when, the subject,
//! the text, and the names of attachments.

use std::io::Read;
use std::path::Path;

pub fn eml(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let message = mail_parser::MessageParser::default().parse(&bytes).ok_or("not an email")?;
    let mut text = String::new();
    let mut line = |label: &str, value: &str| {
        if !value.is_empty() {
            text.push_str(&format!("{label}: {value}\n"));
        }
    };
    let addresses = |list: Option<mail_parser::AddressList>| -> String {
        list.map(|list| {
            list.mailboxes().map(|one| match (one.name(), one.address()) {
                (Some(name), Some(email)) => format!("{name} <{email}>"),
                (None, Some(email)) => email.to_string(),
                (Some(name), None) => name.to_string(),
                _ => String::new(),
            }).collect::<Vec<_>>().join(", ")
        }).unwrap_or_default()
    };
    line("From", &addresses(message.from()));
    line("To", &addresses(message.to()));
    line("Cc", &addresses(message.cc()));
    line("Subject", message.subject().unwrap_or(""));
    if let Some(date) = message.date() {
        line("Date", &date.to_rfc3339());
    }
    text.push('\n');
    let mut body = String::new();
    for part in message.text_bodies() {
        body.push_str(&part);
        body.push('\n');
    }
    if body.trim().is_empty() {
        if let Some(html) = message.body_html(0) {
            body = crate::html::to_text(&html);
        }
    }
    text.push_str(&body);
    let attachments: Vec<String> = message.attachments().filter_map(|part| part.attachment_name().map(str::to_string)).collect();
    if !attachments.is_empty() {
        text.push_str(&format!("\nAttachments: {}\n", attachments.join(", ")));
    }
    Ok(text)
}

/// Outlook's .msg: MAPI properties as streams named `__substg1.0_<tag><type>` (001F: UTF-16, 001E: ANSI).
pub fn msg(path: &Path) -> Result<String, String> {
    let mut file = cfb::open(path).map_err(|error| error.to_string())?;
    let mut property = |tag: &str| -> Option<String> {
        for (kind, wide) in [("001F", true), ("001E", false)] {
            let name = format!("/__substg1.0_{tag}{kind}");
            if let Ok(mut stream) = file.open_stream(&name) {
                let mut bytes = Vec::new();
                stream.by_ref().take(16 << 20).read_to_end(&mut bytes).ok()?;
                return Some(if wide {
                    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
                    String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()
                } else {
                    crate::text::decode(&bytes).unwrap_or_default().trim_end_matches('\0').to_string()
                });
            }
        }
        None
    };
    let mut text = String::new();
    for (label, tag) in [("From", "0C1A"), ("To", "0E04"), ("Cc", "0E03"), ("Subject", "0037")] {
        if let Some(value) = property(tag).filter(|value| !value.is_empty()) {
            text.push_str(&format!("{label}: {value}\n"));
        }
    }
    text.push('\n');
    match property("1000") {
        Some(body) if !body.trim().is_empty() => text.push_str(&body),
        _ => {
            if let Some(html) = property("1013") {
                text.push_str(&crate::html::to_text(&html));
            }
        }
    }
    if text.trim().is_empty() {
        return Err("no message in this file".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eml_fields_and_body() {
        let dir = std::env::temp_dir().join(format!("excavate-eml-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.eml");
        std::fs::write(&path, "From: Ana <ana@example.com>\r\nTo: me@example.com\r\nSubject: Lisbon trip\r\n\
                               Date: Tue, 1 Sep 2026 10:00:00 +0000\r\nContent-Type: text/plain\r\n\r\nFlights are booked.\r\n").unwrap();
        let text = eml(&path).unwrap();
        assert!(text.contains("From: Ana <ana@example.com>"), "{text}");
        assert!(text.contains("Subject: Lisbon trip"));
        assert!(text.contains("Flights are booked."));
        let _ = std::fs::remove_dir_all(dir);
    }
}
