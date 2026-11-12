//! The tray icon, the global hotkey, and the door a second launch knocks on, all on one hidden window with a thread
//! of its own. That thread does nothing but wait for messages, so the hotkey answers even while the UI is busy.
//!
//! The window is a plain hidden top-level window rather than a message-only one: only top-level windows hear the
//! broadcasts that matter here ("TaskbarCreated" after Explorer restarts, WM_SETTINGCHANGE when the theme changes).

use crate::wide;
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW,
    FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, ICONINFO, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SendMessageW, TPM_BOTTOMALIGN,
    TranslateMessage, WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ENDSESSION, WM_HOTKEY, WM_LBUTTONUP, WM_MOUSEMOVE, WM_SETTINGCHANGE, WM_USER,
    WNDCLASSEXW, WS_OVERLAPPED,
};
use windows::core::PCWSTR;

const CLASS: &str = "Excavate.Shell";
const WM_TRAY: u32 = WM_APP + 1;
const WM_REOPEN: u32 = WM_APP + 2;
const WM_SET_HOTKEY: u32 = WM_APP + 3;
const WM_QUIT_SHELL: u32 = WM_APP + 4;
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const HOTKEY_ID: i32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The global hotkey.
    Hotkey,
    /// A left click (or Enter) on the tray icon.
    Click,
    /// A menu item was chosen.
    Menu(u16),
    /// Excavate was launched again (the Start menu, a pinned icon): it knocks here instead of starting twice.
    Reopen,
    /// Windows' light/dark setting changed.
    ThemeChanged,
    /// The pointer is over the tray icon: the moment to bring its tooltip up to date.
    Hover,
    /// Windows is shutting down or signing out: what must be saved is saved now, before this returns (the handler is
    /// called on the tray's own thread for this one, and Windows may end the process right after).
    EndSession,
}

pub use crate::menu::Item as MenuItem;

/// A key combination for RegisterHotKey: "Alt+S", "Ctrl+Shift+Space", "Win+F1".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

impl std::str::FromStr for Hotkey {
    type Err = String;

    fn from_str(text: &str) -> Result<Hotkey, String> {
        let mut modifiers = 0u32;
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|part| !part.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "alt" => modifiers |= MOD_ALT.0,
                "ctrl" | "control" => modifiers |= MOD_CONTROL.0,
                "shift" => modifiers |= MOD_SHIFT.0,
                "win" | "windows" | "meta" => modifiers |= MOD_WIN.0,
                other => {
                    if key.is_some() {
                        return Err(format!("\"{text}\" has two keys"));
                    }
                    key = Some(virtual_key(other).ok_or_else(|| format!("\"{part}\" isn't a key Excavate knows"))?);
                }
            }
        }
        let key = key.ok_or_else(|| format!("\"{text}\" has no key"))?;
        if modifiers == 0 && !(0x70..=0x87).contains(&key) {
            return Err(format!("\"{text}\" needs Alt, Ctrl, Shift or Win (or be an F key)"));
        }
        Ok(Hotkey { modifiers, key })
    }
}

impl std::fmt::Display for Hotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        for (flag, name) in [(MOD_CONTROL.0, "Ctrl"), (MOD_ALT.0, "Alt"), (MOD_SHIFT.0, "Shift"), (MOD_WIN.0, "Win")] {
            if self.modifiers & flag != 0 {
                parts.push(name.to_string());
            }
        }
        parts.push(key_name(self.key));
        write!(f, "{}", parts.join("+"))
    }
}

fn virtual_key(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "SPACE" => return Some(0x20),
        "ENTER" | "RETURN" => return Some(0x0D),
        "TAB" => return Some(0x09),
        "ESC" | "ESCAPE" => return Some(0x1B),
        "`" | "BACKQUOTE" => return Some(0xC0),
        "." | "PERIOD" => return Some(0xBE),
        "," | "COMMA" => return Some(0xBC),
        "/" | "SLASH" => return Some(0xBF),
        ";" | "SEMICOLON" => return Some(0xBA),
        _ => {}
    }
    let bytes = upper.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_alphanumeric() {
        return Some(u32::from(bytes[0]));
    }
    if let Some(number) = upper.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&number) {
            return Some(0x6F + number);
        }
    }
    None
}

fn key_name(key: u32) -> String {
    match key {
        0x20 => "Space".into(),
        0x0D => "Enter".into(),
        0x09 => "Tab".into(),
        0x1B => "Esc".into(),
        0xC0 => "`".into(),
        0xBE => ".".into(),
        0xBC => ",".into(),
        0xBF => "/".into(),
        0xBA => ";".into(),
        0x70..=0x87 => format!("F{}", key - 0x6F),
        _ => char::from_u32(key).map(String::from).unwrap_or_else(|| format!("#{key}")),
    }
}

/// What the shell thread needs to know.
pub struct Options {
    pub tooltip: String,
    /// Icon resources in the executable (see app/excavate.rc): drawn for a dark taskbar, and for a light one.
    pub icon_for_dark_taskbar: u16,
    pub icon_for_light_taskbar: u16,
    pub hotkey: Hotkey,
}

/// The icon drawn by the app instead of the executable's (in a theme's colors): square, straight RGBA, `size` pixels a
/// side (`icon_size`), for a light taskbar and for a dark one.
#[derive(Clone, Debug, PartialEq)]
pub struct Artwork {
    pub size: u32,
    pub for_light_taskbar: Vec<u8>,
    pub for_dark_taskbar: Vec<u8>,
}

struct State {
    handler: Box<dyn Fn(Event) + Send + Sync>,
    menu: Mutex<Vec<MenuItem>>,
    options: Options,
    tooltip: Mutex<String>,
    artwork: Mutex<Option<Artwork>>,
    taskbar_created: u32,
}

static STATE: OnceLock<Arc<State>> = OnceLock::new();

/// The running shell thread's window, to talk to it from any thread.
#[derive(Clone, Copy, Debug)]
pub struct Tray {
    window: isize,
}

impl Tray {
    /// Starts the shell thread: tray icon, hotkey, and the door for second launches. `handler` runs on that thread.
    /// The second result says how the hotkey is heard; failing to hear it isn't fatal (the tray icon still opens).
    pub fn start(options: Options, menu: Vec<MenuItem>, handler: impl Fn(Event) + Send + Sync + 'static)
                 -> Result<(Tray, Result<Hearing, String>), String> {
        let state = Arc::new(State {
            handler: Box::new(handler),
            menu: Mutex::new(menu),
            tooltip: Mutex::new(options.tooltip.clone()),
            artwork: Mutex::new(None),
            options,
            taskbar_created: unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) },
        });
        STATE.set(state.clone()).map_err(|_| "the shell thread runs already".to_string())?;
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("shell".into())
            .spawn(move || run(state, sender))
            .map_err(|error| error.to_string())?;
        let (window, hotkey) = receiver.recv().map_err(|_| "the shell thread didn't start".to_string())??;
        Ok((Tray { window }, hotkey))
    }

    /// Changes the hotkey (on the shell thread, which owns it and its hook), and says how it's heard now.
    pub fn set_hotkey(&self, hotkey: Hotkey) -> Result<Hearing, String> {
        let packed = (hotkey.modifiers as usize) << 16 | hotkey.key as usize;
        // SAFETY: a message to our own window, handled synchronously by its thread.
        let result = unsafe { SendMessageW(hwnd(self.window), WM_SET_HOTKEY, Some(WPARAM(packed)), Some(LPARAM(0))) };
        match result.0 {
            1 => Ok(Hearing::Registered),
            2 => Ok(Hearing::Hooked),
            _ => Err(format!("{hotkey} is taken by another app")),
        }
    }

    /// Stops listening for the hotkey (while a new one is being recorded), until `set_hotkey`.
    pub fn suspend_hotkey(&self) {
        // SAFETY: a message to our own window, handled synchronously by its thread.
        unsafe {
            SendMessageW(hwnd(self.window), WM_SET_HOTKEY, Some(WPARAM(0)), Some(LPARAM(0)));
        }
    }

    /// How the hotkey is heard now (None while suspended or when it couldn't be).
    pub fn hearing(&self) -> Option<Hearing> {
        match HEARING.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Some(Hearing::Registered),
            2 => Some(Hearing::Hooked),
            _ => None,
        }
    }

    pub fn set_menu(&self, items: Vec<MenuItem>) {
        if let Some(state) = STATE.get() {
            *state.menu.lock().unwrap() = items;
        }
    }

    /// What the icon says when the pointer rests on it.
    pub fn set_tooltip(&self, text: &str) {
        if let Some(state) = STATE.get() {
            let mut tooltip = state.tooltip.lock().unwrap();
            if *tooltip == text {
                return;
            }
            *tooltip = text.to_string();
            drop(tooltip);
            let data = icon_data(hwnd(self.window), state);
            // SAFETY: updating our own tray icon.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            }
        }
    }

    /// Draws the icon from `artwork`, or from the executable's icons with none.
    pub fn set_artwork(&self, artwork: Option<Artwork>) {
        if let Some(state) = STATE.get() {
            let mut slot = state.artwork.lock().unwrap();
            if *slot == artwork {
                return;
            }
            *slot = artwork;
            drop(slot);
            let data = icon_data(hwnd(self.window), state);
            // SAFETY: updating our own tray icon.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
            }
        }
    }

    /// Removes the tray icon and ends the thread.
    pub fn quit(&self) {
        // SAFETY: a message to our own window.
        unsafe {
            let _ = PostMessageW(Some(hwnd(self.window)), WM_QUIT_SHELL, WPARAM(0), LPARAM(0));
        }
    }
}

/// When Excavate is running already: asks it to open and returns true (this launch should then exit).
pub fn knock() -> bool {
    let class = wide(CLASS);
    // SAFETY: finding and messaging another process's window by its class.
    unsafe {
        match FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) {
            Ok(window) if !window.is_invalid() => PostMessageW(Some(window), WM_REOPEN, WPARAM(0), LPARAM(0)).is_ok(),
            _ => false,
        }
    }
}

fn hwnd(handle: isize) -> HWND {
    HWND(handle as *mut _)
}

type Started = Result<(isize, Result<Hearing, String>), String>;

fn run(state: Arc<State>, started: std::sync::mpsc::Sender<Started>) {
    // SAFETY: the window class, window, icon and message loop all belong to this thread.
    unsafe {
        let instance = match GetModuleHandleW(None) {
            Ok(instance) => instance,
            Err(error) => {
                let _ = started.send(Err(error.message().to_string()));
                return;
            }
        };
        let class = wide(CLASS);
        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&window_class);
        let title = wide("Excavate");
        let window = match CreateWindowExW(Default::default(), PCWSTR(class.as_ptr()), PCWSTR(title.as_ptr()), WS_OVERLAPPED,
                                           CW_USEDEFAULT, CW_USEDEFAULT, 0, 0, None, None, Some(instance.into()), None) {
            Ok(window) => window,
            Err(error) => {
                let _ = started.send(Err(error.message().to_string()));
                return;
            }
        };
        add_icon(window, &state);
        let hotkey = register_hotkey(window, state.options.hotkey);
        let _ = started.send(Ok((window.0 as isize, hotkey)));

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// How the hotkey is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hearing {
    /// Registered with Windows (RegisterHotKey): the usual way.
    Registered,
    /// Another app registered the same keys first, so a low-level keyboard hook catches them before it does.
    Hooked,
}

/// Registers the hotkey; when another app holds it, catches it with a low-level keyboard hook instead (as PowerToys'
/// Keyboard Manager does), since the person chose these keys for Excavate.
fn register_hotkey(window: HWND, hotkey: Hotkey) -> Result<Hearing, String> {
    unregister_hotkey(window);
    // SAFETY: a hotkey for our own window.
    unsafe {
        if RegisterHotKey(Some(window), HOTKEY_ID, HOT_KEY_MODIFIERS(hotkey.modifiers | MOD_NOREPEAT.0), hotkey.key).is_ok() {
            HEARING.store(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(Hearing::Registered);
        }
    }
    let hearing = hook::start(window, hotkey).map(|()| Hearing::Hooked).map_err(|error| format!("{hotkey} is taken by another app ({error})"));
    if hearing.is_ok() {
        HEARING.store(2, std::sync::atomic::Ordering::Relaxed);
    }
    hearing
}

fn unregister_hotkey(window: HWND) {
    hook::stop();
    HEARING.store(0, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: our own hotkey.
    unsafe {
        let _ = UnregisterHotKey(Some(window), HOTKEY_ID);
    }
}

/// 0 none, 1 registered, 2 hooked.
static HEARING: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The low-level keyboard hook, on the shell thread (whose message loop it needs). It only looks at the hotkey's key
/// and returns at once for everything else, so typing elsewhere never waits on it.
mod hook {
    use super::{HOTKEY_ID, Hotkey};
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_SHIFT,
        MOD_WIN, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, PostMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
        WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    static HOOK: AtomicIsize = AtomicIsize::new(0);
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    /// (modifiers << 32) | virtual key; 0 when off.
    static KEYS: AtomicU64 = AtomicU64::new(0);
    static DOWN: AtomicBool = AtomicBool::new(false);

    pub fn start(window: HWND, hotkey: Hotkey) -> Result<(), String> {
        WINDOW.store(window.0 as isize, Ordering::SeqCst);
        KEYS.store((u64::from(hotkey.modifiers) << 32) | u64::from(hotkey.key), Ordering::SeqCst);
        // SAFETY: a hook procedure of this module for the calling (message-looping) thread's process.
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), None, 0) }.map_err(|error| error.message().to_string())?;
        HOOK.store(hook.0 as isize, Ordering::SeqCst);
        Ok(())
    }

    pub fn stop() {
        KEYS.store(0, Ordering::SeqCst);
        let hook = HOOK.swap(0, Ordering::SeqCst);
        if hook != 0 {
            // SAFETY: a hook this module installed.
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
            }
        }
    }

    fn pressed(key: VIRTUAL_KEY) -> bool {
        // SAFETY: a plain query of the async key state.
        unsafe { GetAsyncKeyState(i32::from(key.0)) < 0 }
    }

    fn modifiers() -> u32 {
        let mut modifiers = 0;
        if pressed(VK_MENU) {
            modifiers |= MOD_ALT.0;
        }
        if pressed(VK_CONTROL) {
            modifiers |= MOD_CONTROL.0;
        }
        if pressed(VK_SHIFT) {
            modifiers |= MOD_SHIFT.0;
        }
        if pressed(VK_LWIN) || pressed(VK_RWIN) {
            modifiers |= MOD_WIN.0;
        }
        modifiers
    }

    /// A keystroke of an unassigned key (0xE8) between Alt's press and release, so the app under the popup doesn't
    /// take a lone Alt tap as "open the menu bar" (AutoHotkey's menu mask).
    fn mask() {
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0xE8), dwFlags: flags, ..Default::default() } },
        };
        // SAFETY: two well-formed keyboard inputs.
        unsafe {
            SendInput(&[key(Default::default()), key(KEYEVENTF_KEYUP)], std::mem::size_of::<INPUT>() as i32);
        }
    }

    unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let keys = KEYS.load(Ordering::Relaxed);
        if code >= 0 && keys != 0 {
            // SAFETY: for WH_KEYBOARD_LL, lparam points at a KBDLLHOOKSTRUCT for the duration of the call.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let (wanted_modifiers, wanted_key) = ((keys >> 32) as u32, keys as u32);
            if info.vkCode == wanted_key {
                match wparam.0 as u32 {
                    WM_KEYDOWN | WM_SYSKEYDOWN if modifiers() == wanted_modifiers => {
                        if !DOWN.swap(true, Ordering::SeqCst) {
                            if wanted_modifiers & MOD_ALT.0 != 0 {
                                mask();
                            }
                            let window = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
                            // SAFETY: a message to our own window.
                            unsafe {
                                let _ = PostMessageW(Some(window), WM_HOTKEY, WPARAM(HOTKEY_ID as usize), LPARAM(0));
                            }
                        }
                        return LRESULT(1); // swallowed: the other app doesn't get these keys
                    }
                    WM_KEYUP | WM_SYSKEYUP if DOWN.swap(false, Ordering::SeqCst) => return LRESULT(1),
                    _ => {}
                }
            }
        }
        // SAFETY: passing the event on to the next hook.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

/// How many pixels a side the tray icon is at this DPI.
pub fn icon_size() -> u32 {
    // SAFETY: a system metric.
    unsafe { GetSystemMetrics(SM_CXSMICON).max(16) as u32 }
}

/// The icon made or loaded last: the shell copies it, so it's destroyed when the next is made (one a tooltip or theme
/// change would otherwise leave behind each time).
static MADE: Mutex<isize> = Mutex::new(0);

fn keep(icon: HICON) -> HICON {
    if !icon.is_invalid() {
        let mut made = MADE.lock().unwrap();
        if *made != 0 && *made != icon.0 as isize {
            // SAFETY: an icon this module made or loaded (not shared) and nothing else holds.
            unsafe {
                let _ = DestroyIcon(HICON(*made as *mut _));
            }
        }
        *made = icon.0 as isize;
    }
    icon
}

/// The tray icon drawn for the taskbar's current theme, at the small-icon size of this DPI.
fn icon(state: &State) -> HICON {
    let light = crate::theme::system_light();
    if let Some(artwork) = state.artwork.lock().unwrap().as_ref() {
        let icon = from_rgba(if light { &artwork.for_light_taskbar } else { &artwork.for_dark_taskbar }, artwork.size);
        if !icon.is_invalid() {
            return keep(icon);
        }
    }
    let id = if light { state.options.icon_for_light_taskbar } else { state.options.icon_for_dark_taskbar };
    // SAFETY: loading an icon resource of this executable (its own copy, not LR_SHARED: the size follows the DPI).
    let loaded = unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let (width, height) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        LoadImageW(Some(instance.into()), PCWSTR(id as usize as *const u16), IMAGE_ICON, width, height, LR_DEFAULTCOLOR)
            .map(|handle| HICON(handle.0))
            .unwrap_or_default()
    };
    keep(loaded)
}

/// An icon from straight RGBA pixels, `size` a side.
fn from_rgba(rgba: &[u8], size: u32) -> HICON {
    let count = (size * size) as usize;
    if rgba.len() < count * 4 {
        return HICON::default();
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // top down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: the bitmaps are made here, written within their size, and deleted once the icon has its own copies.
    unsafe {
        let mut bits = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) else { return HICON::default() };
        let out = std::slice::from_raw_parts_mut(bits.cast::<u8>(), count * 4);
        for (to, from) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            to.copy_from_slice(&[from[2], from[1], from[0], from[3]]);
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

fn icon_data(window: HWND, state: &State) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon(state),
        ..Default::default()
    };
    for (slot, unit) in data.szTip.iter_mut().zip(state.tooltip.lock().unwrap().encode_utf16().take(127)) {
        *slot = unit;
    }
    data
}

fn add_icon(window: HWND, state: &State) {
    let mut data = icon_data(window, state);
    // SAFETY: a tray icon owned by our window.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
    }
}

fn show_menu(window: HWND, state: &State) {
    let items = state.menu.lock().unwrap().clone();
    let mut point = POINT::default();
    // SAFETY: the cursor position, written by the call.
    unsafe {
        let _ = GetCursorPos(&mut point);
    }
    if let Some(chosen) = crate::menu::popup(window.0 as isize, &items, point.x, point.y, TPM_BOTTOMALIGN.0) {
        (state.handler)(Event::Menu(chosen));
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(state) = STATE.get() else {
        // SAFETY: default handling for messages before the state exists.
        return unsafe { DefWindowProcW(window, message, wparam, lparam) };
    };
    match message {
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => (state.handler)(Event::Hotkey),
        WM_TRAY => match (lparam.0 & 0xFFFF) as u32 {
            NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP => (state.handler)(Event::Click),
            WM_CONTEXTMENU => show_menu(window, state),
            WM_MOUSEMOVE => (state.handler)(Event::Hover),
            _ => {}
        },
        WM_REOPEN => (state.handler)(Event::Reopen),
        WM_ENDSESSION if wparam.0 != 0 => (state.handler)(Event::EndSession),
        WM_SET_HOTKEY => {
            if wparam.0 == 0 {
                unregister_hotkey(window);
                return LRESULT(1);
            }
            let hotkey = Hotkey { modifiers: (wparam.0 >> 16) as u32, key: (wparam.0 & 0xFFFF) as u32 };
            return LRESULT(match register_hotkey(window, hotkey) {
                Ok(Hearing::Registered) => 1,
                Ok(Hearing::Hooked) => 2,
                Err(_) => 0,
            });
        }
        WM_SETTINGCHANGE => {
            // "ImmersiveColorSet": light/dark changed. The tray icon follows the taskbar's theme.
            let changed = lparam.0 != 0 && unsafe { PCWSTR(lparam.0 as *const u16).to_string() }
                .is_ok_and(|area| area == "ImmersiveColorSet");
            if changed {
                let data = icon_data(window, state);
                // SAFETY: updating our own tray icon.
                unsafe {
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
                }
                (state.handler)(Event::ThemeChanged);
            }
        }
        WM_QUIT_SHELL => {
            let data = icon_data(window, state);
            // SAFETY: removing our own tray icon and window.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                let _ = UnregisterHotKey(Some(window), HOTKEY_ID);
                hook::stop();
                let _ = DestroyWindow(window);
            }
        }
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        other if other == state.taskbar_created => add_icon(window, state), // Explorer restarted
        _ => {}
    }
    // SAFETY: default handling for everything else (and alongside the messages above).
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_from_pixels() {
        let icon = from_rgba(&[200, 80, 40, 255].repeat(16 * 16), 16);
        assert!(!icon.is_invalid());
        // SAFETY: the icon made above.
        unsafe {
            let _ = DestroyIcon(icon);
        }
        assert!(from_rgba(&[0; 8], 16).is_invalid());
    }

    #[test]
    fn hotkeys_parse_and_print() {
        let hotkey: Hotkey = "Alt+S".parse().unwrap();
        assert_eq!(hotkey, Hotkey { modifiers: MOD_ALT.0, key: u32::from(b'S') });
        assert_eq!(hotkey.to_string(), "Alt+S");
        assert_eq!("ctrl + shift + space".parse::<Hotkey>().unwrap().to_string(), "Ctrl+Shift+Space");
        assert_eq!("F9".parse::<Hotkey>().unwrap().to_string(), "F9");
        assert!("S".parse::<Hotkey>().is_err());
        assert!("Alt+Q+W".parse::<Hotkey>().is_err());
        assert!("Alt+Banana".parse::<Hotkey>().is_err());
    }
}
