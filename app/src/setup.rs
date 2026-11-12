//! The onboarding and settings window's behavior: filling it from the settings, applying each change as it's made
//! (saved at once, and passed to the indexer when it concerns reading), counting what's in the chosen folders through
//! Everything (or by walking them), estimating how long reading images takes on this PC, and following the indexer's
//! progress.

use crate::app::App;
use crate::settings::{Reading, Schedule, Settings, log};
use crate::themes::{self, Palette};
use crate::{CardWay, FolderItem, MenuRow, NowRow, PickerRow, ProblemRow, ReadingCard, SettingsWindow, StagePart, StageRow, TagRow, Theme, ThemeTile, with_app};
use config::status::Status;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};
use winsys::files::Known;

/// The Activity page's place in the rail.
pub const GENERAL: i32 = 0;
pub const ACTIVITY: i32 = 5;

/// A folder offered for reading: one of Windows' own (Desktop, Documents…) or one the person added.
#[derive(Clone, Debug)]
struct Choice {
    path: String,
    on: bool,
    custom: bool,
}

struct Setup {
    window: SettingsWindow,
    /// The onboarding step to go on to once the question before Windows' administrator prompt is answered (None: asked
    /// from Settings).
    after_admin_ask: Option<i32>,
    onboarding: bool,
    choices: Vec<Choice>,
    /// Per folder (lowercased): files a reader handles, and images among them.
    counts: HashMap<String, (u64, u64)>,
    /// Recordings and videos in each folder (lowercased), counted beside `counts`.
    media: HashMap<String, (u64, u64)>,
    /// What each folder leaves out (settings' `folder_kinds`), and which kinds are on at all, in `FOLDER_KINDS` order,
    /// with what each toggle says on hover.
    folder_kinds: std::collections::BTreeMap<String, Vec<String>>,
    kinds_anywhere: [bool; 6],
    kind_tips: [&'static str; 6],
    /// Per folder (lowercased): the images in the first sweep. Counted after the rest: it takes a listing of them all.
    first_sweep: HashMap<String, u64>,
    status: Option<Status>,
    timer: slint::Timer,
    /// Telling the indexer waits a moment, so several changes in a row restart it once.
    indexer_timer: slint::Timer,
    /// The hotkey before recording a new one (restored on cancel).
    hotkey_before: Option<String>,
    /// Reading has started (the last step of onboarding, or settings): folders show how far along they are.
    reading_started: bool,
    /// Recent (when, files read, images Deep has read): how fast each read is going, for the time left.
    pace: VecDeque<(Instant, u64, u64)>,
    /// Everything's installer is running.
    installing: Cell<bool>,
    /// What the folder rows show, changed in place: rows that aren't touched stay (and don't play their entrance again).
    folders: Rc<VecModel<FolderItem>>,
    reading_folders: Rc<VecModel<FolderItem>>,
    stages: Rc<VecModel<StageRow>>,
    now: Rc<VecModel<NowRow>>,
    problems: Rc<VecModel<ProblemRow>>,
    /// The monitors "Open Excavate on" last listed, after its first row.
    screens: Vec<winsys::display::Screen>,
    /// The Workshop is open: the window shows the theme being made, until it's saved or cancelled.
    workshop: Option<Palette>,
}

/// Brings a model up to date, row by row while its length stays the same.
fn update_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (index, row) in rows.into_iter().enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
}

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}

fn with_setup<R>(f: impl FnOnce(&mut Setup) -> R) -> Option<R> {
    SETUP.with(|setup| setup.borrow_mut().as_mut().map(f))
}

/// Switches Bedrock, Excavate's own index, on or off: the indexer starts again (with it on, it walks the folders it
/// reads instead of asking Everything), and Everything is closed or started again to match.
pub fn set_own_index(app: &App, on: bool) {
    app.settings.borrow_mut().own_index = on;
    changed(app, true);
    crate::own_index::apply(app);
    app.set_everything_health(everything::Health::Unknown);
    // Everything chosen again: started again if Excavate closed it (check_everything asks it to exit otherwise).
    if !on {
        crate::own_index::match_everything(app);
    }
    crate::popup::check_everything(app);
    fill(app);
}

/// Onboarding's next step, as "Continue" goes to it.
fn next_step() {
    // (What the step's handler does borrows the setup again: it's called once that borrow is over.)
    let Some(window) = with_setup(|setup| setup.window.clone_strong()) else { return };
    let step = window.get_step() + 1;
    window.set_step(step);
    window.invoke_step_shown(step);
}

/// Opens the window: onboarding (the first time), or settings.
/// A first run starts on what the cards recommend for this PC: without a graphics card that's Quick for what images
/// show (Balanced there takes hours for a library, Quick minutes). The person can still pick any of them.
fn first_run_choices(app: &App) {
    let mut settings = app.settings.borrow_mut();
    let first_time = settings.folders.is_empty() && !settings.setup_done;
    if first_time && settings.seeing == "balanced" && winsys::gpu::capable().is_none() {
        settings.seeing = "quick".into();
    }
}

pub fn open(app: &App, onboarding: bool) {
    if onboarding {
        first_run_choices(app);
    }
    if let Some(window) = SETUP.with(|setup| setup.borrow().as_ref().map(|setup| setup.window.clone_strong())) {
        if window.get_onboarding() == onboarding || !onboarding {
            let _ = window.show();
            style(&window);
            if let Some(handle) = handle() {
                winsys::window::activate(handle);
            }
            return;
        }
    }
    let (place, size) = create(app, onboarding);
    let _ = slint::invoke_from_event_loop(move || bring_in(place, size));
}

/// Moves the window from out of sight to where it opens, drawn: see `popup::open`.
fn bring_in(place: slint::PhysicalPosition, size: Option<slint::LogicalSize>) {
    let Some(window) = with_setup(|setup| setup.window.clone_strong()) else { return };
    let handle = handle();
    if handle.is_some_and(|handle| winsys::window::cloak(handle, true)) {
        CLOAKED.with(|cloaked| cloaked.set(true));
        // In case no frame comes: it can't stay invisible.
        slint::Timer::single_shot(Duration::from_millis(150), uncloak);
    }
    window.window().set_position(place);
    if let Some(size) = size {
        window.window().set_size(size);
    }
    // Showing it draws all of it. Its first frame may have come before Windows had it showing, and been lost.
    let _ = window.hide();
    let _ = window.show();
    if let Some(handle) = handle {
        winsys::window::activate(handle);
    }
}

fn uncloak() {
    if CLOAKED.with(|cloaked| cloaked.replace(false)) {
        if let Some(handle) = handle() {
            winsys::window::cloak(handle, false);
        }
    }
}

/// Creates the window, out of sight: only a window that exists can be kept off the screen while it draws. Where it goes, and
/// how large.
fn create(app: &App, onboarding: bool) -> (slint::PhysicalPosition, Option<slint::LogicalSize>) {
    crate::CREATING_SETTINGS.with(|creating| creating.set(true));
    let window = SettingsWindow::new().expect("the settings window");
    crate::CREATING_SETTINGS.with(|creating| creating.set(false));
    let settings = app.settings.borrow().clone();
    let choices = choices(&settings);
    window.set_onboarding(onboarding);
    // For screenshots and tests: open on a given step (onboarding) or page (settings), and tab ("2.1").
    if let Ok(asked) = std::env::var("EXCAVATE_SETUP_STEP") {
        let (step, tab) = asked.split_once('.').unwrap_or((&asked, "0"));
        if let Ok(step) = step.parse::<i32>() {
            if onboarding { window.set_step(step) } else { window.set_page(step) }
        }
        window.set_images_tab(tab.parse().unwrap_or(0));
        window.set_videos_tab(tab.parse().unwrap_or(0));
    }
    window.set_version(format!("Version {} · Beta", env!("CARGO_PKG_VERSION")).into());
    let monitor = crate::popup::opening_monitor(&settings);
    let size = opening_size(&monitor);
    if let Some(size) = size {
        window.window().set_size(size);
    }
    let place = centered(&monitor, size.unwrap_or(slint::LogicalSize::new(940.0, 700.0)));
    window.window().set_position(slint::PhysicalPosition::new(-32_000, -32_000));
    wire(&window);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), || with_app(|app| poll(app)));
    let (folders, reading_folders) = (Rc::new(VecModel::default()), Rc::new(VecModel::default()));
    window.set_folders(ModelRc::from(folders.clone()));
    window.set_reading_folders(ModelRc::from(reading_folders.clone()));
    let (stages, now, problems) = (Rc::new(VecModel::default()), Rc::new(VecModel::default()), Rc::new(VecModel::default()));
    window.set_stages(ModelRc::from(stages.clone()));
    window.set_now(ModelRc::from(now.clone()));
    window.set_problems(ModelRc::from(problems.clone()));
    window.set_leave_out_info(info(&[
        "**A name** leaves out every folder or file called that: `node_modules`",
        "**A wildcard**: `*` matches anything and `?` one character. `*.log`, or with a path, `C:\\temp\\*.log`",
        "**A regular expression** starts with `regex:`, like `regex:^IMG_\\d{4}\\.jpe?g$`. It's tried on each name, or on the whole path if it has `\\\\` in it, and isn't case-sensitive. Lookaround, backreferences and recursion work too. [Its syntax](https://docs.rs/fancy-regex/latest/fancy_regex/#syntax)",
    ]));
    SETUP.with(|setup| {
        *setup.borrow_mut() = Some(Setup {
            window: window.clone_strong(),
            after_admin_ask: None,
            onboarding,
            choices,
            counts: HashMap::new(),
            media: HashMap::new(),
            folder_kinds: std::collections::BTreeMap::new(),
            kinds_anywhere: [true; 6],
            kind_tips: KIND_NAMES,
            first_sweep: HashMap::new(),
            status: None,
            timer,
            indexer_timer: slint::Timer::default(),
            hotkey_before: None,
            reading_started: !onboarding,
            pace: VecDeque::new(),
            installing: Cell::new(false),
            folders,
            reading_folders,
            stages,
            now,
            problems,
            screens: Vec::new(),
            workshop: None,
        })
    });
    apply_theme(app);
    fill(app);
    count_folders(app, false);
    check_everything(app);
    poll(app);
    page_shown(app);
    let _ = window.show();
    if let Some(size) = size {
        window.window().set_size(size);
    }
    style(&window);
    crate::living::attach(&window, onboarding, themes::current(&settings));
    (place, size)
}

/// Larger than its smallest comfortable size where the screen has room: what's on the pages scales with it. In logical
/// pixels, which mean the same on a monitor of any scale: the window doesn't always start on the one it ends up on.
fn opening_size(monitor: &winsys::display::Monitor) -> Option<slint::LogicalSize> {
    let margin = 48.0;
    let width = 1040.0f32.min(monitor.work.width() as f32 / monitor.scale - margin);
    let height = 800.0f32.min(monitor.work.height() as f32 / monitor.scale - margin);
    (width >= 940.0 && height >= 700.0).then(|| slint::LogicalSize::new(width, height))
}

/// Where a window of this size goes to sit in the middle of a monitor. (As it will be there, which may not have the scale
/// of the monitor it's on now.)
fn centered(monitor: &winsys::display::Monitor, size: slint::LogicalSize) -> slint::PhysicalPosition {
    let (width, height) = ((size.width * monitor.scale) as i32, (size.height * monitor.scale) as i32);
    slint::PhysicalPosition::new(monitor.work.left + (monitor.work.width() - width).max(0) / 2, monitor.work.top + (monitor.work.height() - height).max(0) / 2)
}

/// Opens onboarding for the launch animation to fly into: out of sight, with nothing on its pages yet.
pub fn open_behind_intro(app: &App) {
    first_run_choices(app);
    create(app, true);
    with_setup(|setup| {
        setup.window.set_mark_landed(false);
        setup.window.set_pages_ready(false);
    });
    crate::demo::stop();
}

/// The animation is about to fly: the window comes up, centered, fading in over the flight. Where its mark is on
/// screen, for the animation to land on.
pub fn intro_reveal(app: &App) -> Option<(i32, i32, i32)> {
    let handle = handle()?;
    let monitor = crate::popup::opening_monitor(&app.settings.borrow());
    with_setup(|setup| {
        let window = setup.window.window();
        let size = window.size().to_logical(window.scale_factor());
        winsys::window::fade(handle, 0);
        window.set_position(centered(&monitor, size));
        setup.window.set_draw_all(true);
    });
    winsys::window::activate(handle);
    let begun = Instant::now();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let progress = (begun.elapsed().as_secs_f32() / 0.34).min(1.0);
        winsys::window::fade(handle, (progress * 255.0) as u8);
        if progress >= 1.0 {
            FADE.with(|fade| fade.borrow_mut().take());
        }
    });
    FADE.with(|fade| *fade.borrow_mut() = Some(timer));
    mark_rect(monitor.scale)
}

thread_local! {
    /// The onboarding window fading in.
    static FADE: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// Windows is keeping the window off the screen until its first frame is out.
    static CLOAKED: Cell<bool> = const { Cell::new(false) };
}

/// Opens settings on a page (0 General, 1 Folders, …).
pub fn open_on(app: &App, page: i32) {
    open(app, !app.settings.borrow().setup_done);
    with_setup(|setup| {
        if !setup.onboarding {
            setup.window.set_page(page);
        }
    });
}

/// What the page now on screen starts: the welcome page's search scene.
fn page_shown(app: &App) {
    let on_welcome = with_setup(|setup| setup.onboarding && setup.window.get_step() == 0 && setup.window.get_pages_ready());
    if on_welcome == Some(true) {
        let accent = themes::current(&app.settings.borrow()).match_color(crate::popup::is_dark(&app.settings.borrow()));
        let saved = app.paths.names();
        with_setup(|setup| crate::demo::start(&setup.window, accent, saved));
    } else {
        crate::demo::stop();
    }
}

/// The launch animation has landed: the page may start.
pub fn intro_landed(app: &App) {
    with_setup(|setup| {
        setup.window.set_mark_landed(true);
        setup.window.set_pages_ready(true);
    });
    page_shown(app);
}

/// Where the rail's mark is on screen (left, top, size), in physical pixels, for the launch animation to land on.
pub fn mark_rect(scale: f32) -> Option<(i32, i32, i32)> {
    let (left, top) = winsys::window::client_origin(handle()?)?;
    // Centered in the 236 px rail, 28 px from the top (see settings.slint).
    let size = (88.0 * scale).round() as i32;
    Some((left + ((236.0 * scale).round() as i32 - size) / 2, top + (28.0 * scale).round() as i32, size))
}

fn handle() -> Option<isize> {
    let handle = winsys::window::find("Excavate.Settings");
    (handle != 0).then_some(handle)
}

/// The title bar in the window's colors.
fn style(window: &SettingsWindow) {
    let theme = window.global::<Theme>();
    let (dark, surfaces) = (theme.get_dark(), themes::Surfaces::from_index(theme.get_surfaces()));
    let window = window.as_weak();
    // The native window exists once the event loop has run after show().
    let _ = slint::invoke_from_event_loop(move || {
        if window.upgrade().is_some() {
            if let Some(handle) = handle() {
                let ([r, g, b], [tr, tg, tb]) = (themes::base(dark, surfaces), themes::ink(dark, surfaces));
                winsys::window::style_titlebar(handle, dark, (r, g, b), (tr, tg, tb));
            }
        }
    });
}

pub fn apply_theme(app: &App) {
    let settings = app.settings.borrow();
    with_setup(|setup| {
        let theme = setup.window.global::<Theme>();
        let dark = crate::popup::is_dark(&settings);
        let flipped = theme.get_dark() != dark;
        theme.set_dark(dark);
        // The tiles show each theme's accent for light or dark, whichever is on.
        if flipped {
            fill_themes(setup, &settings);
        }
        themes::paint(&theme, setup.workshop.as_ref().unwrap_or(&themes::current(&settings)));
        crate::living::repaint(setup.workshop.clone().unwrap_or_else(|| themes::current(&settings)));
        // (`EXCAVATE_NO_ANIMATION` shows the still pages on a PC with Windows' animation effects on, for tests.)
        theme.set_animate(winsys::theme::animations_enabled() && std::env::var_os("EXCAVATE_NO_ANIMATION").is_none());
        style(&setup.window);
    });
}

/// The folders on the Folders page: a new install starts with Documents and Pictures (the person adds the rest), and
/// every one can be removed. Windows' own folders come first, the rest as they were added.
fn choices(settings: &Settings) -> Vec<Choice> {
    let known = |which: &[Known]| -> Vec<String> {
        let mut paths: Vec<String> = which.iter().copied().filter_map(winsys::files::known).map(|path| path.display().to_string()).collect();
        paths.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        paths
    };
    let windows = known(&[Known::Desktop, Known::Documents, Known::Downloads, Known::Pictures]);
    let first_time = settings.folders.is_empty() && !settings.setup_done;
    if first_time {
        return known(&[Known::Documents, Known::Pictures]).into_iter().map(|path| Choice { path, on: true, custom: false }).collect();
    }
    let off = match &settings.folders_off {
        Some(off) => off.clone(),
        // Saved before every folder could be removed: Windows' four show as they did.
        None => windows.iter().filter(|path| !settings.folders.iter().any(|folder| folder.eq_ignore_ascii_case(path))).cloned().collect(),
    };
    let mut choices: Vec<Choice> = Vec::new();
    for (path, on) in settings.folders.iter().map(|path| (path, true)).chain(off.iter().map(|path| (path, false))) {
        if !choices.iter().any(|choice| choice.path.eq_ignore_ascii_case(path)) {
            let custom = !windows.iter().any(|known| known.eq_ignore_ascii_case(path));
            choices.push(Choice { path: path.clone(), on, custom });
        }
    }
    let place = |choice: &Choice| windows.iter().position(|known| known.eq_ignore_ascii_case(&choice.path)).unwrap_or(windows.len());
    choices.sort_by_key(place);
    choices
}

fn group(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{} {}", group(count), if count == 1 { one } else { many })
}

/// Everything shown, from the settings and what's known.
fn fill(app: &App) {
    fill_about(app);
    let settings = app.settings.borrow().clone();
    with_setup(|setup| {
        // Images are looked at, and videos with them, only once their text is read at all.
        let pictures = settings.reading() != Reading::Off;
        setup.folder_kinds = settings.folder_kinds.clone();
        // Off on its own page: dimmed in every folder, saying where it's turned on.
        let images_off = (!pictures).then_some("Off under Images");
        let off: [Option<&'static str>; 6] = [
            images_off,
            images_off.or((!settings.seeing_on).then_some("Off under Images")),
            (!settings.video_text).then_some("Off under Videos").or(images_off),
            (!settings.videos_on).then_some("Off under Videos").or(images_off).or((!settings.seeing_on).then_some("Off under Images")),
            (!settings.video_speech).then_some("Off under Videos"),
            (!settings.listening_on).then_some("Off under Audio"),
        ];
        setup.kinds_anywhere = off.map(|off| off.is_none());
        setup.kind_tips = std::array::from_fn(|index| off[index].unwrap_or(KIND_NAMES[index]));
        let window = &setup.window;
        window.set_hotkey_keys(ModelRc::new(VecModel::from(
            settings.hotkey.split('+').map(|key| SharedString::from(key.trim())).collect::<Vec<_>>(),
        )));
        window.set_hotkey(settings.hotkey.as_str().into());
        window.set_hotkey_note(hotkey_note(&settings.hotkey).into());
        window.set_start_with_windows(winsys::registry::starts_with_windows(crate::RUN_NAME));
        window.set_new_search(settings.new_search_each_time);
        window.set_typed_toggles(settings.typed_toggles);
        window.set_own_index(settings.own_index);
        window.set_theme_choice(match settings.theme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        });
        fill_themes(setup, &settings);
        let window = &setup.window;
        window.set_blur_secrets(settings.blur_secrets);
        window.set_blur_sharing(settings.blur_when_sharing);
        window.set_read_on_battery(settings.read_on_battery);
        window.set_reading(chosen_reading(&settings).name().into());
        window.set_pictures_on(settings.pictures_on);
        let gpu = winsys::gpu::capable();
        window.set_seeing(chosen_seeing(&settings, gpu.is_some()).into());
        window.set_seeing_on(settings.seeing_on);
        window.set_deep_choice(i32::from(settings.seeing_deep == "qwen" && strong_enough(gpu.as_ref())));
        window.set_schedule(schedule_index(settings.schedule()));
        window.set_schedule_every(every_index(&settings.schedule_every));
        window.set_schedule_at(config::clock(i64::from(settings.schedule_minutes()), app.twelve_hour()).into());
        window.set_seeing_schedule(schedule_index(settings.seeing_schedule()));
        window.set_seeing_schedule_every(every_index(&settings.seeing_schedule_every));
        window.set_seeing_schedule_at(config::clock(i64::from(settings.seeing_schedule_minutes()), app.twelve_hour()).into());
        window.set_look_on_battery(settings.look_on_battery);
        // (Off is the pane hidden, as Ctrl+P hides it.)
        window.set_preview_size(match settings.preview_size.as_str() {
            _ if !settings.preview => 0,
            "small" => 1,
            "large" => 3,
            _ => 2,
        });
        window.set_time_format(match settings.time_format.as_str() {
            "12-hour" => 1,
            "24-hour" => 2,
            _ => 0,
        });
        window.set_exclusions(ModelRc::new(VecModel::from(tag_rows(&settings.exclude))));
        window.set_names_scope(if settings.names_scope == "folders" { 1 } else { 0 });
        window.set_leave_out_apps(settings.names_leave_out_apps);
        window.set_name_folders(ModelRc::new(VecModel::from(tag_rows(&settings.name_folders))));
        fill_names_missing(setup, &settings);
        fill_pause(window, &settings);
        fill_read_all(setup, &settings);
        fill_look_all(setup, &settings);
        fill_listen_all(setup, &settings);
        fill_listening(setup, &settings);
        fill_videos(setup, &settings);
        setup.window.set_listen_schedule_at(config::clock(i64::from(settings.listen_schedule_minutes()), app.twelve_hour()).into());
        fill_folders(setup);
        fill_cards(setup);
        fill_schedule_note(setup, &settings);
        fill_monitor(setup, &settings);
        fill_gpu(&setup.window, &settings);
        fill_popup_size(&setup.window, &settings);
    });
}

/// Whether a folder being read isn't in the name filter's list: a small link offers to add it.
fn fill_names_missing(setup: &Setup, settings: &Settings) {
    let missing = setup.choices.iter().filter(|choice| choice.on).any(|choice| !settings.name_folders.iter().any(|folder| folder.eq_ignore_ascii_case(&choice.path)));
    setup.window.set_name_folders_missing(missing);
}

/// The rail's link and Activity's button.
fn fill_pause(window: &SettingsWindow, settings: &Settings) {
    let paused = settings.paused(crate::app::now());
    window.set_reading_paused(paused);
    window.set_pause_label(if paused { "Resume reading" } else { "Pause reading" }.into());
}

/// What the pause offers, saying when each would end, as of now.
fn pause_rows(app: &App) -> ModelRc<MenuRow> {
    let (now, offset, twelve) = (crate::app::now(), winsys::process::utc_offset(), app.twelve_hour());
    let rows = [
        ("For an hour", format!("until {}", config::clock_at(now + 3600, offset, twelve))),
        ("Until tomorrow", config::clock_at(config::tomorrow_morning(now, offset), offset, twelve)),
        ("Until I turn it back on", String::new()),
    ];
    ModelRc::new(VecModel::from(rows.map(|(label, note)| MenuRow { label: label.into(), note: note.into(), ..MenuRow::default() }).to_vec()))
}

/// "Open Excavate on": the monitors there are now, with the one chosen even if it's unplugged.
fn fill_monitor(setup: &mut Setup, settings: &Settings) {
    setup.screens = winsys::display::screens();
    const MOUSE: &str = "The monitor my mouse is on";
    let chosen = setup.screens.iter().find(|screen| screen.id == settings.monitor);
    let unplugged = !settings.monitor.is_empty() && chosen.is_none();
    let label = chosen.map_or(if unplugged { settings.monitor_name.as_str() } else { MOUSE }, |screen| screen.name.as_str());
    setup.window.set_monitor_label(label.into());
    setup.window.set_monitor_note(if unplugged { "It's not connected right now, so Excavate opens where your mouse is." } else { "" }.into());
    let mut rows = vec![MenuRow { label: MOUSE.into(), checked: settings.monitor.is_empty(), line: true, ..MenuRow::default() }];
    rows.extend(setup.screens.iter().map(|screen| MenuRow { label: screen.name.as_str().into(), checked: screen.id == settings.monitor, ..MenuRow::default() }));
    if unplugged {
        rows.push(MenuRow { label: settings.monitor_name.as_str().into(), note: "not connected".into(), checked: true, ..MenuRow::default() });
    }
    setup.window.set_monitor_rows(ModelRc::new(VecModel::from(rows)));
}

/// "Graphics card": Automatic (saying which it finds), each card Windows lists, and the processor alone.
fn fill_gpu(window: &SettingsWindow, settings: &Settings) {
    let cards = winsys::gpu::adapters();
    let best = winsys::gpu::automatic();
    let chosen = settings.graphics_card.as_str();
    let missing = !chosen.is_empty() && chosen != "none" && !cards.iter().any(|card| card.name == chosen);
    let kind = |card: &winsys::gpu::Gpu| if card.dedicated_mb >= 1000 { format!("{:.0} GB of its own", card.dedicated_mb as f64 / 1024.0) } else { "the processor's graphics".to_string() };
    window.set_gpu_label(match chosen {
        "" => "Automatic".to_string(),
        "none" => "None".to_string(),
        name => name.to_string(),
    }.into());
    window.set_gpu_note(match (chosen, &best) {
        _ if missing => "It's not there right now, so Excavate uses the best one it finds.".to_string(),
        ("", Some(best)) => format!("Where every model runs and videos are decoded. Automatic uses {}.", best.name),
        ("", None) => "Where every model runs and videos are decoded. No card here has the memory for them, so the processor does it all.".to_string(),
        ("none", _) => "Everything runs on the processor: slower, and the graphics card stays free.".to_string(),
        _ => "Where every model runs and videos are decoded.".to_string(),
    }.into());
    let mut rows = vec![MenuRow {
        label: "Automatic".into(),
        note: best.as_ref().map_or("the processor".to_string(), |best| best.name.clone()).into(),
        checked: chosen.is_empty(),
        line: true,
        ..MenuRow::default()
    }];
    rows.extend(cards.iter().map(|card| MenuRow { label: card.name.as_str().into(), note: kind(card).into(), checked: card.name == chosen, ..MenuRow::default() }));
    rows.push(MenuRow { label: "None".into(), note: "the processor only".into(), checked: chosen == "none", ..MenuRow::default() });
    window.set_gpu_rows(ModelRc::new(VecModel::from(rows)));
}

/// "Search window size": the sizes to choose from, and after them one dragged by hand.
fn fill_popup_size(window: &SettingsWindow, settings: &Settings) {
    window.set_popup_size_label(settings.popup_size_label().into());
    let size = (settings.popup_width, settings.popup_height);
    let row = |name: &str, width: u32, height: u32| MenuRow { label: name.into(), note: format!("{width} \u{d7} {height}").into(), checked: (width, height) == size, ..MenuRow::default() };
    let mut rows: Vec<MenuRow> = config::POPUP_SIZES.iter().map(|&(name, width, height)| row(name, width, height)).collect();
    if !rows.iter().any(|row| row.checked) {
        rows.push(row("Custom", size.0, size.1));
    }
    window.set_popup_size_rows(ModelRc::new(VecModel::from(rows)));
}

/// The popup was left at another size while this window is open.
pub fn popup_size_changed(app: &App) {
    let settings = app.settings.borrow();
    with_setup(|setup| fill_popup_size(&setup.window, &settings));
}

/// A schedule as its place in the three-part control.
fn schedule_index(schedule: Schedule) -> i32 {
    match schedule {
        Schedule::Arrive => 0,
        Schedule::Scheduled => 1,
        Schedule::Manual => 2,
    }
}

fn every_index(every: &str) -> i32 {
    match every {
        "week" => 1,
        "month" => 2,
        _ => 0,
    }
}

/// "Read everything now" for what images show: whether it's going.
fn fill_look_all(setup: &Setup, settings: &Settings) {
    let status = setup.status.as_ref();
    let on = settings.look_all_at > 0 && (status.is_some_and(|status| status.seeing_all) || crate::app::now() - settings.look_all_at < 20);
    setup.window.set_looking_all(on);
    // The model furthest behind.
    let left = status.and_then(|status| status.looking.iter().map(|looking| looking.queued + looking.videos_queued).max()).unwrap_or(0);
    setup.window.set_look_all_note(if on {
        format!("Looking at every image in one sitting, even while you use the PC: {} to go.", group(left))
    } else {
        "Looks at every image waiting, without waiting for a quiet moment.".to_string()
    }.into());
}

/// Activity's "Full speed": the three "Read everything now" at once (images and their text, what images and videos
/// show, what's said), while reading has work left; on while any of them is going.
fn fill_full_speed(setup: &Setup, settings: &Settings) {
    let status = setup.status.as_ref();
    let now = crate::app::now();
    let going = |at: i64, all: bool| at > 0 && (all || now - at < 20);
    let on = status.is_some_and(|status| going(settings.read_all_at, status.reading_all) || going(settings.look_all_at, status.seeing_all)
        || going(settings.listen_all_at, status.listen_all));
    let working = status.is_some_and(|status| status.state == "reading");
    setup.window.set_full_speed(on);
    setup.window.set_full_speed_shown(working && !settings.paused(now));
}

/// "Read everything now": whether it's going, and how long it takes.
fn fill_read_all(setup: &Setup, settings: &Settings) {
    let status = setup.status.as_ref();
    // (Just asked for, the indexer is on its way to it.)
    let on = settings.read_all_at > 0 && (status.is_some_and(|status| status.reading_all) || crate::app::now() - settings.read_all_at < 20);
    setup.window.set_reading_all(on);
    let left = status.map_or(0, |status| status.queued_pictures);
    let ms = picture_ms(status, chosen_reading(settings));
    let note = if on {
        format!("Reading every image in one sitting, even while you use the PC: {} to go.", group(left))
    } else {
        let images = chosen_images(setup);
        if images > 0 {
            let mut time = duration(ms * images as f64);
            time[..1].make_ascii_uppercase();
            format!("Every image in one sitting, at full speed, even while you use the PC. {time}.")
        } else {
            "Every image in one sitting, at full speed, even while you use the PC.".to_string()
        }
    };
    setup.window.set_read_all_note(note.into());
}

/// The images in the folders chosen.
fn chosen_images(setup: &Setup) -> u64 {
    setup.choices.iter().filter(|choice| choice.on).filter_map(|choice| setup.counts.get(&choice.path.to_lowercase()).map(|&(_, images)| images)).sum()
}

/// The preview shown or hidden in the search window (Ctrl+P): Settings says so too.
pub fn preview_changed(app: &App) {
    let settings = app.settings.borrow();
    with_setup(|setup| setup.window.set_preview_size(match settings.preview_size.as_str() {
        _ if !settings.preview => 0,
        "small" => 1,
        "large" => 3,
        _ => 2,
    }));
}

/// Reading was paused or resumed, from wherever.
pub fn pause_changed(app: &App) {
    let settings = app.settings.borrow();
    with_setup(|setup| fill_pause(&setup.window, &settings));
}

/// The card that's chosen, whether text in images is read right now or not.
fn chosen_reading(settings: &Settings) -> Reading {
    match Reading::parse(&settings.reading) {
        Reading::Off => Reading::Balanced,
        reading => reading,
    }
}

/// What waits for the schedule, in a line, for each tab.
fn fill_schedule_note(setup: &Setup, settings: &Settings) {
    let status = setup.status.as_ref();
    let note = match status.filter(|status| status.pictures_wait && status.queued_pictures > 0) {
        Some(status) if status.pictures_at > 0 => format!("{} will be read at the next scheduled time.", plural(status.queued_pictures, "new image", "new images")),
        Some(status) => format!("{} waiting.", plural(status.queued_pictures, "new image", "new images")),
        None => String::new(),
    };
    setup.window.set_schedule_note(note.into());
    let waiting = status.and_then(|status| status.looking.iter().map(|looking| looking.queued).max()).unwrap_or(0);
    let note = match status.filter(|status| status.seeing_wait && waiting > 0) {
        Some(status) if status.seeing_at > 0 => format!("{} will be looked at at the next scheduled time.", plural(waiting, "new image", "new images")),
        Some(_) => format!("{} waiting.", plural(waiting, "new image", "new images")),
        // An image is looked at once its text is read: sooner than that, the other tab's timing holds it.
        None if settings.schedule() != Schedule::Arrive && settings.seeing_schedule() != settings.schedule() =>
            "New images are looked at once their text has been read, so they also wait for Text in images.".into(),
        None => String::new(),
    };
    setup.window.set_seeing_schedule_note(note.into());
}

/// A path short enough for a tag: "C:\…\Projects\photos". (A regular expression's backslashes aren't folders.)
fn short_path(path: &str) -> String {
    if path.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case("regex:")) {
        return if path.chars().count() <= 48 { path.to_string() } else { format!("{}…", path.chars().take(47).collect::<String>()) };
    }
    let parts: Vec<&str> = path.split('\\').filter(|part| !part.is_empty()).collect();
    if path.chars().count() <= 48 || parts.len() < 4 {
        return path.to_string();
    }
    format!("{}\\…\\{}\\{}", parts[0], parts[parts.len() - 2], parts[parts.len() - 1])
}

/// Exclusions and folders in rows that fit the page (about 560 px of tags).
fn tag_rows(exclusions: &[String]) -> Vec<TagRow> {
    let mut rows = Vec::new();
    let mut row: Vec<SharedString> = Vec::new();
    let (mut width, mut start) = (0.0f32, 0usize);
    for (index, exclusion) in exclusions.iter().enumerate() {
        let shown = short_path(exclusion);
        let tag = 7.0 * shown.chars().count() as f32 + 40.0;
        if width + tag > 560.0 && !row.is_empty() {
            rows.push(TagRow { tags: ModelRc::new(VecModel::from(std::mem::take(&mut row))), start: start as i32 });
            width = 0.0;
            start = index;
        }
        row.push(shown.into());
        width += tag + 6.0;
    }
    if !row.is_empty() {
        rows.push(TagRow { tags: ModelRc::new(VecModel::from(row)), start: start as i32 });
    }
    rows
}

/// What each of a folder's toggles reads, in `config::FOLDER_KINDS` order: said on hover.
const KIND_NAMES: [&str; 6] = ["Text in images", "What images show", "Text in videos", "What videos show", "What's said in videos", "What's said in recordings"];

fn fill_folders(setup: &Setup) {
    let folders: Vec<FolderItem> = setup
        .choices
        .iter()
        .map(|choice| {
            let key = choice.path.to_lowercase();
            let name = choice.path.rsplit('\\').next().unwrap_or(&choice.path).to_string();
            let reading = setup.status.as_ref().and_then(|status| status.folders.iter().find(|folder| folder.path.eq_ignore_ascii_case(&choice.path)));
            let off = setup.folder_kinds.get(&config::folder_key(&choice.path));
            let kinds: Vec<bool> = config::FOLDER_KINDS.iter().map(|kind| off.is_none_or(|off| !off.iter().any(|left| left == kind))).collect();
            let mut done = false;
            let (detail, progress) = match (reading, setup.counts.get(&key)) {
                // The step it's on while it's read, "Reading · 640 of 1,000" (or Seeing,
                // Listening); then "✓ All read · 322 files" (All seen, All listened to when that's all it does).
                (Some(folder), Some(&(counted, _))) if setup.reading_started && (counted > 0 || folder.files + folder.heard + folder.to_hear > 0) => {
                    let files = folder.files.max(1);
                    let read = folder.read.min(folder.files);
                    let step = |verb: &str, done: u64, waiting: u64| (format!("{verb} · {} of {}", group(done), group(done + waiting)), done as f32 / (done + waiting).max(1) as f32);
                    // The catalog holds only what's been read so far: while files are still waiting, a folder isn't done,
                    // whatever its own counts say (it would show "All read · 205 files" at the start of a first sweep).
                    let waiting = setup.status.as_ref().is_some_and(|status| status.queued_text + status.queued_pictures > 0 || status.state == "listing");
                    if read < folder.files {
                        step("Reading", read, folder.files - read)
                    } else if waiting && counted > read {
                        step("Reading", read, counted - read)
                    } else if folder.to_see > 0 {
                        step("Seeing", folder.seen, folder.to_see)
                    } else if folder.to_hear > 0 {
                        step("Listening", folder.heard, folder.to_hear)
                    } else {
                        done = true;
                        let only_recordings = folder.files == 0 && folder.heard > 0;
                        // Only images, looked at but not read (its text in images left out).
                        let only_seen = folder.files > 0 && folder.pictures == folder.files && folder.seen > 0 && !kinds[0];
                        let (verb, count) = if only_recordings { ("All listened to", folder.heard) } else if only_seen { ("All seen", files) } else { ("All read", folder.files) };
                        (format!("{verb} · {}", plural(count, "file", "files")), -1.0)
                    }
                }
                (_, Some(&(files, pictures))) => {
                    let (recordings, videos) = setup.media.get(&key).copied().unwrap_or_default();
                    let mut parts = Vec::new();
                    if files > 0 || recordings + videos == 0 {
                        parts.push(plural(files, "file", "files"));
                    }
                    if pictures > 0 {
                        parts.push(plural(pictures, "image", "images"));
                    }
                    if recordings > 0 {
                        parts.push(plural(recordings, "recording", "recordings"));
                    }
                    if videos > 0 {
                        parts.push(plural(videos, "video", "videos"));
                    }
                    let mut detail = parts.join(" · ");
                    // Code, data sets and downloads in bulk: worth a second look.
                    if files > 250_000 {
                        detail.push_str("
A lot: leave out data and project folders to read the rest sooner");
                    }
                    (detail, -1.0)
                }
                (_, None) => ("Counting…".to_string(), -1.0),
            };
            FolderItem {
                path: choice.path.as_str().into(),
                name: name.into(),
                detail: detail.into(),
                on: choice.on,
                progress,
                done,
                custom: choice.custom,
                kinds: ModelRc::new(VecModel::from(kinds)),
                kinds_anywhere: ModelRc::new(VecModel::from(setup.kinds_anywhere.to_vec())),
                kind_tips: ModelRc::new(VecModel::from(setup.kind_tips.iter().map(|tip| SharedString::from(*tip)).collect::<Vec<_>>())),
            }
        })
        .collect();
    update_rows(&setup.reading_folders, folders.iter().filter(|folder| folder.on).cloned().collect());
    update_rows(&setup.folders, folders);
}

/// Deep's read, per image, on a PC in use. Alone on the graphics card it takes 0.7 s (median); on a PC in
/// use it was measured at 2.2 s, and 3.3 s for the whole round with everything around it.
const DEEP_MS: f64 = 3000.0;

/// How long an image takes to read on this PC (ms), for each reader: measured by the indexer when it has read some
/// with that reader, else estimated from this CPU (measured on an i9-13900HX at idle priority: 420 ms small, about
/// 0.45 × that tiny).
fn picture_ms(status: Option<&Status>, reading: Reading) -> f64 {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as f64;
    let small = 420.0 * (16.0 / cores.min(16.0)).powf(0.75);
    let measured = status.and_then(|status| status.picture_ms.filter(|_| !status.engine.is_empty()).map(|ms| (status.engine.clone(), ms)));
    match reading {
        // Both reads, each image: the first on the CPU, then Deep's, at its real pace once that's known.
        Reading::Deep => picture_ms(status, Reading::Balanced) + status.and_then(|status| status.deep_pace_ms).unwrap_or(DEEP_MS),
        Reading::Quick => match measured {
            Some((engine, ms)) if engine.contains("tiny") => ms,
            Some((_, ms)) => ms * 0.45,
            None => small * 0.45,
        },
        _ => match measured {
            Some((engine, ms)) if engine.contains("small") => ms,
            Some((_, ms)) => ms / 0.45,
            None => small,
        },
    }
}

fn duration(ms: f64) -> String {
    let minutes = ms / 60_000.0;
    if minutes < 1.0 {
        "under a minute".into()
    } else if minutes < 60.0 {
        format!("about {} min", minutes.round() as u64)
    } else if minutes < 48.0 * 60.0 {
        let hours = (minutes / 60.0).floor();
        let rest = (minutes - hours * 60.0).round();
        if rest < 1.0 { format!("about {hours} h") } else { format!("about {hours} h {rest} min") }
    } else {
        let days = (minutes / (24.0 * 60.0)).floor();
        let rest = (minutes / 60.0 - days * 24.0).round();
        if rest < 1.0 { format!("about {days} days") } else { format!("about {days} days {rest} h") }
    }
}

/// A popover's paragraphs: markdown, with links ("[Learn more](models)") that open a page of the documentation.
fn info(paragraphs: &[&str]) -> ModelRc<slint::StyledText> {
    let paragraphs: Vec<slint::StyledText> = paragraphs.iter().map(|text| {
        slint::StyledText::from_markdown(text).unwrap_or_else(|_| slint::StyledText::from_plain_text(text))
    }).collect();
    ModelRc::new(VecModel::from(paragraphs))
}

fn lines(lines: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(lines.iter().map(|line| SharedString::from(*line)).collect::<Vec<_>>()))
}

/// Room for a download of this many bytes where the models go (with some to spare).
fn room_for(path: &std::path::Path, bytes: u64) -> bool {
    winsys::files::free_space(path).is_none_or(|free| free > bytes + (1 << 30) / 2)
}

/// The settings as they are, for what's drawn from them outside `fill` (the defaults while they're being changed).
fn settings_now() -> Settings {
    let mut settings = None;
    with_app(|app| settings = app.settings.try_borrow().ok().map(|settings| settings.clone()));
    settings.unwrap_or_default()
}

/// An Images card's device menu: the graphics card (`on_gpu`: its title and what it means for this choice), then the
/// processor; the chosen one ticked, and its label for the card. Nothing to choose without a graphics card.
fn image_ways(chosen: &str, gpu: bool, on_gpu: (&str, &str, &str), on_cpu: &str, run: Option<&config::devices::Run>) -> (SharedString, ModelRc<CardWay>, i32) {
    let mut ways = Vec::new();
    if gpu {
        ways.push(("gpu", CardWay { label: on_gpu.0.into(), title: on_gpu.1.into(), note: on_gpu.2.into() }));
    }
    ways.push(("cpu", CardWay { label: "CPU".into(), title: "Processor".into(), note: on_cpu.into() }));
    // What this PC measured, in place of the benchmarks' figures.
    for (key, way) in &mut ways {
        if let Some(ms) = run.and_then(|run| run.ms.get(*key)) {
            way.note = format!("Measured on this PC: {} an image.", seconds_each(*ms)).into();
        }
    }
    let way = ways.iter().position(|(key, _)| *key == chosen).unwrap_or(0);
    let label = ways[way].1.label.clone();
    let ways: Vec<CardWay> = if ways.len() > 1 { ways.into_iter().map(|(_, way)| way).collect() } else { Vec::new() };
    (label, ModelRc::new(VecModel::from(ways)), way as i32)
}

/// Where each model runs fastest on this PC (measured by the indexer the first time each ran): nothing measured
/// with another graphics card or driver counts.
fn measured_here() -> config::devices::Measured {
    let measured = config::devices::Measured::load(&config::Paths::new());
    let adapter = winsys::gpu::capable().map(|gpu| gpu.identity()).unwrap_or_default();
    if measured.adapter == adapter { measured } else { config::devices::Measured::default() }
}

/// "0.02 s", "3.1 s".
fn seconds_each(ms: f64) -> String {
    if ms < 1000.0 { format!("{:.2} s", ms / 1000.0) } else { format!("{:.1} s", ms / 1000.0) }
}

/// A vision-language model's read of an image on the processor (27 to 38 s).
const DEEP_CPU_MS: f64 = 30_000.0;

fn fill_cards(setup: &Setup) {
    let settings = settings_now();
    let pictures = chosen_images(setup);
    // What goes first, when that's worth saying: its time beside the whole's.
    let first: Option<u64> = setup
        .choices
        .iter()
        .filter(|choice| choice.on)
        .map(|choice| setup.first_sweep.get(&choice.path.to_lowercase()).copied())
        .sum::<Option<u64>>()
        .filter(|&first| first * 10 < pictures * 9);
    setup.window.set_sweep_note(first.map_or(SharedString::new(), |first| {
        format!("A library this large is read in two goes: {} images first, one of each kind, so it's searchable sooner; the rest when your PC is free.", group(first)).into()
    }));
    let counted = setup.choices.iter().filter(|choice| choice.on).all(|choice| setup.counts.contains_key(&choice.path.to_lowercase()));
    let status = setup.status.as_ref();
    let gpu = winsys::gpu::capable();
    let here = measured_here();
    let device = |id: &str| settings.reading_device(id, gpu.is_some());
    let estimate = |reading: Reading| -> String {
        if !counted {
            return String::new();
        }
        if pictures == 0 {
            return "No images in these folders yet.".into();
        }
        // Where each runs: PP-OCRv6 on the processor, Thorough's second read on the graphics card unless chosen
        // otherwise. Measured on this PC where it runs, when it was: the benchmarks' figures otherwise.
        let here_ms = here.runs.get(&format!("reading:{}", reading.name())).and_then(|run| run.ms.get(device(reading.name())).copied());
        let per_image = match reading {
            Reading::Deep if device("deep") == "cpu" => picture_ms(status, Reading::Balanced) + DEEP_CPU_MS,
            Reading::Deep => picture_ms(status, reading),
            _ if here_ms.is_some() => here_ms.unwrap_or_default(),
            _ => picture_ms(status, reading),
        };
        let mut text = duration(per_image * pictures as f64);
        if let Some(first) = first {
            let mut sweep = duration(per_image * first as f64);
            sweep[..1].make_ascii_uppercase();
            return format!("{sweep} for the first sweep, {text} for all {}", plural(pictures, "image", "images"));
        }
        text[..1].make_ascii_uppercase();
        format!("{text} for your {}", plural(pictures, "image", "images"))
    };
    let models = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    // Quick reads on the processor only; Balanced on the graphics card too, when chosen (`Settings::reading_device`).
    let (quick_device, quick_ways, quick_way) = image_ways("cpu", false, ("", "", ""), "About 0.1 s an image.", None);
    let ocr_gpu = ("GPU, DirectML", "Graphics card, DirectML", "About twice as fast, but holds a few GB of memory while it reads.");
    let (balanced_device, balanced_ways, balanced_way) = image_ways(device("balanced"), gpu.is_some(), ocr_gpu,
                                                                    "Light on memory. About 0.3 s an image.", None);
    let (deep_device, deep_ways, deep_way) = image_ways(device("deep"), gpu.is_some(), ("GPU, Vulkan", "Graphics card, Vulkan", "About 1 to 3 s an image."),
                                                        "About 30 s an image: for a few images, not a library.", None);
    let cards = vec![
        ReadingCard {
            id: "quick".into(),
            title: "Quick".into(),
            lines: lines(&["Processes images almost instantly.", "Good for English and Chinese."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: estimate(Reading::Quick).into(),
            counting: !counted,
            size: "6 MB".into(),
            size_ok: room_for(&models, 6 << 20),
            device: quick_device,
            device_ok: true,
            recommended: false,
            available: true,
            ways: quick_ways,
            way: quick_way,
            info: info(&[
                "PP-OCRv6 tiny, by Baidu's PaddleOCR. [Learn more](models)",
                "6 MB. Runs on the processor, in the background.",
                "Reads English, other Latin-script languages and Chinese. Doesn't read Arabic or Cyrillic.",
                "About 0.15 s per image on most PCs; your mileage may vary.",
            ]),
        },
        ReadingCard {
            id: "balanced".into(),
            title: "Balanced".into(),
            lines: lines(&["More accurate, needs more processing.", "Good for English, European languages and Chinese."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: estimate(Reading::Balanced).into(),
            counting: !counted,
            size: "31 MB".into(),
            size_ok: room_for(&models, 31 << 20),
            device: balanced_device,
            device_ok: true,
            recommended: gpu.is_none(),
            available: true,
            ways: balanced_ways,
            way: balanced_way,
            info: info(&[
                "PP-OCRv6 small, by Baidu's PaddleOCR. [Learn more](models)",
                "31 MB. Runs on the processor, in the background.",
                "Reads English, European languages and Chinese, and is better than Quick with small text and photos of documents. Doesn't read Arabic or Cyrillic.",
                "About 0.4 s per image on most PCs; your mileage may vary.",
            ]),
        },
        ReadingCard {
            id: "deep".into(),
            title: "Thorough".into(),
            lines: lines(&["Reads text most accurately.", "Nearly every language, plus tables and layouts."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: estimate(Reading::Deep).into(),
            counting: !counted,
            size: "1.7 GB".into(),
            size_ok: room_for(&models, 1800 << 20),
            device: deep_device,
            device_ok: true,
            recommended: gpu.is_some(),
            available: true,
            ways: deep_ways,
            way: deep_way,
            info: info(&[
                "Balanced reads each image first, so it's searchable right away. Then PaddleOCR-VL-1.5 re-reads those with text on the graphics card ([Learn more](models)). Tables and layouts included.",
                "Requires llama.cpp (installed automatically if it isn't there).",
                "1.7 GB. Runs on the graphics card through llama.cpp, loaded only while reading.",
                "Reads 111 languages, including Arabic, Russian and Hindi ([Learn more](models)).",
                "About 1 to 3 s per image, depending on what else the PC is doing. Needs about 2.5 GB of the graphics card's memory; your mileage may vary.",
            ]),
        },
    ];
    setup.window.set_cards(ModelRc::new(VecModel::from(cards)));
    fill_seeing(setup);
}

/// What the models that find images by what they show weigh on disk, in MB as Windows counts them.
const SIGLIP_MB: u64 = 725;
const GEMMA_MB: u64 = 825;
const QWEN_MB: u64 = 5179;

/// How long they take for an image, in ms: measured on an i9-13900HX with 8 of its threads and an RTX 4080 Laptop GPU,
/// with the PC otherwise quiet. Measured on this PC once there's something to measure.
const SIGLIP_MS: f64 = 100.0;
const GEMMA_MS: f64 = 65.0;
const QWEN_MS: f64 = 270.0;
/// A graphics card that can hold Qwen3-VL-Embedding-8B while reading (8 GB cards report a little under).
const QWEN_GPU_MB: u64 = 7900;

fn strong_enough(gpu: Option<&winsys::gpu::Gpu>) -> bool {
    gpu.is_some_and(|gpu| gpu.dedicated_mb >= QWEN_GPU_MB)
}

/// The card that's chosen (each runs on the processor too, without a graphics card).
fn chosen_seeing(settings: &Settings, _gpu: bool) -> &'static str {
    match settings.seeing.as_str() {
        "quick" => "quick",
        "deep" => "deep",
        _ => "balanced",
    }
}

/// EmbeddingGemma 2 on the processor, and SigLIP 2 on the graphics card through DirectML, per image (ms): measured in
/// Excavate with the processor 64 to 98% busy with other work.
const GEMMA_CPU_MS: f64 = 3200.0;
const SIGLIP_GPU_MS: f64 = 20.0;

fn megabytes(mb: u64) -> String {
    if mb < 1000 { format!("{mb} MB") } else { format!("{:.1} GB", mb as f64 / 1024.0) }
}

/// The cards for finding images by what they show.
fn fill_seeing(setup: &Setup) {
    let settings = settings_now();
    let pictures = chosen_images(setup);
    let counted = setup.choices.iter().filter(|choice| choice.on).all(|choice| setup.counts.contains_key(&choice.path.to_lowercase()));
    let estimate = |per_image: f64| -> SharedString {
        if !counted {
            return SharedString::new();
        }
        if pictures == 0 {
            return "No images in these folders yet.".into();
        }
        let mut text = duration(per_image * pictures as f64);
        text[..1].make_ascii_uppercase();
        format!("{text} for your {}", plural(pictures, "image", "images")).into()
    };
    let gpu = winsys::gpu::capable();
    let strong = strong_enough(gpu.as_ref());
    let models = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as f64;
    // Measured here once a model has looked at some images; Qwen's 8B took about 4.5 times EmbeddingGemma 2's.
    let measured = |engine: &str| setup.status.as_ref().and_then(|status| status.looking.iter().find(|looking| looking.engine == engine).and_then(|looking| looking.ms));
    // Each card's own times, where it runs as chosen (what's measured here is for the choice that's running).
    let here = measured_here();
    let device = |id: &str| settings.seeing_device(id, gpu.is_some(), &here);
    let running = chosen_seeing(&settings, gpu.is_some());
    let measured_on = |id: &str, engine: &str| if id == running { measured(engine) } else { None };
    let siglip_cpu = SIGLIP_MS * (16.0 / cores.min(16.0)).powf(0.75);
    let siglip_ms = measured_on("quick", "siglip2-b16").unwrap_or(if device("quick") == "gpu" { SIGLIP_GPU_MS } else { siglip_cpu });
    let gemma = |id: &str| measured_on(id, "eg2-140").unwrap_or(if device(id) == "gpu" { GEMMA_MS } else { GEMMA_CPU_MS });
    let gemma_ms = gemma("balanced");
    let qwen_ms = measured_on("deep", "qwen3vl-8b").unwrap_or(gemma("deep") * QWEN_MS / GEMMA_MS);
    let most_accurate = setup.window.get_deep_choice() == 1;
    // The second model runs after the first on the same graphics card, or beside it on the processor.
    let (deep_mb, deep_ms) = if most_accurate { (GEMMA_MB + QWEN_MB, gemma("deep") + qwen_ms) }
        else if device("deep") == "gpu" { (GEMMA_MB + SIGLIP_MB, gemma("deep").max(siglip_cpu)) }
        else { (GEMMA_MB + SIGLIP_MB, gemma("deep") + siglip_cpu) };
    let vulkan = ("GPU, Vulkan", "Graphics card, Vulkan", "Fastest: about 0.06 to 0.13 s an image.");
    let (quick_device, quick_ways, quick_way) = image_ways(device("quick"), gpu.is_some(),
        ("GPU, DirectML", "Graphics card, DirectML", "Fastest: about 0.02 s an image, and leaves the processor free."), "About 0.1 to 0.2 s an image.", here.runs.get("seeing:siglip"));
    let (balanced_device, balanced_ways, balanced_way) = image_ways(device("balanced"), gpu.is_some(), vulkan, "About 3 s an image: 20 to 50× slower.", here.runs.get("seeing:gemma"));
    let (deep_device, deep_ways, deep_way) = image_ways(device("deep"), gpu.is_some(), vulkan, "About 3 s an image or more: 20 to 50× slower.", None);
    let cards = vec![
        ReadingCard {
            id: "quick".into(),
            title: "Quick".into(),
            lines: lines(&["Runs on any PC, no graphics card needed.", "Good for English and European languages."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: estimate(siglip_ms),
            counting: !counted,
            size: megabytes(SIGLIP_MB).into(),
            size_ok: room_for(&models, SIGLIP_MB << 20),
            device: quick_device,
            device_ok: true,
            recommended: gpu.is_none(),
            available: true,
            ways: quick_ways,
            way: quick_way,
            info: info(&[
                "SigLIP 2 base, by Google. [Learn more](models)",
                "725 MB. Runs on the graphics card when there is one, through DirectML (fastest), else on the processor. You can choose under the card.",
                "Finds photos well in English and European languages, and is the best of the three at telling kinds of screenshot apart: an invoice, a terminal, a chat. Weaker in Hindi, Japanese and Chinese.",
                "About 0.1 s per image on most PCs; your mileage may vary.",
            ]),
        },
        ReadingCard {
            id: "balanced".into(),
            title: "Balanced".into(),
            lines: lines(&["More accurate, in nearly every language."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: estimate(gemma_ms),
            counting: !counted,
            size: megabytes(GEMMA_MB).into(),
            size_ok: room_for(&models, GEMMA_MB << 20),
            device: balanced_device,
            device_ok: true,
            recommended: gpu.is_some(),
            available: true,
            ways: balanced_ways,
            way: balanced_way,
            info: info(&[
                "EmbeddingGemma 2, by Google. [Learn more](models)",
                "Requires llama.cpp (installed automatically if it isn't there).",
                "825 MB. Runs on the graphics card through llama.cpp, loaded only while looking at images. Needs about 1.7 GB of the graphics card's memory. It can run on the processor too, 20 to 50× slower.",
                "Finds the right image first two times in three in our test, and works in every language we tried.",
                "About 0.06 s per image on a recent graphics card; your mileage may vary.",
            ]),
        },
        ReadingCard {
            id: "deep".into(),
            title: "Thorough".into(),
            lines: lines(&["Balanced with a second model, for better results."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: estimate(deep_ms),
            counting: !counted,
            size: megabytes(deep_mb).into(),
            size_ok: room_for(&models, deep_mb << 20),
            device: deep_device,
            device_ok: true,
            recommended: false,
            available: true,
            ways: deep_ways,
            way: deep_way,
            info: info(&[
                "Balanced looks at each image first, so it can be found right away. Then a second model looks too, and their rankings are combined. [Learn more](models)",
                "Choose the second model under the card's description. The (i) beside it says what each one takes.",
                "Requires llama.cpp (installed automatically if it isn't there).",
            ]),
        },
    ];
    setup.window.set_seeing_cards(ModelRc::new(VecModel::from(cards)));
    setup.window.set_deep_rows(ModelRc::new(VecModel::from(vec![
        PickerRow {
            label: "A little better".into(),
            detail: "Light. Better at English and kinds of screenshot.".into(),
            models: "EmbeddingGemma 2 + SigLIP 2".into(),
            size: format!("+{}", megabytes(SIGLIP_MB)).into(),
            available: true,
        },
        PickerRow {
            label: "Most accurate".into(),
            detail: if strong { "Needs a strong GPU, more time and disk space." } else { "Needs a graphics card with 8 GB of memory or more." }.into(),
            models: "EmbeddingGemma 2 + Qwen3-VL-Embedding-8B".into(),
            size: format!("+{}", megabytes(QWEN_MB)).into(),
            available: strong,
        },
    ])));
    setup.window.set_deep_info(info(&[
        "**A little better** adds SigLIP 2 base, by Google. 725 MB more, on the processor, about 0.1 s per image. Better at English and at kinds of screenshot; no better in other languages.",
        "**Most accurate** adds Qwen3-VL-Embedding-8B, by Alibaba's Qwen team. 5.1 GB more. Needs a graphics card with 8 GB of memory or more, and uses about 6.5 GB of it while looking at images.",
        "About 0.27 s per image on an RTX 4080 Laptop GPU, about 4.5× Balanced's time. It finds the right image first 73% of the time in our test, against 66% for Balanced. While you search, part of it stays loaded for a few minutes (5 GB).",
        "Before downloading it, Excavate checks your graphics card, your free disk space and how long it would take, then tries 10 images first.",
    ]));
}

/// A frame of video with EmbeddingGemma 2 on the graphics card, decoding included, and the decoding's part (ms).
const VIDEO_FRAME_MS: f64 = 76.0;
const VIDEO_DECODE_MS: f64 = 11.0;

/// The cards for videos by what they show. They look with the models chosen for images, a frame every so often
/// (`Settings::video_pace`). Until measured here, an hour's estimate is its frames at an image's time: not yet benchmarked
/// for videos, and decoding comes on top.
fn fill_videos(setup: &Setup, settings: &Settings) {
    let gpu = winsys::gpu::capable().is_some();
    // Videos are looked at by the first model chosen for images, where it runs.
    let seeing = chosen_seeing(settings, gpu);
    let on_gpu = settings.seeing_device(seeing, gpu, &measured_here()) == "gpu";
    let measured = setup.status.as_ref().and_then(|status| status.looking.first()).and_then(|looking| looking.ms);
    // A frame's time with its decoding: an hour of 1080p took 9.1 s at Quick's pace, 54.6 s at Balanced's and 138 s at
    // Thorough's with EmbeddingGemma 2 on a quiet PC, 76 ms a frame; the others, an image's time
    // and the decoding's 11 ms.
    let frame_ms = measured.map(|ms| ms + VIDEO_DECODE_MS).unwrap_or(match (seeing, on_gpu) {
        ("quick", true) => SIGLIP_GPU_MS + VIDEO_DECODE_MS,
        ("quick", false) => SIGLIP_MS + VIDEO_DECODE_MS,
        (_, true) => VIDEO_FRAME_MS,
        (_, false) => GEMMA_CPU_MS + VIDEO_DECODE_MS,
    });
    let hour = |id: &str| -> SharedString {
        let pace = Settings { video: id.to_string(), ..Settings::default() }.video_pace();
        let frames = (3600.0 / pace.0).min(pace.1);
        format!("An hour of video in {}", duration(frames * frame_ms)).into()
    };
    let device: SharedString = if on_gpu { "GPU".into() } else { "CPU".into() };
    let card = |id: &str, title: &str, said: [&str; 2], frames: &str| ReadingCard {
        id: id.into(),
        title: title.into(),
        lines: lines(&said),
        requires: SharedString::new(),
        requires_met: true,
        estimate: hour(id),
        counting: false,
        size: "No more to download".into(),
        size_ok: true,
        device: device.clone(),
        device_ok: true,
        recommended: id == "balanced",
        available: true,
        info: info(&[
            frames,
            "Uses the models chosen under Images, so there's nothing more to download.",
            "The time is an estimate from how long an image takes; videos haven't been measured yet.",
        ]),
        ways: Default::default(),
        way: 0,
    };
    let cards = vec![
        card("quick", "Quick", ["A frame every 30 seconds.", "Finds what a video is mostly about."], "At most 120 frames a video: an hour's video gets one every 30 seconds, a longer one fewer."),
        card("balanced", "Balanced", ["A frame every 5 seconds.", "Finds most scenes, and opens at them."], "At most 720 frames a video: an hour's video gets one every 5 seconds, a longer one fewer."),
        card("deep", "Thorough", ["A frame every 2 seconds.", "Catches short moments too."], "At most 1,800 frames a video: an hour's video gets one every 2 seconds, a longer one fewer."),
    ];
    let window = &setup.window;
    window.set_video_cards(ModelRc::new(VecModel::from(cards)));
    window.set_video(match settings.video.as_str() {
        "quick" => "quick",
        "deep" => "deep",
        _ => "balanced",
    }.into());
    window.set_videos_on(settings.videos_on);
    window.set_video_text_on(settings.video_text);
    window.set_video_speech_on(settings.video_speech);
    fill_video_text(setup, settings, gpu);
}

/// The cards for text in videos: how often a video is looked at for new text. Measured on the test videos,
/// one thread with the detector on the graphics card: an hour of slides took
/// about 2.5, 5 and 8 min at the three paces (captions 2, 4.5 and 7.5), and found 85%, 95% and 99.8% of the slides'
/// words, 62%, 85% and 87% of the captions'.
fn fill_video_text(setup: &Setup, settings: &Settings, gpu: bool) {
    let card = |id: &str, title: &str, said: [&str; 2], hour: &str, every: &str| ReadingCard {
        id: id.into(),
        title: title.into(),
        lines: lines(&said),
        requires: SharedString::new(),
        requires_met: true,
        estimate: format!("An hour of video in about {hour}").into(),
        counting: false,
        size: "No more to download".into(),
        size_ok: true,
        device: if gpu { "CPU and GPU".into() } else { "CPU".into() },
        device_ok: true,
        recommended: id == "balanced",
        available: true,
        info: info(&[
            every,
            "Uses the models that read your images, so there's nothing more to download.",
            "Measured on slides and screen recordings, with a graphics card; videos without text take less. Your mileage may vary.",
        ]),
        ways: Default::default(),
        way: 0,
    };
    let cards = vec![
        card("quick", "Quick", ["A look every 4 seconds.", "Slides and screen recordings."], "2 min",
             "Each cut, and every 4 seconds where the text changes. In our test it found 85% of the words on slides, and 62% of captions'."),
        card("balanced", "Balanced", ["A look every 2 seconds.", "Finds most captions too."], "5 min",
             "Each cut, and every 2 seconds where the text changes. In our test it found 95% of the words on slides, and 85% of captions'."),
        card("deep", "Thorough", ["A look every second.", "Catches text that shows briefly."], "8 min",
             "Each cut, and every second where the text changes. In our test it found 99.8% of the words on slides, and 87% of captions'."),
    ];
    let window = &setup.window;
    window.set_video_text_cards(ModelRc::new(VecModel::from(cards)));
    window.set_video_text_choice(match settings.video_text_choice() {
        "quick" => "quick",
        "deep" => "deep",
        _ => "balanced",
    }.into());
}

/// "Looked at 12 of 40 videos", from the first model that looks.
fn video_progress(status: &Status) -> String {
    let Some(looking) = status.looking.first() else { return String::new() };
    let all = looking.videos_done + looking.videos_queued;
    if all == 0 {
        return String::new();
    }
    if looking.videos_queued == 0 {
        return format!("Looked at all {}.", plural(all, "video", "videos"));
    }
    format!("Looked at {} of {}.", group(looking.videos_done), plural(all, "video", "videos"))
}

/// The listener's models, as published, with the listener itself (18 MB).
const PARAKEET_MB: u64 = 638 + 18;
const TURBO_MB: u64 = 834 + 18;
const WHISPER_LARGE_MB: u64 = 1031 + 18;

/// The card that's chosen: Thorough needs a graphics card, so without one it's Balanced (on the processor).
fn chosen_listening(settings: &Settings, gpu: bool) -> &'static str {
    match settings.listening.as_str() {
        "quick" => "quick",
        "deep" if gpu => "deep",
        _ => "balanced",
    }
}

/// The cards for recordings by what's said in them, and the rows beside them.
fn fill_listening(setup: &Setup, settings: &Settings) {
    let gpu = winsys::gpu::capable();
    let nvidia = gpu.as_ref().is_some_and(|gpu| gpu.name.to_ascii_lowercase().contains("nvidia"));
    let models = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    // Measured here once it has listened to some recordings: a minute of sound in this many milliseconds.
    let cuda = nvidia && config::LISTEN_CUDA_PUBLISHED;
    let here = measured_here();
    let device = |id: &str| settings.listen_device(id, gpu.is_some(), cuda, &here);
    let measured = setup.status.as_ref().and_then(|status| status.listening.as_ref())
        .filter(|listening| listening.engine == chosen_ear(settings, gpu.is_some()) && listening.device.eq_ignore_ascii_case(device(&settings.listening)))
        .and_then(|listening| listening.ms_a_minute);
    // Where each card can run, for its device menu, with the one chosen: the graphics card's ways (Vulkan, and CUDA on
    // NVIDIA's), then the processor. Nothing to choose without a graphics card.
    let ways = |id: &str, on_gpu: &str, on_cpu: Option<&str>| -> (SharedString, ModelRc<CardWay>, i32) {
        let mut ways: Vec<(&str, CardWay)> = Vec::new();
        if gpu.is_some() {
            if cuda {
                ways.push(("vulkan", CardWay { label: "GPU, Vulkan".into(), title: "Graphics card, Vulkan".into(), note: "Comes with your driver. Nothing more to download.".into() }));
                ways.push(("cuda", CardWay { label: "GPU, CUDA".into(), title: "Graphics card, CUDA".into(), note: "A little faster on longer recordings. 660 MB more.".into() }));
            } else {
                ways.push(("vulkan", CardWay { label: "GPU".into(), title: "Graphics card".into(), note: format!("{on_gpu}.").into() }));
            }
            if let Some(on_cpu) = on_cpu {
                ways.push(("cpu", CardWay { label: "CPU".into(), title: "Processor".into(), note: format!("{on_cpu}.").into() }));
            }
        }
        // What this PC measured, in place of the benchmarks' figures: a minute of sound's time, an hour's here.
        if let Some(run) = here.runs.get(&format!("listening:{id}")) {
            for (key, way) in &mut ways {
                if let Some(ms) = run.ms.get(*key) {
                    let minutes = ms * 60.0 / 60_000.0;
                    way.note = if minutes < 1.0 { "Measured on this PC: an hour of speech in under a minute.".into() }
                        else { format!("Measured on this PC: an hour of speech in about {minutes:.0} min.").into() };
                }
            }
        }
        let chosen = device(id);
        let way = ways.iter().position(|(key, _)| *key == chosen).unwrap_or(0);
        let label = ways.get(way).map(|(_, way)| way.label.clone()).unwrap_or_else(|| if gpu.is_some() { "GPU".into() } else { "CPU".into() });
        // A single way is no choice.
        let ways: Vec<CardWay> = if ways.len() > 1 { ways.into_iter().map(|(_, way)| way).collect() } else { Vec::new() };
        (label, ModelRc::new(VecModel::from(ways)), way as i32)
    };
    let (quick_device, quick_ways, quick_way) = ways("quick", "An hour of speech in about a minute", Some("An hour of speech in about 4 min"));
    let (balanced_device, balanced_ways, balanced_way) = ways("balanced", "An hour of speech in under a minute", Some("An hour of speech in about 25 min"));
    let (deep_device, deep_ways, deep_way) = ways("deep", "An hour of speech in about 2.5 min", None);
    let quick_gpu = device("quick") != "cpu";
    let balanced_gpu = device("balanced") != "cpu";
    // An hour of speech: Parakeet on the processor ~4 min, Whisper turbo ~40 s on the graphics card (25 min on the
    // processor), large-v3 ~2.5 min.
    let hour = |measured_here: Option<f64>, otherwise: &str| -> SharedString {
        match measured_here {
            Some(ms) => {
                let minutes = ms * 60.0 / 60_000.0;
                if minutes < 1.0 { "An hour of speech in under a minute here".into() } else { format!("An hour of speech in about {minutes:.0} min here").into() }
            }
            None => otherwise.into(),
        }
    };
    let chosen = chosen_listening(settings, gpu.is_some());
    let mine = |id: &str| (chosen == id).then_some(measured).flatten();
    let cards = vec![
        ReadingCard {
            id: "quick".into(),
            title: "Quick".into(),
            lines: lines(&["Fast on any PC, no graphics card needed.", "English and 24 other European languages."]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: hour(mine("quick"), if quick_gpu { "An hour of speech in about a minute" } else { "An hour of speech in about 4 min" }),
            counting: false,
            size: megabytes(PARAKEET_MB).into(),
            size_ok: room_for(&models, PARAKEET_MB << 20),
            device: quick_device,
            device_ok: true,
            recommended: false,
            available: true,
            ways: quick_ways,
            way: quick_way,
            info: info(&[
                "Parakeet TDT 0.6B v3, by NVIDIA. [Learn more](models)",
                "640 MB. Runs on the processor, in the background, through whisper.cpp.",
                "Nearly as accurate as Whisper in English and 24 other European languages, and knows no others.",
                "About 4 minutes for an hour of speech on a recent laptop; your mileage may vary.",
            ]),
        },
        ReadingCard {
            id: "balanced".into(),
            title: "Balanced".into(),
            lines: lines(&["Nearly every language.", if balanced_gpu { "Uses the graphics card." } else { "Slower on the processor." }]),
            requires: SharedString::new(),
            requires_met: true,
            estimate: hour(mine("balanced"), if balanced_gpu { "An hour of speech in under a minute" } else { "An hour of speech in about 25 min" }),
            counting: false,
            size: megabytes(TURBO_MB).into(),
            size_ok: room_for(&models, TURBO_MB << 20),
            device: balanced_device,
            device_ok: true,
            recommended: true,
            available: true,
            ways: balanced_ways,
            way: balanced_way,
            info: info(&[
                "Whisper large-v3-turbo, by OpenAI. [Learn more](models)",
                "835 MB. Runs on the graphics card when there is one (about 1.2 GB of its memory, loaded only while listening), on the processor otherwise.",
                "The most accurate of the three in most languages we tried, and the only one for Arabic, Japanese and Chinese. Told your languages, it mistakes them less.",
                "About 40 seconds for an hour of speech on a recent graphics card, 25 minutes on the processor; your mileage may vary.",
            ]),
        },
        ReadingCard {
            id: "deep".into(),
            title: "Thorough".into(),
            lines: lines(&["Most accurate, nearly every language."]),
            requires: "Requires a GPU".into(),
            requires_met: gpu.is_some(),
            estimate: if gpu.is_some() { hour(mine("deep"), "An hour of speech in about 2.5 min") } else { SharedString::new() },
            counting: false,
            size: megabytes(WHISPER_LARGE_MB).into(),
            size_ok: room_for(&models, WHISPER_LARGE_MB << 20),
            device: if gpu.is_some() { deep_device } else { "GPU".into() },
            device_ok: gpu.is_some(),
            recommended: false,
            available: gpu.is_some(),
            ways: deep_ways,
            way: deep_way,
            info: info(&[
                "Whisper large-v3, by OpenAI. [Learn more](models)",
                "1 GB. Runs on the graphics card, loaded only while listening; needs about 1.9 GB of its memory.",
                "About 7% fewer mistakes than Balanced in our test (most in French, Arabic and Hindi), at a quarter of its speed.",
                "About 2.5 minutes for an hour of speech on a recent graphics card; your mileage may vary.",
            ]),
        },
    ];
    let window = &setup.window;
    // For the Videos page's "What's said": the listener chosen here, and where it runs.
    if let Some(card) = cards.iter().find(|card| card.id == chosen) {
        let name = match chosen { "quick" => "Parakeet", "deep" => "Whisper large-v3", _ => "Whisper turbo" };
        window.set_video_listener(format!("Listener: {name} · {}", card.device).into());
    }
    window.set_listen_cards(ModelRc::new(VecModel::from(cards)));
    window.set_listening(chosen.into());
    window.set_listening_on(settings.listening_on);
    window.set_listen_schedule(schedule_index(settings.listen_schedule()));
    window.set_listen_schedule_every(every_index(&settings.listen_schedule_every));
    window.set_listen_on_battery(settings.listen_on_battery);
    window.set_languages_note(languages_note().into());
}

/// The indexer's name for the model the choice listens with.
fn chosen_ear(settings: &Settings, gpu: bool) -> &'static str {
    match chosen_listening(settings, gpu) {
        "quick" => "parakeet",
        "deep" => "whisper-large",
        _ => "whisper-turbo",
    }
}

/// "English and Arabic, from Windows. Telling the model cuts its mistakes."
fn languages_note() -> String {
    let mut names: Vec<String> = Vec::new();
    for tag in winsys::registry::languages() {
        let name = winsys::registry::language_name(&tag);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let list = match names.len() {
        0 => return "Windows doesn't list any. Whisper tells the language itself.".into(),
        1 => names[0].clone(),
        _ => format!("{} and {}", names[..names.len() - 1].join(", "), names[names.len() - 1]),
    };
    format!("{list}, from Windows. Telling the model cuts its mistakes.")
}

/// "Listen to everything now": whether it's going.
fn fill_listen_all(setup: &Setup, settings: &Settings) {
    let status = setup.status.as_ref();
    let on = settings.listen_all_at > 0 && (status.is_some_and(|status| status.listen_all) || crate::app::now() - settings.listen_all_at < 20);
    setup.window.set_listening_all(on);
    let left = status.and_then(|status| status.listening.as_ref()).map_or(0, |listening| listening.queued);
    setup.window.set_listen_all_note(if on {
        format!("Listening to every recording in one sitting, even while you use the PC: {} to go.", group(left))
    } else {
        "Once, whatever the schedule, until every recording is done.".to_string()
    }.into());
    let note = match status.filter(|status| status.listen_wait && left > 0) {
        Some(status) if status.listen_at > 0 => format!("{} will be listened to at the next scheduled time.", plural(left, "new recording", "new recordings")),
        Some(_) => format!("{} waiting.", plural(left, "new recording", "new recordings")),
        None => String::new(),
    };
    setup.window.set_listen_schedule_note(note.into());
}

/// "Balanced has listened to 42 of 310 recordings."
fn listen_progress(status: &Status) -> String {
    let Some(listening) = status.listening.as_ref() else { return String::new() };
    let name = match listening.engine.as_str() {
        "parakeet" => "Quick",
        "whisper-large" => "Thorough",
        _ => "Balanced",
    };
    let total = listening.done + listening.queued;
    if listening.model.state == "downloading" && listening.model.total > 0 {
        return format!("Getting {name}'s model: {} of {}.", crate::format::size(listening.model.received), crate::format::size(listening.model.total));
    }
    if listening.model.state == "failed" {
        return format!("{name}'s model couldn't be downloaded: {}. Trying again soon.", listening.model.error);
    }
    if total == 0 {
        return "No recordings in the folders being read yet.".into();
    }
    let mut line = format!("{name} has listened to {} of {}.", group(listening.done), plural(total, "recording", "recordings"));
    if !listening.waiting.is_empty() && listening.queued > 0 {
        line = format!("{line} {}.", listening.waiting);
    }
    line
}

/// The theme's tiles and name, and the Workshop's readout while it's open.
fn fill_themes(setup: &Setup, settings: &Settings) {
    let dark = setup.window.global::<Theme>().get_dark();
    let tiles: Vec<ThemeTile> = themes::all(settings).into_iter().map(|palette| {
        let [r, g, b] = palette.accent(dark);
        ThemeTile { name: palette.name.into(), color: slint::Color::from_rgb_u8(r, g, b), own: palette.own }
    }).collect();
    setup.window.set_theme_tiles(ModelRc::new(VecModel::from(tiles)));
    setup.window.set_theme_name(themes::current(settings).name.into());
    fill_workshop(setup);
}

fn fill_workshop(setup: &Setup) {
    let Some(palette) = setup.workshop.as_ref() else { return };
    let dark = setup.window.global::<Theme>().get_dark();
    let (checks, ok) = themes::checks(palette);
    setup.window.set_workshop_hex(themes::hex(palette.accent(dark)).into());
    setup.window.set_workshop_checks(lines(&checks.iter().map(String::as_str).collect::<Vec<_>>()));
    setup.window.set_workshop_checks_ok(ok);
}

/// The Workshop's draft from what's in its controls: the window wears it.
fn workshop_edited(app: &App) {
    with_setup(|setup| {
        let window = &setup.window;
        window.set_workshop_error(SharedString::new());
        setup.workshop = Some(themes::workshop(&window.get_workshop_name(), window.get_workshop_hue(), themes::Surfaces::from_index(window.get_workshop_surfaces())));
    });
    apply_theme(app);
    with_setup(|setup| fill_workshop(setup));
}

/// The theme changed for good: every window follows.
fn theme_saved(app: &App) {
    with_setup(|setup| {
        setup.workshop = None;
        setup.window.set_workshop_open(false);
        setup.window.set_workshop_error(SharedString::new());
    });
    changed(app, false);
    crate::popup::theme_changed(app);
    fill(app);
}

/// Shown only when there's a conflict: Windows refused the shortcut, so it's held by another app.
fn hotkey_note(hotkey: &str) -> String {
    match crate::popup::tray_hearing() {
        Some(winsys::tray::Hearing::Hooked) => match hotkey.parse().ok().and_then(crate::launchers::holder) {
            Some(who) => format!("{who} uses {hotkey}. Excavate catches it first while it runs. Choose another shortcut if you'd rather leave it to {who}."),
            None => format!("Another app uses {hotkey}. Excavate catches it first while it runs. Choose another shortcut if you'd rather leave it to that app."),
        },
        None => format!("{hotkey} isn't working: another app holds it. Choose another shortcut."),
        _ => String::new(),
    }
}

/// Saves the settings, and tells the indexer (a moment later) when reading is concerned.
fn changed(app: &App, reading: bool) {
    app.settings.borrow().save(&app.paths);
    if reading {
        with_setup(|setup| {
            if setup.onboarding {
                return;
            }
            setup.indexer_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(1200), || {
                with_app(|app| app.indexer.settings_changed());
            });
        });
    }
}

fn save_folders(app: &App) {
    let folders = with_setup(|setup| {
        let paths = |on: bool| setup.choices.iter().filter(|choice| choice.on == on).map(|choice| choice.path.clone()).collect::<Vec<_>>();
        (paths(true), paths(false))
    });
    if let Some((on, off)) = folders {
        let mut settings = app.settings.borrow_mut();
        settings.folders = on;
        settings.folders_off = Some(off);
        drop(settings);
        changed(app, true);
    }
}

/// Adds to what's left out; false (saying why, by the field) when it can't go in the list.
fn exclusion_added(app: &App, text: &str) -> bool {
    let text = text.trim().trim_matches('"').to_string();
    if text.is_empty() {
        return true;
    }
    if let Err(error) = config::check_exclusion(&text) {
        with_setup(|setup| setup.window.set_exclusion_error(error.into()));
        return false;
    }
    {
        let mut settings = app.settings.borrow_mut();
        if settings.exclude.iter().any(|existing| existing.eq_ignore_ascii_case(&text)) {
            return true;
        }
        settings.exclude.push(text);
    }
    changed(app, true);
    fill(app);
    count_folders(app, true);
    true
}

/// The window's size in logical pixels, kept through moves between monitors of different scales.
///
/// Dragged across the edge between two such monitors, the scale can flip back and forth within milliseconds (the window
/// shrinks or grows, and the monitor with most of it changes again). Windows then applies one more resize worked out
/// from a size already scaled, and the window grows by the ratio each time (measured: 1400 × 1060 to 2198 × 1684 in one
/// drag). So the size the person gave it is remembered, and put back once the scale has settled.
#[derive(Default)]
struct KeptSize {
    /// The size in logical pixels, from the last resize that wasn't part of a scale change.
    wanted: Rc<Cell<Option<winit::dpi::LogicalSize<f64>>>>,
    /// When the scale last changed.
    changed: Rc<Cell<Option<Instant>>>,
    settle: Rc<slint::Timer>,
}

impl KeptSize {
    /// Resizes within this long of a scale change belong to it.
    const SETTLE: Duration = Duration::from_millis(400);

    fn follow(&self, window: &slint::Window, event: &winit::event::WindowEvent, weak: &slint::Weak<SettingsWindow>) {
        match event {
            winit::event::WindowEvent::ScaleFactorChanged { .. } => {
                self.changed.set(Some(Instant::now()));
                let (wanted, weak) = (self.wanted.clone(), weak.clone());
                self.settle.start(slint::TimerMode::SingleShot, Self::SETTLE, move || {
                    crate::living::rescaled();
                    let Some(window) = weak.upgrade() else { return };
                    let Some(wanted) = wanted.get() else { return };
                    window.window().with_winit_window(|window| {
                        if window.is_maximized() || window.is_minimized() == Some(true) {
                            return;
                        }
                        let now = window.inner_size().to_logical::<f64>(window.scale_factor());
                        if (now.width - wanted.width).abs() > 2.0 || (now.height - wanted.height).abs() > 2.0 {
                            log(&format!("settings window: {:.0} × {:.0} after a scale change, put back to {:.0} × {:.0}", now.width, now.height, wanted.width, wanted.height));
                            let _ = window.request_inner_size(wanted);
                        }
                    });
                });
            }
            winit::event::WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                let settling = self.changed.get().is_some_and(|at| at.elapsed() < Self::SETTLE);
                // At the scale this resize was made for: Slint's, which follows the events in the order they come.
                // (winit's is already the next one when Windows sent a resize from inside a scale change, delivered later.)
                let maximized = window.with_winit_window(|window| window.is_maximized()).unwrap_or(false);
                if !settling && !maximized {
                    self.wanted.set(Some(size.to_logical(window.scale_factor() as f64)));
                }
            }
            _ => {}
        }
    }
}

fn wire(window: &SettingsWindow) {
    // A finger leaves no hover, so the demos need to know what the last pointer was. (Windows follows a touch with
    // pretend mouse moves, so those don't count right after one.)
    let weak = window.as_weak();
    let touched = Cell::new(None::<Instant>);
    let size = KeptSize::default();
    window.window().on_winit_window_event(move |winit_window, event| {
        size.follow(winit_window, event, &weak);
        if let (winit::event::WindowEvent::Focused(false), Some(window)) = (event, weak.upgrade()) {
            let theme = window.global::<Theme>();
            theme.set_menus_closed(theme.get_menus_closed() + 1);
        }
        // Slint draws (and presents) right after this event; what's queued now runs once the frame is out.
        if matches!(event, winit::event::WindowEvent::RedrawRequested) && CLOAKED.with(Cell::get) {
            let _ = slint::invoke_from_event_loop(uncloak);
        }
        let touch = match event {
            winit::event::WindowEvent::Touch(_) => {
                touched.set(Some(Instant::now()));
                Some(true)
            }
            winit::event::WindowEvent::CursorMoved { .. } if touched.get().is_none_or(|at| at.elapsed() > Duration::from_secs(1)) => Some(false),
            _ => None,
        };
        if let (Some(touch), Some(window)) = (touch, weak.upgrade()) {
            window.global::<Theme>().set_touch(touch);
        }
        EventResult::Propagate
    });
    window.on_search_paused(crate::demo::hold);
    window.on_show_in_folder(|path| {
        let _ = winsys::shell::reveal(&path);
    });
    window.on_add_read_folders(|| with_app(|app| {
        {
            let folders = with_setup(|setup| setup.choices.iter().filter(|choice| choice.on).map(|choice| choice.path.clone()).collect::<Vec<_>>()).unwrap_or_default();
            let mut settings = app.settings.borrow_mut();
            for folder in folders {
                if !settings.name_folders.iter().any(|existing| existing.eq_ignore_ascii_case(&folder)) {
                    settings.name_folders.push(folder);
                }
            }
        }
        changed(app, false);
        app.scope_changed();
        fill(app);
    }));
    window.on_record_hotkey(|| with_app(|app| {
        crate::popup::tray_suspend();
        with_setup(|setup| {
            setup.hotkey_before = Some(app.settings.borrow().hotkey.clone());
            setup.window.set_recording(true);
            setup.window.set_hotkey_error(SharedString::new());
        });
    }));
    window.on_cancel_recording(|| with_app(|app| {
        let before = with_setup(|setup| {
            setup.window.set_recording(false);
            setup.hotkey_before.take()
        }).flatten().unwrap_or_else(|| app.settings.borrow().hotkey.clone());
        if let Ok(hotkey) = before.parse() {
            let _ = crate::popup::tray_set_hotkey(app, hotkey);
        }
        fill(app);
    }));
    window.on_hotkey_pressed(|control, alt, shift, meta, key| with_app(|app| {
        let mut parts = Vec::new();
        if control { parts.push("Ctrl".to_string()); }
        if alt { parts.push("Alt".to_string()); }
        if shift { parts.push("Shift".to_string()); }
        if meta { parts.push("Win".to_string()); }
        let key = if key.chars().count() == 1 { key.to_uppercase() } else { key.to_string() };
        parts.push(key);
        let text = parts.join("+");
        match text.parse::<winsys::tray::Hotkey>() {
            Err(error) => {
                with_setup(|setup| setup.window.set_hotkey_error(error.into()));
            }
            Ok(hotkey) => match crate::popup::tray_set_hotkey(app, hotkey) {
                Ok(_) => {
                    app.settings.borrow_mut().hotkey = hotkey.to_string();
                    changed(app, false);
                    with_setup(|setup| {
                        setup.window.set_recording(false);
                        setup.hotkey_before = None;
                        setup.window.set_hotkey_error(SharedString::new());
                    });
                    log(&format!("hotkey is now {hotkey}"));
                    fill(app);
                }
                Err(error) => {
                    with_setup(|setup| setup.window.set_hotkey_error(format!("{error}. Try another.").into()));
                }
            },
        }
    }));
    window.on_everything_action(|| with_app(|app| {
        if app.everything_fixing().is_some() {
            return;
        }
        if matches!(app.everything_health(), everything::Health::Behind(_)) && everything_installed() {
            crate::popup::fix_everything(app);
            return;
        }
        if everything_installed() {
            crate::popup::check_everything(app);
            with_setup(|setup| setup.window.set_everything_state("checking".into()));
            let timer = slint::Timer::default();
            timer.start(slint::TimerMode::SingleShot, Duration::from_secs(3), || with_app(|app| check_everything(app)));
            std::mem::forget(timer);
        } else {
            install_everything(app);
        }
    }));
    window.on_everything_custom(|| with_app(|app| {
        // The normal installer, to choose where and how: its first screen asks.
        let folder = app.paths.local.join("downloads");
        std::thread::spawn(move || {
            let result = crate::everything_setup::download(&folder, &mut |_, _| {}).and_then(|installer| {
                winsys::shell::open(0, &installer.display().to_string())
            });
            if let Err(error) = result {
                log(&format!("Everything's installer: {error}"));
                let _ = winsys::shell::open(0, "https://www.voidtools.com/downloads/");
            }
        });
    }));
    window.on_learn_more(|page| with_app(|app| crate::docs::open(app, &page)));
    window.on_folder_toggled(|index, on| with_app(|app| {
        with_setup(|setup| {
            if let Some(choice) = setup.choices.get_mut(index as usize) {
                choice.on = on;
            }
            fill_folders(setup);
            fill_cards(setup);
            fill_names_missing(setup, &app.settings.borrow());
        });
        save_folders(app);
    }));
    window.on_folder_kind_toggled(|index, kind| with_app(|app| {
        let Some(path) = with_setup(|setup| setup.choices.get(index as usize).map(|choice| choice.path.clone())).flatten() else { return };
        let Some(&kind) = config::FOLDER_KINDS.get(kind as usize) else { return };
        {
            let mut settings = app.settings.borrow_mut();
            let off = settings.folder_kinds.entry(config::folder_key(&path)).or_default();
            if let Some(at) = off.iter().position(|left| left == kind) {
                off.remove(at);
            } else {
                off.push(kind.to_string());
            }
            // Only the exceptions are kept.
            settings.folder_kinds.retain(|_, off| !off.is_empty());
        }
        changed(app, true);
        fill(app);
    }));
    window.on_add_folder(|| with_app(|app| {
        let Some(path) = winsys::dialog::pick_folder(handle().unwrap_or(0), "Choose a folder for Excavate to read") else { return };
        let added = with_setup(|setup| {
            if setup.choices.iter().any(|choice| choice.path.eq_ignore_ascii_case(&path)) {
                if let Some(choice) = setup.choices.iter_mut().find(|choice| choice.path.eq_ignore_ascii_case(&path)) {
                    choice.on = true;
                }
            } else {
                setup.choices.push(Choice { path: path.clone(), on: true, custom: true });
            }
            fill_folders(setup);
            fill_names_missing(setup, &app.settings.borrow());
        });
        if added.is_some() {
            save_folders(app);
            count_folders(app, false);
        }
    }));
    window.on_remove_folder(|index| with_app(|app| {
        with_setup(|setup| {
            if (index as usize) < setup.choices.len() {
                setup.choices.remove(index as usize);
            }
            fill_folders(setup);
            fill_cards(setup);
            fill_names_missing(setup, &app.settings.borrow());
        });
        save_folders(app);
    }));
    window.on_exclusion_added(|text| {
        let mut went_in = false;
        with_app(|app| went_in = exclusion_added(app, &text));
        went_in
    });
    window.on_exclusion_removed(|index| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            if (index as usize) < settings.exclude.len() {
                settings.exclude.remove(index as usize);
            }
        }
        changed(app, true);
        fill(app);
        count_folders(app, true);
    }));
    window.on_names_scope_chosen(|index| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            settings.names_scope = if index == 1 { "folders" } else { "everything" }.to_string();
            // Starts from the folders it reads.
            if index == 1 && settings.name_folders.is_empty() {
                let folders = with_setup(|setup| setup.choices.iter().filter(|choice| choice.on).map(|choice| choice.path.clone()).collect::<Vec<_>>());
                settings.name_folders = folders.unwrap_or_default();
            }
        }
        changed(app, false);
        app.scope_changed();
        fill(app);
    }));
    window.on_leave_out_apps_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().names_leave_out_apps = on;
        changed(app, false);
        app.scope_changed();
        fill(app);
    }));
    window.on_name_folder_added(|| with_app(|app| {
        let Some(path) = winsys::dialog::pick_folder(handle().unwrap_or(0), "Choose a folder to find files by name in") else { return };
        {
            let mut settings = app.settings.borrow_mut();
            if settings.name_folders.iter().any(|existing| existing.eq_ignore_ascii_case(&path)) {
                return;
            }
            settings.name_folders.push(path);
        }
        changed(app, false);
        app.scope_changed();
        fill(app);
    }));
    window.on_name_folder_removed(|index| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            if (index as usize) < settings.name_folders.len() {
                settings.name_folders.remove(index as usize);
            }
        }
        changed(app, false);
        app.scope_changed();
        fill(app);
    }));
    window.on_pause_clicked(|| {
        let mut menu = false;
        with_app(|app| {
            if app.settings.borrow().paused(crate::app::now()) {
                crate::popup::pause(app, 0);
            } else {
                with_setup(|setup| setup.window.set_pause_rows(pause_rows(app)));
                menu = true;
            }
        });
        menu
    });
    window.on_pause_chosen(|index| with_app(|app| {
        crate::popup::pause_chosen(app, [crate::TRAY_PAUSE_HOUR, crate::TRAY_PAUSE_TOMORROW, crate::TRAY_PAUSE_UNTIL_ON][index.clamp(0, 2) as usize]);
    }));
    window.on_monitors_opening(|| with_app(|app| {
        let settings = app.settings.borrow();
        with_setup(|setup| fill_monitor(setup, &settings));
    }));
    window.on_gpu_chosen(|index| with_app(|app| {
        let cards = winsys::gpu::adapters();
        let chosen = match index as usize {
            0 => String::new(),
            index if index <= cards.len() => cards[index - 1].name.clone(),
            _ => "none".to_string(),
        };
        if app.settings.borrow().graphics_card == chosen {
            return;
        }
        app.settings.borrow_mut().graphics_card = chosen.clone();
        winsys::gpu::choose(&chosen);
        // The reader starts over on the new card, and so does the helper that searches with the models.
        changed(app, true);
        app.seeker.stop();
        fill(app);
    }));
    window.on_monitor_chosen(|index| with_app(|app| {
        let screens = with_setup(|setup| setup.screens.clone()).unwrap_or_default();
        {
            let mut settings = app.settings.borrow_mut();
            if index == 0 {
                settings.monitor.clear();
                settings.monitor_name.clear();
            } else if let Some(screen) = screens.into_iter().nth(index as usize - 1) {
                settings.monitor = screen.id;
                settings.monitor_name = screen.name;
            }
            // (Past the list is the row of an unplugged monitor, which stays chosen.)
        }
        changed(app, false);
        fill(app);
    }));
    window.on_new_search_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().new_search_each_time = on;
        changed(app, false);
        fill(app);
    }));
    window.on_typed_toggles_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().typed_toggles = on;
        changed(app, false);
        fill(app);
    }));
    window.on_own_index_toggled(|on| with_app(|app| {
        // Bedrock chosen with nothing built yet: asked first whether Windows may let it read the drives directly.
        if on && crate::own_index::from_nothing(app) {
            with_setup(|setup| {
                setup.after_admin_ask = None;
                setup.window.set_admin_ask(true);
            });
            // (The switch shows what's in force until the answer.)
            fill(app);
            return;
        }
        set_own_index(app, on);
    }));
    // The Indexers step's "Continue": Bedrock or Everything, as chosen there; Bedrock with nothing built yet asks
    // first, and the next step comes once it's answered.
    window.on_indexers_continue(|bedrock| with_app(|app| {
        let own = app.settings.borrow().own_index;
        if bedrock && !own && crate::own_index::from_nothing(app) {
            with_setup(|setup| {
                setup.after_admin_ask = Some(setup.window.get_step() + 1);
                setup.window.set_admin_ask(true);
            });
            return;
        }
        if !bedrock {
            crate::popup::EVERYTHING_CHOSEN.set(true);
        }
        if bedrock != own {
            set_own_index(app, bedrock);
        } else if !bedrock {
            // Everything, as already in force: started now that it's chosen.
            crate::popup::check_everything(app);
        }
        next_step();
    }));
    window.on_journal_answered(|yes| with_app(|app| {
        let drive = crate::own_index::state().and_then(|state| state.relisting.map(|now| now.drive).or(state.relisted.map(|last| last.drive)))
            .unwrap_or_else(|| r"C:\".to_string());
        if yes {
            crate::own_index::fix_journal(drive);
        } else {
            log("journal: not made larger (Not now)");
        }
        fill(app);
    }));
    window.on_admin_answered(|allow| with_app(|app| {
        if allow {
            crate::own_index::build_then_on(app);
        } else {
            log("own index: listed the usual way (administrator rights not asked for)");
            set_own_index(app, true);
        }
        if with_setup(|setup| setup.after_admin_ask.take().is_some()) == Some(true) {
            next_step();
        }
        check_everything(app);
        fill(app);
    }));
    window.on_popup_size_chosen(|index| with_app(|app| {
        // (Past them is the row of a size dragged by hand, which stays chosen.)
        if let Some(&(_, width, height)) = config::POPUP_SIZES.get(index as usize) {
            let mut settings = app.settings.borrow_mut();
            settings.popup_width = width;
            settings.popup_height = height;
        }
        changed(app, false);
        fill(app);
    }));
    window.on_time_format_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().time_format = ["windows", "12-hour", "24-hour"][index.clamp(0, 2) as usize].to_string();
        // (While paused, what the rail says comes from the indexer, which says it again when told.)
        let paused = app.settings.borrow().paused(crate::app::now());
        changed(app, paused);
        // The results' times.
        app.edited(&app.query_text());
        fill(app);
    }));
    window.on_read_all(|on| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            settings.read_all_at = if on { crate::app::now() } else { 0 };
            // Asked for now: a pause wouldn't make sense.
            if on {
                settings.paused_until = 0;
            }
        }
        changed(app, true);
        crate::popup::refresh_tray_menu(app);
        fill(app);
    }));
    window.on_exclude_folder(|| with_app(|app| {
        let Some(path) = winsys::dialog::pick_folder(handle().unwrap_or(0), "Choose a folder Excavate should leave out") else { return };
        {
            let mut settings = app.settings.borrow_mut();
            if settings.exclude.iter().any(|existing| existing.eq_ignore_ascii_case(&path)) {
                return;
            }
            settings.exclude.push(path);
        }
        changed(app, true);
        fill(app);
        count_folders(app, true);
    }));
    window.on_reading_chosen(|id| with_app(|app| {
        app.settings.borrow_mut().reading = Reading::parse(&id).name().to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_pictures_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().pictures_on = on;
        changed(app, true);
        fill(app);
    }));
    window.on_schedule_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().schedule = ["arrive", "scheduled", "manual"][index.clamp(0, 2) as usize].to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_every_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().schedule_every = ["day", "week", "month"][index.clamp(0, 2) as usize].to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_at_stepped(|by| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            // By half hours, around the clock.
            let minutes = (settings.schedule_minutes() as i32 + by * 30).rem_euclid(24 * 60);
            settings.schedule_at = format!("{:02}:{:02}", minutes / 60, minutes % 60);
        }
        changed(app, true);
        fill(app);
    }));
    window.on_read_now(|| with_app(|app| {
        app.settings.borrow_mut().read_pictures_at = crate::app::now();
        changed(app, true);
        with_setup(|setup| setup.window.set_schedule_note("Reading new images now.".into()));
    }));
    window.on_reread(|| with_app(|app| {
        app.settings.borrow_mut().reread_pictures_at = crate::app::now();
        changed(app, true);
        with_setup(|setup| setup.window.set_measured("Your images will be read again, newest first.".into()));
    }));
    window.on_battery_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().read_on_battery = on;
        changed(app, true);
        fill(app);
    }));
    // The same for what images show, apart.
    window.on_seeing_schedule_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().seeing_schedule = ["arrive", "scheduled", "manual"][index.clamp(0, 2) as usize].to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_seeing_every_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().seeing_schedule_every = ["day", "week", "month"][index.clamp(0, 2) as usize].to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_seeing_at_stepped(|by| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            let minutes = (settings.seeing_schedule_minutes() as i32 + by * 30).rem_euclid(24 * 60);
            settings.seeing_schedule_at = format!("{:02}:{:02}", minutes / 60, minutes % 60);
        }
        changed(app, true);
        fill(app);
    }));
    window.on_look_now(|| with_app(|app| {
        app.settings.borrow_mut().look_new_at = crate::app::now();
        changed(app, true);
        with_setup(|setup| setup.window.set_seeing_schedule_note("Looking at new images now.".into()));
    }));
    window.on_skip_file(|path| with_app(|app| {
        app.indexer.skip(&app.paths, &path);
    }));
    window.on_full_speed_chosen(|on| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            let at = if on { crate::app::now() } else { 0 };
            (settings.read_all_at, settings.look_all_at, settings.listen_all_at) = (at, at, at);
            if on {
                settings.paused_until = 0;
            }
        }
        changed(app, true);
        crate::popup::refresh_tray_menu(app);
        fill(app);
    }));
    window.on_look_all(|on| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            settings.look_all_at = if on { crate::app::now() } else { 0 };
            if on {
                settings.paused_until = 0;
            }
        }
        changed(app, true);
        fill(app);
    }));
    window.on_look_battery_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().look_on_battery = on;
        changed(app, true);
        fill(app);
    }));
    window.on_preview_size_chosen(|index| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            settings.preview = index > 0;
            if index > 0 {
                settings.preview_size = ["small", "medium", "large"][index.clamp(1, 3) as usize - 1].to_string();
                // A size chosen here: the one dragged in the popup goes.
                settings.preview_dragged_width = 0.0;
                settings.preview_dragged_image = 0.0;
            }
        }
        changed(app, false);
        fill(app);
        app.preview_size_changed();
    }));
    window.on_blur_secrets_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().blur_secrets = on;
        changed(app, false);
        fill(app);
    }));
    window.on_blur_sharing_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().blur_when_sharing = on;
        changed(app, false);
        fill(app);
    }));
    window.on_start_with_windows_toggled(|on| with_app(|app| {
        if std::env::var_os("EXCAVATE_HOME").is_none() {
            let _ = winsys::registry::set_start_with_windows(crate::RUN_NAME, "--hidden", on);
        }
        crate::popup::refresh_tray_menu(app);
        fill(app);
    }));
    window.on_theme_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().theme = ["system", "light", "dark"][index.clamp(0, 2) as usize].to_string();
        changed(app, false);
        apply_theme(app);
        crate::popup::theme_changed(app);
        fill(app);
    }));
    window.on_theme_picked(|index| with_app(|app| {
        let name = themes::all(&app.settings.borrow()).into_iter().nth(index as usize).map(|palette| palette.name);
        if let Some(name) = name {
            app.settings.borrow_mut().theme_name = name;
            theme_saved(app);
        }
    }));
    window.on_theme_removed(|index| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            let Some(palette) = themes::all(&settings).into_iter().nth(index as usize).filter(|palette| palette.own) else { return };
            settings.own_themes.retain(|theme| theme.name != palette.name);
            if settings.theme_name == palette.name {
                settings.theme_name = themes::DEFAULT.to_string();
            }
        }
        theme_saved(app);
    }));
    window.on_workshop_opened(|| with_app(|app| {
        // From the theme that's on: one of the person's own is edited, and any other is a place to start.
        let settings = app.settings.borrow().clone();
        let current = themes::current(&settings);
        let hue = settings.own_themes.iter().find(|theme| current.own && theme.name == current.name).map_or_else(|| themes::hue_of(current.accent_dark).round(), |theme| f32::from(theme.hue));
        let name = if current.own { current.name.clone() } else { "My theme".to_string() };
        with_setup(|setup| {
            setup.window.set_workshop_name(name.into());
            setup.window.set_workshop_hue(hue);
            setup.window.set_workshop_surfaces(current.surfaces.index());
            setup.window.set_workshop_open(true);
        });
        workshop_edited(app);
    }));
    window.on_workshop_edited(|| with_app(|app| workshop_edited(app)));
    window.on_workshop_cancelled(|| with_app(|app| theme_saved(app)));
    window.on_workshop_saved(|| with_app(|app| {
        let Some((name, hue, surfaces)) = with_setup(|setup| {
            let window = &setup.window;
            (window.get_workshop_name().trim().to_string(), window.get_workshop_hue(), window.get_workshop_surfaces())
        }) else { return };
        let name = if name.is_empty() { "My theme".to_string() } else { name };
        if themes::is_built_in(&name) {
            with_setup(|setup| setup.window.set_workshop_error(format!("{name} is one of the nine. Choose another name.").into()));
            return;
        }
        {
            let mut settings = app.settings.borrow_mut();
            let theme = config::OwnTheme { name: name.clone(), hue: hue.round() as u16, surfaces: themes::Surfaces::from_index(surfaces).name().to_string() };
            match settings.own_themes.iter_mut().find(|existing| existing.name == name) {
                Some(existing) => *existing = theme,
                None => settings.own_themes.push(theme),
            }
            settings.theme_name = name;
        }
        theme_saved(app);
    }));
    window.on_seeing_chosen(|id| with_app(|app| {
        app.settings.borrow_mut().seeing = id.to_string();
        app.settings.borrow_mut().seeing_note.clear();
        changed(app, false);
        app.seeker.stop();
        fill(app);
    }));
    window.on_seeing_deep_chosen(|index| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            settings.seeing_deep = if index == 1 { "qwen" } else { "siglip" }.to_string();
            settings.seeing = "deep".to_string();
            settings.seeing_note.clear();
        }
        changed(app, false);
        app.seeker.stop();
        fill(app);
    }));
    window.on_seeing_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().seeing_on = on;
        changed(app, false);
        app.seeker.stop();
        app.show_toggles();
        fill(app);
    }));
    window.on_listening_chosen(|id| with_app(|app| {
        app.settings.borrow_mut().listening = id.to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_listening_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().listening_on = on;
        changed(app, true);
        fill(app);
    }));
    window.on_listen_way_chosen(|id, index| with_app(|app| {
        let Some(cards) = with_setup(|setup| setup.window.get_listen_cards()) else { return };
        let Some(card) = cards.iter().find(|card| card.id == id) else { return };
        let Some(way) = card.ways.row_data(index as usize) else { return };
        let key = match way.label.as_str() {
            "CPU" => "cpu",
            label if label.ends_with("CUDA") => "cuda",
            _ => "vulkan",
        };
        app.settings.borrow_mut().listening_devices.insert(id.to_string(), key.to_string());
        changed(app, true);
        fill(app);
    }));
    window.on_reading_way_chosen(|id, index| with_app(|app| {
        let Some(cards) = with_setup(|setup| setup.window.get_cards()) else { return };
        let Some(way) = cards.iter().find(|card| card.id == id).and_then(|card| card.ways.row_data(index as usize)) else { return };
        let key = if way.label == "CPU" { "cpu" } else { "gpu" };
        app.settings.borrow_mut().reading_devices.insert(id.to_string(), key.to_string());
        changed(app, true);
        fill(app);
    }));
    window.on_seeing_way_chosen(|id, index| with_app(|app| {
        let Some(cards) = with_setup(|setup| setup.window.get_seeing_cards()) else { return };
        let Some(way) = cards.iter().find(|card| card.id == id).and_then(|card| card.ways.row_data(index as usize)) else { return };
        let key = if way.label == "CPU" { "cpu" } else { "gpu" };
        app.settings.borrow_mut().seeing_devices.insert(id.to_string(), key.to_string());
        changed(app, true);
        fill(app);
    }));
    window.on_open_languages(|| {
        let _ = winsys::shell::open(handle().unwrap_or(0), "ms-settings:regionlanguage");
    });
    window.on_video_chosen(|id| with_app(|app| {
        app.settings.borrow_mut().video = id.to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_video_text_chosen(|id| with_app(|app| {
        app.settings.borrow_mut().video_text_pace = Some(id.to_string());
        changed(app, true);
        fill(app);
    }));
    window.on_videos_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().videos_on = on;
        changed(app, true);
        fill(app);
    }));
    window.on_video_text_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().video_text = on;
        changed(app, true);
        fill(app);
    }));
    window.on_video_speech_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().video_speech = on;
        changed(app, true);
        fill(app);
    }));
    window.on_relook_videos(|| with_app(|app| {
        app.settings.borrow_mut().relook_videos_at = crate::app::now();
        changed(app, true);
        with_setup(|setup| setup.window.set_relook_videos_note("Your videos will be looked at again, newest first.".into()));
    }));
    window.on_relisten(|| with_app(|app| {
        app.settings.borrow_mut().relisten_at = crate::app::now();
        changed(app, true);
        with_setup(|setup| setup.window.set_relisten_note("Your recordings will be listened to again, newest first.".into()));
    }));
    window.on_listen_all(|on| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            settings.listen_all_at = if on { crate::app::now() } else { 0 };
            if on {
                settings.paused_until = 0;
            }
        }
        changed(app, true);
        fill(app);
    }));
    window.on_listen_schedule_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().listen_schedule = ["arrive", "scheduled", "manual"][index.clamp(0, 2) as usize].to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_listen_every_chosen(|index| with_app(|app| {
        app.settings.borrow_mut().listen_schedule_every = ["day", "week", "month"][index.clamp(0, 2) as usize].to_string();
        changed(app, true);
        fill(app);
    }));
    window.on_listen_at_stepped(|by| with_app(|app| {
        {
            let mut settings = app.settings.borrow_mut();
            let minutes = (settings.listen_schedule_minutes() as i32 + by * 30).rem_euclid(24 * 60);
            settings.listen_schedule_at = format!("{:02}:{:02}", minutes / 60, minutes % 60);
        }
        changed(app, true);
        fill(app);
    }));
    window.on_listen_now(|| with_app(|app| {
        app.settings.borrow_mut().listen_new_at = crate::app::now();
        changed(app, true);
        with_setup(|setup| setup.window.set_listen_schedule_note("Listening to new recordings now.".into()));
    }));
    window.on_listen_battery_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().listen_on_battery = on;
        changed(app, true);
        fill(app);
    }));
    window.on_relook(|| with_app(|app| {
        app.settings.borrow_mut().relook_images_at = crate::app::now();
        changed(app, false);
        with_setup(|setup| setup.window.set_relook_note("Your images will be looked at again, newest first.".into()));
    }));
    window.on_step_shown(|_| {
        cancel_recording();
        with_app(|app| {
            page_shown(app);
            // (The Activity page is filled only while it's open: at once, then.)
            poll(app);
        });
    });
    // "Start reading": the indexer starts, and the window stays to show how it goes.
    window.on_finish(|| with_app(|app| {
        app.settings.borrow_mut().setup_done = true;
        save_folders(app);
        app.settings.borrow().save(&app.paths);
        log(&format!("setup done: reading {:?} in {} folders", app.settings.borrow().reading(), app.settings.borrow().folders.len()));
        app.indexer.start();
        with_setup(|setup| {
            setup.reading_started = true;
            fill_folders(setup);
        });
        page_shown(app);
        poll(app);
        app.update_status();
    }));
    window.on_done(|| with_app(|app| {
        close();
        crate::popup::show(app);
    }));
    window.on_auto_update_toggled(|on| with_app(|app| {
        app.settings.borrow_mut().auto_update = on;
        changed(app, false);
        log(&format!("updates: automatic {}", if on { "on" } else { "off" }));
        // Turned on: looked for now, rather than a day from now.
        if on && !matches!(crate::update::status(), crate::update::Status::Ready(_)) {
            app.settings.borrow_mut().update_checked_at = crate::app::now();
            app.settings.borrow().save(&app.paths);
            crate::update::check(&app.paths);
        }
        fill_about(app);
    }));
    window.on_update_action_clicked(|| with_app(|app| {
        match crate::update::status() {
            crate::update::Status::Ready(_) => {
                log("updates: restarting on the update");
                crate::update::restart();
                crate::popup::quit(app);
            }
            _ => {
                app.settings.borrow_mut().update_checked_at = crate::app::now();
                app.settings.borrow().save(&app.paths);
                crate::update::check(&app.paths);
            }
        }
        fill_about(app);
    }));
    window.on_open_log(|| with_app(|app| {
        let _ = winsys::shell::open(0, &app.paths.log().display().to_string());
    }));
    window.on_open_settings_file(|| with_app(|app| {
        app.settings.borrow().save(&app.paths);
        let _ = winsys::shell::open(0, &app.paths.settings().display().to_string());
    }));
    window.on_open_index_folder(|| with_app(|app| {
        let _ = winsys::shell::open(0, &app.paths.local.display().to_string());
    }));
    window.window().on_close_requested(|| {
        cancel_recording();
        // Closed, not just hidden: the window and its timers are dropped once the event loop is done with it.
        let _ = slint::invoke_from_event_loop(close);
        slint::CloseRequestResponse::HideWindow
    });
}

/// Leaving the shortcut while recording one keeps the old shortcut.
fn cancel_recording() {
    let window = with_setup(|setup| setup.window.get_recording().then(|| setup.window.clone_strong())).flatten();
    if let Some(window) = window {
        window.invoke_cancel_recording();
    }
}

fn close() {
    crate::demo::stop();
    crate::living::detach();
    let setup = SETUP.with(|setup| setup.borrow_mut().take());
    if let Some(setup) = setup {
        setup.timer.stop();
        let _ = setup.window.hide();
    }
}

/// Whether Everything is on this PC. `EXCAVATE_NO_EVERYTHING` shows the "not installed" state on a PC that has it (for
/// screenshots and tests).
fn everything_installed() -> bool {
    std::env::var_os("EXCAVATE_NO_EVERYTHING").is_none() && everything::installed_everything().is_some()
}

/// "Install Everything": the official installer, silently, then started, then waited for until its list is ready, on
/// a thread (with progress on the page). Windows asks for approval once.
fn install_everything(app: &App) {
    let already = with_setup(|setup| setup.installing.replace(true));
    if already != Some(false) {
        return;
    }
    with_setup(|setup| {
        setup.window.set_everything_state("installing".into());
        setup.window.set_everything_detail("Downloading from voidtools.com…".into());
        setup.window.set_everything_progress(0.0);
    });
    let folder = app.paths.local.join("downloads");
    let everything = app.everything.clone();
    let progress = |detail: String, fraction: f32| {
        let _ = slint::invoke_from_event_loop(move || {
            with_setup(|setup| {
                setup.window.set_everything_detail(detail.into());
                setup.window.set_everything_progress(fraction);
            });
        });
    };
    std::thread::spawn(move || {
        let result = crate::everything_setup::install(&folder, &everything, &progress);
        let _ = slint::invoke_from_event_loop(move || with_app(|app| {
            with_setup(|setup| setup.installing.set(false));
            match result {
                Ok(()) => {
                    log("Everything installed");
                    crate::popup::check_everything(app);
                    check_everything(app);
                    count_folders(app, true);
                }
                Err(error) => {
                    log(&format!("Everything's install: {error}"));
                    with_setup(|setup| {
                        setup.window.set_everything_state("failed".into());
                        setup.window.set_everything_detail(error.into());
                    });
                }
            }
        }));
    });
}

const IMAGE_EXTENSIONS: [&str; 10] = ["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif"];

/// What's in a folder (files a reader handles, and images among them), without Everything: walking it.
const AUDIO_EXTENSIONS: [&str; 12] = ["mp3", "m4a", "m4b", "wav", "wma", "flac", "aac", "ogg", "opus", "amr", "aiff", "aif"];
const VIDEO_EXTENSIONS: [&str; 12] = ["mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm", "mpg", "mpeg", "3gp", "mts", "m2ts"];

/// What a folder holds: files a reader handles, images among them, recordings and videos.
type Counted = [u64; 4];

/// Adds one file to `counted`, by its (lowercased) name.
fn count_file(name: &str, readable: &std::collections::HashSet<&str>, counted: &mut Counted) {
    let Some((_, extension)) = name.rsplit_once('.') else { return };
    if readable.contains(extension) {
        counted[0] += 1;
        counted[1] += u64::from(IMAGE_EXTENSIONS.contains(&extension));
    }
    counted[2] += u64::from(AUDIO_EXTENSIONS.contains(&extension));
    counted[3] += u64::from(VIDEO_EXTENSIONS.contains(&extension));
}

/// What's in a folder, by walking it: its subfolders are listed on several threads at once (a folder's listing waits
/// on the disk, so one thread mostly waits).
#[cfg(test)]
fn walk_count(folder: &str, exclusions: &config::Exclusions) -> Counted {
    walk_count_until(folder, exclusions, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_default()
}

/// `walk_count`, given up (`None`) once `enough` is set.
fn walk_count_until(folder: &str, exclusions: &config::Exclusions, enough: &std::sync::atomic::AtomicBool) -> Option<Counted> {
    use std::sync::{Condvar, Mutex};
    let readable = readable_extensions();
    let readable: std::collections::HashSet<&str> = readable.split(';').collect();
    // Folders still to list, and how many threads are listing one (none, and nothing left: done).
    let queue = Mutex::new((vec![folder.trim_end_matches('\\').to_string()], 0usize));
    let ready = Condvar::new();
    let total = Mutex::new([0u64; 4]);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut counted = [0u64; 4];
                loop {
                    let next = {
                        let mut queue = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        loop {
                            if let Some(path) = queue.0.pop() {
                                queue.1 += 1;
                                break Some(path);
                            }
                            if queue.1 == 0 {
                                break None;
                            }
                            queue = ready.wait(queue).unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                    };
                    let Some(path) = next else { break };
                    if enough.load(std::sync::atomic::Ordering::Relaxed) {
                        // Empties the queue without listing: every thread then ends.
                        let mut queue = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        queue.0.clear();
                        queue.1 -= 1;
                        ready.notify_all();
                        continue;
                    }
                    let mut inside = Vec::new();
                    for found in engine::walk::list(&path).unwrap_or_default() {
                        let name = found.name.to_lowercase();
                        if found.meta.flags & engine::store::REPARSE != 0 || exclusions.excludes_name(&name) {
                            continue;
                        }
                        if found.meta.flags & engine::store::DIR != 0 {
                            inside.push(format!("{path}\\{}", found.name));
                        } else {
                            count_file(&name, &readable, &mut counted);
                        }
                    }
                    let mut queue = queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    queue.0.extend(inside);
                    queue.1 -= 1;
                    ready.notify_all();
                }
                let mut total = total.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                for (sum, part) in total.iter_mut().zip(counted) {
                    *sum += part;
                }
            });
        }
    });
    if enough.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    Some(total.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner()))
}

/// What's in a folder, through Everything: four counts (each a pass over all its names, 50 to 120 ms on 9 million;
/// a few seconds for a folder of millions). `None`: Everything has nothing in the folder at all (a drive it leaves out).
fn everything_count(everything: &everything::Everything, folder: &str, left_out: &str) -> Option<Counted> {
    let base = format!("file: \"{}\\\"{left_out}", folder.trim_end_matches('\\'));
    let count = |extensions: &str| -> Option<u64> {
        let query = everything::Query { count: 1, ..everything::Query::listing(format!("{base} ext:{extensions}"), everything::Sort::NameAscending) };
        everything.ask(vec![query]).ok().and_then(|pages| pages.first().map(|page| page.total))
    };
    let counted = [count(&readable_extensions())?, count(&IMAGE_EXTENSIONS.join(";"))?, count(&AUDIO_EXTENSIONS.join(";"))?,
                   count(&VIDEO_EXTENSIONS.join(";"))?];
    if counted.iter().all(|&count| count == 0) {
        // Nothing that counts: is the folder in Everything's list at all?
        let anything = everything::Query { count: 1, ..everything::Query::listing(format!("\"{}\\\"", folder.trim_end_matches('\\')), everything::Sort::NameAscending) };
        if everything.ask(vec![anything]).ok().and_then(|pages| pages.first().map(|page| page.total)).unwrap_or(0) == 0 {
            return None;
        }
    }
    Some(counted)
}

/// Shows a folder's counts (`first`: how many of its images the first sweep takes, when known).
fn counted(folder: String, counted: Counted, first: Option<u64>) {
    let _ = slint::invoke_from_event_loop(move || {
        with_setup(|setup| {
            let key = folder.to_lowercase();
            setup.counts.insert(key.clone(), (counted[0], counted[1]));
            setup.media.insert(key.clone(), (counted[2], counted[3]));
            // A smaller folder's images all go first.
            match first {
                Some(first) => { setup.first_sweep.insert(key, first); }
                None if counted[1] < sweep::LARGE as u64 => { setup.first_sweep.insert(key, counted[1]); }
                None => {}
            }
            fill_folders(setup);
            fill_cards(setup);
        });
    });
}

/// Counts what's in the folders not counted yet (all of them `again`, when what's left out changed), each on its own
/// thread and shown as soon as it's done; the counts shown stay until new ones come. Walking a folder and asking
/// Everything race, and the first to answer counts: walking takes milliseconds for an ordinary folder (1 to 143 ms
/// for up to 6,000 files, against Everything's 240 to 530), Everything seconds for one of millions (4.5 s for 2.1
/// million files, against 81 s walking).
fn count_folders(app: &App, again: bool) {
    let Some(folders) = with_setup(|setup| setup.choices.iter().map(|choice| choice.path.clone())
        .filter(|path| again || !setup.counts.contains_key(&path.to_lowercase())).collect::<Vec<_>>()) else { return };
    if folders.is_empty() {
        return;
    }
    let exclusions = app.settings.borrow().exclusions();
    let (everything, probe) = (app.everything.clone(), app.paths.everything_probe("app"));
    let use_everything = std::env::var_os("EXCAVATE_NO_EVERYTHING").is_none();
    for folder in folders {
        let (everything, probe, exclusions) = (everything.clone(), probe.clone(), exclusions.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let enough = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (answer, answers) = std::sync::mpsc::channel::<(&'static str, Option<Counted>)>();
            {
                let (folder, exclusions, enough, answer) = (folder.clone(), exclusions.clone(), enough.clone(), answer.clone());
                std::thread::spawn(move || {
                    let _ = answer.send(("walking it", walk_count_until(&folder, &exclusions, &enough)));
                });
            }
            if use_everything {
                let (folder, everything, left_out) = (folder.clone(), everything.clone(), exclusions.everything());
                std::thread::spawn(move || {
                    // Only once its list is loaded and kept up to date (what it last said, when it was live: asking
                    // again takes a second or more).
                    let live = everything.last_health() == everything::Health::Live
                        || everything.settled_health(&probe, crate::everything_setup::HEALTH_FRESH, Duration::from_secs(180)) == everything::Health::Live;
                    let _ = answer.send(("Everything", if live { everything_count(&everything, &folder, &left_out) } else { None }));
                });
            } else {
                drop(answer);
            }
            // The first real answer (Everything's "don't know" waits for the walk).
            let Some((how, Some(found))) = answers.iter().find(|(_, found)| found.is_some()) else { return };
            enough.store(true, std::sync::atomic::Ordering::Relaxed);
            log(&format!("counted a folder by {how} in {} ms", started.elapsed().as_millis()));
            // A large folder's first sweep takes a listing of all its images, with what's known of each.
            let large = found[1] >= sweep::LARGE as u64;
            let first = if large && use_everything && everything.last_health() == everything::Health::Live {
                let text = format!("file: \"{}\\\"{} ext:{}", folder.trim_end_matches('\\'), exclusions.everything(), IMAGE_EXTENSIONS.join(";"));
                everything.list(everything::Query::listing(text, everything::Sort::NameAscending)).ok().map(|page| {
                    let files: Vec<(String, u64, i64)> = page.items.into_iter().map(|item| (item.path(), item.size.unwrap_or(0), item.modified.unwrap_or(0))).collect();
                    let images: Vec<sweep::Image> = files.iter().map(|(path, size, modified)| sweep::Image { path, size: *size, modified: *modified }).collect();
                    sweep::first_sweep(&sweep::places(&images)) as u64
                })
            } else {
                None
            };
            counted(folder, found, first.or(large.then_some(found[1])));
        });
    }
}

/// The extensions some reader handles (the extract crate's list, which the app doesn't link).
fn readable_extensions() -> String {
    [
        "txt", "md", "markdown", "log", "csv", "tsv", "ini", "cfg", "conf", "srt", "vtt", "nfo", "rst", "adoc", "tex", "org",
        "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cc", "cpp", "cxx", "hpp", "cs", "java", "kt", "go",
        "rb", "php", "swift", "m", "scala", "lua", "pl", "r", "jl", "dart", "sql", "sh", "ps1", "bat", "cmd", "vue",
        "svelte", "css", "scss", "json", "yaml", "yml", "toml", "xml", "ipynb", "docx", "docm", "xlsx", "xlsm", "pptx",
        "pptm", "odt", "ods", "odp", "epub", "html", "htm", "rtf", "eml", "msg", "pdf", "png", "jpg", "jpeg", "webp",
        "bmp", "tif", "tiff", "gif", "heic", "heif",
    ]
    .join(";")
}

/// Everything started, stopped, or began or stopped keeping up.
pub fn everything_changed(app: &App) {
    check_everything(app);
}

/// A fix of Everything moved on a step (`popup::fix_everything`).
pub fn everything_fixing(app: &App) {
    let Some(step) = app.everything_fixing() else { return };
    with_setup(|setup| {
        setup.window.set_everything_state("fixing".into());
        setup.window.set_everything_detail(step.into());
    });
}

/// A fix of Everything didn't work: offered again, with what went wrong.
pub fn everything_fix_failed(error: &str) {
    with_setup(|setup| {
        setup.window.set_everything_state("behind".into());
        setup.window.set_everything_detail(error.into());
    });
}

fn check_everything(app: &App) {
    // (While fixing, that says how it's going.)
    if app.everything_fixing().is_some() {
        everything_fixing(app);
        return;
    }
    let everything = app.everything.clone();
    let health = app.everything_health();
    std::thread::spawn(move || {
        let status = everything.status();
        let _ = slint::invoke_from_event_loop(move || {
            with_setup(|setup| {
                // (While installing, that says how it's going.)
                if setup.installing.get() {
                    return;
                }
                let pretend_missing = std::env::var_os("EXCAVATE_NO_EVERYTHING").is_some();
                let behind = "Files added or changed since then are missing from name searches. Excavate reads your folders directly meanwhile.";
                let own = crate::own_index::state();
                let (state, detail) = if crate::own_index::building() {
                    ("own-building", "Reading each drive's own list of files. Names are found once it's done.".to_string())
                } else if let Some(own) = own {
                    use engine::names::Phase;
                    let drives = own.volumes.iter().map(|drive| drive.trim_end_matches('\\').to_string()).collect::<Vec<_>>().join(", ");
                    match own.phase {
                        Phase::Starting | Phase::Building => ("own-building", format!("Listing {drives} for the first time. Names are found once it's done.")),
                        Phase::Failed => ("own-failed", own.error.clone()),
                        Phase::CatchingUp => ("own", format!("{} names on {drives}. Catching up with what changed while Excavate was closed.", group(own.entries as u64))),
                        Phase::Live => ("own", format!("{} names on {drives}, kept up to date. Everything isn't used while this is on.", group(own.entries as u64))),
                    }
                } else if pretend_missing {
                    ("missing", "Without it, Excavate searches your own folders only.".to_string())
                } else if status.available && health == everything::Health::Loading {
                    ("loading", "Names may be missing until it's done, which can take a few minutes.".to_string())
                } else if let (true, everything::Health::Behind(fix)) = (status.available, health) {
                    setup.window.set_everything_fix(match fix {
                        everything::Fix::StartService => "Start its service",
                        everything::Fix::InstallService => "Install its service",
                        everything::Fix::Restart => "Start it again",
                    }.into());
                    ("behind", match fix {
                        everything::Fix::StartService => format!("Its service is stopped, so it can't watch your drives. {behind}"),
                        everything::Fix::InstallService => format!("It has no service and doesn't run as administrator, so it can't watch your drives. {behind}"),
                        everything::Fix::Restart => format!("It isn't noticing changes on your drives. {behind}"),
                    })
                } else if status.available {
                    ("running", if status.version.is_empty() { String::new() } else { format!("Version {}", status.version) })
                } else if status.installed.is_some() {
                    ("installed", "Excavate searches your own folders until it runs.".to_string())
                } else {
                    ("missing", "Without it, Excavate searches your own folders only.".to_string())
                };
                setup.window.set_everything_state(state.into());
                setup.window.set_everything_detail(detail.into());
            });
        });
    });
}

/// Follows the indexer (status.json, once a second while the window is open).
/// Qwen's 8B was tried on this PC's first images and didn't suit it (`looking::lane`): Thorough goes back to SigLIP 2
/// beside EmbeddingGemma 2, and the Images page says why until the choice is changed again.
pub fn check_trial(app: &App, status: &Status) {
    let Some(refused) = status.looking.iter().find(|looking| looking.engine == "qwen3vl-8b" && looking.model.state == "unsuitable") else { return };
    {
        let mut settings = app.settings.borrow_mut();
        if settings.seeing != "deep" || settings.seeing_deep != "qwen" {
            return;
        }
        settings.seeing_deep = "siglip".into();
        settings.seeing_note = format!("Qwen3-VL-Embedding-8B was tried on your first images and didn't suit this PC: {} Thorough uses SigLIP 2 instead.", refused.model.error);
        settings.save(&app.paths);
    }
    log(&format!("Qwen's 8B refused: {}", refused.model.error));
    app.indexer.settings_changed();
    app.seeker.stop();
    with_setup(|setup| {
        setup.window.set_deep_choice(0);
        fill_seeing(setup);
    });
}

fn poll(app: &App) {
    let status = Status::load(&app.paths);
    if let Some(status) = &status {
        check_trial(app, status);
    }
    let note = app.settings.borrow().seeing_note.clone();
    let setup_done = app.settings.borrow().setup_done;
    let index_size = folder_size(&app.paths.index());
    with_setup(|setup| {
        setup.status = status.clone();
        let progress = status.as_ref().map(seeing_progress).unwrap_or_default();
        setup.window.set_seeing_progress(if note.is_empty() { progress } else { format!("{note} {progress}") }.trim().to_string().into());
        let line = match &status {
            _ if setup.onboarding => String::new(),
            None => if setup_done { "The indexer hasn't started yet.".into() } else { String::new() },
            Some(status) => describe(status),
        };
        setup.window.set_status(line.into());
        if let Some(status) = &status {
            let measured = status.picture_ms.map(|ms| format!(
                "An image takes about {:.2} s on this PC with {}.", ms / 1000.0, if status.engine.is_empty() { "the chosen reader" } else { &status.engine }
            ));
            if let Some(measured) = measured {
                if !setup.window.get_measured().starts_with("Your images") {
                    setup.window.set_measured(measured.into());
                }
            }
            setup.window.set_index_summary(format!(
                "{} searchable by their contents · {} on disk",
                plural(status.documents, "file", "files"),
                crate::format::size(index_size)
            ).into());
        }
        fill_folders(setup);
        fill_schedule_note(setup, &app.settings.borrow());
        fill_progress(setup);
        fill_activity(setup);
        fill_read_all(setup, &app.settings.borrow());
        fill_look_all(setup, &app.settings.borrow());
        fill_listen_all(setup, &app.settings.borrow());
        fill_full_speed(setup, &app.settings.borrow());
        setup.window.set_listen_progress(setup.status.as_ref().map(listen_progress).unwrap_or_default().into());
        setup.window.set_video_progress(setup.status.as_ref().map(video_progress).unwrap_or_default().into());
        fill_pause(&setup.window, &app.settings.borrow());
        fill_names_missing(setup, &app.settings.borrow());
    });
    crate::living::reading(status.as_ref().is_some_and(|status| matches!(status.state.as_str(), "listing" | "reading")));
}

/// Whether what shows how far along reading is, is on screen: the last onboarding step, or the Activity page.
fn progress_shown(setup: &Setup) -> bool {
    setup.reading_started && (setup.onboarding || setup.window.get_page() == ACTIVITY)
}

/// How long each read has left (ms), at the pace it's really going once that's known: the first read's, and Deep's.
fn times_left(status: &Status, progress: &Progress, pace: &VecDeque<(Instant, u64, u64)>) -> (f64, f64) {
    let moved = |count: fn(&(Instant, u64, u64)) -> u64| -> Option<f64> {
        let (first, last) = (pace.front()?, pace.back()?);
        let seconds = last.0.duration_since(first.0).as_secs_f64();
        (seconds > 10.0 && count(last) > count(first)).then(|| (count(last) - count(first)) as f64 / seconds)
    };
    let first_rate = moved(|entry| entry.1).or_else(|| status.picture_ms.map(|ms| 1000.0 / ms.max(1.0))).unwrap_or(1000.0 / picture_ms(Some(status), Reading::Balanced));
    let deep_rate = moved(|entry| entry.2).or_else(|| status.deep_pace_ms.map(|ms| 1000.0 / ms.max(1.0))).unwrap_or(1000.0 / DEEP_MS);
    (progress.first_left as f64 / first_rate * 1000.0, progress.deep_left as f64 / deep_rate * 1000.0)
}

/// "About 45 min left". They run side by side, so the slower read decides.
fn left_text(ms: f64) -> String {
    let mut text = duration(ms);
    text[..1].make_ascii_uppercase();
    format!("{text} left")
}

/// The last onboarding step and the Activity page: how far along, the time left, what's being downloaded, what's being read.
fn fill_progress(setup: &mut Setup) {
    if !progress_shown(setup) {
        return;
    }
    let window = setup.window.clone_strong();
    let Some(status) = setup.status.clone() else {
        window.set_reading_summary("Starting…".into());
        window.set_reading_progress(-1.0);
        return;
    };
    let progress = progress(&status);
    window.set_reading_summary(progress.summary.as_str().into());
    window.set_reading_progress(progress.fraction);
    // How fast it's going, over the last two minutes.
    setup.pace.push_back((Instant::now(), status.read + status.failed, status.deep_done));
    while setup.pace.len() > 120 {
        setup.pace.pop_front();
    }
    let (first, deep) = times_left(&status, &progress, &setup.pace);
    let all = stages(&status, first, deep);
    window.set_reading_images(all.iter().find(|stage| stage.split).cloned().unwrap_or_default());
    window.set_reading_videos(all.into_iter().find(|stage| stage.name == "Videos").unwrap_or_default());
    window.set_reading_left(if matches!(status.state.as_str(), "reading") && first + deep > 0.0 { left_text(first.max(deep)).into() } else { SharedString::new() });
    let model = &status.model;
    if model.state == "downloading" && model.total > 0 {
        window.set_reading_model(format!("Getting {}: {} of {}", model.name, crate::format::size(model.received), crate::format::size(model.total)).into());
        window.set_reading_model_progress(model.received as f32 / model.total as f32);
    } else if model.state == "failed" {
        window.set_reading_model(format!("Couldn't get the image reader ({}). Trying again in a while.", model.error).into());
        window.set_reading_model_progress(0.0);
    } else {
        window.set_reading_model(SharedString::new());
    }
    window.set_reading_current(if status.current.is_empty() {
        SharedString::new()
    } else {
        format!("Reading {}", file_name(&status.current)).into()
    });
}

fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

/// Settings' pages filled again (from elsewhere: a background job ended).
pub fn refill(app: &App) {
    fill(app);
}

/// Activity's Bedrock card: a drive listed whole again, now or last, and "Fix this" when its record of
/// changes was too small for the time Excavate was closed.
fn fill_bedrock(setup: &Setup) {
    let window = &setup.window;
    // A sandbox shows the card as it looks a third of the way through C: (`EXCAVATE_PRETEND_RELIST`).
    let pretend = std::env::var_os("EXCAVATE_PRETEND_RELIST").is_some();
    let state = crate::own_index::state();
    enum Shown {
        Now { drive: String, done: usize, total: usize, overwritten: bool },
        Done { drive: String, took: Duration, overwritten: bool },
    }
    let shown = if pretend {
        Some(Shown::Now { drive: r"C:\".into(), done: 3_100_000, total: 9_100_000, overwritten: true })
    } else {
        state.as_ref().and_then(|state| match (&state.relisting, &state.relisted) {
            (Some(now), _) => Some(Shown::Now { drive: now.drive.clone(), done: engine::walk::listed().saturating_sub(now.from), total: now.total, overwritten: now.overwritten }),
            (None, Some(last)) => Some(Shown::Done { drive: last.drive.clone(), took: last.took, overwritten: last.overwritten }),
            _ => None,
        })
    };
    let Some(shown) = shown else {
        window.set_bedrock_shown(false);
        return;
    };
    let (drive, overwritten) = match &shown {
        Shown::Now { drive, overwritten, .. } | Shown::Done { drive, overwritten, .. } => (drive.clone(), *overwritten),
    };
    let letter = drive.trim_end_matches('\\').to_string();
    let size = engine::journal::size(&drive).unwrap_or(0);
    let fixed = crate::own_index::journal_fixed(&drive);
    let fixable = overwritten && !fixed && size < crate::own_index::JOURNAL_SIZE / 2;
    let why = if overwritten {
        format!("Windows' record of changes on {letter} ran out while Excavate was closed")
    } else {
        format!("Windows couldn't say what changed on {letter} while Excavate was closed")
    };
    window.set_bedrock_shown(true);
    window.set_bedrock_fix(fixable);
    window.set_journal_ask_line(format!(
        "Windows keeps a record of what changes on each drive, so Bedrock catches up in a moment after Excavate was closed. On {letter} it holds about {} MB, and it ran out.",
        (size >> 20).max(1)
    ).into());
    match shown {
        Shown::Now { done, total, .. } => {
            let done = done.min(total);
            window.set_bedrock_title(format!("Bedrock is catching up on {letter}").into());
            window.set_bedrock_line(format!("{why}, so {letter} is listed again. Search works meanwhile.").into());
            window.set_bedrock_progress(if total > 0 { done as f32 / total as f32 } else { -1.0 });
            let count = if total >= 1_000_000 {
                format!("{:.1} of {:.1} million files", done as f64 / 1e6, total as f64 / 1e6)
            } else {
                format!("{done} of {}", plural(total as u64, "file", "files"))
            };
            window.set_bedrock_count(format!("{count} · going gently").into());
        }
        Shown::Done { took, .. } => {
            let minutes = took.as_secs_f64() / 60.0;
            let took = if minutes >= 1.5 { format!("about {} minutes", minutes.round()) } else { format!("{} seconds", took.as_secs()) };
            window.set_bedrock_title(format!("Bedrock caught up on {letter}").into());
            window.set_bedrock_line(if fixed {
                format!("It took {took}. Windows now keeps about 1 GB of changes on {letter}, so this shouldn't happen again.")
            } else {
                format!("It took {took}, because {why}.")
            }.into());
            window.set_bedrock_progress(-1.0);
            window.set_bedrock_count("".into());
        }
    }
}

/// The Activity page, only while it's open.
fn fill_activity(setup: &mut Setup) {
    if setup.onboarding || setup.window.get_page() != ACTIVITY {
        return;
    }
    fill_bedrock(setup);
    let Some(status) = setup.status.clone() else {
        setup.stages.set_vec(Vec::new());
        return;
    };
    let progress = progress(&status);
    let (first, deep) = times_left(&status, &progress, &setup.pace);
    update_rows(&setup.stages, stages(&status, first, deep));
    update_rows(&setup.now, now_rows(&status));
    let more = if status.failed > status.errors.len() as u64 { format!(", the latest {} here", status.errors.len()) } else { String::new() };
    setup.window.set_problems_title(format!("Couldn't be read: {}{more}", plural(status.failed.max(status.errors.len() as u64), "file", "files")).into());
    update_rows(&setup.problems, status.errors.iter().map(|(path, why)| ProblemRow {
        name: file_name(path).into(),
        path: path.as_str().into(),
        why: plain_why(why).into(),
    }).collect());
}

/// Why a file couldn't be read, in plain words where the reader's message isn't.
fn plain_why(why: &str) -> &str {
    if why.contains("PasswordError") {
        "It's protected by a password"
    } else if why.contains("FormatError") {
        "It's damaged, or it isn't really a PDF"
    } else if why.contains("component cannot be found") {
        "It's damaged, or Windows can't open this kind of image"
    } else {
        why
    }
}

/// A stage's state, for the page.
const STILL_TO_COME: i32 = 0;
const GOING: i32 = 1;
const WAITING: i32 = 2;
const DONE: i32 = 3;

/// The state a lane's note from the indexer means: it waits ("Waiting for a quiet moment: …", "Paused on battery
/// power"), or it goes on at a slower pace ("Going slower: your processor is busy with Python") or not at all.
fn note_state(note: &str) -> i32 {
    if note.is_empty() || note.starts_with("Going slower") { GOING } else { WAITING }
}

/// "0.15 s an image · About 2 min left".
fn pace_note(ms: Option<f64>, left_ms: f64) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = ms {
        parts.push(format!("{:.2} s an image", ms / 1000.0));
    }
    if left_ms > 0.0 {
        parts.push(left_text(left_ms));
    }
    parts.join(" · ")
}

/// How looking at what images show is going, by the model furthest behind.
fn shown_part(status: &Status, listing: bool) -> StagePart {
    let Some(looking) = status.looking.iter().max_by_key(|looking| looking.queued) else { return StagePart::default() };
    let name = crate::format::model_name(&looking.engine);
    let model = &looking.model;
    let total = looking.done + looking.queued;
    let (state, note) = if listing {
        (STILL_TO_COME, String::new())
    } else if model.state == "downloading" && model.total > 0 {
        (GOING, format!("Getting {name}: {} of {}", crate::format::size(model.received), crate::format::size(model.total)))
    } else if model.state == "failed" {
        (WAITING, format!("{name} couldn't be downloaded, trying again soon"))
    } else if status.seeing_wait && looking.queued > 0 {
        (WAITING, if status.seeing_at > 0 { "Waiting for the next scheduled time".to_string() } else { "Waiting until you start it, under Images".to_string() })
    } else if total == 0 {
        (STILL_TO_COME, "Once their text has been read".to_string())
    } else if looking.queued == 0 {
        (DONE, String::new())
    } else if !looking.waiting.is_empty() {
        (note_state(&looking.waiting), looking.waiting.clone())
    } else {
        (GOING, looking.ms.map(|ms| format!("{:.2} s an image", ms / 1000.0)).unwrap_or_default())
    };
    StagePart {
        state,
        count: format!("{} of {}", group(looking.done), plural(total, "image", "images")).into(),
        note: note.into(),
        progress: if state == GOING || state == WAITING { if total > 0 { looking.done as f32 / total as f32 } else { -1.0 } } else { -1.0 },
    }
}

/// How looking at what videos show is going, by the model furthest behind; none without videos to look at.
fn shown_in_videos(status: &Status, listing: bool) -> Option<StagePart> {
    let looking = status.looking.iter().filter(|looking| looking.videos_done + looking.videos_queued > 0)
        .max_by_key(|looking| looking.videos_queued)?;
    let all = looking.videos_done + looking.videos_queued;
    let (state, note) = if listing {
        (STILL_TO_COME, String::new())
    } else if looking.videos_queued == 0 {
        (DONE, String::new())
    } else if status.seeing_wait {
        (WAITING, if status.seeing_at > 0 { "Waiting for the next scheduled time".to_string() } else { "Waiting until you start it, under Videos".to_string() })
    } else if looking.queued > 0 {
        (STILL_TO_COME, "After the images".to_string())
    } else if !looking.waiting.is_empty() {
        (note_state(&looking.waiting), looking.waiting.clone())
    } else {
        (GOING, "Looking at what they show".to_string())
    };
    Some(StagePart {
        state,
        count: format!("{} of {}", group(looking.videos_done), plural(all, "video", "videos")).into(),
        note: note.into(),
        progress: if state == GOING || state == WAITING { looking.videos_done as f32 / all as f32 } else { -1.0 },
    })
}

/// How reading the text videos show is going (after the images, in their lane); none when it's off or there are none.
fn text_in_videos(status: &Status, listing: bool) -> Option<StagePart> {
    let reading = status.video_text.as_ref()?;
    let all = reading.done + reading.queued;
    if all == 0 && !listing {
        return None;
    }
    let (state, note) = if listing {
        (STILL_TO_COME, String::new())
    } else if reading.queued == 0 {
        (DONE, String::new())
    } else if status.pictures_wait {
        (WAITING, if status.pictures_at > 0 { "Waiting for the next scheduled time".to_string() } else { "Waiting until you start it, under Images".to_string() })
    } else if status.queued_pictures > 0 {
        (STILL_TO_COME, "After the images".to_string())
    } else if !status.waits[1].is_empty() {
        (note_state(&status.waits[1]), status.waits[1].clone())
    } else {
        (GOING, "Reading the text they show".to_string())
    };
    Some(StagePart {
        state,
        count: format!("{} of {}", group(reading.done), plural(all, "video", "videos")).into(),
        note: note.into(),
        progress: if (state == GOING || state == WAITING) && all > 0 { reading.done as f32 / all as f32 } else { -1.0 },
    })
}

/// How hearing what's said in videos is going (after the recordings, by the same listener); none when it's off or
/// there are none.
fn said_in_videos(status: &Status, listing: bool) -> Option<StagePart> {
    let listening = status.listening.as_ref().filter(|listening| listening.videos)?;
    let all = listening.videos_done + listening.videos_queued;
    if all == 0 && !listing {
        return None;
    }
    let (state, note) = if listing {
        (STILL_TO_COME, String::new())
    } else if listening.videos_queued == 0 {
        (DONE, String::new())
    } else if status.listen_wait {
        (WAITING, if status.listen_at > 0 { "Waiting for the next scheduled time".to_string() } else { "Waiting until you start it, under Audio".to_string() })
    } else if listening.model.state == "downloading" || listening.model.state == "failed" || (listening.recordings && listening.queued > 0) {
        (STILL_TO_COME, if listening.recordings { "After the recordings" } else { "Once the listener is ready" }.to_string())
    } else if !listening.waiting.is_empty() {
        (note_state(&listening.waiting), listening.waiting.clone())
    } else {
        (GOING, "Listening to what's said".to_string())
    };
    Some(StagePart {
        state,
        count: format!("{} of {}", group(listening.videos_done), plural(all, "video", "videos")).into(),
        note: note.into(),
        progress: if (state == GOING || state == WAITING) && all > 0 { listening.videos_done as f32 / all as f32 } else { -1.0 },
    })
}

/// The Videos card: the text they show, what they show and what's said in them, as parts of one card when more than
/// one is on; none without videos.
fn video_stage(status: &Status, listing: bool) -> Option<StageRow> {
    let parts: Vec<(&str, StagePart)> = [
        ("Text in videos", text_in_videos(status, listing)),
        ("What they show", shown_in_videos(status, listing)),
        ("What's said", said_in_videos(status, listing)),
    ].into_iter().filter_map(|(name, part)| part.map(|part| (name, part))).collect();
    let (first_name, first) = parts.first()?.clone();
    let mut row = StageRow {
        name: "Videos".into(),
        state: first.state,
        count: first.count.clone(),
        note: first.note.clone(),
        progress: first.progress,
        ..StageRow::default()
    };
    if let Some((name, second)) = parts.get(1) {
        row.split = true;
        row.part = first_name.into();
        row.shown_part = (*name).into();
        row.shown = second.clone();
    }
    if let Some((name, third)) = parts.get(2) {
        row.third = true;
        row.third_part = (*name).into();
        row.extra = third.clone();
    }
    Some(row)
}

/// What reading is made of, each with how it's going. `first` and `deep`: the time each read has left (ms).
fn stages(status: &Status, first: f64, deep: f64) -> Vec<StageRow> {
    let listing = status.state == "listing";
    let later = status.queued_later.min(status.queued_pictures);
    let waiting = if status.pictures_wait { status.queued_pictures } else { later };
    let images_left = status.queued_pictures - waiting;
    let share = |done: u64, all: u64| if all > 0 { done.min(all) as f32 / all as f32 } else { -1.0 };
    let mut stages = vec![StageRow {
        name: "Finding files".into(),
        state: if listing { GOING } else { DONE },
        count: if listing { SharedString::new() } else { plural(status.files, "file", "files").into() },
        note: if listing { "Looking through your folders".into() } else { SharedString::new() },
        progress: -1.0,
        ..StageRow::default()
    }];
    let documents = status.files.saturating_sub(status.images);
    let documents_done = (status.read + status.failed).saturating_sub(status.images_read);
    stages.push(StageRow {
        name: "Documents and code".into(),
        state: if listing { STILL_TO_COME } else if status.queued_text == 0 { DONE } else if !status.waits[0].is_empty() { note_state(&status.waits[0]) } else { GOING },
        // (Done is all of them: what's read is counted by subtracting images, which are counted at other moments.)
        count: format!("{} of {}", group(if status.queued_text == 0 && !listing { documents } else { documents_done.min(documents) }), plural(documents, "file", "files")).into(),
        note: status.waits[0].as_str().into(),
        progress: if status.queued_text > 0 { share(documents_done, documents) } else { -1.0 },
        ..StageRow::default()
    });
    let model = &status.model;
    let getting = |name: &str| (model.state == "downloading" && model.name == name && model.total > 0).then(|| format!("Getting {name}: {} of {}", crate::format::size(model.received), crate::format::size(model.total)));
    if !status.engine.is_empty() {
        let images = status.images.saturating_sub(waiting);
        let scheduled = status.pictures_wait && status.queued_pictures > 0;
        let (state, note) = if listing {
            (STILL_TO_COME, String::new())
        } else if scheduled {
            (WAITING, if status.pictures_at > 0 { "Waiting for the next scheduled time".to_string() } else { "Waiting until you start it, under Images".to_string() })
        } else if let Some(note) = getting("the image reader") {
            (GOING, note)
        } else if images_left == 0 {
            (DONE, String::new())
        } else if !status.waits[1].is_empty() {
            (note_state(&status.waits[1]), status.waits[1].clone())
        } else {
            (GOING, pace_note(status.picture_ms, first))
        };
        let mut row = StageRow {
            name: if status.deep { "First read" } else { "Images" }.into(),
            state,
            count: format!("{} of {}", group(status.images_read.min(images)), plural(images, "image", "images")).into(),
            note: note.into(),
            progress: if state == GOING || state == WAITING { share(status.images_read, images) } else { -1.0 },
            ..StageRow::default()
        };
        // Images by what they show too: one card, its two halves.
        if !status.looking.is_empty() {
            row.split = true;
            row.part = if status.deep { "Text in images, first read" } else { "Text in images" }.into();
            row.name = "Images".into();
            row.shown = shown_part(status, listing);
        }
        stages.push(row);
    }
    if later > 0 && !status.pictures_wait {
        let reached = images_left == 0;
        stages.push(StageRow {
            name: "What the first sweep set aside".into(),
            state: if !reached { STILL_TO_COME } else if !status.waits[1].is_empty() { note_state(&status.waits[1]) } else { GOING },
            count: plural(later, "image", "images").into(),
            note: if reached && !status.waits[1].is_empty() { status.waits[1].as_str().into() } else { "Read after the first sweep, when your PC is free".into() },
            progress: -1.0,
            ..StageRow::default()
        });
    }
    if status.deep {
        let all = status.deep_done + status.deep_queued;
        let scheduled = status.pictures_wait && status.deep_queued > 0;
        let (state, note) = if let Some(note) = getting("the thorough reader") {
            (GOING, note)
        } else if model.state == "failed" && model.name == "the thorough reader" {
            (WAITING, format!("Couldn't start the thorough reader ({}). Trying again in a while.", model.error))
        } else if scheduled {
            (WAITING, "Waiting for the next scheduled time".to_string())
        } else if !status.waits[2].is_empty() {
            (note_state(&status.waits[2]), status.waits[2].clone())
        } else if status.deep_queued > 0 {
            (GOING, pace_note(status.deep_pace_ms, deep))
        } else if listing || images_left > 0 {
            (STILL_TO_COME, "Reads again those with text, on the graphics card".to_string())
        } else {
            (DONE, String::new())
        };
        stages.push(StageRow {
            name: "Thorough read".into(),
            state,
            // (While the first read goes on, it finds Deep more to read.)
            count: format!("{} of {}{}", group(status.deep_done), plural(all, "image", "images"), if images_left > 0 { " so far" } else { "" }).into(),
            note: note.into(),
            progress: if state == GOING || state == WAITING { share(status.deep_done, all) } else { -1.0 },
            ..StageRow::default()
        });
    }
    // Videos, by what they show: their own card (they're looked at after the images).
    if let Some(video) = video_stage(status, listing) {
        stages.push(video);
    }
    // Recordings, by what's said in them.
    if let Some(listening) = status.listening.as_ref().filter(|listening| listening.recordings) {
        let all = listening.done + listening.queued;
        let model = &listening.model;
        let (state, note) = if listing {
            (STILL_TO_COME, String::new())
        } else if model.state == "downloading" && model.total > 0 {
            (GOING, format!("Getting its model: {} of {}", crate::format::size(model.received), crate::format::size(model.total)))
        } else if model.state == "failed" {
            (WAITING, format!("Couldn't start ({}). Trying again in a while.", model.error))
        } else if status.listen_wait && listening.queued > 0 {
            (WAITING, if status.listen_at > 0 { "Waiting for the next scheduled time".to_string() } else { "Waiting until you start it, under Audio".to_string() })
        } else if !listening.waiting.is_empty() && listening.queued > 0 {
            (note_state(&listening.waiting), listening.waiting.clone())
        } else if listening.queued > 0 {
            (GOING, listening.ms_a_minute.map(|ms| format!("{:.1} s a minute of speech", ms / 1000.0)).unwrap_or_default())
        } else {
            (DONE, String::new())
        };
        stages.push(StageRow {
            name: "Recordings".into(),
            state,
            count: format!("{} of {}", group(listening.done), plural(all, "recording", "recordings")).into(),
            note: note.into(),
            progress: if state == GOING || state == WAITING { share(listening.done, all) } else { -1.0 },
            ..StageRow::default()
        });
    }
    stages
}

/// What's being read this moment, and with what.
fn now_rows(status: &Status) -> Vec<NowRow> {
    if !matches!(status.state.as_str(), "reading" | "paused") {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let later = status.queued_later.min(status.queued_pictures);
    let waiting = if status.pictures_wait { status.queued_pictures } else { later };
    let memory = status.memory_mb;
    // (While its model is still downloading, it isn't reading yet.)
    let getting = status.model.state == "downloading";
    if status.queued_pictures > waiting && !status.engine.is_empty() && note_state(&status.waits[1]) != WAITING && !getting {
        let what = if status.deep { "First read" } else { "Reading images" };
        let device = if status.reading_gpu { "Graphics card" } else { "Processor" };
        rows.push(now_row(format!("{what} · {} · {device} · {memory} MB", status.engine), &status.current, false));
    } else if status.queued_text > 0 && note_state(&status.waits[0]) != WAITING {
        rows.push(now_row(format!("Reading documents and code · Processor · {memory} MB"), &status.current, false));
    }
    if status.deep_running && note_state(&status.waits[2]) != WAITING {
        rows.push(now_row("Thorough read · PaddleOCR-VL-1.5 · Graphics card · about 1.9 GB of its memory".into(), &status.deep_current, false));
    }
    // The text videos show (after the images, in their lane).
    if let Some(reading) = status.video_text.as_ref().filter(|reading| !reading.current.is_empty()) {
        rows.push(now_row(format!("Reading the text a video shows · {}", if status.engine.is_empty() { "PP-OCRv6" } else { status.engine.as_str() }), &reading.current, true));
    }
    // What images and videos show, and what documents mean: each model at work.
    for looking in status.looking.iter().filter(|looking| looking.running && !looking.current.is_empty()) {
        let video = is_video(&looking.current);
        let what = if video { "Looking at what a video shows" } else if is_image(&looking.current) { "Looking at what an image shows" } else { "Looking at what a document means" };
        rows.push(now_row(format!("{what} · {}", crate::format::model_name(&looking.engine)), &looking.current, video));
    }
    // What's said in recordings and videos.
    if let Some(listening) = status.listening.as_ref().filter(|listening| listening.running && !listening.current.is_empty()) {
        let device = if listening.device.is_empty() { String::new() } else { format!(" · {}", listening.device) };
        rows.push(now_row(format!("Listening to what's said · {}{device}", ear_name(&listening.engine)), &listening.current, true));
    }
    rows
}

/// A "Right now" row: what's at work, on which file (its name shown, its path kept for "Skip").
fn now_row(what: String, path: &str, skip: bool) -> NowRow {
    NowRow { what: what.into(), file: file_name(path).into(), path: path.into(), skip: skip && !path.is_empty() }
}

fn is_video(path: &str) -> bool {
    search::filter::extension(path).is_some_and(|extension| search::filter::VIDEO.contains(&extension.to_ascii_lowercase().as_str()))
}

fn is_image(path: &str) -> bool {
    search::filter::extension(path).is_some_and(|extension| ["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif"]
        .contains(&extension.to_ascii_lowercase().as_str()))
}

/// The listener's model, by the name the status gives it.
fn ear_name(engine: &str) -> &str {
    match engine {
        "whisper-turbo" => "Whisper large-v3-turbo",
        "whisper-large" => "Whisper large-v3",
        "parakeet" => "Parakeet",
        other => other,
    }
}

/// How much longer an image takes Deep than a file takes the first read, for the bar. Measured with 835 files: 5.6 min for
/// all of them, then 3.3 s an image.
const DEEP_WEIGHT: f64 = 8.0;
/// And an image takes a model that looks at what it shows: 0.06 to 0.16 s against the first read's 0.4 s a file.
const LOOK_WEIGHT: f64 = 0.3;
const VIDEO_FRAMES: u64 = 40;
/// Reading the text a video shows: its frames through the detector and reader (measured 0.5 to 6 min a lecture),
/// against the first read's 0.4 s a file.
const VIDEO_TEXT_WEIGHT: f64 = 100.0;
/// A document looked at for what it means, against a file read: a few pieces of text through EmbeddingGemma 2.
const MEANING_WEIGHT: f64 = 0.2;

/// How far along the indexer is, however it's said.
pub struct Progress {
    /// "Reading 1,234 of 5,678 files", "Looking through your folders…", "Up to date · 5,678 files".
    pub summary: String,
    /// All of reading's work, 0 to 1, or below 0 when there's no measure yet.
    pub fraction: f32,
    /// What each read still has to do now: files, and images (with those the first read hasn't reached, which may have
    /// text too).
    pub first_left: u64,
    pub deep_left: u64,
    /// Whether it's working (or paused, with work left).
    pub active: bool,
}

pub fn progress_of(status: &Status) -> Progress {
    // Images waiting for their schedule aren't work in progress.
    // Nor are those the first sweep set aside, which come when it's over.
    let later = status.queued_later.min(status.queued_pictures);
    let waiting = if status.pictures_wait { status.queued_pictures } else { later };
    let scheduled = if status.pictures_wait { status.queued_pictures } else { 0 };
    let first_pass_left = status.queued_text + status.queued_pictures - waiting;
    let done = status.read + status.failed;
    // Everything listed, without what waits: steadier than what's done plus what's queued, which are counted at
    // different moments.
    let total = status.files.saturating_sub(waiting).max(done + first_pass_left);
    // One bar for all of it, the first read and Deep's weighted by how long each takes, and what the first sweep set aside
    // included from the start: it never has to go back. Deep's own count comes from the catalog, so a restart of reading
    // doesn't lose it. With everything the first read has yet to reach counted as Deep's work, finding an image with no
    // text only ever moves it forward.
    let all = status.files.saturating_sub(scheduled).max(done + first_pass_left + waiting - scheduled);
    let deep_ahead = if status.deep { status.queued_pictures - scheduled } else { 0 };
    let (deep_done, deep_queued) = if status.deep { (status.deep_done, status.deep_queued) } else { (0, 0) };
    // Each model that looks at images, likewise: its count from the catalog.
    // (A video weighs as much as a few dozen images: its frames.)
    let (look_done, look_queued) = status.looking.iter().fold((0, 0), |(done, queued), looking| {
        (done + looking.done + VIDEO_FRAMES * looking.videos_done, queued + looking.queued + VIDEO_FRAMES * looking.videos_queued)
    });
    // The text in videos, and documents by what they mean: they come last, and the bar isn't full until they're done.
    let (text_done, text_queued) = status.video_text.as_ref().map_or((0, 0), |reading| (reading.done, reading.queued));
    let (meaning_done, meaning_queued) = status.looking.iter().fold((0, 0), |(done, queued), looking| (done + looking.documents_done, queued + looking.documents_queued));
    let work = all as f64 + DEEP_WEIGHT * (deep_done + deep_queued + deep_ahead) as f64 + LOOK_WEIGHT * (look_done + look_queued) as f64
        + VIDEO_TEXT_WEIGHT * (text_done + text_queued) as f64 + MEANING_WEIGHT * (meaning_done + meaning_queued) as f64;
    let fraction = if work == 0.0 { -1.0 } else {
        ((done as f64 + DEEP_WEIGHT * deep_done as f64 + LOOK_WEIGHT * look_done as f64 + VIDEO_TEXT_WEIGHT * text_done as f64
            + MEANING_WEIGHT * meaning_done as f64) / work) as f32
    };
    let sweeping = later > 0 && !status.pictures_wait;
    let reading = || {
        let images_left = status.queued_pictures - waiting;
        let text = if status.deep && images_left > 0 {
            let images = status.images.saturating_sub(waiting).max(status.images_read.min(status.images));
            format!("First read: {} of {}", group(status.images_read.min(images)), plural(images, "image", "images"))
        } else {
            format!("Reading {} of {}", group(done), plural(total, "file", "files"))
        };
        if sweeping { format!("{text} in the first sweep") } else { text }
    };
    let working = |summary: String| Progress { summary, fraction, first_left: first_pass_left + later - scheduled.min(later), deep_left: deep_queued + deep_ahead, active: true };
    let model = &status.model;
    let looking_model = status.looking.iter().map(|looking| &looking.model).find(|model| model.state == "downloading" && model.total > 0);
    match status.state.as_str() {
        "listing" => Progress { summary: "Looking through your folders…".into(), fraction: -1.0, first_left: 0, deep_left: 0, active: true },
        "reading" if first_pass_left == 0 && model.state == "downloading" && model.total > 0 => {
            working(format!("Getting {}: {} of {}", model.name, crate::format::size(model.received), crate::format::size(model.total)))
        }
        "reading" if first_pass_left == 0 && deep_queued > 0 => {
            working(format!("Thorough read: {} of {} · already searchable", group(deep_done), plural(deep_done + deep_queued, "image", "images")))
        }
        "reading" if first_pass_left == 0 && looking_model.is_some() => {
            let model = looking_model.unwrap();
            working(format!("Getting {}: {} of {}", model.name, crate::format::size(model.received), crate::format::size(model.total)))
        }
        // The model furthest behind, as the Images stage counts it (`shown_part`).
        "reading" if first_pass_left == 0 && status.looking.iter().any(|looking| looking.queued > 0) => {
            let first = status.looking.iter().max_by_key(|looking| looking.queued).unwrap_or(&status.looking[0]);
            // Why it isn't moving, when it isn't.
            let why = if first.model.state == "failed" {
                format!(" · {} couldn't be downloaded, trying again soon", crate::format::model_name(&first.engine))
            } else if !first.waiting.is_empty() {
                format!(" · {}", first.waiting)
            } else {
                String::new()
            };
            working(format!("Looking at what your images show: {} of {}{why}", group(first.done), plural(first.done + first.queued, "image", "images")))
        }
        "reading" if first_pass_left == 0 && status.looking.iter().any(|looking| looking.videos_queued > 0) => {
            let first = status.looking.iter().max_by_key(|looking| looking.videos_queued).unwrap_or(&status.looking[0]);
            working(format!("Looking at your videos: {} of {}", group(first.videos_done), plural(first.videos_done + first.videos_queued, "video", "videos")))
        }
        "reading" if first_pass_left == 0 && text_queued > 0 => {
            working(format!("Reading the text in your videos: {} of {}", group(text_done), plural(text_done + text_queued, "video", "videos")))
        }
        "reading" if first_pass_left == 0 && meaning_queued > 0 => {
            working(format!("Looking at what your documents mean: {} of {}", group(meaning_done), plural(meaning_done + meaning_queued, "file", "files")))
        }
        // The first sweep is over: what it set aside is filled in now.
        "reading" if first_pass_left == 0 && sweeping => working(format!("{} read in the first sweep, {} more to come", plural(done, "file", "files"), group(later))),
        "reading" => working(reading()),
        "paused" => working(status.detail.clone()),
        "idle" => Progress { summary: format!("Up to date · {}", plural(status.documents, "file", "files")), fraction: 1.0, first_left: 0, deep_left: 0, active: false },
        "stopped" if !status.detail.is_empty() => Progress { summary: format!("Stopped: {}", status.detail), fraction, first_left: 0, deep_left: 0, active: false },
        _ => Progress { summary: String::new(), fraction: -1.0, first_left: 0, deep_left: 0, active: false },
    }
}

thread_local! {
    /// The furthest the bar has come: in which session of reading, and how far.
    static FURTHEST: Cell<(i64, f32)> = const { Cell::new((0, 0.0)) };
}

/// How far along, for showing. The counts behind it are taken at slightly different moments, so it holds its place rather
/// than step back. (A new session starts over here, and is back at the same place by its counts, which come from the
/// catalog.)
pub fn progress(status: &Status) -> Progress {
    let mut progress = progress_of(status);
    if !progress.active {
        FURTHEST.set((status.started, 0.0));
    } else if progress.fraction >= 0.0 {
        let (session, furthest) = FURTHEST.get();
        progress.fraction = if session == status.started { progress.fraction.max(furthest) } else { progress.fraction };
        FURTHEST.set((status.started, progress.fraction));
    }
    progress
}

/// How far each model that looks at images has got: "EmbeddingGemma 2 has looked at 120 of 352 images, 0.07 s each".
fn seeing_progress(status: &Status) -> String {
    let lines: Vec<String> = status.looking.iter().map(|looking| {
        let name = crate::format::model_name(&looking.engine);
        let model = &looking.model;
        let total = looking.done + looking.queued;
        if model.state == "downloading" && model.total > 0 {
            format!("Getting {name}: {} of {}", crate::format::size(model.received), crate::format::size(model.total))
        } else if model.state == "failed" {
            format!("{name} couldn't be downloaded: {}. Trying again soon", model.error)
        } else if total == 0 {
            format!("{name} looks at your images once their text has been read")
        } else if looking.queued == 0 && looking.videos_queued > 0 {
            format!("{name} has looked at all {} and is looking at your videos: {} of {}", plural(total, "image", "images"),
                    group(looking.videos_done), plural(looking.videos_done + looking.videos_queued, "video", "videos"))
        } else if looking.queued == 0 {
            let videos = if looking.videos_done > 0 { format!(" and {}", plural(looking.videos_done, "video", "videos")) } else { String::new() };
            format!("{name} has looked at all {}{videos}", plural(total, "image", "images"))
        } else {
            let each = looking.ms.map(|ms| format!(", {:.2} s each", ms / 1000.0)).unwrap_or_default();
            let waiting = if looking.waiting.is_empty() { String::new() } else { format!(". {}", looking.waiting) };
            format!("{name} has looked at {} of {}{each}{waiting}", group(looking.done), plural(total, "image", "images"))
        }
    }).collect();
    lines.join(". ")
}

/// "Reading 1,234 of 5,678 files", "Up to date · 5,678 files", "Paused on battery power".
pub fn describe(status: &Status) -> String {
    if status.model.state == "downloading" && status.model.total > 0 {
        return format!("Getting {}: {} of {}", status.model.name, crate::format::size(status.model.received), crate::format::size(status.model.total));
    }
    if status.state == "waiting" {
        return "Waiting for setup".into();
    }
    progress(status).summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: &str) -> Status {
        Status { state: state.into(), ..Status::default() }
    }

    #[test]
    fn how_far_along() {
        let reading = Status { files: 100, read: 40, queued_text: 10, queued_pictures: 50, ..status("reading") };
        let progress = progress_of(&reading);
        assert_eq!(progress.summary, "Reading 40 of 100 files");
        assert_eq!(progress.first_left, 60);
        assert!(progress.active && (progress.fraction - 0.4).abs() < 0.001);

        // Images waiting for their schedule aren't work in progress.
        let waiting = Status { files: 100, read: 40, queued_text: 10, queued_pictures: 50, pictures_wait: true, ..status("reading") };
        let progress = progress_of(&waiting);
        assert_eq!(progress.summary, "Reading 40 of 50 files");
        assert_eq!(progress.first_left, 10);

        // The first sweep, then what it set aside: said by stage, one bar for all of it.
        let sweep = Status { files: 100, read: 10, queued_text: 2, queued_pictures: 88, queued_later: 80, ..status("reading") };
        let progress = progress_of(&sweep);
        assert_eq!(progress.summary, "Reading 10 of 20 files in the first sweep");
        assert!((progress.fraction - 0.1).abs() < 0.001);
        let filling = Status { files: 100, read: 20, queued_pictures: 80, queued_later: 80, ..status("reading") };
        let progress = progress_of(&filling);
        assert_eq!(progress.summary, "20 files read in the first sweep, 80 more to come");
        assert!((progress.fraction - 0.2).abs() < 0.001);

        let paused = Status { files: 100, read: 40, queued_text: 60, detail: "Paused on battery power".into(), ..status("paused") };
        assert_eq!(progress_of(&paused).summary, "Paused on battery power");
        assert!(progress_of(&paused).active);

        let idle = Status { documents: 1234, ..status("idle") };
        assert_eq!(progress_of(&idle).summary, "Up to date · 1,234 files");
        assert!(!progress_of(&idle).active);
        assert!(progress_of(&status("listing")).active);
        assert_eq!(progress_of(&status("listing")).fraction, -1.0);
    }

    /// The two reads with Deep are one job: the bar goes one way, from the first file to Deep's last image.
    #[test]
    fn looking_at_images_after_the_first_read() {
        let looking = |done: u64, queued: u64| Status {
            files: 100, read: 100, images: 60, images_read: 60,
            looking: vec![config::status::Looking { engine: "eg2-140".into(), done, queued, ..Default::default() }],
            ..status("reading")
        };
        assert_eq!(progress_of(&looking(20, 40)).summary, "Looking at what your images show: 20 of 60 images");
        let (early, late) = (progress_of(&looking(20, 40)).fraction, progress_of(&looking(50, 10)).fraction);
        assert!(early < late && late < 1.0, "{early} {late}");
        let mut downloading = looking(0, 60);
        downloading.looking[0].model = config::status::Model { state: "downloading".into(), name: "EmbeddingGemma 2".into(), received: 1 << 20, total: 4 << 20, ..Default::default() };
        assert!(progress_of(&downloading).summary.starts_with("Getting EmbeddingGemma 2: "));
        // Not moving: why.
        let mut waiting = looking(0, 60);
        waiting.looking[0].waiting = "Waiting for a quiet moment: the graphics card is busy".into();
        assert_eq!(progress_of(&waiting).summary, "Looking at what your images show: 0 of 60 images · Waiting for a quiet moment: the graphics card is busy");
        assert!(seeing_progress(&waiting).ends_with(". Waiting for a quiet moment: the graphics card is busy"));
        let mut failed = looking(0, 60);
        failed.looking[0].model = config::status::Model { state: "failed".into(), error: "no connection".into(), ..Default::default() };
        assert!(progress_of(&failed).summary.ends_with(" · EmbeddingGemma 2 couldn't be downloaded, trying again soon"));
    }

    #[test]
    fn deep_is_one_bar_that_never_goes_back() {
        let deep = |read: u64, images_read: u64, queued_pictures: u64, deep_queued: u64, deep_done: u64| Status {
            deep: true, files: 835, images: 834, read, images_read, queued_pictures, deep_queued, deep_done, ..status("reading")
        };
        // What happened on this PC: 834 images, the first read reads them all with 72 having no text, Deep then reads the rest.
        let mut last = -1.0;
        let mut steps = Vec::new();
        for read in (0..=834).step_by(40).chain([834]) {
            let with_text = read * 762 / 834;
            steps.push(deep(read + 1, read, 834 - read, with_text, 0));
        }
        for deep_done in (0..=762).step_by(40).chain([762]) {
            steps.push(deep(835, 834, 0, 762 - deep_done, deep_done));
        }
        for step in &steps {
            let fraction = progress_of(step).fraction;
            assert!(fraction >= last, "{fraction} after {last}");
            last = fraction;
        }
        assert!((last - 1.0).abs() < 0.001);
        // Most of the bar is Deep's, which takes most of the time.
        let first_read_over = progress_of(&deep(835, 834, 0, 762, 0));
        assert!(first_read_over.fraction > 0.1 && first_read_over.fraction < 0.2);

        // Words that say what's happening.
        assert_eq!(progress_of(&deep(613, 612, 222, 500, 0)).summary, "First read: 612 of 834 images");
        assert_eq!(progress_of(&deep(835, 834, 0, 706, 64)).summary, "Thorough read: 64 of 770 images · already searchable");
        // Only one read with Quick or Balanced.
        let balanced = Status { files: 835, images: 834, read: 613, images_read: 612, queued_pictures: 222, ..status("reading") };
        assert_eq!(progress_of(&balanced).summary, "Reading 613 of 835 files");

        // A restart of reading: Deep's count comes from the catalog, so it's back where it was.
        let before = progress_of(&deep(835, 834, 0, 700, 62));
        let restarted = Status { started: 99, ..deep(835, 834, 0, 700, 62) };
        assert_eq!(progress_of(&restarted).fraction, before.fraction);

        // Counts taken at slightly different moments: it holds its place.
        let ahead = Status { started: 7, ..deep(835, 834, 0, 700, 62) };
        let behind = Status { started: 7, ..deep(835, 834, 0, 700, 40) };
        let shown = progress(&ahead).fraction;
        assert_eq!(progress(&behind).fraction, shown);
        // Done, and new work: it starts afresh.
        assert!(!progress(&Status { started: 7, ..status("idle") }).active);
        assert!(progress(&behind).fraction < shown);
    }

    /// With images found by what they show, the Images stage has two halves, each with its own timing.
    #[test]
    fn images_split_in_two() {
        let looking = config::status::Looking { engine: "siglip2-b16".into(), done: 20, queued: 80, ..Default::default() };
        let reading = Status { engine: "PP-OCRv6 tiny".into(), files: 100, images: 100, images_read: 100, read: 100, looking: vec![looking], ..status("reading") };
        let images = stages(&reading, 0.0, 0.0).into_iter().find(|stage| stage.split).expect("a split stage");
        assert_eq!((images.name.as_str(), images.part.as_str(), images.state), ("Images", "Text in images", DONE));
        assert_eq!((images.shown.state, images.shown.count.as_str()), (GOING, "20 of 100 images"));
        let scheduled = Status { seeing_wait: true, seeing_at: 1, ..reading.clone() };
        let images = stages(&scheduled, 0.0, 0.0).into_iter().find(|stage| stage.split).unwrap();
        assert_eq!((images.shown.state, images.shown.note.as_str()), (WAITING, "Waiting for the next scheduled time"));
        // Without them, one stage as before.
        let alone = Status { looking: Vec::new(), ..reading };
        assert!(stages(&alone, 0.0, 0.0).iter().all(|stage| !stage.split));
    }

    #[test]
    fn what_the_activity_page_lists() {
        let names = |stages: &[StageRow]| stages.iter().map(|stage| (stage.name.to_string(), stage.state)).collect::<Vec<_>>();
        // Deep with the graphics card busy: it says why.
        let waiting = Status {
            deep: true, engine: "PP-OCRv6 small".into(), files: 835, images: 834, read: 835, images_read: 834, deep_queued: 700, deep_done: 62,
            waits: [String::new(), String::new(), "Waiting for a quiet moment: the graphics card is busy".into()], ..status("reading")
        };
        let stages = stages(&waiting, 0.0, 1000.0);
        assert_eq!(names(&stages), [("Finding files".to_string(), DONE), ("Documents and code".to_string(), DONE), ("First read".to_string(), DONE), ("Thorough read".to_string(), WAITING)]);
        assert_eq!(stages[3].note, "Waiting for a quiet moment: the graphics card is busy");
        assert_eq!(stages[3].count, "62 of 762 images");
        // A library set aside by the first sweep, read with Balanced.
        let sweep = Status { engine: "PP-OCRv6 small".into(), files: 100, images: 90, read: 15, images_read: 5, queued_pictures: 85, queued_later: 80, ..status("reading") };
        let stages = super::stages(&sweep, 1000.0, 0.0);
        assert_eq!(names(&stages), [("Finding files".to_string(), DONE), ("Documents and code".to_string(), DONE), ("Images".to_string(), GOING), ("What the first sweep set aside".to_string(), STILL_TO_COME)]);
        assert_eq!(stages[2].count, "5 of 10 images");
        // Text in images is off: no such stage.
        assert_eq!(super::stages(&status("idle"), 0.0, 0.0).len(), 2);
        // Why a file couldn't be read: plain words, or the reader's own when it has nothing odd to say.
        assert_eq!(plain_why("PdfiumLibraryInternalError(FormatError)"), "It's damaged, or it isn't really a PDF");
        assert_eq!(plain_why("Too large to read"), "Too large to read");
    }

    #[test]
    fn videos_have_their_own_card() {
        // Images all looked at, videos under way: the Images card is done, the Videos card says how far along.
        let looking = config::status::Looking { engine: "eg2-140".into(), done: 50, queued: 0, videos_done: 683, videos_queued: 317, ..Default::default() };
        let reading = Status { files: 50, images: 50, read: 50, images_read: 50, engine: "PP-OCRv6 small".into(), looking: vec![looking], ..status("reading") };
        let stages = super::stages(&reading, 0.0, 0.0);
        let images = stages.iter().find(|stage| stage.split).unwrap();
        assert_eq!((images.shown.state, images.shown.note.as_str()), (DONE, ""));
        let videos = stages.iter().find(|stage| stage.name == "Videos").unwrap();
        assert_eq!((videos.state, videos.count.as_str()), (GOING, "683 of 1,000 videos"));
        // No videos: no card.
        let looking = config::status::Looking { engine: "eg2-140".into(), done: 50, ..Default::default() };
        assert!(super::stages(&Status { looking: vec![looking.clone()], ..reading.clone() }, 0.0, 0.0).iter().all(|stage| stage.name != "Videos"));
        // Their text read too: the card's two halves, text first.
        let text = config::status::VideoText { done: 3, queued: 7, ..Default::default() };
        let both = Status { video_text: Some(text.clone()), ..reading.clone() };
        let stages = super::stages(&both, 0.0, 0.0);
        let videos = stages.iter().find(|stage| stage.name == "Videos").unwrap();
        assert!(videos.split);
        assert_eq!((videos.part.as_str(), videos.state, videos.count.as_str()), ("Text in videos", GOING, "3 of 10 videos"));
        assert_eq!(videos.shown.count.as_str(), "683 of 1,000 videos");
        // And what's said in them: a third part, after the recordings.
        let listening = config::status::Listening { recordings: true, videos: true, done: 4, queued: 2, videos_done: 1, videos_queued: 9, ..Default::default() };
        let all = Status { listening: Some(listening.clone()), ..both.clone() };
        let stages = super::stages(&all, 0.0, 0.0);
        let videos = stages.iter().find(|stage| stage.name == "Videos").unwrap();
        assert!(videos.third);
        assert_eq!((videos.third_part.as_str(), videos.extra.state, videos.extra.note.as_str()), ("What's said", STILL_TO_COME, "After the recordings"));
        assert_eq!(videos.extra.count.as_str(), "1 of 10 videos");
        assert!(stages.iter().any(|stage| stage.name == "Recordings"));
        // Videos' speech without recordings: no Recordings card; text and speech, two parts.
        let videos_only = config::status::Listening { recordings: false, ..listening };
        let stages = super::stages(&Status { listening: Some(videos_only), ..both }, 0.0, 0.0);
        assert!(stages.iter().all(|stage| stage.name != "Recordings"));
        let videos = stages.iter().find(|stage| stage.name == "Videos").unwrap();
        assert!(videos.third);
        assert_eq!((videos.extra.state, videos.extra.note.as_str()), (GOING, "Listening to what's said"));
        // Only their text (what they show is off): one card, not split.
        let only = Status { video_text: Some(text), looking: vec![looking], ..reading };
        let videos = super::stages(&only, 0.0, 0.0).into_iter().find(|stage| stage.name == "Videos").unwrap();
        assert!(!videos.split);
        assert_eq!(videos.note.as_str(), "Reading the text they show");
    }

    #[test]
    fn counts_by_walking() {
        let folder = std::env::temp_dir().join(format!("excavate-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::create_dir_all(folder.join("node_modules")).unwrap();
        for file in ["a.txt", "b.PNG", "sub/c.jpg", "sub/d.pdf", "e.exe", "node_modules/f.png", "g.wav", "sub/h.MKV"] {
            std::fs::write(folder.join(file), b"x").unwrap();
        }
        let exclusions = config::Exclusions::new(&["node_modules".to_string()]);
        assert_eq!(walk_count(&folder.display().to_string(), &exclusions), [4, 2, 1, 1]);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn short_paths_for_tags() {
        assert_eq!(short_path(r"C:\Users\me"), r"C:\Users\me");
        assert_eq!(short_path(r"C:\Users\me\AppData\Local\Packages\long-folder-name\Downloads\library"), r"C:\…\Downloads\library");
        assert_eq!(short_path(r"regex:^\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d$"), format!("{}…", &r"regex:^\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d\d$"[..47]));
    }

    #[test]
    fn what_the_status_line_says() {
        let downloading = Status {
            model: config::status::Model { state: "downloading".into(), name: "the thorough reader".into(), received: 1 << 30, total: 2 << 30, error: String::new() },
            ..status("reading")
        };
        assert!(describe(&downloading).starts_with("Getting the thorough reader: "));
        assert_eq!(describe(&status("waiting")), "Waiting for setup");
    }
}

fn folder_size(folder: &std::path::Path) -> u64 {
    std::fs::read_dir(folder)
        .map(|entries| entries.flatten().filter_map(|entry| entry.metadata().ok()).filter(|metadata| metadata.is_file()).map(|metadata| metadata.len()).sum())
        .unwrap_or(0)
}

/// About's updates: the switch, how it's going, and the button beside it.
pub fn fill_about(app: &App) {
    use crate::update::Status;
    let on = app.settings.borrow().auto_update;
    let ready = crate::update::staged(&app.paths).map(|(version, _)| version);
    let (line, action) = match (crate::update::status(), ready) {
        (Status::NotPublished, _) => ("Updates will come from Excavate's GitHub page once it's published.".to_string(), ""),
        (_, Some(version)) | (Status::Ready(version), None) => (format!("Version {version} is ready. It's used the next time Excavate starts."), "Restart now"),
        (Status::Checking, None) => ("Looking for a newer version…".to_string(), ""),
        (Status::Downloading(version, part), None) => (format!("Downloading version {version}… {}%", (part * 100.0).round() as u32), ""),
        (Status::Latest, None) => ("You have the latest version.".to_string(), "Check now"),
        (Status::Failed(error), None) => (format!("Couldn't look for updates: {error}."), "Try again"),
        (Status::Idle, None) => (String::new(), "Check now"),
    };
    with_setup(|setup| {
        setup.window.set_auto_update(on);
        setup.window.set_update_line(line.into());
        setup.window.set_update_action(action.into());
    });
}
