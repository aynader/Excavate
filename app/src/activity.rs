//! Activity: how reading is going, kind by kind, as one ready-made view built off the UI thread.
//!
//! A worker thread ("activity") reads status.json and settings.json about every half second while Settings is open
//! (every ten seconds while it isn't, to keep each lane's pace for the sparklines and a view ready for the next
//! opening), measures the index's size every half minute, and builds the view (`build`: pure, the status in, the view
//! out). It hands the UI thread a `Snapshot` only when something in it changed; the UI thread applies it
//! (`setup::apply_snapshot`), touching only what differs. Before, the window's one-second timer did all of this on
//! the UI thread: 0.26 to 0.76 s a tick on a PC busy reading, during which nothing on screen answered.
//!
//! What the person does here shows at once (`Marks`: Skip, Leave it, an action asked; the settings are saved as they
//! change): the UI thread changes what's on screen itself, and the indexer's own state follows a moment later.

use common::grouped as group;
use config::Settings;
use config::status::{Pace, Status};
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// An item counts as stuck once nothing of it has moved for this long (seconds); as one long file once it has taken
/// this long, while it still moves.
pub const STUCK_AFTER: i64 = 180;
/// A kind with work, no file in hand and nothing finished for this long: "Not moving" (a reader that ended without a
/// word once left documents "Working" for hours).
pub const NOT_MOVING_AFTER: i64 = 600;
pub const SLOW_AFTER: i64 = 60;
/// The pace samples a sparkline shows, one about every 15 s of a lane's work (the indexer's own sample).
pub const SPARK: usize = 30;
/// How often the view is built while Settings is open, and while it isn't.
const OPEN_EVERY: Duration = Duration::from_millis(500);
const HIDDEN_EVERY: Duration = Duration::from_secs(10);
/// The index's size is measured this often (it walks the index's folder).
const INDEX_EVERY: Duration = Duration::from_secs(30);
const DAY_MS: f64 = 86_400_000.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// "Reading documents and images", "Paused until 4:30 PM", "Up to date".
    pub title: String,
    /// "2 h 50 – 3 h 30 left", "more than a day left", or nothing.
    pub left: String,
    /// "30,405 of 145,823 files · 418 files a minute · last finished 0 s ago".
    pub counts: String,
    pub beat: Beat,
    /// When an item was last finished, any kind (seconds since 1970): the heartbeat pulses when it changes.
    pub last_finished: i64,
    /// 0 to 1, below 0 with no measure yet.
    pub fraction: f32,
    /// "Longest: documents and code (about 100,000 left). Using 24 of 32 processor threads".
    pub longest: String,
    pub kinds: Vec<Kind>,
    pub stuck: Option<Stuck>,
    pub problems: Vec<Group>,
    pub problems_total: u64,
    pub paused_all: bool,
    /// The speed control's stop: its place in `config::Speed::ALL`.
    pub speed: usize,
    /// Reading has work (the speed and the pause matter).
    pub active: bool,
    /// Bedrock's card (`bedrock`), when there's one to show.
    pub bedrock: Option<Bedrock>,
    /// What was read today (`today`), when anything was.
    pub today: Option<Today>,
    /// What's wrong with a kind's reader, each with what can be done about it (`fixes`).
    pub fixes: Vec<Fix>,
}

/// A problem card: a reader that can't start or whose model couldn't be downloaded, and its buttons (label, what it
/// does: "retry" restarts reading, "images-cpu" and "listen-cpu" move that reader to the processor).
#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub title: String,
    pub line: String,
    pub actions: Vec<(&'static str, &'static str)>,
}

/// The problem cards the status calls for. Each reader tries again by itself in a while; the card lets it be now, or
/// moved off the graphics card when that's where it fails.
fn fixes(status: &Status, settings: &Settings) -> Vec<Fix> {
    let mut fixes = Vec::new();
    let mut card = |what: &str, model: &config::status::Model, off_card: Option<&'static str>| {
        if model.state != "failed" {
            return;
        }
        let downloading = model.total > 0 || model.error.to_lowercase().contains("download");
        let title = if downloading { format!("{what} couldn't download its model") } else { format!("{what} couldn't start") };
        let reason = short_error(&model.error);
        let line = if reason.is_empty() { "It tries again by itself in a while.".to_string() } else { format!("{reason}. It tries again by itself in a while.") };
        let mut actions = vec![("Try again now", "retry")];
        if let Some(action) = off_card.filter(|_| !downloading) {
            actions.push(("Run on the processor", action));
        }
        fixes.push(Fix { title, line, actions });
    };
    card(if status.model.name == "the thorough reader" { "Thorough reading" } else { "Text in images" }, &status.model,
         status.reading_gpu.then_some("images-cpu"));
    for looking in &status.looking {
        card("What images show", &looking.model, None);
    }
    if let Some(listening) = &status.listening {
        let on_card = !listening.device.is_empty() && !listening.device.eq_ignore_ascii_case("cpu")
            && settings.listening_devices.get(&settings.listening).is_none_or(|device| device != "cpu");
        card("Recordings", &listening.model, on_card.then_some("listen-cpu"));
    }
    fixes
}

/// An error's first line, without what only a developer reads, at most about a line long.
fn short_error(error: &str) -> String {
    let first = error.lines().next().unwrap_or("").trim().trim_end_matches('.');
    let mut short: String = first.chars().take(110).collect();
    if first.chars().count() > 110 {
        short.push('…');
    }
    let mut chars = short.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Activity's Files read card: today by kind, and the choices of its menu (`files_read`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Today {
    /// "Documents 902 · Text in images 344 · What images show 344".
    pub parts: String,
    /// Today, the past week, month and year.
    pub spans: Vec<crate::files_read::Span>,
}

/// What was done since midnight, for Today: each thing's part and when.
type Done = Vec<(&'static str, i64)>;

/// What was done since `midnight` (seconds since 1970), from the catalog (read-only): files read, and what was looked at
/// or heard.
fn done_today(catalog: &std::path::Path, midnight: i64) -> Option<Done> {
    use rusqlite::OpenFlags;
    if !catalog.exists() {
        return None;
    }
    let db = rusqlite::Connection::open_with_flags(catalog, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let mut done: Vec<(&'static str, i64)> = Vec::new();
    // Read: its words (documents, code) or the text in it (images); failures aren't counted.
    if let Ok(mut statement) = db.prepare("SELECT kind, read_at FROM files WHERE read_at >= ?1 AND state IN (1, 3)") {
        let rows = statement.query_map([midnight], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)));
        for (kind, at) in rows.into_iter().flatten().flatten() {
            done.push((if kind == content::kind::PICTURE { "Text in images" } else { "Documents" }, at));
        }
    }
    // Looked at, listened to, or read again by the thorough reader.
    if let Ok(mut statement) = db.prepare("SELECT engine, at FROM seen WHERE at >= ?1 AND ok = 1") {
        let rows = statement.query_map([midnight], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)));
        for (engine, at) in rows.into_iter().flatten().flatten() {
            done.push((today_part(&engine), at));
        }
    }
    Some(done)
}

/// Whether a part of Today counts as a file read (the rest was seen or heard).
fn is_read(part: &str) -> bool {
    matches!(part, "Documents" | "Text in images")
}

/// What a `seen` engine's work is called on Today.
fn today_part(engine: &str) -> &'static str {
    if engine == "video-text" {
        "Text in videos"
    } else if engine.starts_with("said-") {
        "Said in videos"
    } else if engine.contains("parakeet") || engine.contains("whisper") {
        "Recordings"
    } else if engine.starts_with("paddleocr-vl") {
        "Thorough reads"
    } else {
        "What images show"
    }
}

/// The card from what was done today and the past year's days; None when nothing was read in the past year.
fn summarize_today(done: &[(&'static str, i64)], midnight: i64, days: &crate::files_read::Days, today: i64, twelve: bool) -> Option<Today> {
    let mut counts: Vec<(&'static str, u64)> = Vec::new();
    let mut hours = [(0u64, 0u64); 24];
    for &(part, at) in done {
        match counts.iter_mut().find(|(name, _)| *name == part) {
            Some((_, count)) => *count += 1,
            None => counts.push((part, 1)),
        }
        let hour = &mut hours[((at - midnight).max(0) / 3600).min(23) as usize];
        if is_read(part) { hour.0 += 1 } else { hour.1 += 1 }
    }
    counts.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let spans = crate::files_read::spans(days, &hours, today, twelve);
    if spans.is_empty() {
        return None;
    }
    Some(Today { parts: counts.iter().map(|(part, count)| format!("{part} {}", group(*count))).collect::<Vec<_>>().join(" · "), spans })
}

/// Activity's Bedrock card: a drive listed whole again, now or last, and "Fix this" when its record of changes was too
/// small for the time Excavate was closed.
#[derive(Clone, Debug, PartialEq)]
pub struct Bedrock {
    pub title: String,
    pub line: String,
    /// 0 to 1, below 0 when done.
    pub progress: f32,
    pub count: String,
    pub fix: bool,
    /// What "Fix this" asks first.
    pub ask: String,
}

/// Bedrock's card as it is now: what the own index says, and how large the drive's record of changes is (a question
/// to the drive: off the UI thread, here). A sandbox shows it as it looks a third of the way through C:
/// (`EXCAVATE_PRETEND_RELIST`).
pub fn bedrock() -> Option<Bedrock> {
    enum Shown {
        Now { drive: String, done: usize, total: usize, overwritten: bool },
        Done { drive: String, took: Duration, overwritten: bool },
    }
    let shown = if std::env::var_os("EXCAVATE_PRETEND_RELIST").is_some() {
        Shown::Now { drive: r"C:\".into(), done: 3_100_000, total: 9_100_000, overwritten: true }
    } else {
        let state = crate::own_index::state()?;
        match (&state.relisting, &state.relisted) {
            (Some(now), _) => Shown::Now { drive: now.drive.clone(), done: engine::walk::listed().saturating_sub(now.from), total: now.total, overwritten: now.overwritten },
            (None, Some(last)) => Shown::Done { drive: last.drive.clone(), took: last.took, overwritten: last.overwritten },
            _ => return None,
        }
    };
    let (drive, overwritten) = match &shown {
        Shown::Now { drive, overwritten, .. } | Shown::Done { drive, overwritten, .. } => (drive.clone(), *overwritten),
    };
    let letter = drive.trim_end_matches('\\').to_string();
    let size = engine::journal::size(&drive).unwrap_or(0);
    let fixed = crate::own_index::journal_fixed(&drive);
    let why = if overwritten {
        format!("Windows' record of changes on {letter} ran out while Excavate was closed")
    } else {
        format!("Windows couldn't say what changed on {letter} while Excavate was closed")
    };
    let ask = format!(
        "Windows keeps a record of what changes on each drive, so Bedrock catches up in a moment after Excavate was closed. On {letter} it holds about {} MB, and it ran out.",
        (size >> 20).max(1)
    );
    let fix = overwritten && !fixed && size < crate::own_index::JOURNAL_SIZE / 2;
    Some(match shown {
        Shown::Now { done, total, .. } => {
            let done = done.min(total);
            let count = if total >= 1_000_000 {
                format!("{:.1} of {:.1} million files", done as f64 / 1e6, total as f64 / 1e6)
            } else {
                format!("{} of {}", group(done as u64), plural(total as u64, "file", "files"))
            };
            Bedrock {
                title: format!("Bedrock is catching up on {letter}"),
                line: format!("{why}, so {letter} is listed again. Search works meanwhile."),
                progress: if total > 0 { done as f32 / total as f32 } else { -1.0 },
                count: format!("{count} · going gently"),
                fix,
                ask,
            }
        }
        Shown::Done { took, .. } => {
            let minutes = took.as_secs_f64() / 60.0;
            let took = if minutes >= 1.5 { format!("about {} minutes", minutes.round()) } else { format!("{} seconds", took.as_secs()) };
            Bedrock {
                title: format!("Bedrock caught up on {letter}"),
                line: if fixed {
                    format!("It took {took}. Windows now keeps about 1 GB of changes on {letter}, so this shouldn't happen again.")
                } else {
                    format!("It took {took}, because {why}.")
                },
                progress: -1.0,
                count: String::new(),
                fix,
                ask,
            }
        }
    })
}

/// The heartbeat: still (grey: nothing to do, or paused), alive (working), or quiet (amber: working, but nothing has
/// finished for a while).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Beat {
    #[default]
    Still,
    Alive,
    Quiet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    Slow,
    Done,
    Waiting,
    Paused,
}

impl State {
    /// For the page: 0 working, 1 slow, 2 done, 3 waiting, 4 paused.
    pub fn number(self) -> i32 {
        match self {
            State::Working => 0,
            State::Slow => 1,
            State::Done => 2,
            State::Waiting => 3,
            State::Paused => 4,
        }
    }
}

/// A kind of reading, a row on the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Kind {
    /// Its row: "documents", "images", "later", "deep", "video-text", "shown", "videos-shown", "meaning",
    /// "recordings", "videos-said".
    pub id: &'static str,
    /// What its pause button pauses (`config::KINDS`).
    pub pause: &'static str,
    pub name: &'static str,
    pub state: State,
    /// "Working", "Slow: one long file", "Done", "Waiting", "Waiting: after the first sweep", "Paused".
    pub chip: String,
    /// "27,807 of 127,775".
    pub count: String,
    /// Its time left: "2 h 50 – 3 h 30", "about 2 min", or nothing.
    pub left: String,
    pub fraction: f32,
    /// Its pace lately, as a path's commands (`spark`); empty when there's none to show.
    pub spark: String,
    /// "Now: Lecture notes week 9.pdf (page 14 of 40) · 6 at once on the processor · 380 a minute", or why it waits.
    pub line: String,
    /// The file Skip lets go (empty: no Skip).
    pub skip: String,
    pub can_pause: bool,
}

/// An item that hasn't moved for `STUCK_AFTER`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stuck {
    /// "Stuck: Scan 2019.pdf hasn't moved for 3 min (page 2 of 1,200)".
    pub text: String,
    pub path: String,
    /// When the item was started: "Leave it" leaves this one, not the next file.
    pub since: i64,
    pub can_skip: bool,
    /// Not a file but a kind not moving: "Restart reading".
    pub can_restart: bool,
}

/// A group of what couldn't be read.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub why: &'static str,
    pub label: &'static str,
    pub count: u64,
    /// "Download and read", "Try again", "Read again", or nothing.
    pub action: String,
    /// Asked, and not done yet: what the action says meanwhile ("Downloading…").
    pub asked: bool,
    /// (name, path, what was said) of its latest files.
    pub files: Vec<(String, String, String)>,
}

/// What the person did on the page that the view keeps to: items left alone when stuck ("Leave it"), files Skip asked
/// to let go, and groups asked to be read again (with their count then). Changed on the UI thread, read by the worker.
#[derive(Clone, Debug, Default)]
pub struct Marks {
    pub left_alone: Vec<(String, i64)>,
    pub skipped: Vec<String>,
    pub asked: HashMap<String, (u64, i64)>,
    /// Counts every change of these and of the settings: a view built before one isn't applied.
    pub generation: u64,
}

/// What `build` needs besides the status.
pub struct Inputs<'a> {
    pub status: Option<&'a Status>,
    pub settings: &'a Settings,
    pub now: i64,
    pub history: &'a History,
    pub marks: &'a Marks,
    /// What the pause says now ("Paused until 4:30 PM"), as the settings and the clock have it.
    pub pause_text: String,
}

// ---- The time left ---------------------------------------------------------------------------------------------------

/// How long what's left takes, as a range from how steady the pace is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Eta {
    /// Not known yet (or nothing left).
    Unknown,
    /// Between these (ms).
    Range(f64, f64),
    /// A day or more: the pace is tiny, or it's been on one item for a minute without a pace at all.
    Long,
}

/// `left` items at this lane's pace: between its pace lately and over the last quarter of an hour, a tenth either way
/// besides, so it's a range even when the pace is steady.
pub fn eta(left: u64, pace: Option<&Pace>, now: i64) -> Eta {
    if left == 0 {
        return Eta::Unknown;
    }
    let Some(pace) = pace.filter(|pace| pace.per_minute > 0.0) else {
        // On an item for a minute, and nothing finished yet: no telling, but not soon.
        let stalled = pace.is_some_and(|pace| pace.current_since > 0 && now - pace.current_since >= SLOW_AFTER && pace.last_progress == 0);
        return if stalled { Eta::Long } else { Eta::Unknown };
    };
    let long = if pace.per_minute_long > 0.0 { pace.per_minute_long } else { pace.per_minute };
    let (fast, slow) = (pace.per_minute.max(long), pace.per_minute.min(long));
    let ms = |per_minute: f64| left as f64 / per_minute * 60_000.0;
    let (low, high) = (ms(fast) * 0.9, ms(slow) * 1.1);
    if low >= DAY_MS { Eta::Long } else { Eta::Range(low, high) }
}

/// "2 h 50 – 3 h 30", "20 – 40 min", "about 2 min", "less than a minute", "18 h or more", "more than a day".
pub fn eta_text(eta: Eta) -> String {
    match eta {
        Eta::Unknown => String::new(),
        Eta::Long => "more than a day".into(),
        Eta::Range(low, high) if high > DAY_MS => format!("{} or more", one_time(low, low)),
        Eta::Range(low, high) => {
            let (low_text, high_text) = (one_time(low, high), one_time(high, high));
            if high < 60_000.0 {
                "less than a minute".into()
            } else if low_text == high_text {
                format!("about {high_text}")
            } else if high < 90.0 * 60_000.0 {
                // "20 – 40 min": the unit once.
                format!("{} – {high_text}", low_text.trim_end_matches(" min"))
            } else {
                format!("{low_text} – {high_text}")
            }
        }
    }
}

/// One end of a range, in the unit the longer end needs: minutes (to the minute under 10, to 5 under an hour, to 10
/// above), or hours and minutes to 10 ("2 h 50", "3 h").
fn one_time(ms: f64, longest: f64) -> String {
    let minutes = ms / 60_000.0;
    let round = |minutes: f64, step: f64| ((minutes / step).round() * step).max(step.min(1.0)) as u64;
    if longest < 90.0 * 60_000.0 {
        let step = if longest < 10.0 * 60_000.0 { 1.0 } else { 5.0 };
        format!("{} min", round(minutes, step).max(1))
    } else {
        let rounded = round(minutes, 10.0);
        let (hours, rest) = (rounded / 60, rounded % 60);
        match (hours, rest) {
            (0, rest) => format!("{rest} min"),
            (hours, 0) => format!("{hours} h"),
            (hours, rest) => format!("{hours} h {rest}"),
        }
    }
}

/// "about 100,000", "about 2,600", "640": a count left, rounded to two figures from a thousand up.
pub fn about(count: u64) -> String {
    if count < 1000 {
        return group(count);
    }
    let digits = count.ilog10();
    let unit = 10u64.pow(digits - 1);
    format!("about {}", group((count + unit / 2) / unit * unit))
}

/// "4 s", "3 min", "2 h".
fn ago(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds < 3600 {
        format!("{} min", seconds / 60)
    } else {
        format!("{} h", seconds / 3600)
    }
}

/// "2 h 14 min", "38 min", "45 s": how long a recording is.
fn length(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{} h {} min", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    }
}

fn plural(count: u64, one: &str, many: &str) -> String {
    format!("{} {}", group(count), if count == 1 { one } else { many })
}

fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

fn is_video(path: &str) -> bool {
    search::filter::extension(path).is_some_and(|extension| search::filter::VIDEO.contains(&extension.to_ascii_lowercase().as_str()))
}

// ---- The pace's history, for the sparklines ----------------------------------------------------------------------------

/// Each lane's pace lately, a sample about every 15 s (when the indexer's pace changes, or a lane goes quiet).
#[derive(Clone, Debug, Default)]
pub struct History {
    lanes: HashMap<String, VecDeque<f32>>,
    /// The pace last taken, and when.
    last: HashMap<String, (f64, i64)>,
}

impl History {
    pub fn sample(&mut self, status: &Status, now: i64) {
        for (lane, pace) in &status.paces {
            // Working now (an item in hand, or one finished in the last half minute): its pace; else nothing moving.
            let working = pace.current_since > 0 || now - pace.last_progress < 30;
            let value = if working { pace.per_minute } else { 0.0 };
            let due = match self.last.get(lane) {
                None => true,
                Some(&(last, at)) => (value != last && now - at >= 5) || now - at >= 15,
            };
            if due {
                self.last.insert(lane.clone(), (value, now));
                let samples = self.lanes.entry(lane.clone()).or_default();
                samples.push_back(value as f32);
                while samples.len() > SPARK {
                    samples.pop_front();
                }
            }
        }
    }

    /// The lane's samples as bars, for a `Path` with a viewbox of `SPARK * 3` by 14 (the newest on the right); empty
    /// without two samples to compare.
    pub fn spark(&self, lane: &str) -> String {
        let Some(samples) = self.lanes.get(lane).filter(|samples| samples.len() >= 2) else { return String::new() };
        let most = samples.iter().copied().fold(0.0f32, f32::max);
        if most <= 0.0 {
            return String::new();
        }
        let offset = SPARK - samples.len();
        let mut commands = String::new();
        for (index, value) in samples.iter().enumerate() {
            let height = if *value > 0.0 { (value / most * 13.0).max(1.5) } else { 0.75 };
            let x = (offset + index) * 3;
            commands.push_str(&format!("M{x} 14v{:.1}h2v{:.1}z", -height, height));
        }
        commands
    }
}

// ---- Building the view ---------------------------------------------------------------------------------------------------

/// Why a kind waits, from the indexer's note: it waits ("Waiting for a quiet moment: …", "Paused on battery power"),
/// or it goes on at a slower pace ("Going slower: …"), or not at all.
fn waits(note: &str) -> bool {
    !note.is_empty() && !note.starts_with("Going slower")
}

/// A row before what's in hand is known: its counts, and why it waits if it does.
struct Row {
    id: &'static str,
    pause: &'static str,
    name: &'static str,
    /// The lane whose pace it goes at (`config::status::LANES`), if any.
    lane: Option<&'static str>,
    done: u64,
    total: u64,
    left: u64,
    /// Why it waits (or what it does instead, "Getting the image reader: …"), and whether that's waiting.
    note: String,
    waiting: bool,
    /// The chip while it waits, when not plain "Waiting".
    chip: Option<&'static str>,
    /// What it's on now when its lane doesn't say (text in videos: the video).
    item: String,
    /// Where it runs, for "6 at once on the processor".
    gpu: Option<bool>,
    /// Skip works on what it's on.
    skippable: bool,
    /// Its count instead of "done of total".
    count: Option<String>,
}

impl Row {
    fn new(id: &'static str, pause: &'static str, name: &'static str, lane: Option<&'static str>, done: u64, total: u64, left: u64) -> Row {
        Row { id, pause, name, lane, done, total, left, note: String::new(), waiting: false, chip: None, item: String::new(), gpu: None, skippable: false, count: None }
    }

    fn waits(mut self, note: impl Into<String>) -> Row {
        self.note = note.into();
        self.waiting = true;
        self
    }

    /// The indexer's note for its lane: waiting, or going (slower or not).
    fn noted(mut self, note: &str) -> Row {
        if !note.is_empty() {
            self.waiting = waits(note);
            self.note = note.to_string();
        }
        self
    }
}

fn schedule_note(at: i64, page: &str) -> String {
    if at > 0 { "Waiting for the next scheduled time".to_string() } else { format!("Waiting until you start it, under {page}") }
}

/// The rows there are, from the status: each kind that's on, in the order reading goes.
fn rows(status: &Status, media_missing: bool) -> Vec<Row> {
    let listing = status.state == "listing";
    let later = status.queued_later.min(status.queued_pictures);
    let waiting = if status.pictures_wait { status.queued_pictures } else { later };
    let images_left = status.queued_pictures - waiting;
    let model = &status.model;
    let getting = |name: &str| (model.state == "downloading" && model.name == name && model.total > 0)
        .then(|| format!("Getting {name}: {} of {}", crate::format::size(model.received), crate::format::size(model.total)));
    let mut rows = Vec::new();

    let documents = status.files.saturating_sub(status.images);
    let documents_done = (status.read + status.failed).saturating_sub(status.images_read).min(documents);
    let mut row = Row::new("documents", "documents", "Documents and code", Some("documents"), if status.queued_text == 0 && !listing { documents } else { documents_done },
                           documents, status.queued_text);
    row.gpu = Some(false);
    // (Each document is read in a process of its own, which Skip ends: `documents` in the indexer.)
    row.skippable = true;
    rows.push(if listing { row.waits("Once your folders have been looked through") } else { row.noted(&status.waits[0]) });

    if !status.engine.is_empty() {
        let images = status.images.saturating_sub(waiting);
        let mut row = Row::new("images", "images", "Text in images", Some("images"), status.images_read.min(images), images, images_left);
        row.gpu = Some(status.reading_gpu);
        // Scanned PDFs' pages are read here too, found as documents are read: not among the images counted.
        if status.queued_scanned > 0 {
            row.count = Some(format!("{} of {} · {} to go", group(row.done.min(row.total)), group(row.total),
                                     plural(status.queued_scanned, "scanned PDF", "scanned PDFs")));
        }
        // (Skip works between a scanned PDF's pages: `build` offers it only then.)
        row.skippable = true;
        row = if listing {
            row.waits("Once your folders have been looked through")
        } else if status.pictures_wait && status.queued_pictures > 0 {
            row.waits(schedule_note(status.pictures_at, "Images"))
        } else if let Some(note) = getting("the image reader") {
            Row { note, ..row }
        } else {
            row.noted(&status.waits[1])
        };
        rows.push(row);
    }
    if later > 0 && !status.pictures_wait {
        let mut row = Row::new("later", "images", "Set aside for later", Some("images"), 0, 0, later);
        row.count = Some(String::new());
        row = if images_left > 0 {
            Row { chip: Some("Waiting: after the first sweep"), ..row.waits(format!("{} · read once the first sweep is done", plural(later, "near-duplicate image", "near-duplicate images"))) }
        } else {
            row.noted(&status.waits[1])
        };
        rows.push(row);
    }
    if status.deep {
        let all = status.deep_done + status.deep_queued;
        let mut row = Row::new("deep", "deep", "Thorough read", Some("deep"), status.deep_done, all, status.deep_queued);
        row.gpu = Some(true);
        row = if let Some(note) = getting("the thorough reader") {
            Row { note, ..row }
        } else if model.state == "failed" && model.name == "the thorough reader" {
            row.waits(format!("Couldn't start the thorough reader ({}). Trying again in a while.", model.error))
        } else if status.pictures_wait && status.deep_queued > 0 {
            row.waits(schedule_note(status.pictures_at, "Images"))
        } else if status.deep_queued > 0 && !status.waits[2].is_empty() {
            row.noted(&status.waits[2])
        } else if status.deep_queued == 0 && (listing || images_left > 0) {
            row.waits("Reads again those with text, after the first read")
        } else {
            row
        };
        rows.push(row);
    }
    if let Some(reading) = status.video_text.as_ref().filter(|reading| reading.done + reading.queued > 0) {
        let mut row = Row::new("video-text", "images", "Text in videos", None, reading.done, reading.done + reading.queued, reading.queued);
        row.item = reading.current.clone();
        row.skippable = true;
        row.gpu = Some(status.reading_gpu);
        row = if status.pictures_wait {
            row.waits(schedule_note(status.pictures_at, "Images"))
        } else if status.queued_pictures > 0 {
            row.waits("After the images")
        } else {
            row.noted(&status.waits[1])
        };
        rows.push(row);
    }
    if let Some(looking) = status.looking.iter().max_by_key(|looking| looking.queued) {
        let name = crate::format::model_name(&looking.engine);
        let total = looking.done + looking.queued;
        let mut row = Row::new("shown", "shown", "What images show", Some("shown"), looking.done, total, looking.queued);
        row.skippable = true;
        let model = &looking.model;
        row = if listing {
            row.waits("Once your folders have been looked through")
        } else if model.state == "downloading" && model.total > 0 {
            Row { note: format!("Getting {name}: {} of {}", crate::format::size(model.received), crate::format::size(model.total)), ..row }
        } else if model.state == "failed" {
            row.waits(format!("{name} couldn't be downloaded, trying again soon"))
        } else if status.seeing_wait && looking.queued > 0 {
            row.waits(schedule_note(status.seeing_at, "Images"))
        } else if total == 0 {
            row.waits("Once their text has been read")
        } else {
            row.noted(&looking.waiting)
        };
        rows.push(row);
    }
    if let Some(looking) = status.looking.iter().filter(|looking| looking.videos_done + looking.videos_queued > 0).max_by_key(|looking| looking.videos_queued) {
        let all = looking.videos_done + looking.videos_queued;
        let mut row = Row::new("videos-shown", "shown", "What videos show", Some("shown"), looking.videos_done, all, looking.videos_queued);
        row.skippable = true;
        row = if status.seeing_wait {
            row.waits(schedule_note(status.seeing_at, "Videos"))
        } else if looking.queued > 0 {
            row.waits("After the images")
        } else {
            row.noted(&looking.waiting)
        };
        rows.push(row);
    }
    let (meaning_done, meaning_queued) = status.looking.iter().fold((0, 0), |(done, queued), looking| (done + looking.documents_done, queued + looking.documents_queued));
    if meaning_done + meaning_queued > 0 {
        let mut row = Row::new("meaning", "shown", "Documents by meaning", Some("meaning"), meaning_done, meaning_done + meaning_queued, meaning_queued);
        row.gpu = status.looking.iter().find(|looking| looking.documents_done + looking.documents_queued > 0).map(|looking| looking.gpu);
        let note = status.looking.first().map(|looking| looking.waiting.clone()).unwrap_or_default();
        rows.push(if status.seeing_wait && meaning_queued > 0 { row.waits(schedule_note(status.seeing_at, "Images")) } else { row.noted(&note) });
    }
    if let Some(listening) = status.listening.as_ref() {
        let gpu = !listening.device.is_empty() && !listening.device.eq_ignore_ascii_case("cpu");
        let model = &listening.model;
        if listening.recordings {
            let all = listening.done + listening.queued;
            let mut row = Row::new("recordings", "said", "Recordings: what's said", Some("said"), listening.done, all, listening.queued);
            row.skippable = true;
            row.gpu = Some(gpu);
            row = if listing {
                row.waits("Once your folders have been looked through")
            } else if model.state == "downloading" && model.total > 0 {
                Row { note: format!("Getting its model: {} of {}", crate::format::size(model.received), crate::format::size(model.total)), ..row }
            } else if model.state == "failed" {
                row.waits(if model.total > 0 { "Its model didn't finish downloading. Trying again in a while.".to_string() } else { format!("Couldn't start ({}). Trying again in a while.", model.error) })
            } else if status.listen_wait && listening.queued > 0 {
                row.waits(schedule_note(status.listen_at, "Audio"))
            } else {
                row.noted(&listening.waiting)
            };
            rows.push(row);
        }
        let all = listening.videos_done + listening.videos_queued;
        if listening.videos && all > 0 {
            let mut row = Row::new("videos-said", "said", "Videos: what's said", Some("said"), listening.videos_done, all, listening.videos_queued);
            row.skippable = true;
            row.gpu = Some(gpu);
            row = if status.listen_wait {
                row.waits(schedule_note(status.listen_at, "Audio"))
            } else if listening.recordings && listening.queued > 0 {
                row.waits("After the recordings")
            } else if model.state == "downloading" || model.state == "failed" {
                row.waits("Once the listener is ready")
            } else {
                row.noted(&listening.waiting)
            };
            rows.push(row);
        }
    }
    // Windows' N editions: recordings and videos were chosen, but there's nothing to read them with until the Media
    // Feature Pack is added (the indexer leaves them for then).
    if media_missing {
        let mut row = Row::new("media", "", "Recordings and videos", None, 0, 0, 0)
            .waits("Windows' Media Feature Pack isn't installed. Add it in Windows Settings → System → Optional features, then restart Excavate.");
        row.count = Some("Left for now".into());
        rows.push(row);
    }
    rows
}

/// "(page 14 of 40)", "(61% heard)": how far into the item, where the reader can tell.
fn progress_part(pace: &Pace) -> String {
    match pace.item_unit.as_str() {
        "pages" if pace.item_total > 0 => format!("page {} of {}", group((pace.item_done + 1).min(pace.item_total)), group(pace.item_total)),
        "seconds" if pace.item_total > 0 => format!("{}% heard", (pace.item_done * 100 / pace.item_total).min(100)),
        _ => String::new(),
    }
}

/// "6 at once on the processor", "8 at a time on the graphics card", "16 documents at a time on the graphics card",
/// "one at a time". `what`: what the items are, where the count isn't the lane's own kind of thing ("documents": the
/// meaning lane's batch is documents sent to the model together, not readers side by side).
fn at_once(count: u32, gpu: Option<bool>, what: &str) -> String {
    let place = match gpu {
        Some(true) => " on the graphics card",
        Some(false) => " on the processor",
        None => "",
    };
    let what = if what.is_empty() { String::new() } else { format!(" {what}") };
    match count {
        0 => String::new(),
        1 => format!("one at a time{place}"),
        count if gpu == Some(true) => format!("{count}{what} at a time{place}"),
        count => format!("{count}{what} at once{place}"),
    }
}

/// "380 a minute", "0.4 a minute".
fn per_minute(pace: f64) -> String {
    if pace <= 0.0 {
        String::new()
    } else if pace < 10.0 {
        format!("{} a minute", (pace * 10.0).round() / 10.0)
    } else {
        format!("{} a minute", group(pace.round() as u64))
    }
}

/// The page's view of reading: the status (None before the indexer ever wrote one), the settings, and what the person
/// did here (`Marks`).
pub fn build(inputs: &Inputs) -> View {
    let settings = inputs.settings;
    let now = inputs.now;
    let paused_all = settings.paused(now);
    let speed = config::Speed::ALL.iter().position(|speed| *speed == settings.speed()).unwrap_or(1);
    let Some(status) = inputs.status else {
        return View {
            title: if settings.setup_done { "The indexer hasn't started yet.".into() } else { String::new() },
            fraction: -1.0,
            paused_all,
            speed,
            ..View::default()
        };
    };
    let pace_of = |lane: Option<&str>| lane.and_then(|lane| status.paces.get(lane));
    let reading = matches!(status.state.as_str(), "reading" | "paused");
    let mut kinds = Vec::new();
    let mut stuck: Option<Stuck> = None;
    // Each lane's work left and its time, the rows that go at its pace together.
    let mut lanes: Vec<(&'static str, &'static str, u64)> = Vec::new();
    let media_missing = (settings.listening_on || settings.videos_on) && !winsys::media::available();
    for row in rows(status, media_missing) {
        let pace = pace_of(row.lane);
        let kind_paused = settings.kind_paused(row.pause) || row.note == config::status::KIND_PAUSED;
        let mut state = if row.left == 0 && row.count.is_none() {
            State::Done
        } else if paused_all || kind_paused {
            State::Paused
        } else if row.waiting {
            State::Waiting
        } else {
            State::Working
        };
        let mut line = match state {
            State::Paused if paused_all => inputs.pause_text.clone(),
            State::Paused => "Paused".to_string(),
            State::Waiting => row.note.clone(),
            _ => String::new(),
        };
        let mut skip = String::new();
        if state == State::Working {
            // What it's on: its lane's oldest item in hand (or what the row knows of), how far into it, and how fast.
            let item = pace.map(|pace| pace.item.clone()).filter(|item| !item.is_empty()).unwrap_or_else(|| row.item.clone());
            // (Rows sharing a lane: the item belongs to the one whose kind it is.)
            let mine = match row.id {
                "shown" | "recordings" => !is_video(&item),
                "videos-shown" | "videos-said" => is_video(&item),
                _ => true,
            };
            let in_hand = pace.filter(|pace| pace.current_since > 0 && mine);
            let mut parts = Vec::new();
            if !row.note.is_empty() {
                parts.push(row.note.clone());
            }
            if let Some(pace) = in_hand.filter(|_| !item.is_empty()) {
                let moved_ago = now - pace.moved_at();
                let running = now - pace.current_since;
                let progress = progress_part(pace);
                let skippable = row.skippable && (row.id != "images" || pace.item_unit == "pages") && !inputs.marks.skipped.iter().any(|path| path.eq_ignore_ascii_case(&item));
                if running >= SLOW_AFTER && (pace.at_once <= 1 || moved_ago >= STUCK_AFTER) {
                    state = State::Slow;
                    let mut slow = vec![file_name(&item).to_string()];
                    if pace.item_unit == "seconds" && pace.item_total > 0 {
                        slow.push(format!("{} of speech", length(pace.item_total)));
                    }
                    if !progress.is_empty() {
                        slow.push(progress.clone());
                    }
                    if pace.item_moved > 0 && moved_ago < STUCK_AFTER {
                        slow.push(format!("still moving (last piece {} ago)", ago(moved_ago)));
                    } else {
                        slow.push(format!("on it for {}", ago(running)));
                    }
                    line = slow.join(" · ");
                    let left_alone = inputs.marks.left_alone.iter().any(|(path, since)| path.eq_ignore_ascii_case(&item) && *since == pace.current_since);
                    if moved_ago >= STUCK_AFTER && !left_alone && stuck.is_none() {
                        let place = if progress.is_empty() { String::new() } else { format!(" ({progress})") };
                        stuck = Some(Stuck {
                            text: format!("Stuck: {} hasn't moved for {}{place}", file_name(&item), ago(moved_ago)),
                            path: item.clone(),
                            since: pace.current_since,
                            can_skip: skippable,
                            can_restart: false,
                        });
                    }
                } else {
                    let place = if progress.is_empty() { String::new() } else { format!(" ({progress})") };
                    parts.push(format!("Now: {}{place}", file_name(&item)));
                }
                if skippable {
                    skip = item.clone();
                }
            } else if !item.is_empty() && row.lane.is_none() {
                parts.push(format!("Now: {}", file_name(&item)));
                if row.skippable && !inputs.marks.skipped.iter().any(|path| path.eq_ignore_ascii_case(&item)) {
                    skip = item.clone();
                }
            }
            if state == State::Working {
                if let Some(pace) = pace {
                    parts.push(at_once(if in_hand.is_some() { pace.at_once } else { 0 }, row.gpu, if row.id == "meaning" { "documents" } else { "" }));
                    parts.push(per_minute(pace.per_minute));
                }
                parts.retain(|part| !part.is_empty());
                line = parts.join(" · ");
            }
        }
        // Work left, nothing in hand, and nothing finished for 10 minutes while reading: not moving.
        let mut not_moving = false;
        if let Some(pace) = pace.filter(|pace| state == State::Working && row.left > 0 && row.lane.is_some() && row.id != "later"
                                        && status.state == "reading" && pace.current_since == 0 && pace.last_progress > 0
                                        && now - pace.last_progress >= NOT_MOVING_AFTER) {
            not_moving = true;
            state = State::Slow;
            line = format!("Not moving: nothing finished for {}", ago(now - pace.last_progress));
            skip = String::new();
            let key = format!("lane:{}", row.id);
            let left_alone = inputs.marks.left_alone.iter().any(|(path, since)| *path == key && *since == pace.last_progress);
            if stuck.is_none() && !left_alone {
                stuck = Some(Stuck { text: line.clone(), path: key, since: pace.last_progress, can_skip: false, can_restart: true });
            }
        }
        if state == State::Done {
            line = String::new();
        }
        let chip = match state {
            State::Working => "Working".to_string(),
            State::Slow if not_moving => "Not moving".to_string(),
            State::Slow => "Slow: one long file".to_string(),
            State::Done => "Done".to_string(),
            State::Waiting => row.chip.unwrap_or("Waiting").to_string(),
            State::Paused => "Paused".to_string(),
        };
        let left = if matches!(state, State::Working | State::Slow) { eta_text(eta(row.left, pace, now)) } else { String::new() };
        if let Some(lane) = row.lane.filter(|_| row.left > 0 && row.id != "later") {
            match lanes.iter_mut().find(|(each, ..)| *each == lane) {
                Some(entry) => entry.2 += row.left,
                None => lanes.push((lane, row.name, row.left)),
            }
        }
        let spark = if matches!(state, State::Working | State::Slow) { row.lane.map(|lane| inputs.history.spark(lane)).unwrap_or_default() } else { String::new() };
        kinds.push(Kind {
            id: row.id,
            pause: row.pause,
            name: row.name,
            state,
            chip,
            count: row.count.unwrap_or_else(|| format!("{} of {}", group(row.done.min(row.total)), group(row.total))),
            left,
            fraction: if row.total > 0 && state != State::Done { row.done.min(row.total) as f32 / row.total as f32 } else { -1.0 },
            spark,
            line,
            skip,
            can_pause: state != State::Done && !paused_all && !row.pause.is_empty(),
        });
    }
    // Those at work first, then those waiting or paused, then those done (each group in reading's order).
    kinds.sort_by_key(|kind| match kind.state {
        State::Working | State::Slow => 0,
        State::Waiting | State::Paused => 1,
        State::Done => 2,
    });
    let working: Vec<&Kind> = kinds.iter().filter(|kind| matches!(kind.state, State::Working | State::Slow)).collect();
    // The longest lane decides the time left: they go side by side.
    let longest = lanes.iter().filter(|(lane, ..)| kinds.iter().any(|kind| matches!(kind.state, State::Working | State::Slow) && rows_lane(kind.id) == Some(*lane)))
        .map(|(lane, name, left)| (*lane, *name, *left, eta(*left, status.paces.get(*lane), now)))
        .max_by(|a, b| eta_key(a.3).total_cmp(&eta_key(b.3)));
    let title = if paused_all {
        inputs.pause_text.clone()
    } else {
        match status.state.as_str() {
            "listing" => "Looking through your folders".to_string(),
            "idle" => format!("Up to date · {}", plural(status.documents, "file", "files")),
            "waiting" => "Waiting for setup".to_string(),
            "stopped" if !status.detail.is_empty() => format!("Stopped: {}", status.detail),
            "stopped" => "Stopped".to_string(),
            _ if !working.is_empty() => {
                let mut nouns: Vec<&str> = Vec::new();
                for kind in &working {
                    let noun = match kind.id {
                        "documents" | "meaning" => "documents",
                        "images" | "later" | "deep" | "shown" => "images",
                        "recordings" => "recordings",
                        _ => "videos",
                    };
                    if !nouns.contains(&noun) {
                        nouns.push(noun);
                    }
                }
                format!("Reading {}", join_and(&nouns))
            }
            // Only kinds paused on their own have work: which ones (Pause all isn't on).
            _ if kinds.iter().any(|kind| kind.state == State::Paused) => {
                let mut names: Vec<String> = Vec::new();
                for kind in kinds.iter().filter(|kind| kind.state == State::Paused) {
                    let name = kind.name.to_lowercase();
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                format!("Paused: {}", join_and(&names))
            }
            _ if !status.detail.is_empty() => status.detail.clone(),
            _ => "Waiting".to_string(),
        }
    };
    let left = match longest.map(|longest| longest.3) {
        Some(eta) if reading && !paused_all && eta != Eta::Unknown => format!("{} left", eta_text(eta)),
        _ => String::new(),
    };
    let last_finished = status.paces.values().map(|pace| pace.last_progress).max().unwrap_or(0);
    let mut counts = Vec::new();
    if status.files > 0 && status.state != "listing" {
        counts.push(format!("{} of {}", group((status.read + status.failed).min(status.files)), plural(status.files, "file", "files")));
    }
    if !working.is_empty() && !paused_all {
        let rate: f64 = status.paces.iter().filter(|(_, pace)| pace.current_since > 0 || now - pace.last_progress < 60).map(|(_, pace)| pace.per_minute).sum();
        if rate > 0.0 {
            counts.push(format!("{} files a minute", group(rate.round() as u64)));
        }
    }
    if last_finished > 0 && reading {
        counts.push(format!("last finished {} ago", ago(now - last_finished)));
    }
    let mut longest_line = String::new();
    if let Some((_, name, left, _)) = longest.filter(|_| reading && !paused_all) {
        longest_line = format!("Longest: {} ({} left).", name.to_lowercase(), about(left));
    }
    if reading && !paused_all && !working.is_empty() && status.cpu_threads > 0 && status.threads > 0 {
        if !longest_line.is_empty() {
            longest_line.push(' ');
        }
        longest_line.push_str(&format!("Using {} of {} processor threads", status.cpu_threads.min(status.threads), status.threads));
    }
    let in_hand = working.iter().any(|kind| kind.state == State::Slow) || status.paces.values().any(|pace| pace.current_since > 0);
    let beat = if paused_all || working.is_empty() {
        Beat::Still
    } else if in_hand && last_finished > 0 && now - last_finished > SLOW_AFTER {
        Beat::Quiet
    } else {
        Beat::Alive
    };
    let fraction = crate::setup::progress_of(status).fraction;
    let problems = groups(status, inputs.marks, now);
    View {
        title,
        left,
        counts: counts.join(" · "),
        beat,
        last_finished,
        fraction,
        longest: longest_line,
        problems_total: problems.iter().map(|group| group.count).sum(),
        problems,
        kinds,
        stuck,
        paused_all,
        speed,
        active: reading,
        bedrock: None,
        today: None,
        fixes: fixes(status, settings),
    }
}

/// The lane a row goes at (as `rows` has it).
fn rows_lane(id: &str) -> Option<&'static str> {
    Some(match id {
        "documents" => "documents",
        "images" | "later" => "images",
        "deep" => "deep",
        "shown" | "videos-shown" => "shown",
        "meaning" => "meaning",
        "recordings" | "videos-said" => "said",
        _ => return None,
    })
}

/// For finding the longest: a known time by its upper end, a day or more above any, an unknown below.
fn eta_key(eta: Eta) -> f64 {
    match eta {
        Eta::Unknown => -1.0,
        Eta::Range(_, high) => high,
        Eta::Long => f64::MAX,
    }
}

/// "documents", "documents and images", "documents, images and videos".
fn join_and(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What couldn't be read, by why, in `config::why` order, with what each one's action is.
fn groups(status: &Status, marks: &Marks, now: i64) -> Vec<Group> {
    status.problems.iter().filter(|(_, count)| *count > 0).filter_map(|(why, count)| {
        let (why, label, action) = match why.as_str() {
            config::why::PASSWORD => (config::why::PASSWORD, "Password-protected", ""),
            config::why::ONLINE => (config::why::ONLINE, "Online only", "Download and read"),
            config::why::DAMAGED => (config::why::DAMAGED, "Damaged", "Try again"),
            config::why::SKIPPED => (config::why::SKIPPED, "Skipped", "Read again"),
            config::why::STOPPED => (config::why::STOPPED, "Stopped the reader twice", "Try again"),
            _ => return None,
        };
        // Asked, and the indexer hasn't got to it yet (its count is as it was, for up to two minutes).
        let asked = !action.is_empty() && marks.asked.get(why).is_some_and(|(then, at)| *then == *count && now - at < 120);
        let files = status.problem_files.iter().filter(|file| file.why == why)
            .map(|file| (file_name(&file.path).to_string(), file.path.clone(), plain_why(&file.said).to_string())).collect();
        Some(Group {
            why,
            label,
            count: *count,
            action: if asked { if why == config::why::ONLINE { "Downloading…" } else { "Reading again…" } } else { action }.to_string(),
            asked,
            files,
        })
    }).collect()
}

/// Why a file couldn't be read, in plain words where the reader's message isn't.
pub fn plain_why(why: &str) -> &str {
    if why.contains("PasswordError") {
        "It's protected by a password"
    } else if why.contains("FormatError") {
        "It's damaged, or it isn't really a PDF"
    } else if why.contains("component cannot be found") {
        "It's damaged, or Windows can't open this kind of image"
    } else if why == config::SKIP_ONLINE {
        "Kept online only"
    } else if why == config::SKIP_STOPPED {
        "The reader stopped on it twice"
    } else if why.contains("stopped on this") {
        "The reader stopped on it"
    } else if matches!(why, "text in videos" | "what it shows" | "what's said" | "text in images") {
        "Skipped by you"
    } else {
        why
    }
}

// ---- The worker ------------------------------------------------------------------------------------------------------------

/// What the UI thread applies.
pub struct Snapshot {
    pub view: View,
    /// The status it was built from, when that changed since the last snapshot (the other pages follow it too).
    pub status: Option<Option<Status>>,
    pub index_size: Option<u64>,
    /// `Marks::generation` as it was when this was built: older than now, it isn't applied (what's on screen already
    /// shows the change).
    pub generation: u64,
}

enum Message {
    Open(bool),
    Wake,
}

static MARKS: LazyLock<Mutex<Marks>> = LazyLock::new(|| Mutex::new(Marks::default()));
static SENDER: Mutex<Option<Sender<Message>>> = Mutex::new(None);
/// The latest view built, for the window to show the moment it opens.
static LATEST: Mutex<Option<(View, Option<Status>, u64)>> = Mutex::new(None);

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Starts the worker (once): the view kept ready, slowly, until Settings opens.
pub fn start(paths: config::Paths) {
    let mut sender = locked(&SENDER);
    if sender.is_some() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    *sender = Some(tx);
    let _ = std::thread::Builder::new().name("activity".into()).spawn(move || {
        // The graphics cards, listed once here so the Settings window doesn't wait for them (`winsys::gpu` keeps them).
        let _ = winsys::gpu::adapters();
        work(paths, rx)
    });
}

/// Settings opened or closed: views every half second while it's open.
pub fn open(open: bool) {
    send(Message::Open(open));
}

/// Something changed (the settings, what the person did): a new view now, rather than at the next tick.
pub fn wake() {
    send(Message::Wake);
}

fn send(message: Message) {
    if let Some(sender) = locked(&SENDER).as_ref() {
        let _ = sender.send(message);
    }
}

/// Changes what the person did, and has the view follow at once.
pub fn mark(change: impl FnOnce(&mut Marks)) {
    {
        let mut marks = locked(&MARKS);
        change(&mut marks);
        marks.generation += 1;
    }
    wake();
}

/// The settings changed on the UI thread (saved already): views built before don't count.
pub fn settings_changed() {
    mark(|_| {});
}

pub fn generation() -> u64 {
    locked(&MARKS).generation
}

/// The latest view and status there are, for a window opening now.
pub fn latest() -> Option<(View, Option<Status>, u64)> {
    locked(&LATEST).clone()
}

fn work(paths: config::Paths, messages: Receiver<Message>) {
    let mut open = false;
    let mut history = History::default();
    let mut last_status: Option<Option<Status>> = None;
    let mut last_view: Option<View> = None;
    let mut index_size: Option<u64> = None;
    let mut index_at: Option<Instant> = None;
    let mut today_now: Option<Today> = None;
    // The past year's days, asked again once a day (or when the window opens).
    let mut days: Option<(i64, crate::files_read::Days)> = None;
    // The furthest the bar has come in this session of reading (it holds its place rather than step back).
    let mut furthest = (0i64, 0.0f32);
    let mut sent_size = None;
    loop {
        match messages.recv_timeout(if open { OPEN_EVERY } else { HIDDEN_EVERY }) {
            Ok(Message::Open(now_open)) => {
                open = now_open;
                if !open {
                    continue;
                }
                // (A window opening: everything sent afresh.)
                days = None;
                last_view = None;
                last_status = None;
                sent_size = None;
            }
            Ok(Message::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let generation = generation();
        let status = Status::load(&paths);
        let settings = Settings::load(&paths);
        let now = crate::app::now();
        if let Some(status) = &status {
            history.sample(status, now);
        }
        if open && index_at.is_none_or(|at| at.elapsed() >= INDEX_EVERY) {
            index_size = Some(folder_size(&paths.index()));
            // Today, from midnight here.
            let offset = winsys::process::utc_offset();
            let midnight = (now + offset).div_euclid(86_400) * 86_400 - offset;
            let day = (now + offset).div_euclid(86_400);
            if days.as_ref().is_none_or(|(asked, _)| *asked != day) {
                days = Some((day, crate::files_read::days(&paths.catalog(), day, offset)));
            }
            let twelve = settings.twelve_hour(winsys::process::twelve_hour_clock());
            today_now = done_today(&paths.catalog(), midnight)
                .and_then(|done| summarize_today(&done, midnight, &days.as_ref().map(|d| d.1.clone()).unwrap_or_default(), day, twelve));
            index_at = Some(Instant::now());
        }
        let marks = locked(&MARKS).clone();
        let pause_text = settings.pause_text(winsys::process::utc_offset(), settings.twelve_hour(winsys::process::twelve_hour_clock()));
        let mut view = build(&Inputs { status: status.as_ref(), settings: &settings, now, history: &history, marks: &marks, pause_text });
        // (Bedrock's card asks the drive: only while the window is open.)
        if open {
            view.bedrock = bedrock();
            view.today = today_now.clone();
        }
        if let Some(status) = &status
            && view.active && view.fraction >= 0.0 {
                view.fraction = if furthest.0 == status.started { view.fraction.max(furthest.1) } else { view.fraction };
                furthest = (status.started, view.fraction);
            }
        *locked(&LATEST) = Some((view.clone(), status.clone(), generation));
        if !open {
            continue;
        }
        let status_changed = last_status.as_ref() != Some(&status);
        let size_changed = sent_size != index_size;
        if !status_changed && !size_changed && last_view.as_ref() == Some(&view) {
            continue;
        }
        let snapshot = Snapshot {
            view: view.clone(),
            status: status_changed.then(|| status.clone()),
            index_size: if size_changed { index_size } else { None },
            generation,
        };
        last_view = Some(view);
        last_status = Some(status);
        sent_size = index_size;
        if slint::invoke_from_event_loop(move || crate::with_app(|app| crate::setup::apply_snapshot(app, snapshot))).is_err() {
            return;
        }
    }
}

/// The index's files, added up (its folder only: tantivy keeps them in one).
fn folder_size(folder: &std::path::Path) -> u64 {
    std::fs::read_dir(folder)
        .map(|entries| entries.flatten().filter_map(|entry| entry.metadata().ok()).filter(|metadata| metadata.is_file()).map(|metadata| metadata.len()).sum())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::status::{Folder, Listening, Problem};

    const NOW: i64 = 1_800_000_000;

    fn pace(per_minute: f64, long: f64) -> Pace {
        Pace { per_minute, per_minute_long: long, last_progress: NOW - 1, ..Pace::default() }
    }

    fn reading() -> Status {
        let mut status = Status { state: "reading".into(), files: 145_823, read: 30_000, failed: 405, images: 18_048, images_read: 2_598, queued_text: 99_968,
                                  queued_pictures: 423, engine: "PP-OCRv6 small".into(), cpu_threads: 24, threads: 32, ..Status::default() };
        status.paces.insert("documents".into(), Pace { current_since: NOW - 5, at_once: 6, item: r"C:\Docs\Lecture notes week 9.pdf".into(), ..pace(380.0, 340.0) });
        status.paces.insert("images".into(), Pace { current_since: NOW - 2, at_once: 1, ..pace(38.0, 36.0) });
        status
    }

    fn view(status: &Status, settings: &Settings) -> View {
        build(&Inputs { status: Some(status), settings, now: NOW, history: &History::default(), marks: &Marks::default(), pause_text: "Paused".into() })
    }

    #[test]
    fn kinds_at_work_come_first_and_scanned_pdfs_are_counted() {
        // Documents all read; every image read too, but scanned PDFs' pages still going (found as documents were
        // read, so not among the images): the images row works, and comes before the documents row that's done.
        let mut status = reading();
        (status.queued_text, status.read, status.images_read) = (0, status.files - status.failed, status.images);
        (status.queued_pictures, status.queued_scanned) = (37, 37);
        status.paces.insert("documents".into(), Pace { current_since: 0, at_once: 0, last_progress: NOW - 30, ..pace(380.0, 340.0) });
        let view = view(&status, &Settings::default());
        let order: Vec<(&str, State)> = view.kinds.iter().map(|kind| (kind.id, kind.state)).collect();
        assert_eq!(order, [("images", State::Working), ("documents", State::Done)]);
        assert_eq!(view.kinds[0].count, "18,048 of 18,048 · 37 scanned PDFs to go");
    }

    #[test]
    fn a_reader_that_fails_has_a_card_with_its_fix() {
        let mut status = reading();
        assert!(fixes(&status, &Settings::default()).is_empty());
        // The image reader didn't start on the graphics card: try again, or the processor.
        status.reading_gpu = true;
        status.model = config::status::Model { state: "failed".into(), error: "the DirectML device was removed\nat line 3".into(), ..Default::default() };
        let cards = fixes(&status, &Settings::default());
        assert_eq!(cards[0].title, "Text in images couldn't start");
        assert_eq!(cards[0].line, "The DirectML device was removed. It tries again by itself in a while.");
        assert_eq!(cards[0].actions, [("Try again now", "retry"), ("Run on the processor", "images-cpu")]);
        // A download that failed: only trying again helps.
        status.model = config::status::Model { state: "failed".into(), total: 900, error: "connection reset".into(), ..Default::default() };
        let cards = fixes(&status, &Settings::default());
        assert_eq!((cards[0].title.as_str(), cards[0].actions.len()), ("Text in images couldn't download its model", 1));
    }

    #[test]
    fn today_says_what_was_read_and_when() {
        let midnight = 1_000_000;
        let done = [("Documents", midnight + 9 * 3600), ("Documents", midnight + 9 * 3600 + 60), ("Text in images", midnight + 14 * 3600),
                    ("What images show", midnight + 14 * 3600)];
        let day = midnight / 86_400;
        let today = summarize_today(&done, midnight, &Vec::new(), day, true).expect("something was read");
        assert_eq!(today.spans[0].total, "3");
        assert_eq!(today.parts, "Documents 2 · Text in images 1 · What images show 1");
        let hours = &today.spans[0].bars;
        assert_eq!((hours.len(), hours[9].share, hours[14].share, hours[14].seen.as_str(), hours[0].share), (24, 1.0, 0.5, "1", 0.0));
        // Nothing today, but on a day before: the card still shows.
        assert!(summarize_today(&[], midnight, &Vec::new(), day, true).is_none());
        assert!(summarize_today(&[], midnight, &vec![(day - 3, 5, 0)], day, true).is_some());
        assert_eq!((today_part("said-parakeet"), today_part("whisper-turbo"), today_part("eg2")), ("Said in videos", "Recordings", "What images show"));
    }

    #[test]
    fn without_media_foundation_recordings_and_videos_say_why() {
        let status = reading();
        assert!(!rows(&status, false).iter().any(|row| row.id == "media"));
        let rows = rows(&status, true);
        let media = rows.iter().find(|row| row.id == "media").expect("a row for them");
        assert!(media.waiting && media.count.is_some() && media.note.contains("Media Feature Pack"), "{:?}", media.note);
    }

    #[test]
    fn a_kind_with_work_and_nothing_in_hand_is_not_moving() {
        // 0.1.3's documents: 106,093 left, no worker holding a file, nothing finished for 12 minutes.
        let mut status = reading();
        status.paces.insert("documents".into(), Pace { current_since: 0, at_once: 0, last_progress: NOW - 720, ..Pace::default() });
        let view = view(&status, &Settings::default());
        let documents = &view.kinds[0];
        assert_eq!((documents.chip.as_str(), documents.line.as_str()), ("Not moving", "Not moving: nothing finished for 12 min"));
        assert!(documents.skip.is_empty());
        let card = view.stuck.expect("a card");
        assert!(card.can_restart && !card.can_skip && card.text == "Not moving: nothing finished for 12 min", "{card:?}");
        // "Leave it": the card goes for that stretch.
        let marks = Marks { left_alone: vec![("lane:documents".into(), NOW - 720)], ..Marks::default() };
        let left = build(&Inputs { status: Some(&status), settings: &Settings::default(), now: NOW, history: &History::default(), marks: &marks, pause_text: String::new() });
        assert!(left.stuck.is_none());
        // Nine minutes is still working; and listing, or a wait, isn't "not moving".
        status.paces.get_mut("documents").unwrap().last_progress = NOW - 540;
        assert_eq!(super::tests::view(&status, &Settings::default()).kinds[0].chip, "Working");
        status.paces.get_mut("documents").unwrap().last_progress = NOW - 720;
        status.state = "listing".into();
        assert_ne!(super::tests::view(&status, &Settings::default()).kinds[0].chip, "Not moving");
    }

    #[test]
    fn the_time_left_is_a_range() {
        // 100,000 at 380 a minute lately and 340 over the quarter hour: 4 h to 5 h 20, a tenth either way included.
        let documents = pace(380.0, 340.0);
        let Eta::Range(low, high) = eta(100_000, Some(&documents), NOW) else { panic!("a range") };
        assert!(low < 100_000.0 / 380.0 * 60_000.0 && high > 100_000.0 / 340.0 * 60_000.0);
        assert_eq!(eta_text(Eta::Range(low, high)), "4 h – 5 h 20");
        // Steady: still a range, a tenth either way.
        assert_eq!(eta_text(eta(60, Some(&pace(2.0, 2.0)), NOW)), "25 – 35 min");
        assert_eq!(eta_text(eta(4, Some(&pace(2.0, 2.0)), NOW)), "about 2 min");
        assert_eq!(eta_text(eta(1, Some(&pace(120.0, 120.0)), NOW)), "less than a minute");
        // A tiny pace: no absurd figure.
        assert_eq!(eta_text(eta(100_000, Some(&pace(0.01, 0.01)), NOW)), "more than a day");
        assert_eq!(eta_text(eta(30_000, Some(&pace(20.0, 25.0)), NOW)), "18 h or more");
        // No pace yet: nothing, until it's been on one item for a minute.
        assert_eq!(eta(10, Some(&Pace { current_since: NOW - 5, ..Pace::default() }), NOW), Eta::Unknown);
        assert_eq!(eta(10, Some(&Pace { current_since: NOW - 90, ..Pace::default() }), NOW), Eta::Long);
        assert_eq!(eta(0, Some(&documents), NOW), Eta::Unknown);
        assert_eq!(about(99_968), "about 100,000");
        assert_eq!(about(2_598), "about 2,600");
        assert_eq!(about(640), "640");
    }

    #[test]
    fn the_whole_page_while_reading() {
        let status = reading();
        let view = view(&status, &Settings::default());
        assert_eq!(view.title, "Reading documents and images");
        assert!(view.left.ends_with(" left") && view.left.contains(" h "), "{}", view.left);
        assert_eq!(view.counts, "30,405 of 145,823 files · 418 files a minute · last finished 1 s ago");
        assert_eq!(view.longest, "Longest: documents and code (about 100,000 left). Using 24 of 32 processor threads");
        assert_eq!(view.beat, Beat::Alive);
        let documents = &view.kinds[0];
        assert_eq!((documents.name, documents.state, documents.chip.as_str()), ("Documents and code", State::Working, "Working"));
        assert_eq!(documents.count, "27,807 of 127,775");
        assert_eq!(documents.line, "Now: Lecture notes week 9.pdf · 6 at once on the processor · 380 a minute");
        assert_eq!(documents.skip, r"C:\Docs\Lecture notes week 9.pdf", "a document is let go by ending its reader");
        let images = &view.kinds[1];
        assert_eq!((images.name, images.count.as_str()), ("Text in images", "2,598 of 18,048"));
        // Documents by meaning at Full: a batch of 16 sent to the model in one request is 16 documents in hand, not
        // "one at a time" (one thread).
        let mut meaning = status.clone();
        meaning.looking = vec![config::status::Looking { engine: "eg2-140".into(), documents_done: 400, documents_queued: 1_200, gpu: true, ..Default::default() }];
        meaning.paces.insert("meaning".into(), Pace { current_since: NOW - 1, at_once: 16, ..pace(160.0, 150.0) });
        let row = view_with(&meaning, &Settings::default()).kinds.into_iter().find(|kind| kind.id == "meaning").expect("the meaning row");
        assert_eq!((row.state, row.line.as_str()), (State::Working, "16 documents at a time on the graphics card · 160 a minute"));
        // At Normal, one piece at a time: as it was.
        meaning.paces.insert("meaning".into(), Pace { current_since: NOW - 1, at_once: 1, ..pace(20.0, 20.0) });
        let row = view_with(&meaning, &Settings::default()).kinds.into_iter().find(|kind| kind.id == "meaning").expect("the meaning row");
        assert_eq!(row.line, "one at a time on the graphics card · 20 a minute");
        // Paused on its own: says so, and the header goes on with the rest.
        let mut settings = Settings::default();
        settings.set_kind_paused("images", true);
        let paused = view_with(&status, &settings);
        assert_eq!((paused.kinds[1].state, paused.kinds[1].chip.as_str()), (State::Paused, "Paused"));
        assert_eq!(paused.title, "Reading documents");
        // Only that kind had work: the header says which is paused, and nothing about threads.
        let only_images = Status { queued_text: 0, read: 127_775 + 405, failed: 0, state: "paused".into(), detail: "Paused".into(), ..status.clone() };
        let alone = view_with(&only_images, &settings);
        assert_eq!(alone.title, "Paused: text in images");
        assert!(!alone.longest.contains("threads"), "{}", alone.longest);
        // Pause all: every kind with work, and the header says until when.
        let all = build(&Inputs { status: Some(&status), settings: &Settings { paused_until: -1, ..Settings::default() }, now: NOW, history: &History::default(),
                                  marks: &Marks::default(), pause_text: "Paused".into() });
        assert!(all.paused_all && all.kinds.iter().all(|kind| kind.state == State::Paused));
        assert_eq!((all.title.as_str(), all.left.as_str(), all.beat), ("Paused", "", Beat::Still));
    }

    fn view_with(status: &Status, settings: &Settings) -> View {
        view(status, settings)
    }

    #[test]
    fn slow_is_told_from_stuck() {
        let mut status = reading();
        // A recording heard for 10 minutes, its last piece 4 s ago: slow, still moving.
        status.listening = Some(Listening { recordings: true, done: 10, queued: 3, device: "Vulkan".into(), current: r"C:\Talks\Lecture 10.mp3".into(), ..Listening::default() });
        status.paces.insert("said".into(), Pace { current_since: NOW - 600, at_once: 1, item: r"C:\Talks\Lecture 10.mp3".into(), item_done: 4900, item_total: 8040,
                                                    item_unit: "seconds".into(), item_moved: NOW - 4, ..pace(0.5, 0.4) });
        let view = view(&status, &Settings::default());
        let said = view.kinds.iter().find(|kind| kind.id == "recordings").unwrap();
        assert_eq!((said.state, said.chip.as_str()), (State::Slow, "Slow: one long file"));
        assert_eq!(said.line, "Lecture 10.mp3 · 2 h 14 min of speech · 60% heard · still moving (last piece 4 s ago)");
        assert_eq!(said.skip, r"C:\Talks\Lecture 10.mp3");
        assert!(view.stuck.is_none());
        // A scanned PDF that hasn't moved for 3 min: stuck, with Skip.
        status.paces.insert("images".into(), Pace { current_since: NOW - 400, at_once: 1, item: r"C:\Scans\Scan 2019.pdf".into(), item_done: 1, item_total: 1200,
                                                      item_unit: "pages".into(), item_moved: NOW - 180, ..pace(38.0, 36.0) });
        let view = super::tests::view(&status, &Settings::default());
        let stuck = view.stuck.clone().expect("stuck");
        assert_eq!(stuck.text, "Stuck: Scan 2019.pdf hasn't moved for 3 min (page 2 of 1,200)");
        assert!(stuck.can_skip);
        // Left alone: no card for that item (the next one stuck would have one).
        let marks = Marks { left_alone: vec![(stuck.path.clone(), stuck.since)], ..Marks::default() };
        let left = build(&Inputs { status: Some(&status), settings: &Settings::default(), now: NOW, history: &History::default(), marks: &marks, pause_text: String::new() });
        assert!(left.stuck.is_none());
        // Several documents at once, the oldest only a few seconds in: working, not slow.
        assert_eq!(view.kinds[0].state, State::Working);
        // A document on for 5 minutes alone: stuck, and Skip ends its reader.
        status.paces.insert("documents".into(), Pace { current_since: NOW - 300, at_once: 1, item: r"C:\Docs\huge.docx".into(), ..pace(380.0, 340.0) });
        let doc = super::tests::view(&status, &Settings::default());
        assert_eq!(doc.kinds[0].state, State::Slow);
        let stuck = doc.stuck.expect("stuck");
        assert!(stuck.text.starts_with("Stuck: huge.docx hasn't moved for 5 min") && stuck.can_skip);
        // The heartbeat: nothing finished for two minutes while it holds an item: amber.
        for pace in status.paces.values_mut() {
            pace.last_progress = NOW - 120;
        }
        assert_eq!(super::tests::view(&status, &Settings::default()).beat, Beat::Quiet);
    }

    #[test]
    fn what_couldnt_be_read_is_grouped() {
        let mut status = reading();
        status.problems = vec![("password".into(), 9), ("online".into(), 6), ("damaged".into(), 5), ("skipped".into(), 2), ("stopped".into(), 1)];
        status.problem_files = vec![
            Problem { why: "online".into(), path: r"C:\OneDrive\Plan.docx".into(), said: config::SKIP_ONLINE.into() },
            Problem { why: "password".into(), path: r"C:\Docs\Locked.pdf".into(), said: "PdfiumLibraryInternalError(PasswordError)".into() },
        ];
        let view = view(&status, &Settings::default());
        assert_eq!(view.problems_total, 23);
        let labels: Vec<(&str, u64, &str)> = view.problems.iter().map(|group| (group.label, group.count, group.action.as_str())).collect();
        assert_eq!(labels, [("Password-protected", 9, ""), ("Online only", 6, "Download and read"), ("Damaged", 5, "Try again"), ("Skipped", 2, "Read again"),
                            ("Stopped the reader twice", 1, "Try again")]);
        assert_eq!(view.problems[1].files, [("Plan.docx".to_string(), r"C:\OneDrive\Plan.docx".to_string(), "Kept online only".to_string())]);
        assert_eq!(view.problems[0].files[0].2, "It's protected by a password");
        // Asked: says so at once, until the count changes.
        let marks = Marks { asked: HashMap::from([("online".to_string(), (6, NOW - 2))]), ..Marks::default() };
        let asked = build(&Inputs { status: Some(&status), settings: &Settings::default(), now: NOW, history: &History::default(), marks: &marks, pause_text: String::new() });
        assert_eq!((asked.problems[1].asked, asked.problems[1].action.as_str()), (true, "Downloading…"));
        status.problems[1].1 = 0;
        let done = build(&Inputs { status: Some(&status), settings: &Settings::default(), now: NOW, history: &History::default(), marks: &marks, pause_text: String::new() });
        assert!(done.problems.iter().all(|group| group.why != "online"));
    }

    #[test]
    fn a_missing_folder_and_the_rest_of_the_states() {
        // (The folder rows say "Folder not found" from the status: `setup::folder_detail`.)
        let mut status = Status { state: "idle".into(), files: 10, read: 10, documents: 10, ..Status::default() };
        status.folders = vec![Folder { path: r"E:\Photos".into(), missing: true, ..Folder::default() }];
        let view = view(&status, &Settings::default());
        assert_eq!(view.title, "Up to date · 10 files");
        assert_eq!((view.kinds[0].state, view.kinds[0].chip.as_str(), view.beat), (State::Done, "Done", Beat::Still));
        assert!(!view.kinds[0].can_pause);
        // No status yet.
        let none = build(&Inputs { status: None, settings: &Settings { setup_done: true, ..Settings::default() }, now: NOW, history: &History::default(),
                                   marks: &Marks::default(), pause_text: String::new() });
        assert_eq!(none.title, "The indexer hasn't started yet.");
        // The first sweep's set-aside images wait for it.
        let mut sweeping = reading();
        sweeping.queued_later = 300;
        sweeping.queued_pictures = 723;
        let view = super::tests::view(&sweeping, &Settings::default());
        let later = view.kinds.iter().find(|kind| kind.id == "later").unwrap();
        assert_eq!((later.state, later.chip.as_str()), (State::Waiting, "Waiting: after the first sweep"));
        assert_eq!(later.line, "300 near-duplicate images · read once the first sweep is done");
        // Waiting for a quiet moment: says why.
        let mut busy = reading();
        busy.waits[1] = "Waiting for a quiet moment: your graphics card is busy".into();
        let view = super::tests::view(&busy, &Settings::default());
        assert_eq!((view.kinds[1].state, view.kinds[1].line.as_str()), (State::Waiting, "Waiting for a quiet moment: your graphics card is busy"));
    }

    #[test]
    fn sparklines_from_the_pace() {
        let mut history = History::default();
        let mut status = reading();
        for step in 0..40 {
            status.paces.get_mut("documents").unwrap().per_minute = 300.0 + (step % 5) as f64 * 20.0;
            history.sample(&status, NOW + step * 15);
        }
        let spark = history.spark("documents");
        assert_eq!(spark.matches('M').count(), SPARK);
        assert!(spark.starts_with("M0 14v"));
        assert!(history.spark("deep").is_empty());
    }
}
