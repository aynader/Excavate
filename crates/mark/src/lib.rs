//! The mark brought to life, from the artwork itself: it has three separate parts (the dark outer swirl, the light
//! inner swirl, the amber core), and walking each swirl from its end tells every pixel how far along it is. A frame then
//! only asks which pixels are "drawn" so far, so the hand-made texture stays exactly as drawn.
//!
//! Pure pixels, no windows: the launch animation shows the frames in a layered window, the settings window's rail in an
//! image. Everything is worked out from the artwork when it's needed (a few milliseconds), and costs nothing otherwise.

use std::collections::BinaryHeap;

/// A swirl's end, as a share of the artwork's size. The dark swirl is walked from its tail on the left to the center;
/// the light one from its tail on the right, then turned around (it's drawn from the core outward).
const DARK_TAIL: (f32, f32) = (91.0 / 512.0, 432.0 / 512.0);
const LIGHT_TAIL: (f32, f32) = (473.0 / 512.0, 345.0 / 512.0);

/// The parts `parts` tells apart.
pub const DARK: u8 = 1;
pub const CORE: u8 = 2;
pub const LIGHT: u8 = 3;

/// The light's color (added to what's there): warm white, a little gold.
const RUN: [f32; 3] = [1.0, 0.86, 0.62];
/// The drawing tip's and the glow's: the core's amber.
const AMBER: [f32; 3] = [1.0, 0.66, 0.2];

#[derive(Clone, Copy)]
struct Pixel {
    index: u32,
    part: u8,
    /// How far along its swirl (0 at its start, 1 at its end).
    along: f32,
    /// Along both swirls in a row: tail to tail, through the center.
    whole: f32,
    /// Premultiplied, 0 to 1.
    rgba: [f32; 4],
}

/// The mark at one size, ready to draw.
pub struct Mark {
    pub size: usize,
    pixels: Vec<Pixel>,
    center: (f32, f32),
    core_radius: f32,
    /// The core alone, premultiplied, for growing it past its size.
    core: Vec<[f32; 4]>,
    core_box: (usize, usize, usize),
    /// The light that runs along it, and the core's glow: gold and amber, unless a theme recolored the artwork.
    run: [f32; 3],
    amber: [f32; 3],
}

impl Mark {
    /// From the artwork (straight RGBA, square), at `size` pixels.
    pub fn new(artwork: &[u8], artwork_size: usize, size: usize) -> Mark {
        let (parts, along) = walk(artwork, artwork_size);
        // Lengths of both swirls, so a light moving along them moves at one speed.
        let (dark_length, light_length) = lengths(&parts);
        let dark_share = dark_length / (dark_length + light_length).max(1.0);
        let source_pixel = |x: usize, y: usize| {
            let at = (y * artwork_size + x) * 4;
            let alpha = f32::from(artwork[at + 3]) / 255.0;
            [f32::from(artwork[at]) / 255.0 * alpha, f32::from(artwork[at + 1]) / 255.0 * alpha, f32::from(artwork[at + 2]) / 255.0 * alpha, alpha]
        };
        let footprint = artwork_size as f32 / size as f32;
        let mut pixels = Vec::with_capacity(size * size / 2);
        for y in 0..size {
            for x in 0..size {
                // The average of the artwork under this pixel; its part from the most-covered one.
                let (x0, x1) = ((x as f32 * footprint) as usize, (((x + 1) as f32 * footprint).ceil() as usize).min(artwork_size));
                let (y0, y1) = ((y as f32 * footprint) as usize, (((y + 1) as f32 * footprint).ceil() as usize).min(artwork_size));
                let mut sum = [0.0f32; 4];
                let mut count = 0.0;
                let mut best = (0.0f32, 0u8, 0.0f32);
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let source = source_pixel(sx, sy);
                        for (total, value) in sum.iter_mut().zip(source) {
                            *total += value;
                        }
                        count += 1.0;
                        let part = parts[sy * artwork_size + sx];
                        if part != 0 && source[3] >= best.0 {
                            best = (source[3], part, along[sy * artwork_size + sx]);
                        }
                    }
                }
                if sum[3] <= 0.0 || best.1 == 0 {
                    continue;
                }
                let rgba = sum.map(|value| value / count);
                let whole = match best.1 {
                    DARK => best.2 * dark_share,
                    LIGHT => dark_share + best.2 * (1.0 - dark_share),
                    _ => dark_share,
                };
                pixels.push(Pixel { index: (y * size + x) as u32, part: best.1, along: best.2, whole, rgba });
            }
        }
        // The core's place and size.
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (size, size, 0, 0);
        for pixel in pixels.iter().filter(|pixel| pixel.part == CORE) {
            let (x, y) = (pixel.index as usize % size, pixel.index as usize / size);
            (min_x, min_y, max_x, max_y) = (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y));
        }
        let side = (max_x.saturating_sub(min_x) + 1).max(max_y.saturating_sub(min_y) + 1).max(1);
        let mut core = vec![[0.0; 4]; side * side];
        for pixel in pixels.iter().filter(|pixel| pixel.part == CORE) {
            let (x, y) = (pixel.index as usize % size, pixel.index as usize / size);
            if x >= min_x && y >= min_y && x - min_x < side && y - min_y < side {
                core[(y - min_y) * side + (x - min_x)] = pixel.rgba;
            }
        }
        let center = (min_x as f32 + side as f32 / 2.0, min_y as f32 + side as f32 / 2.0);
        Mark { size, pixels, center, core_radius: side as f32 / 2.0, core, core_box: (min_x, min_y, side), run: RUN, amber: AMBER }
    }

    /// The light's and the glow's colors (0 to 1), for artwork in a theme's colors.
    pub fn set_lights(&mut self, run: [f32; 3], glow: [f32; 3]) {
        self.run = run;
        self.amber = glow;
    }

    pub const INTRO_DRAWN: f32 = 1.25;

    /// A frame of the launch animation up to where the mark is whole, `t` seconds in (BGRA, premultiplied, `size` ×
    /// `size`): the dark swirl draws in from its tail to the center, the core pops in with one soft glow, the light
    /// swirl draws from the core out to its tail, and a light runs along the finished mark.
    pub fn intro(&self, t: f32, out: &mut [u8]) {
        out.fill(0);
        let dark_front = ease(t / 0.5);
        let light_front = ease((t - 0.62) / 0.42);
        let run = ((t - 0.95) / 0.42).clamp(-1.0, 2.0);
        let run_at = run * 1.2 - 0.1;
        let run_on = run > 0.0 && run < 1.0;
        const EDGE: f32 = 0.035;
        for pixel in &self.pixels {
            if pixel.part == CORE {
                continue;
            }
            let front = if pixel.part == DARK { dark_front * (1.0 + EDGE) } else { light_front * (1.0 + EDGE) };
            let shown = ((front - pixel.along) / EDGE).clamp(0.0, 1.0);
            if shown <= 0.0 {
                continue;
            }
            let [mut r, mut g, mut b, a] = pixel.rgba.map(|value| value * shown);
            // The drawing tip: a little amber, while it's moving.
            if front < 1.0 + EDGE {
                let tip = (1.0 - ((front - pixel.along) / (EDGE * 2.5))).clamp(0.0, 1.0);
                let tip = tip * tip * 0.75 * a;
                r += self.amber[0] * tip;
                g += self.amber[1] * tip;
                b += self.amber[2] * tip;
            }
            if run_on {
                let light = gaussian((pixel.whole - run_at) / 0.07) * 0.7 * a;
                r += self.run[0] * light;
                g += self.run[1] * light;
                b += self.run[2] * light;
            }
            put_bgra(out, pixel.index as usize, r, g, b, a);
        }
        // The core: grows past its size and settles, with one soft ring of glow that fades.
        let core_time = t - 0.42;
        if core_time > 0.0 {
            let grown = ease_back(core_time / 0.3);
            let fade = (core_time / 0.14).clamp(0.0, 1.0);
            let ring = (core_time - 0.05) / 0.5;
            if (0.0..1.0).contains(&ring) {
                self.glow(out, self.core_radius * (1.0 + ring * 1.5), (1.0 - ring) * (1.0 - ring) * 0.55, false);
            }
            self.draw_core(out, grown.max(0.02), fade, t);
        }
    }

    /// The finished mark, without any light on it (RGBA, premultiplied, as Slint takes it).
    pub fn still(&self, out: &mut [u8]) {
        out.fill(0);
        for pixel in &self.pixels {
            let at = pixel.index as usize * 4;
            for (channel, value) in pixel.rgba.into_iter().enumerate() {
                out[at + channel] = to_byte(value);
            }
        }
    }

    /// The living mark: while reading, a light travels along the spiral from tail to tail and back, and the core
    /// breathes with a slow glow (RGBA, premultiplied, as Slint takes it).
    pub fn alive(&self, t: f32, out: &mut [u8]) {
        out.fill(0);
        // Back and forth, slowing at the ends.
        let travel = 0.5 - 0.5 * (t * std::f32::consts::TAU / 7.0).cos();
        let at = -0.08 + travel * 1.16;
        let breath = 0.5 - 0.5 * (t * std::f32::consts::TAU / 2.6).cos();
        for pixel in &self.pixels {
            let [mut r, mut g, mut b, a] = pixel.rgba;
            if pixel.part == CORE {
                let lift = 1.0 + 0.22 * breath;
                r = (r * lift).min(a);
                g = (g * lift).min(a);
                b = (b * lift).min(a);
            } else {
                let light = gaussian((pixel.whole - at) / 0.055) * 0.62 * a;
                r += self.run[0] * light;
                g += self.run[1] * light;
                b += self.run[2] * light;
            }
            let at_out = pixel.index as usize * 4;
            // Premultiplied: never brighter than its own alpha.
            out[at_out] = to_byte(r.min(a));
            out[at_out + 1] = to_byte(g.min(a));
            out[at_out + 2] = to_byte(b.min(a));
            out[at_out + 3] = to_byte(a);
        }
        self.glow(out, self.core_radius * 1.25, 0.12 + 0.2 * breath, true);
    }

    fn draw_core(&self, out: &mut [u8], scale: f32, fade: f32, t: f32) {
        let (left, top, side) = self.core_box;
        let half = side as f32 / 2.0;
        let (center_x, center_y) = (left as f32 + half, top as f32 + half);
        let reach = half * scale.max(1.0) + 2.0;
        let (x0, x1) = (((center_x - reach).floor().max(0.0)) as usize, ((center_x + reach).ceil() as usize).min(self.size));
        let (y0, y1) = (((center_y - reach).floor().max(0.0)) as usize, ((center_y + reach).ceil() as usize).min(self.size));
        // A touch brighter as it arrives.
        let lift = 1.0 + 0.35 * (1.0 - ((t - 0.42) / 0.5).clamp(0.0, 1.0));
        for y in y0..y1 {
            for x in x0..x1 {
                let (sx, sy) = ((x as f32 + 0.5 - center_x) / scale + half - 0.5, (y as f32 + 0.5 - center_y) / scale + half - 0.5);
                if sx < -1.0 || sy < -1.0 || sx > side as f32 || sy > side as f32 {
                    continue;
                }
                let [r, g, b, a] = sample(&self.core, side, sx, sy).map(|value| value * fade);
                if a <= 0.0 {
                    continue;
                }
                let at = (y * self.size + x) * 4;
                // Over what's there.
                let keep = 1.0 - a;
                out[at] = to_byte(f32::from(out[at]) / 255.0 * keep + (b * lift).min(a));
                out[at + 1] = to_byte(f32::from(out[at + 1]) / 255.0 * keep + (g * lift).min(a));
                out[at + 2] = to_byte(f32::from(out[at + 2]) / 255.0 * keep + (r * lift).min(a));
                out[at + 3] = to_byte(f32::from(out[at + 3]) / 255.0 * keep + a);
            }
        }
    }

    /// A soft ring of amber light around the core, added to what's there (also where there's nothing yet).
    fn glow(&self, out: &mut [u8], radius: f32, strength: f32, rgba: bool) {
        let width = self.core_radius * 0.42;
        let reach = radius + width * 2.5;
        let (x0, x1) = ((self.center.0 - reach).max(0.0) as usize, ((self.center.0 + reach).ceil() as usize).min(self.size));
        let (y0, y1) = ((self.center.1 - reach).max(0.0) as usize, ((self.center.1 + reach).ceil() as usize).min(self.size));
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - self.center.0, y as f32 + 0.5 - self.center.1);
                let distance = (dx * dx + dy * dy).sqrt();
                // Soft on the outside; inside, it fades toward the core rather than covering it.
                let ring = gaussian((distance - radius) / width) * strength;
                if ring < 0.004 {
                    continue;
                }
                let at = (y * self.size + x) * 4;
                let (red, blue) = if rgba { (at, at + 2) } else { (at + 2, at) };
                out[red] = to_byte(f32::from(out[red]) / 255.0 + self.amber[0] * ring);
                out[at + 1] = to_byte(f32::from(out[at + 1]) / 255.0 + self.amber[1] * ring);
                out[blue] = to_byte(f32::from(out[blue]) / 255.0 + self.amber[2] * ring);
                out[at + 3] = to_byte(f32::from(out[at + 3]) / 255.0 + ring);
            }
        }
    }
}

/// `from` (premultiplied, `size` × `size`) scaled into `out` (`target` × `target`), smoothly.
pub fn scale(from: &[u8], size: usize, out: &mut [u8], target: usize) {
    let ratio = size as f32 / target.max(1) as f32;
    let last = size as i32 - 1;
    for y in 0..target {
        let sy = (y as f32 + 0.5) * ratio - 0.5;
        let (y0, fy) = (sy.floor() as i32, sy - sy.floor());
        let (row0, row1) = (y0.clamp(0, last) as usize * size, (y0 + 1).clamp(0, last) as usize * size);
        for x in 0..target {
            let sx = (x as f32 + 0.5) * ratio - 0.5;
            let (x0, fx) = (sx.floor() as i32, sx - sx.floor());
            let (c0, c1) = (x0.clamp(0, last) as usize, (x0 + 1).clamp(0, last) as usize);
            let at = (y * target + x) * 4;
            for channel in 0..4 {
                let top = f32::from(from[(row0 + c0) * 4 + channel]) * (1.0 - fx) + f32::from(from[(row0 + c1) * 4 + channel]) * fx;
                let bottom = f32::from(from[(row1 + c0) * 4 + channel]) * (1.0 - fx) + f32::from(from[(row1 + c1) * 4 + channel]) * fx;
                out[at + channel] = (top * (1.0 - fy) + bottom * fy + 0.5) as u8;
            }
        }
    }
}

fn put_bgra(out: &mut [u8], index: usize, r: f32, g: f32, b: f32, a: f32) {
    let at = index * 4;
    // Premultiplied: never brighter than its own alpha.
    out[at] = to_byte(b.min(a));
    out[at + 1] = to_byte(g.min(a));
    out[at + 2] = to_byte(r.min(a));
    out[at + 3] = to_byte(a);
}

fn to_byte(value: f32) -> u8 {
    (value * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

fn gaussian(x: f32) -> f32 {
    (-x * x).exp()
}

/// Slow in, slow out, 0 to 1.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Goes a little past 1, then settles.
fn ease_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    const OVER: f32 = 1.9;
    1.0 + t * t * ((OVER + 1.0) * t + OVER)
}

fn sample(from: &[[f32; 4]], side: usize, x: f32, y: f32) -> [f32; 4] {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |x: f32, y: f32| -> [f32; 4] {
        if x < 0.0 || y < 0.0 || x as usize >= side || y as usize >= side { [0.0; 4] } else { from[y as usize * side + x as usize] }
    };
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
    let mut out = [0.0; 4];
    for channel in 0..4 {
        out[channel] = (a[channel] * (1.0 - fx) + b[channel] * fx) * (1.0 - fy) + (c[channel] * (1.0 - fx) + d[channel] * fx) * fy;
    }
    out
}

/// Each pixel's part: `DARK`, `CORE`, `LIGHT`, or 0 where there's none (straight RGBA, square). The three biggest connected
/// pieces of what's solid are the swirls and the core (told apart by where they are); what's see-through at their edges joins
/// the piece next to it.
pub fn parts(artwork: &[u8], size: usize) -> Vec<u8> {
    let alpha = |index: usize| artwork[index * 4 + 3];
    let mut labels = vec![0u32; size * size];
    let mut pieces: Vec<(u32, usize)> = Vec::new();
    let mut stack = Vec::new();
    for start in 0..size * size {
        if alpha(start) <= 128 || labels[start] != 0 {
            continue;
        }
        let label = pieces.len() as u32 + 1;
        let mut count = 0;
        labels[start] = label;
        stack.push(start);
        while let Some(at) = stack.pop() {
            count += 1;
            for next in neighbors(at, size) {
                if alpha(next) > 128 && labels[next] == 0 {
                    labels[next] = label;
                    stack.push(next);
                }
            }
        }
        pieces.push((label, count));
    }
    pieces.sort_by_key(|piece| std::cmp::Reverse(piece.1));
    // The core is the smallest of the three; of the swirls, the dark one has the darker pixels.
    let mut parts = vec![0u8; size * size];
    if pieces.len() >= 3 {
        let brightness = |label: u32| {
            let (mut sum, mut count) = (0u64, 0u64);
            for (index, &value) in labels.iter().enumerate() {
                if value == label {
                    sum += u64::from(artwork[index * 4]) + u64::from(artwork[index * 4 + 1]) + u64::from(artwork[index * 4 + 2]);
                    count += 1;
                }
            }
            sum / count.max(1)
        };
        let (first, second, third) = (pieces[0].0, pieces[1].0, pieces[2].0);
        let (dark, light) = if brightness(first) < brightness(second) { (first, second) } else { (second, first) };
        for (index, &label) in labels.iter().enumerate() {
            parts[index] = if label == dark { DARK } else if label == light { LIGHT } else if label == third { CORE } else { 0 };
        }
    }
    // The soft edges (and specks) go with the part beside them.
    for _ in 0..4 {
        let snapshot = parts.clone();
        for index in 0..size * size {
            if snapshot[index] == 0 && alpha(index) > 0
                && let Some(next) = neighbors(index, size).into_iter().find(|&next| snapshot[next] != 0)
            {
                parts[index] = snapshot[next];
            }
        }
    }
    parts
}

/// Each pixel's part and how far along it is.
fn walk(artwork: &[u8], size: usize) -> (Vec<u8>, Vec<f32>) {
    let parts = parts(artwork, size);
    let tail = |part: u8, (x, y): (f32, f32)| {
        let (x, y) = (x * size as f32, y * size as f32);
        parts.iter().enumerate().filter(|&(_, &value)| value == part).min_by(|a, b| {
            let distance = |index: usize| {
                let (px, py) = ((index % size) as f32, (index / size) as f32);
                (px - x) * (px - x) + (py - y) * (py - y)
            };
            distance(a.0).partial_cmp(&distance(b.0)).unwrap_or(std::cmp::Ordering::Equal)
        }).map(|(index, _)| index)
    };
    let mut along = vec![0.0f32; size * size];
    if let Some(start) = tail(DARK, DARK_TAIL) {
        distances(&parts, DARK, start, size, false, &mut along);
    }
    if let Some(start) = tail(LIGHT, LIGHT_TAIL) {
        distances(&parts, LIGHT, start, size, true, &mut along);
    }
    (parts, along)
}

fn neighbors(at: usize, size: usize) -> [usize; 4] {
    let (x, y) = (at % size, at / size);
    [
        if x > 0 { at - 1 } else { at },
        if x + 1 < size { at + 1 } else { at },
        if y > 0 { at - size } else { at },
        if y + 1 < size { at + size } else { at },
    ]
}

/// Walks a part from `start`, staying inside it; writes how far along each pixel is, 0 to 1 (or 1 to 0 when `reversed`).
fn distances(parts: &[u8], part: u8, start: usize, size: usize, reversed: bool, along: &mut [f32]) {
    #[derive(PartialEq, Eq)]
    struct Next(u32, usize);
    impl Ord for Next {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other.0.cmp(&self.0).then(self.1.cmp(&other.1))
        }
    }
    impl PartialOrd for Next {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    let mut distance = vec![u32::MAX; size * size];
    let mut queue = BinaryHeap::new();
    distance[start] = 0;
    queue.push(Next(0, start));
    let mut farthest = 1;
    while let Some(Next(so_far, at)) = queue.pop() {
        if so_far > distance[at] {
            continue;
        }
        farthest = farthest.max(so_far);
        let (x, y) = ((at % size) as i64, (at / size) as i64);
        for (dx, dy, step) in [(1, 0, 10), (-1, 0, 10), (0, 1, 10), (0, -1, 10), (1, 1, 14), (1, -1, 14), (-1, 1, 14), (-1, -1, 14)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= size as i64 || ny >= size as i64 {
                continue;
            }
            let next = ny as usize * size + nx as usize;
            if parts[next] == part && so_far + step < distance[next] {
                distance[next] = so_far + step;
                queue.push(Next(so_far + step, next));
            }
        }
    }
    for index in 0..size * size {
        if parts[index] == part && distance[index] != u32::MAX {
            let share = distance[index] as f32 / farthest as f32;
            along[index] = if reversed { 1.0 - share } else { share };
        }
    }
}

/// How long each swirl is, in pixels along its middle: the count of its pixels stands in for it, as both are about as
/// wide.
fn lengths(parts: &[u8]) -> (f32, f32) {
    let count = |part: u8| parts.iter().filter(|&&value| value == part).count() as f32;
    (count(DARK), count(LIGHT))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two bars and a dot, as a stand-in for the artwork.
    fn artwork() -> (Vec<u8>, usize) {
        let size = 64;
        let mut pixels = vec![0u8; size * size * 4];
        let mut paint = |x0: usize, x1: usize, y0: usize, y1: usize, color: u8| {
            for y in y0..y1 {
                for x in x0..x1 {
                    let at = (y * size + x) * 4;
                    pixels[at] = color;
                    pixels[at + 1] = color;
                    pixels[at + 2] = color;
                    pixels[at + 3] = 255;
                }
            }
        };
        paint(4, 60, 50, 60, 30); // dark, running from the left
        paint(4, 60, 30, 40, 150); // light
        paint(28, 36, 10, 18, 220); // core
        (pixels, size)
    }

    #[test]
    fn finds_the_parts_and_walks_them() {
        let (pixels, size) = artwork();
        let (parts, along) = walk(&pixels, size);
        assert_eq!(parts[55 * size + 20], DARK);
        assert_eq!(parts[35 * size + 20], LIGHT);
        assert_eq!(parts[14 * size + 30], CORE);
        // The dark bar is walked from the end nearest its tail (left): the far end is 1.
        assert!(along[55 * size + 6] < along[55 * size + 58]);
        // The light one from its tail (right), reversed: the left end is 0.
        assert!(along[35 * size + 6] < along[35 * size + 58]);
    }

    #[test]
    fn frames_build_up_to_the_finished_mark() {
        let (pixels, size) = artwork();
        let mark = Mark::new(&pixels, size, 32);
        let mut frame = vec![0u8; 32 * 32 * 4];
        let coverage = |frame: &[u8]| frame.chunks(4).filter(|pixel| pixel[3] > 0).count();
        mark.intro(0.0, &mut frame);
        assert_eq!(coverage(&frame), 0);
        mark.intro(0.3, &mut frame);
        let early = coverage(&frame);
        assert!(early > 0);
        mark.intro(Mark::INTRO_DRAWN + 0.3, &mut frame);
        let late = coverage(&frame);
        assert!(late > early);
        // Valid premultiplied color, always (Slint's renderer relies on it).
        let valid = |frame: &[u8]| frame.chunks(4).all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3]);
        for moment in [0.2, 0.5, 0.8, 1.1, 1.3] {
            mark.intro(moment, &mut frame);
            assert!(valid(&frame), "at {moment} s");
            mark.alive(moment * 3.0, &mut frame);
            assert!(valid(&frame), "alive at {moment} s");
        }
        let mut finished = vec![0u8; 32 * 32 * 4];
        mark.still(&mut finished);
        assert_eq!(late, coverage(&finished));
        let mut smaller = vec![0u8; 16 * 16 * 4];
        scale(&finished, 32, &mut smaller, 16);
        assert!(coverage(&smaller) > 0);
    }
}
