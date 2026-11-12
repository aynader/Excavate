//! Plain text and code in any of the encodings Windows files come in, and Jupyter notebooks.

use std::io::Read;
use std::path::Path;

/// A text file's contents (its first `limit` bytes): UTF-8 or UTF-16 by their marks, UTF-8 when it's valid, else the
/// PC's own code page (Windows-1252 in the West), as Notepad reads it.
pub fn read(path: &Path, limit: u64) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    let cut = bytes.len() as u64 == limit;
    decode_read(&bytes, cut).ok_or_else(|| "binary, not text".to_string())
}

/// A whole file's bytes as text.
pub fn decode(bytes: &[u8]) -> Option<String> {
    decode_read(bytes, false)
}

/// `cut`: the bytes stop at a read limit, so a character may be cut at the end.
fn decode_read(bytes: &[u8], cut: bool) -> Option<String> {
    let has_mark = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    if !has_mark && !bytes.iter().take(8192).any(|&b| b == 0) {
        if let Err(error) = std::str::from_utf8(bytes) {
            // Invalid inside (or at the end of a whole file): not UTF-8.
            if error.error_len().is_some() || !cut {
                return Some(ansi(bytes));
            }
        }
    }
    winsys::files::decode_text(bytes)
}

/// Bytes in the PC's ANSI code page: the system locale's (Windows-1252 in the West), not the process's, which a
/// UTF-8 manifest turns into UTF-8.
fn ansi(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::{MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
    let code_page = system_code_page();
    // SAFETY: the output buffer is sized by the first call.
    unsafe {
        let length = MultiByteToWideChar(code_page, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if length <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; length as usize];
        MultiByteToWideChar(code_page, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide));
        String::from_utf16_lossy(&wide)
    }
}

fn system_code_page() -> u32 {
    use std::sync::OnceLock;
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IDEFAULTANSICODEPAGE, LOCALE_RETURN_NUMBER};
    static CODE_PAGE: OnceLock<u32> = OnceLock::new();
    *CODE_PAGE.get_or_init(|| {
        let mut value = [0u16; 2];
        // SAFETY: LOCALE_RETURN_NUMBER writes a DWORD into the two-unit buffer.
        let written = unsafe {
            GetLocaleInfoEx(windows::core::w!("!x-sys-default-locale"), LOCALE_IDEFAULTANSICODEPAGE | LOCALE_RETURN_NUMBER, Some(&mut value))
        };
        let code_page = u32::from(value[0]) | (u32::from(value[1]) << 16);
        if written > 0 && code_page != 0 && code_page != 65001 { code_page } else { 1252 }
    })
}

/// A notebook's cells: code and Markdown, in order (outputs left out: they're mostly numbers and images).
pub fn notebook(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let mut text = String::new();
    for cell in json.get("cells").and_then(|cells| cells.as_array()).into_iter().flatten() {
        match cell.get("source") {
            Some(serde_json::Value::String(source)) => text.push_str(source),
            Some(serde_json::Value::Array(lines)) => lines.iter().filter_map(|line| line.as_str()).for_each(|line| text.push_str(line)),
            _ => {}
        }
        text.push_str("\n\n");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        assert_eq!(decode("naïve".as_bytes()).as_deref(), Some("naïve"));
        // "café" in Windows-1252.
        let western = decode(&[b'c', b'a', b'f', 0xE9]).unwrap();
        assert!(western.starts_with("caf") && western.chars().count() == 4, "{western}");
        assert_eq!(decode(&[0xFF, 0xFE, b'o', 0, b'k', 0]).as_deref(), Some("ok"));
        assert!(decode(&[0x4D, 0x5A, 0x90, 0x00, 0x03]).is_none(), "an executable");
    }
}
