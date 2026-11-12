//! Places and files: the known folders (Start menu, Recent, Downloads…), what a shortcut points at, and the start of a
//! text file for the preview.

use crate::wide;
use std::io::Read;
use std::path::PathBuf;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile, STGM_READ};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
    FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Programs, FOLDERID_Recent, FOLDERID_Videos, IShellLinkW,
    KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{GUID, Interface, PCWSTR};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Known {
    Profile,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    /// Recent Items: shortcuts to what was opened lately, in any app.
    Recent,
    /// The current user's Start menu programs.
    Programs,
    /// Everyone's Start menu programs.
    CommonPrograms,
}

impl Known {
    fn id(self) -> &'static GUID {
        match self {
            Known::Profile => &FOLDERID_Profile,
            Known::Desktop => &FOLDERID_Desktop,
            Known::Documents => &FOLDERID_Documents,
            Known::Downloads => &FOLDERID_Downloads,
            Known::Pictures => &FOLDERID_Pictures,
            Known::Music => &FOLDERID_Music,
            Known::Videos => &FOLDERID_Videos,
            Known::Recent => &FOLDERID_Recent,
            Known::Programs => &FOLDERID_Programs,
            Known::CommonPrograms => &FOLDERID_CommonPrograms,
        }
    }
}

/// Where a known folder is (wherever the person moved it).
pub fn known(folder: Known) -> Option<PathBuf> {
    // SAFETY: the shell allocates the path; it's copied and freed here.
    unsafe {
        let path = SHGetKnownFolderPath(folder.id(), KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const _));
        text.map(PathBuf::from)
    }
}

/// Free bytes on the drive of a folder.
pub fn free_space(folder: &std::path::Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let path = wide(&folder.display().to_string());
    let mut free = 0u64;
    // SAFETY: the path outlives the call, which fills our own number.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

/// What a .lnk points at (empty for shortcuts to things that aren't files, like Control Panel items). COM must be ready.
pub fn shortcut_target(path: &str) -> Option<String> {
    let file = wide(path);
    // SAFETY: COM objects used and released here; the buffer is sized for GetPath.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist.Load(PCWSTR(file.as_ptr()), STGM_READ).ok()?;
        let mut buffer = [0u16; 1024];
        link.GetPath(&mut buffer, std::ptr::null_mut(), 0).ok()?;
        let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let target = String::from_utf16_lossy(&buffer[..length]);
        (!target.is_empty()).then_some(target)
    }
}

/// The start of a text file, decoded (UTF-8, or UTF-16 with its byte-order mark), for a preview. None for what looks
/// binary (a NUL in the first bytes of a file without a UTF-16 mark).
pub fn text_head(path: &str, max_bytes: usize) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(max_bytes.min(1 << 20));
    file.by_ref().take(max_bytes as u64).read_to_end(&mut bytes).ok()?;
    decode_text(&bytes)
}

pub fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None;
    }
    // A multi-byte character cut at the end of the read is dropped, not shown as garbage.
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(error) if error.error_len().is_none() => String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    };
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_folders() {
        crate::com_sta();
        for folder in [Known::Profile, Known::Desktop, Known::Downloads, Known::Recent, Known::Programs, Known::CommonPrograms] {
            let path = known(folder).unwrap_or_else(|| panic!("{folder:?}"));
            assert!(path.is_absolute(), "{folder:?}: {}", path.display());
        }
    }

    #[test]
    fn shortcuts_resolve() {
        crate::com_sta();
        let start = known(Known::CommonPrograms).unwrap();
        let link = std::fs::read_dir(&start).unwrap().flatten().map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")));
        if let Some(link) = link {
            // Some shortcuts point at shell items rather than files; most point at an executable.
            let _ = shortcut_target(&link.display().to_string());
        }
        assert!(shortcut_target(r"C:\no\such.lnk").is_none());
    }

    #[test]
    fn text_decoding() {
        assert_eq!(decode_text(b"hello").as_deref(), Some("hello"));
        assert_eq!(decode_text(&[0xEF, 0xBB, 0xBF, b'h', b'i']).as_deref(), Some("hi"));
        assert_eq!(decode_text(&[0xFF, 0xFE, b'h', 0, b'i', 0]).as_deref(), Some("hi"));
        assert_eq!(decode_text(&[b'a', 0, 1, 2]), None);
        assert_eq!(decode_text("é".as_bytes()[..1].as_ref()).as_deref(), Some(""));
    }
}
