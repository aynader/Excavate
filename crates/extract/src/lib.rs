//! Reads the words out of files, without the apps that made them: text and code, Word/Excel/PowerPoint and
//! OpenDocument (ZIP + XML, parsed here), PDF (PDFium's text layer; pages without one are handed back for OCR), EPUB,
//! HTML, RTF, email (.eml, Outlook's .msg) and Jupyter notebooks.
//!
//! Runs in the indexer process, so a file that trips a reader can't take the popup down.

mod email;
mod html;
mod office;
pub mod pdf;
mod rtf;
mod text;

use std::path::Path;

/// Kinds, as the content index stores them.
pub use kinds::*;
mod kinds {
    pub const TEXT: &str = "text";
    pub const CODE: &str = "code";
    pub const PDF: &str = "pdf";
    pub const WORD: &str = "word";
    pub const SHEET: &str = "sheet";
    pub const SLIDES: &str = "slides";
    pub const EBOOK: &str = "ebook";
    pub const EMAIL: &str = "email";
    pub const WEB: &str = "web";
    pub const PICTURE: &str = "picture";
}

/// How a file is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader {
    Text,
    Code,
    Notebook,
    Docx,
    Xlsx,
    Pptx,
    OpenDocument,
    Epub,
    Html,
    Rtf,
    Eml,
    Msg,
    Pdf,
    /// Text in a picture: OCR, done by the indexer.
    Picture,
}

impl Reader {
    pub fn kind(self) -> &'static str {
        match self {
            Reader::Text | Reader::Rtf => TEXT,
            Reader::Code | Reader::Notebook => CODE,
            Reader::Docx => WORD,
            Reader::Xlsx => SHEET,
            Reader::Pptx => SLIDES,
            Reader::OpenDocument => WORD,
            Reader::Epub => EBOOK,
            Reader::Html => WEB,
            Reader::Eml | Reader::Msg => EMAIL,
            Reader::Pdf => PDF,
            Reader::Picture => PICTURE,
        }
    }
}

pub const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "tsv", "ini", "cfg", "conf", "srt", "vtt", "nfo", "rst", "adoc", "tex", "org",
    "text", "readme", "bib", "env",
];
pub const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cc", "cpp", "cxx", "hpp", "cs", "java", "kt", "kts",
    "go", "rb", "php", "swift", "m", "mm", "scala", "lua", "pl", "r", "jl", "dart", "sql", "sh", "bash", "zsh", "ps1",
    "psm1", "bat", "cmd", "vue", "svelte", "css", "scss", "less", "json", "jsonc", "yaml", "yml", "toml", "xml",
    "gradle", "cmake", "zig", "hs", "ex", "exs", "erl", "clj", "elm", "ml", "fs", "slint", "proto", "graphql", "tf",
    "nix", "glsl", "hlsl", "wgsl", "cu", "asm", "vb", "sln", "csproj", "vcxproj", "props", "dockerfile", "makefile",
];
/// Pictures OCR reads (decoded by the `image` crate, or by Windows for HEIC).
pub const PICTURE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif"];

/// Every extension some reader handles, for asking Everything what to read.
pub fn extensions() -> Vec<&'static str> {
    let mut all: Vec<&str> = Vec::new();
    all.extend(TEXT_EXTENSIONS);
    all.extend(CODE_EXTENSIONS);
    all.extend(["ipynb", "docx", "docm", "dotx", "xlsx", "xlsm", "pptx", "pptm", "odt", "ods", "odp", "epub", "html",
                "htm", "xhtml", "mht", "rtf", "eml", "msg", "pdf"]);
    all.extend(PICTURE_EXTENSIONS);
    all
}

/// Which reader a file needs, by its name; None for what isn't read.
pub fn reader_for(name: &str) -> Option<Reader> {
    let lower = name.to_ascii_lowercase();
    if matches!(lower.as_str(), "readme" | "license" | "makefile" | "dockerfile" | "changelog" | "authors") {
        return Some(Reader::Text);
    }
    let extension = lower.rsplit_once('.').map(|(_, extension)| extension)?;
    Some(match extension {
        "ipynb" => Reader::Notebook,
        "docx" | "docm" | "dotx" => Reader::Docx,
        "xlsx" | "xlsm" => Reader::Xlsx,
        "pptx" | "pptm" => Reader::Pptx,
        "odt" | "ods" | "odp" => Reader::OpenDocument,
        "epub" => Reader::Epub,
        "html" | "htm" | "xhtml" | "mht" => Reader::Html,
        "rtf" => Reader::Rtf,
        "eml" => Reader::Eml,
        "msg" => Reader::Msg,
        "pdf" => Reader::Pdf,
        e if TEXT_EXTENSIONS.contains(&e) => Reader::Text,
        e if CODE_EXTENSIONS.contains(&e) => Reader::Code,
        e if PICTURE_EXTENSIONS.contains(&e) => Reader::Picture,
        _ => return None,
    })
}

/// What a file says.
#[derive(Clone, Debug, Default)]
pub struct Extracted {
    pub text: String,
    /// PDF pages (0-based) with no text layer: scans, for OCR.
    pub scanned_pages: Vec<u16>,
    /// Pages in all (PDF), for weighing how much is scanned.
    pub pages: u16,
}

/// Larger text files are read only this far (logs, data dumps).
pub const TEXT_LIMIT: u64 = 8 << 20;
/// Larger documents aren't opened (a 200 MB PDF is a scan; its pages go to OCR one by one instead).
pub const DOCUMENT_LIMIT: u64 = 256 << 20;

/// Reads a file with the reader its name calls for. Pictures aren't read here (see the indexer's OCR).
pub fn extract(path: &Path, reader: Reader) -> Result<Extracted, String> {
    let size = std::fs::metadata(path).map_err(|error| error.to_string())?.len();
    if reader != Reader::Text && reader != Reader::Code && size > DOCUMENT_LIMIT {
        return Err(format!("too large ({} MB)", size >> 20));
    }
    let text = match reader {
        Reader::Text | Reader::Code => text::read(path, TEXT_LIMIT)?,
        Reader::Notebook => text::notebook(path)?,
        Reader::Docx => office::docx(path)?,
        Reader::Xlsx => office::xlsx(path)?,
        Reader::Pptx => office::pptx(path)?,
        Reader::OpenDocument => office::open_document(path)?,
        Reader::Epub => office::epub(path)?,
        Reader::Html => html::to_text(&text::read(path, TEXT_LIMIT)?),
        Reader::Rtf => rtf::to_text(&std::fs::read(path).map_err(|error| error.to_string())?),
        Reader::Eml => email::eml(path)?,
        Reader::Msg => email::msg(path)?,
        Reader::Pdf => return pdf::text(path),
        Reader::Picture => return Err("pictures are read by OCR".into()),
    };
    Ok(Extracted { text, ..Extracted::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_by_name() {
        assert_eq!(reader_for("Report.DOCX"), Some(Reader::Docx));
        assert_eq!(reader_for("main.rs"), Some(Reader::Code));
        assert_eq!(reader_for("notes.md"), Some(Reader::Text));
        assert_eq!(reader_for("Screenshot 2026-10-01.png"), Some(Reader::Picture));
        assert_eq!(reader_for("README"), Some(Reader::Text));
        assert_eq!(reader_for("setup.exe"), None);
        assert_eq!(reader_for("archive"), None);
    }
}
