//! Text in videos: the words a video shows (slides, screen recordings, captions, signs), each with the moment it
//! first appears. Compute goes only to frames that can show new text:
//!
//! 1. **Keyframes first.** A seek lands on the keyframe before the moment asked for, and that frame alone is decoded:
//!    the cheapest frame there is. Encoders put one at every cut (a new slide, a new scene) and every few seconds.
//! 2. **Between two keyframes only when their text differs.** Then the frames between them are decoded, forward, at
//!    the pace asked for: a caption or a line typed between them is read at its moment. Footage without text, and a
//!    slide held still, never get there.
//! 3. **Each frame looked at:** unchanged since the last one (a 64×36 grey thumbnail)? done. No text (PP-OCRv6's
//!    detector alone, on the small frame)? done. Every box already seen (the same size and pixels, wherever it moved:
//!    scrolling)? done.
//! 4. **Read:** that frame decoded again, large, and the recognizer on the new boxes cut from it. A line already kept
//!    isn't kept again.

use crate::models::{self, Size};
use oar_ocr::core::config::{OrtExecutionProvider, OrtSessionConfig};
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::predictors::{TextDetectionPredictor, TextRecognitionPredictor};
use oar_ocr::processors::LimitType;
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};
use winsys::video::{At, Frame, Video};

/// A line a video shows, and when it first shows (milliseconds from the start).
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub ms: u64,
    pub text: String,
}

/// Where a line with a secret shows (left, top, right, bottom, as shares of the frame), from the frame looked at before
/// it was read until the first frame without its box (milliseconds): the preview's playing video covers it then.
#[derive(Clone, Debug, PartialEq)]
pub struct SecretBox {
    pub from_ms: u64,
    pub to_ms: u64,
    pub bounds: [f32; 4],
}

/// What reading a video found.
#[derive(Debug, Default)]
pub struct Read {
    pub shown: Vec<Shown>,
    pub secrets: Vec<SecretBox>,
    /// A secret's label showed ("Password", "PIN") with its value beside or under it: that value is covered, and
    /// isn't kept.
    pub labelled: bool,
}

/// Frames are decoded at most this large (the longer side), and lines recognized from them (as well as from 1920:
/// VIDEO.md).
const READ_SIDE: u32 = 1280;

/// The checks look at the frame shrunk to at most this (by whole steps); the detector too, as it is.
const SMALL_SIDE: u32 = 640;

/// The change thumbnail: 64×36 cells, and how much a cell's mean grey must move (0–255) to count as changed.
const THUMB: (usize, usize) = (64, 36);
const CELL_CHANGE: i32 = 6;

/// A box's own thumbnail has a cell for every pixel of the small frame (up to 320 × 24), and two boxes hold the same
/// text when their thumbnails differ by less than this on average (0–255). Coarser cells (2, 3 px) took different
/// lines of code of the same length for the same line (VIDEO.md).
const SAME_BOX_CHANGE: f32 = 12.0;

/// Lines recognized together (sorted by shape, so little of a batch is padding).
const BATCH: usize = 8;

/// What each step stopped, and the processor time (all threads) it took: for measuring (`--video-text`).
#[derive(Clone, Debug, Default)]
pub struct Steps {
    /// Keyframes asked for, and the different ones found.
    pub probes: u32,
    pub keyframes: u32,
    /// Spans between keyframes decoded at the pace, and the frames looked at in them.
    pub spans: u32,
    pub span_frames: u32,
    pub unchanged: u32,
    pub no_text: u32,
    pub same_text: u32,
    pub read: u32,
    pub boxes_read: u32,
    pub lines_kept: u32,
    pub decode: Duration,
    pub thumb: Duration,
    pub detect: Duration,
    pub compare: Duration,
    pub recognize: Duration,
}

pub struct VideoText {
    detector: TextDetectionPredictor,
    recognizer: TextRecognitionPredictor,
    small: u32,
    large: u32,
}

/// A box of text: where it is (small-frame pixels) and its grey content, a cell every few pixels.
#[derive(Clone)]
struct TextBox {
    bounds: [f32; 4],
    cells: (usize, usize),
    grey: Vec<u8>,
}

/// A frame looked at: as decoded, shrunk for the checks, its thumbnail, and its boxes once the detector has run.
struct Looked {
    frame: Frame,
    small: winsys::imaging::Rgb,
    thumb: Vec<u8>,
    boxes: Option<Vec<TextBox>>,
}

/// What a video's reading carries from frame to frame.
struct Reading {
    last_thumb: Option<Vec<u8>>,
    last_boxes: Vec<TextBox>,
    kept: HashSet<String>,
    shown: Vec<Shown>,
    /// Secrets on screen now (their box, to see when it's gone), and those that have gone.
    open: Vec<(TextBox, SecretBox)>,
    secrets: Vec<SecretBox>,
    /// Labels of a secret showing now ("Password" on a line of its own): what's beside or under them is its value.
    labels: Vec<TextBox>,
    labelled: bool,
    /// The moment of the last frame looked at before this one (ms): a secret read now may have shown since then.
    last_ms: u64,
    cpu: Duration,
}

impl Reading {
    /// The secrets whose box isn't among `boxes` (a frame's, at `ms`) have gone by then.
    fn close_gone(&mut self, boxes: &[TextBox], picture: &winsys::imaging::Rgb, ms: u64) {
        let (gone, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.open).into_iter()
            .partition(|(seen, _)| !boxes.iter().any(|now| same(seen, now, picture)));
        self.open = still;
        self.secrets.extend(gone.into_iter().map(|(_, secret)| SecretBox { to_ms: ms, ..secret }));
    }
}

impl Reading {
    /// Processor time since the last lap, added to `into`.
    fn lap(&mut self, into: &mut Duration) {
        let now = winsys::load::own_cpu();
        *into += now.saturating_sub(self.cpu);
        self.cpu = now;
    }
}

impl VideoText {
    /// `gpu`: the detector on the graphics card (DirectML); the recognizer stays on the processor.
    pub fn load(models: &Path, size: Size, threads: usize, gpu: bool) -> Result<VideoText, String> {
        let folder = models::folder(models);
        let [detector, recognizer, dictionary] = size.files();
        let session = |gpu: bool| {
            let providers = if gpu {
                vec![OrtExecutionProvider::DirectML { device_id: Some(crate::ocr::dml_device()) }, OrtExecutionProvider::CPU]
            } else {
                vec![OrtExecutionProvider::CPU]
            };
            OrtSessionConfig::new()
                .with_intra_threads(threads)
                .with_inter_threads(1)
                .with_execution_providers(providers)
                .add_config_entry("session.intra_op.allow_spinning", "0")
        };
        let (small, large) = (SMALL_SIDE, READ_SIDE);
        let detection = TextDetectionConfig {
            score_threshold: 0.2,
            box_threshold: if size == Size::Tiny { 0.4 } else { 0.45 },
            unclip_ratio: 1.4,
            max_candidates: 1000,
            limit_side_len: Some(small),
            limit_type: Some(LimitType::Max),
            ..TextDetectionConfig::default()
        };
        let detector = TextDetectionPredictor::builder()
            .with_config(detection)
            .with_ort_config(session(gpu))
            .build(folder.join(detector.name))
            .map_err(|error| error.to_string())?;
        let recognizer = TextRecognitionPredictor::builder()
            .dict_path(folder.join(dictionary.name))
            .with_ort_config(session(false))
            .build(folder.join(recognizer.name))
            .map_err(|error| error.to_string())?;
        Ok(VideoText { detector, recognizer, small, large })
    }

    /// The lines `path` shows, looked for every `every` seconds, until `stop` says so. COM must be initialized on the
    /// calling thread.
    pub fn read(&self, path: &str, every: f64, steps: &mut Steps, stop: &dyn Fn() -> bool) -> Result<Read, String> {
        let mut video = Video::open(path, self.large)?;
        let mut reading = Reading {
            last_thumb: None,
            last_boxes: Vec::new(),
            kept: HashSet::new(),
            shown: Vec::new(),
            open: Vec::new(),
            secrets: Vec::new(),
            labels: Vec::new(),
            labelled: false,
            last_ms: 0,
            cpu: winsys::load::own_cpu(),
        };
        // The last keyframe looked at: its moment, thumbnail and boxes.
        let mut previous: Option<(f64, Vec<u8>, Vec<TextBox>)> = None;
        let mut at = 0.0;
        // A video Windows can't seek in is decoded forward, a frame at the pace.
        let mut seekable = true;
        loop {
            if stop() || (video.length > 0.0 && at > video.length) {
                break;
            }
            let found = if seekable { video.keyframe(at)? } else { video.forward(at)? };
            reading.lap(&mut steps.decode);
            steps.probes += 1;
            let frame = match found {
                At::End => break,
                At::Unseekable => {
                    seekable = false;
                    continue;
                }
                At::Nothing => {
                    at += every;
                    continue;
                }
                At::Frame(frame) => frame,
            };
            // The same keyframe as last time: the next one is further on.
            if seekable && previous.as_ref().is_some_and(|(moment, _, _)| *moment == frame.seconds) {
                at += every;
                continue;
            }
            steps.keyframes += 1;
            let moment = frame.seconds;
            let mut looked = self.look(frame, &mut reading, steps)?;
            let boxes = looked.boxes.clone().unwrap_or_else(|| previous.as_ref().map(|(_, _, boxes)| boxes.clone()).unwrap_or_default());
            // Text changed between this keyframe and the last: what came between them is looked at, at the pace.
            if seekable && let Some((before, _, before_boxes)) = &previous
                && moment - before > every * 1.5
                && differ(before_boxes, &boxes, &looked.small)
            {
                steps.spans += 1;
                let (before, end) = (*before, moment);
                // (The seek lands on the earlier keyframe; the frames after it are decoded forward.)
                let mut next = before + every;
                let _ = video.at(before, 0.0)?;
                while next < end - every / 2.0 && !stop() {
                    let found = video.forward(next)?;
                    reading.lap(&mut steps.decode);
                    let At::Frame(between) = found else { break };
                    if between.seconds >= end {
                        break;
                    }
                    steps.span_frames += 1;
                    next = between.seconds + every;
                    let between = self.look(between, &mut reading, steps)?;
                    self.read_new(between, &mut reading, steps)?;
                }
                // The keyframe is compared with the last frame of the span now.
                looked = self.relook(looked, &mut reading, steps)?;
            }
            previous = Some((moment, looked.thumb.clone(), boxes));
            self.read_new(looked, &mut reading, steps)?;
            at = moment.max(at) + every;
        }
        // Secrets still showing at the end show until the end.
        let end = (video.length.max(at) * 1000.0) as u64;
        reading.secrets.extend(reading.open.drain(..).map(|(_, secret)| SecretBox { to_ms: end, ..secret }));
        Ok(Read { shown: reading.shown, secrets: reading.secrets, labelled: reading.labelled })
    }

    /// Steps 3's checks up to the detector: the thumbnail against the last frame looked at, then the detector if it
    /// changed. `boxes` stays None when it didn't.
    fn look(&self, frame: Frame, reading: &mut Reading, steps: &mut Steps) -> Result<Looked, String> {
        let small = shrink(&frame.picture, self.small);
        let (width, height) = (small.width as usize, small.height as usize);
        let thumb = grey_cells(&small.pixels, width, height, [0.0, 0.0, width as f32, height as f32], THUMB);
        let changed = reading.last_thumb.as_ref().is_none_or(|last| changed(last, &thumb));
        reading.lap(&mut steps.thumb);
        if !changed {
            return Ok(Looked { frame, small, thumb, boxes: None });
        }
        reading.last_thumb = Some(thumb.clone());
        let boxes = self.detect(&small)?;
        reading.lap(&mut steps.detect);
        Ok(Looked { frame, small, thumb, boxes: Some(boxes) })
    }

    /// A keyframe looked at before the span that came before it: compared again with the span's last frame.
    fn relook(&self, looked: Looked, reading: &mut Reading, steps: &mut Steps) -> Result<Looked, String> {
        let changed = reading.last_thumb.as_ref().is_none_or(|last| changed(last, &looked.thumb));
        if !changed {
            return Ok(Looked { boxes: None, ..looked });
        }
        reading.last_thumb = Some(looked.thumb.clone());
        match looked.boxes {
            Some(_) => Ok(looked),
            None => {
                let boxes = self.detect(&looked.small)?;
                reading.lap(&mut steps.detect);
                Ok(Looked { boxes: Some(boxes), ..looked })
            }
        }
    }

    fn detect(&self, picture: &winsys::imaging::Rgb) -> Result<Vec<TextBox>, String> {
        let (width, height) = (picture.width as usize, picture.height as usize);
        let image = image::RgbImage::from_raw(picture.width, picture.height, picture.pixels.clone()).ok_or("a frame of the wrong size")?;
        let found = self.detector.predict(vec![image]).map_err(|error| error.to_string())?.detections.into_iter().next().unwrap_or_default();
        Ok(found
            .iter()
            .filter_map(|detection| {
                let points = &detection.bbox.points;
                let bounds = [
                    points.iter().map(|p| p.x).fold(f32::MAX, f32::min).max(0.0),
                    points.iter().map(|p| p.y).fold(f32::MAX, f32::min).max(0.0),
                    points.iter().map(|p| p.x).fold(f32::MIN, f32::max).min(width as f32),
                    points.iter().map(|p| p.y).fold(f32::MIN, f32::max).min(height as f32),
                ];
                let (w, h) = (bounds[2] - bounds[0], bounds[3] - bounds[1]);
                if w < 3.0 || h < 3.0 {
                    return None;
                }
                let cells = ((w.round() as usize).clamp(4, 320), (h.round() as usize).clamp(2, 24));
                Some(TextBox { bounds, cells, grey: grey_cells(&picture.pixels, width, height, bounds, cells) })
            })
            .collect())
    }

    /// Step 3's last check and step 4: the boxes not seen in the last frame with text, read from the frame decoded
    /// large.
    fn read_new(&self, looked: Looked, reading: &mut Reading, steps: &mut Steps) -> Result<(), String> {
        let since = std::mem::replace(&mut reading.last_ms, (looked.frame.seconds * 1000.0).max(0.0) as u64);
        let Some(boxes) = looked.boxes else {
            steps.unchanged += 1;
            return Ok(());
        };
        let ms = (looked.frame.seconds * 1000.0).max(0.0) as u64;
        reading.close_gone(&boxes, &looked.small, ms);
        if boxes.is_empty() {
            steps.no_text += 1;
            reading.last_boxes.clear();
            return Ok(());
        }
        let picture = &looked.small;
        let new: Vec<TextBox> = boxes.iter().filter(|now| !reading.last_boxes.iter().any(|seen| same(seen, now, picture))).cloned().collect();
        reading.last_boxes = boxes;
        reading.lap(&mut steps.compare);
        if new.is_empty() {
            steps.same_text += 1;
            return Ok(());
        }

        let scale = looked.frame.picture.width as f32 / looked.small.width as f32;
        let large = image::RgbImage::from_raw(looked.frame.picture.width, looked.frame.picture.height, looked.frame.picture.pixels)
            .ok_or("a frame of the wrong size")?;

        steps.read += 1;
        steps.boxes_read += new.len() as u32;
        // Sorted by shape and read a few at a time, so a batch's lines are padded to nearly their own width.
        let mut crops: Vec<(image::RgbImage, &TextBox)> = new.iter().map(|text_box| (crop(&large, text_box.bounds.map(|value| value * scale)), text_box)).collect();
        crops.sort_by(|(a, _), (b, _)| (a.width() as f32 / a.height() as f32).total_cmp(&(b.width() as f32 / b.height() as f32)));
        let (small_w, small_h) = (looked.small.width as f32, looked.small.height as f32);
        let share = |b: [f32; 4]| [b[0] / small_w, b[1] / small_h, b[2] / small_w, b[3] / small_h];
        // All read first: a label read now covers what's beside or under it, however well that was read.
        let mut lines: Vec<(TextBox, String, f32)> = Vec::new();
        for batch in crops.chunks(BATCH) {
            let read = self.recognizer.predict(batch.iter().map(|(crop, _)| crop.clone()).collect()).map_err(|error| error.to_string())?;
            for ((text, score), (_, text_box)) in read.texts.into_iter().zip(read.scores).zip(batch) {
                lines.push(((*text_box).clone(), text.trim().to_string(), score));
            }
        }
        // The labels showing: those still on screen, and those just read.
        let on_screen = &reading.last_boxes;
        reading.labels.retain(|label| on_screen.iter().any(|now| same(label, now, picture)));
        let mut new_labels = Vec::new();
        for (text_box, text, score) in &lines {
            if *score >= 0.5 && privacy::label_only(text) && !reading.labels.iter().any(|label| same(label, text_box, picture)) {
                reading.labels.push(text_box.clone());
                new_labels.push(text_box.clone());
            }
        }
        for (text_box, text, score) in lines {
            // A label's value: covered while it shows, and not kept.
            if !privacy::label_only(&text) && reading.labels.iter().any(|label| next_to(label, &text_box)) {
                reading.labelled = true;
                let bounds = share(text_box.bounds);
                reading.open.push((text_box, SecretBox { from_ms: since.min(ms), to_ms: ms, bounds }));
                continue;
            }
            if score < 0.5 || text.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
                continue;
            }
            // A secret on screen: its box is covered while it shows.
            if !crate::work::private(&text).1.is_empty() {
                let bounds = share(text_box.bounds);
                reading.open.push((text_box, SecretBox { from_ms: since.min(ms), to_ms: ms, bounds }));
            }
            if reading.kept.insert(folded(&text)) {
                steps.lines_kept += 1;
                reading.shown.push(Shown { ms, text });
            }
        }
        // A label with nothing found beside or under it (its value too faint, or too small, to be found): where the
        // value would show is covered while the label shows.
        for label in new_labels {
            if !reading.last_boxes.iter().any(|now| next_to(&label, now)) {
                reading.labelled = true;
                let bounds = share(value_area(&label, (small_w, small_h)));
                reading.open.push((label, SecretBox { from_ms: since.min(ms), to_ms: ms, bounds }));
            }
        }
        reading.lap(&mut steps.recognize);
        Ok(())
    }
}

/// Whether `found` is where a label's value shows: after it on its line, or on the line under it.
fn next_to(label: &TextBox, found: &TextBox) -> bool {
    let [left, top, right, bottom] = label.bounds;
    let tall = (bottom - top).max(1.0);
    let [found_left, found_top, found_right, found_bottom] = found.bounds;
    let middle = |top: f32, bottom: f32| (top + bottom) / 2.0;
    let after = (middle(found_top, found_bottom) - middle(top, bottom)).abs() < 0.6 * tall
        && found_left >= right - 0.5 * tall && found_left - right < 6.0 * tall;
    let under = found_top >= bottom - 0.4 * tall && found_top - bottom < 1.8 * tall
        && found_left < right + 2.0 * tall && found_right > left - 2.0 * tall;
    after || under
}

/// Where a label's value would show: the rest of its line and the line under it (small-frame pixels).
fn value_area(label: &TextBox, (width, height): (f32, f32)) -> [f32; 4] {
    let [left, top, right, bottom] = label.bounds;
    let tall = bottom - top;
    [left, top, (right + 12.0 * tall).min(width), (bottom + 1.8 * tall).min(height)]
}

/// A frame shrunk by whole steps (averaging each block) so its longer side is at most `side`.
fn shrink(picture: &winsys::imaging::Rgb, side: u32) -> winsys::imaging::Rgb {
    let (width, height) = (picture.width as usize, picture.height as usize);
    let step = width.max(height).div_ceil(side as usize).max(1);
    if step == 1 {
        return winsys::imaging::Rgb { pixels: picture.pixels.clone(), ..*picture };
    }
    let (out_width, out_height) = (width / step, height / step);
    let mut pixels = vec![0u8; out_width * out_height * 3];
    let mut sums = vec![0u32; out_width * 3];
    let count = (step * step) as u32;
    for y in 0..out_height {
        sums.fill(0);
        for dy in 0..step {
            let row = &picture.pixels[(y * step + dy) * width * 3..][..out_width * step * 3];
            for (x, block) in row.chunks_exact(step * 3).enumerate() {
                for pixel in block.chunks_exact(3) {
                    sums[x * 3] += u32::from(pixel[0]);
                    sums[x * 3 + 1] += u32::from(pixel[1]);
                    sums[x * 3 + 2] += u32::from(pixel[2]);
                }
            }
        }
        for (out, sum) in pixels[y * out_width * 3..][..out_width * 3].iter_mut().zip(&sums) {
            *out = (sum / count) as u8;
        }
    }
    winsys::imaging::Rgb { width: out_width as u32, height: out_height as u32, pixels, original: picture.original }
}

/// Whether any cell of a thumbnail moved.
fn changed(last: &[u8], now: &[u8]) -> bool {
    last.iter().zip(now).any(|(a, b)| (*a as i32 - *b as i32).abs() > CELL_CHANGE)
}

/// Whether a keyframe's text differs from the last one's: a box in it that the other hasn't, or one gone.
fn differ(before: &[TextBox], now: &[TextBox], frame: &winsys::imaging::Rgb) -> bool {
    now.iter().any(|box_| !before.iter().any(|seen| same(seen, box_, frame)))
        || before.iter().any(|seen| !now.iter().any(|box_| same(seen, box_, frame)))
}

/// Lowercase letters and digits only: the same line read twice with a stray mark is one line.
fn folded(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The same text: nearly the same size (the detector's boxes wobble by a pixel or two), and the same pixels inside
/// when the new box is sampled on the old one's grid, wherever it is (a line scrolled up is still the line).
fn same(seen: &TextBox, now: &TextBox, frame: &winsys::imaging::Rgb) -> bool {
    let size = |bounds: &[f32; 4]| (bounds[2] - bounds[0], bounds[3] - bounds[1]);
    let ((seen_w, seen_h), (now_w, now_h)) = (size(&seen.bounds), size(&now.bounds));
    if (seen_w - now_w).abs() > (seen_w * 0.04).max(4.0) || (seen_h - now_h).abs() > (seen_h * 0.2).max(3.0) {
        return false;
    }
    let differs = |grey: &[u8]| {
        let difference: u32 = seen.grey.iter().zip(grey).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs()).sum();
        difference as f32 / seen.grey.len().max(1) as f32
    };
    if seen.cells == now.cells && differs(&now.grey) < SAME_BOX_CHANGE {
        return true;
    }
    // Sampled on the old box's grid, its size, from where the new box is and a pixel or two around (the strokes of
    // small text are as fine as the grid, so a box a pixel off reads as different text).
    let [left, top, ..] = now.bounds;
    let (width, height) = (frame.width as usize, frame.height as usize);
    SHIFTS.iter().any(|&(dx, dy)| {
        let (x, y) = (left + dx, top + dy);
        let bounds = [x, y, x + seen_w, y + seen_h];
        differs(&grey_cells(&frame.pixels, width, height, bounds, seen.cells)) < SAME_BOX_CHANGE
    })
}

/// Where a box is looked for around where it's found (pixels across, down).
const SHIFTS: [(f32, f32); 7] = [(0.0, 0.0), (-1.0, 0.0), (1.0, 0.0), (-2.0, 0.0), (2.0, 0.0), (0.0, -1.0), (0.0, 1.0)];

/// A box cut from the frame, with a little room around it (the detector's boxes are tight).
fn crop(image: &image::RgbImage, [left, top, right, bottom]: [f32; 4]) -> image::RgbImage {
    let pad = ((bottom - top) * 0.15).max(2.0);
    let x = (left - pad).max(0.0) as u32;
    let y = (top - pad).max(0.0) as u32;
    let w = ((right + pad).min(image.width() as f32) as u32).saturating_sub(x).max(1);
    let h = ((bottom + pad).min(image.height() as f32) as u32).saturating_sub(y).max(1);
    image::imageops::crop_imm(image, x, y, w, h).to_image()
}

/// The mean grey of each of `cols`×`rows` cells of a region of an RGB picture, sampling every other pixel each way
/// when the cells are large enough.
fn grey_cells(pixels: &[u8], width: usize, height: usize, [left, top, right, bottom]: [f32; 4], (cols, rows): (usize, usize)) -> Vec<u8> {
    let (x0, y0) = (left.max(0.0) as usize, top.max(0.0) as usize);
    let (x1, y1) = ((right as usize).min(width), (bottom as usize).min(height));
    let mut out = vec![0u8; cols * rows];
    if x1 <= x0 || y1 <= y0 {
        return out;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let step = if w >= cols * 4 && h >= rows * 4 { 2 } else { 1 };
    for row in 0..rows {
        let (ya, yb) = (y0 + row * h / rows, (y0 + (row + 1) * h / rows).max(y0 + row * h / rows + 1).min(y1));
        for col in 0..cols {
            let (xa, xb) = (x0 + col * w / cols, (x0 + (col + 1) * w / cols).max(x0 + col * w / cols + 1).min(x1));
            let (mut sum, mut count) = (0u32, 0u32);
            for y in (ya..yb).step_by(step) {
                let line = &pixels[(y * width + xa) * 3..(y * width + xb) * 3];
                for pixel in line.chunks_exact(3).step_by(step) {
                    sum += (pixel[0] as u32 * 77 + pixel[1] as u32 * 150 + pixel[2] as u32 * 29) >> 8;
                    count += 1;
                }
            }
            out[row * cols + col] = (sum / count.max(1)) as u8;
        }
    }
    out
}

/// `--video-text <video> [seconds between candidates]`: reads one video and prints each step's share and processor
/// time, and the lines with their moments, as JSON (for scoring against a transcript). `EXCAVATE_VIDEO_GPU` puts the
/// detector on the graphics card; `EXCAVATE_VIDEO_TINY` uses the tiny models; `EXCAVATE_OCR_THREADS` sets the threads.
pub fn measure(paths: &config::Paths) {
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|arg| arg == "--video-text").unwrap_or(0);
    let Some(video) = args.get(at + 1) else {
        eprintln!("--video-text <video> [every]");
        return;
    };
    let every: f64 = args.get(at + 2).and_then(|every| every.parse().ok()).unwrap_or(2.0);
    let size = if std::env::var_os("EXCAVATE_VIDEO_TINY").is_some() { Size::Tiny } else { Size::Small };
    let gpu = std::env::var_os("EXCAVATE_VIDEO_GPU").is_some();
    let threads = crate::ocr::threads();
    winsys::com_mta();
    let before = winsys::load::own_cpu();
    let reader = match VideoText::load(&paths.models(), size, threads, gpu) {
        Ok(reader) => reader,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    let loaded = winsys::load::own_cpu();
    let started = Instant::now();
    let mut steps = Steps::default();
    let shown = reader.read(video, every, &mut steps, &|| false).map(|read| read.shown);
    let wall = started.elapsed();
    let used = winsys::load::own_cpu().saturating_sub(loaded);
    let length = winsys::video::duration(video).unwrap_or(0.0);
    let shown = match shown {
        Ok(shown) => shown,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    let s = &steps;
    let report = serde_json::json!({
        "video": video, "every": every, "seconds": length, "threads": threads, "size": size.label(), "gpu": gpu,
        "small": reader.small, "large": reader.large, "load_cpu_s": loaded.saturating_sub(before).as_secs_f64(),
        "wall_s": wall.as_secs_f64(), "cpu_s": used.as_secs_f64(), "cpu_s_per_hour": used.as_secs_f64() * 3600.0 / length.max(1.0),
        "probes": s.probes, "keyframes": s.keyframes, "spans": s.spans, "span_frames": s.span_frames,
        "unchanged": s.unchanged, "no_text": s.no_text, "same_text": s.same_text,
        "read": s.read, "boxes_read": s.boxes_read, "lines_kept": s.lines_kept,
        "cpu_ms": {
            "decode": ms(s.decode), "thumb": ms(s.thumb), "detect": ms(s.detect), "compare": ms(s.compare),
            "recognize": ms(s.recognize),
        },
        "shown": shown.iter().map(|line| serde_json::json!([line.ms, line.text])).collect::<Vec<_>>(),
    });
    println!("{report}");
}

// ---- In the indexer: the images' lane reads videos' text once the images are read -------------------------------

/// What the catalog records as having read a video's text.
pub const ENGINE: &str = "video-text";

/// Reads one video's text and hands its lines to the index, each with its moment (`marks` as a recording's). Says
/// whether to go on (false: the indexer is stopping). A video stopped partway (the PC got busy, the schedule closed)
/// is read again from its start later.
pub fn read_one(context: &crate::work::Context, catalog: &crate::catalog::Catalog, models: &Path, size: Size, reader: &mut Option<VideoText>,
                (path, bytes, modified): (String, u64, i64)) -> bool {
    use crate::work::{self, Need, Write};
    if !crate::scan::in_scope(&path, &context.roots, &context.exclusions) || !context.kinds.on(&path, "video-text") {
        catalog.seen_set(&path, ENGINE, modified, work::now(), false);
        return true;
    }
    let _busy = work::Busy::new(&context.shared);
    if !work::wait_until_free(context, Need::Cpu) {
        return false;
    }
    if reader.is_none() {
        // (The images' reader brought these models; until it has, videos wait.)
        if !models::present(models, size) {
            return context.control.sleep(Duration::from_secs(60));
        }
        // Where it was chosen to run (`Context::video_text_gpu`).
        match VideoText::load(models, size, 1, context.video_text_gpu) {
            Ok(loaded) => *reader = Some(loaded),
            Err(error) => {
                config::log(&format!("text in videos: {error}"));
                return context.control.sleep(Duration::from_secs(600));
            }
        }
    }
    let Some(open) = reader.as_ref() else { return true };
    if let Some(reading) = context.shared.status().video_text.as_mut() {
        reading.current = path.clone();
    }
    let started = Instant::now();
    let skip = || context.control.skip_asked(&path);
    let stopping = || context.control.stopping() || !context.gate.open() || work::pause_reason(context, Need::Cpu).is_some() || skip();
    let mut steps = Steps::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open.read(&path, context.video_text_pace, &mut steps, &stopping)))
        .unwrap_or_else(|_| Err("the reader stopped on this video".into()));
    if let Some(reading) = context.shared.status().video_text.as_mut() {
        reading.current.clear();
    }
    // Let go by "Skip": among what couldn't be read, and not read again until it changes.
    if skip() {
        context.control.skipped();
        config::log(&format!("text in videos: {path}: skipped"));
        catalog.seen_set(&path, ENGINE, modified, work::now(), false);
        catalog.skipped_add(&path, "text in videos");
        return true;
    }
    if stopping() {
        return !context.control.stopping();
    }
    match result {
        Ok(read) => {
            let lines = read.shown.len();
            match doc(&path, bytes, modified, read.shown, &read.secrets, read.labelled) {
                Some(doc) => {
                    let _ = context.sender.send(Write::Words(Box::new(doc)));
                }
                None => {
                    let _ = context.sender.send(Write::Unindex(content::entry_id(&path, false)));
                }
            }
            catalog.seen_set(&path, ENGINE, modified, work::now(), true);
            config::log(&format!("text in videos: {path}: {lines} lines in {:.1} s ({} keyframes, {} between, {} read)",
                                 started.elapsed().as_secs_f64(), steps.keyframes, steps.span_frames, steps.read));
        }
        Err(error) => {
            config::log(&format!("text in videos, {path}: {error}"));
            catalog.seen_set(&path, ENGINE, modified, work::now(), false);
        }
    }
    true
}

/// A video's lines as the index keeps them: one a line, with where each starts and its moment, secrets covered.
fn doc(path: &str, size: u64, modified: i64, shown: Vec<Shown>, boxes: &[SecretBox], labelled: bool) -> Option<content::Doc> {
    if shown.is_empty() && boxes.is_empty() {
        return None;
    }
    let mut secrets = Vec::new();
    let mut body = String::new();
    let mut times = Vec::with_capacity(shown.len());
    for line in shown {
        let (text, found) = crate::work::private(&line.text);
        secrets.extend(found);
        if !body.is_empty() {
            body.push('\n');
        }
        times.push((body.len(), line.ms as i64));
        body.push_str(&text);
    }
    let mut marks = serde_json::json!({ "times": times });
    let mut kinds = crate::work::kinds(&secrets);
    // (A label's value isn't kept, so it isn't among them: the label says what it was.)
    if labelled && !kinds.contains(&"password") {
        kinds.push("password");
        kinds.sort_unstable();
    }
    if !kinds.is_empty() {
        marks["kinds"] = kinds.into();
    }
    // Where secrets show, and when: [from ms, to ms, left, top, right, bottom], the box in ten-thousandths of the frame.
    if !boxes.is_empty() {
        let share = |value: f32| (value.clamp(0.0, 1.0) * 10_000.0).round() as i64;
        marks["video"] = serde_json::json!({ "boxes": boxes.iter().map(|b| {
            [b.from_ms as i64, b.to_ms as i64, share(b.bounds[0]), share(b.bounds[1]), share(b.bounds[2]), share(b.bounds[3])]
        }).collect::<Vec<_>>() });
    }
    Some(content::Doc {
        path: path.to_string(),
        name: path.rsplit('\\').next().unwrap_or(path).to_string(),
        kind: content::kind::VIDEO,
        modified,
        size,
        source: content::Source::Video,
        body,
        marks: serde_json::to_vec(&marks).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_kept_with_their_moments() {
        let shown = vec![Shown { ms: 1500, text: "Quarterly results".into() }, Shown { ms: 64_000, text: "Next steps".into() }];
        let doc = doc(r"C:\Videos\talk.mp4", 9, 4, shown, &[], false).expect("a doc");
        assert_eq!(doc.body, "Quarterly results\nNext steps");
        assert_eq!(doc.source, content::Source::Video);
        assert_eq!(content::moment_of(&doc.marks, 20), Some(64.0));
        assert_eq!(content::moment_of(&doc.marks, 3), Some(1.5));
        assert!(super::doc("x.mp4", 1, 1, Vec::new(), &[], false).is_none());
    }

    #[test]
    fn secrets_kept_with_where_and_when() {
        let shown = vec![Shown { ms: 2000, text: "password: hunter22".into() }];
        let boxes = [SecretBox { from_ms: 2000, to_ms: 9000, bounds: [0.1, 0.5, 0.6, 0.55] }];
        let doc = doc(r"C:\Videos\demo.mp4", 9, 4, shown, &boxes, false).expect("a doc");
        let marks: serde_json::Value = serde_json::from_slice(&doc.marks).unwrap();
        assert_eq!(marks["video"]["boxes"], serde_json::json!([[2000, 9000, 1000, 5000, 6000, 5500]]));
        // A label alone, its value covered and not kept: the kind is still said.
        let shown = vec![Shown { ms: 0, text: "Password".into() }];
        let doc = super::doc(r"C:\Videos\wifi.mp4", 9, 4, shown, &boxes, true).expect("a doc");
        let marks: serde_json::Value = serde_json::from_slice(&doc.marks).unwrap();
        assert_eq!(marks["kinds"], serde_json::json!(["password"]));
    }
}
