//! Light or dark, as the person set Windows (Settings → Personalization → Colors), and the accent color.

use crate::registry::{Root, dword};

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Apps use the light theme ("Choose your app mode").
pub fn apps_light() -> bool {
    dword(Root::CurrentUser, PERSONALIZE, "AppsUseLightTheme") != Some(0)
}

/// The taskbar and Start use the light theme ("Choose your default Windows mode"): the tray icon follows this.
pub fn system_light() -> bool {
    dword(Root::CurrentUser, PERSONALIZE, "SystemUsesLightTheme") == Some(1)
}

/// The accent color as (r, g, b).
pub fn accent() -> Option<(u8, u8, u8)> {
    let abgr = dword(Root::CurrentUser, r"Software\Microsoft\Windows\DWM", "AccentColor")?;
    Some(((abgr & 0xff) as u8, ((abgr >> 8) & 0xff) as u8, ((abgr >> 16) & 0xff) as u8))
}

/// Whether Windows shows animations (Settings → Accessibility → Visual effects → Animation effects): false when the user
/// asked for fewer.
pub fn animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut enabled = windows::core::BOOL(1);
    // SAFETY: a BOOL written by the call.
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some((&mut enabled as *mut windows::core::BOOL).cast()),
                                      SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    enabled.as_bool()
}
