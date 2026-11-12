// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use std::rc::{Rc, Weak};

use i_slint_core::timers::{Timer, TimerMode};

#[cfg(target_vendor = "apple")]
mod apple_display_link;

use crate::winitwindowadapter::WinitWindowAdapter;

pub fn create_frame_throttle(
    window_adapter: Weak<WinitWindowAdapter>,
    _winit_window: &winit::window::Window,
    _is_wayland: bool,
) -> Box<dyn FrameThrottle> {
    if _is_wayland {
        WinitBasedFrameThrottle::create()
    } else {
        #[cfg(target_vendor = "apple")]
        if let Some(throttle) =
            apple_display_link::try_create(window_adapter.clone(), _winit_window)
        {
            return throttle;
        }
        TimerBasedFrameThrottle::create(window_adapter)
    }
}

pub trait FrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window);
}

struct TimerBasedFrameThrottle {
    window_adapter: Weak<WinitWindowAdapter>,
    timer: Rc<Timer>,
}

impl TimerBasedFrameThrottle {
    fn create(window_adapter: Weak<WinitWindowAdapter>) -> Box<dyn FrameThrottle> {
        Box::new(Self { window_adapter, timer: Rc::new(Timer::default()) })
    }
}

impl FrameThrottle for TimerBasedFrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window) {
        if self.timer.running() {
            return;
        }
        // A hidden window gets no RedrawRequested, so `pending_redraw` would never clear and the timer would tick at
        // the display's refresh rate until the window is shown again. Showing it draws it anyway.
        if self.window_adapter.upgrade().is_some_and(|adapter| is_hidden(&adapter)) {
            return;
        }
        let refresh_interval_millihertz = winit_window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .unwrap_or(60000) as u64;
        let window_adapter = self.window_adapter.clone();
        let timer = Rc::downgrade(&self.timer);
        let interval =
            std::time::Duration::from_millis((1000 * 1000) / refresh_interval_millihertz);
        // Excavate: the first change after a quiet moment is drawn at once (a keystroke's frame was a whole refresh
        // interval late, ~16 ms at 60 Hz, waiting for the timer's first tick); the timer then paces what follows.
        winit_window.request_redraw();
        self.timer.start(TimerMode::Repeated, interval, move || {
            let Some(timer) = timer.upgrade() else { return };
            let Some(adapter) = window_adapter.upgrade() else { return };
            if adapter.pending_redraw() && !is_hidden(&adapter) {
                redraw_now(&window_adapter);
            } else {
                timer.stop();
            }
        });
    }
}

fn is_hidden(adapter: &WinitWindowAdapter) -> bool {
    matches!(adapter.visibility(), crate::winitwindowadapter::WindowVisibility::Hidden)
}

fn redraw_now(window_adapter: &Weak<WinitWindowAdapter>) {
    let Some(winit_window) = window_adapter.upgrade().and_then(|adapter| adapter.winit_window())
    else {
        return;
    };
    winit_window.request_redraw();
}

struct WinitBasedFrameThrottle;

impl WinitBasedFrameThrottle {
    fn create() -> Box<dyn FrameThrottle> {
        Box::new(Self)
    }
}

impl FrameThrottle for WinitBasedFrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window) {
        winit_window.request_redraw();
    }
}
