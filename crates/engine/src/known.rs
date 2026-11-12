//! What Windows knows that a search can ask for by name: its known folders (`shell:desktop`) and each file type's
//! name (`type:text document`), as Everything has them.

use std::collections::HashMap;
use std::sync::Mutex;
use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    AssocQueryStringW, SHGetKnownFolderPath, ASSOCF_INIT_DEFAULTTOSTAR, ASSOCSTR_FRIENDLYDOCNAME, FOLDERID_AppDataProgramData,
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Favorites, FOLDERID_Fonts, FOLDERID_LocalAppData,
    FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86,
    FOLDERID_Programs, FOLDERID_Public, FOLDERID_Recent, FOLDERID_RoamingAppData, FOLDERID_SendTo, FOLDERID_StartMenu,
    FOLDERID_Startup, FOLDERID_System, FOLDERID_Templates, FOLDERID_Videos, FOLDERID_Windows, KF_FLAG_DEFAULT,
};

/// A known folder's path by the name `shell:` takes (Windows' own names for them, as in Run: `shell:downloads`).
pub fn folder(name: &str) -> Option<String> {
    let id: &GUID = match name.trim().to_ascii_lowercase().replace([' ', '_'], "").as_str() {
        "desktop" => &FOLDERID_Desktop,
        "documents" | "personal" | "mydocuments" => &FOLDERID_Documents,
        "downloads" => &FOLDERID_Downloads,
        "pictures" | "mypictures" => &FOLDERID_Pictures,
        "music" | "mymusic" => &FOLDERID_Music,
        "videos" | "myvideo" | "myvideos" => &FOLDERID_Videos,
        "favorites" => &FOLDERID_Favorites,
        "fonts" => &FOLDERID_Fonts,
        "appdata" | "roamingappdata" => &FOLDERID_RoamingAppData,
        "localappdata" => &FOLDERID_LocalAppData,
        "programdata" | "commonappdata" => &FOLDERID_ProgramData,
        "appdataprogramdata" => &FOLDERID_AppDataProgramData,
        "profile" | "userprofile" => &FOLDERID_Profile,
        "programfiles" => &FOLDERID_ProgramFiles,
        "programfilesx86" => &FOLDERID_ProgramFilesX86,
        "programs" => &FOLDERID_Programs,
        "public" => &FOLDERID_Public,
        "recent" => &FOLDERID_Recent,
        "sendto" => &FOLDERID_SendTo,
        "startmenu" => &FOLDERID_StartMenu,
        "startup" => &FOLDERID_Startup,
        "system" => &FOLDERID_System,
        "templates" => &FOLDERID_Templates,
        "windows" => &FOLDERID_Windows,
        _ => return None,
    };
    let mut path = std::ptr::null_mut();
    // SAFETY: Windows writes a string it allocated, freed below.
    unsafe {
        if SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, std::ptr::null_mut(), &mut path) != 0 || path.is_null() {
            return None;
        }
        let len = (0..).take_while(|&i| *path.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
        CoTaskMemFree(path as *const _);
        Some(text)
    }
}

/// Windows' name for files with this extension ("Text Document", "PNG File"), as Explorer's Type column shows it.
/// Looked up once per extension.
pub fn type_name(extension: &str) -> String {
    static NAMES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let key = extension.to_ascii_lowercase();
    if let Some(name) = NAMES.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return name.clone();
    }
    let wide: Vec<u16> = format!(".{key}").encode_utf16().chain(Some(0)).collect();
    let mut buffer = [0u16; 260];
    let mut len = buffer.len() as u32;
    // SAFETY: a buffer of `len` characters, filled by Windows.
    let found = unsafe {
        AssocQueryStringW(ASSOCF_INIT_DEFAULTTOSTAR as u32, ASSOCSTR_FRIENDLYDOCNAME, wide.as_ptr(), std::ptr::null(), buffer.as_mut_ptr(), &mut len)
    } == 0;
    let name = if found {
        String::from_utf16_lossy(&buffer[..(len as usize).saturating_sub(1)])
    } else {
        format!("{} File", key.to_ascii_uppercase())
    };
    NAMES.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, name.clone());
    name
}
