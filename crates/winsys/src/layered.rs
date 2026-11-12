//! A borderless, always-on-top, click-through window with true per-pixel transparency (a layered window), for the
//! launch animation. It belongs to the thread that creates it.

use crate::wide;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, RegisterClassExW, SW_SHOWNOACTIVATE,
    ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::PCWSTR;

unsafe extern "system" fn window_proc(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: default handling of a message for our own window.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

pub struct Layered {
    window: HWND,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    capacity: usize,
}

impl Layered {
    /// A window that can show up to `capacity` × `capacity` pixels. Nothing shows until the first `present`.
    pub fn new(capacity: usize) -> Result<Layered, String> {
        let error = |error: windows::core::Error| error.message().to_string();
        // SAFETY: the class, window, memory DC and bitmap are created here and released in Drop.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None).map_err(error)?.into();
            let class = wide("Excavate.Intro");
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            });
            let window = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class.as_ptr()), PCWSTR::null(), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None,
            ).map_err(error)?;
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: capacity as i32,
                    biHeight: -(capacity as i32), // top down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).map_err(error)?;
            let previous = SelectObject(dc, bitmap.into());
            Ok(Layered { window, dc, bitmap, previous, bits: bits.cast(), capacity })
        }
    }

    /// Shows `size` × `size` pixels (BGRA, premultiplied) with their top left corner at (`left`, `top`), at `opacity`.
    pub fn present(&self, left: i32, top: i32, size: usize, pixels: &[u8], opacity: u8) {
        let size = size.min(self.capacity);
        // SAFETY: the DIB holds `capacity` rows of `capacity` pixels; what's copied fits, and `pixels` is checked.
        unsafe {
            if pixels.len() >= size * size * 4 {
                for row in 0..size {
                    std::ptr::copy_nonoverlapping(pixels.as_ptr().add(row * size * 4), self.bits.add(row * self.capacity * 4), size * 4);
                }
            }
            let (position, extent, origin) = (POINT { x: left, y: top }, SIZE { cx: size as i32, cy: size as i32 }, POINT::default());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: opacity, AlphaFormat: AC_SRC_ALPHA as u8 };
            let _ = UpdateLayeredWindow(self.window, None, Some(&position), Some(&extent), Some(self.dc), Some(&origin), COLORREF(0), Some(&blend), ULW_ALPHA);
            let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
        }
    }

    /// Waits for the next frame on screen, handling what Windows sends the window meanwhile.
    pub fn next_frame(&self) {
        // SAFETY: this thread's message queue; DwmFlush takes no arguments.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if DwmFlush().is_err() {
                std::thread::sleep(std::time::Duration::from_millis(8));
            }
        }
    }
}

impl Drop for Layered {
    fn drop(&mut self) {
        // SAFETY: what `new` created.
        unsafe {
            let _ = DestroyWindow(self.window);
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
