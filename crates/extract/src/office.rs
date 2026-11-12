//! Office Open XML (Word, Excel, PowerPoint), OpenDocument and EPUB: ZIP archives of XML, read here directly. Nothing
//! of Office's is needed.

use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::Read;
use std::path::Path;

type Archive = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

fn open(path: &Path) -> Result<Archive, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|error| error.to_string())
}

/// A member of the archive as text (at most 64 MB, against ZIP bombs).
fn member(archive: &mut Archive, name: &str) -> Option<String> {
    let mut entry = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    entry.by_ref().take(64 << 20).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn names_of(archive: &Archive) -> Vec<String> {
    archive.file_names().filter_map(Result::ok).map(|name| name.into_owned()).collect()
}

/// Members whose names start with `prefix` and end with `suffix`, in natural order (slide2 before slide10).
fn members(archive: &Archive, prefix: &str, suffix: &str) -> Vec<String> {
    let mut names: Vec<String> =
        names_of(archive).into_iter().filter(|name| name.starts_with(prefix) && name.ends_with(suffix)).collect();
    names.sort_by_key(|name| natural_key(name));
    names
}

fn natural_key(name: &str) -> (String, u64) {
    // The last run of digits, as a number; the rest of the name, as text.
    let Some(end) = name.rfind(|c: char| c.is_ascii_digit()).map(|at| at + 1) else { return (name.to_string(), 0) };
    // (The digits are ASCII: the character before them may not be.)
    let start = name[..end].char_indices().rev().find(|(_, c)| !c.is_ascii_digit()).map_or(0, |(at, c)| at + c.len_utf8());
    (format!("{}{}", &name[..start], &name[end..]), name[start..end].parse().unwrap_or(0))
}

/// Which elements of an XML part carry the words.
struct Rules {
    /// Elements whose text counts (local names); empty: all text.
    text: &'static [&'static str],
    /// Elements that end a line when they end.
    lines: &'static [&'static str],
    /// Empty elements that stand for a space or a line break.
    spaces: &'static [&'static str],
}

const WORD: Rules = Rules { text: &["t"], lines: &["p", "tr"], spaces: &["tab", "br", "cr"] };
const DRAWING: Rules = Rules { text: &["t"], lines: &["p"], spaces: &["br"] };
const OPEN_DOCUMENT: Rules = Rules { text: &[], lines: &["p", "h", "list-item", "table-row", "frame"], spaces: &["tab", "s", "line-break"] };

/// The words of an XML part.
fn xml_text(xml: &str, rules: &Rules, out: &mut String) {
    let mut reader = Reader::from_str(xml);
    let mut depth_in_text = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if rules.text.contains(&element.local_name().as_ref()) {
                    depth_in_text += 1;
                }
            }
            Ok(Event::End(element)) => {
                let local_name = element.local_name();
                let name = local_name.as_ref();
                if rules.text.contains(&name) {
                    depth_in_text = depth_in_text.saturating_sub(1);
                }
                if rules.lines.contains(&name) && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Ok(Event::Empty(element)) => {
                if rules.spaces.contains(&element.local_name().as_ref()) {
                    out.push(' ');
                }
            }
            Ok(Event::Text(text)) if rules.text.is_empty() || depth_in_text > 0 => out.push_str(&text.html_content()),
            Ok(Event::CData(data)) if rules.text.is_empty() || depth_in_text > 0 => {
                out.push_str(&data);
            }
            Ok(Event::GeneralRef(reference)) if rules.text.is_empty() || depth_in_text > 0 => {
                push_reference(&reference, out);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

/// `&amp;`, `&#233;`, `&#xE9;`.
pub(crate) fn push_reference(reference: &quick_xml::events::BytesRef, out: &mut String) {
    if reference.is_char_ref() {
        if let Ok(Some(c)) = reference.resolve_char_ref() {
            out.push(c);
        }
        return;
    }
    out.push_str(match reference.html_content().as_ref() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        _ => "",
    });
}

/// Word: the body, then headers, footers, footnotes, endnotes and comments.
pub fn docx(path: &Path) -> Result<String, String> {
    let mut archive = open(path)?;
    let mut text = String::new();
    let body = member(&mut archive, "word/document.xml").ok_or("not a Word document")?;
    xml_text(&body, &WORD, &mut text);
    let mut extra: Vec<String> = Vec::new();
    for prefix in ["word/header", "word/footer"] {
        extra.extend(members(&archive, prefix, ".xml"));
    }
    extra.extend(["word/footnotes.xml", "word/endnotes.xml", "word/comments.xml"].map(String::from));
    for name in extra {
        if let Some(xml) = member(&mut archive, &name) {
            text.push('\n');
            xml_text(&xml, &WORD, &mut text);
        }
    }
    Ok(text)
}

/// PowerPoint: each slide's text, then its speaker notes.
pub fn pptx(path: &Path) -> Result<String, String> {
    let mut archive = open(path)?;
    let mut text = String::new();
    let slides = members(&archive, "ppt/slides/slide", ".xml");
    if slides.is_empty() {
        return Err("not a PowerPoint presentation".into());
    }
    for name in slides.iter().chain(&members(&archive, "ppt/notesSlides/notesSlide", ".xml")) {
        if let Some(xml) = member(&mut archive, name) {
            xml_text(&xml, &DRAWING, &mut text);
            text.push('\n');
        }
    }
    Ok(text)
}

/// Excel: sheet names, then each sheet's cells row by row (shared strings looked up, numbers and dates as stored).
pub fn xlsx(path: &Path) -> Result<String, String> {
    let mut archive = open(path)?;
    let mut shared = Vec::new();
    if let Some(xml) = member(&mut archive, "xl/sharedStrings.xml") {
        shared = shared_strings(&xml);
    }
    let mut text = String::new();
    if let Some(xml) = member(&mut archive, "xl/workbook.xml") {
        let mut reader = Reader::from_str(&xml);
        while let Ok(event) = reader.read_event() {
            match event {
                Event::Empty(element) | Event::Start(element) if element.local_name().as_ref() == "sheet" => {
                    if let Some(name) = attribute(&element, "name") {
                        text.push_str(&name);
                        text.push('\n');
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
    }
    let sheets = members(&archive, "xl/worksheets/sheet", ".xml");
    if sheets.is_empty() {
        return Err("not an Excel workbook".into());
    }
    for name in sheets {
        if let Some(xml) = member(&mut archive, &name) {
            sheet(&xml, &shared, &mut text);
        }
    }
    Ok(text)
}

fn shared_strings(xml: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut reader = Reader::from_str(xml);
    let (mut current, mut in_text) = (String::new(), false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) if element.local_name().as_ref() == "t" => in_text = true,
            Ok(Event::End(element)) => match element.local_name().as_ref() {
                "t" => in_text = false,
                "si" => strings.push(std::mem::take(&mut current)),
                _ => {}
            },
            Ok(Event::Text(text)) if in_text => current.push_str(&text.html_content()),
            Ok(Event::GeneralRef(reference)) if in_text => push_reference(&reference, &mut current),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    strings
}

fn sheet(xml: &str, shared: &[String], out: &mut String) {
    let mut reader = Reader::from_str(xml);
    let (mut kind, mut value, mut in_value) = (String::new(), String::new(), false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => match element.local_name().as_ref() {
                "c" => {
                    kind = attribute(&element, "t").unwrap_or_default();
                    value.clear();
                }
                "v" | "t" => in_value = true,
                _ => {}
            },
            Ok(Event::End(element)) => match element.local_name().as_ref() {
                "v" | "t" => in_value = false,
                "c" => {
                    let cell = if kind == "s" {
                        value.trim().parse::<usize>().ok().and_then(|index| shared.get(index)).cloned().unwrap_or_default()
                    } else {
                        std::mem::take(&mut value)
                    };
                    if !cell.is_empty() {
                        out.push_str(&cell);
                        out.push('\t');
                    }
                }
                "row" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Text(text)) if in_value => value.push_str(&text.html_content()),
            Ok(Event::GeneralRef(reference)) if in_value => push_reference(&reference, &mut value),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
}

fn attribute(element: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
    element.attributes().flatten().find(|attribute| attribute.key.local_name().as_ref() == name).map(|attribute| {
        attribute.value.to_string()
    })
}

/// OpenDocument (LibreOffice's .odt, .ods, .odp): one content.xml.
pub fn open_document(path: &Path) -> Result<String, String> {
    let mut archive = open(path)?;
    let xml = member(&mut archive, "content.xml").ok_or("not an OpenDocument file")?;
    let mut text = String::new();
    xml_text(&xml, &OPEN_DOCUMENT, &mut text);
    Ok(text)
}

/// EPUB: the book's XHTML chapters, in the order of their names (close to reading order).
pub fn epub(path: &Path) -> Result<String, String> {
    let mut archive = open(path)?;
    let mut chapters: Vec<String> = names_of(&archive)
        .into_iter()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
        })
        .collect();
    if chapters.is_empty() {
        return Err("no chapters".into());
    }
    chapters.sort_by_key(|name| natural_key(name));
    let mut text = String::new();
    for name in chapters {
        if let Some(html) = member(&mut archive, &name) {
            text.push_str(&crate::html::to_text(&html));
            text.push_str("\n\n");
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_xml() {
        let xml = r#"<w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Kitchen</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve">renovation &amp; plan</w:t></w:r></w:p><w:p><w:r><w:instrText>PAGE</w:instrText><w:t>Budget</w:t></w:r></w:p></w:body></w:document>"#;
        let mut text = String::new();
        xml_text(xml, &WORD, &mut text);
        assert_eq!(text, "Kitchen renovation & plan\nBudget\n");
    }

    #[test]
    fn excel_cells() {
        let shared = shared_strings(r#"<sst><si><t>Rent</t></si><si><r><t>Lis</t></r><r><t>bon</t></r></si></sst>"#);
        assert_eq!(shared, ["Rent", "Lisbon"]);
        let mut text = String::new();
        sheet(r#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c><v>1250.5</v></c></row><row><c t="s"><v>1</v></c><c t="inlineStr"><is><t>trip</t></is></c></row></sheetData></worksheet>"#, &shared, &mut text);
        assert_eq!(text, "Rent\t1250.5\t\nLisbon\ttrip\t\n");
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["ppt/slides/slide10.xml", "ppt/slides/slide2.xml", "ppt/slides/slide1.xml"];
        names.sort_by_key(|name| natural_key(name));
        assert_eq!(names, ["ppt/slides/slide1.xml", "ppt/slides/slide2.xml", "ppt/slides/slide10.xml"]);
    }
}
