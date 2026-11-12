//! Reading text in images with PP-OCRv6 (PaddleOCR's text detector and recognizer) on ONNX Runtime, on the processor
//! or, through DirectML, the graphics card: the benchmark's choice. Images are decoded by Windows (every format it knows), and each line
//! read keeps its box, so a secret on it can be blurred in the preview.

use crate::models::{self, Size};
use oar_ocr::core::config::{OrtExecutionProvider, OrtSessionConfig};
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::processors::LimitType;
use oar_ocr::prelude::{OAROCR, OAROCRBuilder};
use std::path::Path;

/// A line read from a picture: its text, and its box in the picture's own pixels (left, top, right, bottom).
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub bounds: [f32; 4],
}

/// What's read from one picture.
#[derive(Clone, Debug, Default)]
pub struct Read {
    pub lines: Vec<Line>,
    /// It has text in a shape this reader doesn't know well: worth another reader's look first.
    pub doubtful: bool,
}

pub struct Ocr {
    engine: OAROCR,
}

/// Pictures are decoded at most this large (the longer side): lines are recognized from this resolution.
pub const MAX_SIDE: u32 = 2560;

/// Text is found (detected) on the picture scaled to at most this; lines are then read from the full picture. The
/// detector's working memory grows with the square of it. Measured on the benchmark's 48 screenshots (up to 1920
/// px): word recall 97.0–97.3% from 960 to 2048 px; peak memory 185 MB at 960, 322 MB at 1600, 423 MB at 2048.
/// 1600 keeps a 4K screenshot's ordinary UI text above 10 px. `EXCAVATE_DETECT_SIDE` overrides it, for measuring.
const DETECT_SIDE: u32 = 1600;

/// The dedicated graphics card's place in DirectML's list (on a laptop the processor's own graphics come first).
pub fn dml_device() -> i32 {
    winsys::gpu::capable().map_or(0, |gpu| gpu.index as i32)
}

impl Ocr {
    /// `gpu`: on the graphics card, through DirectML (with the processor for what it can't run).
    pub fn load(models: &Path, size: Size, threads: usize, gpu: bool) -> Result<Ocr, String> {
        let folder = models::folder(models);
        let [detector, recognizer, dictionary] = size.files();
        // PP-OCRv6's published detection settings (as oar-ocr's preset has them).
        let detection = TextDetectionConfig {
            score_threshold: 0.2,
            box_threshold: if size == Size::Tiny { 0.4 } else { 0.45 },
            unclip_ratio: 1.4,
            max_candidates: 3000,
            limit_side_len: Some(std::env::var("EXCAVATE_DETECT_SIDE").ok().and_then(|side| side.parse().ok()).unwrap_or(DETECT_SIDE)),
            limit_type: Some(LimitType::Max),
            ..TextDetectionConfig::default()
        };
        let engine = OAROCRBuilder::new(folder.join(detector.name), folder.join(recognizer.name), folder.join(dictionary.name))
            .text_detection_config(detection)
            // The CPU provider registered explicitly runs without ONNX Runtime's memory arena, which keeps the high
            // water mark of every picture size it has seen: measured 1.15 GB with it, 0.32 GB without, for 10% more
            // time per picture (RapidOCR turns it off for the same reason). No spin-waiting between operations: idle
            // threads sleep instead of burning power.
            .ort_session(OrtSessionConfig::new().with_intra_threads(threads).with_inter_threads(1)
                .with_execution_providers(if gpu { vec![OrtExecutionProvider::DirectML { device_id: Some(dml_device()) }, OrtExecutionProvider::CPU] } else { vec![OrtExecutionProvider::CPU] })
                .add_config_entry("session.intra_op.allow_spinning", "0"))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Ocr { engine })
    }

    /// The lines in a picture (8-bit RGB), in reading order: rows top to bottom, then left to right.
    pub fn read(&self, width: u32, height: u32, pixels: Vec<u8>) -> Result<Read, String> {
        let image = image::RgbImage::from_raw(width, height, pixels).ok_or("an image of the wrong size")?;
        let results = self.engine.predict(vec![image]).map_err(|error| error.to_string())?;
        let Some(result) = results.into_iter().next() else { return Ok(Read::default()) };
        let doubtful = doubtful(result.text_regions.iter().map(|region| region.text_with_confidence()
            .filter(|(text, _)| !text.trim().is_empty()).map(|(_, confidence)| confidence).unwrap_or(0.0)));
        let mut lines: Vec<Line> = result
            .text_regions
            .iter()
            .filter_map(|region| {
                let (text, confidence) = region.text_with_confidence()?;
                if text.trim().is_empty() || confidence < 0.5 {
                    return None;
                }
                let points = &region.bounding_box.points;
                let left = points.iter().map(|p| p.x).fold(f32::MAX, f32::min);
                let top = points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
                let right = points.iter().map(|p| p.x).fold(f32::MIN, f32::max);
                let bottom = points.iter().map(|p| p.y).fold(f32::MIN, f32::max);
                Some(Line { text: text.to_string(), bounds: [left, top, right, bottom] })
            })
            .collect();
        let row = |line: &Line| (line.bounds[1] / 12.0).round();
        lines.sort_by(|a, b| (row(a), a.bounds[0]).partial_cmp(&(row(b), b.bounds[0])).unwrap_or(std::cmp::Ordering::Equal));
        Ok(Read { lines, doubtful })
    }
}

/// Whether what the recognizer made of a picture's text regions looks like a script it can't read (Arabic, Cyrillic,
/// Devanagari…): the detector finds their shapes, but the recognizer reads them as nothing at all, or is unsure. Such
/// pictures go to Deep first. `confidences`: per region, 0 for one read as nothing. A photo's textures often give a blank
/// region or two, which says nothing.
fn doubtful(confidences: impl Iterator<Item = f32>) -> bool {
    let confidences: Vec<f32> = confidences.collect();
    if confidences.len() <= 2 {
        return false;
    }
    let unsure = confidences.iter().filter(|&&confidence| confidence < UNSURE).count();
    unsure * 100 >= confidences.len() * DOUBTFUL_SHARE
}

/// A region read with less confidence than this is unsure, and when this share of them are (percent), the picture's
/// doubtful. Measured on the benchmark.
const UNSURE: f32 = 0.85;
const DOUBTFUL_SHARE: usize = 35;

/// Threads for one picture at a time: half the logical cores, at most 8. At idle priority they only use what the
/// person isn't using (measured on an i9-13900HX: 290 ms a screenshot with 4 threads, 219 ms with 8; more don't help).
pub fn threads() -> usize {
    if let Some(threads) = std::env::var("EXCAVATE_OCR_THREADS").ok().and_then(|value| value.parse().ok()) {
        return threads;
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (cores / 2).clamp(1, 8)
}
