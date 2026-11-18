//! Each lane's pace, for its time left: items a minute over the time it was working (at least one item in hand), so
//! waiting, a pause or nothing to do doesn't count; weighted to the last few minutes. Kept in status.json's `paces`
//! with when it last finished an item and since when it's been on the one it's on, so a slow lane can be told from a
//! stalled one. And what each lane has in hand: the oldest item's path and how far into it the reader is, where it
//! can tell (`note`, `progress`, from the thread working on it), and how many it has at once.

use config::status::{LANES, Status};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How much working time one sample covers, and how much each new one weighs (about the last few minutes count).
const SAMPLE: Duration = Duration::from_secs(15);
const WEIGHT: f64 = 0.25;
/// The slower average beside it: about the last quarter of an hour of work.
const WEIGHT_LONG: f64 = 0.02;
/// Before the first full sample, a first estimate once this much has been worked.
const FIRST_ESTIMATE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct Pace {
    /// Items finished and working time in the sample being gathered.
    count: u64,
    worked: Duration,
    /// Since when at least one item has been in hand (None: none now).
    busy_since: Option<Instant>,
    /// Each item in hand.
    starts: Vec<InHand>,
    per_minute: Option<f64>,
    per_minute_long: Option<f64>,
    last_progress: i64,
}

/// An item in hand: the thread working on it, when it started (seconds since 1970), and what the thread has said of it.
#[derive(Clone)]
struct InHand {
    thread: std::thread::ThreadId,
    started: i64,
    item: String,
    done: u64,
    total: u64,
    unit: &'static str,
    moved: i64,
    /// How many items it is (`Paces::holding`): a batch of documents sent to a model at once counts as each of them.
    count: u32,
}

impl InHand {
    fn new(started: i64) -> InHand {
        InHand { thread: std::thread::current().id(), started, item: String::new(), done: 0, total: 0, unit: "", moved: 0, count: 1 }
    }
}

impl Pace {
    fn start(&mut self, at: Instant, now: i64) {
        if self.starts.is_empty() {
            self.busy_since = Some(at);
        }
        self.starts.push(InHand::new(now));
    }

    fn finish(&mut self, started: i64, count: u64, at: Instant, now: i64) {
        let thread = std::thread::current().id();
        let place = self.starts.iter().position(|hand| hand.started == started && hand.thread == thread)
            .or_else(|| self.starts.iter().position(|hand| hand.started == started));
        if let Some(place) = place {
            self.starts.remove(place);
        }
        if let Some(since) = self.busy_since {
            self.worked += at.saturating_duration_since(since);
            self.busy_since = if self.starts.is_empty() { None } else { Some(at) };
        }
        self.count += count;
        self.last_progress = now;
        if self.worked >= SAMPLE {
            let sample = self.count as f64 / self.worked.as_secs_f64() * 60.0;
            self.per_minute = Some(self.per_minute.map_or(sample, |old| old * (1.0 - WEIGHT) + sample * WEIGHT));
            self.per_minute_long = Some(self.per_minute_long.map_or(sample, |old| old * (1.0 - WEIGHT_LONG) + sample * WEIGHT_LONG));
            (self.count, self.worked) = (0, Duration::ZERO);
        }
    }

    /// Items a minute: the weighted pace, or the first estimate.
    fn per_minute(&self) -> f64 {
        self.per_minute.unwrap_or_else(|| if self.worked >= FIRST_ESTIMATE { self.count as f64 / self.worked.as_secs_f64() * 60.0 } else { 0.0 })
    }

    fn shown(&self) -> config::status::Pace {
        let round = |value: f64| (value * 100.0).round() / 100.0;
        let oldest = self.starts.iter().min_by_key(|hand| hand.started);
        config::status::Pace {
            per_minute: round(self.per_minute()),
            per_minute_long: round(self.per_minute_long.unwrap_or_else(|| self.per_minute())),
            last_progress: self.last_progress,
            current_since: oldest.map_or(0, |hand| hand.started),
            at_once: self.starts.iter().map(|hand| hand.count).sum(),
            item: oldest.map(|hand| hand.item.clone()).unwrap_or_default(),
            item_done: oldest.map_or(0, |hand| hand.done),
            item_total: oldest.map_or(0, |hand| hand.total),
            item_unit: oldest.map(|hand| hand.unit.to_string()).unwrap_or_default(),
            item_moved: oldest.map_or(0, |hand| hand.moved),
        }
    }
}

/// Every lane's pace.
#[derive(Default)]
pub struct Paces([Mutex<Pace>; LANES.len()]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Documents,
    Images,
    Deep,
    Shown,
    Meaning,
    Said,
}

/// The kind of reading this thread does, as Activity pauses them (`config::KINDS`): by the names run.rs gives the
/// workers ("text 2", "pictures", "deep", "looking siglip2", "listening"). None on any other thread.
pub fn kind_of_thread() -> Option<&'static str> {
    kind_of(std::thread::current().name()?)
}

fn kind_of(thread: &str) -> Option<&'static str> {
    let first = thread.split(' ').next()?;
    Some(match first {
        "text" => "documents",
        "pictures" => "images",
        "deep" => "deep",
        "looking" => "shown",
        "listening" => "said",
        _ => return None,
    })
}

/// An item in hand: finished when dropped, as `count` items (one unless set).
pub struct Item<'a> {
    pace: &'a Mutex<Pace>,
    started: i64,
    pub count: u64,
}

impl Drop for Item<'_> {
    fn drop(&mut self) {
        let mut pace = self.pace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pace.finish(self.started, self.count, Instant::now(), crate::work::now());
    }
}

impl Paces {
    pub fn item(&self, lane: Lane) -> Item<'_> {
        let pace = &self.0[lane as usize];
        let started = crate::work::now();
        pace.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).start(Instant::now(), started);
        Item { pace, started, count: 1 }
    }

    /// The item this thread has in hand is this file (the worker says so once it knows).
    pub fn note(&self, path: &str) {
        self.on_mine(|hand| {
            if hand.item != path {
                hand.item = path.to_string();
                (hand.done, hand.total, hand.unit, hand.moved) = (0, 0, "", 0);
            }
        });
    }

    /// How far into it this thread is: `done` of `total` `unit` ("pages", "seconds"), moved now.
    pub fn progress(&self, done: u64, total: u64, unit: &'static str) {
        let now = crate::work::now();
        self.on_mine(|hand| {
            if done != hand.done || hand.moved == 0 {
                hand.moved = now;
            }
            (hand.done, hand.total, hand.unit) = (done, total, unit);
        });
    }

    /// The item this thread has in hand is `count` items at once (a batch of documents in one request): what Activity
    /// says it has at a time. Before, it counted the threads, so a lane sending 16 documents in one request was "one
    /// at a time".
    pub fn holding(&self, count: u32) {
        self.on_mine(|hand| hand.count = count.max(1));
    }

    fn on_mine(&self, change: impl Fn(&mut InHand)) {
        let thread = std::thread::current().id();
        for pace in &self.0 {
            let mut pace = pace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(hand) = pace.starts.iter_mut().rev().find(|hand| hand.thread == thread) {
                change(hand);
                return;
            }
        }
    }

    /// Into the status, for status.json. A lane whose workers don't name their item says it elsewhere in the status
    /// (what the models are on): that's taken.
    pub fn show(&self, status: &mut Status) {
        for (name, pace) in LANES.iter().zip(&self.0) {
            let pace = pace.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if pace.last_progress > 0 || !pace.starts.is_empty() {
                let mut shown = pace.shown();
                if shown.item.is_empty() && shown.current_since > 0 {
                    shown.item = match *name {
                        "deep" => status.deep_current.clone(),
                        "shown" | "meaning" => status.looking.iter().find(|looking| !looking.current.is_empty()).map(|looking| looking.current.clone()).unwrap_or_default(),
                        "said" => status.listening.as_ref().map(|listening| listening.current.clone()).unwrap_or_default(),
                        _ => String::new(),
                    };
                }
                status.paces.insert(name.to_string(), shown);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pace_counts_working_time_only() {
        let mut pace = Pace::default();
        let start = Instant::now();
        // One item every 2 s for 20 s: 30 a minute.
        for i in 0..10u64 {
            pace.start(start + Duration::from_secs(i * 2), 0);
            pace.finish(0, 1, start + Duration::from_secs(i * 2 + 2), 100);
        }
        assert!((pace.per_minute() - 30.0).abs() < 0.5, "{}", pace.per_minute());
        // An hour paused, then the same pace: still 30, not slowed by the hour.
        let later = start + Duration::from_secs(20 + 3600);
        for i in 0..10u64 {
            pace.start(later + Duration::from_secs(i * 2), 0);
            pace.finish(0, 1, later + Duration::from_secs(i * 2 + 2), 200);
        }
        assert!((pace.per_minute() - 30.0).abs() < 0.5, "{}", pace.per_minute());
        assert_eq!(pace.shown().last_progress, 200);
        assert_eq!(pace.shown().current_since, 0);
    }

    #[test]
    fn workers_side_by_side_count_once() {
        // Four items at once, each taking 4 s, back to back for 20 s: 60 a minute.
        let mut pace = Pace::default();
        let start = Instant::now();
        for round in 0..5u64 {
            let at = start + Duration::from_secs(round * 4);
            for worker in 0..4 {
                pace.start(at, worker);
            }
            for worker in 0..4 {
                pace.finish(worker, 1, at + Duration::from_secs(4), 1);
            }
        }
        // (A sample ends with the item that fills it, the others finishing that moment counted in the next.)
        assert!((pace.per_minute() - 60.0).abs() < 15.0, "{}", pace.per_minute());
        // One in hand: when it started is shown, and the pace weighs a change in a few samples.
        pace.start(start + Duration::from_secs(20), 1234);
        assert_eq!(pace.shown().current_since, 1234);
        let at = start + Duration::from_secs(20);
        pace.finish(1234, 1, at + Duration::from_secs(30), 2);
        assert!(pace.per_minute() < 50.0 && pace.per_minute() > 2.0, "{}", pace.per_minute());
    }

    #[test]
    fn a_batch_counts_as_its_items() {
        let paces = Paces::default();
        {
            let mut batch = paces.item(Lane::Meaning);
            // In hand: 16 documents at once, from one thread.
            paces.holding(16);
            let mut status = Status::default();
            paces.show(&mut status);
            assert_eq!(status.paces["meaning"].at_once, 16);
            batch.count = 16;
        }
        let mut status = Status::default();
        paces.show(&mut status);
        assert!(status.paces.contains_key("meaning") && !status.paces.contains_key("documents"));
        assert_eq!(status.paces["meaning"].at_once, 0);
    }

    #[test]
    fn the_item_in_hand_and_how_far_into_it() {
        let paces = Paces::default();
        let item = paces.item(Lane::Images);
        paces.note(r"C:\Scans\2019.pdf");
        paces.progress(2, 60, "pages");
        let mut status = Status::default();
        paces.show(&mut status);
        let images = &status.paces["images"];
        assert_eq!((images.item.as_str(), images.item_done, images.item_total, images.item_unit.as_str(), images.at_once),
                   (r"C:\Scans\2019.pdf", 2, 60, "pages", 1));
        assert!(images.item_moved > 0 && images.current_since > 0);
        // Another thread's item is its own.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _other = paces.item(Lane::Images);
                paces.note(r"C:\Other.png");
                let mut status = Status::default();
                paces.show(&mut status);
                assert_eq!(status.paces["images"].at_once, 2);
            });
        });
        drop(item);
        let mut status = Status::default();
        paces.show(&mut status);
        assert_eq!((status.paces["images"].at_once, status.paces["images"].item.as_str()), (0, ""));
        // A lane whose workers don't name theirs: what the status says the model is on.
        let _heard = paces.item(Lane::Said);
        let mut status = Status { listening: Some(config::status::Listening { current: r"C:\Talk.mp3".into(), ..Default::default() }), ..Status::default() };
        paces.show(&mut status);
        assert_eq!(status.paces["said"].item, r"C:\Talk.mp3");
    }

    #[test]
    fn the_kind_a_worker_does() {
        assert_eq!(kind_of("text"), Some("documents"));
        assert_eq!(kind_of("text 3"), Some("documents"));
        assert_eq!(kind_of("pictures 2"), Some("images"));
        assert_eq!(kind_of("looking gemma"), Some("shown"));
        assert_eq!(kind_of("listening"), Some("said"));
        assert_eq!(kind_of("deep"), Some("deep"));
        assert_eq!(kind_of("status"), None);
        assert_eq!(kind_of("writer"), None);
    }

    #[test]
    fn the_long_pace_follows_slowly() {
        let mut pace = Pace::default();
        let start = Instant::now();
        // 30 a minute for a while, then 60: the short pace follows within a few samples, the long one slowly.
        for i in 0..20u64 {
            pace.start(start + Duration::from_secs(i * 2), 0);
            pace.finish(0, 1, start + Duration::from_secs(i * 2 + 2), 1);
        }
        let later = start + Duration::from_secs(40);
        for i in 0..120u64 {
            pace.start(later + Duration::from_secs(i), 0);
            pace.finish(0, 1, later + Duration::from_secs(i + 1), 2);
        }
        let shown = pace.shown();
        assert!(shown.per_minute > 50.0, "{}", shown.per_minute);
        assert!(shown.per_minute_long < 40.0 && shown.per_minute_long > 29.0, "{}", shown.per_minute_long);
    }
}
