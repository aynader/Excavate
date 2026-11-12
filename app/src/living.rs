//! The mark in the settings window's rail. During onboarding it's always alive, pausing only when the window is
//! minimized; in settings it's still when there's nothing to do. Alive, a light travels along the spiral from tail to
//! tail and back, and the core breathes.

use crate::{SettingsWindow, Theme};
use crate::settings::log;
use crate::themes::{self, Palette};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// 88 px in settings.slint.
const SIZE: f32 = 88.0;

struct Living {
    window: slint::Weak<SettingsWindow>,
    /// Onboarding: alive whatever the indexer is doing, and whether the window is in front or not.
    always: bool,
    /// The theme to draw it in.
    palette: Palette,
    /// Made for dark or light, a theme, and a size in pixels (a monitor of another scale needs another).
    mark: Option<(bool, Palette, usize, ::mark::Mark)>,
    timer: Option<slint::Timer>,
    /// A theme's colors reach the mark a moment after the window's: making the mark takes longer than a slider's step.
    repaint: slint::Timer,
    begun: Instant,
}

thread_local! {
    static LIVING: RefCell<Option<Living>> = const { RefCell::new(None) };
}

pub fn attach(window: &SettingsWindow, always: bool, palette: Palette) {
    let living = Living { window: window.as_weak(), always, palette, mark: None, timer: None, repaint: slint::Timer::default(), begun: Instant::now() };
    LIVING.with(|slot| *slot.borrow_mut() = Some(living));
    paint_still();
    if always {
        run(true);
    }
}

pub fn detach() {
    LIVING.with(|slot| slot.borrow_mut().take());
}

/// The window's theme changed.
pub fn repaint(palette: Palette) {
    LIVING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(living) = slot.as_mut() else { return };
        living.palette = palette;
        living.repaint.start(slint::TimerMode::SingleShot, Duration::from_millis(120), paint_still);
    });
}

/// The window moved to a monitor of another scale: the mark is made again at its new size.
pub fn rescaled() {
    paint_still();
}

/// The mark when it isn't alive: the artwork as drawn for Lantern, made from it for the others.
fn paint_still() {
    LIVING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(living) = slot.as_mut() else { return };
        let Some(window) = living.window.upgrade() else { return };
        if living.palette.name == themes::DEFAULT && !living.palette.own {
            window.set_mark_tinted_on(false);
            return;
        }
        if !prepared(living, &window) {
            return;
        }
        let Some((_, _, _, mark)) = living.mark.as_ref() else { return };
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(mark.size as u32, mark.size as u32);
        mark.still(buffer.make_mut_bytes());
        window.set_mark_tinted(slint::Image::from_rgba8_premultiplied(buffer));
        window.set_mark_tinted_on(true);
    });
}

/// The indexer is reading, or not.
pub fn reading(on: bool) {
    let always = LIVING.with(|slot| slot.borrow().as_ref().is_some_and(|living| living.always));
    if !always {
        run(on);
    }
}

/// Starts or stops it. Follows Windows' animation effects.
fn run(on: bool) {
    LIVING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(living) = slot.as_mut() else { return };
        let on = on && winsys::theme::animations_enabled();
        if on == living.timer.is_some() {
            return;
        }
        if !on {
            living.timer = None;
            if let Some(window) = living.window.upgrade() {
                window.set_mark_alive(false);
            }
            return;
        }
        let timer = slint::Timer::default();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(33), frame);
        living.timer = Some(timer);
    });
}

/// The mark for the theme now, made once for each it's asked for.
fn prepared(living: &mut Living, window: &SettingsWindow) -> bool {
    let dark = window.global::<Theme>().get_dark();
    let pixels = (SIZE * window.window().scale_factor()).round() as usize;
    if living.mark.as_ref().is_some_and(|(made_for, palette, size, _)| *made_for == dark && *palette == living.palette && *size == pixels) {
        return true;
    }
    match themes::mark(dark, &living.palette, pixels) {
        Ok(mark) => {
            living.mark = Some((dark, living.palette.clone(), pixels, mark));
            true
        }
        Err(error) => {
            log(&format!("the living mark: {error}"));
            false
        }
    }
}

fn frame() {
    LIVING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(living) = slot.as_mut() else { return };
        let Some(window) = living.window.upgrade() else { return };
        // The launch animation is still on its way here.
        if !window.get_mark_landed() {
            return;
        }
        // Nobody's watching: minimized, or in settings behind other windows.
        let handle = winsys::window::find("Excavate.Settings");
        let watched = if living.always { !winsys::window::is_minimized(handle) } else { winsys::window::is_foreground(handle) };
        if !watched || !prepared(living, &window) {
            return;
        }
        let Some((_, _, _, mark)) = living.mark.as_ref() else { return };
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(mark.size as u32, mark.size as u32);
        mark.alive(living.begun.elapsed().as_secs_f32(), buffer.make_mut_bytes());
        window.set_mark_frame(slint::Image::from_rgba8_premultiplied(buffer));
        window.set_mark_alive(true);
    });
}
