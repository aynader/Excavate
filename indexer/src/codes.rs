//! Where QR codes are in an image, without reading them: a router's sticker or a phone's "share Wi-Fi" screen shows
//! the network's password as a QR code, which OCR can't read and a phone can. A code is found by its three finder
//! patterns (the squares at its corners: dark, light, dark three times as wide, light, dark, 1:1:3:1:1 across and down),
//! three of a size at a right angle; the code is the square they make, with a margin. What it says isn't read (that
//! would take a decoder); `work.rs` covers the codes of images whose text is about Wi-Fi (`privacy::about_wifi`).

/// The codes in an RGB image (`width` × `height`, 3 bytes a pixel), each [left, top, right, bottom] in its pixels.
pub fn find(width: u32, height: u32, pixels: &[u8]) -> Vec<[f32; 4]> {
    let (w, h) = (width as usize, height as usize);
    if w < 21 || h < 21 || pixels.len() < w * h * 3 {
        return Vec::new();
    }
    let dark = binarize(w, h, pixels);
    let mut candidates: Vec<(f32, f32, f32)> = Vec::new();
    // Every other row is enough: a finder pattern is at least 7 pixels tall to be found at all.
    for y in (0..h).step_by(2) {
        let row = &dark[y * w..(y + 1) * w];
        for (center, module) in stripes(row.iter().copied()) {
            let x = center.round() as usize;
            // Down through its middle: the same stripes, about the same size.
            let column = (0..h).map(|yy| dark[yy * w + x]);
            if let Some((cy, vertical)) = stripes(column).into_iter()
                .filter(|(cy, vertical)| (cy - y as f32).abs() <= module * 1.5 && (vertical / module - 1.0).abs() < 0.5)
                .min_by(|a, b| (a.0 - y as f32).abs().total_cmp(&(b.0 - y as f32).abs()))
            {
                candidates.push((center, cy, (module + vertical) / 2.0));
            }
        }
    }
    let finders = cluster(&candidates);
    codes(&finders, w as f32, h as f32)
}

/// Dark pixels: darker than the average around them (a window an eighth of the image wide), so shade and glare on a
/// photographed sticker don't matter.
fn binarize(w: usize, h: usize, pixels: &[u8]) -> Vec<bool> {
    let gray: Vec<u32> = pixels.chunks_exact(3).take(w * h).map(|p| (p[0] as u32 * 30 + p[1] as u32 * 59 + p[2] as u32 * 11) / 100).collect();
    // Sums over rectangles from the integral image.
    let mut integral = vec![0u64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u64;
        for x in 0..w {
            row += gray[y * w + x] as u64;
            integral[(y + 1) * (w + 1) + x + 1] = integral[y * (w + 1) + x + 1] + row;
        }
    }
    let half = (w.max(h) / 16).clamp(8, 64);
    let mut dark = vec![false; w * h];
    for y in 0..h {
        let (top, bottom) = (y.saturating_sub(half), (y + half + 1).min(h));
        for x in 0..w {
            let (left, right) = (x.saturating_sub(half), (x + half + 1).min(w));
            let sum = integral[bottom * (w + 1) + right] + integral[top * (w + 1) + left]
                - integral[top * (w + 1) + right] - integral[bottom * (w + 1) + left];
            let mean = sum / ((bottom - top) * (right - left)) as u64;
            dark[y * w + x] = (gray[y * w + x] as u64) * 100 < mean * 88;
        }
    }
    dark
}

/// Along a line of pixels: where dark, light, dark (3), light, dark run in a 1:1:3:1:1 ratio. Each one's center and
/// module size.
fn stripes(line: impl Iterator<Item = bool>) -> Vec<(f32, f32)> {
    let mut runs: Vec<(bool, usize, usize)> = Vec::new();
    for (at, dark) in line.enumerate() {
        match runs.last_mut() {
            Some(last) if last.0 == dark => last.2 += 1,
            _ => runs.push((dark, at, 1)),
        }
    }
    let mut found = Vec::new();
    for window in runs.windows(5) {
        if !window[0].0 {
            continue;
        }
        let total: usize = window.iter().map(|run| run.2).sum();
        if total < 7 {
            continue;
        }
        let module = total as f32 / 7.0;
        let fits = |run: usize, modules: f32| (run as f32 - module * modules).abs() <= module * modules * 0.5 + 0.5;
        if fits(window[0].2, 1.0) && fits(window[1].2, 1.0) && fits(window[2].2, 3.0) && fits(window[3].2, 1.0) && fits(window[4].2, 1.0) {
            found.push((window[2].1 as f32 + window[2].2 as f32 / 2.0, module));
        }
    }
    found
}

/// Candidates seen on several rows, as one finder each: its center and module size. Seen on fewer than two rows: noise.
fn cluster(candidates: &[(f32, f32, f32)]) -> Vec<(f32, f32, f32)> {
    let mut groups: Vec<(f32, f32, f32, usize)> = Vec::new();
    for &(x, y, module) in candidates {
        match groups.iter_mut().find(|g| (g.0 / g.3 as f32 - x).abs() <= module * 2.0 && (g.1 / g.3 as f32 - y).abs() <= module * 2.0) {
            Some(group) => {
                group.0 += x;
                group.1 += y;
                group.2 += module;
                group.3 += 1;
            }
            None => groups.push((x, y, module, 1)),
        }
    }
    groups.into_iter().filter(|g| g.3 >= 2).map(|g| (g.0 / g.3 as f32, g.1 / g.3 as f32, g.2 / g.3 as f32)).collect()
}

/// Three finders of a size at a right angle, as a code's square, with a margin.
fn codes(finders: &[(f32, f32, f32)], w: f32, h: f32) -> Vec<[f32; 4]> {
    let mut found: Vec<[f32; 4]> = Vec::new();
    let distance = |a: (f32, f32, f32), b: (f32, f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    for i in 0..finders.len() {
        for j in i + 1..finders.len() {
            for k in j + 1..finders.len() {
                let three = [finders[i], finders[j], finders[k]];
                let modules: Vec<f32> = three.iter().map(|f| f.2).collect();
                let (small, large) = (modules.iter().copied().fold(f32::MAX, f32::min), modules.iter().copied().fold(0.0, f32::max));
                if large > small * 1.5 {
                    continue;
                }
                // The corner is the one across from the longest side.
                let sides = [distance(three[1], three[2]), distance(three[0], three[2]), distance(three[0], three[1])];
                let corner = (0..3).max_by(|&a, &b| sides[a].total_cmp(&sides[b])).unwrap_or(0);
                let (a, b, c) = (three[corner], three[(corner + 1) % 3], three[(corner + 2) % 3]);
                let (ab, ac, bc) = (distance(a, b), distance(a, c), distance(b, c));
                let module = (a.2 + b.2 + c.2) / 3.0;
                // Two sides alike, at a right angle; 14 to 170 modules apart (versions 1 to 40).
                if (ab / ac - 1.0).abs() > 0.25 || (bc / (ab.hypot(ac)) - 1.0).abs() > 0.12 {
                    continue;
                }
                if !(12.0..=180.0).contains(&(ab.max(ac) / module)) {
                    continue;
                }
                let d = (b.0 + c.0 - a.0, b.1 + c.1 - a.1);
                let xs = [a.0, b.0, c.0, d.0];
                let ys = [a.1, b.1, c.1, d.1];
                let margin = module * 5.0;
                let square = [
                    (xs.iter().copied().fold(f32::MAX, f32::min) - margin).max(0.0),
                    (ys.iter().copied().fold(f32::MAX, f32::min) - margin).max(0.0),
                    (xs.iter().copied().fold(0.0, f32::max) + margin).min(w),
                    (ys.iter().copied().fold(0.0, f32::max) + margin).min(h),
                ];
                if !found.iter().any(|kept| overlap(kept, &square)) {
                    found.push(square);
                }
            }
        }
    }
    found
}

fn overlap(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A code's look: 25 modules (version 2), its three finders, the rest a fixed scatter, `scale` pixels a module, on a
    /// white page at (`left`, `top`).
    fn code(page: (usize, usize), left: usize, top: usize, scale: usize) -> Vec<u8> {
        let (w, h) = page;
        let mut pixels = vec![255u8; w * h * 3];
        let n = 25;
        let finder = |mx: usize, my: usize| -> Option<bool> {
            for (fx, fy) in [(0, 0), (n - 7, 0), (0, n - 7)] {
                if (fx..fx + 7).contains(&mx) && (fy..fy + 7).contains(&my) {
                    let (dx, dy) = (mx - fx, my - fy);
                    let ring = dx.min(dy).min(6 - dx).min(6 - dy);
                    return Some(ring != 1);
                }
                // The light separator around each finder.
                if (fx.saturating_sub(1)..fx + 8).contains(&mx) && (fy.saturating_sub(1)..fy + 8).contains(&my) {
                    return Some(false);
                }
            }
            None
        };
        let mut seed = 12345u32;
        for my in 0..n {
            for mx in 0..n {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let on = finder(mx, my).unwrap_or(seed >> 16 & 1 == 1);
                if !on {
                    continue;
                }
                for y in top + my * scale..top + (my + 1) * scale {
                    for x in left + mx * scale..left + (mx + 1) * scale {
                        let at = (y * w + x) * 3;
                        pixels[at..at + 3].copy_from_slice(&[20, 20, 20]);
                    }
                }
            }
        }
        pixels
    }

    #[test]
    fn finds_a_code() {
        let (w, h) = (600, 400);
        let pixels = code((w, h), 300, 100, 8);
        let found = find(w as u32, h as u32, &pixels);
        assert_eq!(found.len(), 1, "{found:?}");
        let [l, t, r, b] = found[0];
        // It covers the code (300..500, 100..300), and not much more.
        assert!(l <= 300.0 && t <= 100.0 && r >= 500.0 && b >= 300.0, "{found:?}");
        assert!(l >= 240.0 && t >= 40.0 && r <= 560.0 && b <= 360.0, "{found:?}");
    }

    #[test]
    fn small_codes_too() {
        let (w, h) = (300, 200);
        let pixels = code((w, h), 150, 40, 4);
        assert_eq!(find(w as u32, h as u32, &pixels).len(), 1);
    }

    #[test]
    fn none_in_plain_pages() {
        let (w, h) = (400, 300);
        // Stripes of text-like bars, and an empty page.
        let mut pixels = vec![255u8; w * h * 3];
        for y in (20..280).step_by(18) {
            for x in 20..380 {
                if (x / 7) % 3 != 0 {
                    for yy in y..y + 9 {
                        let at = (yy * w + x) * 3;
                        pixels[at..at + 3].copy_from_slice(&[0, 0, 0]);
                    }
                }
            }
        }
        assert!(find(w as u32, h as u32, &pixels).is_empty());
        assert!(find(w as u32, h as u32, &vec![255u8; w * h * 3]).is_empty());
    }
}
