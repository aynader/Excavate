//! Whole-image sensitivity: an image whose vector is close to "a passport", "an ID card", "a driver's license" or "a bank
//! card" (and further from ordinary descriptions: "a photo", "a receipt", "a poster"…) is blurred whole in the preview
//! until Reveal, even where its text read nothing. Zero-shot: the descriptions' vectors were made once by each model's
//! own text half (`excavate-indexer --seeing-bench`) and are kept here, so the reader needs no text model to judge.
//!
//! An image's score is its best fit among the sensitive descriptions less its best among the ordinary ones. The
//! thresholds come from the privacy benchmark: public specimen documents against COCO's and TextVQA's photos
//! (PRIVACY.md has the figures). Made with `sensitivity.py bake`.

use crate::Engine;
use std::sync::OnceLock;

/// What an image shows that's blurred whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shows {
    /// A passport, an ID card or a driving licence.
    Document,
    /// A bank card.
    Card,
}

/// The descriptions, in the order `sensitive-*.bin` keeps them (the first `SENSITIVE.len()` sensitive, then the
/// ordinary ones). Changing them means making the files again.
pub const SENSITIVE: [(&str, Shows); 7] = [
    ("a passport", Shows::Document),
    ("a passport data page", Shows::Document),
    ("an ID card", Shows::Document),
    ("an identity card with a photo", Shows::Document),
    ("a driver's license", Shows::Document),
    ("a bank card", Shows::Card),
    ("a credit card", Shows::Card),
];
pub const ORDINARY: [&str; 23] = ["a photo", "a screenshot", "a document", "a page of text", "a receipt", "a poster", "a sign",
    "a book cover", "a business card", "a ticket", "a menu", "a label", "a magazine", "a map", "a certificate", "a license plate",
    "a car", "a person", "a phone", "a laptop", "a book", "a photo of objects on a table", "a card"];

/// The score from which an image is blurred whole, for each model (none: not judged).
fn threshold(engine: Engine) -> Option<f32> {
    match engine {
        Engine::Siglip => Some(SIGLIP_THRESHOLD),
        Engine::Gemma => Some(GEMMA_THRESHOLD),
        Engine::Qwen | Engine::GemmaText => None,
    }
}

// Each blurs 1 of 1,000 ordinary photos (COCO and TextVQA, 8,166) and 96% of the specimens and the benchmark's cards and
// passports (224). Chosen the same way on half the photos, the other half had 0.5 to 2.4 per 1,000 blurred.
const SIGLIP_THRESHOLD: f32 = 0.0095;
const GEMMA_THRESHOLD: f32 = 0.0118;

struct Descriptions {
    sensitive: Vec<Vec<f32>>,
    ordinary: Vec<Vec<f32>>,
}

/// `count × (scale: f32, dims × i8)`, little-endian, as `crate::quantize` makes them.
fn decode(bytes: &[u8], dims: usize) -> Option<Descriptions> {
    let record = 4 + dims;
    if bytes.len() != record * (SENSITIVE.len() + ORDINARY.len()) {
        return None;
    }
    let vectors: Vec<Vec<f32>> = bytes.chunks(record).map(|chunk| {
        let scale = f32::from_le_bytes(chunk[..4].try_into().unwrap());
        chunk[4..].iter().map(|&value| f32::from(value as i8) * scale).collect()
    }).collect();
    let (sensitive, ordinary) = vectors.split_at(SENSITIVE.len());
    Some(Descriptions { sensitive: sensitive.to_vec(), ordinary: ordinary.to_vec() })
}

fn descriptions(engine: Engine) -> Option<&'static Descriptions> {
    static SIGLIP: OnceLock<Option<Descriptions>> = OnceLock::new();
    static GEMMA: OnceLock<Option<Descriptions>> = OnceLock::new();
    match engine {
        Engine::Siglip => SIGLIP.get_or_init(|| decode(include_bytes!("sensitive-siglip2-b16.bin"), Engine::Siglip.dims())).as_ref(),
        Engine::Gemma => GEMMA.get_or_init(|| decode(include_bytes!("sensitive-eg2-140.bin"), Engine::Gemma.dims())).as_ref(),
        Engine::Qwen | Engine::GemmaText => None,
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The image's score and what it fits best; none for a model that isn't judged, or a vector of the wrong length.
pub fn score(engine: Engine, vector: &[f32]) -> Option<(f32, Shows)> {
    let descriptions = descriptions(engine)?;
    if vector.len() != engine.dims() {
        return None;
    }
    let vector = crate::normalized(vector);
    let (best, shows) = descriptions.sensitive.iter().zip(SENSITIVE).map(|(description, (_, shows))| (dot(&vector, description), shows))
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    let ordinary = descriptions.ordinary.iter().map(|description| dot(&vector, description)).fold(f32::MIN, f32::max);
    Some((best - ordinary, shows))
}

/// What the image shows that blurs it whole, if anything.
pub fn shows(engine: Engine, vector: &[f32]) -> Option<Shows> {
    let threshold = threshold(engine)?;
    score(engine, vector).filter(|(score, _)| *score >= threshold).map(|(_, shows)| shows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_are_there() {
        for engine in [Engine::Siglip, Engine::Gemma] {
            let descriptions = descriptions(engine).expect("its descriptions");
            assert!(descriptions.sensitive.iter().chain(&descriptions.ordinary).all(|vector| (dot(vector, vector) - 1.0).abs() < 0.02));
            // A description's own vector fits itself best.
            let passport = descriptions.sensitive[0].clone();
            assert_eq!(score(engine, &passport).map(|(_, shows)| shows), Some(Shows::Document));
            let card = descriptions.sensitive[5].clone();
            assert_eq!(score(engine, &card).map(|(_, shows)| shows), Some(Shows::Card));
            let photo = descriptions.ordinary[0].clone();
            assert!(shows(engine, &photo).is_none());
        }
        assert!(score(Engine::Qwen, &vec![0.0; Engine::Qwen.dims()]).is_none());
        assert!(score(Engine::Siglip, &[1.0, 0.0]).is_none());
    }
}
