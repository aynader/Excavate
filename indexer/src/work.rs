//! The work: two lanes, so documents never wait behind pictures. Documents and code are read by one worker (cheap);
//! pictures and scanned pages by another, with OCR. Each lane takes the newest files first, and changes the person
//! just made before anything else; in a large library the first sweep's images go ahead of the rest. One writer thread
//! puts what's read into the index and commits every two seconds, so it shows up in the popup as it goes; the catalog
//! records each file once its words are committed.
//!
//! Heavy models run only when the PC is free: the CPU's while it's idle or other programs aren't using it much, the GPU's
//! while other programs aren't using the graphics card much and its memory has room. A thread looks every few seconds
//! (`watch_load`). Documents and code, which cost little, go on at idle priority.

use crate::catalog::{self, Catalog, State, Update};
use crate::control::Control;
use crate::deep;
use crate::models::Size;
use crate::ocr::{self, Ocr};
use crate::scan::FileInfo;
use config::Schedule;
use config::status::Status;
use extract::Reader;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Text,
    Pictures,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub file: FileInfo,
    /// A PDF's scanned pages to OCR, with its text layer (already read).
    pub pages: Vec<u16>,
    pub text_layer: String,
    /// Its place in the first sweep.
    pub place: sweep::Place,
}

impl Job {
    pub fn new(file: FileInfo) -> Job {
        Job { file, pages: Vec::new(), text_layer: String::new(), place: sweep::Place::default() }
    }
}

struct Queued {
    priority: i64,
    sequence: u64,
    job: Job,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.priority, self.sequence) == (other.priority, other.sequence)
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Highest priority first; among equals, the earlier pushed.
        self.priority.cmp(&other.priority).then(other.sequence.cmp(&self.sequence))
    }
}

#[derive(Default)]
struct Lanes {
    text: BinaryHeap<Queued>,
    pictures: BinaryHeap<Queued>,
    queued: HashSet<(String, bool)>,
    sequence: u64,
    closed: bool,
    /// Images that come after the first sweep.
    later: u64,
}

#[derive(Default)]
pub struct Queue {
    lanes: Mutex<Lanes>,
    ready: Condvar,
}

/// Priority of a file the person just changed: above every listed file (whose priority is its date).
pub const NOW: i64 = i64::MAX / 2;

/// An image set aside by the first sweep comes after all the others: its date minus this (below zero, whatever its date).
const AFTER_THE_SWEEP: i64 = 1 << 40;

/// A listed file's priority.
pub fn priority(job: &Job) -> i64 {
    if job.place.later { job.file.modified - AFTER_THE_SWEEP } else { job.file.modified }
}

impl Queue {
    fn lanes(&self) -> std::sync::MutexGuard<'_, Lanes> {
        self.lanes.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn push(&self, job: Job, priority: i64) {
        let lane = lane_of(&job);
        let mut lanes = self.lanes();
        let key = (catalog::id(&job.file.path), lane == Lane::Pictures);
        if !lanes.queued.insert(key) {
            return;
        }
        lanes.sequence += 1;
        lanes.later += u64::from(priority < 0);
        let queued = Queued { priority, sequence: lanes.sequence, job };
        match lane {
            Lane::Text => lanes.text.push(queued),
            Lane::Pictures => lanes.pictures.push(queued),
        }
        drop(lanes);
        self.ready.notify_all();
    }

    /// The next job of a lane, waiting at most `wait`: Ok(None) when none came, Err(()) once closed.
    pub fn pop_within(&self, lane: Lane, wait: std::time::Duration) -> Result<Option<Job>, ()> {
        let deadline = Instant::now() + wait;
        let mut lanes = self.lanes();
        loop {
            if lanes.closed {
                return Err(());
            }
            let next = match lane {
                Lane::Text => lanes.text.pop(),
                Lane::Pictures => lanes.pictures.pop(),
            };
            if let Some(queued) = next {
                lanes.queued.remove(&(catalog::id(&queued.job.file.path), lane == Lane::Pictures));
                lanes.later -= u64::from(queued.priority < 0);
                return Ok(Some(queued.job));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            lanes = self.ready.wait_timeout(lanes, deadline - now).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }

    /// The next job of a lane; waits for one. None once closed.
    pub fn pop(&self, lane: Lane) -> Option<Job> {
        let mut lanes = self.lanes();
        loop {
            if lanes.closed {
                return None;
            }
            let next = match lane {
                Lane::Text => lanes.text.pop(),
                Lane::Pictures => lanes.pictures.pop(),
            };
            if let Some(queued) = next {
                lanes.queued.remove(&(catalog::id(&queued.job.file.path), lane == Lane::Pictures));
                lanes.later -= u64::from(queued.priority < 0);
                return Some(queued.job);
            }
            lanes = self.ready.wait(lanes).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub fn close(&self) {
        self.lanes().closed = true;
        self.ready.notify_all();
    }

    /// Waiting: documents and code, images, and of those the ones that come after the first sweep.
    pub fn lengths(&self) -> (u64, u64, u64) {
        let lanes = self.lanes();
        (lanes.text.len() as u64, lanes.pictures.len() as u64, lanes.later)
    }
}

pub fn lane_of(job: &Job) -> Lane {
    if job.file.reader == Reader::Picture || !job.pages.is_empty() { Lane::Pictures } else { Lane::Text }
}

/// What the workers hand the writer.
pub enum Write {
    /// Words for the index, and the file's record.
    Put(Box<content::Doc>, Update),
    /// A file with nothing to index (empty, or it failed): its record, and its old words gone.
    Record(Update),
    /// A file that's gone, or no longer wanted.
    Remove(String),
    /// Words for the index of a file with no record of its own (a recording: the catalog keeps who heard it).
    Words(Box<content::Doc>),
    /// No words for such an entry any more (a video that shows none, or where no one speaks): out of the index, its
    /// records kept. The entry's id (`content::entry_id`).
    Unindex(String),
}

/// What the workers share with the status writer.
#[derive(Default)]
pub struct Shared {
    pub status: Mutex<Status>,
    pub listing: std::sync::atomic::AtomicBool,
    /// Why each lane waits, if it does: documents, images, Deep.
    pub paused: Mutex<[Option<String>; 3]>,
    picture_times: Mutex<VecDeque<f64>>,
    pub busy: std::sync::atomic::AtomicUsize,
    /// Deep's pass is on: its server is up or starting.
    pub deep_on: std::sync::atomic::AtomicBool,
    /// Whether the CPU's and the GPU's models have to wait for a quiet moment (`watch_load`): another program uses
    /// nearly all of it.
    pub cpu_busy: AtomicBool,
    pub gpu_busy: AtomicBool,
    /// Whether they go on at a slower pace: another program uses a good part of it, not all.
    pub cpu_slow: AtomicBool,
    pub gpu_slow: AtomicBool,
    /// What's keeping each busy, by the name Windows shows ("Python"), while it is.
    pub cpu_by: Mutex<String>,
    pub gpu_by: Mutex<String>,
    /// Deep's server, to leave its use out of what the PC is judged by (0: none).
    pub server: AtomicU32,
    /// The server of a model that looks at images, the same way.
    pub seeing_server: AtomicU32,
    /// And the listener, while it runs on the graphics card.
    pub listen_server: AtomicU32,
    /// Whether something heavy has work: otherwise there's no need to look.
    pub wants_load: AtomicBool,
    /// Whether Deep's and the seeing models' servers run on the graphics card (else on the processor): what they hold
    /// of its memory counts as room only then.
    pub deep_gpu: AtomicBool,
    pub seeing_gpu: AtomicBool,
}

/// When new pictures are read. Documents never wait for this; pictures (and scanned pages) do, and Deep's pass with
/// them. The first time, they're read right away ("runs once now"); after that, by the schedule. Looking at what images
/// show has a gate of its own (`Gate::seeing`), with its own schedule.
pub struct Gate {
    schedule: Schedule,
    /// Where in the catalog's meta it keeps when it last ran out of work, and when everything was last done in one sitting.
    done_key: &'static str,
    all_key: &'static str,
    every_days: i64,
    minutes: i64,
    requested_at: i64,
    catalog: Arc<Catalog>,
    done_at: AtomicI64,
    /// "Read everything now": on until everything's read, or "Stop". No schedule, no waiting for a quiet moment.
    all: AtomicBool,
    first_run: bool,
}

const PICTURES_DONE_AT: &str = "pictures-done-at";
const ALL_DONE_AT: &str = "all-done-at";
const SEEING_DONE_AT: &str = "seeing-done-at";
const SEEING_ALL_DONE_AT: &str = "seeing-all-done-at";
const LISTEN_DONE_AT: &str = "listen-done-at";
const LISTEN_ALL_DONE_AT: &str = "listen-all-done-at";

impl Gate {
    /// Reading the text in images.
    pub fn new(settings: &config::Settings, catalog: Arc<Catalog>) -> Gate {
        Gate::with(settings.schedule(), &settings.schedule_every, settings.schedule_minutes(), settings.read_pictures_at,
                   settings.read_all_at, catalog, (PICTURES_DONE_AT, ALL_DONE_AT))
    }

    /// Looking at what images show.
    pub fn seeing(settings: &config::Settings, catalog: Arc<Catalog>) -> Gate {
        Gate::with(settings.seeing_schedule(), &settings.seeing_schedule_every, settings.seeing_schedule_minutes(), settings.look_new_at,
                   settings.look_all_at, catalog, (SEEING_DONE_AT, SEEING_ALL_DONE_AT))
    }

    /// Listening to recordings.
    pub fn listening(settings: &config::Settings, catalog: Arc<Catalog>) -> Gate {
        Gate::with(settings.listen_schedule(), &settings.listen_schedule_every, settings.listen_schedule_minutes(), settings.listen_new_at,
                   settings.listen_all_at, catalog, (LISTEN_DONE_AT, LISTEN_ALL_DONE_AT))
    }

    fn with(schedule: Schedule, every: &str, minutes: u32, requested_at: i64, all_at: i64, catalog: Arc<Catalog>,
            (done_key, all_key): (&'static str, &'static str)) -> Gate {
        let every_days = match every {
            "week" => 7,
            "month" => 30,
            _ => 1,
        };
        let done_at = catalog.meta(done_key);
        let all = all_at > catalog.meta(all_key);
        Gate {
            schedule,
            done_key,
            all_key,
            every_days,
            minutes: i64::from(minutes),
            requested_at,
            catalog,
            done_at: AtomicI64::new(done_at),
            all: AtomicBool::new(all),
            first_run: done_at == 0,
        }
    }

    /// Reading everything in one sitting.
    pub fn all(&self) -> bool {
        self.all.load(Ordering::Relaxed)
    }

    /// Everything's been read, in one sitting.
    pub fn all_finished(&self) {
        self.all.store(false, Ordering::Relaxed);
        self.catalog.set_meta(self.all_key, now());
    }

    /// The very first time images are read: what waits for its time afterwards is what the first sweep set aside.
    pub fn first_run(&self) -> bool {
        self.first_run
    }

    pub fn waits(&self) -> bool {
        self.schedule != Schedule::Arrive
    }

    pub fn open(&self) -> bool {
        if self.all() {
            return true;
        }
        let done_at = self.done_at.load(Ordering::Relaxed);
        match self.schedule {
            Schedule::Arrive => true,
            Schedule::Manual => done_at == 0 || self.requested_at > done_at,
            Schedule::Scheduled => done_at == 0 || self.requested_at > done_at || now() >= self.next_after(done_at),
        }
    }

    /// When it opens next on its own (seconds since 1970; 0 when only a click opens it).
    pub fn next(&self) -> i64 {
        match self.schedule {
            Schedule::Scheduled => self.next_after(self.done_at.load(Ordering::Relaxed)),
            _ => 0,
        }
    }

    /// The chosen time of day, the chosen number of days after the last run's date.
    fn next_after(&self, done_at: i64) -> i64 {
        next_run(done_at, self.every_days, self.minutes, winsys::process::utc_offset())
    }

    /// Everything that was waiting has been read: it waits again.
    pub fn finished(&self) {
        let at = now();
        self.done_at.store(at, Ordering::Relaxed);
        self.catalog.set_meta(self.done_key, at);
    }
}

fn next_run(done_at: i64, every_days: i64, minutes: i64, offset: i64) -> i64 {
    let day = (done_at + offset).div_euclid(86_400);
    (day + every_days) * 86_400 + minutes * 60 - offset
}

impl Shared {
    pub fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn picture_took(&self, ms: f64) {
        let mut times = self.picture_times.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        times.push_back(ms);
        if times.len() > 64 {
            times.pop_front();
        }
        let mut sorted: Vec<f64> = times.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];
        self.status().picture_ms = Some((median * 10.0).round() / 10.0);
    }

    fn set_current(&self, path: &str) {
        self.status().current = path.to_string();
    }
}

/// Puts what the workers read into the index, committing every two seconds (or 500 files), then records it.
pub fn write(receiver: Receiver<Write>, mut writer: content::Writer, catalog: Arc<Catalog>, shared: Arc<Shared>) {
    let mut updates: Vec<Update> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut last_commit = Instant::now();
    let commit = |writer: &mut content::Writer, updates: &mut Vec<Update>, removed: &mut Vec<String>| {
        if writer.uncommitted == 0 && updates.is_empty() && removed.is_empty() {
            return;
        }
        match writer.commit() {
            Ok(()) => {
                catalog.apply(updates, removed);
                updates.clear();
                removed.clear();
                shared.status().documents = writer.documents();
            }
            Err(error) => config::log(&format!("index commit: {error}")),
        }
    };
    loop {
        // Nothing waiting to be committed: nothing to do until a worker sends something (or they're all gone).
        let nothing_pending = writer.uncommitted == 0 && updates.is_empty() && removed.is_empty();
        let received = if nothing_pending { receiver.recv().map_err(|_| RecvTimeoutError::Disconnected) } else { receiver.recv_timeout(Duration::from_millis(500)) };
        match received {
            Ok(Write::Put(doc, update)) => {
                if let Err(error) = writer.put(&doc) {
                    config::log(&format!("index {}: {error}", doc.path));
                }
                updates.push(update);
            }
            Ok(Write::Words(doc)) => {
                if let Err(error) = writer.put(&doc) {
                    config::log(&format!("index {}: {error}", doc.path));
                }
            }
            Ok(Write::Record(update)) => {
                writer.remove(&update.path);
                updates.push(update);
            }
            Ok(Write::Unindex(id)) => writer.remove_entry(&id),
            Ok(Write::Remove(path)) => {
                writer.remove(&path);
                removed.push(path);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // (A recording's words come with no record: what's uncommitted counts too.)
        let pending = updates.len() + removed.len() + writer.uncommitted;
        if pending > 0 && (pending >= 500 || last_commit.elapsed() >= Duration::from_secs(2)) {
            commit(&mut writer, &mut updates, &mut removed);
            last_commit = Instant::now();
        }
    }
    commit(&mut writer, &mut updates, &mut removed);
    writer.close();
}

/// What a lane's work asks of the PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Documents and code: cheap, at idle priority.
    Little,
    /// A model on the CPU.
    Cpu,
    /// A model on the graphics card.
    Gpu,
}

impl Need {
    fn lane(self) -> usize {
        match self {
            Need::Little => 0,
            Need::Cpu => 1,
            Need::Gpu => 2,
        }
    }
}

/// Other programs' use of the CPU that counts as busy, and that counts as quiet again. Measured on an idle desktop (with
/// a browser and this app open): 12 to 17% of an i9-13900HX. Twice as much is allowed once the person has been away.
/// Busy, work goes on at a slower pace (`SLOW_SHARE`); only nearly full does it wait. (Excavate's processes run at idle priority, so on the CPU they only ever get what's left.)
const CPU_BUSY: f32 = 0.40;
const CPU_QUIET: f32 = 0.30;
const CPU_FULL: f32 = 0.85;
const CPU_ROOM: f32 = 0.75;
/// Other programs' use of the graphics card's busiest engine. Measured the same way: 16 to 20%.
const GPU_BUSY: f32 = 0.45;
const GPU_QUIET: f32 = 0.30;
const GPU_FULL: f32 = 0.85;
const GPU_ROOM: f32 = 0.70;
/// At the slower pace, the share of the time spent working: after each item, a rest as long as the item took.
const SLOW_SHARE: f64 = 0.5;
/// The person counts as away after this long without input.
const AWAY: Duration = Duration::from_secs(120);
/// Free memory Deep needs on the graphics card to start, with some to spare (it takes 1.9 GB), in megabytes.
const GPU_ROOM_MB: u64 = 3000;
const DEEP_USES_MB: u64 = 1900;
/// What EmbeddingGemma 2 takes while looking at images (1.7 GB measured); Qwen's 8B takes more, and is only offered with
/// room for it.
const SEEING_USES_MB: u64 = 1700;
/// What the listener takes on the graphics card: 1.2 GB for Whisper turbo, 1.9 GB for large-v3.
const LISTEN_USES_MB: u64 = 1200;
/// How often it looks.
const LOOK_EVERY: Duration = Duration::from_secs(4);

/// Looks at how busy the PC is, every few seconds while something heavy has work. The verdicts have some patience, so they
/// don't flap (`winsys::load::Quiet`).
pub fn watch_load(shared: &Shared, gates: &[&Gate], control: &Control) {
    let mut sampler = winsys::load::Sampler::new();
    let (mut cpu, mut gpu) = (winsys::load::Quiet::default(), winsys::load::Quiet::default());
    let (mut cpu_full, mut gpu_full) = (winsys::load::Quiet::default(), winsys::load::Quiet::default());
    // Names asked once per process: reading a file's version block isn't free.
    let mut names: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut name_of = move |process: u32, file: &str| -> String {
        names.entry(process).or_insert_with(|| winsys::process::friendly_name(process).unwrap_or_else(|| file.trim_end_matches(".exe").to_string())).clone()
    };
    let me = std::process::id();
    sampler.sample(&[me]);
    let mut looked = false;
    loop {
        if !control.sleep(if looked { LOOK_EVERY } else { Duration::from_secs(1) }) {
            return;
        }
        if gates.iter().all(|gate| gate.all()) || !shared.wants_load.load(Ordering::Relaxed) {
            // (The next look covers a long stretch, which is harmless: it's one look.)
            for flag in [&shared.cpu_busy, &shared.gpu_busy, &shared.cpu_slow, &shared.gpu_slow] {
                flag.store(false, Ordering::Relaxed);
            }
            looked = false;
            continue;
        }
        let server = shared.server.load(Ordering::Relaxed);
        let seeing = shared.seeing_server.load(Ordering::Relaxed);
        let listen = shared.listen_server.load(Ordering::Relaxed);
        let load = sampler.sample(&[me, server, seeing, listen]);
        looked = true;
        // (`EXCAVATE_PRESENT`: as if the person were at the PC, for measuring the waits while nobody is.)
        let away = load.idle >= AWAY && std::env::var_os("EXCAVATE_PRESENT").is_none();
        let cpu_load = if away { load.cpu / 2.0 } else { load.cpu };
        // Its own memory on the card is room it has: as Windows counts it per process (this process too: text in
        // videos' detector), else by what each model is known to take.
        let (deep_gpu, seeing_gpu) = (shared.deep_gpu.load(Ordering::Relaxed), shared.seeing_gpu.load(Ordering::Relaxed));
        let room = if load.gpu_own_counted { load.gpu_free_mb } else {
            load.gpu_free_mb.map(|free| free + if server != 0 && deep_gpu { DEEP_USES_MB } else { 0 } + if seeing != 0 && seeing_gpu { SEEING_USES_MB } else { 0 }
                + if listen != 0 { LISTEN_USES_MB } else { 0 })
        };
        let gpu_load = if room.is_some_and(|room| room < GPU_ROOM_MB) { 1.0 } else { load.gpu };
        let (cpu_slow, gpu_slow) = (!cpu.update(cpu_load, CPU_BUSY, CPU_QUIET), !gpu.update(gpu_load, GPU_BUSY, GPU_QUIET));
        let (cpu_busy, gpu_busy) = (!cpu_full.update(cpu_load, CPU_FULL, CPU_ROOM), !gpu_full.update(gpu_load, GPU_FULL, GPU_ROOM));
        // With what, while it's anything but quiet.
        // (Looked at from a little below busy: a name needs two looks, and is ready by the time it's busy.)
        let cpu_by = if cpu_load > CPU_QUIET { sampler.cpu_top(&[me, server, seeing, listen]).map(|top| name_of(top.process, &top.file)) } else { None };
        let cpu_by = cpu_by.filter(|_| cpu_slow || cpu_busy);
        let gpu_by = if gpu_slow || gpu_busy { load.gpu_top.map(|process| name_of(process, "")) } else { None };
        *shared.cpu_by.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = cpu_by.clone().unwrap_or_default();
        *shared.gpu_by.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = gpu_by.clone().unwrap_or_default();
        let mut changed = false;
        for (flag, value) in [(&shared.cpu_busy, cpu_busy), (&shared.gpu_busy, gpu_busy), (&shared.cpu_slow, cpu_slow), (&shared.gpu_slow, gpu_slow)] {
            changed |= flag.swap(value, Ordering::Relaxed) != value;
        }
        if changed {
            let state = |busy: bool, slow: bool| if busy { "busy" } else if slow { "slower" } else { "free" };
            config::log(&format!("load: CPU {:.0}%, GPU {:.0}%, {:?} MB free, away {away}: CPU {} ({}), GPU {} ({})", load.cpu * 100.0, load.gpu * 100.0,
                                 load.gpu_free_mb, state(cpu_busy, cpu_slow), cpu_by.unwrap_or_default(), state(gpu_busy, gpu_slow), gpu_by.unwrap_or_default()));
        }
    }
}

/// Why this kind of work waits now, if it does.
pub fn pause_reason(context: &Context, need: Need) -> Option<String> {
    reason(context, need, context.read_on_battery, &context.gate)
}

/// The same for looking at what images show, by its own choices.
pub fn seeing_pause(context: &Context, need: Need) -> Option<String> {
    reason(context, need, context.look_on_battery, &context.seeing_gate)
}

/// And for listening to recordings.
pub fn listen_pause(context: &Context, need: Need) -> Option<String> {
    reason(context, need, context.listen_on_battery, &context.listen_gate)
}

fn reason(context: &Context, need: Need, on_battery: bool, gate: &Gate) -> Option<String> {
    if context.paused_until == -1 || context.paused_until > now() {
        return Some(context.pause_text.clone());
    }
    if !on_battery && winsys::process::on_battery() {
        return Some("Paused on battery power".into());
    }
    if winsys::process::person_busy() {
        return Some("Paused while a game or full-screen app runs".into());
    }
    if gate.all() {
        return None;
    }
    // The graphics card's work needs a little of the processor too: a processor others use fully starves it.
    let shared = &context.shared;
    let (cpu, gpu) = (shared.cpu_busy.load(Ordering::Relaxed), shared.gpu_busy.load(Ordering::Relaxed));
    let (cpu, gpu) = match need {
        Need::Little => (false, false),
        Need::Cpu => (cpu, false),
        Need::Gpu => (cpu, gpu),
    };
    (cpu || gpu).then(|| format!("Waiting for a quiet moment: {}", busy_with(shared, cpu, gpu)))
}

/// "your graphics card is busy with Python", with Excavate's names for the two.
fn busy_with(shared: &Shared, cpu: bool, gpu: bool) -> String {
    let name = |by: &Mutex<String>| by.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    let (cpu_by, gpu_by) = (if cpu { name(&shared.cpu_by) } else { String::new() }, if gpu { name(&shared.gpu_by) } else { String::new() });
    let device = match (cpu, gpu) {
        (true, true) => "your processor and graphics card are busy",
        (false, true) => "your graphics card is busy",
        _ => "your processor is busy",
    };
    let mut by: Vec<String> = [gpu_by, cpu_by].into_iter().filter(|by| !by.is_empty()).collect();
    by.dedup();
    if by.is_empty() { device.to_string() } else { format!("{device} with {}", by.join(" and ")) }
}

/// Whether this kind of work goes at the slower pace now, and why (`SLOW_SHARE`).
fn slow_reason(context: &Context, need: Need) -> Option<String> {
    let shared = &context.shared;
    let (cpu, gpu) = (shared.cpu_slow.load(Ordering::Relaxed), shared.gpu_slow.load(Ordering::Relaxed));
    let (cpu, gpu) = match need {
        Need::Little => (false, false),
        Need::Cpu => (cpu, false),
        Need::Gpu => (false, gpu),
    };
    (cpu || gpu).then(|| format!("Going slower: {}", busy_with(shared, cpu, gpu)))
}

/// Waits while this kind of work has to; false when stopping.
pub fn wait_until_free(context: &Context, need: Need) -> bool {
    wait_with(context, need, pause_reason, |context| &context.gate, None)
}

/// The same for looking at what images show (`seeing_pause`), telling `note` why it waits each time it checks (None
/// once it doesn't).
pub fn wait_seeing(context: &Context, need: Need, note: &dyn Fn(Option<&str>)) -> bool {
    wait_with(context, need, seeing_pause, |context| &context.seeing_gate, Some(note))
}

/// And for listening (`listen_pause`).
pub fn wait_listening(context: &Context, need: Need, note: &dyn Fn(Option<&str>)) -> bool {
    wait_with(context, need, listen_pause, |context| &context.listen_gate, Some(note))
}

/// Waits while `why` says so. Reading's reasons go to its lane's line in the status; looking's, to `note`.
fn wait_with(context: &Context, need: Need, why: fn(&Context, Need) -> Option<String>, gate: fn(&Context) -> &Gate, note: Option<&dyn Fn(Option<&str>)>) -> bool {
    use std::sync::atomic::AtomicU8;
    // Efficiency mode follows the power source: 0 not set yet, 1 on (battery), 2 off.
    static MODE: AtomicU8 = AtomicU8::new(0);
    let wanted = if winsys::process::on_battery() { 1 } else { 2 };
    if MODE.swap(wanted, Ordering::Relaxed) != wanted {
        winsys::process::efficiency_mode(wanted == 1);
    }
    // When this thread last went back to work: what it did since is the item it just finished.
    thread_local! {
        static WORKING_SINCE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    }
    let worked = WORKING_SINCE.with(|since| since.get()).map(|since| since.elapsed());
    let mut rested = false;
    loop {
        if context.control.stopping() {
            return false;
        }
        let reason = why(context, need);
        let waiting = reason.is_some();
        // Not waiting: perhaps slower. A rest as long as the last item took (up to 10 s), once.
        // ("Read everything now" goes at full pace.)
        let slow = if waiting || gate(context).all() { None } else { slow_reason(context, need) };
        let shown = reason.clone().or(slow.clone());
        match note {
            Some(note) => note(shown.as_deref()),
            None => context.shared.paused.lock().unwrap_or_else(|poisoned| poisoned.into_inner())[need.lane()] = shown,
        }
        if !waiting {
            if let (Some(_), Some(worked), false) = (&slow, worked, rested) {
                rested = true;
                let rest = worked.mul_f64((1.0 - SLOW_SHARE) / SLOW_SHARE).min(Duration::from_secs(10));
                if !context.control.sleep(rest) {
                    return false;
                }
                continue;
            }
            WORKING_SINCE.with(|since| since.set(Some(Instant::now())));
            return true;
        }
        if !context.control.sleep(Duration::from_secs(5)) {
            return false;
        }
    }
}

/// Counts the worker as busy while alive (for "reading" vs "idle").
pub struct Busy<'a>(&'a Shared);

impl<'a> Busy<'a> {
    pub fn new(shared: &'a Shared) -> Busy<'a> {
        shared.busy.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Busy(shared)
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

pub struct Context {
    pub queue: Arc<Queue>,
    pub sender: Sender<Write>,
    pub control: &'static Control,
    pub shared: Arc<Shared>,
    pub pictures: bool,
    pub read_on_battery: bool,
    pub gate: Arc<Gate>,
    /// Looking at what images show: its own gate and battery choice.
    pub seeing_gate: Arc<Gate>,
    pub look_on_battery: bool,
    /// Listening to recordings: the same.
    pub listen_gate: Arc<Gate>,
    pub listen_on_battery: bool,
    /// Paused by the person: until when (-1: until turned back on), and how it's said.
    pub paused_until: i64,
    pub pause_text: String,
    /// The chosen folders and what's left out: Deep's list comes from the catalog, so it checks.
    pub roots: Vec<String>,
    pub exclusions: config::Exclusions,
    /// What each chosen folder leaves out.
    pub kinds: config::FolderKinds,
    /// A video's frames: every so many seconds, at most so many (`Settings::video_pace`); and "Look at my videos again".
    pub video_pace: (f64, f64),
    pub relook_videos_at: i64,
    /// Whether videos are looked at for what they show, and whether (and how often, seconds) their text is read.
    pub videos_shown: bool,
    pub video_text: bool,
    pub video_text_pace: f64,
    /// Where each runs (chosen on the Images cards, `Settings::reading_device` / `seeing_device`): text in images on
    /// the graphics card (DirectML); Thorough's second read; the models that look at what images show; and SigLIP 2
    /// among them (on the graphics card only when it's alone, Quick: beside EmbeddingGemma 2 it runs on the processor).
    pub reading_gpu: bool,
    /// Text in videos' detector (run.rs: the choice for reading text when there is one).
    pub video_text_gpu: bool,
    pub deep_gpu: bool,
    pub seeing_gpu: bool,
    pub siglip_gpu: bool,
    pub paths: config::Paths,
    /// What to measure the first time it runs, if anything (`devices`): "seeing:gemma", "listening:balanced". (Not
    /// text in images: it reads on the processor unless chosen on its card, `Settings::reading_device`.)
    pub seeing_measure: Option<String>,
    pub listen_measure: Option<String>,
}

/// The facts of a file as it is now; None (and its removal sent) when it's gone.
pub fn current(context: &Context, file: &FileInfo) -> Option<(u64, i64)> {
    use std::os::windows::fs::MetadataExt;
    match std::fs::metadata(&file.path) {
        // Moved online since it was listed (OneDrive's "Free up space"): left alone, not downloaded.
        Ok(metadata) if metadata.is_file() && !crate::scan::on_this_pc(metadata.file_attributes()) => None,
        Ok(metadata) if metadata.is_file() => Some((metadata.len(), crate::scan::seconds(&metadata))),
        _ => {
            let _ = context.sender.send(Write::Remove(file.path.clone()));
            None
        }
    }
}

fn update(file: &FileInfo, size: u64, modified: i64, state: State, engine: &str, error: String, characters: usize) -> Update {
    Update {
        path: file.path.clone(),
        size,
        modified,
        kind: file.reader.kind(),
        state,
        engine: engine.to_string(),
        read_at: now(),
        error,
        characters: characters as u64,
        deep: catalog::DEEP_NO,
        group: 0,
        later: false,
        hash: 0,
    }
}

/// Files (of any kind) whose stored text shows a secret the detector finds now and didn't when they were read: a value on
/// the line under its label, an identity number after its label; and images about Wi-Fi with nothing
/// covered, for a QR code with the network's password (`codes`). Their text in the index still has the secret, and their
/// previews nothing covered, until they're read again.
pub fn files_with_unmarked_secrets(index: &Path) -> Vec<String> {
    let Ok(reader) = content::Reader::open(index) else { return Vec::new() };
    // (The labels secrets leave in the text are words too: "‹password›" is found by "password".)
    let words = ["password", "passwort", "kennwort", "passphrase", "psk", "pin", "key", "wifi", "wlan", "network", "wireless",
                 "code", "token", "secret", "id", "national", "nid", "personal", "ssn", "nin", "nic", "passport", "ssid", "wpa",
                 "wpa2", "scan", "connect", "card", "iban", "number", "phrase"];
    let mut found: Vec<String> = Vec::new();
    let mut looked: HashSet<String> = HashSet::new();
    for word in words {
        let query = content::Query { words: vec![word.into()], prefix: false, inside_only: true, ..content::Query::default() };
        for hit in reader.search(&query, 10_000) {
            if !looked.insert(hit.path.to_lowercase()) {
                continue;
            }
            let Some(stored) = reader.stored(&hit.path) else { continue };
            let marks: serde_json::Value = serde_json::from_slice(&stored.marks).unwrap_or(serde_json::Value::Null);
            let covered = marks["kinds"].as_array().is_some_and(|kinds| !kinds.is_empty());
            let wifi_image = hit.kind == content::kind::PICTURE && !covered && privacy::about_wifi(&stored.text);
            let unplaced_pdf = covered && hit.path.to_lowercase().ends_with(".pdf") && marks["pdf"].is_null() && marks["picture"].is_null();
            if !privacy::find(&stored.text).is_empty() || wifi_image || unplaced_pdf {
                found.push(hit.path);
            }
        }
    }
    found
}

/// Secrets out (labels in), and where they were (kinds, for the preview).
pub fn private(text: &str) -> (String, Vec<privacy::Secret>) {
    let secrets = privacy::find(text);
    (privacy::redact(text, &secrets), secrets)
}

pub fn kinds(secrets: &[privacy::Secret]) -> Vec<&'static str> {
    let mut kinds: Vec<&'static str> = secrets.iter().map(|secret| secret.kind.name()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

/// The documents-and-code lane (mostly reading files: low I/O priority).
pub fn read_text(context: Context) {
    winsys::process::background_thread();
    while let Some(job) = context.queue.pop(Lane::Text) {
        let _busy = Busy::new(&context.shared);
        if !wait_until_free(&context, Need::Little) {
            break;
        }
        let Some((size, modified)) = current(&context, &job.file) else { continue };
        context.shared.set_current(&job.file.path);
        let file = &job.file;
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract::extract(Path::new(&file.path), file.reader)))
            .unwrap_or_else(|_| Err("the reader stopped on this file".into()));
        match read {
            Ok(extracted) => {
                let (body, secrets) = private(&extracted.text);
                let characters = body.chars().filter(|c| !c.is_whitespace()).count();
                let scanned = !extracted.scanned_pages.is_empty() && context.pictures;
                if characters == 0 && !scanned {
                    let _ = context.sender.send(Write::Record(update(file, size, modified, State::Empty, "", String::new(), 0)));
                    continue;
                }
                if characters > 0 {
                    let mut doc = doc(file, size, modified, body.clone(), content::Source::Text, &secrets, None, 0);
                    // A PDF's secrets, placed on its first page: only they are covered in its preview (and nothing, when
                    // none is on that page). Without places (a turned page, a scan), the page is covered whole.
                    if !secrets.is_empty() && file.reader == extract::Reader::Pdf {
                        let values: Vec<String> = secrets.iter().filter_map(|secret| extracted.text.get(secret.range.clone()).map(str::to_string)).collect();
                        if let Ok(Some(places)) = extract::pdf::first_page_places(Path::new(&file.path), &values) {
                            let mut marks: serde_json::Value = serde_json::from_slice(&doc.marks).unwrap_or_else(|_| serde_json::json!({}));
                            marks["pdf"] = serde_json::json!({ "boxes": places.iter().map(|place| place.map(|value| (value * 10_000.0).round() as i32)).collect::<Vec<_>>() });
                            doc.marks = serde_json::to_vec(&marks).unwrap_or_default();
                        }
                    }
                    let _ = context.sender.send(Write::Put(Box::new(doc), update(file, size, modified, State::Read, "", String::new(), characters)));
                }
                if scanned {
                    let pages = extracted.scanned_pages.into_iter().take(SCANNED_PAGES).collect();
                    context.queue.push(Job { pages, text_layer: body, ..Job::new(file.clone()) }, modified);
                }
            }
            Err(error) => {
                let _ = context.sender.send(Write::Record(update(file, size, modified, State::Failed, "", error, 0)));
            }
        }
    }
}

/// How much the indexer's memory (private bytes) may grow past what it was once the OCR reader had read its first
/// picture, before the reader is loaded fresh. Growth, not a total: on the graphics card the reader's own allocations
/// count as private bytes too (several GB), and a total of 1 GB had it loaded fresh after nearly every picture
/// (~0.3 s each).
const RECYCLE_GROWTH: u64 = 1 << 30;

/// Scanned pages read per PDF, at most (a 300-page scanned book is read as far as this).
const SCANNED_PAGES: usize = 60;

#[allow(clippy::too_many_arguments)]
fn doc(file: &FileInfo, size: u64, modified: i64, body: String, source: content::Source, secrets: &[privacy::Secret],
       picture: Option<serde_json::Value>, similar: u32) -> content::Doc {
    let marks = if secrets.is_empty() && picture.is_none() && similar == 0 {
        Vec::new()
    } else {
        let mut marks = serde_json::json!({ "kinds": kinds(secrets) });
        if let Some(picture) = picture {
            marks["picture"] = picture;
        }
        if similar > 0 {
            marks["similar"] = similar.into();
        }
        serde_json::to_vec(&marks).unwrap_or_default()
    };
    content::Doc {
        path: file.path.clone(),
        name: file.name.clone(),
        kind: file.reader.kind(),
        modified,
        size,
        source,
        body,
        marks,
    }
}

/// What the first sweep's look at an image found.
enum Look {
    /// Read it.
    Read,
    /// Nearly the same as another in its burst: it comes later.
    Later,
    /// Exactly the same as one already read: its words were shared, nothing to read.
    Shared,
}

/// The images' lane's memory of what it has seen: the contents' hashes of images already read, and the look of each burst.
struct Seen {
    copies: HashMap<u64, String>,
    bursts: HashMap<u32, Vec<u64>>,
    reader: Option<content::Reader>,
    index: std::path::PathBuf,
    /// What the looks saved, to say in the log.
    shared: u64,
    set_aside: u64,
}

/// The contents of a file, hashed. (Not a security matter: it tells copies apart.)
fn content_hash(path: &str) -> Option<u64> {
    use std::hash::Hasher;
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(&bytes);
    // (0 means "not known".)
    Some(hasher.finish().max(1))
}

/// Before an image costs a read: is it a copy, or nearly the same as another just read? `hash` gets its contents' hash.
fn look(context: &Context, catalog: &Catalog, seen: &mut Seen, job: &Job, engine: &str, hash: &mut u64) -> Look {
    let file = &job.file;
    if job.place.twin && job.pages.is_empty() {
        *hash = content_hash(&file.path).unwrap_or(0);
        if *hash != 0 && let Some(first) = seen.copies.get(hash).cloned() && !first.eq_ignore_ascii_case(&file.path) && share_copy(context, catalog, seen, job, &first, engine, *hash) {
            return Look::Shared;
        }
    }
    // A burst set aside already has had its look.
    if job.place.burst != 0 && !job.place.later && job.pages.is_empty() {
        let Ok(tiny) = winsys::imaging::decode(&file.path, 32) else { return Look::Read };
        let hash = sweep::dhash(tiny.width, tiny.height, &tiny.pixels);
        let looks = seen.bursts.entry(job.place.burst).or_default();
        if looks.iter().any(|&other| sweep::near(other, hash)) {
            return Look::Later;
        }
        if looks.len() < 16 {
            looks.push(hash);
        }
    }
    Look::Read
}

/// Gives a copy what its original has in the index; false when it isn't there yet (it's read like any other then).
fn share_copy(context: &Context, catalog: &Catalog, seen: &mut Seen, job: &Job, first: &str, engine: &str, hash: u64) -> bool {
    let file = &job.file;
    let Some((size, modified)) = current(context, file) else { return false };
    let Some(entry) = catalog.get(first) else { return false };
    let place = job.place;
    let with_place = |update: Update| Update { group: place.group, later: place.later, hash, ..update };
    if entry.state == State::Empty {
        let _ = context.sender.send(Write::Record(with_place(update(file, size, modified, State::Empty, engine, String::new(), 0))));
        return true;
    }
    if seen.reader.is_none() {
        seen.reader = content::Reader::open(&seen.index).ok();
    } else if let Some(reader) = seen.reader.as_mut() {
        reader.refresh();
    }
    let Some(stored) = seen.reader.as_ref().and_then(|reader| reader.stored(first)) else { return false };
    let characters = stored.text.chars().filter(|c| !c.is_whitespace()).count();
    if entry.state != State::Read || characters == 0 {
        return false;
    }
    let doc = content::Doc {
        path: file.path.clone(),
        name: file.name.clone(),
        kind: file.reader.kind(),
        modified,
        size,
        source: content::Source::Picture,
        body: stored.text,
        marks: stored.marks,
    };
    let record = Update { deep: catalog::DEEP_WANTED, ..with_place(update(file, size, modified, State::Read, engine, String::new(), characters)) };
    let _ = context.sender.send(Write::Put(Box::new(doc), record));
    true
}

/// The pictures lane: gets the models, loads OCR, then reads pictures and scanned pages.
pub fn read_pictures(context: Context, models: std::path::PathBuf, size: Size, catalog: Arc<Catalog>, index: std::path::PathBuf) {
    winsys::com_mta();
    let mut seen = Seen { copies: catalog.hashes(), bursts: HashMap::new(), reader: None, index, shared: 0, set_aside: 0 };
    // The reader is loaded while there are pictures to read, and let go after a minute without (it holds hundreds
    // of megabytes while loaded; loading again takes a fifth of a second).
    let mut ocr: Option<Ocr> = None;
    // Text in videos has its own detector and recognizer: loaded while there are videos to read, after the images.
    let mut video_reader: Option<crate::video_text::VideoText> = None;
    let mut recycle = false;
    // The memory once the reader loaded last had read its first picture.
    let mut settled: Option<u64> = None;
    loop {
        if std::mem::take(&mut recycle) {
            ocr = None;
        }
        if ocr.is_none() {
            settled = None;
        }
        // Waiting for the schedule: nothing loaded meanwhile. (Text in images off: nothing to wait for.)
        if context.pictures && !context.gate.open() {
            ocr = None;
            if !context.control.sleep(Duration::from_secs(20)) {
                break;
            }
            continue;
        }
        // Waiting for a quiet moment, or paused: nothing loaded meanwhile either.
        if context.queue.lengths().1 > 0 {
            context.shared.wants_load.store(true, Ordering::Relaxed);
            if pause_reason(&context, Need::Cpu).is_some() {
                if ocr.take().is_some() {
                    config::log("waiting: OCR unloaded");
                }
                let _busy = Busy::new(&context.shared);
                if !wait_until_free(&context, Need::Cpu) {
                    break;
                }
            }
        }
        // The images read: their videos' text next, one video at a time (the images' reader let go meanwhile).
        if context.video_text && context.queue.lengths().1 == 0 {
            if let Some(video) = catalog.video_next(crate::video_text::ENGINE, 0, 1, config::video_kind::TEXT).pop() {
                ocr = None;
                if !crate::video_text::read_one(&context, &catalog, &models, size, &mut video_reader, video) {
                    break;
                }
                continue;
            }
            if video_reader.take().is_some() {
                config::log("text in videos: every video read, its reader unloaded");
            }
        }
        let job = match context.queue.pop_within(Lane::Pictures, Duration::from_secs(60)) {
            Err(()) => break,
            Ok(None) => {
                if ocr.take().is_some() {
                    config::log(&format!("no pictures for a minute: OCR unloaded ({} copies shared, {} nearly the same set aside)", seen.shared, seen.set_aside));
                }
                continue;
            }
            Ok(Some(job)) => job,
        };
        // The gate may have closed while this was waiting for work: it goes back in line.
        if context.pictures && !context.gate.open() {
            let priority = priority(&job);
            context.queue.push(job, priority);
            continue;
        }
        // Its text left out (by its folder, or text in images off): recorded without it, so it can still be looked at.
        if job.pages.is_empty() && (!context.pictures || !context.kinds.on(&job.file.path, "image-text")) {
            if let Some((size, modified)) = current(&context, &job.file) {
                let _ = context.sender.send(Write::Record(update(&job.file, size, modified, State::Empty, catalog::SKIPPED, String::new(), 0)));
            }
            continue;
        }
        let engine = size.engine();
        let mut hash = 0;
        match look(&context, &catalog, &mut seen, &job, engine, &mut hash) {
            Look::Shared => {
                seen.shared += 1;
                continue;
            }
            Look::Later => {
                seen.set_aside += 1;
                let mut job = job;
                job.place.later = true;
                let priority = priority(&job);
                context.queue.push(job, priority);
                continue;
            }
            Look::Read => {}
        }
        if ocr.is_none() {
            ocr = load(&context, &models, size);
            context.shared.status().engine = size.label().to_string();
            context.shared.status().reading_gpu = context.reading_gpu;
        }
        let Some(ocr) = ocr.as_ref() else { break };
        let _busy = Busy::new(&context.shared);
        if !wait_until_free(&context, Need::Cpu) {
            break;
        }
        let Some((file_size, modified)) = current(&context, &job.file) else { continue };
        context.shared.set_current(&job.file.path);
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if job.pages.is_empty() { picture(ocr, &job.file) } else { scanned(ocr, &job) }
        }))
        .unwrap_or_else(|_| Err("OCR stopped on this file".into()));
        let file = &job.file;
        let place = job.place;
        // The first of its contents: copies that come later share what's read from it.
        if hash != 0 && result.is_ok() {
            seen.copies.entry(hash).or_insert_with(|| file.path.clone());
        }
        let with_place = |update: Update| Update { group: place.group, later: place.later, hash, ..update };
        // A safety net: should the reader's memory still grow (a runtime that keeps what it allocated), it's let go
        // and loaded fresh for the next picture.
        let memory = winsys::process_memory().1;
        match settled {
            None => settled = Some(memory),
            Some(at) if memory > at + RECYCLE_GROWTH => {
                config::log(&format!("OCR grew from {} to {} MB: loading it fresh", at >> 20, memory >> 20));
                recycle = true;
            }
            Some(_) => {}
        }
        match result {
            Ok(Pass { text, lines: mark, codes, pictures: pictures_read, doubtful }) => {
                if pictures_read > 0 {
                    context.shared.picture_took(started.elapsed().as_secs_f64() * 1000.0 / pictures_read as f64);
                }
                let (body, secrets) = private(&text);
                let characters = body.chars().filter(|c| !c.is_whitespace()).count();
                // (Only for pictures: scanned pages stay with the first pass.)
                let deep = if !job.pages.is_empty() { catalog::DEEP_NO } else if doubtful { catalog::DEEP_FIRST } else if characters > 0 { catalog::DEEP_WANTED } else { catalog::DEEP_NO };
                if deep == catalog::DEEP_FIRST {
                    config::log(&format!("{}: unsure of what it read, so Deep goes first", file.path));
                }
                if characters == 0 {
                    let record = Update { deep, ..with_place(update(file, file_size, modified, State::Empty, engine, String::new(), 0)) };
                    let _ = context.sender.send(Write::Record(record));
                    continue;
                }
                let source = if job.pages.is_empty() || job.text_layer.trim().is_empty() { content::Source::Picture } else { content::Source::Mixed };
                // A QR code in an image about Wi-Fi: the network's password, covered (the code alone).
                let wifi_codes = !codes.is_empty() && privacy::about_wifi(&text);
                let mut secrets = secrets;
                if wifi_codes && !secrets.iter().any(|secret| secret.kind == privacy::Kind::Password) {
                    secrets.push(privacy::Secret { kind: privacy::Kind::Password, range: 0..0 });
                }
                let mut mark = if secrets.is_empty() { None } else { mark.map(|mark| boxes_with_secrets(mark, &text, &secrets)) };
                if let (true, Some(mark)) = (wifi_codes, mark.as_mut()) {
                    if let Some(boxes) = mark["boxes"].as_array_mut() {
                        boxes.extend(codes.iter().map(|code| serde_json::json!(code.map(|value| value.round() as i32))));
                    }
                }
                let doc = doc(file, file_size, modified, body, source, &secrets, mark, place.similar);
                let record = Update { deep, ..with_place(update(file, file_size, modified, State::Read, engine, String::new(), characters)) };
                let _ = context.sender.send(Write::Put(Box::new(doc), record));
            }
            Err(error) => {
                let _ = context.sender.send(Write::Record(with_place(update(file, file_size, modified, State::Failed, engine, error, 0))));
            }
        }
    }
    config::log(&format!("images: {} copies shared, {} nearly the same set aside", seen.shared, seen.set_aside));
}

fn load(context: &Context, models: &Path, size: Size) -> Option<Ocr> {
    loop {
        if context.control.stopping() {
            return None;
        }
        if !crate::models::present(models, size) {
            context.shared.status().model = config::status::Model { state: "downloading".into(), name: "the image reader".into(), total: size.download_size(), ..Default::default() };
            let shared = context.shared.clone();
            let result = crate::models::ensure(models, size, context.control, &mut |received, total| {
                let mut status = shared.status();
                status.model.received = received;
                status.model.total = total;
            });
            if let Err(error) = result {
                config::log(&format!("models: {error}"));
                context.shared.status().model = config::status::Model { state: "failed".into(), error, ..Default::default() };
                // Offline, most likely: try again in a while.
                if !context.control.sleep(Duration::from_secs(600)) {
                    return None;
                }
                continue;
            }
        }
        let started = Instant::now();
        match Ocr::load(models, size, ocr::threads(), context.reading_gpu) {
            Ok(ocr) => {
                config::log(&format!("{} loaded in {} ms ({} threads)", size.label(), started.elapsed().as_millis(), ocr::threads()));
                context.shared.status().model = config::status::Model { state: "ready".into(), ..Default::default() };
                return Some(ocr);
            }
            Err(error) => {
                config::log(&format!("OCR didn't load: {error}"));
                context.shared.status().model = config::status::Model { state: "failed".into(), error, ..Default::default() };
                if !context.control.sleep(Duration::from_secs(600)) {
                    return None;
                }
            }
        }
    }
}

/// A picture's lines, each with its byte range in the joined text and its box (in the picture's own pixels).
struct PictureLines {
    /// The picture's size as stored.
    width: u32,
    height: u32,
    /// How its metadata says to turn it (EXIF's 1 to 8): the lines are where they are the right way up, as read, so a
    /// secret's part of a line is told along the line (`boxes_with_secrets`), then taken back to the picture as stored.
    orientation: u16,
    lines: Vec<(std::ops::Range<usize>, [f32; 4])>,
}

/// What the first pass made of a job.
struct Pass {
    text: String,
    lines: Option<PictureLines>,
    /// Where QR codes are (in the original's pixels): a Wi-Fi network's password, when the text is about one.
    codes: Vec<[f32; 4]>,
    pictures: usize,
    doubtful: bool,
}

fn picture(ocr: &Ocr, file: &FileInfo) -> Result<Pass, String> {
    // Read the right way up (a phone's photo is often stored sideways); where each line is,
    // kept as the picture is stored (the preview turns it, `blur::turned`).
    let (decoded, orientation) = winsys::imaging::decode_upright(&file.path, ocr::MAX_SIDE)?;
    let stored_width = if orientation >= 5 { decoded.height } else { decoded.width };
    let scale = decoded.original.0 as f32 / stored_width as f32;
    let read = ocr.read(decoded.width, decoded.height, decoded.pixels)?;
    let mut text = String::new();
    let mut placed = Vec::with_capacity(read.lines.len());
    for line in read.lines {
        let start = text.len();
        text.push_str(&line.text);
        placed.push((start..text.len(), line.bounds.map(|value| value * scale)));
        text.push('\n');
    }
    // QR codes only in an image about Wi-Fi (rare): it's decoded again, smaller, and looked through. Looking through
    // every image cost 11 ms at 1600 × 1200; this costs the others nothing.
    let codes = if privacy::about_wifi(&text) { wifi_codes(&file.path) } else { Vec::new() };
    let lines = PictureLines { width: decoded.original.0, height: decoded.original.1, orientation, lines: placed };
    Ok(Pass { text, lines: Some(lines), codes, pictures: 1, doubtful: read.doubtful })
}

/// Where QR codes are in an image, in its original's pixels (`codes::find` on a copy at most 1000 pixels wide).
fn wifi_codes(path: &str) -> Vec<[f32; 4]> {
    let Ok(small) = winsys::imaging::decode(path, 1000) else { return Vec::new() };
    let scale = small.original.0 as f32 / small.width.max(1) as f32;
    crate::codes::find(small.width, small.height, &small.pixels).into_iter().map(|code| code.map(|value| value * scale)).collect()
}

fn scanned(ocr: &Ocr, job: &Job) -> Result<Pass, String> {
    let mut text = job.text_layer.clone();
    let mut pictures = 0;
    extract::pdf::render_each(Path::new(&job.file.path), &job.pages, 1800, &mut |width, height, pixels| {
        for line in ocr.read(width, height, pixels)?.lines {
            text.push_str(&line.text);
            text.push('\n');
        }
        text.push('\n');
        pictures += 1;
        Ok(())
    })?;
    Ok(Pass { text, lines: None, codes: Vec::new(), pictures, doubtful: false })
}

/// Deep's lane: pictures the first pass has read, again, on the graphics card; its text replaces the first pass's. The
/// server runs only while there are pictures for it, and stops the moment a game or a full-screen app starts.
pub fn read_deep(context: Context, catalog: Arc<Catalog>, models: std::path::PathBuf, index: std::path::PathBuf) {
    // On the graphics card, or the processor when chosen so: it waits for that one to be quiet.
    let deep_need = if context.deep_gpu { Need::Gpu } else { Need::Cpu };
    winsys::com_mta();
    let mut server: Option<deep::Server> = None;
    let mut reader: Option<content::Reader> = None;
    // Read this session; the catalog hears of them with the next commit.
    let mut read: HashSet<String> = HashSet::new();
    let mut idle_since = Instant::now();
    let mut times: VecDeque<f64> = VecDeque::new();
    // The whole round for each picture, whatever it spent the time on, to tell how long the rest takes. (A long wait isn't
    // a picture's doing: a pause, or a busy graphics card.)
    let mut rounds: VecDeque<f64> = VecDeque::new();
    let mut last_round: Option<Instant> = None;
    loop {
        if context.control.stopping() {
            break;
        }
        if !context.gate.open() {
            server = None;
            context.shared.server.store(0, Ordering::Relaxed);
            context.shared.deep_on.store(false, std::sync::atomic::Ordering::SeqCst);
            if !context.control.sleep(Duration::from_secs(20)) {
                break;
            }
            continue;
        }
        // What it needs, once. Before anything's taken from the catalog: a download takes a while.
        if catalog.deep_count() > 0 && !crate::models::deep_present(&models) {
            context.shared.status().model = config::status::Model { state: "downloading".into(), name: "the thorough reader".into(), total: crate::models::deep_download_size(), ..Default::default() };
            let shared = context.shared.clone();
            let result = crate::models::deep_ensure(&models, context.control, &mut |received, total| {
                let mut status = shared.status();
                status.model.received = received;
                status.model.total = total;
            });
            if let Err(error) = result {
                config::log(&format!("Deep's models: {error}"));
                context.shared.status().model = config::status::Model { state: "failed".into(), name: "the thorough reader".into(), error, ..Default::default() };
                if !context.control.sleep(Duration::from_secs(600)) {
                    break;
                }
                continue;
            }
            context.shared.status().model = config::status::Model { state: "ready".into(), ..Default::default() };
        }
        let batch: Vec<_> = catalog.deep_next(64 + read.len().min(64)).into_iter().filter(|(path, ..)| !read.contains(&catalog::id(path))).take(64).collect();
        if batch.is_empty() {
            if server.is_some() && idle_since.elapsed() > Duration::from_secs(60) {
                server = None;
                context.shared.server.store(0, Ordering::Relaxed);
            }
            context.shared.deep_on.store(server.is_some(), std::sync::atomic::Ordering::SeqCst);
            if !context.control.sleep(Duration::from_secs(5)) {
                break;
            }
            continue;
        }
        context.shared.wants_load.store(true, Ordering::Relaxed);
        for (path, size, modified) in batch {
            if context.control.stopping() || !context.gate.open() {
                break;
            }
            // No longer in what's being read (the catalog hasn't heard yet): writing it back would bring it back.
            if !crate::scan::in_scope(&path, &context.roots, &context.exclusions) {
                read.insert(catalog::id(&path));
                let _ = context.sender.send(Write::Remove(path));
                continue;
            }
            // Something else needs the graphics card, or it's paused: it's theirs, at once.
            if pause_reason(&context, deep_need).is_some() {
                server = None;
                context.shared.server.store(0, Ordering::Relaxed);
                context.shared.deep_on.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            let _busy = Busy::new(&context.shared);
            if !wait_until_free(&context, deep_need) {
                break;
            }
            if server.as_mut().is_none_or(|server| !server.running()) {
                context.shared.deep_on.store(true, std::sync::atomic::Ordering::SeqCst);
                match deep::Server::start(&models, context.deep_gpu, &|| context.control.stopping()) {
                    Ok(started) => {
                        context.shared.server.store(started.process(), Ordering::Relaxed);
                        server = Some(started);
                    }
                    Err(error) => {
                        config::log(&format!("Deep: {error}"));
                        context.shared.status().model = config::status::Model { state: "failed".into(), name: "the thorough reader".into(), error, ..Default::default() };
                        if !context.control.sleep(Duration::from_secs(600)) {
                            return;
                        }
                        break;
                    }
                }
                context.shared.status().model = config::status::Model { state: "ready".into(), ..Default::default() };
            }
            let Some(server) = server.as_ref() else { break };
            let id = catalog::id(&path);
            let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
            let file = FileInfo { name, path: path.clone(), size, modified, reader: Reader::Picture };
            let Some((file_size, file_modified)) = current(&context, &file) else {
                read.insert(id);
                continue;
            };
            // Changed since the first pass: that pass comes first again.
            if file_size != size || file_modified != modified {
                read.insert(id);
                continue;
            }
            context.shared.status().deep_current = path.clone();
            if let Some(reader) = reader.as_mut() {
                reader.refresh();
            } else {
                reader = content::Reader::open(&index).ok();
            }
            let started = Instant::now();
            let result = winsys::imaging::decode_upright(&path, deep::MAX_SIDE).and_then(|(picture, _)| {
                server.read(picture.width, picture.height, &picture.pixels).map(|text| (text, picture.original))
            });
            let first = reader.as_ref().and_then(|reader| reader.stored(&path));
            read.insert(id);
            if let Some(round) = last_round.replace(Instant::now()).map(|last| last.elapsed().as_secs_f64()).filter(|&round| round < 30.0) {
                rounds.push_back(round * 1000.0);
                if rounds.len() > 64 {
                    rounds.pop_front();
                }
                let mean = rounds.iter().sum::<f64>() / rounds.len() as f64;
                context.shared.status().deep_pace_ms = Some(mean.round());
            }
            match result {
                Ok((text, original)) => {
                    times.push_back(started.elapsed().as_secs_f64() * 1000.0);
                    if times.len() > 64 {
                        times.pop_front();
                    }
                    let mut sorted: Vec<f64> = times.iter().copied().collect();
                    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    context.shared.status().deep_ms = Some((sorted[sorted.len() / 2] * 10.0).round() / 10.0);
                    deep_result(&context, &catalog, &file, &text, original, first.as_ref());
                }
                Err(error) => {
                    config::log(&format!("Deep on {path}: {error}"));
                    catalog.deep_set(&path, catalog::DEEP_FAILED);
                }
            }
            idle_since = Instant::now();
        }
    }
}

/// Deep's text, then the first pass's lines it mostly missed (at least two thirds of their words): a vision model now and
/// then skips a line, often the one with the invoice number. Measured on the benchmark: word recall 98.5% → 99.7%, exact
/// strings 75 → 76 of 76, precision 99.9% → 99.6%. The first pass's text is already redacted.
fn with_missed_lines(deep: String, first: &str) -> String {
    fn words(text: &str) -> impl Iterator<Item = String> + '_ {
        text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase)
    }
    let have: HashSet<String> = words(&deep).collect();
    let mut out = deep;
    for line in first.lines() {
        let (mut all, mut missed) = (0, 0);
        for word in words(line) {
            all += 1;
            if !have.contains(&word) {
                missed += 1;
            }
        }
        if all > 0 && missed * 3 >= all * 2 {
            out.push('\n');
            out.push_str(line);
        }
    }
    out
}

/// Puts what Deep read in place of the first pass's text, keeping that pass's blurred boxes. A secret only Deep saw has
/// no box (its text has no positions): the whole picture is blurred in the preview then.
fn deep_result(context: &Context, catalog: &Catalog, file: &FileInfo, text: &str, original: (u32, u32), first: Option<&content::Stored>) {
    let (body, secrets) = private(text);
    // It came back with nothing: the first pass stands.
    if body.trim().is_empty() {
        catalog.deep_set(&file.path, catalog::DEEP_NO);
        return;
    }
    let body = with_missed_lines(body, first.map_or("", |first| first.text.as_str()));
    let characters = body.chars().filter(|c| !c.is_whitespace()).count();
    let marks: serde_json::Value = first.and_then(|first| serde_json::from_slice(&first.marks).ok()).unwrap_or(serde_json::Value::Null);
    let known: Vec<&str> = marks["kinds"].as_array().map(|kinds| kinds.iter().filter_map(|kind| kind.as_str()).collect()).unwrap_or_default();
    let unboxed = secrets.iter().any(|secret| !known.contains(&secret.kind.name()));
    let mut kinds: Vec<String> = known.iter().map(|kind| kind.to_string()).collect();
    kinds.extend(secrets.iter().map(|secret| secret.kind.name().to_string()));
    kinds.sort_unstable();
    kinds.dedup();
    let picture = if unboxed {
        Some(serde_json::json!({ "w": original.0, "h": original.1, "boxes": [[0, 0, original.0, original.1]] }))
    } else {
        (!marks["picture"].is_null()).then(|| marks["picture"].clone())
    };
    let similar = marks["similar"].as_u64().unwrap_or(0);
    let marks = if kinds.is_empty() && picture.is_none() && similar == 0 {
        Vec::new()
    } else {
        let mut marks = serde_json::json!({ "kinds": kinds });
        if let Some(picture) = picture {
            marks["picture"] = picture;
        }
        if similar > 0 {
            marks["similar"] = similar.into();
        }
        serde_json::to_vec(&marks).unwrap_or_default()
    };
    let doc = content::Doc {
        path: file.path.clone(),
        name: file.name.clone(),
        kind: file.reader.kind(),
        modified: file.modified,
        size: file.size,
        source: content::Source::Picture,
        body,
        marks,
    };
    let record = update(file, file.size, file.modified, State::Read, deep::ENGINE, String::new(), characters);
    let _ = context.sender.send(Write::Put(Box::new(doc), record));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_scheduled_time() {
        // 2026-10-30 05:00 UTC.
        let done = 1_793_336_400;
        assert_eq!(done % 86_400, 5 * 3600);
        // The next day at 03:00, a week on, and in a time zone two hours ahead (07:00 there, so the same local dates).
        assert_eq!(next_run(done, 1, 180, 0), done - 2 * 3600 + 86_400);
        assert_eq!(next_run(done, 7, 180, 0), done - 2 * 3600 + 7 * 86_400);
        assert_eq!(next_run(done, 1, 180, 2 * 3600), done - 4 * 3600 + 86_400);
        // Finished late at night in a zone behind UTC: still the next local day.
        assert_eq!(next_run(done, 1, 180, -6 * 3600), done - 2 * 3600 + 6 * 3600);
    }

    #[test]
    fn only_the_secret_of_a_line_is_blurred() {
        let text = "Network: Home\nWi-Fi password: hunter2";
        let line = |start: usize, end: usize, top: f32| (start..end, [100.0, top, 100.0 + (end - start) as f32 * 10.0, top + 20.0]);
        let lines = |orientation| PictureLines { width: 800, height: 600, orientation, lines: vec![line(0, 13, 0.0), line(14, text.len(), 30.0)] };
        let secrets = privacy::find(text);
        assert_eq!(secrets.len(), 1);
        let marks = boxes_with_secrets(lines(1), text, &secrets);
        // "hunter2" is characters 16 to 23 of its line (10 px each from x 100): 1.5 more on each side.
        assert_eq!(marks["boxes"], serde_json::json!([[245, 30, 330, 50]]));
        // A picture stored upside down (3) or sideways (6), read the right way up: the same part of the line, kept
        // where it is as stored, so the preview's turn puts it back over "hunter2".
        for orientation in [3u16, 6, 8] {
            let marks = boxes_with_secrets(lines(orientation), text, &secrets);
            let stored: Vec<[i32; 4]> = serde_json::from_value(marks["boxes"].clone()).unwrap();
            let (shown, _) = winsys::imaging::turned(&stored, (800, 600), orientation);
            assert_eq!(shown, vec![[245, 30, 330, 50]], "orientation {orientation}");
        }
    }

    #[test]
    fn keeps_the_lines_deep_missed() {
        let first = "Northwind Traders\nINV-2026-86754\nreturn 0k(response)\n\nTotal: $12.40";
        let deep = "Northwind Traders\nreturn Ok(response)\nTotal $12.40".to_string();
        // The invoice number stays; a line with one misread word doesn't come back.
        assert_eq!(with_missed_lines(deep.clone(), first), format!("{deep}\nINV-2026-86754"));
        assert_eq!(with_missed_lines(deep.clone(), ""), deep);
    }
}

/// The parts of the lines that hold a secret, for the preview to blur: {"w", "h", "boxes": [[l, t, r, b], …]}. Only
/// the secret, not its whole line: OCR boxes a line, so
/// where the secret sits along it is told by its characters (a line's are about as wide as each other), with
/// `SECRET_MARGIN` characters more on each side for what that misjudges.
fn boxes_with_secrets(lines: PictureLines, text: &str, secrets: &[privacy::Secret]) -> serde_json::Value {
    let mut boxes: Vec<[i32; 4]> = Vec::new();
    for (range, bounds) in &lines.lines {
        let Some(line) = text.get(range.clone()) else { continue };
        let count = line.chars().count().max(1) as f32;
        let width = bounds[2] - bounds[0];
        for secret in secrets.iter().filter(|secret| secret.range.start < range.end && secret.range.end > range.start) {
            let from = secret.range.start.max(range.start) - range.start;
            let to = secret.range.end.min(range.end) - range.start;
            let (Some(before), Some(inside)) = (line.get(..from), line.get(from..to)) else {
                boxes.push(bounds.map(|value| value.round() as i32));
                continue;
            };
            let start = (before.chars().count() as f32 - SECRET_MARGIN).max(0.0);
            let end = (before.chars().count() as f32 + inside.chars().count() as f32 + SECRET_MARGIN).min(count);
            boxes.push([
                (bounds[0] + width * start / count).floor() as i32,
                bounds[1].round() as i32,
                (bounds[0] + width * end / count).ceil() as i32,
                bounds[3].round() as i32,
            ]);
        }
    }
    // Back to where they are in the picture as stored (what the preview turns again for showing).
    if lines.orientation > 1 {
        let (w, h) = (lines.width as f32, lines.height as f32);
        let upright = if lines.orientation >= 5 { (h, w) } else { (w, h) };
        for b in boxes.iter_mut() {
            let stored = winsys::imaging::unturned(b.map(|value| value as f32), upright, lines.orientation);
            *b = stored.map(|value| value.round() as i32);
        }
    }
    serde_json::json!({ "w": lines.width, "h": lines.height, "boxes": boxes })
}

/// Characters more blurred on each side of a secret within its line.
const SECRET_MARGIN: f32 = 1.5;
