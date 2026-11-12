//! Native popup menus (the tray's, a result's right-click menu, the sort menu): Windows draws them, so they look and
//! behave like every other menu on the system, in light and dark.

use crate::wide;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, HMENU, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW,
    SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TRACK_POPUP_MENU_FLAGS, TrackPopupMenuEx, WM_NULL,
};
use windows::core::PCWSTR;

#[derive(Clone, Debug)]
pub enum Item {
    Item { id: u16, label: String, checked: bool, enabled: bool },
    Submenu { label: String, items: Vec<Item> },
    Separator,
}

impl Item {
    pub fn new(id: u16, label: impl Into<String>) -> Item {
        Item::Item { id, label: label.into(), checked: false, enabled: true }
    }

    pub fn checked(id: u16, label: impl Into<String>, checked: bool) -> Item {
        Item::Item { id, label: label.into(), checked, enabled: true }
    }
}

/// The Windows error of the last menu that failed to show (0: none), for diagnostics.
pub static LAST_ERROR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A menu and its submenus (destroying the menu destroys them).
fn build(items: &[Item]) -> Option<HMENU> {
    // SAFETY: a menu made here, handed to the caller.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for item in items {
            match item {
                Item::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                Item::Submenu { label, items } => {
                    let label = wide(label);
                    if let Some(inner) = build(items) {
                        let _ = AppendMenuW(menu, MF_STRING | MF_POPUP, inner.0 as usize, PCWSTR(label.as_ptr()));
                    }
                }
                Item::Item { id, label, checked, enabled } => {
                    let label = wide(label);
                    let mut menu_flags = MF_STRING;
                    if *checked {
                        menu_flags |= MF_CHECKED;
                    }
                    if !*enabled {
                        menu_flags |= MF_GRAYED;
                    }
                    let _ = AppendMenuW(menu, menu_flags, *id as usize, PCWSTR(label.as_ptr()));
                }
            }
        }
        Some(menu)
    }
}

/// Shows the menu at (x, y) in screen pixels, owned by `owner`, and returns the chosen id. `flags` adds alignment
/// (TPM_BOTTOMALIGN for the tray).
pub fn popup(owner: isize, items: &[Item], x: i32, y: i32, flags: u32) -> Option<u16> {
    let window = HWND(owner as *mut _);
    // SAFETY: a menu made, shown and destroyed here, owned by the caller's window on this thread.
    unsafe {
        let menu = build(items)?;
        // A menu must belong to the foreground window, or it won't close when clicking elsewhere.
        let _ = SetForegroundWindow(window);
        let chosen = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TRACK_POPUP_MENU_FLAGS(flags)).0, x, y, window, None);
        let error = windows::Win32::Foundation::GetLastError();
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if chosen.0 == 0 && error.0 != 0 {
            LAST_ERROR.store(error.0, std::sync::atomic::Ordering::Relaxed);
        }
        (chosen.0 != 0).then_some(chosen.0 as u16)
    }
}
