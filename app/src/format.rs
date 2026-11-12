//! How results read: "Today 2:02 PM", "1.2 MB", a long folder shortened in the middle, and the name with what matched in
//! the accent color (Slint's styled text: CommonMark with <font> tags, so names are escaped first).

use std::ops::Range;

/// A moment in a video: "0:42", "12:05", "1:02:03".
pub fn moment(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

/// Seconds since 1970 → local "Today 14:02" (or "Today 2:02 PM"), "Yesterday", "Mon", "Mar 3", "Mar 3, 2024".
pub fn when(seconds: i64, now: i64, twelve: bool) -> String {
    let (date, time) = local(seconds);
    let (today, _) = local(now);
    let days = days_between(date, today);
    match days {
        0 => format!("Today {}", config::clock(i64::from(time.0) * 60 + i64::from(time.1), twelve)),
        1 => "Yesterday".into(),
        2..=6 => WEEKDAYS[weekday(date)].into(),
        _ if date.0 == today.0 => format!("{} {}", MONTHS[date.1 as usize - 1], date.2),
        _ => format!("{} {}, {}", MONTHS[date.1 as usize - 1], date.2, date.0),
    }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

type Date = (i32, u32, u32);

/// A UTC timestamp as the local date and (hour, minute), with the local offset Windows reports.
fn local(seconds: i64) -> (Date, (u32, u32)) {
    let seconds = seconds + offset_seconds();
    let days = seconds.div_euclid(86_400);
    let in_day = seconds.rem_euclid(86_400);
    (civil(days), ((in_day / 3600) as u32, ((in_day % 3600) / 60) as u32))
}

fn offset_seconds() -> i64 {
    // Asked again each minute, not once a session: the offset changes when daylight saving starts or ends (or the time
    // zone is changed) while Excavate runs.
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static OFFSET: Mutex<Option<(Instant, i64)>> = Mutex::new(None);
    let mut kept = OFFSET.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match *kept {
        Some((asked, offset)) if asked.elapsed() < Duration::from_secs(60) => offset,
        _ => {
            let offset = winsys::process::utc_offset();
            *kept = Some((Instant::now(), offset));
            offset
        }
    }
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil_from_days).
fn civil(days: i64) -> Date {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

fn days_from_civil((year, month, day): Date) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_between(earlier: Date, later: Date) -> i64 {
    days_from_civil(later) - days_from_civil(earlier)
}

fn weekday(date: Date) -> usize {
    // 1970-01-01 was a Thursday (index 3 with Monday = 0).
    (days_from_civil(date) + 3).rem_euclid(7) as usize
}

/// How people read a model that looks at images, by `seeing::Engine::name`.
pub fn model_name(engine: &str) -> &'static str {
    match engine {
        "siglip2-b16" => "SigLIP 2",
        "qwen3vl-8b" => "Qwen3-VL-Embedding-8B",
        _ => "EmbeddingGemma 2",
    }
}

/// Bytes as Explorer shows them: "0 bytes", "812 bytes", "4.2 KB", "1.2 MB", "3.75 GB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let digits = if value < 10.0 { 2 } else if value < 100.0 { 1 } else { 0 };
    format!("{value:.digits$} {}", UNITS[unit])
}

/// A folder shown in at most `max` characters: the start and the end kept, the middle elided.
pub fn folder(path: &str, max: usize) -> String {
    let count = path.chars().count();
    if count <= max {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('\\').collect();
    if parts.len() > 3 {
        // Keep the drive and as many trailing folders as fit: "C:\…\Taxes\2026".
        let head = parts[0];
        let mut tail: Vec<&str> = Vec::new();
        let mut length = head.chars().count() + 2;
        for part in parts.iter().skip(1).rev() {
            let part_length = part.chars().count() + 1;
            if length + part_length > max && !tail.is_empty() {
                break;
            }
            length += part_length;
            tail.push(part);
        }
        tail.reverse();
        let shortened = format!("{head}\\…\\{}", tail.join("\\"));
        if shortened.chars().count() <= max + 8 {
            return shortened;
        }
    }
    let keep = max.saturating_sub(1) / 2;
    let start: String = path.chars().take(keep).collect();
    let end: String = path.chars().skip(count - keep).collect();
    format!("{start}…{end}")
}

/// The name with `highlights` (byte ranges) in `color`, as Slint styled text markup. Shortcut extensions are hidden
/// (".lnk", ".url"): "Visual Studio Code", as the Start menu shows it.
pub fn styled_name(name: &str, highlights: &[Range<usize>], color: &str) -> String {
    let shown = display_name(name);
    let mut out = String::with_capacity(shown.len() + 48);
    let mut position = 0;
    for range in highlights {
        let (start, end) = (range.start.min(shown.len()), range.end.min(shown.len()));
        if start >= end || start < position || !shown.is_char_boundary(start) || !shown.is_char_boundary(end) {
            continue;
        }
        escape_into(&mut out, &shown[position..start]);
        out.push_str("<font color=\"");
        out.push_str(color);
        out.push_str("\">**");
        escape_into(&mut out, &shown[start..end]);
        out.push_str("**</font>");
        position = end;
    }
    escape_into(&mut out, &shown[position..]);
    out
}

/// Any text with `highlights` (byte ranges) in `color`, not bold: for a result's second line (a snippet).
pub fn styled_plain(text: &str, highlights: &[Range<usize>], color: &str) -> String {
    let mut out = String::with_capacity(text.len() + 48);
    let mut position = 0;
    for range in highlights {
        let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
        if start >= end || start < position || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        escape_into(&mut out, &text[position..start]);
        out.push_str("<font color=\"");
        out.push_str(color);
        out.push_str("\">");
        escape_into(&mut out, &text[start..end]);
        out.push_str("</font>");
        position = end;
    }
    escape_into(&mut out, &text[position..]);
    out
}

pub fn display_name(name: &str) -> &str {
    for ext in [".lnk", ".url", ".appref-ms"] {
        // `get`, not indexing: the cut can land inside a multi-byte character ("Notes：" ends in a 3-byte one).
        let cut = name.len().saturating_sub(ext.len());
        if cut > 0 && name.get(cut..).is_some_and(|tail| tail.eq_ignore_ascii_case(ext)) {
            return &name[..cut];
        }
    }
    name
}

/// Every ASCII punctuation character backslash-escaped (CommonMark allows it for all of them), so a file name is
/// never read as markup; spaces kept as they are.
fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(812), "812 bytes");
        assert_eq!(size(4300), "4.20 KB");
        assert_eq!(size(1_250_000), "1.19 MB");
        assert_eq!(size(250_000_000), "238 MB");
        assert_eq!(size(4_000_000_000), "3.73 GB");
    }

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(days_from_civil((2026, 10, 29))), (2026, 10, 29));
        assert_eq!(WEEKDAYS[weekday((2026, 10, 29))], "Thu");
        let now = 1_793_260_800; // 2026-10-29 08:00 UTC
        assert!(when(now - 60, now, false).starts_with("Today") || when(now - 60, now, false) == "Yesterday");
        assert!(when(now - 400 * 86_400, now, false).ends_with("2025"));
        let today = when(now, now, true);
        assert!(today.starts_with("Today") && (today.ends_with("AM") || today.ends_with("PM")), "{today}");
    }

    #[test]
    fn folders() {
        assert_eq!(folder(r"C:\Users\me", 40), r"C:\Users\me");
        let long = r"C:\Users\me\Documents\Projects\Clients\Acme Corporation\Quarterly Reports\2026";
        let short = folder(long, 40);
        assert!(short.starts_with(r"C:\…\") && short.ends_with(r"Quarterly Reports\2026"), "{short}");
    }

    #[test]
    fn names() {
        assert_eq!(styled_name("report.pdf", &[0..3], "#f4a63a"), "<font color=\"#f4a63a\">**rep**</font>ort\\.pdf");
        assert_eq!(styled_name("Visual Studio Code.lnk", &[14..18], "#c"), "Visual Studio <font color=\"#c\">**Code**</font>");
        assert_eq!(styled_name("*weird*_[name].txt", &[], "#c"), "\\*weird\\*\\_\\[name\\]\\.txt");
        // Ranges past a hidden extension are dropped rather than breaking the markup.
        assert_eq!(styled_name("a.lnk", &[2..5], "#c"), "a");
        assert_eq!(display_name("Notes："), "Notes：");
        assert_eq!(display_name("会議.lnk"), "会議");
        for markup in [styled_name("1. intro # <b> `x` ~y~.md", &[0..2], "#123456"), styled_name("été.txt", &[0..5], "#aabbcc")] {
            assert!(slint::StyledText::from_markdown(&markup).is_ok(), "{markup}");
        }
    }
}
