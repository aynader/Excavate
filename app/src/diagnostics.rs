//! Settings → About → "Copy diagnostics": what a bug report needs, as text for the clipboard. This PC (Windows, the
//! processor, memory, the graphics cards), which indexer answers and how it's doing, the settings, the models chosen,
//! and the logs' last lines. Nothing in it names the person's files or folders: settings that hold paths are given as
//! counts, and every path in the logs is cut to its drive and its file's extension (`redact`).

use crate::app::App;
use config::Settings;
use serde_json::Value;

/// How many of the logs' lines go in.
const LOG_LINES: usize = 200;

/// The logs whose last lines go in: Excavate's own, the indexer's and the search helper's.
const LOGS: [&str; 3] = ["excavate", "indexer", "seeker"];

/// Settings that hold paths or folders' names: only how many there are goes in.
const COUNTED: [&str; 6] = ["folders", "folders-off", "exclude", "name-folders", "folder-kinds", "own-themes"];

/// Everything, as the text that's copied.
pub fn gather(app: &App) -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let settings = app.settings.borrow().clone();
    let mut text = format!("Excavate {}\n\n", env!("CARGO_PKG_VERSION"));
    text += &section("This PC", &this_pc());
    text += &section("Indexer", &indexer(app, &settings));
    text += &section("Models", &models(&settings));
    text += &section("Settings", &settings_lines(&settings, &home));
    let logs: Vec<(&str, String)> = LOGS.iter()
        .filter_map(|name| std::fs::read(app.paths.local.join("logs").join(format!("{name}.log"))).ok().map(|bytes| (*name, String::from_utf8_lossy(&bytes).into_owned())))
        .collect();
    let lines: Vec<String> = last_lines(&logs, LOG_LINES).iter().map(|line| redact(line, &home)).collect();
    text += &section(&format!("Last {} log lines", lines.len()), &lines);
    text
}

fn section(title: &str, lines: &[String]) -> String {
    let mut text = format!("## {title}\n");
    for line in lines {
        text += line;
        text.push('\n');
    }
    text.push('\n');
    text
}

/// Windows, the processor, memory and the graphics cards.
fn this_pc() -> Vec<String> {
    use winsys::registry::{Root, dword, string};
    let key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = string(Root::LocalMachine, key, "CurrentBuild").and_then(|build| build.trim().parse().ok()).unwrap_or(0);
    let mut product = string(Root::LocalMachine, key, "ProductName").unwrap_or_else(|| "Windows".into());
    // (Windows 11 still calls itself Windows 10 there.)
    if build >= 22000 {
        product = product.replace("Windows 10", "Windows 11");
    }
    let edition = string(Root::LocalMachine, key, "EditionID").unwrap_or_default();
    let release = string(Root::LocalMachine, key, "DisplayVersion").unwrap_or_default();
    let patch = dword(Root::LocalMachine, key, "UBR").map(|ubr| format!(".{ubr}")).unwrap_or_default();
    let mut lines = vec![format!("Windows: {product} ({edition}), {release}, build {build}{patch}")];
    lines.push(format!("Media Foundation: {}", if winsys::media::available() { "installed" } else { "missing" }));
    let cpu = string(Root::LocalMachine, r"HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString").unwrap_or_default();
    let threads = std::thread::available_parallelism().map_or(0, |threads| threads.get());
    lines.push(format!("Processor: {}, {threads} threads", cpu.trim()));
    lines.push(format!("Memory: {}", memory_gb().map_or_else(|| "unknown".into(), |gb| format!("{gb:.1} GB"))));
    let gpus = winsys::gpu::adapters();
    if gpus.is_empty() {
        lines.push("Graphics: none found".into());
    }
    for gpu in gpus {
        let driver = if gpu.driver.is_empty() { "unknown".to_string() } else { gpu.driver.clone() };
        lines.push(format!("Graphics: {}, {} MB of its own, driver {driver}", gpu.name, gpu.dedicated_mb));
    }
    let used = winsys::gpu::capable().map_or_else(|| "the processor".to_string(), |gpu| gpu.name);
    lines.push(format!("Models run on: {used}"));
    lines
}

fn memory_gb() -> Option<f64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: a structure of this process's, sized as Windows asks, filled in.
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some(status.ullTotalPhys as f64 / (1u64 << 30) as f64)
}

/// Which indexer answers for names, and how it's doing (as last found: nothing is asked of it here); and the reader.
fn indexer(app: &App, settings: &Settings) -> Vec<String> {
    let mut lines = Vec::new();
    if settings.own_index {
        let state = crate::own_index::state().map_or_else(|| "not started".to_string(), |state| {
            format!("{:?}, {} names on {} drives{}", state.phase, state.entries, state.volumes.len(),
                    if state.refused > 0 { format!(", {} folders couldn't be listed", state.refused) } else { String::new() })
        });
        lines.push(format!("Names: Bedrock, {state}{}", if crate::own_index::building() { " (building)" } else { "" }));
    } else {
        lines.push(format!("Names: Everything, {:?}", app.everything.last_health()));
    }
    lines.push(format!("Reader: {}", if app.indexer.running() { "running" } else { "not running" }));
    if let Some(status) = config::status::Status::load(&app.paths) {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        lines.push(redact(&format!("Reader's state: {}, {}", status.state, status.detail), &home));
        lines.push(format!("Files: {} read of {}, {} couldn't be read; images: {} read of {}", status.read, status.files, status.failed,
                           status.images_read, status.images));
    }
    lines
}

/// Each kind's model: on or off, the choice, and where it runs.
fn models(settings: &Settings) -> Vec<String> {
    let card = winsys::gpu::capable();
    let nvidia = card.as_ref().is_some_and(|card| card.name.to_ascii_lowercase().contains("nvidia"));
    let measured = crate::setup::measured_here();
    let on = |on: bool| if on { "on" } else { "off" };
    let reading = &settings.reading;
    let seeing = &settings.seeing;
    let listening = &settings.listening;
    vec![
        format!("Text in images: {}, {reading} on {}", on(settings.pictures_on), settings.reading_device(reading, card.is_some())),
        format!("What images show: {}, {seeing}{} on {}", on(settings.seeing_on),
                if seeing == "deep" { format!(" ({})", settings.seeing_deep) } else { String::new() },
                settings.seeing_device(seeing, card.is_some(), &measured)),
        format!("What's said in recordings: {}, {listening} on {}", on(settings.listening_on),
                settings.listen_device(listening, card.is_some(), nvidia && config::LISTEN_CUDA_PUBLISHED, &measured)),
        format!("What videos show: {}, {}", on(settings.videos_on), settings.video),
        format!("Text in videos: {}, {}", on(settings.video_text), settings.video_text_choice()),
        format!("What's said in videos: {}", on(settings.video_speech)),
    ]
}

/// The settings, one a line, without a path or a folder's name in them: those that hold them as how many, and any
/// other text redacted as the logs are.
pub fn settings_lines(settings: &Settings, home: &str) -> Vec<String> {
    let Ok(Value::Object(map)) = serde_json::to_value(settings) else { return Vec::new() };
    map.into_iter().map(|(key, value)| {
        let shown = match value {
            Value::Array(items) if COUNTED.contains(&key.as_str()) => items.len().to_string(),
            Value::Object(items) if COUNTED.contains(&key.as_str()) => items.len().to_string(),
            _ if COUNTED.contains(&key.as_str()) => "0".into(),
            // The monitor's ID is Windows' path to it.
            Value::String(text) if key == "monitor" => if text.is_empty() { "the mouse's".into() } else { "chosen".into() },
            Value::String(text) => redact(&text, home),
            Value::Array(items) => format!("{} items", items.len()),
            other => redact(&other.to_string(), home),
        };
        format!("{key}: {shown}")
    }).collect()
}

/// The last `count` lines of the logs, oldest first, each marked with its log (but Excavate's own). A line starts
/// with the seconds since 1970 it was written at; one that doesn't goes with the line before it.
pub fn last_lines(logs: &[(&str, String)], count: usize) -> Vec<String> {
    let mut lines: Vec<(f64, usize, String)> = Vec::new();
    for (name, text) in logs {
        let all: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
        let mut at = 0.0;
        for (place, line) in all[all.len().saturating_sub(count)..].iter().enumerate() {
            if let Some(seconds) = line.split(' ').next().and_then(|first| first.parse::<f64>().ok()) {
                at = seconds;
            }
            let shown = if *name == "excavate" { line.to_string() } else { format!("[{name}] {line}") };
            lines.push((at, place, shown));
        }
    }
    // (By time; lines written in the same instant, and those without one, keep their order.)
    lines.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let skip = lines.len().saturating_sub(count);
    lines.into_iter().skip(skip).map(|(_, _, line)| line).collect()
}

/// `line` with every path in it cut short: the person's own folder as `~`, every other folder as its drive, and what
/// the path names as its extension alone. `C:\Users\quokka\Documents\Taxes\2025.pdf` → `~\…\<file>.pdf`;
/// `D:\Photos\Wedding` → `D:\…\<name>`.
pub fn redact(line: &str, home: &str) -> String {
    let line = home_as_tilde(line, home);
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    while at < chars.len() {
        match root_at(&chars, at) {
            Some((root, after)) => {
                out += &root;
                let (shown, end) = rest_of_path(&chars, after);
                out += &shown;
                at = end;
            }
            None => {
                out.push(chars[at]);
                at += 1;
            }
        }
    }
    out
}

/// The person's folder (`C:\Users\quokka`), in any case and with either slash, as `~`.
fn home_as_tilde(line: &str, home: &str) -> String {
    let home = home.trim_end_matches(['\\', '/']);
    if home.len() < 3 {
        return line.to_string();
    }
    // (Byte by byte: ASCII in either case, either slash, anything else as it is.)
    let same = |text: &str| text.bytes().zip(home.bytes())
        .all(|(a, b)| a.eq_ignore_ascii_case(&b) || (separator(a as char) && separator(b as char)));
    let mut out = String::with_capacity(line.len());
    let mut at = 0;
    while at < line.len() {
        if let Some(text) = line.get(at..at + home.len()) {
            let next = line[at + home.len()..].chars().next();
            if same(text) && next.is_none_or(|next| !next.is_alphanumeric() && !matches!(next, '_' | '-' | '.')) {
                out.push('~');
                at += home.len();
                continue;
            }
        }
        let next = line[at..].chars().next().unwrap_or_default();
        out.push(next);
        at += next.len_utf8();
    }
    out
}

fn separator(c: char) -> bool {
    c == '\\' || c == '/'
}

/// Characters no file's or folder's name has: a path ends at one.
fn ends_path(c: char) -> bool {
    matches!(c, ':' | '"' | '<' | '>' | '|' | '*' | '?') || c.is_control()
}

/// A path's start at `at`: `C:\`, `~\` or `\\` (a network share's, or `\\?\` before a drive), as it's shown, and
/// where what follows it starts.
fn root_at(chars: &[char], at: usize) -> Option<(String, usize)> {
    if at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_') {
        return None;
    }
    let get = |place: usize| chars.get(place).copied().unwrap_or('\0');
    let c = chars[at];
    if c == '~' && separator(get(at + 1)) {
        return Some(("~\\".into(), at + 2));
    }
    if c.is_ascii_alphabetic() && get(at + 1) == ':' && separator(get(at + 2)) {
        return Some((format!("{}:\\", c.to_ascii_uppercase()), at + 3));
    }
    if c == '\\' && get(at + 1) == '\\' {
        if matches!(get(at + 2), '?' | '.') && get(at + 3) == '\\' {
            return root_at(chars, at + 4).or(Some(("\\\\".into(), at + 4)));
        }
        let next = get(at + 2);
        if next != '\0' && !separator(next) && !next.is_whitespace() {
            return Some(("\\\\".into(), at + 2));
        }
    }
    None
}

/// The rest of a path after its root, as it's shown (`…\<file>.pdf`), and where the line goes on after it. Folders
/// go up to the next separator; the last name up to a character no name has, or the line's end, then back to just
/// after its extension: `report v2.pdf: 3 pages` is `<file>.pdf: 3 pages`.
fn rest_of_path(chars: &[char], start: usize) -> (String, usize) {
    let mut folders = 0;
    let mut at = start;
    loop {
        // (A name doesn't start with a space: `E:\ only` is the drive alone.)
        if chars.get(at).is_some_and(|c| c.is_whitespace()) {
            return (if folders > 1 { "…\\<name>\\".into() } else if folders == 1 { "<name>\\".into() } else { String::new() }, at);
        }
        let mut end = at;
        while end < chars.len() && !separator(chars[end]) && !ends_path(chars[end]) {
            end += 1;
        }
        if end < chars.len() && separator(chars[end]) {
            if end > at {
                folders += 1;
            }
            at = end + 1;
            continue;
        }
        let last = &chars[at..end];
        let hidden = if folders > 0 && !last.is_empty() { "…\\" } else { "" };
        if last.is_empty() {
            // Ended with a separator: the last folder is the name.
            return (if folders > 1 { "…\\<name>\\".into() } else if folders == 1 { "<name>\\".into() } else { String::new() }, end);
        }
        return match extension(last) {
            Some((extension, length)) => (format!("{hidden}<file>.{extension}"), at + length),
            None => {
                // A name without one (a folder's, mostly): all of it, but for punctuation after it.
                let mut length = last.len();
                while length > 0 && matches!(last[length - 1], ')' | ']' | '}' | ',' | ';' | '.' | '\'' | ' ') {
                    length -= 1;
                }
                if length == 0 {
                    // (Only punctuation: no name.)
                    return (hidden.trim_end_matches('\\').to_string(), at);
                }
                (format!("{hidden}<name>"), at + length)
            }
        };
    }
}

/// The last extension in `name` that ends a word (`.pdf` in `report v1.2 final.pdf at 3`), and where it ends.
fn extension(name: &[char]) -> Option<(String, usize)> {
    let mut found = None;
    for (dot, &c) in name.iter().enumerate() {
        if c != '.' {
            continue;
        }
        let run = name[dot + 1..].iter().take_while(|c| c.is_ascii_alphanumeric()).count();
        let after = dot + 1 + run;
        if (1..=10).contains(&run) && name.get(after).is_none_or(|c| matches!(c, ' ' | ')' | ']' | ',' | ';' | '\'' | '.')) {
            found = Some((name[dot + 1..after].iter().collect(), after));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = r"C:\Users\quokka";

    #[test]
    fn paths_cut_short() {
        let cases = [
            (r"listening: C:\Users\quokka\Documents\Taxes\2025 return.pdf: 3 s of sound", "listening: ~\\…\\<file>.pdf: 3 s of sound"),
            (r"Deep on c:\users\QUOKKA\Pictures\scan.PNG: out of memory", "Deep on ~\\…\\<file>.PNG: out of memory"),
            (r"D:\Photos\Wedding: 120 files to consider, listed in 4 ms", "D:\\…\\<name>: 120 files to consider, listed in 4 ms"),
            (r"own index: would start Everything again (C:\Program Files\Everything\Everything.exe -startup; pretend)",
             "own index: would start Everything again (C:\\…\\<file>.exe -startup; pretend)"),
            (r"asked to skip \\nas\share\Secret Project\plan v1.2 final.docx", "asked to skip \\\\…\\<file>.docx"),
            (r"open \\?\C:\Users\quokka\Desktop\notes.txt failed", "open ~\\…\\<file>.txt failed"),
            (r"listening: C:\Users\quokka\Music\call.m4a's vector", "listening: ~\\…\\<file>.m4a's vector"),
            (r"file:///C:/Users/quokka/Videos/holiday.mp4 played", "file:///~\\…\\<file>.mp4 played"),
            (r"can't write C:\Users\quokka", "can't write ~"),
            (r"watching E:\ only", "watching E:\\ only"),
            (r"C:\notes.txt", "C:\\<file>.txt"),
            (r"(D:\Private Folder)", "(D:\\<name>)"),
            (r"D:\Archive\", "D:\\<name>\\"),
            (r"other user: C:\Users\quokkax\Documents\diary.md", "other user: C:\\…\\<file>.md"),
            ("nothing here: 12 files in 3.5 s, http://localhost:8080/x", "nothing here: 12 files in 3.5 s, http://localhost:8080/x"),
        ];
        for (line, expected) in cases {
            assert_eq!(redact(line, HOME), expected, "{line}");
        }
    }

    #[test]
    fn no_name_left() {
        // Whatever the path, no part of a folder's or a file's name is left.
        let line = r"index C:\Users\quokka\Documents\Divorce papers\letter to Sam.pdf: error; also D:\Secret\Thing one.tar.gz and F:\Money\bank";
        let shown = redact(line, HOME);
        for word in ["quokka", "Divorce", "papers", "letter", "Sam", "Secret", "Thing", "one", "Money", "bank", "tar"] {
            assert!(!shown.contains(word), "{word} in {shown}");
        }
        assert_eq!(shown, "index ~\\…\\<file>.pdf: error; also D:\\…\\<file>.gz and F:\\…\\<name>");
    }

    #[test]
    fn settings_without_paths() {
        let mut settings = Settings {
            folders: vec![r"C:\Users\quokka\Documents".into(), r"D:\Photos".into(), r"E:\Work".into()],
            exclude: vec!["node_modules".into(), r"C:\Users\quokka\Secret".into()],
            name_folders: vec![r"D:\Photos".into()],
            monitor: r"\\?\DISPLAY#DEL1234#5&abc".into(),
            seeing_note: r"Qwen didn't suit C:\Users\quokka\AppData\Local\Excavate\models".into(),
            ..Settings::default()
        };
        settings.folder_kinds.insert(r"d:\photos".into(), vec!["audio".into()]);
        let lines = settings_lines(&settings, HOME);
        let text = lines.join("\n");
        assert!(lines.contains(&"folders: 3".to_string()), "{text}");
        assert!(lines.contains(&"exclude: 2".to_string()), "{text}");
        assert!(lines.contains(&"name-folders: 1".to_string()), "{text}");
        assert!(lines.contains(&"folder-kinds: 1".to_string()), "{text}");
        assert!(lines.contains(&"monitor: chosen".to_string()), "{text}");
        assert!(lines.contains(&"own-index: false".to_string()) || lines.contains(&"own-index: true".to_string()), "{text}");
        for word in ["quokka", "Photos", "Work", "node_modules", "Secret", "DISPLAY", "Documents", "models"] {
            assert!(!text.contains(word), "{word} in {text}");
        }
    }

    #[test]
    fn logs_merged_by_time() {
        let excavate = "100.000 popup shown\n103.000 search\n".to_string();
        let indexer = "101.500 reading\nnot a timed line\n104.000 done\n".to_string();
        let lines = last_lines(&[("excavate", excavate), ("indexer", indexer)], 4);
        assert_eq!(lines, ["[indexer] 101.500 reading", "[indexer] not a timed line", "103.000 search", "[indexer] 104.000 done"]);
    }
}
