//! The popup's window, as Windows 11 frames it: no taskbar button or Alt+Tab entry, rounded corners, a dark or light
//! frame, the acrylic backdrop of Start and Search, and taking the foreground when summoned.

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR, DWMWA_CLOAK, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

fn hwnd(handle: isize) -> HWND {
    HWND(handle as *mut _)
}

/// No taskbar button and no Alt+Tab entry: a popup, like Start or Search.
pub fn make_tool_window(handle: isize) {
    // SAFETY: style bits of a window this process owns.
    unsafe {
        let style = GetWindowLongPtrW(hwnd(handle), GWL_EXSTYLE);
        let style = (style | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
        SetWindowLongPtrW(hwnd(handle), GWL_EXSTYLE, style);
    }
}

fn set_attribute<T>(handle: isize, attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE, value: T) -> bool {
    // SAFETY: a value of the size DWM expects for the attribute, passed by reference.
    unsafe {
        DwmSetWindowAttribute(hwnd(handle), attribute, (&value as *const T).cast(), std::mem::size_of::<T>() as u32).is_ok()
    }
}

/// Keeps the window off the screen while Windows goes on treating it as showing. Unlike a hidden window, what's drawn into it
/// stays: so it can be drawn before anyone sees it.
pub fn cloak(handle: isize, cloaked: bool) -> bool {
    set_attribute(handle, DWMWA_CLOAK, windows::core::BOOL::from(cloaked))
}

/// The window in front now (0 if none).
pub fn foreground() -> isize {
    // SAFETY: no arguments.
    unsafe { GetForegroundWindow().0 as isize }
}

/// Gives the foreground back from `from` (this process's window, about to be cloaked rather than hidden) to `to`, the
/// window that had it before. Hiding a window makes Windows activate the next one; cloaking doesn't, so it's done here.
/// True when `from` no longer has it (it hadn't, or `to` took it); false when `to` is gone or refused it.
pub fn give_back(from: isize, to: isize) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};
    // SAFETY: plain queries; SetForegroundWindow is allowed while this process has the foreground.
    unsafe {
        if GetForegroundWindow() != hwnd(from) {
            return true;
        }
        if to == 0 || to == from || !IsWindow(Some(hwnd(to))).as_bool() || !IsWindowVisible(hwnd(to)).as_bool() {
            return false;
        }
        let _ = SetForegroundWindow(hwnd(to));
        GetForegroundWindow() != hwnd(from)
    }
}

/// Rounded corners, the frame's theme, a hairline border, and optionally the acrylic backdrop behind transparent
/// content (Windows 11 22H2+; returns whether the backdrop was accepted).
pub fn style_popup(handle: isize, dark: bool, border: (u8, u8, u8), acrylic: bool) -> bool {
    set_attribute(handle, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND);
    set_attribute(handle, DWMWA_USE_IMMERSIVE_DARK_MODE, windows::core::BOOL::from(dark));
    let (r, g, b) = border;
    set_attribute(handle, DWMWA_BORDER_COLOR, COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)));
    if !acrylic {
        set_attribute(handle, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_NONE);
        return false;
    }
    // The backdrop shows through what the app leaves transparent once the frame covers the whole client area.
    let margins = MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 };
    // SAFETY: a window this process owns.
    let extended = unsafe { DwmExtendFrameIntoClientArea(hwnd(handle), &margins) }.is_ok();
    extended && set_attribute(handle, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)
}

/// A normal window's title bar in the app's colors: Windows 11 draws the caption in `caption` with `text` on it, and
/// a dark or light frame (earlier Windows ignores the colors and keeps the dark/light frame).
pub fn style_titlebar(handle: isize, dark: bool, caption: (u8, u8, u8), text: (u8, u8, u8)) {
    use windows::Win32::Graphics::Dwm::{DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR};
    let color = |(r, g, b): (u8, u8, u8)| COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16));
    set_attribute(handle, DWMWA_USE_IMMERSIVE_DARK_MODE, windows::core::BOOL::from(dark));
    set_attribute(handle, DWMWA_CAPTION_COLOR, color(caption));
    set_attribute(handle, DWMWA_TEXT_COLOR, color(text));
}

/// Brings the window to the front with the keyboard focus. Windows allows that to the process that received the
/// last input (a registered hotkey, a tray click); a hotkey caught by a keyboard hook doesn't count, so then this
/// joins the foreground window's input queue for a moment (AttachThreadInput), which lifts the lock. Returns whether
/// the window ended up in front.
pub fn activate(handle: isize) -> bool {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let window = hwnd(handle);
    // SAFETY: a window this process owns; the input queues are joined and separated again here.
    unsafe {
        let _ = BringWindowToTop(window);
        if SetForegroundWindow(window).as_bool() && GetForegroundWindow() == window {
            let _ = SetFocus(Some(window));
            return true;
        }
        let foreground = GetForegroundWindow();
        let theirs = GetWindowThreadProcessId(foreground, None);
        let ours = GetCurrentThreadId();
        let joined = theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool();
        let _ = BringWindowToTop(window);
        let _ = SetForegroundWindow(window);
        let _ = SetFocus(Some(window));
        if joined {
            let _ = AttachThreadInput(ours, theirs, false);
        }
        GetForegroundWindow() == window
    }
}

/// This process's top-level window of this class (0 if none). Another copy of Excavate may have one too: a sandboxed one
/// beside the real one.
pub fn find(class: &str) -> isize {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId};
    let class = crate::wide(class);
    let mut after = None;
    // SAFETY: a lookup by class name, going through the windows that have it.
    unsafe {
        let me = GetCurrentProcessId();
        while let Ok(window) = FindWindowExW(None, after, windows::core::PCWSTR(class.as_ptr()), windows::core::PCWSTR::null()) {
            let mut owner = 0;
            GetWindowThreadProcessId(window, Some(&mut owner));
            if owner == me {
                return window.0 as isize;
            }
            after = Some(window);
        }
    }
    0
}

/// Whether any process has a top-level window whose class starts with `prefix` (Everything's client:
/// "EVERYTHING_TASKBAR_NOTIFICATION", with "_(1.5a)" after it for 1.5's alpha). Its service has none.
pub fn any_of_class(prefix: &str) -> bool {
    use windows::Win32::Foundation::{LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW};
    use windows::core::BOOL;
    struct Search {
        prefix: String,
        found: bool,
    }
    unsafe extern "system" fn each(window: HWND, search: LPARAM) -> BOOL {
        // SAFETY: `search` is the `Search` below, alive for the enumeration; the class name fits the buffer.
        unsafe {
            let search = &mut *(search.0 as *mut Search);
            let mut name = [0u16; 128];
            let length = GetClassNameW(window, &mut name);
            if length > 0 && String::from_utf16_lossy(&name[..length as usize]).starts_with(&search.prefix) {
                search.found = true;
                return BOOL(0);
            }
        }
        TRUE
    }
    let mut search = Search { prefix: prefix.to_string(), found: false };
    // SAFETY: the callback only reads class names into its own buffer and writes to `search`.
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

pub fn is_foreground(handle: isize) -> bool {
    // SAFETY: a plain query.
    unsafe { GetForegroundWindow() == hwnd(handle) }
}

pub fn is_minimized(handle: isize) -> bool {
    // SAFETY: a plain query.
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(hwnd(handle)).as_bool() }
}

/// The whole window at this opacity. It stays a layered window afterwards, at full opacity: taking the style off clears what
/// the window shows, and the software renderer only draws what changed, so what hadn't changed would need drawing again.
pub fn fade(handle: isize, opacity: u8) {
    use windows::Win32::UI::WindowsAndMessaging::{LWA_ALPHA, SetLayeredWindowAttributes, WS_EX_LAYERED};
    // SAFETY: style bits and the opacity of a window this process owns.
    unsafe {
        let style = GetWindowLongPtrW(hwnd(handle), GWL_EXSTYLE);
        if style & WS_EX_LAYERED.0 as isize == 0 {
            SetWindowLongPtrW(hwnd(handle), GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
        }
        let _ = SetLayeredWindowAttributes(hwnd(handle), COLORREF(0), opacity, LWA_ALPHA);
    }
}

/// Where the window's content begins on screen (below its title bar), in physical pixels.
pub fn client_origin(handle: isize) -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    let mut point = POINT::default();
    // SAFETY: a plain query, filling our own point.
    unsafe { ClientToScreen(hwnd(handle), &mut point) }.as_bool().then_some((point.x, point.y))
}
