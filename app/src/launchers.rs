//! Who holds a shortcut. Windows only says that a shortcut is taken, not by whom, so an app is named only when a
//! launcher Excavate knows is running and its own settings show it uses those keys.

use winsys::tray::Hotkey;

const ALT: u32 = 1;
const CTRL: u32 = 2;
const SHIFT: u32 = 4;
const WIN: u32 = 8;

/// The known launcher that uses this shortcut now, if any.
pub fn holder(hotkey: Hotkey) -> Option<&'static str> {
    if winsys::process::running("EverythingToolbar").is_some() && everything_toolbar() == Some(hotkey) {
        return Some("EverythingToolbar");
    }
    if winsys::process::running("PowerToys.PowerLauncher").is_some() && powertoys_run() == Some(hotkey) {
        return Some("PowerToys Run");
    }
    if winsys::process::running("Flow.Launcher").is_some() && flow_launcher() == Some(hotkey) {
        return Some("Flow Launcher");
    }
    None
}

/// %APPDATA%\EverythingToolbar\settings.ini: `ShortcutModifiers` and `ShortcutKey` as WPF writes them; a value that
/// was never changed isn't there (its default is Win+Alt+S).
fn everything_toolbar() -> Option<Hotkey> {
    let path = std::path::Path::new(&std::env::var_os("APPDATA")?).join("EverythingToolbar").join("settings.ini");
    let text = std::fs::read_to_string(path).ok()?;
    let value = |name: &str| {
        text.lines().find_map(|line| line.trim().strip_prefix(name)?.strip_prefix('=')).and_then(|value| value.trim().parse::<u32>().ok())
    };
    let modifiers = value("ShortcutModifiers").unwrap_or(ALT | WIN);
    let key = wpf_key(value("ShortcutKey").unwrap_or(62))?;
    Some(Hotkey { modifiers: modifiers_from_wpf(modifiers), key })
}

/// System.Windows.Input.ModifierKeys.
fn modifiers_from_wpf(modifiers: u32) -> u32 {
    let mut out = 0;
    for (wpf, flag) in [(1, ALT), (2, CTRL), (4, SHIFT), (8, WIN)] {
        if modifiers & wpf != 0 {
            out |= flag;
        }
    }
    out
}

/// System.Windows.Input.Key to a virtual key.
fn wpf_key(key: u32) -> Option<u32> {
    match key {
        18 => Some(0x20),                                 // Space
        34..=43 => Some(0x30 + key - 34),                 // D0–D9
        44..=69 => Some(u32::from(b'A') + key - 44),      // A–Z
        90..=113 => Some(0x70 + key - 90),                // F1–F24
        _ => None,
    }
}

/// %LOCALAPPDATA%\Microsoft\PowerToys\PowerToys Run\settings.json: properties.open_powerlauncher.
fn powertoys_run() -> Option<Hotkey> {
    let path = std::path::Path::new(&std::env::var_os("LOCALAPPDATA")?).join(r"Microsoft\PowerToys\PowerToys Run\settings.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let keys = &json["properties"]["open_powerlauncher"];
    let on = |name: &str, flag: u32| if keys[name].as_bool().unwrap_or(false) { flag } else { 0 };
    Some(Hotkey {
        modifiers: on("alt", ALT) | on("ctrl", CTRL) | on("shift", SHIFT) | on("win", WIN),
        key: keys["code"].as_u64()? as u32,
    })
}

/// %APPDATA%\FlowLauncher\Settings\Settings.json: "Hotkey": "Alt + Space".
fn flow_launcher() -> Option<Hotkey> {
    let path = std::path::Path::new(&std::env::var_os("APPDATA")?).join(r"FlowLauncher\Settings\Settings.json");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    json["Hotkey"].as_str()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpf_values() {
        assert_eq!(wpf_key(62), Some(u32::from(b'S')));
        assert_eq!(wpf_key(18), Some(0x20));
        assert_eq!(wpf_key(90), Some(0x70));
        assert_eq!(wpf_key(1), None);
        assert_eq!(modifiers_from_wpf(9), ALT | WIN);
    }
}
