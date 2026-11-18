//! Settings → Privacy's viewer: what was found of one kind of secret, after Windows Hello. The files are listed from the
//! content index's marks (it keeps where secrets are, never their values). A picture is shown with its secrets
//! outlined and pixelated until Show; a document is read again by the indexer's own documents' reader
//! (`excavate-indexer --read-documents`, one file, then it ends) and its lines with a secret of this kind are shown,
//! the values as dots until Show. Nothing is kept: it closes itself after `OPEN_FOR`, and what it showed goes with it.

use crate::app::{App, Marks};
use crate::settings::log;
use crate::{FoundFile, FoundLine, SettingsWindow, with_app};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

/// How long the viewer stays open.
const OPEN_FOR: Duration = Duration::from_secs(120);
/// The longest side a picture is decoded at for the viewer.
const PICTURE_SIDE: u32 = 1400;
/// At most this many lines of a document are shown.
const LINES: usize = 30;
/// Characters of a line kept before and after a secret.
const AROUND: usize = 48;

struct Viewer {
    kind: String,
    files: Vec<(String, Vec<String>, Marks)>,
    selected: usize,
    shown: bool,
    until: Instant,
    /// The newest preview asked for: older answers are dropped.
    request: u64,
    /// A document's text and where its secrets of this kind are, once read (for Show without reading again).
    document: Option<(String, Vec<std::ops::Range<usize>>)>,
}

thread_local! {
    static VIEWER: RefCell<Option<Viewer>> = const { RefCell::new(None) };
    static TIMER: slint::Timer = slint::Timer::default();
}

/// "Passwords", "IBANs": the kinds as Settings → Privacy names them.
pub fn plural_name(kind: &str) -> &'static str {
    match kind {
        "password" => "Passwords",
        "code" => "One-time and backup codes",
        "key" => "API keys and tokens",
        "private-key" => "Private keys",
        "card" => "Card numbers",
        "iban" => "IBANs",
        "id" => "ID numbers",
        "document" => "Passports",
        "seed" => "Recovery phrases",
        _ => "Secrets",
    }
}

/// "Not a password": said of a file that has nothing else hidden.
fn not_one(kind: &str) -> &'static str {
    match kind {
        "password" => "Not a password",
        "code" => "Not a code",
        "key" => "Not a key",
        "private-key" => "Not a private key",
        "card" => "Not a card number",
        "iban" => "Not an IBAN",
        "id" => "Not an ID number",
        "document" => "Not a passport",
        "seed" => "Not a recovery phrase",
        _ => "Not a secret",
    }
}

fn settings_hwnd(window: &SettingsWindow) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window().window_handle();
    if let Ok(RawWindowHandle::Win32(win32)) = handle.window_handle().map(|handle| handle.as_raw()) {
        return win32.hwnd.get();
    }
    0
}

/// A kind clicked in Settings → Privacy: Windows Hello first, then the viewer.
pub fn open(app: &App, window: &SettingsWindow, kind: String) {
    let owner = settings_hwnd(window);
    let index = app.paths.index();
    let message = format!("Show the {} Excavate hid", plural_name(&kind).to_lowercase());
    std::thread::spawn(move || {
        winsys::com_mta();
        // A sandbox (`EXCAVATE_PRETEND_HELLO`) is taken as verified: no system prompt on the person's screen.
        let verified = if std::env::var_os("EXCAVATE_PRETEND_HELLO").is_some() {
            log("Privacy viewer: Windows Hello pretended");
            Ok(winsys::hello::Verified::Yes)
        } else {
            winsys::hello::verify(owner, &message)
        };
        match verified {
            Ok(winsys::hello::Verified::Yes) | Ok(winsys::hello::Verified::Unavailable) => {}
            Ok(winsys::hello::Verified::No) => {
                log("Privacy viewer: not verified");
                return;
            }
            Err(error) => {
                log(&format!("Privacy viewer: Windows Hello {error}"));
                return;
            }
        }
        let files = content::Reader::open(&index).map(|reader| reader.secret_files(&kind)).unwrap_or_default();
        let _ = slint::invoke_from_event_loop(move || with_app(|app| opened(app, kind, files)));
    });
}

fn opened(app: &App, kind: String, files: Vec<(String, Vec<String>, Vec<u8>)>) {
    let cleared = app.settings.borrow().not_secret.clone();
    let files: Vec<(String, Vec<String>, Marks)> = files.into_iter()
        .filter(|(path, _, _)| !cleared.iter().any(|path_cleared| path_cleared.eq_ignore_ascii_case(path)))
        .map(|(path, kinds, marks)| (path, kinds, Marks::parse(&marks)))
        .collect();
    log(&format!("Privacy viewer: {} files with {kind}", files.len()));
    let rows: Vec<FoundFile> = files.iter().map(|(path, _, marks)| {
        let (folder, name) = path.rsplit_once('\\').unwrap_or(("", path));
        let place = folder.rsplit('\\').next().unwrap_or(folder);
        let found = marks.boxes.len();
        let note = if found > 1 { format!("{found} found") } else { String::new() };
        FoundFile { name: name.into(), place: place.into(), note: note.into() }
    }).collect();
    let title = format!("{} · {}", plural_name(&kind), if files.len() == 1 { "1 file".to_string() } else { format!("{} files", files.len()) });
    VIEWER.with(|viewer| *viewer.borrow_mut() = Some(Viewer { kind, files, selected: 0, shown: false, until: Instant::now() + OPEN_FOR, request: 0, document: None }));
    with_window(|window| {
        window.set_found_title(title.into());
        window.set_found_files(slint::ModelRc::new(slint::VecModel::from(rows)));
        window.set_found_open(true);
    });
    tick();
    TIMER.with(|timer| timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), tick));
    pick(app, 0);
}

fn with_window(f: impl FnOnce(&SettingsWindow)) {
    crate::setup::with_window(f);
}

/// The time left, and closing when it's up.
fn tick() {
    let left = VIEWER.with(|viewer| viewer.borrow().as_ref().map(|viewer| viewer.until.saturating_duration_since(Instant::now())));
    let Some(left) = left else { return };
    if left.is_zero() {
        close();
        return;
    }
    let seconds = left.as_secs() + u64::from(left.subsec_millis() > 0);
    with_window(|window| window.set_found_left(format!("{}:{:02}", seconds / 60, seconds % 60).into()));
}

/// Closed, or its time up: everything it showed is let go.
pub fn close() {
    TIMER.with(|timer| timer.stop());
    VIEWER.with(|viewer| *viewer.borrow_mut() = None);
    with_window(|window| {
        window.set_found_open(false);
        window.set_found_image(slint::Image::default());
        window.set_found_has_image(false);
        window.set_found_lines(slint::ModelRc::default());
        window.set_found_files(slint::ModelRc::default());
        window.set_found_shown(false);
    });
}

/// A file chosen in the list.
pub fn pick(app: &App, index: usize) {
    let asked = VIEWER.with(|viewer| {
        let mut viewer = viewer.borrow_mut();
        let viewer = viewer.as_mut()?;
        let (path, kinds, marks) = viewer.files.get(index)?.clone();
        viewer.selected = index;
        viewer.shown = false;
        viewer.document = None;
        viewer.request += 1;
        Some((viewer.kind.clone(), path, kinds, marks, viewer.request))
    });
    let Some((kind, path, kinds, marks, request)) = asked else { return };
    let dismiss = if kinds.len() == 1 && kinds[0] == kind { not_one(&kind) } else { "" };
    with_window(|window| {
        window.set_found_selected(index as i32);
        window.set_found_shown(false);
        window.set_found_has_image(false);
        window.set_found_image(slint::Image::default());
        window.set_found_lines(slint::ModelRc::default());
        window.set_found_note("".into());
        window.set_found_loading(true);
        window.set_found_dismiss(dismiss.into());
    });
    load(app, kind, path, marks, request, false);
}

/// Show or hide the values of the file shown.
pub fn show_toggled(app: &App) {
    let asked = VIEWER.with(|viewer| {
        let mut viewer = viewer.borrow_mut();
        let viewer = viewer.as_mut()?;
        viewer.shown = !viewer.shown;
        let (path, _, marks) = viewer.files.get(viewer.selected)?.clone();
        Some((viewer.kind.clone(), path, marks, viewer.request, viewer.shown, viewer.document.clone()))
    });
    let Some((kind, path, marks, request, shown, document)) = asked else { return };
    with_window(|window| window.set_found_shown(shown));
    if let Some((text, ranges)) = document {
        with_window(|window| window.set_found_lines(lines_model(&text, &ranges, shown)));
        return;
    }
    load(app, kind, path, marks, request, shown);
}

fn is_picture(path: &str) -> bool {
    search::filter::extension(path).is_some_and(|extension| common::kinds::PICTURES.contains(&extension.to_ascii_lowercase().as_str()))
}

fn is_video(path: &str) -> bool {
    search::filter::extension(path).is_some_and(|extension| search::filter::VIDEO.contains(&extension.to_ascii_lowercase().as_str()))
}

fn load(app: &App, kind: String, path: String, marks: Marks, request: u64, shown: bool) {
    let accent = crate::app::rgb(&app.accent());
    if is_picture(&path) {
        std::thread::spawn(move || {
            winsys::com_mta();
            let picture = winsys::imaging::decode_for_showing(&path, (PICTURE_SIDE, PICTURE_SIDE)).map(|mut picture| {
                let (regions, size) = crate::blur::turned(&marks.boxes, (marks.width, marks.height), picture.orientation);
                if !shown {
                    crate::blur::pixelate(&mut picture.bitmap, &regions, size);
                }
                crate::blur::outline(&mut picture.bitmap, &regions, size, accent);
                let bitmap = picture.bitmap;
                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bitmap.rgba, bitmap.width, bitmap.height)
            });
            let _ = slint::invoke_from_event_loop(move || {
                if current(request) {
                    with_window(|window| {
                        window.set_found_loading(false);
                        match picture {
                            Ok(buffer) => {
                                window.set_found_image(slint::Image::from_rgba8_premultiplied(buffer));
                                window.set_found_has_image(true);
                                window.set_found_note(if marks.boxes.is_empty() { "Hidden whole: where it is in the picture wasn't kept" } else { "" }.into());
                            }
                            Err(error) => window.set_found_note(format!("Couldn't open it ({error})").into()),
                        }
                    });
                }
            });
        });
        return;
    }
    if is_video(&path) {
        let note = match marks.video_boxes.first() {
            Some(first) => format!("Shown in the video at {}. Open it to see it there.", clock(first[0] / 1000)),
            None => "Said in the video. Open it to hear it.".to_string(),
        };
        with_window(|window| {
            window.set_found_loading(false);
            window.set_found_note(note.into());
        });
        return;
    }
    std::thread::spawn(move || {
        let read = read_document(&path);
        let _ = slint::invoke_from_event_loop(move || {
            if !current(request) {
                return;
            }
            match read {
                Ok((text, secrets)) => {
                    let ranges: Vec<std::ops::Range<usize>> = secrets.into_iter().filter(|(each, _)| *each == kind).map(|(_, range)| range).collect();
                    VIEWER.with(|viewer| {
                        if let Some(viewer) = viewer.borrow_mut().as_mut() {
                            viewer.document = Some((text.clone(), ranges.clone()));
                        }
                    });
                    with_window(|window| {
                        window.set_found_loading(false);
                        if ranges.is_empty() {
                            window.set_found_note("Not found in it any more. It may have changed since it was read.".into());
                        } else {
                            let more = ranges.len().saturating_sub(LINES);
                            window.set_found_note(if more > 0 { format!("and {more} more in the file") } else { String::new() }.into());
                            window.set_found_lines(lines_model(&text, &ranges, shown));
                        }
                    });
                }
                Err(error) => with_window(|window| {
                    window.set_found_loading(false);
                    window.set_found_note(format!("Couldn't read it again ({error})").into());
                }),
            }
        });
    });
}

fn current(request: u64) -> bool {
    VIEWER.with(|viewer| viewer.borrow().as_ref().is_some_and(|viewer| viewer.request == request))
}

fn clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds >= 3600 { format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60) } else { format!("{}:{:02}", seconds / 60, seconds % 60) }
}

fn lines_model(text: &str, ranges: &[std::ops::Range<usize>], shown: bool) -> slint::ModelRc<FoundLine> {
    let lines: Vec<FoundLine> = found_lines(text, ranges, shown).into_iter()
        .map(|(before, value, after)| FoundLine { before: before.into(), value: value.into(), after: after.into() })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(lines))
}

/// Each secret on its line: up to `AROUND` characters before and after it, and the value, or dots when hidden.
fn found_lines(text: &str, ranges: &[std::ops::Range<usize>], shown: bool) -> Vec<(String, String, String)> {
    ranges.iter().take(LINES).filter(|range| range.end <= text.len() && text.is_char_boundary(range.start) && text.is_char_boundary(range.end)).map(|range| {
        let line_start = text[..range.start].rfind('\n').map_or(0, |at| at + 1);
        let line_end = text[range.end..].find('\n').map_or(text.len(), |at| range.end + at);
        let before: Vec<char> = text[line_start..range.start].chars().collect();
        let before: String = if before.len() > AROUND { format!("…{}", before[before.len() - AROUND..].iter().collect::<String>()) } else { before.into_iter().collect() };
        let after: Vec<char> = text[range.end..line_end].chars().collect();
        let after: String = if after.len() > AROUND { format!("{}…", after[..AROUND].iter().collect::<String>()) } else { after.into_iter().collect() };
        let value = if shown { text[range.clone()].replace(['\n', '\r', '\t'], " ") } else { "•".repeat(text[range.clone()].chars().count().clamp(6, 12)) };
        (before.replace('\t', " "), value, after.replace('\t', " "))
    }).collect()
}

/// The document read again by the indexer's documents' reader, in a process of its own: its text, and its secrets'
/// kinds and places.
type Secrets = Vec<(String, std::ops::Range<usize>)>;
fn read_document(path: &str) -> Result<(String, Secrets), String> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|folder| folder.join("excavate-indexer.exe")))
        .ok_or("the reader isn't beside Excavate")?;
    let mut child = std::process::Command::new(exe)
        .arg("--read-documents")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .spawn()
        .map_err(|error| error.to_string())?;
    let answer = (|| {
        let mut input = child.stdin.take().ok_or("no input")?;
        writeln!(input, "{path}").map_err(|error| error.to_string())?;
        drop(input);
        let output = child.stdout.take().ok_or("no output")?;
        let mut line = String::new();
        BufReader::new(output).read_line(&mut line).map_err(|error| error.to_string())?;
        Ok::<String, String>(line)
    })();
    let _ = child.kill();
    let _ = child.wait();
    let value: serde_json::Value = serde_json::from_str(&answer?).map_err(|error| error.to_string())?;
    if let Some(error) = value["error"].as_str() {
        return Err(error.to_string());
    }
    let text = value["text"].as_str().unwrap_or_default().to_string();
    let secrets = value["secrets"].as_array().into_iter().flatten().filter_map(|secret| {
        Some((secret[0].as_str()?.to_string(), secret[1].as_u64()? as usize..secret[2].as_u64()? as usize))
    }).collect();
    Ok((text, secrets))
}

/// "Not a password": the file is never blurred again, and leaves the list.
pub fn dismissed(app: &App) {
    let path = VIEWER.with(|viewer| viewer.borrow().as_ref().and_then(|viewer| viewer.files.get(viewer.selected).map(|file| file.0.clone())));
    let Some(path) = path else { return };
    log("Privacy viewer: a file said to hold nothing secret");
    {
        let mut settings = app.settings.borrow_mut();
        let lower = path.to_lowercase();
        if !settings.not_secret.contains(&lower) {
            settings.not_secret.push(lower);
        }
        settings.save(&app.paths);
    }
    let left = VIEWER.with(|viewer| {
        let mut viewer = viewer.borrow_mut();
        let viewer = viewer.as_mut()?;
        viewer.files.retain(|file| !file.0.eq_ignore_ascii_case(&path));
        viewer.selected = viewer.selected.min(viewer.files.len().saturating_sub(1));
        Some((viewer.kind.clone(), viewer.files.iter().map(|(path, _, marks)| {
            let (folder, name) = path.rsplit_once('\\').unwrap_or(("", path));
            let found = marks.boxes.len();
            FoundFile { name: name.into(), place: folder.rsplit('\\').next().unwrap_or(folder).into(), note: if found > 1 { format!("{found} found").into() } else { "".into() } }
        }).collect::<Vec<_>>(), viewer.selected))
    });
    let Some((kind, rows, selected)) = left else { return };
    if rows.is_empty() {
        close();
        return;
    }
    let title = format!("{} · {}", plural_name(&kind), if rows.len() == 1 { "1 file".to_string() } else { format!("{} files", rows.len()) });
    with_window(|window| {
        window.set_found_title(title.into());
        window.set_found_files(slint::ModelRc::new(slint::VecModel::from(rows)));
    });
    pick(app, selected);
}

/// The file shown, opened or shown in its folder.
pub fn selected_path() -> Option<String> {
    VIEWER.with(|viewer| viewer.borrow().as_ref().and_then(|viewer| viewer.files.get(viewer.selected).map(|file| file.0.clone())))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_secret_is_shown_on_its_line_hidden_until_asked() {
        let text = "Router\nWi-Fi password: hunter2-Blue! (kitchen)\nend";
        let start = text.find("hunter2").unwrap();
        let ranges = [start..start + "hunter2-Blue!".len()];
        let hidden = super::found_lines(text, &ranges, false);
        assert_eq!(hidden, [("Wi-Fi password: ".to_string(), "•".repeat(12), " (kitchen)".to_string())]);
        let shown = super::found_lines(text, &ranges, true);
        assert_eq!(shown[0].1, "hunter2-Blue!");
        assert_eq!(super::clock(83), "1:23");
    }
}
