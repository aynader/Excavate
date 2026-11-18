//! Excavate: one shortcut (Alt+S) opens a popup over the taskbar that finds anything on the PC as you type.
//!
//! Threads: the UI (Slint, this one), the shell (tray icon, global hotkey, second-launch door), Everything's SDK, a
//! few icon threads, and short-lived ones for the hot tier and previews. Idle, all of them wait on a message, but for
//! one timer a minute on this thread (whether a day has passed since updates were last looked for): no polling.

#![windows_subsystem = "windows"]

mod activity;
mod app;
mod bedrock_helper;
mod bench;
mod blur;
mod contents;
mod demo;
mod diagnostics;
mod docs;
mod everything_setup;
mod files_read;
mod format;
mod found;
mod functions;
mod icons;
mod indexer;
mod install;
mod intro;
mod launchers;
mod own_index;
mod living;
mod popup;
mod settings;
mod seeking;
mod setup;
mod themes;
mod update;
mod watchdog;
mod whole_text;

slint::include_modules!();

use app::App;
use settings::{Paths, Settings, log};
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
    /// Set while the settings window is being created: Slint asks for its native window's attributes then (in
    /// `SettingsWindow::new()`), and it gets an ordinary window rather than the popup's.
    pub static CREATING_SETTINGS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with the app, on the UI thread (where it lives). Closures sent from other threads land here through
/// `slint::invoke_from_event_loop`. One that holds the UI thread a quarter of a second or more is logged with where
/// it came from: the popup can't answer meanwhile (see `watchdog`).
#[track_caller]
pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let caller = std::panic::Location::caller();
    let app = APP.with(|app| app.borrow().clone());
    if let Some(app) = app {
        let started = std::time::Instant::now();
        f(&app);
        let took = started.elapsed();
        if took >= std::time::Duration::from_millis(250) {
            log(&format!("slow on the UI thread: {} ms in {}:{}", took.as_millis(), caller.file(), caller.line()));
        } else if ui_timing().is_some_and(|least| took >= least) {
            log(&format!("on the UI thread: {:.1} ms in {}:{}", took.as_secs_f64() * 1000.0, caller.file(), caller.line()));
        }
    }
}

/// For measuring: `EXCAVATE_UI_TIMING=<ms>` logs every `with_app` that takes at least that long (not only the slow ones).
pub fn ui_timing() -> Option<std::time::Duration> {
    static LEAST: std::sync::OnceLock<Option<std::time::Duration>> = std::sync::OnceLock::new();
    *LEAST.get_or_init(|| std::env::var("EXCAVATE_UI_TIMING").ok().and_then(|ms| ms.parse::<f64>().ok()).map(|ms| std::time::Duration::from_secs_f64(ms / 1000.0)))
}

pub const TRAY_OPEN: u16 = 1;
pub const TRAY_START_WITH_WINDOWS: u16 = 2;
pub const TRAY_SETTINGS: u16 = 3;
pub const TRAY_LOG: u16 = 4;
pub const TRAY_PAUSE_HOUR: u16 = 5;
pub const TRAY_PAUSE_TOMORROW: u16 = 6;
pub const TRAY_PAUSE_UNTIL_ON: u16 = 7;
pub const TRAY_RESUME: u16 = 8;
pub const TRAY_QUIT: u16 = 9;
pub const RUN_NAME: &str = "Excavate";

/// The launch animation plays once per version.
fn played_intro(app: &App) {
    app.settings.borrow_mut().intro_version = env!("CARGO_PKG_VERSION").to_string();
    app.settings.borrow().save(&app.paths);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let hidden = args.iter().any(|arg| arg == "--hidden");
    let bench = args.iter().any(|arg| arg == "--bench");
    // Started by an update: the copy it replaces is quitting.
    let after_update = args.iter().any(|arg| arg == "--after-update");
    // `--quit`: asks the copy running for this home to quit, as its tray menu's Quit does (for scripts and tests).
    if args.iter().any(|arg| arg == "--quit") {
        let heard = winsys::tray::ask_to_quit(&config::window_class("Shell"));
        log(&format!("--quit: {}", if heard { "asked" } else { "no Excavate running" }));
        winsys::process::exit_now(if heard { 0 } else { 1 });
    }
    // Run from where it was unzipped: Excavate puts itself in place first (install.rs). `--uninstall` takes it out.
    match install::decide(&args) {
        install::Start::Run => {}
        // (Silently, a copy already in place is put in place again: winget repairs that way, and nothing opens.)
        install::Start::Hand(_) if install::silent(&args) => winsys::process::exit_now(install::install_silently()),
        install::Start::Hand(exe) => {
            install::hand_over(&exe);
            return;
        }
        install::Start::Install { update } => {
            if install::silent(&args) {
                winsys::process::exit_now(install::install_silently());
            }
            let paths = Paths::new();
            install::run_install(update, &Settings::load(&paths));
            winsys::process::exit_now(0);
        }
        install::Start::Uninstall => {
            let paths = Paths::new();
            if install::silent(&args) {
                winsys::process::exit_now(install::uninstall_silently(&paths));
            }
            let settings = Settings::load(&paths);
            install::run_uninstall(paths, &settings);
            winsys::process::exit_now(0);
        }
    }
    let first = if after_update {
        winsys::single_instance_after(&config::instance_name("Running"), std::time::Duration::from_secs(20))
    } else {
        bench || winsys::single_instance(&config::instance_name("Running"))
    };
    if !first {
        // Already running: ask it to open, and leave.
        winsys::tray::knock(&config::window_class("Shell"));
        return;
    }
    let started = std::time::Instant::now();
    // There's no console to see a panic on: it goes in the log.
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));
    // A busy moment mustn't turn the popup into a "Not responding" stand-in with a plain icon on the taskbar.
    winsys::window::no_ghosting();
    let paths = Paths::new();
    // An update downloaded and checked earlier: put in place, and started instead of this.
    if !bench && update::apply_staged(&paths) {
        winsys::process::exit_now(0);
    }
    let mut settings = Settings::load(&paths);
    winsys::gpu::choose(&settings.graphics_card);
    // (Not for a debug build, a benchmark or a sandboxed copy under EXCAVATE_HOME: Start with Windows belongs to the real one.)
    if !cfg!(debug_assertions) && !bench && std::env::var_os("EXCAVATE_HOME").is_none() {
        if !settings.first_run_done {
            // A launcher that doesn't start with Windows has no shortcut after a restart. Off in the tray menu.
            let _ = winsys::registry::set_start_with_windows(RUN_NAME, "--hidden", true);
            settings.first_run_done = true;
            settings.save(&paths);
        }
        // On, it follows the build last opened; a build that moved would otherwise leave it pointing at nothing.
        if winsys::registry::follow_start_with_windows(RUN_NAME, "--hidden").unwrap_or(false) {
            log("Start with Windows now names this build");
        }
    }

    let renderer = std::env::var("EXCAVATE_RENDERER").unwrap_or_else(|_| settings.renderer.clone());
    let acrylic = settings.acrylic;
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.clone())
        .with_winit_window_attributes_hook(move |attributes| {
            use slint::winit_030::winit::platform::windows::{BackdropType, CornerPreference, WindowAttributesExtWindows};
            // The settings window is an ordinary window (a taskbar button, a title bar).
            if CREATING_SETTINGS.with(|creating| creating.get()) {
                return attributes.with_class_name("Excavate.Settings");
            }
            let attributes = attributes
                .with_skip_taskbar(true)
                .with_undecorated_shadow(true)
                .with_corner_preference(CornerPreference::Round)
                .with_class_name("Excavate.Popup");
            if acrylic {
                attributes.with_transparent(true).with_system_backdrop(BackdropType::TransientWindow)
            } else {
                attributes
            }
        })
        .select()
        .expect("no window system");
    winsys::com_sta();

    let ui = Popup::new().expect("the popup");
    popup::apply_theme(&ui, &settings);
    let app = App::new(&ui, settings, paths);
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    popup::wire(&ui);
    popup::start_tray(&app);
    own_index::apply(&app);
    popup::check_everything(&app);
    // Everything may start with Windows after Excavate did: with the own index on, it's asked to exit then too.
    let later = slint::Timer::default();
    later.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(120), || with_app(|app| {
        if app.settings.borrow().own_index {
            own_index::match_everything(app);
        }
    }));
    app.refresh_hot();
    if app.settings.borrow().setup_done && !bench {
        app.indexer.start();
    }
    // Updates, with the setting on: looked for a minute after starting, then once a day.
    let updates = slint::Timer::default();
    updates.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(60), || with_app(|app| {
        // The indexer, started again should it have ended by itself.
        app.indexer.watch();
        let due = {
            let settings = app.settings.borrow();
            settings.auto_update && settings.setup_done && app::now() - settings.update_checked_at >= update::EVERY
        };
        if due && !matches!(update::status(), update::Status::Ready(_)) {
            app.settings.borrow_mut().update_checked_at = app::now();
            app.settings.borrow().save(&app.paths);
            update::check(&app.paths);
        }
    }));
    // With the setting on, a program an earlier version's updater left out is fetched (once, from this version's release).
    if !bench && app.settings.borrow().auto_update {
        update::repair(&app.paths);
    }
    log(&format!("started in {} ms (renderer {renderer})", started.elapsed().as_millis()));
    watchdog::start();
    // Activity's view of reading, kept ready off the UI thread (slowly, until Settings opens).
    activity::start(app.paths.clone());
    if !winsys::media::available() {
        log(&format!("media: {}", winsys::media::MISSING));
    }

    // For measuring: Settings opened as the tray menu opens it, every so many seconds, and closed again before the next.
    let cycle = slint::Timer::default();
    if let Some(every) = std::env::var("EXCAVATE_SETTINGS_CYCLE").ok().and_then(|seconds| seconds.parse::<u64>().ok()) {
        let opened = std::rc::Rc::new(std::cell::Cell::new(false));
        cycle.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(every.max(2)), move || with_app(|app| {
            if opened.replace(!opened.get()) {
                setup::close_for_measuring();
            } else {
                let started = std::time::Instant::now();
                popup::hide_for_settings(app);
                setup::open(app, false);
                log(&format!("settings cycle: opened in {:.1} ms", started.elapsed().as_secs_f64() * 1000.0));
            }
        }));
    }
    if bench {
        bench::run(&app);
    } else if std::env::var_os("EXCAVATE_SETUP_STEP").is_some() && app.settings.borrow().setup_done {
        // For screenshots and tests: the settings window, on a given page.
        setup::open(&app, false);
        popup::prewarm(&app);
    } else if !app.settings.borrow().setup_done && !hidden {
        // The first time: the launch animation and onboarding, then the popup.
        let intro = intro::wanted(&app.settings.borrow().intro_version) && std::env::var_os("EXCAVATE_SETUP_STEP").is_none();
        if intro {
            setup::open_behind_intro(&app);
            played_intro(&app);
            intro::play(popup::opening_monitor(&app.settings.borrow()), true, popup::is_dark(&app.settings.borrow()), themes::current(&app.settings.borrow()), || {});
        } else {
            setup::open(&app, true);
        }
        popup::prewarm(&app);
    } else if !hidden {
        // For screenshots and tests: the popup opens with this already typed.
        let query = std::env::var("EXCAVATE_QUERY").ok();
        if query.is_none() && intro::wanted(&app.settings.borrow().intro_version) {
            // The first launch of a new version.
            played_intro(&app);
            popup::prewarm(&app);
            intro::play(popup::opening_monitor(&app.settings.borrow()), false, popup::is_dark(&app.settings.borrow()), themes::current(&app.settings.borrow()), || with_app(|app| popup::show(app)));
        } else {
            popup::show(&app);
            if let Some(query) = query {
                ui.set_query(query.clone().into());
                app.edited(&query);
            }
        }
    } else {
        popup::prewarm(&app);
    }
    slint::run_event_loop_until_quit().expect("the event loop");
    log("quitting: event loop ended");
    APP.with(|slot| slot.borrow_mut().take());
    log("quitting: done");
    // (Not Windows' usual ending: a graphics driver's clean-up can then spin forever; winsys::process::exit_now.)
    winsys::process::exit_now(0);
}
