//! Excavate puts itself in place: run from wherever it was unzipped, it offers one screen, Install (or Update over an
//! older copy), copies its programs to `%LOCALAPPDATA%\Programs\Excavate` (no administrator), adds itself to the Start
//! menu and to Windows' Apps list, and starts from there. `--uninstall` (the Apps list's Uninstall) takes it out again,
//! keeping what's been read or deleting everything. `/S` does either without a window, for winget.
//!
//! In a sandbox (EXCAVATE_HOME, with EXCAVATE_INSTALL_TEST) everything goes under the sandbox's home, and what would be
//! written to the registry is only logged.

use crate::settings::{Paths, log};
use slint::ComponentHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

/// The Apps list's entry (the current user's own).
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Excavate";
/// Beside the installed programs: the version installed there.
const VERSION_FILE: &str = "installed-version";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where it goes, and what's written for it.
#[derive(Clone, Debug)]
pub struct Place {
    pub folder: PathBuf,
    shortcut: PathBuf,
    /// The registry is written (false in a sandbox: logged instead).
    register: bool,
}

fn sandbox() -> Option<PathBuf> {
    std::env::var_os("EXCAVATE_HOME").map(PathBuf::from)
}

fn testing() -> bool {
    std::env::var_os("EXCAVATE_INSTALL_TEST").is_some()
}

impl Place {
    /// Where it was installed before (the Apps list says), else the usual place.
    pub fn current() -> Place {
        if let Some(home) = sandbox() {
            let chosen = std::fs::read_to_string(home.join("install-location")).ok().map(|text| PathBuf::from(text.trim()));
            return Place { folder: chosen.unwrap_or_else(|| home.join("Programs").join("Excavate")), shortcut: home.join("Start Menu").join("Excavate.lnk"), register: false };
        }
        let chosen = winsys::registry::string(winsys::registry::Root::CurrentUser, UNINSTALL_KEY, "InstallLocation").map(PathBuf::from);
        let usual = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Programs").join("Excavate");
        let shortcut = winsys::files::known(winsys::files::Known::Programs).unwrap_or_else(std::env::temp_dir).join("Excavate.lnk");
        Place { folder: chosen.unwrap_or(usual), shortcut, register: true }
    }

    fn exe(&self) -> PathBuf {
        self.folder.join("excavate.exe")
    }

    /// The version installed there, if any.
    fn installed(&self) -> Option<String> {
        self.exe().exists().then(|| std::fs::read_to_string(self.folder.join(VERSION_FILE)).map(|text| text.trim().to_string()).unwrap_or_default())
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).display().to_string().to_lowercase();
    canonical(a) == canonical(b)
}

fn here() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// What starting this copy should do.
pub enum Start {
    /// Run as always (it's the installed copy, a test or a measuring build, or started by Windows or an update).
    Run,
    /// The installed copy is this version or newer: it's started instead.
    Hand(PathBuf),
    /// Offer to install, or to update an older copy.
    Install { update: bool },
    Uninstall,
}

pub fn decide(args: &[String]) -> Start {
    let has = |flag: &str| args.iter().any(|arg| arg.eq_ignore_ascii_case(flag));
    if has("--uninstall") {
        return Start::Uninstall;
    }
    // Not a debug build or a sandbox, unless a test asks; not when Windows starts it, an update restarts it, or it's
    // measuring; and not a copy asked to stay where it is.
    if (cfg!(debug_assertions) || sandbox().is_some()) && !testing() {
        return Start::Run;
    }
    if ["--hidden", "--after-update", "--bench", "--portable", "--quit"].iter().any(|flag| has(flag)) {
        return Start::Run;
    }
    let place = Place::current();
    let Some(here) = here() else { return Start::Run };
    if same_folder(&here, &place.folder) {
        return Start::Run;
    }
    match place.installed() {
        Some(version) if !version.is_empty() && !crate::update::newer(env!("CARGO_PKG_VERSION"), &version) => Start::Hand(place.exe()),
        Some(_) => Start::Install { update: true },
        None => Start::Install { update: false },
    }
}

/// Whether this copy runs from somewhere other than where Excavate is installed (where it was unzipped), so the popup
/// offers to install it, once. Worked out once.
pub fn offered() -> bool {
    static OFFERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFFERED.get_or_init(|| {
        if (cfg!(debug_assertions) || sandbox().is_some()) && !testing() {
            return false;
        }
        if std::env::args().any(|arg| arg.eq_ignore_ascii_case("--portable")) {
            return false;
        }
        here().is_some_and(|here| !same_folder(&here, &Place::current().folder))
    })
}

/// The offer taken: this copy started again without arguments (it shows the install screen), and this one quits.
pub fn start_install_screen() -> bool {
    std::env::current_exe().ok().is_some_and(|exe| std::process::Command::new(exe).spawn().is_ok())
}

/// The installed copy started in this one's place, with the same arguments.
pub fn hand_over(exe: &Path) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = std::process::Command::new(exe).args(args).spawn() {
        log(&format!("install: couldn't start the installed copy: {error}"));
    }
}

/// How far a copy has got (0 to 1), and what it's doing.
type Progress = Box<dyn Fn(f32, &str) + Send>;

/// Puts this copy's programs in `place`: the running installed copy is asked to quit first, a file in use goes aside as
/// `.old` (removed at its next start, as an update's are), and a copy that fails is undone. Then the Start menu, the
/// Apps list, and the version.
fn install_files(place: &Place, progress: &Progress) -> Result<(), String> {
    let here = here().ok_or("where this copy is isn't known")?;
    // The installed copy, if it's running, is asked to quit (the same home, so the same tray window).
    if winsys::tray::ask_to_quit(&config::window_class("Shell")) {
        progress(0.02, "Closing the copy that's running");
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    std::fs::create_dir_all(&place.folder).map_err(|error| format!("{}: {error}", place.folder.display()))?;
    let files: Vec<(&str, u64)> = crate::update::FILES.iter()
        .filter_map(|file| std::fs::metadata(here.join(file)).ok().map(|metadata| (*file, metadata.len())))
        .collect();
    if !files.iter().any(|(file, _)| *file == "excavate.exe") {
        return Err("excavate.exe isn't beside this copy".into());
    }
    let total: u64 = files.iter().map(|(_, size)| size).sum::<u64>().max(1);
    let (mut done, mut moved, mut put) = (0u64, Vec::new(), Vec::new());
    let mut result = Ok(());
    for (file, size) in &files {
        progress(0.05 + 0.85 * done as f32 / total as f32, "Putting Excavate in place");
        let target = place.folder.join(file);
        if target.exists() {
            let old = place.folder.join(format!("{file}.old"));
            let _ = std::fs::remove_file(&old);
            if let Err(error) = std::fs::rename(&target, &old) {
                result = Err(format!("{file}: {error}"));
                break;
            }
            moved.push((old, target.clone()));
        }
        if let Err(error) = std::fs::copy(here.join(file), &target) {
            result = Err(format!("{file}: {error}"));
            break;
        }
        put.push(target);
        done += size;
    }
    if let Err(error) = result {
        for target in put {
            let _ = std::fs::remove_file(target);
        }
        for (old, target) in moved {
            let _ = std::fs::rename(old, target);
        }
        return Err(error);
    }
    let _ = std::fs::write(place.folder.join(VERSION_FILE), env!("CARGO_PKG_VERSION"));
    progress(0.93, "Adding it to the Start menu");
    register(place, total);
    progress(1.0, "Done");
    Ok(())
}

/// The Start menu's shortcut and the Apps list's entry (logged in a sandbox).
fn register(place: &Place, bytes: u64) {
    if let Some(folder) = place.shortcut.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    winsys::com_sta();
    if let Err(error) = winsys::files::make_shortcut(&place.shortcut, &place.exe(), "Search your PC by name, by what's inside, and by what it shows") {
        log(&format!("install: the Start menu's shortcut: {error}"));
    }
    let exe = place.exe().display().to_string();
    let uninstall = format!("\"{exe}\" --uninstall");
    let quiet = format!("\"{exe}\" --uninstall /S");
    let location = place.folder.display().to_string();
    let size = (bytes / 1024) as u32;
    use winsys::registry::Value;
    let values = [
        ("DisplayName", Value::Text("Excavate")),
        ("DisplayVersion", Value::Text(env!("CARGO_PKG_VERSION"))),
        ("Publisher", Value::Text("Excavate")),
        ("DisplayIcon", Value::Text(&exe)),
        ("InstallLocation", Value::Text(&location)),
        ("UninstallString", Value::Text(&uninstall)),
        ("QuietUninstallString", Value::Text(&quiet)),
        ("URLInfoAbout", Value::Text("https://aynader.github.io/Excavate/")),
        ("EstimatedSize", Value::Number(size)),
        ("NoModify", Value::Number(1)),
        ("NoRepair", Value::Number(1)),
    ];
    if !place.register {
        let _ = std::fs::write(sandbox().unwrap_or_default().join("install-location"), &location);
        log(&format!("install: would register {UNINSTALL_KEY} ({location}, {size} KB, {uninstall})"));
        return;
    }
    if let Err(error) = winsys::registry::set_user_key(UNINSTALL_KEY, &values) {
        log(&format!("install: the Apps list's entry: {error}"));
    }
}

/// Everything installed taken out: Start with Windows, the shortcut, the Apps list's entry, and, chosen, what's been
/// read and the settings. The programs go once this process has ended (it's one of them).
fn uninstall_files(place: &Place, paths: &Paths, delete_all: bool) -> Result<(), String> {
    if winsys::tray::ask_to_quit(&config::window_class("Shell")) {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if place.register {
        let _ = winsys::registry::set_start_with_windows(crate::RUN_NAME, "", false);
        let _ = winsys::registry::delete_user_key(UNINSTALL_KEY);
    } else {
        log(&format!("uninstall: would take out the Run value and {UNINSTALL_KEY}"));
    }
    let _ = std::fs::remove_file(&place.shortcut);
    // Excavate Bedrock, the helper, if it was allowed: it goes too (Windows asks once; it's in Program Files, which
    // only an administrator may change). Its program here does it, before it's deleted.
    let bedrock = place.folder.join("excavate-bedrock.exe");
    if !place.register {
        log("uninstall: would remove Excavate Bedrock if installed (a sandbox)");
    } else if engine::names::service_installed() && bedrock.exists() {
        match winsys::process::run_elevated(&bedrock.to_string_lossy(), "--remove") {
            winsys::process::Elevated::Ended(0) => log("uninstall: Excavate Bedrock removed"),
            other => log(&format!("uninstall: Excavate Bedrock not removed ({other:?})")),
        }
    }
    if delete_all {
        for folder in [&paths.local, &paths.roaming] {
            // (The log can't go while it's open: what's left goes with the folder's last files.)
            if let Err(error) = std::fs::remove_dir_all(folder) {
                log(&format!("uninstall: {}: {error}", folder.display()));
            }
        }
    }
    // Only Excavate's own files, by name, then the folder if that left it empty: never a folder of someone else's.
    let mut names: Vec<String> = crate::update::FILES.iter().flat_map(|file| [file.to_string(), format!("{file}.old")]).collect();
    names.push(VERSION_FILE.to_string());
    let files: Vec<String> = names.iter().map(|name| place.folder.join(name)).filter(|path| path.exists())
        .map(|path| format!("\"{}\"", path.display())).collect();
    let script = format!("ping -n 3 127.0.0.1 >nul & del /f /q {} & rmdir \"{}\"", files.join(" "), place.folder.display());
    std::process::Command::new("cmd.exe").args(["/d", "/c"]).raw_arg(&script).creation_flags(CREATE_NO_WINDOW).spawn()
        .map(|_| ()).map_err(|error| error.to_string())
}

/// How much what's been read takes (the index, the models), for the uninstall screen.
fn kept_size(paths: &Paths) -> u64 {
    fn walk(folder: &Path) -> u64 {
        std::fs::read_dir(folder).map(|entries| entries.flatten().map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&entry.path()),
            Ok(_) => entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
            Err(_) => 0,
        }).sum()).unwrap_or(0)
    }
    walk(&paths.local)
}

fn size_text(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => String::new(),
        1_000_000..1_000_000_000 => format!("{} MB", bytes / 1_000_000),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

/// `/S`: no window.
pub fn silent(args: &[String]) -> bool {
    args.iter().any(|arg| arg.eq_ignore_ascii_case("/S") || arg == "--silent")
}

/// Installs or updates without a window (`/S`); the installed copy isn't started. Its exit code says how it went.
pub fn install_silently() -> u32 {
    let place = Place::current();
    match install_files(&place, &(Box::new(|_: f32, _: &str| {}) as Progress)) {
        Ok(()) => {
            log(&format!("install: {} put in {} (silently)", env!("CARGO_PKG_VERSION"), place.folder.display()));
            0
        }
        Err(error) => {
            log(&format!("install: couldn't install silently: {error}"));
            1
        }
    }
}

pub fn uninstall_silently(paths: &Paths) -> u32 {
    match uninstall_files(&Place::current(), paths, false) {
        Ok(()) => 0,
        Err(error) => {
            log(&format!("uninstall: {error}"));
            1
        }
    }
}

fn select_backend() {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|attributes| {
            use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
            attributes.with_class_name("Excavate.Install")
        })
        .select()
        .expect("no window system");
}

/// The title bar in the window's colors, as Settings' is.
fn style(dark: bool) {
    let _ = slint::invoke_from_event_loop(move || {
        let handle = winsys::window::find("Excavate.Install");
        if handle != 0 {
            let surfaces = crate::themes::Surfaces::from_index(0);
            let ([r, g, b], [tr, tg, tb]) = (crate::themes::base(dark, surfaces), crate::themes::ink(dark, surfaces));
            winsys::window::style_titlebar(handle, dark, (r, g, b), (tr, tg, tb));
        }
    });
}

fn hwnd(window: &crate::InstallWindow) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window().window_handle().window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(win32)) => win32.hwnd.get(),
        _ => 0,
    }
}

/// The install screen; when it's done the installed copy starts and this one ends.
pub fn run_install(update: bool, settings: &config::Settings) {
    select_backend();
    let window = crate::InstallWindow::new().expect("the install window");
    let dark = crate::popup::is_dark(settings);
    window.global::<crate::Theme>().set_dark(dark);
    let place = std::rc::Rc::new(std::cell::RefCell::new(Place::current()));
    if update {
        window.set_heading("Update Excavate".into());
        window.set_line(format!("Version {} replaces the one installed, keeping everything it has read.", env!("CARGO_PKG_VERSION")).into());
        window.set_button("Update".into());
    }
    window.set_place(place.borrow().folder.display().to_string().into());
    {
        let (weak, place) = (window.as_weak(), place.clone());
        window.on_change_place(move || {
            let Some(window) = weak.upgrade() else { return };
            if let Some(chosen) = winsys::dialog::pick_folder(hwnd(&window), "Choose where Excavate goes") {
                let mut place = place.borrow_mut();
                place.folder = PathBuf::from(chosen).join("Excavate");
                window.set_place(place.folder.display().to_string().into());
            }
        });
    }
    {
        let (weak, place) = (window.as_weak(), place.clone());
        window.on_install(move || {
            let Some(window) = weak.upgrade() else { return };
            window.set_stage(1);
            window.set_progress(0.0);
            let place = place.borrow().clone();
            let weak = window.as_weak();
            std::thread::spawn(move || {
                let shown = weak.clone();
                let progress: Progress = Box::new(move |done, note| {
                    let (shown, note) = (shown.clone(), note.to_string());
                    let _ = shown.upgrade_in_event_loop(move |window| {
                        window.set_progress(done);
                        window.set_note(note.into());
                    });
                });
                let result = install_files(&place, &progress);
                let _ = weak.upgrade_in_event_loop(move |window| match result {
                    Ok(()) => {
                        log(&format!("install: {} put in {}; starting it", env!("CARGO_PKG_VERSION"), place.folder.display()));
                        if let Err(error) = std::process::Command::new(place.exe()).spawn() {
                            log(&format!("install: couldn't start it: {error}"));
                        }
                        let _ = window.hide();
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        log(&format!("install: {error}"));
                        window.set_error(format!("Something got in the way ({error}). Nothing was changed.").into());
                        window.set_stage(2);
                    }
                });
            });
        });
    }
    style(dark);
    window.run().expect("the install window");
}

/// The uninstall screen.
pub fn run_uninstall(paths: Paths, settings: &config::Settings) {
    select_backend();
    let window = crate::InstallWindow::new().expect("the uninstall window");
    let dark = crate::popup::is_dark(settings);
    window.global::<crate::Theme>().set_dark(dark);
    window.set_uninstalling(true);
    {
        let (weak, paths) = (window.as_weak(), paths.clone());
        std::thread::spawn(move || {
            let size = size_text(kept_size(&paths));
            let _ = weak.upgrade_in_event_loop(move |window| window.set_kept_size(size.into()));
        });
    }
    window.on_cancel(|| {
        let _ = slint::quit_event_loop();
    });
    {
        let weak = window.as_weak();
        window.on_uninstall(move || {
            let Some(window) = weak.upgrade() else { return };
            window.set_stage(1);
            let delete_all = window.get_delete_all();
            match uninstall_files(&Place::current(), &paths, delete_all) {
                Ok(()) => {
                    log(&format!("uninstall: done{}", if delete_all { ", everything deleted" } else { ", what's been read kept" }));
                    let _ = window.hide();
                    let _ = slint::quit_event_loop();
                }
                Err(error) => {
                    window.set_error(format!("Something got in the way ({error}).").into());
                    window.set_stage(2);
                }
            }
        });
    }
    style(dark);
    window.run().expect("the uninstall window");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_said_plainly() {
        assert_eq!(size_text(500), "");
        assert_eq!(size_text(250_000_000), "250 MB");
        assert_eq!(size_text(2_140_000_000), "2.1 GB");
    }
}
