//! Covering parts of a preview picture: each region becomes coarse blocks of its average color, so nothing in it
//! can be read (a Gaussian blur of text can sometimes be undone; averaged blocks this coarse can't).

use winsys::icons::Bitmap;

/// Pixelates `regions` (left, top, right, bottom, in the original picture's pixels, which was `original` in size) of
/// a premultiplied RGBA bitmap drawn from it at a smaller size.
pub fn pixelate(bitmap: &mut Bitmap, regions: &[[i32; 4]], original: (u32, u32)) {
    if bitmap.width == 0 || bitmap.height == 0 || original.0 == 0 || original.1 == 0 {
        return;
    }
    let (sx, sy) = (bitmap.width as f32 / original.0 as f32, bitmap.height as f32 / original.1 as f32);
    // Blocks about a line of text tall, never smaller than 6 pixels.
    let block = ((bitmap.width.max(bitmap.height) as f32) / 40.0).max(6.0) as u32;
    for region in regions {
        let margin = 3.0;
        let left = ((region[0] as f32 * sx) - margin).max(0.0) as u32;
        let top = ((region[1] as f32 * sy) - margin).max(0.0) as u32;
        let right = ((region[2] as f32 * sx) + margin).min(bitmap.width as f32) as u32;
        let bottom = ((region[3] as f32 * sy) + margin).min(bitmap.height as f32) as u32;
        let mut y = top;
        while y < bottom {
            let y_end = (y + block).min(bottom);
            let mut x = left;
            while x < right {
                let x_end = (x + block).min(right);
                average(bitmap, x, y, x_end, y_end);
                x = x_end;
            }
            y = y_end;
        }
    }
}

/// Pixelates all of a bitmap: a picture shown while presenting, or one not read yet (its secrets aren't known).
pub fn cover(bitmap: &mut Bitmap) {
    let (width, height) = (bitmap.width, bitmap.height);
    pixelate(bitmap, &[[0, 0, width as i32, height as i32]], (width, height));
}

pub use winsys::imaging::turned;

/// Outlines `regions` (in the original's pixels, `original` in size) of a premultiplied RGBA bitmap drawn from it, as
/// the README's film marks the words searched for (`draw_outline`).
pub fn outline(bitmap: &mut Bitmap, regions: &[[i32; 4]], original: (u32, u32), color: [u8; 3]) {
    if bitmap.width == 0 || bitmap.height == 0 || original.0 == 0 || original.1 == 0 {
        return;
    }
    let (sx, sy) = (bitmap.width as f32 / original.0 as f32, bitmap.height as f32 / original.1 as f32);
    let places: Vec<[f32; 4]> = regions.iter().map(|r| [r[0] as f32 * sx, r[1] as f32 * sy, r[2] as f32 * sx, r[3] as f32 * sy]).collect();
    let (width, height) = (bitmap.width, bitmap.height);
    draw_outline(&mut bitmap.rgba, width, height, 4, &places, color);
}

/// Outlines `places` (in pixels) of an image `channels` bytes a pixel (RGB or RGBA): a border in `color` about two
/// pixels wide, rounded at the corners, over a light wash of it.
pub fn draw_outline(pixels: &mut [u8], width: u32, height: u32, channels: usize, places: &[[f32; 4]], color: [u8; 3]) {
    let border = ((width.max(height) as f32) / 400.0).clamp(1.5, 3.0);
    for place in places {
        let left = (place[0] - border).max(0.0);
        let top = (place[1] - border).max(0.0);
        let right = (place[2] + border).min(width as f32);
        let bottom = (place[3] + border).min(height as f32);
        if right - left < 2.0 || bottom - top < 2.0 {
            continue;
        }
        let radius = border * 1.5;
        for y in top as u32..(bottom.ceil() as u32).min(height) {
            for x in left as u32..(right.ceil() as u32).min(width) {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                // Past a corner's circle: outside.
                let dx = (left + radius - px).max(px - (right - radius)).max(0.0);
                let dy = (top + radius - py).max(py - (bottom - radius)).max(0.0);
                let corner = (dx * dx + dy * dy).sqrt();
                if corner > radius {
                    continue;
                }
                let edge = if corner > 0.0 { radius - corner } else { (px - left).min(right - px).min(py - top).min(bottom - py) };
                let alpha = if edge <= border { 0.95 } else { 0.18 };
                let at = (y as usize * width as usize + x as usize) * channels;
                let Some(pixel) = pixels.get_mut(at..at + channels) else { continue };
                for channel in 0..3 {
                    pixel[channel] = (f32::from(color[channel]) * alpha + f32::from(pixel[channel]) * (1.0 - alpha)).round() as u8;
                }
                if channels == 4 {
                    pixel[3] = (255.0 * alpha + f32::from(pixel[3]) * (1.0 - alpha)).round() as u8;
                }
            }
        }
    }
}

fn average(bitmap: &mut Bitmap, left: u32, top: u32, right: u32, bottom: u32) {
    let stride = bitmap.width as usize * 4;
    let mut sum = [0u64; 4];
    let mut count = 0u64;
    for y in top..bottom {
        for x in left..right {
            let at = y as usize * stride + x as usize * 4;
            for channel in 0..4 {
                sum[channel] += u64::from(bitmap.rgba[at + channel]);
            }
            count += 1;
        }
    }
    if count == 0 {
        return;
    }
    let color: Vec<u8> = sum.iter().map(|total| (total / count) as u8).collect();
    for y in top..bottom {
        for x in left..right {
            let at = y as usize * stride + x as usize * 4;
            bitmap.rgba[at..at + 4].copy_from_slice(&color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_turn_with_the_picture() {
        // In a picture 100 wide and 200 high: a region near its top left.
        let region = [[10, 20, 30, 40]];
        let turn = |orientation| turned(&region, (100, 200), orientation);
        assert_eq!(turn(1), (vec![[10, 20, 30, 40]], (100, 200)));
        assert_eq!(turn(2), (vec![[70, 20, 90, 40]], (100, 200)), "mirrored: near the top right");
        assert_eq!(turn(3), (vec![[70, 160, 90, 180]], (100, 200)), "upside down: near the bottom right");
        assert_eq!(turn(4), (vec![[10, 160, 30, 180]], (100, 200)), "flipped: near the bottom left");
        assert_eq!(turn(5), (vec![[20, 10, 40, 30]], (200, 100)), "transposed");
        assert_eq!(turn(6), (vec![[160, 10, 180, 30]], (200, 100)), "a quarter turn clockwise: near the top right");
        assert_eq!(turn(7), (vec![[160, 70, 180, 90]], (200, 100)), "transversed");
        assert_eq!(turn(8), (vec![[20, 70, 40, 90]], (200, 100)), "a quarter turn back: near the bottom left");
    }

    #[test]
    fn covers_the_region_only() {
        // 100×20, a checkerboard; the region is the left half of a 200×40 original.
        let mut rgba = Vec::new();
        for y in 0..20u32 {
            for x in 0..100u32 {
                let value = if (x + y) % 2 == 0 { 255 } else { 0 };
                rgba.extend([value, value, value, 255]);
            }
        }
        let mut bitmap = Bitmap { width: 100, height: 20, rgba };
        pixelate(&mut bitmap, &[[0, 0, 80, 40]], (200, 40));
        let pixel = |x: usize, y: usize| bitmap.rgba[(y * 100 + x) * 4];
        // Inside: one flat average; outside (past the margin): the checkerboard as it was.
        assert_eq!(pixel(0, 0), pixel(1, 0));
        assert!(pixel(0, 0) > 100 && pixel(0, 0) < 160);
        assert_ne!(pixel(90, 0), pixel(91, 0));
    }
}
