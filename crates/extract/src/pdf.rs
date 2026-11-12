//! PDF through PDFium (Chrome's PDF engine, `pdfium.dll` beside the executable): the text layer of each page, and
//! pages without one (scans, photos of documents) found and rendered for OCR.

use crate::Extracted;
use pdfium_render::prelude::*;
use std::path::Path;
use std::sync::OnceLock;

/// PDFium, from pdfium.dll beside the executable.
static LIBRARY: OnceLock<Result<Pdfium, String>> = OnceLock::new();

fn pdfium() -> Result<&'static Pdfium, String> {
    LIBRARY
        .get_or_init(|| {
            let folder = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)).unwrap_or_default();
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&folder))
                .map(Pdfium::new)
                .map_err(|error| format!("PDFium isn't there ({}): {error}", folder.display()))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Whether PDFs can be read on this PC.
pub fn available() -> bool {
    pdfium().is_ok()
}

/// A page with fewer characters than this in its text layer is treated as a scan.
const SCANNED_BELOW: usize = 24;

pub fn text(path: &Path) -> Result<Extracted, String> {
    let pdfium = pdfium()?;
    let document = pdfium.load_pdf_from_file(path, None).map_err(|error| describe(&error))?;
    let pages = document.pages();
    let count = pages.len().max(0) as u16;
    let mut text = String::new();
    let mut scanned = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let page_text = page.text().map(|text| text.all()).unwrap_or_default();
        let characters = page_text.chars().filter(|c| !c.is_whitespace()).count();
        if characters < SCANNED_BELOW && has_images(&page) {
            scanned.push(index as u16);
        }
        text.push_str(&page_text);
        text.push_str("\n\n");
        if text.len() > content_limit() {
            break;
        }
    }
    Ok(Extracted { text, scanned_pages: scanned, pages: count })
}

/// Where each of `values` (a secret's text, as the text layer has it) is on the first page, as fractions of the page
/// (left, top, right, bottom, from its top left), for covering only them in a preview of that page. Every place each
/// is found, by PDFium's box of each character. None when the page is turned (the preview shows it turned) or has no
/// text layer; an empty list when none of them is on it.
pub fn first_page_places(path: &Path, values: &[String]) -> Result<Option<Vec<[f32; 4]>>, String> {
    let pdfium = pdfium()?;
    let document = pdfium.load_pdf_from_file(path, None).map_err(|error| describe(&error))?;
    let page = document.pages().get(0).map_err(|error| describe(&error))?;
    if !matches!(page.rotation(), Ok(PdfPageRenderRotation::None)) {
        return Ok(None);
    }
    let (width, height) = (page.width().value, page.height().value);
    if width <= 0.0 || height <= 0.0 {
        return Ok(None);
    }
    let Ok(text) = page.text() else { return Ok(None) };
    // The page's characters in order, each with where it is in `joined` and its box.
    let mut joined = String::new();
    let mut spans: Vec<(std::ops::Range<usize>, Option<PdfRect>)> = Vec::new();
    for character in text.chars().iter() {
        let Some(unicode) = character.unicode_string() else { continue };
        let start = joined.len();
        joined.push_str(&unicode);
        spans.push((start..joined.len(), character.loose_bounds().ok()));
    }
    if joined.trim().is_empty() {
        return Ok(None);
    }
    let mut places = Vec::new();
    for value in values.iter().map(|value| value.trim()).filter(|value| !value.is_empty()) {
        let mut from = 0;
        while let Some(at) = joined[from..].find(value) {
            let found = from + at..from + at + value.len();
            from = found.end;
            let mut bounds: Option<[f32; 4]> = None;
            for (_, rect) in spans.iter().filter(|(range, _)| range.start < found.end && range.end > found.start) {
                let Some(rect) = rect else { continue };
                // (PDF pages count up from the bottom.)
                let one = [rect.left().value / width, 1.0 - rect.top().value / height, rect.right().value / width, 1.0 - rect.bottom().value / height];
                bounds = Some(match bounds {
                    None => one,
                    Some(b) => [b[0].min(one[0]), b[1].min(one[1]), b[2].max(one[2]), b[3].max(one[3])],
                });
            }
            if let Some(bounds) = bounds {
                places.push(bounds.map(|value| value.clamp(0.0, 1.0)));
            }
        }
    }
    Ok(Some(places))
}

fn content_limit() -> usize {
    4 << 20
}

fn has_images(page: &PdfPage) -> bool {
    page.objects().iter().any(|object| object.object_type() == PdfPageObjectType::Image)
}

/// Pages of a PDF as pixels, `width` wide, the PDF opened once (a scanned book isn't loaded again for each page):
/// `each` gets every page's pixels in turn, and an error from it stops there.
pub fn render_each(path: &Path, pages: &[u16], width: u32, each: &mut dyn FnMut(u32, u32, Vec<u8>) -> Result<(), String>) -> Result<(), String> {
    let pdfium = pdfium()?;
    let document = pdfium.load_pdf_from_file(path, None).map_err(|error| describe(&error))?;
    let config = PdfRenderConfig::new().set_target_width(width as Pixels).set_maximum_height((width * 2) as Pixels);
    for &page in pages {
        let page = document.pages().get(page as PdfPageIndex).map_err(|error| describe(&error))?;
        let bitmap = page.render_with_config(&config).map_err(|error| describe(&error))?;
        let image = bitmap.as_image().map_err(|error| describe(&error))?.into_rgb8();
        drop(bitmap);
        drop(page);
        each(image.width(), image.height(), image.into_raw())?;
    }
    Ok(())
}

fn describe(error: &PdfiumError) -> String {
    match error {
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => "protected by a password".into(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Several pages from one opening, each its own size, in the order asked (PDFium beside the test, else skipped).
    #[test]
    fn pages_rendered_from_one_opening() {
        let Ok(pdfium) = pdfium() else {
            eprintln!("skipped: no PDFium beside the test");
            return;
        };
        let file = std::env::temp_dir().join(format!("extract-pages-{}.pdf", std::process::id()));
        {
            let mut document = pdfium.create_new_pdf().unwrap();
            document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()).unwrap();
            document.pages_mut().create_page_at_end(PdfPagePaperSize::a4().landscape()).unwrap();
            document.save_to_file(&file).unwrap();
        }
        let mut sizes = Vec::new();
        render_each(&file, &[1, 0, 1], 400, &mut |width, height, pixels| {
            assert_eq!(pixels.len(), (width * height * 3) as usize);
            sizes.push((width, height));
            Ok(())
        })
        .unwrap();
        assert_eq!(sizes.len(), 3);
        assert!(sizes[0].0 == 400 && sizes[0].1 < 400, "landscape: {sizes:?}");
        assert!(sizes[1].0 == 400 && sizes[1].1 > 400, "portrait: {sizes:?}");
        assert_eq!(sizes[0], sizes[2]);
        let stopped = render_each(&file, &[0, 1], 400, &mut |_, _, _| Err("stop".into()));
        assert_eq!(stopped, Err("stop".to_string()));
        assert!(render_each(&file, &[5], 400, &mut |_, _, _| Ok(())).is_err(), "no such page");
        let _ = std::fs::remove_file(&file);
    }
}
