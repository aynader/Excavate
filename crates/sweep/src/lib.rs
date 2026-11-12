//! The first sweep for large libraries: which images are worth reading first. In date order, a PC with 121,000 images would
//! spend days on game assets, model exports and video frames before reaching what the person would search for. So they're
//! grouped first, from what's already known of each (its folder, name, size and date), with no model and no disk:
//!
//! - **Folders of one kind** (`frame_0001.png`, thousands of tiny assets, whole branches of them without a photo in sight)
//!   are sampled: a few images stand for the rest. An image with a camera's or a screenshot's name is never set aside.
//! - **Bursts** (the same folder, moments apart, about the same size) are candidates for near duplicates: the indexer looks
//!   at each with a small perceptual hash before setting it aside.
//! - **Twins** (images of exactly the same size) may be exact copies: the indexer compares their contents, and reads one.
//!
//! Everything else goes first, and the rest is read later, when the PC has time. Nothing is ever skipped for good.

use std::collections::HashMap;

/// With fewer images than this there's no need for any of it: they're read in date order.
pub const LARGE: usize = 2_000;

/// A burst: moments apart, sizes within this factor.
const BURST_SECONDS: i64 = 8;
const BURST_SIZES: f64 = 1.6;

/// Tiny files are icons and assets, rarely what anyone searches for by what's in them.
const TINY: u64 = 20 * 1024;
/// A photo, if it has no better name: a JPEG or HEIC of at least this size.
const PHOTO: u64 = 500 * 1024;

/// A folder is one of a kind when this share of its images are tiny, from this many on…
const TINY_SET_FROM: usize = 30;
const SET_SHARE: f64 = 0.6;
/// …or numbered alike, from this many on: a hundred numbered notes are worth a minute, thousands of frames aren't.
const NUMBERED_SET_FROM: usize = 100;
/// A folder is the person's own when at least this share of its images have a camera's or screenshot's name, or are photos.
const OWN_SHARE: f64 = 0.5;
/// A branch of folders is a collection from this many images on, when under this share are the person's own…
const TREE_FROM: usize = 1_000;
const TREE_OWN_SHARE: f64 = 0.25;
/// …or from this many, when at least this share are tiny.
const TINY_TREE_FROM: usize = 300;
const TINY_TREE_SHARE: f64 = 0.6;
/// Inside a collection, a branch of up to this many images is sampled as one.
const UNIT: usize = 400;
const LOOSE: usize = 30;
/// How many images stand for a sampled folder or branch, at most.
const SAMPLES: usize = 12;

#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    pub path: &'a str,
    pub size: u64,
    pub modified: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// Read after the first sweep.
    pub later: bool,
    /// The group it belongs to (0: none): a sampled folder.
    pub group: u32,
    /// For one that stands for others: how many.
    pub similar: u32,
    /// In a burst (0: none): the indexer looks before setting it aside.
    pub burst: u32,
    /// Another image has exactly its size: it may be a copy.
    pub twin: bool,
}

/// How many of the images go in the first sweep.
pub fn first_sweep(places: &[Place]) -> usize {
    places.iter().filter(|place| !place.later).count()
}

/// Where each image goes.
pub fn places(images: &[Image]) -> Vec<Place> {
    let mut places = vec![Place::default(); images.len()];
    let mut sizes: HashMap<u64, u32> = HashMap::with_capacity(images.len());
    for image in images {
        *sizes.entry(image.size).or_default() += 1;
    }
    for (place, image) in places.iter_mut().zip(images) {
        place.twin = image.size >= 1024 && sizes[&image.size] > 1;
    }
    if images.len() < LARGE {
        return places;
    }

    let mut folders: HashMap<&str, Vec<u32>> = HashMap::new();
    for (index, image) in images.iter().enumerate() {
        folders.entry(folder_of(image.path)).or_default().push(index as u32);
    }
    let trees = trees(images);
    // What's sampled together: a folder of one kind, or a branch of a collection.
    let mut units: HashMap<&str, Vec<u32>> = HashMap::new();
    let mut burst = 0;
    for (&folder, members) in &folders {
        let own = share(members, |&index| is_own(&images[index as usize])) >= OWN_SHARE;
        if !own && let Some(unit) = unit_of(folder, &trees) {
            units.entry(unit).or_default().extend_from_slice(members);
        } else if !own && is_set(members, images) {
            units.entry(folder).or_default().extend_from_slice(members);
        } else {
            burst = bursts(members, images, burst, &mut places);
        }
    }
    for (group, members) in units.values().enumerate() {
        sample(members, images, group as u32 + 1, &mut places);
    }
    places
}

fn folder_of(path: &str) -> &str {
    path.rfind('\\').map_or("", |at| &path[..at])
}

fn name_of(path: &str) -> &str {
    path.rfind('\\').map_or(path, |at| &path[at + 1..])
}

fn share(members: &[u32], test: impl Fn(&u32) -> bool) -> f64 {
    members.iter().filter(|member| test(member)).count() as f64 / members.len().max(1) as f64
}

/// A photo or a screenshot of the person's, by its name or by what it is.
fn is_own(image: &Image) -> bool {
    has_own_name(image.path) || {
        let name = name_of(image.path).to_lowercase();
        let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
        image.size >= PHOTO && matches!(extension, "jpg" | "jpeg" | "heic" | "heif")
    }
}

/// A camera's or a screenshot's name.
fn has_own_name(path: &str) -> bool {
    let name = name_of(path).to_lowercase();
    const NAMES: [&str; 18] = [
        "img", "dsc", "pxl", "mvimg", "photo", "foto", "screenshot", "screen shot", "bildschirmfoto", "schermata", "captura", "capture",
        "snip", "whatsapp", "signal-", "telegram", "gopr", "dji",
    ];
    if NAMES.iter().any(|start| name.starts_with(start)) {
        return true;
    }
    // "20230512_123456.jpg": a date for a name.
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
    stem.chars().filter(char::is_ascii_digit).count() >= 8 && stem.chars().all(|c| c.is_ascii_digit() || matches!(c, '_' | '-' | ' ' | '.'))
}

/// The name without its numbers: `frame_0001.png` and `frame_0002.png` are the same.
fn template(path: &str) -> String {
    let mut out = String::new();
    let mut in_number = false;
    for c in name_of(path).chars().flat_map(char::to_lowercase) {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('#');
            }
            in_number = true;
        } else {
            out.push(c);
            in_number = false;
        }
    }
    out
}

/// Numbered alike, or mostly tiny.
fn is_set(members: &[u32], images: &[Image]) -> bool {
    if members.len() < TINY_SET_FROM {
        return false;
    }
    if share(members, |&index| images[index as usize].size < TINY) >= SET_SHARE {
        return true;
    }
    if members.len() < NUMBERED_SET_FROM {
        return false;
    }
    let mut templates: HashMap<String, usize> = HashMap::new();
    for &index in members {
        *templates.entry(template(images[index as usize].path)).or_default() += 1;
    }
    let most = templates.values().copied().max().unwrap_or(0);
    most as f64 / members.len() as f64 >= SET_SHARE
}

#[derive(Default)]
struct Tree {
    /// Images in it and below.
    count: usize,
    /// Those not yet in a collection below it: how many, how many the person's own, how many tiny.
    left: usize,
    own: usize,
    tiny: usize,
    collection: bool,
}

/// Every folder's tree, with the collections marked. Judged from the bottom up, so a collection is the smallest branch that
/// shows it, and a folder above one isn't a collection just for having it inside: a handful of images beside a project
/// full of assets stays out of it.
fn trees<'a>(images: &[Image<'a>]) -> HashMap<&'a str, Tree> {
    let mut trees: HashMap<&'a str, Tree> = HashMap::new();
    for image in images {
        let folder = folder_of(image.path);
        let tree = trees.entry(folder).or_default();
        tree.left += 1;
        tree.own += usize::from(is_own(image));
        tree.tiny += usize::from(image.size < TINY);
        // Every folder above it exists too, and counts it.
        let mut at = folder;
        while !at.is_empty() {
            trees.entry(at).or_default().count += 1;
            at = folder_of(at);
        }
    }
    let mut deepest_first: Vec<&'a str> = trees.keys().copied().collect();
    deepest_first.sort_by_key(|folder| std::cmp::Reverse(folder.matches('\\').count()));
    for folder in deepest_first {
        let tree = &trees[folder];
        let (left, own, tiny) = (tree.left, tree.own, tree.tiny);
        let collection = (left >= TREE_FROM && (own as f64 / left as f64) < TREE_OWN_SHARE)
            || (left >= TINY_TREE_FROM && tiny as f64 / left as f64 >= TINY_TREE_SHARE);
        if let Some(tree) = trees.get_mut(folder) {
            tree.collection = collection;
        }
        // What isn't in a collection yet is judged with the folder above.
        let parent = folder_of(folder);
        if !collection && !parent.is_empty() && let Some(above) = trees.get_mut(parent) {
            above.left += left;
            above.own += own;
            above.tiny += tiny;
        }
    }
    trees
}

/// The branch a folder is sampled with, when it's in a collection: the largest of no more than `UNIT` images, inside it.
fn unit_of<'a>(folder: &'a str, trees: &HashMap<&'a str, Tree>) -> Option<&'a str> {
    let mut at = folder;
    let mut found = false;
    while !at.is_empty() {
        if trees.get(at).is_some_and(|tree| tree.collection) {
            found = true;
            break;
        }
        at = folder_of(at);
    }
    if !found {
        return None;
    }
    let top = at;
    let mut unit = folder;
    while unit != top {
        let parent = folder_of(unit);
        if trees.get(parent).is_none_or(|tree| tree.count > UNIT) {
            break;
        }
        unit = parent;
    }
    // A small branch among many goes with them, or a tree of little folders would be read folder by folder.
    if unit != top && trees.get(unit).is_some_and(|tree| tree.count < LOOSE) {
        unit = folder_of(unit);
    }
    Some(unit)
}

/// A few images, evenly spaced in name order, stand for the rest, which come later. A camera's or a screenshot's name is
/// never set aside.
fn sample(members: &[u32], images: &[Image], group: u32, places: &mut [Place]) {
    let mut ordered: Vec<u32> = members.iter().copied().filter(|&index| !has_own_name(images[index as usize].path)).collect();
    if ordered.is_empty() {
        return;
    }
    ordered.sort_by_key(|&index| name_of(images[index as usize].path));
    let count = ordered.len();
    let samples = ((count as f64).sqrt().ceil() as usize).clamp(1, SAMPLES);
    let similar = ((count - samples) / samples) as u32;
    for (position, &index) in ordered.iter().enumerate() {
        let place = &mut places[index as usize];
        place.group = group;
        // The first of each of `samples` equal stretches.
        if position == 0 || position * samples / count != (position - 1) * samples / count {
            place.similar = similar;
        } else {
            place.later = true;
        }
    }
}

/// Runs of images moments apart and about the same size.
fn bursts(members: &[u32], images: &[Image], mut burst: u32, places: &mut [Place]) -> u32 {
    let mut ordered = members.to_vec();
    ordered.sort_by_key(|&index| std::cmp::Reverse(images[index as usize].modified));
    let mut run: Vec<u32> = Vec::new();
    let mut close_run = |run: &mut Vec<u32>, places: &mut [Place]| {
        if run.len() > 1 {
            burst += 1;
            for &index in run.iter() {
                places[index as usize].burst = burst;
            }
            places[run[0] as usize].similar = run.len() as u32 - 1;
        }
        run.clear();
    };
    for &index in &ordered {
        let continues = run.last().is_some_and(|&last| {
            let (before, now) = (&images[last as usize], &images[index as usize]);
            let (small, large) = (before.size.min(now.size) as f64, before.size.max(now.size) as f64);
            before.modified - now.modified <= BURST_SECONDS && large <= small.max(1.0) * BURST_SIZES
        });
        if !continues {
            close_run(&mut run, places);
        }
        run.push(index);
    }
    close_run(&mut run, places);
    burst
}

/// A difference hash: 64 bits saying whether each cell of a tiny grayscale image is brighter than its right neighbor. From
/// an image already shrunk to a few dozen pixels (8-bit RGB).
pub fn dhash(width: u32, height: u32, pixels: &[u8]) -> u64 {
    let (width, height) = (width.max(1) as usize, height.max(1) as usize);
    let cell = |x: usize, y: usize| -> f32 {
        // The average of what's under this cell of the 9 by 8 grid.
        let (x0, x1) = (x * width / 9, ((x + 1) * width / 9).max(x * width / 9 + 1).min(width));
        let (y0, y1) = (y * height / 8, ((y + 1) * height / 8).max(y * height / 8 + 1).min(height));
        let (mut sum, mut count) = (0.0, 0.0);
        for row in y0..y1 {
            for column in x0..x1 {
                let at = (row * width + column) * 3;
                if at + 2 < pixels.len() {
                    sum += 0.299 * f32::from(pixels[at]) + 0.587 * f32::from(pixels[at + 1]) + 0.114 * f32::from(pixels[at + 2]);
                    count += 1.0;
                }
            }
        }
        if count > 0.0 { sum / count } else { 0.0 }
    };
    let mut hash = 0;
    for y in 0..8 {
        for x in 0..8 {
            hash = (hash << 1) | u64::from(cell(x, y) > cell(x + 1, y));
        }
    }
    hash
}

/// Whether two hashes are of nearly the same image.
pub fn near(a: u64, b: u64) -> bool {
    (a ^ b).count_ones() <= 10
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Vec<(String, u64, i64)> {
        let mut files = Vec::new();
        // Video frames, in one folder.
        for number in 0..900 {
            files.push((format!(r"C:\Users\me\Desktop\video\frame_{number:04}.png"), 300_000 + number * 13, 1_000 + number as i64));
        }
        // Game assets, spread over many small folders of one big tree.
        for folder in 0..300 {
            for number in 0..6 {
                files.push((format!(r"C:\Users\me\Desktop\game\assets\pack{folder}\tex{number}.png"), 9_000 + folder * 7 + number, 2_000));
            }
        }
        // The person's photos, and a burst of three among them.
        for number in 0..1_000 {
            files.push((format!(r"C:\Users\me\Pictures\trip\IMG_{number:04}.jpg"), 2_000_000 + number * 1_000, 10_000 + number as i64 * 600));
        }
        for number in 0..3 {
            files.push((format!(r"C:\Users\me\Pictures\trip\IMG_9{number}00.jpg"), 2_500_000 + number * 1_000, 5_000 + number as i64 * 2));
        }
        // A screenshot on its own, somewhere else; a few plain images beside the projects; a screenshot among the assets.
        files.push((r"C:\Users\me\Desktop\Screenshot 2026-10-30.png".to_string(), 180_000, 90_000));
        for number in 0..5 {
            files.push((format!(r"C:\Users\me\Desktop\receipts\receipt {number}.png"), 300_000 + number, 3_000 + number as i64 * 100));
        }
        files.push((r"C:\Users\me\Desktop\game\assets\pack7\Screenshot 1.png".to_string(), 9_000, 2_000));
        files
    }

    fn run(files: &[(String, u64, i64)]) -> Vec<Place> {
        let images: Vec<Image> = files.iter().map(|(path, size, modified)| Image { path, size: *size, modified: *modified }).collect();
        places(&images)
    }

    #[test]
    fn small_libraries_are_read_in_order() {
        let files: Vec<_> = library().into_iter().take(LARGE - 1).collect();
        assert_eq!(first_sweep(&run(&files)), files.len());
    }

    #[test]
    fn samples_sets_and_collections_and_keeps_photos() {
        let files = library();
        let places = run(&files);
        let in_folder = |part: &str| -> Vec<&Place> {
            files.iter().zip(&places).filter(|((path, ..), _)| path.contains(part)).map(|(_, place)| place).collect()
        };
        // The frames: a dozen stand for 900.
        let frames = in_folder(r"\video\");
        assert_eq!(frames.iter().filter(|place| !place.later).count(), SAMPLES);
        assert!(frames.iter().all(|place| place.group == frames[0].group && place.group != 0));
        // The assets: 1,800 tiny files in 300 folders, sampled by branch rather than one by one.
        let assets = in_folder(r"\game\");
        let first = assets.iter().filter(|place| !place.later).count();
        assert!((12..=300).contains(&first), "{first}");
        // The photos all go first, and the burst is marked for a look.
        let photos = in_folder(r"\trip\");
        assert!(photos.iter().all(|place| !place.later));
        let burst: Vec<_> = photos.iter().filter(|place| place.burst != 0).collect();
        assert_eq!(burst.len(), 3);
        assert_eq!(burst.iter().map(|place| place.similar).sum::<u32>(), 2);
        assert!(in_folder("Screenshot").iter().all(|place| !place.later));
        assert!(in_folder(r"\receipts\").iter().all(|place| !place.later));
        // Every image has its place, and every group something in the first sweep.
        assert!(first_sweep(&places) < files.len() / 2);
    }

    #[test]
    fn twins_have_the_same_size() {
        let files = vec![
            (r"C:\a\one.png".to_string(), 5000, 1),
            (r"C:\b\two.png".to_string(), 5000, 2),
            (r"C:\c\three.png".to_string(), 6000, 3),
            (r"C:\d\tiny.png".to_string(), 10, 4),
            (r"C:\e\tiny.png".to_string(), 10, 5),
        ];
        let places = run(&files);
        assert_eq!(places.iter().map(|place| place.twin).collect::<Vec<_>>(), [true, true, false, false, false]);
    }

    #[test]
    fn hashes_tell_images_apart() {
        let image = |seed: u32, brightness: i32| -> Vec<u8> {
            let mut pixels = Vec::new();
            let mut value = seed;
            for _ in 0..(36 * 27) {
                value = value.wrapping_mul(2_654_435_761).wrapping_add(13);
                let shade = ((value >> 24) as i32 + brightness).clamp(0, 255) as u8;
                pixels.extend([shade, shade, shade]);
            }
            pixels
        };
        let one = dhash(36, 27, &image(1, 0));
        assert!(near(one, dhash(36, 27, &image(1, 8))), "the same image, a little brighter");
        assert!(!near(one, dhash(36, 27, &image(2, 0))), "another image");
    }
}
