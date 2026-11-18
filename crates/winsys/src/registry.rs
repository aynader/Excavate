//! Small registry reads and writes (theme, taskbar alignment, start with Windows).

use crate::wide;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_BINARY, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RRF_RT_REG_MULTI_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug)]
pub enum Root {
    CurrentUser,
    LocalMachine,
}

impl Root {
    fn key(self) -> HKEY {
        match self {
            Root::CurrentUser => HKEY_CURRENT_USER,
            Root::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }
}

pub fn string(root: Root, key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut size = 0u32;
    // SAFETY: the first call asks for the size, the second fills a buffer of that size.
    unsafe {
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut size))
            .ok()
            .ok()?;
        let mut buffer = vec![0u16; (size as usize).div_ceil(2).max(1)];
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_SZ, None,
                     Some(buffer.as_mut_ptr().cast()), Some(&mut size))
            .ok()
            .ok()?;
        let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..length]))
    }
}

/// A list of strings (REG_MULTI_SZ).
pub fn strings(root: Root, key: &str, value: &str) -> Option<Vec<String>> {
    let (key, value) = (wide(key), wide(value));
    let mut size = 0u32;
    // SAFETY: as `string`.
    unsafe {
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_MULTI_SZ, None, None, Some(&mut size))
            .ok()
            .ok()?;
        let mut buffer = vec![0u16; (size as usize).div_ceil(2).max(1)];
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_MULTI_SZ, None,
                     Some(buffer.as_mut_ptr().cast()), Some(&mut size))
            .ok()
            .ok()?;
        Some(buffer.split(|&c| c == 0).filter(|part| !part.is_empty()).map(String::from_utf16_lossy).collect())
    }
}

/// The person's languages, as Windows' language settings list them, first first ("en-US", "ar-EG").
pub fn languages() -> Vec<String> {
    strings(Root::CurrentUser, r"Control Panel\International\User Profile", "Languages").unwrap_or_default()
}

/// A language's name as Windows says it in English ("ar-EG" → "Arabic"), or the tag when Windows doesn't know it.
pub fn language_name(tag: &str) -> String {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SENGLISHLANGUAGENAME};
    let wide_tag = wide(tag);
    let mut buffer = [0u16; 128];
    // SAFETY: a NUL-terminated tag and a buffer of its stated length.
    let length = unsafe { GetLocaleInfoEx(PCWSTR(wide_tag.as_ptr()), LOCALE_SENGLISHLANGUAGENAME, Some(&mut buffer)) };
    if length <= 1 {
        return tag.to_string();
    }
    String::from_utf16_lossy(&buffer[..length as usize - 1])
}

pub fn dword(root: Root, key: &str, value: &str) -> Option<u32> {
    let (key, value) = (wide(key), wide(value));
    let mut data = 0u32;
    let mut size = 4u32;
    // SAFETY: a DWORD read into a DWORD.
    unsafe {
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_DWORD, None,
                     Some((&mut data as *mut u32).cast()), Some(&mut size))
            .ok()
            .ok()?;
    }
    Some(data)
}

/// A binary value's bytes.
pub fn bytes(root: Root, key: &str, value: &str) -> Option<Vec<u8>> {
    let (key, value) = (wide(key), wide(value));
    let mut size = 0u32;
    // SAFETY: as `string`.
    unsafe {
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_BINARY, None, None, Some(&mut size))
            .ok()
            .ok()?;
        let mut buffer = vec![0u8; size as usize];
        RegGetValueW(root.key(), PCWSTR(key.as_ptr()), PCWSTR(value.as_ptr()), RRF_RT_REG_BINARY, None,
                     Some(buffer.as_mut_ptr().cast()), Some(&mut size))
            .ok()
            .ok()?;
        buffer.truncate(size as usize);
        Some(buffer)
    }
}

/// Sets (Some) or deletes (None) a string value under an existing key of the current user.
pub fn set_user_string(key: &str, value: &str, data: Option<&str>) -> windows::core::Result<()> {
    let (key, value) = (wide(key), wide(value));
    let mut handle = HKEY::default();
    // SAFETY: the key is opened, written and closed here; the data buffer outlives the call.
    unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), None, KEY_SET_VALUE, &mut handle).ok()?;
        let result = match data {
            Some(data) => {
                let data = wide(data);
                let bytes = std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
                RegSetValueExW(handle, PCWSTR(value.as_ptr()), None, REG_SZ, Some(bytes)).ok()
            }
            None => {
                let result = RegDeleteValueW(handle, PCWSTR(value.as_ptr()));
                if result.0 == 2 { Ok(()) } else { result.ok() } // ERROR_FILE_NOT_FOUND: already gone
            }
        };
        let _ = RegCloseKey(handle);
        result
    }
}

/// A value to write with `set_user_key`.
pub enum Value<'a> {
    Text(&'a str),
    Number(u32),
    Bytes(&'a [u8]),
}

/// Creates (or opens) a key of the current user and writes these values in it.
pub fn set_user_key(key: &str, values: &[(&str, Value)]) -> windows::core::Result<()> {
    let key = wide(key);
    let mut handle = HKEY::default();
    // SAFETY: the key is created, written and closed here; each data buffer outlives its call.
    unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut handle, None).ok()?;
        let mut result = Ok(());
        for (name, value) in values {
            let name = wide(name);
            let written = match value {
                Value::Text(text) => {
                    let data = wide(text);
                    let bytes = std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2);
                    RegSetValueExW(handle, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes))
                }
                Value::Number(number) => RegSetValueExW(handle, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&number.to_le_bytes())),
                Value::Bytes(bytes) => RegSetValueExW(handle, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(bytes)),
            };
            if written.is_err() {
                result = written.ok();
                break;
            }
        }
        let _ = RegCloseKey(handle);
        result
    }
}

/// Deletes a key of the current user with everything under it (already gone is fine).
pub fn delete_user_key(key: &str) -> windows::core::Result<()> {
    let key = wide(key);
    // SAFETY: the key name outlives the call.
    let result = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr())) };
    if result.0 == 2 { Ok(()) } else { result.ok() } // ERROR_FILE_NOT_FOUND
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Windows' own list of what may start at sign-in (Task Manager's Startup apps, Settings' Startup): a program turned off
/// there keeps its Run entry, and its record here starts with an odd byte (3 off; 2 or 6 on).
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Whether this executable starts with Windows: the current user's Run key names it, and it isn't turned off in Task
/// Manager or Settings.
pub fn starts_with_windows(name: &str) -> bool {
    string(Root::CurrentUser, RUN_KEY, name).is_some() && !turned_off_in_windows(name)
}

fn turned_off_in_windows(name: &str) -> bool {
    bytes(Root::CurrentUser, APPROVED_KEY, name).and_then(|record| record.first().copied()).is_some_and(|first| first & 1 == 1)
}

/// Starts this executable at sign-in with `arguments`, or stops doing so. On also turns it back on in Windows' own
/// list, where Task Manager may have turned it off.
pub fn set_start_with_windows(name: &str, arguments: &str, on: bool) -> windows::core::Result<()> {
    if !on {
        // (Windows' record may not exist at all.)
        let _ = set_user_string(APPROVED_KEY, name, None);
        return set_user_string(RUN_KEY, name, None);
    }
    set_user_string(RUN_KEY, name, Some(&run_value(arguments)?))?;
    if turned_off_in_windows(name) {
        set_user_key(APPROVED_KEY, &[(name, Value::Bytes(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]))])?;
    }
    Ok(())
}

/// Start with Windows follows the build: when it's on and names another program, it's pointed at this one. Off stays off.
/// Whether it changed.
pub fn follow_start_with_windows(name: &str, arguments: &str) -> windows::core::Result<bool> {
    let Some(value) = string(Root::CurrentUser, RUN_KEY, name) else { return Ok(false) };
    let wanted = run_value(arguments)?;
    if value.eq_ignore_ascii_case(&wanted) {
        return Ok(false);
    }
    set_user_string(RUN_KEY, name, Some(&wanted))?;
    Ok(true)
}

/// What the Run key says to start this executable.
fn run_value(arguments: &str) -> windows::core::Result<String> {
    let exe = std::env::current_exe().map_err(|error| windows::core::Error::new(windows::core::HRESULT(-1), error.to_string()))?;
    Ok(format!("\"{}\" {arguments}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads() {
        let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        assert!(string(Root::LocalMachine, key, "ProductName").is_some());
        assert!(dword(Root::LocalMachine, key, "CurrentMajorVersionNumber").is_some_and(|major| major >= 10));
        assert!(string(Root::LocalMachine, r"SOFTWARE\No\Such\Key", "x").is_none());
    }

    #[test]
    fn run_key_round_trip() {
        let name = "Excavate.Test.RunKey";
        set_start_with_windows(name, "--test", true).unwrap();
        assert!(starts_with_windows(name));
        set_start_with_windows(name, "", false).unwrap();
        assert!(!starts_with_windows(name));
        set_start_with_windows(name, "", false).unwrap(); // deleting twice is fine
    }

    #[test]
    fn turned_off_in_task_manager() {
        let name = "Excavate.Test.Approved";
        set_start_with_windows(name, "--test", true).unwrap();
        set_user_key(APPROVED_KEY, &[(name, Value::Bytes(&[3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]))]).unwrap();
        assert!(!starts_with_windows(name), "off in Task Manager");
        set_start_with_windows(name, "--test", true).unwrap();
        assert!(starts_with_windows(name), "on again from Excavate");
        set_start_with_windows(name, "", false).unwrap();
        assert!(bytes(Root::CurrentUser, APPROVED_KEY, name).is_none());
    }

    #[test]
    fn start_with_windows_follows_the_build() {
        let name = "Excavate.Test.Follow";
        // Off stays off.
        set_start_with_windows(name, "", false).unwrap();
        assert!(!follow_start_with_windows(name, "--hidden").unwrap());
        assert!(!starts_with_windows(name));
        // On, naming another build: it now names this one, once.
        set_user_string(RUN_KEY, name, Some(r#""C:\old\excavate.exe" --hidden"#)).unwrap();
        assert!(follow_start_with_windows(name, "--hidden").unwrap());
        assert_eq!(string(Root::CurrentUser, RUN_KEY, name), Some(run_value("--hidden").unwrap()));
        assert!(!follow_start_with_windows(name, "--hidden").unwrap());
        set_start_with_windows(name, "", false).unwrap();
    }
}
