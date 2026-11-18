//! Small things several of Excavate's crates need alike, kept once: dates, numbers with commas, writing a file whole,
//! and which files are pictures, videos and recordings. No dependencies, so any crate can use it.

use std::path::{Path, PathBuf};

/// The kinds of file Excavate reads besides text, by their (lowercase) extensions: the indexer reads them, the app
/// counts and shows them.
pub mod kinds {
    /// Pictures OCR reads (decoded by the `image` crate, or by Windows for HEIC).
    pub const PICTURES: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif"];

    /// Videos a model can look at frame by frame (what Windows' Media Foundation plays).
    pub const VIDEOS: &[&str] = &["mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm", "mpg", "mpeg", "3gp", "mts", "m2ts"];

    /// Recordings the model that listens can hear (what Windows' Media Foundation plays; Ogg and Opus with Windows' own
    /// extensions for them).
    pub const RECORDINGS: &[&str] = &["mp3", "m4a", "m4b", "wav", "wma", "flac", "aac", "ogg", "opus", "amr", "aiff", "aif"];
}

/// Days since 1970-01-01 for a date (Howard Hinnant's days_from_civil). `month` is 1 to 12.
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a day since 1970-01-01: (year, month, day) (Howard Hinnant's civil_from_days).
pub fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 { month_from_march + 3 } else { month_from_march - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// A whole number with commas between its thousands: 9218442 as "9,218,442".
pub fn grouped(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Writes a file whole or not at all (`<name>.tmp` beside it, then a rename over it), so a reader never sees half of
/// it, and a write that fails leaves the file as it was.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = temporary(path);
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

fn temporary(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_lowercase_and_apart() {
        let all: Vec<&str> = [kinds::PICTURES, kinds::VIDEOS, kinds::RECORDINGS].concat();
        assert!(all.iter().all(|extension| !extension.is_empty() && *extension == extension.to_ascii_lowercase() && !extension.contains('.')));
        let mut unique = all.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), all.len());
        assert!(kinds::PICTURES.contains(&"heic") && kinds::VIDEOS.contains(&"m2ts") && kinds::RECORDINGS.contains(&"opus"));
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28), 2);
        assert_eq!(days_from_civil(2023, 3, 1) - days_from_civil(2023, 2, 28), 1);
        assert_eq!(days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28), 1);
        // Every day from 1600 to 2400 there and back, one after the other.
        let first = days_from_civil(1600, 1, 1);
        let mut last = civil(first - 1);
        for days in first..days_from_civil(2400, 1, 1) {
            let date = civil(days);
            assert_eq!(days_from_civil(date.0, date.1, date.2), days);
            assert!(date > last && (1..=12).contains(&date.1) && (1..=31).contains(&date.2));
            last = date;
        }
    }

    #[test]
    fn files_written_whole() {
        let folder = std::env::temp_dir().join(format!("excavate-common-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("settings.json");
        assert_eq!(temporary(&file), folder.join("settings.json.tmp"));
        write_atomic(&file, b"first").unwrap();
        write_atomic(&file, b"second").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"second");
        assert!(!folder.join("settings.json.tmp").exists());
        // A folder that isn't there: an error, and nothing written.
        assert!(write_atomic(&folder.join("gone").join("x.json"), b"x").is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn commas() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(9_218_442), "9,218,442");
        assert_eq!(grouped(100_000), "100,000");
        assert_eq!(grouped(u64::MAX), "18,446,744,073,709,551,615");
    }
}
