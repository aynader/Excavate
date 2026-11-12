//! The welcome page's first scene: a query types itself, and the real ranking runs on each keystroke over 10,000
//! made-up file names. The time it shows is measured here, on this PC: on its own index of the drives when there is
//! one (the one answering, or the one saved, opened while the page shows: only how many names it has and how long
//! the search took are shown, never a name from it), otherwise on the 10,000 names.

use crate::{DemoRow, SettingsWindow, Theme, format};
use search::Terms;
use search::rank::{self, Candidate, Context};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DOCUMENT: &str = "M7 3h7l5 5v13H7z M14 3v5h5";
const SHEET: &str = "M4 5h16v14H4z M4 10h16 M4 15h16 M10 5v14";
const PICTURE: &str = "M4 5h16v14H4z M4 15l5-5l4 4l3-3l4 4";
const FOLDER: &str = "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z";
const CODE: &str = "M9 7l-5 5l5 5 M15 7l5 5l-5 5";

const SUBJECTS: [&str; 24] = [
    "invoice", "budget", "contract", "report", "meeting notes", "holiday photos", "resume", "tax return", "floor plan", "recipes",
    "lease", "itinerary", "quote", "receipt", "presentation", "project plan", "insurance", "statement", "newsletter", "minutes",
    "training", "roadmap", "survey", "schedule",
];
const QUALIFIERS: [&str; 20] = [
    "2024", "2025", "2026", "final", "draft", "v2", "march", "june", "october", "q1", "q3", "client", "team", "family", "kitchen",
    "summer", "signed", "scanned", "old", "new",
];
const KINDS: [(&str, &str, &str); 6] = [
    ("pdf", DOCUMENT, "Documents"),
    ("docx", DOCUMENT, "Documents\\Work"),
    ("xlsx", SHEET, "Documents\\Finance"),
    ("jpg", PICTURE, "Pictures"),
    ("py", CODE, "Code\\tools"),
    ("", FOLDER, "Desktop"),
];

/// What it types, in turn.
const QUERIES: [&str; 5] = ["budget", "tax ret", "holid", "meet not", "floor"];

struct Demo {
    window: slint::Weak<SettingsWindow>,
    names: Vec<Candidate>,
    context: Context,
    accent: String,
    query: usize,
    letters: usize,
    timer: slint::Timer,
    /// When the next keystroke is due, and how long there was to go when it was paused.
    next: Instant,
    held: Option<Duration>,
    /// What's typed, to the search on the real index; once that one has answered (`REAL_SHOWN`), its time is the one
    /// shown.
    real: Option<mpsc::Sender<String>>,
}

/// 10,000 names from the lists above, the same on every PC.
fn names() -> Vec<Candidate> {
    let mut names = Vec::with_capacity(10_000);
    let mut seed = 0x2545_f491u32;
    let mut next = move |limit: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize % limit
    };
    for index in 0..10_000 {
        let (subject, qualifier) = (SUBJECTS[next(SUBJECTS.len())], QUALIFIERS[next(QUALIFIERS.len())]);
        let (extension, _, folder) = KINDS[next(KINDS.len())];
        let stem = match index % 3 {
            0 => format!("{subject} {qualifier}"),
            1 => format!("{qualifier} {subject}"),
            _ => format!("{subject}"),
        };
        let name = if extension.is_empty() { stem } else { format!("{stem}.{extension}") };
        names.push(Candidate {
            name,
            folder: format!("C:\\Users\\you\\{folder}"),
            is_folder: extension.is_empty(),
            size: Some(40_000 + next(4_000_000) as u64),
            modified: Some(1_780_000_000 - next(60_000_000) as i64),
            ..Candidate::default()
        });
    }
    names
}

thread_local! {
    static DEMO: RefCell<Option<Rc<RefCell<Demo>>>> = const { RefCell::new(None) };
}

/// Starts typing into the window's search scene. Stops by itself when the window closes. `saved` is where the own
/// index is saved.
pub fn start(window: &SettingsWindow, accent: String, saved: PathBuf) {
    let real = Some(real(window.as_weak(), saved));
    let demo = Rc::new(RefCell::new(Demo {
        window: window.as_weak(),
        names: names(),
        context: Context { now: 1_780_000_000, profile: "c:\\users\\you".into() },
        accent,
        query: 0,
        letters: 0,
        timer: slint::Timer::default(),
        next: Instant::now(),
        held: None,
        real,
    }));
    // Without Windows' animation effects it stands still: one search, finished.
    if window.global::<Theme>().get_animate() {
        tick(&demo);
    } else {
        demo.borrow().show(window, QUERIES[0]);
    }
    DEMO.with(|slot| *slot.borrow_mut() = Some(demo));
}

/// Stops it (leaving the page).
pub fn stop() {
    DEMO.with(|slot| slot.borrow_mut().take());
}

/// Holds the typing (the pointer is on its card), or lets it carry on from where it was.
pub fn hold(on: bool) {
    let demo = DEMO.with(|slot| slot.borrow().clone());
    let Some(demo) = demo else { return };
    if on {
        let mut demo = demo.borrow_mut();
        demo.held = Some(demo.next.saturating_duration_since(Instant::now()));
        demo.timer.stop();
    } else {
        let held = demo.borrow_mut().held.take();
        if let Some(held) = held {
            schedule(&demo, held);
        }
    }
}

fn tick(demo: &Rc<RefCell<Demo>>) {
    let wait = {
        let mut demo = demo.borrow_mut();
        demo.step()
    };
    schedule(demo, wait);
}

fn schedule(demo: &Rc<RefCell<Demo>>, wait: Duration) {
    let weak = Rc::downgrade(demo);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, wait, move || {
        if let Some(demo) = weak.upgrade() {
            tick(&demo);
        }
    });
    let mut demo = demo.borrow_mut();
    demo.timer = timer;
    demo.next = Instant::now() + wait;
}

impl Demo {
    /// One keystroke; returns how long until the next.
    fn step(&mut self) -> Duration {
        let Some(window) = self.window.upgrade() else { return Duration::from_secs(3600) };
        let query = QUERIES[self.query % QUERIES.len()];
        let count = query.chars().count();
        if self.letters > count {
            // Cleared at once, as selecting the text and typing over it does.
            self.letters = 0;
            self.query += 1;
            return Duration::from_millis(250);
        }
        let typed: String = query.chars().take(self.letters).collect();
        self.show(&window, &typed);
        self.letters += 1;
        if self.letters > count { Duration::from_millis(1700) } else { Duration::from_millis(120) }
    }

    fn show(&self, window: &SettingsWindow, typed: &str) {
        window.set_search_typed(typed.into());
        let terms = Terms::parse(typed);
        if terms.is_empty() {
            window.set_search_rows(ModelRc::new(VecModel::from(Vec::<DemoRow>::new())));
            window.set_search_time(SharedString::new());
            return;
        }
        let started = Instant::now();
        let found = rank::rank(self.names.iter().cloned(), &terms, &self.context, 3);
        let took = started.elapsed();
        if let Some(real) = &self.real {
            let _ = real.send(typed.to_string());
        }
        let rows: Vec<DemoRow> = found.iter().map(|ranked| {
            let candidate = &ranked.candidate;
            let extension = candidate.name.rsplit_once('.').map(|(_, extension)| extension).unwrap_or("");
            let (_, glyph, _) = KINDS.iter().find(|(kind, ..)| *kind == extension).copied().unwrap_or(KINDS[5]);
            let markup = format::styled_name(&candidate.name, &ranked.highlights, &self.accent);
            DemoRow {
                name: slint::StyledText::from_markdown(&markup).unwrap_or_else(|_| slint::StyledText::from_plain_text(&candidate.name)),
                detail: candidate.folder.rsplit('\\').next().unwrap_or("").into(),
                glyph: glyph.into(),
            }
        }).collect();
        window.set_search_rows(ModelRc::new(VecModel::from(rows)));
        if !REAL_SHOWN.with(Cell::get) {
            window.set_search_time(format!("{:.1} ms · 10,000 files", took.as_secs_f64() * 1000.0).into());
        }
    }
}

thread_local! {
    /// The real index has answered (on the window's thread).
    static REAL_SHOWN: Cell<bool> = const { Cell::new(false) };
}

/// The search on the drives' own index, on a thread of its own: the one answering if it's on, otherwise the saved one,
/// opened once (mapped: a few milliseconds, and almost none of it in memory). Each query typed is searched and timed
/// the way the search window's is (a page of the first results, by name), and the time shown with how many names
/// there are. Ends when the demo does.
fn real(window: slint::Weak<SettingsWindow>, saved: PathBuf) -> mpsc::Sender<String> {
    REAL_SHOWN.with(|shown| shown.set(false));
    let (sender, typed) = mpsc::channel::<String>();
    let _ = std::thread::Builder::new().name("welcome search".into()).spawn(move || {
        let mut opened: Option<Option<engine::store::Index>> = None;
        while let Ok(mut text) = typed.recv() {
            // Only the latest, if it typed on meanwhile.
            while let Ok(newer) = typed.try_recv() {
                text = newer;
            }
            let measure = |index: &engine::store::Index| {
                let search = engine::search::Search::new(&text);
                let started = Instant::now();
                let found = search.page(index, None, engine::search::Sort::Name, false, 0, 3);
                std::hint::black_box(&found);
                (started.elapsed(), index.live())
            };
            let answer = match crate::own_index::answering() {
                Some(names) => Some(names.read(measure)),
                None => opened.get_or_insert_with(|| engine::store::Index::open(&saved).ok().filter(|index| index.live() > 0)).as_ref().map(measure),
            };
            let Some((took, names)) = answer else { continue };
            let label = format!("{:.1} ms · {} names on this PC", took.as_secs_f64() * 1000.0, grouped(names));
            let window = window.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = window.upgrade() {
                    REAL_SHOWN.with(|shown| shown.set(true));
                    window.set_search_time(label.into());
                }
            });
        }
    });
    sender
}

/// 9218442 as "9,218,442".
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_digits() {
        assert_eq!(grouped(9_218_442), "9,218,442");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
    }

    #[test]
    fn every_query_finds_something() {
        let names = names();
        let context = Context { now: 1_780_000_000, profile: "c:\\users\\you".into() };
        for query in QUERIES {
            let found = rank::rank(names.iter().cloned(), &Terms::parse(query), &context, 3);
            assert_eq!(found.len(), 3, "{query}");
        }
    }
}
