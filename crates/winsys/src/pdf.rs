//! A PDF's first page as an image, by Windows' own PDF renderer (Windows.Data.Pdf, in Windows since 8.1): nothing to
//! ship or load beyond it, rendered at the size asked for and nothing larger, and nothing kept. Call on a thread with
//! COM in the multithreaded apartment (`com_mta`): its calls finish on threads of their own.

use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
use windows::core::HSTRING;

/// The first page of the PDF at `path`, its longer side `max_side` pixels: straight RGBA (the page is opaque, so it's
/// premultiplied too). Fails for a PDF with a password, or one Windows can't read.
pub fn first_page(path: &str, max_side: u32) -> Result<crate::icons::Bitmap, String> {
    first_page_sized(path, &|_| max_side).map(|(bitmap, _)| bitmap)
}

/// The first page, at the longer side `side` picks from the page's own size (device-independent pixels, 1/96 inch):
/// the bitmap, and that size.
pub fn first_page_sized(path: &str, side: &dyn Fn((f32, f32)) -> u32) -> Result<(crate::icons::Bitmap, (f32, f32)), String> {
    let error = |error: windows::core::Error| error.message().to_string();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path)).map_err(error)?.join().map_err(error)?;
    let document = PdfDocument::LoadFromFileAsync(&file).map_err(error)?.join().map_err(error)?;
    if document.PageCount().map_err(error)? == 0 {
        return Err("no pages".into());
    }
    let page = document.GetPage(0).map_err(error)?;
    let size = page.Size().map_err(error)?;
    let (width, height) = (size.Width.max(1.0), size.Height.max(1.0));
    let scale = side((width, height)).max(1) as f32 / width.max(height);
    let options = PdfPageRenderOptions::new().map_err(error)?;
    options.SetDestinationWidth(((width * scale).round() as u32).max(1)).map_err(error)?;
    options.SetDestinationHeight(((height * scale).round() as u32).max(1)).map_err(error)?;
    let stream = InMemoryRandomAccessStream::new().map_err(error)?;
    page.RenderWithOptionsToStreamAsync(&stream, &options).map_err(error)?.join().map_err(error)?;
    let length = stream.Size().map_err(error)? as u32;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(error)?).map_err(error)?;
    reader.LoadAsync(length).map_err(error)?.join().map_err(error)?;
    let mut bytes = vec![0u8; length as usize];
    reader.ReadBytes(&mut bytes).map_err(error)?;
    let (rendered_width, rendered_height, rgba) = crate::imaging::decode_rgba(&bytes)?;
    Ok((crate::icons::Bitmap { width: rendered_width, height: rendered_height, rgba }, (width, height)))
}
