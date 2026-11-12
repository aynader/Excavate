//! Screens, the taskbar, and where the popup goes: at the taskbar's edge of the screen it opens on, centered when the
//! taskbar centers its icons (Windows 11's default), as EverythingToolbar and the Start menu do.

use crate::registry::{Root, dword};
use std::collections::HashMap;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows::core::BOOL;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{ABM_GETTASKBARPOS, APPBARDATA, SHAppBarMessage};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

impl From<RECT> for Rect {
    fn from(rect: RECT) -> Rect {
        Rect { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

/// A screen, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub bounds: Rect,
    /// The screen without the taskbar (and other app bars).
    pub work: Rect,
    /// 1.0 at 96 DPI, 1.5 at 144…
    pub scale: f32,
}

impl Monitor {
    /// The taskbar's edge of this screen: the side the work area gives up. A hidden taskbar gives up nothing; the
    /// primary taskbar's edge then.
    pub fn taskbar_edge(&self) -> Edge {
        let (bounds, work) = (self.bounds, self.work);
        if work.bottom < bounds.bottom {
            Edge::Bottom
        } else if work.top > bounds.top {
            Edge::Top
        } else if work.left > bounds.left {
            Edge::Left
        } else if work.right < bounds.right {
            Edge::Right
        } else {
            primary_taskbar_edge().unwrap_or(Edge::Bottom)
        }
    }
}

pub fn cursor() -> (i32, i32) {
    let mut point = POINT::default();
    // SAFETY: a POINT written by the call.
    unsafe {
        let _ = GetCursorPos(&mut point);
    }
    (point.x, point.y)
}

/// The screen showing the point (the nearest one, if none does).
pub fn monitor_at(x: i32, y: i32) -> Monitor {
    // SAFETY: a plain query.
    let handle = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    described(handle).0
}

/// A screen, what Windows calls it (`\\.\DISPLAY1`, which can change), and whether it's the primary one.
fn described(handle: HMONITOR) -> (Monitor, String, bool) {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    // SAFETY: plain queries about a monitor handle Windows returned; the info struct is sized for the call.
    unsafe {
        let _ = GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO);
        let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }
    let monitor = Monitor { bounds: info.monitorInfo.rcMonitor.into(), work: info.monitorInfo.rcWork.into(), scale: dpi_x as f32 / 96.0 };
    (monitor, wide(&info.szDevice), info.monitorInfo.dwFlags & 1 != 0)
}

fn wide(text: &[u16]) -> String {
    String::from_utf16_lossy(&text[..text.iter().position(|&c| c == 0).unwrap_or(text.len())])
}

/// A screen as it's offered in settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// Lasts while it's plugged into the same port, however Windows renumbers its screens.
    pub id: String,
    /// "DELL U2723QE · 3840 × 2160", or "Second · 1920 × 1080" where Windows has no name for it.
    pub name: String,
    pub monitor: Monitor,
}

/// The screens there are now: the primary first, then from left to right.
pub fn screens() -> Vec<Screen> {
    unsafe extern "system" fn found(handle: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        // SAFETY: `list` is the vector passed below, which outlives the call.
        unsafe { (*(list.0 as *mut Vec<HMONITOR>)).push(handle) };
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only runs during the call.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(found), LPARAM(&mut handles as *mut _ as isize));
    }
    let mut screens: Vec<(Monitor, String, bool)> = handles.into_iter().map(described).collect();
    screens.sort_by_key(|(monitor, _, primary)| (!primary, monitor.bounds.left, monitor.bounds.top));
    let known = known_names();
    let details: Vec<(String, bool, Rect)> = screens
        .iter()
        .map(|(monitor, device, _)| {
            let (_, name, built_in) = known.get(device).cloned().unwrap_or_default();
            (name, built_in, monitor.bounds)
        })
        .collect();
    let names = screen_names(&details);
    screens
        .into_iter()
        .zip(names)
        .map(|((monitor, device, _), name)| {
            // (Without a lasting ID, what Windows calls it now is the best there is.)
            let id = known.get(&device).map(|(id, ..)| id.clone()).filter(|id| !id.is_empty()).unwrap_or(device);
            Screen { id, name, monitor }
        })
        .collect()
}

/// What each screen is called, in order: its name with its size. One Windows has no name for goes by its place in the list.
fn screen_names(screens: &[(String, bool, Rect)]) -> Vec<String> {
    const PLACES: [&str; 4] = ["Main", "Second", "Third", "Fourth"];
    let base: Vec<String> = screens
        .iter()
        .enumerate()
        .map(|(index, (name, built_in, _))| match () {
            _ if !name.is_empty() => name.clone(),
            _ if *built_in => "Built-in screen".to_string(),
            _ => PLACES.get(index).map_or_else(|| format!("Monitor {}", index + 1), |place| place.to_string()),
        })
        .collect();
    base.iter()
        .zip(screens)
        .enumerate()
        .map(|(index, (name, (.., bounds)))| {
            // Two of the same kind: they're told apart by order.
            let same = base.iter().filter(|other| *other == name).count();
            let number = base[..=index].iter().filter(|other| *other == name).count();
            let name = if same > 1 { format!("{name} {number}") } else { name.clone() };
            format!("{name} · {} × {}", bounds.width(), bounds.height())
        })
        .collect()
}

/// What Windows knows of the displays connected, by what it calls each now (`\\.\DISPLAY1`): its lasting ID (the path of
/// the device), its name ("DELL U2723QE"), and whether it's built into this PC.
fn known_names() -> HashMap<String, (String, String, bool)> {
    let (mut path_count, mut mode_count) = (0u32, 0u32);
    let mut known = HashMap::new();
    // SAFETY: the arrays are as long as the counts the first call gave, which the second call may lower; each request
    // packet says its own type and size.
    unsafe {
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) != ERROR_SUCCESS {
            return known;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut path_count, paths.as_mut_ptr(), &mut mode_count, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return known;
        }
        paths.truncate(path_count as usize);
        for path in paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 || DisplayConfigGetDeviceInfo(&mut target.header) != 0 {
                continue;
            }
            let built_in = [
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
                DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
            ]
            .contains(&target.outputTechnology);
            // (One picture on two displays: the first names it.)
            known.entry(wide(&source.viewGdiDeviceName)).or_insert((wide(&target.monitorDevicePath), wide(&target.monitorFriendlyDeviceName), built_in));
        }
    }
    known
}

/// The primary taskbar's edge.
pub fn primary_taskbar_edge() -> Option<Edge> {
    let mut data = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    // SAFETY: APPBARDATA is sized and filled by the call.
    if unsafe { SHAppBarMessage(ABM_GETTASKBARPOS, &mut data) } == 0 {
        return None;
    }
    Some(match data.uEdge {
        0 => Edge::Left,
        1 => Edge::Top,
        2 => Edge::Right,
        _ => Edge::Bottom,
    })
}

/// Whether the taskbar centers its icons (Settings → Personalization → Taskbar → Taskbar alignment).
pub fn taskbar_centered() -> bool {
    dword(Root::CurrentUser, r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced", "TaskbarAl") != Some(0)
}

/// Where a popup of `width` × `height` physical pixels goes on `monitor`: against the taskbar's edge with `margin`
/// around it, centered along that edge when `centered`, else at its start (where a left-aligned Start button is).
pub fn place(monitor: &Monitor, edge: Edge, centered: bool, width: i32, height: i32, margin: i32) -> (i32, i32) {
    let work = monitor.work;
    let along = |start: i32, end: i32, size: i32| -> i32 {
        let wanted = if centered { start + (end - start - size) / 2 } else { start + margin };
        wanted.clamp(start + margin, (end - size - margin).max(start + margin))
    };
    match edge {
        Edge::Bottom => (along(work.left, work.right, width), work.bottom - height - margin),
        Edge::Top => (along(work.left, work.right, width), work.top + margin),
        Edge::Left => (work.left + margin, along(work.top, work.bottom, height)),
        Edge::Right => (work.right - width - margin, along(work.top, work.bottom, height)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Monitor {
        Monitor {
            bounds: Rect { left: 0, top: 0, right: 2560, bottom: 1600 },
            work: Rect { left: 0, top: 0, right: 2560, bottom: 1528 },
            scale: 1.5,
        }
    }

    #[test]
    fn above_a_centered_taskbar() {
        let monitor = screen();
        assert_eq!(monitor.taskbar_edge(), Edge::Bottom);
        assert_eq!(place(&monitor, Edge::Bottom, true, 1200, 800, 18), (680, 1528 - 800 - 18));
        assert_eq!(place(&monitor, Edge::Bottom, false, 1200, 800, 18), (18, 710));
    }

    #[test]
    fn other_edges_and_small_screens() {
        let monitor = Monitor { work: Rect { left: 100, top: 0, right: 1280, bottom: 720 },
                                bounds: Rect { left: 0, top: 0, right: 1280, bottom: 720 }, scale: 1.0 };
        assert_eq!(monitor.taskbar_edge(), Edge::Left);
        assert_eq!(place(&monitor, Edge::Left, true, 600, 400, 12), (112, 160));
        // Taller than the screen: pinned at the start rather than off it.
        assert_eq!(place(&monitor, Edge::Left, true, 600, 900, 12).1, 12);
    }

    #[test]
    fn live_screen() {
        let (x, y) = cursor();
        let monitor = monitor_at(x, y);
        assert!(monitor.bounds.width() > 0 && monitor.scale >= 1.0, "{monitor:?}");
    }

    #[test]
    fn what_screens_are_called() {
        let size = |width, height| Rect { left: 0, top: 0, right: width, bottom: height };
        let names = screen_names(&[
            (String::new(), true, size(2560, 1600)),
            ("DELL U2723QE".to_string(), false, size(3840, 2160)),
            (String::new(), false, size(1920, 1080)),
        ]);
        assert_eq!(names, ["Built-in screen · 2560 × 1600", "DELL U2723QE · 3840 × 2160", "Third · 1920 × 1080"]);
        // Two of the same.
        let names = screen_names(&[("LG 27".to_string(), false, size(1920, 1080)), ("LG 27".to_string(), false, size(1920, 1080))]);
        assert_eq!(names, ["LG 27 1 · 1920 × 1080", "LG 27 2 · 1920 × 1080"]);
        // No name, and no way to tell.
        assert_eq!(screen_names(&[(String::new(), false, size(800, 600))]), ["Main · 800 × 600"]);
    }

    /// Run with --nocapture to see what this PC has, and how long asking takes (the popup asks each time it opens on a chosen
    /// monitor).
    #[test]
    fn live_screens() {
        let started = std::time::Instant::now();
        let screens = screens();
        let took = started.elapsed();
        assert!(!screens.is_empty());
        assert!(screens.iter().all(|screen| !screen.id.is_empty() && !screen.name.is_empty()), "{screens:?}");
        for screen in &screens {
            println!("{} ({}) scale {}", screen.name, screen.id, screen.monitor.scale);
        }
        println!("{} µs", took.as_micros());
    }
}
