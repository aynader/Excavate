//! Showing and hiding the popup at the taskbar, its window's Windows 11 styling, what results do, and the menus Windows
//! draws itself (tray, right-click).

use crate::app::{App, Target};
use crate::settings::{Settings, log};
use crate::{
    Popup, RUN_NAME, TRAY_LOG, TRAY_OPEN, TRAY_PAUSE_HOUR, TRAY_PAUSE_TOMORROW, TRAY_PAUSE_UNTIL_ON, TRAY_QUIT, TRAY_RESUME,
    TRAY_SETTINGS, TRAY_START_WITH_WINDOWS, Theme, with_app,
};
use everything::Sort;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use std::time::{Duration, Instant};
use winsys::menu::Item;
use winsys::tray::{Event, Hotkey, MenuItem, Options, Tray};

/// The smallest size the popup lays out at, in logical pixels: also `min-width` and `min-height` in app.slint.
const SMALLEST: (u32, u32) = (720, 480);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Hotkey,
    Tray,
}

pub fn is_dark(settings: &Settings) -> bool {
    match settings.theme.as_str() {
        "dark" => true,
        "light" => false,
        _ => !winsys::theme::apps_light(),
    }
}

pub fn apply_theme(ui: &Popup, settings: &Settings) {
    let theme = ui.global::<Theme>();
    theme.set_dark(is_dark(settings));
    crate::themes::paint(&theme, &crate::themes::current(settings));
    theme.set_translucent(settings.acrylic);
    theme.set_animate(winsys::theme::animations_enabled());
}

/// The popup's native window, once Slint has created it (it does so lazily, in the event loop after `show()`). Its
/// window class is Excavate's own, so `FindWindow` finds it when the raw handle isn't available yet.
fn hwnd(ui: &Popup) -> isize {
    let handle = ui.window().window_handle();
    if let Ok(RawWindowHandle::Win32(win32)) = handle.window_handle().map(|handle| handle.as_raw()) {
        return win32.hwnd.get();
    }
    winsys::window::find("Excavate.Popup")
}

/// Styles the window the first time it exists (and again if Slint recreated it), and returns it.
fn window_ready(app: &App) -> isize {
    let Some(ui) = app.ui.upgrade() else { return 0 };
    let handle = hwnd(&ui);
    if handle != 0 && app.hwnd.get() != handle {
        app.hwnd.set(handle);
        style_window(app, handle);
    }
    handle
}

/// Takes the foreground and the keyboard: now if the window exists, else as soon as the event loop has created it.
fn take_keyboard(app: &App, retry: bool) {
    let handle = window_ready(app);
    if handle == 0 {
        if retry {
            let _ = slint::invoke_from_event_loop(|| with_app(|app| take_keyboard(app, false)));
        } else {
            log("the popup's window doesn't exist yet");
        }
        return;
    }
    if !winsys::window::activate(handle) {
        log("the popup couldn't take the foreground");
    }
    if let Some(ui) = app.ui.upgrade() {
        ui.invoke_focus_field();
    }
}

/// Takes the keyboard back after a system dialog (Windows Hello) had it.
pub fn take_back_keyboard(app: &App) {
    take_keyboard(app, false);
}

/// The monitor Excavate opens on: the one chosen, or the one the mouse is on (also while the one chosen is unplugged).
pub fn opening_monitor(settings: &Settings) -> winsys::display::Monitor {
    if !settings.monitor.is_empty() {
        if let Some(screen) = winsys::display::screens().into_iter().find(|screen| screen.id == settings.monitor) {
            return screen.monitor;
        }
    }
    let (x, y) = winsys::display::cursor();
    winsys::display::monitor_at(x, y)
}

/// Opens the popup. Only a window that exists can be kept out of sight while it draws, so the first time it's created first.
pub fn show(app: &App) {
    app.show_started.set(Some(Instant::now()));
    // The search helper started now rather than when the typing pauses: its models are loaded by then. A moment after
    // the window is out, so opening isn't slowed by starting a process.
    let warm = { let settings = app.settings.borrow(); settings.seeing_on && settings.setup_done };
    if warm {
        slint::Timer::single_shot(std::time::Duration::from_millis(80), || crate::with_app(|app| app.seeker.warm()));
    }
    if app.hwnd.get() != 0 {
        open(app);
        return;
    }
    app.show_wanted.set(true);
    if !app.warming.get() {
        prewarm(app);
    }
}

/// Places the popup at the taskbar of the monitor it opens on, shows it, and gives it the keyboard. Slint shows a window first
/// and draws it afterward, and Windows discards what's drawn into a hidden one: for a moment there'd be a blank, white window. So
/// Windows keeps it off the screen until its first frame is out.
///
/// Closed, it isn't hidden but cloaked (`park`): what it showed stays drawn, already the empty search, so opening it
/// again needs only the small frame that gives the search field its caret before it's revealed.
fn open(app: &App) {
    let Some(ui) = app.ui.upgrade() else { return };
    let monitor = opening_monitor(&app.settings.borrow());
    app.scale.set(monitor.scale);
    let margin = (12.0 * monitor.scale).round() as i32;
    // As it was left, if that fits here. If not it's smaller on this monitor, and the size saved stays.
    let fitted = |saved: u32, smallest: u32, room: i32| ((saved.max(smallest) as f32 * monitor.scale).round() as i32).min(room - 2 * margin).max(1);
    let (width, height) = {
        let settings = app.settings.borrow();
        (fitted(settings.popup_width, SMALLEST.0, monitor.work.width()), fitted(settings.popup_height, SMALLEST.1, monitor.work.height()))
    };
    app.opened_size.set((width as u32, height as u32));
    let (left, top) = winsys::display::place(&monitor, monitor.taskbar_edge(), winsys::display::taskbar_centered(), width, height, margin);
    let window = ui.window();
    // In logical pixels until the window exists: physical ones would be read with the scale of a monitor it won't be on.
    let size = |window: &slint::Window, exists: bool| {
        if exists {
            window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
        } else {
            window.set_size(slint::LogicalSize::new(width as f32 / monitor.scale, height as f32 / monitor.scale));
        }
    };
    let exists = app.hwnd.get() != 0;
    // Parked on this screen at this size: what it shows is already right (the empty search), so it's revealed at once.
    let unchanged = window.size() == slint::PhysicalSize::new(width as u32, height as u32) && (window.scale_factor() - monitor.scale).abs() < 0.01;
    // (Already showing, there's no frame on its way, and nothing to hide.)
    let parked = app.parked.take();
    if !app.visible.get() {
        app.before.set(winsys::window::foreground());
    }
    if exists && !app.visible.get() && (parked || winsys::window::cloak(app.hwnd.get(), true)) {
        app.cloaked.set(true);
        // In case no frame comes: it can't stay invisible.
        app.uncloak_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(150), || with_app(|app| uncloak(app)));
    }
    // Moved, sized, then moved again: arriving on a screen of another scale makes Windows rescale the window, so it
    // is sized once it's there (EverythingToolbar does the same).
    window.set_position(slint::PhysicalPosition::new(left, top));
    size(window, exists);
    if let Err(error) = ui.show() {
        log(&format!("can't show the popup: {error}"));
        uncloak(app);
        return;
    }
    window.set_position(slint::PhysicalPosition::new(left, top));
    size(window, exists);
    app.visible.set(true);
    take_keyboard(app, true);
    // A frame always comes (the caret's, at least), and with it the window is revealed: or now, when nothing about it
    // changed while parked.
    window.request_redraw();
    if parked && unchanged && app.frames.get() > app.park_frames.get() {
        uncloak(app);
        if let Some(started) = app.show_started.take() {
            log(&format!("popup on screen in {:.1} ms (revealed)", started.elapsed().as_secs_f64() * 1000.0));
            crate::bench::shown(started.elapsed());
        }
    }
    if app.hot_age() > Duration::from_secs(120) {
        app.refresh_hot();
    }
    if app.query_text().is_empty() {
        app.edited("");
    }
    app.follow_reading(true);
}

fn uncloak(app: &App) {
    app.uncloak_timer.stop();
    if app.cloaked.take() {
        winsys::window::cloak(app.hwnd.get(), false);
    }
}

/// Creates the window once while nothing's visible (started with Windows), so the first Alt+S is as fast as the rest.
pub fn prewarm(app: &App) {
    let Some(ui) = app.ui.upgrade() else { return };
    let window = ui.window();
    window.set_position(slint::PhysicalPosition::new(-32_000, -32_000));
    app.warming.set(true);
    if ui.show().is_ok() {
        // Hidden once the event loop has created (and styled) the window.
        let _ = slint::invoke_from_event_loop(|| with_app(|app| {
            app.warming.set(false);
            window_ready(app);
            if !app.visible.get() {
                if let Some(ui) = app.ui.upgrade() {
                    let _ = ui.hide();
                }
            }
            // Wanted already. Once the news that hiding it lost the keyboard is in, or that would close it again.
            if app.show_wanted.take() {
                let _ = slint::invoke_from_event_loop(|| with_app(|app| open(app)));
            }
        }));
    } else {
        app.warming.set(false);
    }
}

fn style_window(app: &App, handle: isize) {
    let Some(ui) = app.ui.upgrade() else { return };
    let dark = ui.global::<Theme>().get_dark();
    winsys::window::make_tool_window(handle);
    let [r, g, b] = crate::themes::border(dark, crate::themes::current(&app.settings.borrow()).surfaces);
    winsys::window::style_popup(handle, dark, (r, g, b), app.settings.borrow().acrylic);
}

pub fn hide(app: &App) {
    let Some(ui) = app.ui.upgrade() else { return };
    if !app.visible.get() {
        return;
    }
    app.visible.set(false);
    app.stop_video();
    app.hidden_at.set(Some(Instant::now()));
    keep_size(app, &ui);
    let theme = ui.global::<Theme>();
    theme.set_menus_closed(theme.get_menus_closed() + 1);
    app.follow_reading(false);
    // What was revealed is hidden again next time.
    app.revealed.borrow_mut().take();
    park(app, &ui);
    // So nobody opens it later and wonders where their files went.
    app.image_text_off();
    if ui.get_enlarged() {
        ui.invoke_close_enlarged();
    }
    // However it closed. Once what closed it is done with the search (what's recorded for a result opened), and before anything
    // can open it again, so nothing old shows.
    if app.settings.borrow().new_search_each_time {
        let _ = slint::invoke_from_event_loop(|| with_app(|app| {
            if !app.visible.get() {
                app.new_search();
            }
        }));
    }
    // Parked: once that's in, a frame of it (counted; opening reveals the window at once only after it).
    app.park_frames.set(u64::MAX);
    if app.parked.get() {
        let _ = slint::invoke_from_event_loop(|| with_app(|app| {
            app.park_frames.set(app.frames.get());
            if let Some(ui) = app.ui.upgrade() {
                ui.window().request_redraw();
            }
        }));
    }
}

/// Closes the popup by cloaking it, not hiding it, so what's drawn in it stays: the keyboard goes back to the window
/// that had it before. Nothing in it moves meanwhile (the reading line, the only animation left, is cleared; opening
/// shows it again at once). Hidden after all when cloaking fails or that window is gone.
fn park(app: &App, ui: &Popup) {
    let handle = app.hwnd.get();
    ui.set_reading(slint::SharedString::new());
    // (EXCAVATE_NO_PARK: hidden as before, to compare.)
    if handle != 0 && std::env::var_os("EXCAVATE_NO_PARK").is_none() && winsys::window::cloak(handle, true) {
        if winsys::window::give_back(handle, app.before.get()) {
            app.parked.set(true);
            return;
        }
        winsys::window::cloak(handle, false);
    }
    let _ = ui.hide();
}

/// A size dragged by hand is kept. Windows resizes the window too, arriving on a monitor of another scale, which isn't the person's doing.
fn keep_size(app: &App, ui: &Popup) {
    let window = ui.window();
    let (size, scale) = (window.size(), window.scale_factor());
    let opened = app.opened_size.get();
    let same = size.width.abs_diff(opened.0) <= 1 && size.height.abs_diff(opened.1) <= 1;
    // (Closed before its first frame: it may not be at that size yet.)
    if same || app.show_started.get().is_some() || (scale - app.scale.get()).abs() > 0.01 {
        return;
    }
    {
        let mut settings = app.settings.borrow_mut();
        settings.popup_width = (size.width as f32 / scale).round() as u32;
        settings.popup_height = (size.height as f32 / scale).round() as u32;
        settings.save(&app.paths);
    }
    crate::setup::popup_size_changed(app);
}

pub fn toggle(app: &App, trigger: Trigger) {
    if app.visible.get() {
        hide(app);
        return;
    }
    // Clicking the tray icon while the popup is open first takes its focus (which hides it), then arrives here.
    let just_hidden = app.hidden_at.get().is_some_and(|at| at.elapsed() < Duration::from_millis(300));
    if trigger == Trigger::Tray && just_hidden {
        return;
    }
    show(app);
}

pub fn wire(ui: &Popup) {
    ui.on_edited(|text| with_app(|app| app.edited(&text)));
    ui.on_activated(|index, action| with_app(|app| act(app, index.max(0) as usize, &action)));
    ui.on_filter_chosen(|index| with_app(|app| app.filter_chosen(index.max(0) as usize)));
    ui.on_text_bytes(|text| text.len() as i32);
    ui.on_toggle_chosen(|index| with_app(|app| app.toggle_chosen(index)));
    ui.on_enlarged_changed(|on| with_app(|app| app.enlarged_changed(on)));
    ui.on_enlarged_zoomed(|settling| with_app(|app| app.enlarged_zoomed(settling)));
    ui.on_enlarged_move(|by| with_app(|app| app.enlarged_move(by)));
    ui.on_selection_moved(|index| with_app(|app| app.selection_moved(index)));
    ui.on_visible_rows(|first, count| with_app(|app| app.visible_rows(first, count)));
    ui.on_context_menu(|index, x, y| with_app(|app| context_menu(app, index.max(0) as usize, x, y)));
    ui.on_sort_chosen(|index| with_app(|app| match index {
        0 => app.choose_fuzzy(false),
        1 => app.choose_fuzzy(true),
        _ => app.choose_sort([Some(Sort::NameAscending), Some(Sort::ModifiedDescending), Some(Sort::SizeDescending), Some(Sort::PathAscending), Some(Sort::DateRunDescending)][(index - 2).clamp(0, 4) as usize]),
    }));
    ui.on_show_more(|| with_app(|app| app.show_all()));
    ui.on_escape(|| with_app(|app| hide(app)));
    ui.on_settings_clicked(|| with_app(|app| {
        hide(app);
        crate::setup::open_on(app, crate::setup::GENERAL);
    }));
    ui.on_reveal(|| with_app(|app| app.reveal()));
    ui.on_preview_media(|| with_app(|app| app.media_clicked()));
    // The preview's edge or corner let go: its size is kept, for the next time and the next start.
    ui.on_preview_resized(|| with_app(|app| {
        let Some(ui) = app.ui.upgrade() else { return };
        let (width, image) = (ui.get_preview_dragged(), ui.get_preview_image_dragged());
        let mut settings = app.settings.borrow_mut();
        if settings.preview_dragged_width != width || settings.preview_dragged_image != image {
            settings.preview_dragged_width = width;
            settings.preview_dragged_image = image;
            settings.save(&app.paths);
        }
    }));
    ui.on_banner_clicked(|| with_app(|app| banner_clicked(app)));
    ui.on_reading_clicked(|| with_app(|app| {
        // Paused by the person: a click resumes it.
        if app.settings.borrow().paused(crate::app::now()) {
            pause(app, 0);
            app.follow_reading(true);
            return;
        }
        hide(app);
        crate::setup::open_on(app, crate::setup::ACTIVITY);
    }));
    ui.on_preview_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().preview = on;
        app.settings.borrow().save(&app.paths);
        crate::setup::preview_changed(app);
    }));

    ui.window().on_winit_window_event(|_window, event| {
        match event {
            // Losing the keyboard to another window hides the popup, as Start and Search do.
            winit::event::WindowEvent::Focused(focused) if !*focused => with_app(|app| {
                if app.visible.get() && !crate::bench::running() && !app.verifying.get() {
                    hide(app);
                }
            }),
            // Slint draws (and presents) right after this event; what's queued now runs once the frame is out. Works
            // with every renderer, unlike rendering notifiers (not offered by the software renderer).
            winit::event::WindowEvent::RedrawRequested => {
                let _ = slint::invoke_from_event_loop(|| with_app(|app| frame_drawn(app)));
            }
            winit::event::WindowEvent::MouseWheel { .. } => with_app(|app| {
                if app.ui.upgrade().is_some_and(|ui| ui.get_enlarged()) {
                    app.wheel_at.set(Some(Instant::now()));
                }
            }),
            _ => {}
        }
        EventResult::Propagate
    });
}

fn frame_drawn(app: &App) {
    app.frames.set(app.frames.get() + 1);
    uncloak(app);
    if let Some(at) = app.wheel_at.take() {
        log(&format!("zoomed in {:.1} ms", at.elapsed().as_secs_f64() * 1000.0));
    }
    // (Not the frames of a window being created out of sight.)
    if app.visible.get() {
        if let Some(started) = app.show_started.take() {
            log(&format!("popup on screen in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0));
            crate::bench::shown(started.elapsed());
        }
    }
    crate::bench::frame();
}

pub fn start_tray(app: &App) {
    let hotkey_text = app.settings.borrow().hotkey.clone();
    let hotkey: Hotkey = hotkey_text.parse().unwrap_or_else(|error| {
        log(&format!("hotkey \"{hotkey_text}\": {error}; using Alt+S"));
        "Alt+S".parse().unwrap()
    });
    let options = Options {
        tooltip: format!("Excavate ({hotkey})"),
        icon_for_dark_taskbar: 2,
        icon_for_light_taskbar: 3,
        hotkey,
    };
    let started = Tray::start(options, tray_menu(app, &hotkey.to_string()), |event| {
        // Windows is shutting down: the own index is saved here and now (the event loop may not get another turn).
        if matches!(event, winsys::tray::Event::EndSession) {
            crate::own_index::save_now();
            return;
        }
        let _ = slint::invoke_from_event_loop(move || with_app(|app| tray_event(app, event)));
    });
    match started {
        Ok((tray, hearing)) => {
            match hearing {
                Ok(winsys::tray::Hearing::Registered) => log(&format!("hotkey {hotkey} registered")),
                Ok(winsys::tray::Hearing::Hooked) => {
                    log(&format!("hotkey {hotkey}: another app registered it too; Excavate catches it first"));
                }
                Err(error) => {
                    log(&format!("hotkey: {error}"));
                    app.notice(&format!("{error}: open Excavate from its tray icon"));
                }
            }
            TRAY.with(|slot| *slot.borrow_mut() = Some(tray));
            tray_theme(&app.settings.borrow());
        }
        Err(error) => log(&format!("tray: {error}")),
    }
}

/// The tray icon in the theme's colors.
fn tray_theme(settings: &Settings) {
    let artwork = crate::themes::tray_artwork(&crate::themes::current(settings), winsys::tray::icon_size());
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow().as_ref() {
            tray.set_artwork(artwork);
        }
    });
}

thread_local! {
    static TRAY: std::cell::RefCell<Option<Tray>> = const { std::cell::RefCell::new(None) };
}

/// Stops listening for the hotkey while a new one is recorded.
pub fn tray_suspend() {
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow().as_ref() {
            tray.suspend_hotkey();
        }
    });
}

/// Listens for a new hotkey (or the old one again).
pub fn tray_set_hotkey(app: &App, hotkey: Hotkey) -> Result<winsys::tray::Hearing, String> {
    let result = TRAY.with(|tray| tray.borrow().as_ref().map(|tray| tray.set_hotkey(hotkey))).unwrap_or_else(|| Err("no tray".into()));
    if result.is_ok() {
        TRAY.with(|tray| {
            if let Some(tray) = tray.borrow().as_ref() {
                tray.set_menu(tray_menu(app, &hotkey.to_string()));
            }
        });
    }
    result
}

pub fn tray_hearing() -> Option<winsys::tray::Hearing> {
    TRAY.with(|tray| tray.borrow().as_ref().and_then(Tray::hearing))
}

pub fn refresh_tray_menu(app: &App) {
    let hotkey = app.settings.borrow().hotkey.clone();
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow().as_ref() {
            tray.set_menu(tray_menu(app, &hotkey));
        }
    });
}

/// The theme changed (Windows' mode, or the setting): every window follows.
pub fn theme_changed(app: &App) {
    if let Some(ui) = app.ui.upgrade() {
        apply_theme(&ui, &app.settings.borrow());
        if app.hwnd.get() != 0 {
            style_window(app, app.hwnd.get());
        }
        // Matched letters are colored for the theme: redraw the rows.
        app.edited(&app.query_text());
    }
    tray_theme(&app.settings.borrow());
    crate::setup::apply_theme(app);
}

/// Pauses reading until a time (seconds since 1970), or -1 until turned back on; 0 resumes it.
pub fn pause(app: &App, until: i64) {
    app.settings.borrow_mut().paused_until = until;
    app.settings.borrow().save(&app.paths);
    log(&format!("reading paused until {until}"));
    if app.settings.borrow().setup_done {
        app.indexer.settings_changed();
    }
    refresh_tray_menu(app);
    crate::setup::pause_changed(app);
}

/// What the pause offers: for a while, until tomorrow, until turned back on; or to resume.
fn pause_items(app: &App) -> Vec<MenuItem> {
    if app.settings.borrow().paused(crate::app::now()) {
        vec![Item::new(TRAY_RESUME, "Resume reading")]
    } else {
        vec![
            Item::new(TRAY_PAUSE_HOUR, "For an hour"),
            Item::new(TRAY_PAUSE_TOMORROW, "Until tomorrow"),
            Item::new(TRAY_PAUSE_UNTIL_ON, "Until I turn it back on"),
        ]
    }
}

/// A choice from those.
pub fn pause_chosen(app: &App, chosen: u16) {
    let now = crate::app::now();
    match chosen {
        TRAY_PAUSE_HOUR => pause(app, now + 3600),
        TRAY_PAUSE_TOMORROW => pause(app, config::tomorrow_morning(now, winsys::process::utc_offset())),
        TRAY_PAUSE_UNTIL_ON => pause(app, -1),
        TRAY_RESUME => pause(app, 0),
        _ => {}
    }
}

fn tray_menu(app: &App, hotkey: &str) -> Vec<MenuItem> {
    let pausing = if app.settings.borrow().paused(crate::app::now()) {
        Item::new(TRAY_RESUME, "Resume reading")
    } else {
        Item::Submenu { label: "Pause reading".into(), items: pause_items(app) }
    };
    vec![
        Item::new(TRAY_OPEN, format!("Open Excavate\t{hotkey}")),
        pausing,
        Item::Separator,
        Item::checked(TRAY_START_WITH_WINDOWS, "Start with Windows", winsys::registry::starts_with_windows(RUN_NAME)),
        Item::new(TRAY_SETTINGS, "Settings…"),
        Item::new(TRAY_LOG, "Log…"),
        Item::Separator,
        Item::new(TRAY_QUIT, "Quit Excavate"),
    ]
}

/// "Excavate (Alt+S): reading, 42%": read when the pointer comes to the icon, so it costs nothing otherwise.
fn update_tooltip(app: &App) {
    let hotkey = app.settings.borrow().hotkey.clone();
    let mut text = format!("Excavate ({hotkey})");
    if let Some(status) = config::status::Status::load(&app.paths) {
        let progress = crate::setup::progress(&status);
        if progress.active {
            text = if progress.fraction >= 0.0 {
                format!("{text}: {}, {:.0}%", if status.state == "paused" { "paused" } else { "reading" }, progress.fraction * 100.0)
            } else {
                format!("{text}: looking through your folders")
            };
        }
    }
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow().as_ref() {
            tray.set_tooltip(&text);
            // (A pause for a while may have ended since.)
            tray.set_menu(tray_menu(app, &hotkey));
        }
    });
}

fn tray_event(app: &App, event: Event) {
    match event {
        Event::Hotkey => toggle(app, Trigger::Hotkey),
        Event::Click => toggle(app, Trigger::Tray),
        Event::Reopen => show(app),
        Event::ThemeChanged => theme_changed(app),
        Event::Hover => update_tooltip(app),
        Event::EndSession => crate::own_index::save_now(),
        Event::Menu(TRAY_OPEN) => show(app),
        Event::Menu(TRAY_START_WITH_WINDOWS) => {
            let on = !winsys::registry::starts_with_windows(RUN_NAME);
            if std::env::var_os("EXCAVATE_HOME").is_none() {
                let _ = winsys::registry::set_start_with_windows(RUN_NAME, "--hidden", on);
            }
            refresh_tray_menu(app);
        }
        Event::Menu(id @ (TRAY_PAUSE_HOUR | TRAY_PAUSE_TOMORROW | TRAY_PAUSE_UNTIL_ON | TRAY_RESUME)) => pause_chosen(app, id),
        Event::Menu(TRAY_SETTINGS) => {
            hide(app);
            let onboarding = !app.settings.borrow().setup_done;
            crate::setup::open(app, onboarding);
        }
        Event::Menu(TRAY_LOG) => {
            let _ = winsys::shell::open(0, &app.paths.log().display().to_string());
        }
        Event::Menu(TRAY_QUIT) => quit(app),
        Event::Menu(_) => {}
    }
}

pub fn quit(app: &App) {
    let started = std::time::Instant::now();
    let step = |what: &str| log(&format!("quitting: {what} ({} ms)", started.elapsed().as_millis()));
    hide(app);
    app.stop_video();
    step("preview stopped");
    app.indexer.stop();
    step("indexer stopped");
    crate::own_index::stop();
    step("own index saved");
    app.settings.borrow().save(&app.paths);
    TRAY.with(|tray| {
        if let Some(tray) = tray.borrow_mut().take() {
            tray.quit();
        }
    });
    step("tray gone");
    let _ = slint::quit_event_loop();
}

thread_local! {
    /// Everything chosen on onboarding's Indexers step: it may be started before setup is done.
    pub static EVERYTHING_CHOSEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether Everything answers; starts it when it's installed but not running (once setup is done, or it's chosen).
pub fn check_everything(app: &App) {
    // Excavate's own index chosen: Everything isn't started, and is asked to exit if it runs.
    if app.settings.borrow().own_index {
        crate::own_index::match_everything(app);
        let everything = app.everything.clone();
        std::thread::spawn(move || {
            let status = everything.status();
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.set_everything_status(status)));
        });
        return;
    }
    let (everything, probe) = (app.everything.clone(), app.paths.everything_probe("app"));
    // Not before it's chosen: onboarding asks which indexer first (Bedrock, the default, or Everything), and starting
    // Everything meanwhile would have it running and listing for nothing.
    let may_start = app.settings.borrow().setup_done || EVERYTHING_CHOSEN.get();
    std::thread::spawn(move || {
        let mut status = everything.status();
        if !status.available && !may_start {
            log("Everything isn't running: not started before an indexer is chosen");
        } else if !status.available {
            // (A sandbox never starts the person's own Everything: EXCAVATE_PRETEND_EVERYTHING.)
            if let Some(exe) = status.installed.as_ref().filter(|_| std::env::var_os("EXCAVATE_PRETEND_EVERYTHING").is_some()) {
                log(&format!("Everything isn't running: would start it ({exe} -startup; pretend)"));
            } else if let Some(exe) = &status.installed {
                log("Everything isn't running: starting it");
                let _ = std::process::Command::new(exe).arg("-startup").spawn();
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(250));
                    status = everything.status();
                    if status.available {
                        break;
                    }
                }
            }
        }
        log(&format!("Everything: {status:?}"));
        let available = status.available;
        let _ = slint::invoke_from_event_loop(move || with_app(|app| app.set_everything_status(status)));
        if available {
            follow_health(&everything, &probe, Duration::ZERO);
        }
    });
}

thread_local! {
    /// The next look at whether Everything keeps up.
    static HEALTH_TIMER: slint::Timer = slint::Timer::default();
}

/// Says whether Everything keeps up, as often as it changes while it loads, and looks again every 15 minutes.
fn follow_health(everything: &everything::Everything, probe: &std::path::Path, fresh: Duration) {
    let started = Instant::now();
    let mut said = None;
    loop {
        let health = crate::everything_setup::health(everything, probe, fresh);
        if said != Some(health) {
            log(&format!("Everything's answers: {health:?}"));
            let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                app.set_everything_health(health);
                crate::setup::everything_changed(app);
            }));
            said = Some(health);
        }
        if health != everything::Health::Loading || started.elapsed() > Duration::from_secs(600) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let _ = slint::invoke_from_event_loop(|| HEALTH_TIMER.with(|timer| {
        timer.start(slint::TimerMode::SingleShot, Duration::from_secs(900), || with_app(|app| {
            let (everything, probe) = (app.everything.clone(), app.paths.everything_probe("app"));
            std::thread::spawn(move || follow_health(&everything, &probe, crate::everything_setup::HEALTH_FRESH));
        }));
    }));
}

/// Gets Everything watching the drives again (`everything_setup::fix`), then looks again. One at a time: while it runs,
/// the search window's footer and Settings say which step it's on, and clicking again does nothing.
pub fn fix_everything(app: &App) {
    let everything::Health::Behind(fix) = app.everything_health() else { return };
    if app.everything_fixing().is_some() {
        return;
    }
    let step = |text: &str| {
        let text = text.to_string();
        let _ = slint::invoke_from_event_loop(move || with_app(|app| {
            app.set_everything_fixing(Some(text));
            crate::setup::everything_fixing(app);
        }));
    };
    // Set before anything else runs, so a second click finds it.
    app.set_everything_fixing(Some("Getting Everything to see new files…".into()));
    crate::setup::everything_fixing(app);
    let (everything, probe) = (app.everything.clone(), app.paths.everything_probe("app"));
    let last = Some(app.settings.borrow().everything_rebuild_seconds).filter(|&seconds| seconds > 0)
        .map(|seconds| Duration::from_secs(u64::from(seconds)));
    std::thread::spawn(move || {
        let result = crate::everything_setup::fix(fix, &everything, &probe, last, &step);
        log(&format!("fixing Everything ({fix:?}): {result:?}"));
        let fixed = result.is_ok();
        let _ = slint::invoke_from_event_loop(move || with_app(|app| {
            app.set_everything_fixing(None);
            match &result {
                Ok(rebuilt) => {
                    if let Some(rebuilt) = rebuilt {
                        app.settings.borrow_mut().everything_rebuild_seconds = rebuilt.as_secs() as u32;
                        app.settings.borrow().save(&app.paths);
                    }
                    // Looked at afresh below.
                    app.set_everything_health(everything::Health::Unknown);
                    crate::setup::everything_changed(app);
                }
                Err(error) => {
                    app.notice(error);
                    crate::setup::everything_fix_failed(error);
                }
            }
        }));
        if fixed {
            follow_health(&everything, &probe, Duration::ZERO);
        }
    });
}

fn banner_clicked(app: &App) {
    if !app.settings.borrow().setup_done {
        hide(app);
        crate::setup::open(app, true);
        return;
    }
    if app.everything_fixing().is_some() {
        return;
    }
    if matches!(app.everything_health(), everything::Health::Behind(_)) {
        fix_everything(app);
        return;
    }
    match everything::installed_everything() {
        Some(_) => check_everything(app),
        // Settings → General has "Install Everything".
        None => {
            hide(app);
            crate::setup::open_on(app, 0);
        }
    }
}

/// What a result does.
pub fn act(app: &App, index: usize, action: &str) {
    let Some(target) = app.target(index) else { return };
    let owner = app.hwnd.get();
    // For tests in a sandbox (`EXCAVATE_PRETEND_OPEN`): what would be opened is logged, nothing starts, and the popup
    // stays.
    if std::env::var_os("EXCAVATE_PRETEND_OPEN").is_some() && matches!(action, "open" | "reveal" | "runas" | "open-with") {
        log(&format!("pretend {action}: {}", target.name));
        return;
    }
    let result = match action {
        "open" => {
            hide(app);
            app.record_open(&target);
            // A video found by a moment opens there, when a player can be asked to.
            match (app.moment(&target.path), crate::seeking::vlc()) {
                (Some(at), Some(vlc)) => std::process::Command::new(vlc).arg(format!("--start-time={at:.0}")).arg(&target.path).spawn()
                    .map(|_| ()).map_err(|error| error.to_string()),
                _ => winsys::shell::open(0, &target.path),
            }
        }
        "reveal" => {
            hide(app);
            app.record_open(&target);
            winsys::shell::reveal(&target.path)
        }
        "runas" => {
            hide(app);
            winsys::shell::verb(0, &target.path, "runas")
        }
        "properties" => winsys::shell::verb(owner, &target.path, "properties"),
        "open-with" => {
            hide(app);
            winsys::shell::open_with(0, &target.path)
        }
        "copy" => winsys::shell::copy_files(owner, std::slice::from_ref(&target.path)).map(|()| {
            app.notice(&format!("Copied \u{201c}{}\u{201d}", crate::format::display_name(&target.name)));
        }),
        "copy-path" => winsys::shell::copy_text(owner, &target.path).map(|()| app.notice("Copied the path")),
        "copy-name" => winsys::shell::copy_text(owner, &target.name).map(|()| app.notice("Copied the name")),
        "everything" => show_in_everything(app, &target),
        _ => Ok(()),
    };
    if let Err(error) = result {
        log(&format!("{action} {}: {error}", target.path));
        app.notice(&format!("Couldn't {}: {error}", action.replace('-', " ")));
    }
}

fn show_in_everything(app: &App, target: &Target) -> Result<(), String> {
    let exe = everything::installed_everything().ok_or("Everything isn't installed")?;
    hide(app);
    std::process::Command::new(exe)
        .args(["-search", &app.query_text(), "-select", &target.path])
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn screen_point(app: &App, x: f32, y: f32) -> (i32, i32) {
    let Some(ui) = app.ui.upgrade() else { return winsys::display::cursor() };
    let position = ui.window().position();
    let scale = ui.window().scale_factor();
    (position.x + (x * scale) as i32, position.y + (y * scale) as i32)
}

const MENU_OPEN: u16 = 1;
const MENU_OPEN_WITH: u16 = 2;
const MENU_REVEAL: u16 = 3;
const MENU_COPY: u16 = 4;
const MENU_COPY_PATH: u16 = 5;
const MENU_COPY_NAME: u16 = 6;
const MENU_RUNAS: u16 = 7;
const MENU_PROPERTIES: u16 = 8;
const MENU_EVERYTHING: u16 = 9;

fn context_menu(app: &App, index: usize, x: f32, y: f32) {
    let Some(target) = app.target(index) else { return };
    let runnable = !target.is_folder
        && search::filter::extension(&target.name)
            .is_some_and(|ext| ["exe", "lnk", "bat", "cmd", "msc", "ps1", "msi"].iter().any(|e| e.eq_ignore_ascii_case(ext)));
    let mut items = vec![
        Item::new(MENU_OPEN, "Open\tEnter"),
        Item::new(MENU_OPEN_WITH, "Open with…\tCtrl+O"),
        Item::new(MENU_REVEAL, "Show in folder\tCtrl+Enter"),
        Item::Separator,
        Item::new(MENU_COPY, "Copy\tCtrl+C"),
        Item::new(MENU_COPY_PATH, "Copy path\tCtrl+Shift+C"),
        Item::new(MENU_COPY_NAME, "Copy name"),
        Item::Separator,
    ];
    if runnable {
        items.push(Item::new(MENU_RUNAS, "Run as administrator\tCtrl+Shift+Enter"));
    }
    items.push(Item::new(MENU_PROPERTIES, "Properties\tAlt+Enter"));
    // (Not while Excavate's own index is on: Everything is closed then.)
    if everything::installed_everything().is_some() && !everything::is_local() {
        items.push(Item::Separator);
        items.push(Item::new(MENU_EVERYTHING, "Show in Everything"));
    }
    let (sx, sy) = screen_point(app, x, y);
    let chosen = winsys::menu::popup(app.hwnd.get(), &items, sx, sy, 0);
    log(&format!("context menu for {} at {sx},{sy}: {chosen:?} (error {})", target.path,
                 winsys::menu::LAST_ERROR.load(std::sync::atomic::Ordering::Relaxed)));
    let action = match chosen {
        Some(MENU_OPEN) => "open",
        Some(MENU_OPEN_WITH) => "open-with",
        Some(MENU_REVEAL) => "reveal",
        Some(MENU_COPY) => "copy",
        Some(MENU_COPY_PATH) => "copy-path",
        Some(MENU_COPY_NAME) => "copy-name",
        Some(MENU_RUNAS) => "runas",
        Some(MENU_PROPERTIES) => "properties",
        Some(MENU_EVERYTHING) => "everything",
        _ => return,
    };
    act(app, index, action);
}
