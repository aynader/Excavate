//! Themes: nine named after digging gear, and those made in the Workshop. A theme is an accent (one for dark, one for
//! light, each readable on its background) and a family of surfaces (warm, plain or cool). theme.slint holds the
//! surfaces; this holds the accents, works out a Workshop theme's from its hue, and recolors the mark to match.

use crate::Theme;
use crate::settings::Settings;
use config::OwnTheme;

pub type Rgb = [u8; 3];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surfaces {
    Warm,
    Plain,
    Cool,
}

impl Surfaces {
    pub fn parse(text: &str) -> Surfaces {
        match text {
            "plain" => Surfaces::Plain,
            "cool" => Surfaces::Cool,
            _ => Surfaces::Warm,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Surfaces::Warm => "warm",
            Surfaces::Plain => "plain",
            Surfaces::Cool => "cool",
        }
    }

    pub fn index(self) -> i32 {
        self as i32
    }

    pub fn from_index(index: i32) -> Surfaces {
        [Surfaces::Warm, Surfaces::Plain, Surfaces::Cool][index.clamp(0, 2) as usize]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub name: String,
    pub accent_dark: Rgb,
    pub accent_light: Rgb,
    pub surfaces: Surfaces,
    /// Made in the Workshop.
    pub own: bool,
}

/// The nine, in their tiles' order: amber first (the mark as drawn), then round the color wheel, then steel.
const NINE: [(&str, u32, u32, Surfaces); 9] = [
    ("Lantern", 0xef9b35, 0xb8651f, Surfaces::Warm),
    ("Dynamite", 0xf26b5b, 0xc0392b, Surfaces::Warm),
    ("Bulldozer", 0xf5c02e, 0x8a6a00, Surfaces::Warm),
    ("Radar", 0x5ccf78, 0x2e7d32, Surfaces::Plain),
    ("Dredge", 0x36c9bf, 0x0f766e, Surfaces::Cool),
    ("Tarp", 0x5ea4f2, 0x1d5fb8, Surfaces::Cool),
    ("Chisel", 0xa98bfa, 0x6d3fc4, Surfaces::Cool),
    ("Flag", 0xf472b6, 0xb8326f, Surfaces::Plain),
    ("Pickaxe", 0xb9c3cd, 0x4b5563, Surfaces::Cool),
];

pub const DEFAULT: &str = "Lantern";

fn rgb(hex: u32) -> Rgb {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// The nine, then the person's own.
pub fn all(settings: &Settings) -> Vec<Palette> {
    let nine = NINE.iter().map(|&(name, dark, light, surfaces)| Palette {
        name: name.into(),
        accent_dark: rgb(dark),
        accent_light: rgb(light),
        surfaces,
        own: false,
    });
    nine.chain(settings.own_themes.iter().map(own)).collect()
}

pub fn is_built_in(name: &str) -> bool {
    NINE.iter().any(|(built_in, ..)| built_in.eq_ignore_ascii_case(name.trim()))
}

/// The theme chosen (Lantern if it's gone).
pub fn current(settings: &Settings) -> Palette {
    let all = all(settings);
    let found = all.iter().position(|palette| palette.name == settings.theme_name).unwrap_or(0);
    all.into_iter().nth(found).expect("the nine")
}

/// A Workshop theme's palette.
pub fn own(theme: &OwnTheme) -> Palette {
    workshop(&theme.name, f32::from(theme.hue), Surfaces::parse(&theme.surfaces))
}

/// The surfaces' base, as theme.slint has it.
pub fn base(dark: bool, surfaces: Surfaces) -> Rgb {
    rgb(match (dark, surfaces) {
        (true, Surfaces::Warm) => 0x1d1b19,
        (true, Surfaces::Plain) => 0x1b1b1b,
        (true, Surfaces::Cool) => 0x191c21,
        (false, Surfaces::Warm) => 0xf7f3ed,
        (false, Surfaces::Plain) => 0xf4f4f4,
        (false, Surfaces::Cool) => 0xf1f4f8,
    })
}

/// Text on the base, as theme.slint has it.
pub fn ink(dark: bool, surfaces: Surfaces) -> Rgb {
    rgb(match (dark, surfaces) {
        (true, Surfaces::Warm) => 0xf3ede5,
        (true, Surfaces::Plain) => 0xededed,
        (true, Surfaces::Cool) => 0xe8edf3,
        (false, Surfaces::Warm) => 0x221c17,
        (false, Surfaces::Plain) => 0x1f1f1f,
        (false, Surfaces::Cool) => 0x18202a,
    })
}

/// The popup's hairline border, drawn by Windows.
pub fn border(dark: bool, surfaces: Surfaces) -> Rgb {
    match (dark, surfaces) {
        (true, Surfaces::Warm) => [62, 57, 52],
        (true, Surfaces::Plain) => [60, 60, 60],
        (true, Surfaces::Cool) => [55, 61, 70],
        (false, Surfaces::Warm) => [219, 209, 197],
        (false, Surfaces::Plain) => [214, 214, 214],
        (false, Surfaces::Cool) => [208, 216, 226],
    }
}

/// What's written on the accent: the base or paper, whichever reads better.
pub fn on_accent(accent: Rgb, surfaces: Surfaces) -> Rgb {
    let dark = base(true, surfaces);
    let paper = if surfaces == Surfaces::Warm { rgb(0xfffaf3) } else { rgb(0xffffff) };
    if contrast(accent, dark) >= contrast(accent, paper) { dark } else { paper }
}

impl Palette {
    pub fn accent(&self, dark: bool) -> Rgb {
        if dark { self.accent_dark } else { self.accent_light }
    }

    fn is_mark_as_drawn(&self) -> bool {
        self.name == DEFAULT && !self.own
    }

    /// Matched letters in results. Lantern's are a touch lighter on dark and deeper on light than its accent, so they read
    /// on both; the others' accents already do.
    pub fn match_color(&self, dark: bool) -> String {
        if self.is_mark_as_drawn() {
            return if dark { "#f2a548" } else { "#a8581a" }.into();
        }
        hex(self.accent(dark))
    }
}

pub fn hex([r, g, b]: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn color([r, g, b]: Rgb) -> slint::Color {
    slint::Color::from_rgb_u8(r, g, b)
}

/// A window's colors for a theme.
pub fn paint(theme: &Theme, palette: &Palette) {
    theme.set_accent_dark(color(palette.accent_dark));
    theme.set_accent_light(color(palette.accent_light));
    theme.set_on_accent_dark(color(on_accent(palette.accent_dark, palette.surfaces)));
    theme.set_on_accent_light(color(on_accent(palette.accent_light, palette.surfaces)));
    theme.set_surfaces(palette.surfaces.index());
}

/// WCAG's relative luminance.
fn luminance(color: Rgb) -> f32 {
    let linear = |value: u8| {
        let value = f32::from(value) / 255.0;
        if value <= 0.039_28 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2])
}

/// WCAG's contrast ratio, 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Hue in degrees, saturation and lightness in percent.
pub fn hsl(hue: f32, saturation: f32, lightness: f32) -> Rgb {
    let (s, l) = (saturation / 100.0, lightness / 100.0);
    let a = s * l.min(1.0 - l);
    let channel = |n: f32| {
        let k = (n + hue / 30.0).rem_euclid(12.0);
        let value = l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        (value * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [channel(0.0), channel(8.0), channel(4.0)]
}

/// Back again: hue in degrees, saturation and lightness 0 to 1.
fn to_hsl([r, g, b]: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max - min < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r { (g - b) / d + if g < b { 6.0 } else { 0.0 } } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    (h * 60.0, s, l)
}

pub fn hue_of(color: Rgb) -> f32 {
    to_hsl(color).0
}

/// A Workshop theme from one hue: a bright accent for dark, a deep one for light, each moved until it reads on its
/// background (4.5:1, what WCAG asks of text).
pub fn workshop(name: &str, hue: f32, surfaces: Surfaces) -> Palette {
    let (dark_base, light_base) = (base(true, surfaces), base(false, surfaces));
    let mut lightness = 60.0;
    let mut dark = hsl(hue, 78.0, lightness);
    while contrast(dark, dark_base) < 4.5 && lightness < 90.0 {
        lightness += 2.0;
        dark = hsl(hue, 78.0, lightness);
    }
    let mut lightness = 40.0;
    let mut light = hsl(hue, 70.0, lightness);
    while contrast(light, light_base) < 4.5 && lightness > 12.0 {
        lightness -= 2.0;
        light = hsl(hue, 70.0, lightness);
    }
    Palette { name: name.into(), accent_dark: dark, accent_light: light, surfaces, own: true }
}

/// What the Workshop says it checked: the color on its background, and text on the color, in dark and light.
pub fn checks(palette: &Palette) -> (Vec<String>, bool) {
    let on_background = (contrast(palette.accent_dark, base(true, palette.surfaces)), contrast(palette.accent_light, base(false, palette.surfaces)));
    let on_color = (
        contrast(palette.accent_dark, on_accent(palette.accent_dark, palette.surfaces)),
        contrast(palette.accent_light, on_accent(palette.accent_light, palette.surfaces)),
    );
    let line = |what: &str, (dark, light): (f32, f32)| format!("{what}: dark {dark:.1}:1, light {light:.1}:1 {}", if dark.min(light) >= 4.5 { "✓" } else { "✗" });
    let ok = [on_background.0, on_background.1, on_color.0, on_color.1].iter().all(|&ratio| ratio >= 4.5);
    (vec![line("The color on the background", on_background), line("Text on the color", on_color)], ok)
}

/// The mark in a theme's colors (straight RGBA, square): its core in the accent, its swirls in the surfaces' family.
/// Lantern is the mark as drawn.
pub fn tint_mark(artwork: &mut [u8], size: usize, palette: &Palette) {
    if palette.is_mark_as_drawn() {
        return;
    }
    let parts = ::mark::parts(artwork, size);
    // The core's own brightness, so its texture carries over to the new color.
    let core: Vec<usize> = (0..size * size).filter(|&index| parts[index] == ::mark::CORE).collect();
    let mean = core.iter().map(|&index| luminance([artwork[index * 4], artwork[index * 4 + 1], artwork[index * 4 + 2]])).sum::<f32>() / core.len().max(1) as f32;
    // The vivid accent in either mode: the core is the brightest thing in the mark.
    let accent = palette.accent_dark.map(f32::from);
    for index in 0..size * size {
        let at = index * 4;
        match parts[index] {
            ::mark::CORE => {
                let lift = luminance([artwork[at], artwork[at + 1], artwork[at + 2]]) / mean.max(1e-4);
                for channel in 0..3 {
                    artwork[at + channel] = (accent[channel] * lift.powf(0.45)).round().clamp(0.0, 255.0) as u8;
                }
            }
            ::mark::DARK | ::mark::LIGHT if palette.surfaces != Surfaces::Warm => {
                let (_, saturation, lightness) = to_hsl([artwork[at], artwork[at + 1], artwork[at + 2]]);
                // Slate for cool, grey for plain, as light or dark as the clay was.
                let [r, g, b] = if palette.surfaces == Surfaces::Cool { hsl(215.0, saturation * 45.0, lightness * 100.0) } else { hsl(0.0, 0.0, lightness * 100.0) };
                (artwork[at], artwork[at + 1], artwork[at + 2]) = (r, g, b);
            }
            _ => {}
        }
    }
}

/// The light that runs along the mark and the core's glow, for a theme (0 to 1); None for the mark as drawn.
pub fn mark_lights(palette: &Palette) -> Option<([f32; 3], [f32; 3])> {
    if palette.is_mark_as_drawn() {
        return None;
    }
    let accent = palette.accent_dark.map(|value| f32::from(value) / 255.0);
    let run = accent.map(|value| 0.72 + 0.28 * value);
    Some((run, accent))
}

/// The artwork the mark is made from, decoded, in a theme's colors.
pub fn mark_artwork(dark: bool, palette: &Palette) -> Result<(u32, Vec<u8>), String> {
    let (width, _, mut artwork) = winsys::imaging::decode_rgba(crate::intro::artwork(dark))?;
    tint_mark(&mut artwork, width as usize, palette);
    Ok((width, artwork))
}

/// The mark, ready to draw in a theme's colors.
pub fn mark(dark: bool, palette: &Palette, pixels: usize) -> Result<::mark::Mark, String> {
    let (width, artwork) = mark_artwork(dark, palette)?;
    let mut mark = ::mark::Mark::new(&artwork, width as usize, pixels.min(width as usize));
    if let Some((run, glow)) = mark_lights(palette) {
        mark.set_lights(run, glow);
    }
    Ok(mark)
}

/// The tray icon in a theme's colors, `size` pixels a side, for a light taskbar and a dark one (straight RGBA). None for
/// Lantern: the executable's own icons are its.
pub fn tray_artwork(palette: &Palette, size: u32) -> Option<winsys::tray::Artwork> {
    if palette.is_mark_as_drawn() {
        return None;
    }
    let draw = |dark: bool| -> Option<Vec<u8>> {
        let mark = mark(dark, palette, size as usize).ok().filter(|mark| mark.size as u32 == size)?;
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        mark.still(&mut pixels);
        for pixel in pixels.chunks_exact_mut(4) {
            let alpha = f32::from(pixel[3]) / 255.0;
            if alpha > 0.0 {
                for channel in &mut pixel[..3] {
                    *channel = (f32::from(*channel) / alpha).round().min(255.0) as u8;
                }
            }
        }
        Some(pixels)
    };
    Some(winsys::tray::Artwork { size, for_light_taskbar: draw(false)?, for_dark_taskbar: draw(true)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icons_in_the_theme() {
        // (Decoding the artwork goes through Windows' imaging, as on the UI thread.)
        winsys::com_mta();
        let settings = Settings::default();
        let palettes = all(&settings);
        assert!(tray_artwork(&palettes[0], 16).is_none(), "Lantern uses the executable's icons");
        let tarp = palettes.iter().find(|palette| palette.name == "Tarp").unwrap();
        let artwork = tray_artwork(tarp, 20).unwrap();
        assert_eq!(artwork.for_light_taskbar.len(), 20 * 20 * 4);
        assert_ne!(artwork.for_light_taskbar, artwork.for_dark_taskbar);
        // The core, in the middle, is Tarp's blue.
        let [r, _, b, _] = <[u8; 4]>::try_from(&artwork.for_dark_taskbar[(10 * 20 + 10) * 4..(10 * 20 + 11) * 4]).unwrap();
        assert!(b > r, "core {r} {b}");
    }

    #[test]
    fn the_nine_read_on_their_backgrounds() {
        for palette in all(&Settings::default()) {
            assert!(contrast(palette.accent_dark, base(true, palette.surfaces)) >= 4.5, "{} on dark", palette.name);
            // Lantern's copper is the mark's own; its matched letters are deeper (match_color).
            if palette.name != DEFAULT {
                assert!(contrast(palette.accent_light, base(false, palette.surfaces)) >= 4.5, "{} on light", palette.name);
            }
            assert!(contrast(palette.accent_dark, on_accent(palette.accent_dark, palette.surfaces)) >= 4.5, "text on {}", palette.name);
        }
    }

    #[test]
    fn workshop_themes_always_read() {
        for surfaces in [Surfaces::Warm, Surfaces::Plain, Surfaces::Cool] {
            for hue in (0..360).step_by(5) {
                let palette = workshop("Test", hue as f32, surfaces);
                assert!(checks(&palette).1, "hue {hue}, {surfaces:?}: {:?}", checks(&palette).0);
            }
        }
    }

    #[test]
    fn hues_go_round() {
        for hue in [0.0, 37.0, 120.0, 200.0, 300.0] {
            let back = hue_of(hsl(hue, 78.0, 55.0));
            assert!((back - hue).abs() < 1.5, "{hue} came back as {back}");
        }
        assert_eq!(hex(hsl(0.0, 100.0, 50.0)), "#ff0000");
    }

    #[test]
    fn a_theme_that_is_gone_falls_back_to_lantern() {
        let settings = Settings { theme_name: "Nowhere".into(), ..Settings::default() };
        assert_eq!(current(&settings).name, DEFAULT);
        let settings = Settings { theme_name: "Mine".into(), own_themes: vec![OwnTheme { name: "Mine".into(), hue: 120, surfaces: "cool".into() }], ..Settings::default() };
        let palette = current(&settings);
        assert!(palette.own && palette.surfaces == Surfaces::Cool);
    }
}
