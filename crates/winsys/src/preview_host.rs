//! Office files shown by Windows' own preview handlers (the ones Explorer's preview pane uses: Word's, Excel's,
//! PowerPoint's), each in a child window of ours placed where the preview's image would be. The handlers usually run
//! in their own process (prevhost.exe), so one that fails doesn't take Excavate with it. COM must be ready on the
//! calling thread (`com_sta`), which owns the window.

use crate::wide;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CoCreateInstance, STGM_READ};
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_DEFAULTTOSTAR, ASSOCSTR_SHELLEXTENSION, AssocQueryStringW, IInitializeWithItem, IPreviewHandler,
    SHCreateItemFromParsingName, SHCreateStreamOnFileEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, SetWindowPos, ShowWindow, WINDOW_EX_STYLE,
    WS_CHILD, WS_CLIPCHILDREN, WS_CLIPSIBLINGS,
};
use windows::core::{GUID, Interface, PCWSTR, w};

pub type Result<T> = std::result::Result<T, String>;

fn text(error: windows::core::Error) -> String {
    error.message().to_string()
}

/// The shell's key for a type's preview handler.
const PREVIEW_HANDLER: PCWSTR = w!("{8895b1c6-b41f-4c1c-a562-0d564250836f}");

/// The preview handler registered for `path`'s type, if there is one.
pub fn handler_for(path: &str) -> Option<GUID> {
    let extension = std::path::Path::new(path).extension()?.to_string_lossy().to_string();
    let extension = wide(&format!(".{extension}"));
    let mut out = [0u16; 64];
    let mut length = out.len() as u32;
    // SAFETY: the buffers outlive the call, and the length is the buffer's.
    unsafe {
        AssocQueryStringW(ASSOCF_INIT_DEFAULTTOSTAR, ASSOCSTR_SHELLEXTENSION, PCWSTR(extension.as_ptr()), PREVIEW_HANDLER,
                          Some(windows::core::PWSTR(out.as_mut_ptr())), &mut length).ok().ok()?;
    }
    let text = String::from_utf16_lossy(&out[..(length as usize).saturating_sub(1)]);
    let text = text.trim_matches(['{', '}']);
    GUID::try_from(text).ok()
}

/// A file previewed by its handler, in a child window of `parent`. Dropped: the handler unloads and the window goes.
pub struct Hosted {
    handler: IPreviewHandler,
    window: HWND,
    pub path: String,
    rect: (i32, i32, i32, i32),
}

impl Hosted {
    /// Previews `path` in a new child window of `parent` at `rect` (x, y, width, height in the parent's pixels).
    pub fn open(parent: isize, path: &str, rect: (i32, i32, i32, i32)) -> Result<Hosted> {
        let clsid = handler_for(path).ok_or_else(|| "no preview handler for this type".to_string())?;
        let (x, y, width, height) = rect;
        // SAFETY: the window is ours (a plain child), the COM objects are kept or dropped here, and the names outlive
        // the calls that take them.
        unsafe {
            let window = CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), PCWSTR::null(), WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                                         x, y, width.max(1), height.max(1), Some(HWND(parent as *mut _)), None, None, None).map_err(text)?;
            let made: windows::core::Result<IPreviewHandler> = CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER | CLSCTX_INPROC_SERVER);
            let handler = match made {
                Ok(handler) => handler,
                Err(error) => {
                    let _ = DestroyWindow(window);
                    return Err(text(error));
                }
            };
            let name = wide(path);
            // A handler takes the file by its path, by a stream, or as a shell item: whichever it offers.
            let initialized = if let Ok(by_file) = handler.cast::<IInitializeWithFile>() {
                by_file.Initialize(PCWSTR(name.as_ptr()), STGM_READ.0)
            } else if let Ok(by_stream) = handler.cast::<IInitializeWithStream>() {
                SHCreateStreamOnFileEx(PCWSTR(name.as_ptr()), STGM_READ.0, 0, false, None)
                    .and_then(|stream| by_stream.Initialize(&stream, STGM_READ.0))
            } else if let Ok(by_item) = handler.cast::<IInitializeWithItem>() {
                SHCreateItemFromParsingName::<_, _, windows::Win32::UI::Shell::IShellItem>(PCWSTR(name.as_ptr()), None)
                    .and_then(|item| by_item.Initialize(&item, STGM_READ.0))
            } else {
                Err(windows::core::Error::from(windows::Win32::Foundation::E_NOINTERFACE))
            };
            let shown = initialized.and_then(|()| {
                handler.SetWindow(window, &RECT { left: 0, top: 0, right: width, bottom: height })?;
                handler.DoPreview()
            });
            if let Err(error) = shown {
                let _ = handler.Unload();
                let _ = DestroyWindow(window);
                return Err(text(error));
            }
            let _ = ShowWindow(window, SW_SHOWNA);
            Ok(Hosted { handler, window, path: path.to_string(), rect })
        }
    }

    /// Where it is now (the parent's pixels).
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        self.rect
    }

    /// Moved and resized to `rect` (the parent's pixels), and shown or hidden.
    pub fn place(&mut self, rect: (i32, i32, i32, i32), shown: bool) {
        let (x, y, width, height) = rect;
        // SAFETY: our own child window and handler.
        unsafe {
            if rect != self.rect {
                let _ = SetWindowPos(self.window, None, x, y, width.max(1), height.max(1), SWP_NOZORDER | SWP_NOACTIVATE);
                let _ = self.handler.SetRect(&RECT { left: 0, top: 0, right: width, bottom: height });
                self.rect = rect;
            }
            let _ = ShowWindow(self.window, if shown { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        // SAFETY: our own handler and window, unloaded and destroyed once.
        unsafe {
            let _ = self.handler.Unload();
            let _ = DestroyWindow(self.window);
        }
    }
}
