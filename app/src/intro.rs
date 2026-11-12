//! The launch animation: when onboarding opens, and on the first launch after an update. A layered window over the
//! screen's center, on a thread of its own, with frames from the mark's own pixels; then the mark shrinks and flies to
//! its place in the onboarding window's rail, where the window's own mark takes over. Nothing's left of it once it ends.

use crate::settings::log;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// The artwork the living mark is made from: on the dark theme, with its dark swirl lifted.
pub fn artwork(dark: bool) -> &'static [u8] {
    if dark { include_bytes!("../../brand/mark-dark-512.png") } else { include_bytes!("../../brand/mark-512.png") }
}

/// The mark on screen while it draws, in pixels at 100% (at most the artwork's own size).
const SIZE: f32 = 300.0;
/// When the flight begins, and how long it takes.
const FLY_AT: f32 = 1.2;
const FLIGHT: f32 = 0.42;

/// Whether it plays now: the first time for this version, unless Windows' animation effects are off.
pub fn wanted(version_seen: &str) -> bool {
    version_seen != env!("CARGO_PKG_VERSION") && winsys::theme::animations_enabled()
}

/// Plays it on a monitor. With `to_rail`, the onboarding window is brought up (and asked where its mark is) as the flight
/// begins; without, the mark fades where it is. `done` runs on the UI thread at the end.
pub fn play(monitor: winsys::display::Monitor, to_rail: bool, dark: bool, palette: crate::themes::Palette, done: impl FnOnce() + Send + 'static) {
    let started = std::thread::Builder::new().name("intro".into()).spawn(move || {
        winsys::com_mta();
        if let Err(error) = run(monitor, to_rail, dark, &palette) {
            log(&format!("launch animation: {error}"));
        }
        let _ = slint::invoke_from_event_loop(done);
    });
    if let Err(error) = started {
        log(&format!("launch animation: {error}"));
    }
}

fn run(monitor: winsys::display::Monitor, to_rail: bool, dark: bool, palette: &crate::themes::Palette) -> Result<(), String> {
    let mark = crate::themes::mark(dark, palette, (SIZE * monitor.scale).round() as usize)?;
    let size = mark.size;
    let window = winsys::layered::Layered::new(size)?;
    let left = monitor.work.left + (monitor.work.width() - size as i32) / 2;
    let top = monitor.work.top + (monitor.work.height() - size as i32) / 2;
    let mut frame = vec![0u8; size * size * 4];
    let begun = Instant::now();
    // Drawing in. It keeps going through the flight (the light's run isn't over yet).
    let mut seconds = 0.0;
    while seconds < FLY_AT {
        mark.intro(seconds, &mut frame);
        window.present(left, top, size, &frame, 255);
        window.next_frame();
        seconds = begun.elapsed().as_secs_f32();
    }
    // The window comes up, and says where the mark goes.
    let target = if to_rail {
        let (sender, receiver) = channel();
        let _ = slint::invoke_from_event_loop(move || crate::with_app(|app| {
            let _ = sender.send(crate::setup::intro_reveal(app));
        }));
        receiver.recv_timeout(Duration::from_millis(600)).ok().flatten()
    } else {
        None
    };
    let mut smaller = vec![0u8; size * size * 4];
    let flight_begun = Instant::now();
    loop {
        let progress = (flight_begun.elapsed().as_secs_f32() / FLIGHT).min(1.0);
        mark.intro(begun.elapsed().as_secs_f32(), &mut frame);
        match target {
            Some((target_left, target_top, target_size)) => {
                // Slow in, slow out.
                let eased = if progress < 0.5 { 4.0 * progress * progress * progress } else { 1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0 };
                let now_size = (size as f32 + (target_size as f32 - size as f32) * eased).round().max(1.0) as usize;
                let now_left = left as f32 + (target_left - left) as f32 * eased;
                let now_top = top as f32 + (target_top - top) as f32 * eased;
                ::mark::scale(&frame, size, &mut smaller, now_size);
                window.present(now_left.round() as i32, now_top.round() as i32, now_size, &smaller, 255);
            }
            None => {
                // Nowhere to land: it fades, drawing back a little.
                let eased = progress * progress;
                let now_size = ((size as f32) * (1.0 - 0.06 * eased)).round() as usize;
                let inset = (size - now_size) as i32 / 2;
                ::mark::scale(&frame, size, &mut smaller, now_size);
                window.present(left + inset, top + inset, now_size, &smaller, ((1.0 - eased) * 255.0) as u8);
            }
        }
        window.next_frame();
        if progress >= 1.0 {
            break;
        }
    }
    if target.is_some() {
        // The window's own mark takes over under this one, before this one goes.
        let _ = slint::invoke_from_event_loop(|| crate::with_app(|app| crate::setup::intro_landed(app)));
        for _ in 0..4 {
            window.next_frame();
        }
    }
    log(&format!("launch animation: {:.2} s, {size} px", begun.elapsed().as_secs_f32()));
    Ok(())
}
