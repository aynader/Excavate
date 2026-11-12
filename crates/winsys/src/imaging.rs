//! Decoding pictures with Windows Imaging Component: every format Windows can open (PNG, JPEG, WebP, TIFF, BMP, GIF,
//! and HEIC, AVIF, RAW when their codecs are installed), scaled down on decode when they're huge.

use crate::wide;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat24bppRGB, GUID_WICPixelFormat32bppPRGBA, IWICBitmapFrameDecode, IWICBitmapSource,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICBitmapTransformFlipHorizontal,
    WICBitmapTransformFlipVertical, WICBitmapTransformOptions, WICBitmapTransformRotate90, WICBitmapTransformRotate180,
    WICBitmapTransformRotate270, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::PCWSTR;

/// A decoded picture: 8-bit RGB, rows top to bottom.
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// The size before scaling.
    pub original: (u32, u32),
}

/// A picture decoded for showing, the right way up.
pub struct Shown {
    pub bitmap: crate::icons::Bitmap,
    /// Its size the right way up, before scaling.
    pub size: (u32, u32),
    /// How its metadata says to turn it (EXIF's 1 to 8, 1 being not at all).
    pub orientation: u16,
}

/// How a picture's metadata says to turn it: EXIF's 1 to 8, 1 being not at all. A phone's photo is often stored sideways.
fn orientation_of(frame: &IWICBitmapFrameDecode) -> u16 {
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Variant::VT_UI2;
    // SAFETY: a query that fills `value`, whose number is only read as what its type says it is.
    unsafe {
        let Ok(reader) = frame.GetMetadataQueryReader() else { return 1 };
        for query in [windows::core::w!("/app1/ifd/{ushort=274}"), windows::core::w!("/ifd/{ushort=274}")] {
            let mut value = PROPVARIANT::default();
            if reader.GetMetadataByName(query, &mut value).is_ok() && value.Anonymous.Anonymous.vt == VT_UI2 {
                let orientation = value.Anonymous.Anonymous.Anonymous.uiVal;
                if (1..=8).contains(&orientation) {
                    return orientation;
                }
            }
        }
    }
    1
}

/// EXIF's orientation as what turns the picture back.
fn transform(orientation: u16) -> WICBitmapTransformOptions {
    match orientation {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => WICBitmapTransformOptions(WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0),
        6 => WICBitmapTransformRotate90,
        7 => WICBitmapTransformOptions(WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformOptions(0),
    }
}

/// How a picture's metadata says to turn it, without decoding it. COM must be initialized on the calling thread.
pub fn orientation(path: &str) -> u16 {
    let file = wide(path);
    // SAFETY: WIC objects created, used and released here.
    unsafe {
        let Ok(factory) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) else { return 1 };
        let Ok(decoder) = factory.CreateDecoderFromFilename(PCWSTR(file.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand) else { return 1 };
        decoder.GetFrame(0).map(|frame| orientation_of(&frame)).unwrap_or(1)
    }
}

/// What a photo's metadata says about where and when it was taken.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Facts {
    /// When it was taken, as the camera's clock read it (its own time zone): seconds since 1970 counted as if that
    /// were UTC.
    pub taken: Option<i64>,
    /// Latitude and longitude, in degrees (north and east positive).
    pub position: Option<(f64, f64)>,
}

/// An image's size in pixels as stored, from its header, without decoding it (for `width:`/`height:`). COM must be
/// initialized on the calling thread.
pub fn size(path: &str) -> Option<(u32, u32)> {
    let file = wide(path);
    // SAFETY: WIC objects created, used and released here.
    unsafe {
        let factory = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = factory.CreateDecoderFromFilename(PCWSTR(file.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).ok()?;
        (width > 0 && height > 0).then_some((width, height))
    }
}

/// Where and when a photo was taken, from its EXIF metadata, without decoding it. COM must be initialized on the
/// calling thread.
pub fn facts(path: &str) -> Facts {
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
    use windows::Win32::System::Variant::{VT_LPSTR, VT_UI8, VT_VECTOR};
    let file = wide(path);
    // SAFETY: WIC objects created, used and released here; each value is read only as its type says, then cleared.
    unsafe {
        let Ok(factory) = CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) else { return Facts::default() };
        let Ok(decoder) = factory.CreateDecoderFromFilename(PCWSTR(file.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand) else { return Facts::default() };
        let Ok(frame) = decoder.GetFrame(0) else { return Facts::default() };
        let Ok(reader) = frame.GetMetadataQueryReader() else { return Facts::default() };
        let text = |queries: &[PCWSTR]| -> Option<String> {
            for &query in queries {
                let mut value = PROPVARIANT::default();
                if reader.GetMetadataByName(query, &mut value).is_ok() {
                    let found = (value.Anonymous.Anonymous.vt == VT_LPSTR).then(|| value.Anonymous.Anonymous.Anonymous.pszVal.to_string().ok()).flatten();
                    let _ = PropVariantClear(&mut value);
                    if found.is_some() {
                        return found;
                    }
                }
            }
            None
        };
        // Degrees, minutes and seconds, each a rational packed in 64 bits (numerator low, denominator high).
        let degrees = |queries: &[PCWSTR]| -> Option<f64> {
            for &query in queries {
                let mut value = PROPVARIANT::default();
                if reader.GetMetadataByName(query, &mut value).is_ok() {
                    let mut found = None;
                    if value.Anonymous.Anonymous.vt == (VT_VECTOR | VT_UI8) {
                        let list = &value.Anonymous.Anonymous.Anonymous.cauh;
                        if list.cElems == 3 && !list.pElems.is_null() {
                            let parts = std::slice::from_raw_parts(list.pElems, 3);
                            let rational = |packed: u64| {
                                let (numerator, denominator) = (packed as u32, (packed >> 32) as u32);
                                (denominator != 0).then(|| f64::from(numerator) / f64::from(denominator))
                            };
                            if let (Some(d), Some(m), Some(s)) = (rational(parts[0]), rational(parts[1]), rational(parts[2])) {
                                found = Some(d + m / 60.0 + s / 3600.0);
                            }
                        }
                    }
                    let _ = PropVariantClear(&mut value);
                    if found.is_some() {
                        return found;
                    }
                }
            }
            None
        };
        let taken = text(&[windows::core::w!("/app1/ifd/exif/{ushort=36867}"), windows::core::w!("/ifd/exif/{ushort=36867}"),
                           windows::core::w!("/app1/ifd/{ushort=306}"), windows::core::w!("/ifd/{ushort=306}")])
            .and_then(|text| exif_time(&text));
        let latitude = degrees(&[windows::core::w!("/app1/ifd/gps/{ushort=2}"), windows::core::w!("/ifd/gps/{ushort=2}")]);
        let longitude = degrees(&[windows::core::w!("/app1/ifd/gps/{ushort=4}"), windows::core::w!("/ifd/gps/{ushort=4}")]);
        let north = text(&[windows::core::w!("/app1/ifd/gps/{ushort=1}"), windows::core::w!("/ifd/gps/{ushort=1}")]);
        let east = text(&[windows::core::w!("/app1/ifd/gps/{ushort=3}"), windows::core::w!("/ifd/gps/{ushort=3}")]);
        let position = match (latitude, longitude) {
            // 0, 0 is where cameras without a fix often say they are.
            (Some(latitude), Some(longitude)) if latitude != 0.0 || longitude != 0.0 => Some((
                if north.as_deref().is_some_and(|reference| reference.starts_with('S')) { -latitude } else { latitude },
                if east.as_deref().is_some_and(|reference| reference.starts_with('W')) { -longitude } else { longitude },
            )),
            _ => None,
        };
        Facts { taken, position }
    }
}

/// EXIF's "2023:07:14 18:02:51" as seconds since 1970, counted as if it were UTC.
pub fn exif_time(text: &str) -> Option<i64> {
    let numbers: Vec<i64> = text.trim().split([':', ' ']).filter(|part| !part.is_empty()).map(|part| part.parse().ok()).collect::<Option<_>>()?;
    let [year, month, day, hour, minute, second] = numbers[..] else { return None };
    if !(1900..=2200).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days since 1970 from a civil date (Howard Hinnant's algorithm).
    let (y, m) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * m + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Decodes the first frame of a picture for showing: premultiplied RGBA, the right way up, scaled down (never up) to fit in
/// `bounds`. COM must be initialized on the calling thread.
pub fn decode_for_showing(path: &str, bounds: (u32, u32)) -> Result<Shown, String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let file = wide(path);
    // SAFETY: WIC objects created, used and released here; the pixel buffer is sized for the copy.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(error)?;
        let decoder = factory
            .CreateDecoderFromFilename(PCWSTR(file.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(error)?;
        let frame = decoder.GetFrame(0).map_err(error)?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).map_err(error)?;
        if width == 0 || height == 0 {
            return Err("empty picture".into());
        }
        let orientation = orientation_of(&frame);
        let sideways = orientation >= 5;
        let size = if sideways { (height, width) } else { (width, height) };
        let scale = (f64::from(bounds.0) / f64::from(size.0)).min(f64::from(bounds.1) / f64::from(size.1));
        let mut source: IWICBitmapSource = frame.into();
        if scale < 1.0 {
            (width, height) = ((f64::from(width) * scale).round().max(1.0) as u32, (f64::from(height) * scale).round().max(1.0) as u32);
            let scaler = factory.CreateBitmapScaler().map_err(error)?;
            scaler.Initialize(&source, width, height, WICBitmapInterpolationModeFant).map_err(error)?;
            source = scaler.into();
        }
        if orientation > 1 {
            let turner = factory.CreateBitmapFlipRotator().map_err(error)?;
            turner.Initialize(&source, transform(orientation)).map_err(error)?;
            source = turner.into();
            if sideways {
                (width, height) = (height, width);
            }
        }
        let converter = factory.CreateFormatConverter().map_err(error)?;
        converter
            .Initialize(&source, &GUID_WICPixelFormat32bppPRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(error)?;
        let stride = width * 4;
        let mut rgba = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut rgba).map_err(error)?;
        Ok(Shown { bitmap: crate::icons::Bitmap { width, height, rgba }, size, orientation })
    }
}

/// Decodes a picture held in memory: (width, height, straight RGBA). COM must be initialized on the calling thread.
pub fn decode_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    use windows::Win32::Graphics::Imaging::GUID_WICPixelFormat32bppRGBA;
    let error = |error: windows::core::Error| error.message().to_string();
    // SAFETY: WIC objects created, used and released here; `bytes` outlives them, and the pixel buffer is sized for the copy.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(error)?;
        let stream = factory.CreateStream().map_err(error)?;
        stream.InitializeFromMemory(bytes).map_err(error)?;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).map_err(error)?;
        let frame = decoder.GetFrame(0).map_err(error)?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).map_err(error)?;
        let converter = factory.CreateFormatConverter().map_err(error)?;
        converter
            .Initialize(&frame, &GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(error)?;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels).map_err(error)?;
        Ok((width, height, pixels))
    }
}

/// Decodes the first frame of a picture, scaled so its longer side is at most `max_side`. COM must be initialized on
/// the calling thread.
pub fn decode(path: &str, max_side: u32) -> Result<Rgb, String> {
    decode_as(path, max_side, false).map(|(picture, _)| picture)
}

/// `decode`, the right way up (its metadata's orientation applied: a phone's photo is often stored sideways), and that
/// orientation (EXIF's 1 to 8). `original` stays the size as stored; `unturned` takes what's found in it back there.
pub fn decode_upright(path: &str, max_side: u32) -> Result<(Rgb, u16), String> {
    decode_as(path, max_side, true)
}

fn decode_as(path: &str, max_side: u32, upright: bool) -> Result<(Rgb, u16), String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let file = wide(path);
    // SAFETY: WIC objects created, used and released here; the pixel buffer is sized for the copy.
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(error)?;
        let decoder = factory
            .CreateDecoderFromFilename(PCWSTR(file.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .map_err(error)?;
        let frame = decoder.GetFrame(0).map_err(error)?;
        let (mut width, mut height) = (0u32, 0u32);
        frame.GetSize(&mut width, &mut height).map_err(error)?;
        if width == 0 || height == 0 {
            return Err("empty picture".into());
        }
        let original = (width, height);
        let mut source: IWICBitmapSource = frame.into();
        let longer = width.max(height);
        if longer > max_side {
            let scale = max_side as f64 / longer as f64;
            let (w, h) = (((width as f64) * scale).round().max(1.0) as u32, ((height as f64) * scale).round().max(1.0) as u32);
            let scaler = factory.CreateBitmapScaler().map_err(error)?;
            scaler.Initialize(&source, w, h, WICBitmapInterpolationModeFant).map_err(error)?;
            source = scaler.into();
            (width, height) = (w, h);
        }
        let orientation = if upright { orientation_of(&decoder.GetFrame(0).map_err(error)?) } else { 1 };
        if orientation > 1 {
            let turner = factory.CreateBitmapFlipRotator().map_err(error)?;
            turner.Initialize(&source, transform(orientation)).map_err(error)?;
            source = turner.into();
            if orientation >= 5 {
                (width, height) = (height, width);
            }
        }
        let converter = factory.CreateFormatConverter().map_err(error)?;
        converter
            .Initialize(&source, &GUID_WICPixelFormat24bppRGB, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
            .map_err(error)?;
        let stride = width * 3;
        let mut pixels = vec![0u8; (stride * height) as usize];
        converter.CopyPixels(std::ptr::null(), stride, &mut pixels).map_err(error)?;
        Ok((Rgb { width, height, pixels, original }, orientation))
    }
}

/// Regions of a picture, and its size, once it's turned as its metadata says (EXIF's 1 to 8): what's kept of a picture
/// is where it is as stored, and what's shown is the right way up.
pub fn turned(regions: &[[i32; 4]], size: (u32, u32), orientation: u16) -> (Vec<[i32; 4]>, (u32, u32)) {
    let (w, h) = (size.0 as i32, size.1 as i32);
    let turned = regions.iter().map(|&[l, t, r, b]| match orientation {
        2 => [w - r, t, w - l, b],
        3 => [w - r, h - b, w - l, h - t],
        4 => [l, h - b, r, h - t],
        5 => [t, l, b, r],
        6 => [h - b, l, h - t, r],
        7 => [h - b, w - r, h - t, w - l],
        8 => [t, w - r, b, w - l],
        _ => [l, t, r, b],
    }).collect();
    (turned, if orientation >= 5 { (size.1, size.0) } else { size })
}

/// A region found in a picture the right way up (`decode_upright`; `size` its size so) where it is in the picture as
/// stored: `turned` undone (a quarter turn one way is undone by one the other way; the rest undo themselves).
pub fn unturned(region: [f32; 4], size: (f32, f32), orientation: u16) -> [f32; 4] {
    let back = match orientation { 6 => 8, 8 => 6, other => other };
    let (w, h) = size;
    let [l, t, r, b] = region;
    match back {
        2 => [w - r, t, w - l, b],
        3 => [w - r, h - b, w - l, h - t],
        4 => [l, h - b, r, h - t],
        5 => [t, l, b, r],
        6 => [h - b, l, h - t, r],
        7 => [h - b, w - r, h - t, w - l],
        8 => [t, w - r, b, w - l],
        _ => region,
    }
}

#[cfg(test)]
mod turn_tests {
    use super::*;

    /// A region turned the right way up and back is where it was, for every orientation.
    #[test]
    fn turned_and_back() {
        let region = [10, 20, 40, 90];
        for orientation in 1..=8 {
            let (up, size) = turned(&[region], (100, 200), orientation);
            let [l, t, r, b] = up[0];
            let back = unturned([l as f32, t as f32, r as f32, b as f32], (size.0 as f32, size.1 as f32), orientation);
            assert_eq!(back, region.map(|value| value as f32), "orientation {orientation}");
        }
    }
}
