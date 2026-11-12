//! Everything's functions that Excavate answers itself, from what it knows beside the names:
//!
//! - `content:` the words inside (the content index: documents, code, text in images, what's said);
//! - `width:`, `height:`, `dimensions:` an image's or a video's size in pixels (read from its header, once);
//! - `length:` a recording's or a video's length (`length:>10:00`, `length:<30s`);
//! - `runcount:` (`rc:`) and `daterun:` (`dr:`) how often and when a file was opened from Excavate.
//!
//! They're taken out of the text before the names are searched (`split`), and every result is checked against them in
//! `App::present`. Typed alone, they search every file of their kind (`width:>3000`: images and videos; `rc:>5`: what was
//! opened).

/// What the functions typed ask for. Empty: none were.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extra {
    pub content: Vec<String>,
    pub width: Option<(u64, u64)>,
    pub height: Option<(u64, u64)>,
    /// Seconds.
    pub length: Option<(f64, f64)>,
    pub runcount: Option<(u64, u64)>,
    /// Seconds since 1970.
    pub daterun: Option<(i64, i64)>,
}

impl Extra {
    pub fn is_empty(&self) -> bool {
        *self == Extra::default()
    }

    /// Whether a result's size in pixels is needed.
    pub fn needs_size(&self) -> bool {
        self.width.is_some() || self.height.is_some()
    }

    pub fn needs_length(&self) -> bool {
        self.length.is_some()
    }

    /// Whether a file with these facts fits (each None: not known, or not that kind of file, so it doesn't).
    pub fn fits(&self, size: Option<(u32, u32)>, length: Option<f64>, runs: Option<(u32, i64)>) -> bool {
        let within = |value: u64, range: (u64, u64)| value >= range.0 && value <= range.1;
        if let Some(range) = self.width {
            if !size.is_some_and(|size| within(u64::from(size.0), range)) {
                return false;
            }
        }
        if let Some(range) = self.height {
            if !size.is_some_and(|size| within(u64::from(size.1), range)) {
                return false;
            }
        }
        if let Some((from, to)) = self.length {
            if !length.is_some_and(|length| length >= from && length <= to) {
                return false;
            }
        }
        if let Some(range) = self.runcount {
            let count = runs.map_or(0, |runs| u64::from(runs.0));
            if !within(count, range) {
                return false;
            }
        }
        if let Some((from, to)) = self.daterun {
            if !runs.is_some_and(|runs| runs.1 >= from && runs.1 <= to) {
                return false;
            }
        }
        true
    }

    /// What to search the names for when nothing else was typed: the kinds of file the functions are about (None:
    /// what was opened, or the words inside, without a name search).
    pub fn kinds_alone(&self) -> Option<&'static str> {
        match (self.needs_size(), self.needs_length()) {
            (true, true) => Some("video:"),
            (true, false) => Some("pic: | video:"),
            (false, true) => Some("audio: | video:"),
            (false, false) => None,
        }
    }
}

/// The text without the functions answered here, and what they ask. A value can be in quotes (`content:"two words"`). A
/// function whose value doesn't read stays in the text (Everything's own error then).
pub fn split(text: &str) -> (String, Extra) {
    let mut extra = Extra::default();
    let mut rest: Vec<String> = Vec::new();
    for token in tokens(text) {
        let Some((name, value)) = token.split_once(':') else {
            rest.push(token);
            continue;
        };
        let value = value.trim_matches('"');
        let taken = match name.to_ascii_lowercase().as_str() {
            "content" | "utf8content" if !value.is_empty() => {
                extra.content.extend(value.split_whitespace().map(str::to_lowercase));
                true
            }
            "width" => numbers(value).map(|range| extra.width = Some(range)).is_some(),
            "height" => numbers(value).map(|range| extra.height = Some(range)).is_some(),
            "dimensions" | "dimension" => dimensions(value).map(|(width, height)| {
                extra.width = Some(width);
                extra.height = Some(height);
            }).is_some(),
            "length" => durations(value).map(|range| extra.length = Some(range)).is_some(),
            "runcount" | "rc" => numbers(value).map(|range| extra.runcount = Some(range)).is_some(),
            "daterun" | "dr" => engine::syntax::dates(value, engine::when::now(), engine::when::utc_offset())
                .map(|range| extra.daterun = Some(range)).is_some(),
            _ => false,
        };
        if !taken {
            rest.push(token);
        }
    }
    (rest.join(" "), extra)
}

/// Words, a quoted part kept whole with its quotes ("content:\"a b\"").
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
            current.push(c);
        } else if c.is_whitespace() && !quoted {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// A range of whole numbers, both ends included: `>1920`, `<=500`, `1000..2000`, `1920`.
fn numbers(value: &str) -> Option<(u64, u64)> {
    range(value, |text| text.trim().parse::<u64>().ok().map(|n| n as f64)).map(|(from, to)| (from as u64, to.min(u64::MAX as f64) as u64))
}

/// `dimensions:1920x1080`, or `>1920x1080` (each side at least so).
fn dimensions(value: &str) -> Option<((u64, u64), (u64, u64))> {
    let (op, sizes) = match value.find(|c: char| c.is_ascii_digit()) {
        Some(at) => value.split_at(at),
        None => return None,
    };
    let (width, height) = sizes.split_once(['x', 'X', '*'])?;
    Some((numbers(&format!("{op}{width}"))?, numbers(&format!("{op}{height}"))?))
}

/// A range of lengths in seconds: `>10:00`, `<30s`, `1:00:00..2:00:00`, `5min`, `90` (seconds).
fn durations(value: &str) -> Option<(f64, f64)> {
    range(value, seconds)
}

fn seconds(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_lowercase();
    if text.contains(':') {
        let mut total = 0.0;
        for part in text.split(':') {
            total = total * 60.0 + part.parse::<f64>().ok()?;
        }
        return Some(total);
    }
    let digits = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let number: f64 = text[..digits].parse().ok()?;
    let scale = match text[digits..].trim() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
        _ => return None,
    };
    Some(number * scale)
}

/// `>a`, `>=a`, `<a`, `<=a`, `=a`, `a..b` (either end may be left out), or `a`, as an inclusive range.
fn range(value: &str, one: impl Fn(&str) -> Option<f64>) -> Option<(f64, f64)> {
    let value = value.trim();
    if let Some((a, b)) = value.split_once("..") {
        let from = if a.trim().is_empty() { 0.0 } else { one(a)? };
        let to = if b.trim().is_empty() { f64::MAX } else { one(b)? };
        return Some((from, to));
    }
    // (A whole number's neighbours for > and <; a length's is a hundredth of a second.)
    let step = |n: f64| if n.fract() == 0.0 { 1.0 } else { 0.01 };
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = value.strip_prefix(op) {
            let n = one(rest)?;
            return Some(match op {
                ">=" => (n, f64::MAX),
                "<=" => (0.0, n),
                ">" => (n + step(n), f64::MAX),
                "<" => (0.0, (n - step(n)).max(0.0)),
                _ => (n, n),
            });
        }
    }
    let n = one(value)?;
    Some((n, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every function and modifier the engine knows is read as one, not searched as text ("sort:size", "files:").
    #[test]
    fn the_app_knows_the_engines_functions() {
        for name in engine::syntax::FUNCTIONS.iter().chain(engine::syntax::MODIFIERS) {
            assert!(search::query::is_function(&format!("{name}:x")), "{name}: isn't read as a function");
        }
    }

    #[test]
    fn taken_out_of_the_text() {
        let (rest, extra) = split("budget content:invoice ext:pdf");
        assert_eq!(rest, "budget ext:pdf");
        assert_eq!(extra.content, ["invoice"]);
        let (rest, extra) = split("width:>1920 height:1000..2000");
        assert_eq!(rest, "");
        assert_eq!(extra.width, Some((1921, u64::MAX)));
        assert_eq!(extra.height, Some((1000, 2000)));
        let (_, extra) = split("dimensions:1920x1080");
        assert_eq!((extra.width, extra.height), (Some((1920, 1920)), Some((1080, 1080))));
        let (rest, extra) = split("content:\"quarterly report\" notes");
        assert_eq!(rest, "notes");
        assert_eq!(extra.content, ["quarterly", "report"]);
        // A value that doesn't read stays (Everything's own error).
        let (rest, extra) = split("width:big");
        assert_eq!(rest, "width:big");
        assert!(extra.is_empty());
    }

    #[test]
    fn lengths_and_runs() {
        assert_eq!(split("length:>10:00").1.length, Some((601.0, f64::MAX)));
        assert_eq!(split("length:<30s").1.length, Some((0.0, 29.0)));
        assert_eq!(split("length:5min..1h").1.length, Some((300.0, 3600.0)));
        assert_eq!(split("rc:>5").1.runcount, Some((6, u64::MAX)));
        assert_eq!(split("runcount:0").1.runcount, Some((0, 0)));
        assert!(split("dr:today").1.daterun.is_some());
    }

    #[test]
    fn checks() {
        let (_, extra) = split("width:>=1920 rc:>=2");
        assert!(extra.fits(Some((1920, 1080)), None, Some((2, 0))));
        assert!(!extra.fits(Some((1280, 720)), None, Some((2, 0))));
        assert!(!extra.fits(None, None, Some((5, 0))));
        assert!(!extra.fits(Some((4000, 3000)), None, None));
        // Never opened: run count 0.
        assert!(split("rc:0").1.fits(None, None, None));
        assert_eq!(split("length:>1m").1.kinds_alone(), Some("audio: | video:"));
    }
}
