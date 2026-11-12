//! File icons and thumbnails as Explorer draws them (IShellItemImageFactory): the shell's own caches, codecs and
//! thumbnail handlers, so HEIC, RAW, video and Office thumbnails come from whatever the PC has installed.
//! Call on a thread with COM ready (`com_sta`).

use crate::wide;
use std::ffi::c_void;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    SIIGBF_INCACHEONLY, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
};
use windows::core::PCWSTR;

/// Pixels, premultiplied RGBA, top row first.
#[derive(Clone, Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The type's (or the file's own) icon.
    Icon,
    /// A thumbnail when the shell has a handler for the type, else the icon.
    Preview,
    /// A thumbnail only if the shell has one cached already (fast; no decoding).
    CachedThumbnail,
    /// A thumbnail only.
    Thumbnail,
}

/// The image of `path` at `size` pixels (the long side; thumbnails keep their aspect).
pub fn image(path: &str, size: u32, kind: Kind) -> Option<Bitmap> {
    let flags: SIIGBF = match kind {
        Kind::Icon => SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
        Kind::Preview => SIIGBF_RESIZETOFIT,
        Kind::CachedThumbnail => SIIGBF_THUMBNAILONLY | SIIGBF_INCACHEONLY,
        Kind::Thumbnail => SIIGBF_THUMBNAILONLY,
    };
    let file = wide(path);
    // SAFETY: the shell item lives through the call; the bitmap is copied out and then deleted.
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(file.as_ptr()), None).ok()?;
        let bitmap = factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, flags).ok()?;
        let pixels = read_bitmap(bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        pixels
    }
}

/// A 32-bit DIB section (premultiplied BGRA, as the shell makes them) as premultiplied RGBA.
///
/// # Safety
/// `bitmap` is a valid bitmap handle.
unsafe fn read_bitmap(bitmap: HBITMAP) -> Option<Bitmap> {
    // SAFETY: the caller's contract; the buffers are sized for the reads.
    unsafe {
        let mut header = BITMAP::default();
        if GetObjectW(HGDIOBJ(bitmap.0), std::mem::size_of::<BITMAP>() as i32, Some((&mut header as *mut BITMAP).cast())) == 0 {
            return None;
        }
        let (width, height) = (header.bmWidth, header.bmHeight.abs());
        if width <= 0 || height <= 0 {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, bitmap, 0, height as u32, Some(pixels.as_mut_ptr() as *mut c_void), &mut info, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }
        // Icons carry alpha; a photo's thumbnail may carry none at all (every alpha 0): opaque then.
        let has_alpha = pixels.chunks_exact(4).any(|pixel| pixel[3] != 0);
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            if !has_alpha {
                pixel[3] = 255;
            } else {
                // Some icons come back with color above their alpha (not premultiplied after all): clamped, so
                // blending stays in range.
                let alpha = pixel[3];
                for channel in &mut pixel[..3] {
                    *channel = (*channel).min(alpha);
                }
            }
        }
        Some(Bitmap { width: width as u32, height: height as u32, rgba: pixels })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_and_thumbnails() {
        crate::com_sta();
        let windows = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let icon = image(&format!(r"{windows}\explorer.exe"), 32, Kind::Icon).expect("explorer's icon");
        assert!(icon.width >= 32 && icon.rgba.len() == (icon.width * icon.height * 4) as usize);
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0), "an icon has transparent corners");
        assert!(image(&windows, 32, Kind::Icon).is_some(), "a folder's icon");
        assert!(image(r"C:\no\such\file.txt", 32, Kind::Icon).is_none());
        let wallpaper = format!(r"{windows}\Web\Wallpaper\Windows\img0.jpg");
        if std::path::Path::new(&wallpaper).exists() {
            let thumbnail = image(&wallpaper, 256, Kind::Thumbnail).expect("a JPEG's thumbnail");
            assert!(thumbnail.width.max(thumbnail.height) <= 256 + 1 && thumbnail.rgba[3] == 255, "{}x{}", thumbnail.width, thumbnail.height);
        }
    }
}
