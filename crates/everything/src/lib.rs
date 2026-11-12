//! Name and path search over every local drive through voidtools' Everything, by its SDKs compiled in (build.rs):
//! SDK3 over Everything 1.5's named pipe, preferred when 1.5 runs, and the 1.4 SDK over window messages otherwise.
//! A search is one round trip to the Everything that's already running: no DLL, no process per query.
//!
//! The 1.4 SDK keeps its query in globals, for the whole process, under a lock held for each round trip: so there's one
//! `Everything` per process (`shared`), and every SDK call happens on its thread. Searches arrive in numbered batches (one
//! per keystroke, several queries each); a batch overtaken by a newer one is dropped before or between its queries, so
//! typing never queues work. Work in the background (`ask`, `list`) takes its turn between them, a page at a time.
//!
//! Everything's answers are only as new as its list of files: `health` says whether it's keeping up.

use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Result order, as both SDKs can ask for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sort {
    NameAscending,
    NameDescending,
    PathAscending,
    SizeAscending,
    SizeDescending,
    ExtensionAscending,
    ModifiedAscending,
    ModifiedDescending,
    RunCountDescending,
    DateRunDescending,
}

impl Sort {
    /// EVERYTHING_SORT_* of the 1.4 SDK.
    fn sdk1(self) -> u32 {
        match self {
            Sort::NameAscending => 1,
            Sort::NameDescending => 2,
            Sort::PathAscending => 3,
            Sort::SizeAscending => 5,
            Sort::SizeDescending => 6,
            Sort::ExtensionAscending => 7,
            Sort::ModifiedAscending => 13,
            Sort::ModifiedDescending => 14,
            Sort::RunCountDescending => 20,
            Sort::DateRunDescending => 26,
        }
    }

    /// SDK3's property id and direction.
    fn sdk3(self) -> (u32, bool) {
        match self {
            Sort::NameAscending => (PROPERTY_NAME, true),
            Sort::NameDescending => (PROPERTY_NAME, false),
            Sort::PathAscending => (PROPERTY_PATH, true),
            Sort::SizeAscending => (PROPERTY_SIZE, true),
            Sort::SizeDescending => (PROPERTY_SIZE, false),
            Sort::ExtensionAscending => (PROPERTY_EXTENSION, true),
            Sort::ModifiedAscending => (PROPERTY_DATE_MODIFIED, true),
            Sort::ModifiedDescending => (PROPERTY_DATE_MODIFIED, false),
            Sort::RunCountDescending => (PROPERTY_RUN_COUNT, false),
            Sort::DateRunDescending => (PROPERTY_DATE_RUN, false),
        }
    }
}

/// What each result carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fields {
    /// All a search shows, run count and last run included. Never attributes: Everything doesn't keep them unless it's
    /// set to, so asking for them reads each file's from the disk (20,000 files took 1.5 to 2.7 s with them and 55 to
    /// 90 ms without; a keystroke's 450 results 70 to 390 ms the first time instead of 50 to 60).
    Search,
    /// What a listing needs: name, folder, size and date modified.
    Listing,
}

/// One query: Everything's search syntax in `text`, and the window of results wanted.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub text: String,
    pub match_case: bool,
    pub match_path: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub sort: Sort,
    pub offset: u32,
    pub count: u32,
    pub fields: Fields,
    /// Names that fit the words loosely, best first (a letter or two off, letters skipped): Excavate's own index scores
    /// them (`engine::fuzzy`); Everything, which has no such search, is asked for the words' letters in order.
    pub fuzzy: bool,
}

impl Query {
    pub fn new(text: impl Into<String>, sort: Sort, count: u32) -> Query {
        Query { text: text.into(), match_case: false, match_path: false, whole_word: false, regex: false, sort,
                offset: 0, count, fields: Fields::Search, fuzzy: false }
    }

    /// For `list`, or a count: each result without what only a search shows.
    pub fn listing(text: impl Into<String>, sort: Sort) -> Query {
        Query { fields: Fields::Listing, ..Query::new(text, sort, PAGE) }
    }
}

/// How much of a list comes back at once: a search waits behind one page at most (60 to 90 ms on 9.8 million files).
const PAGE: u32 = 20_000;

/// One result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub name: String,
    /// The folder it's in ("C:\Users\me"); empty for a drive.
    pub folder: String,
    /// Byte ranges of `name` that matched (Everything's highlighting).
    pub highlights: Vec<Range<usize>>,
    pub is_folder: bool,
    pub size: Option<u64>,
    /// Seconds since 1970.
    pub modified: Option<i64>,
    /// How often it was opened through Everything (or a client that reports it, as Excavate does).
    pub run_count: u32,
    pub date_run: Option<i64>,
}

impl Item {
    pub fn path(&self) -> String {
        if self.folder.is_empty() {
            // A drive: "C:" → "C:\"
            return if self.name.ends_with(':') { format!("{}\\", self.name) } else { self.name.clone() };
        }
        if self.folder.ends_with('\\') { format!("{}{}", self.folder, self.name) } else { format!("{}\\{}", self.folder, self.name) }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    /// All results of the query; `items` are those from `offset` on.
    pub total: u64,
    pub offset: u32,
    pub items: Vec<Item>,
    /// The round trip to Everything.
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Neither Everything 1.5 nor 1.4 answered.
    NotRunning,
    /// A newer batch arrived before this one finished.
    Superseded,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotRunning => write!(f, "Everything isn't running"),
            Error::Superseded => write!(f, "superseded by a newer search"),
            Error::Failed(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

/// Which Everything answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub available: bool,
    /// "1.4.1.1026"
    pub version: String,
    /// Everything 1.5 over its pipe (SDK3), or 1.4 over window messages.
    pub pipe: bool,
    pub db_loaded: bool,
    /// Running as administrator, which lets it watch NTFS drives without its service. Only 1.4 says.
    pub admin: Option<bool>,
    /// Where Everything.exe is installed (to start it when it isn't running).
    pub installed: Option<String>,
}

/// Whether Everything's answers include what's new.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Health {
    /// Not asked yet.
    #[default]
    Unknown,
    NotRunning,
    /// Loading its list of files: answers are empty or partial until it's done.
    Loading,
    /// It sees files change as they do.
    Live,
    /// It answers from a list it can't keep up to date, so what's new since is missing; and what would fix it.
    Behind(Fix),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Its service is installed but stopped. Without it, or administrator rights, Everything can't watch NTFS drives.
    StartService,
    /// It has no service, and doesn't run as administrator.
    InstallService,
    /// It could watch, and didn't (its updates paused, the drive left out…): a restart is worth a try.
    Restart,
}

type Reply = Box<dyn FnOnce(Result<Vec<Page>, Error>) + Send>;

/// An index that answers in Everything's place: Excavate's own, when it's switched on. Searches, status and health
/// then come from it, so everything that asks Everything works unchanged.
pub trait Local: Send + Sync {
    fn search(&self, query: &Query) -> Result<Page, Error>;
    fn status(&self) -> Status;
    fn health(&self) -> Health;
}

static LOCAL: std::sync::RwLock<Option<Arc<dyn Local>>> = std::sync::RwLock::new(None);

/// Answers from `local` instead of Everything (none: Everything again).
pub fn set_local(local: Option<Arc<dyn Local>>) {
    *LOCAL.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = local;
}

fn local() -> Option<Arc<dyn Local>> {
    LOCAL.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// Whether a local index answers instead of Everything.
pub fn is_local() -> bool {
    local().is_some()
}

static DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Leaves Everything alone in this process (when Excavate's own index is on, the indexer lists folders itself).
pub fn set_disabled(disabled: bool) {
    DISABLED.store(disabled, Ordering::Relaxed);
}

fn disabled() -> bool {
    DISABLED.load(Ordering::Relaxed)
}

enum Job {
    /// Numbered batches can be overtaken; one without a number (`ask`) never is.
    Search { generation: Option<u64>, queries: Vec<Query>, reply: Reply },
    Status(Sender<Status>),
    Ran(String),
}

/// The thread that owns the SDKs.
pub struct Everything {
    jobs: Sender<Job>,
    latest: Arc<AtomicU64>,
    /// When `health` last asked, and what it found.
    health: Mutex<Option<(Instant, Health)>>,
}

impl Everything {
    /// This process's. Two would wait on each other's round trips, and the 1.4 SDK could run one's query for the other.
    pub fn shared() -> Arc<Everything> {
        static SHARED: OnceLock<Arc<Everything>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Everything::start())).clone()
    }

    fn start() -> Everything {
        let (jobs, receiver) = channel();
        let latest = Arc::new(AtomicU64::new(0));
        let seen = latest.clone();
        std::thread::Builder::new()
            .name("everything".into())
            .spawn(move || Worker::default().run(receiver, seen))
            .expect("can't start Everything's thread");
        Everything { jobs, latest, health: Mutex::new(None) }
    }

    /// Runs `queries` in order and replies on Everything's thread. A batch whose `generation` is older than the newest
    /// one sent replies `Superseded` instead (generations only need to grow; one per keystroke is the idea).
    pub fn search(&self, generation: u64, queries: Vec<Query>,
                  reply: impl FnOnce(Result<Vec<Page>, Error>) + Send + 'static) {
        self.latest.fetch_max(generation, Ordering::SeqCst);
        let job = Job::Search { generation: Some(generation), queries, reply: Box::new(reply) };
        if let Err(error) = self.jobs.send(job) {
            if let Job::Search { reply, .. } = error.0 {
                reply(Err(Error::Failed("Everything's thread ended".into())));
            }
        }
    }

    pub fn search_blocking(&self, generation: u64, queries: Vec<Query>) -> Result<Vec<Page>, Error> {
        let (sender, receiver) = channel();
        self.search(generation, queries, move |result| {
            let _ = sender.send(result);
        });
        receiver.recv().unwrap_or_else(|_| Err(Error::Failed("Everything's thread ended".into())))
    }

    /// Runs `queries` outside the numbered batches: never overtaken, and never overtaking one. Blocking.
    pub fn ask(&self, queries: Vec<Query>) -> Result<Vec<Page>, Error> {
        let (sender, receiver) = channel();
        let reply: Reply = Box::new(move |result| {
            let _ = sender.send(result);
        });
        if let Err(error) = self.jobs.send(Job::Search { generation: None, queries, reply }) {
            if let Job::Search { reply, .. } = error.0 {
                reply(Err(Error::Failed("Everything's thread ended".into())));
            }
        }
        receiver.recv().unwrap_or_else(|_| Err(Error::Failed("Everything's thread ended".into())))
    }

    /// Every result of `query` (its offset and count aside), asked a page at a time: a search waits behind a page at most.
    pub fn list(&self, mut query: Query) -> Result<Page, Error> {
        let started = Instant::now();
        let mut whole = Page::default();
        query.offset = 0;
        query.count = PAGE;
        loop {
            let page = self.ask(vec![query.clone()])?.into_iter().next().unwrap_or_default();
            let got = page.items.len() as u32;
            whole.total = page.total;
            whole.items.extend(page.items);
            query.offset += got;
            if got < PAGE || u64::from(query.offset) >= whole.total {
                break;
            }
        }
        whole.elapsed = started.elapsed();
        Ok(whole)
    }

    /// Whether Everything's answers include what's new, asked again when the last answer is older than `fresh`.
    /// `probe` is a file of the caller's own, rewritten each time to see whether Everything notices: it's told of changes
    /// within about a second when it's live, so this takes a second or two then, and up to a minute when it's unclear.
    /// Blocking: not on a UI thread.
    pub fn health(&self, probe: &Path, fresh: Duration) -> Health {
        if let Some(local) = local() {
            return local.health();
        }
        let mut last = self.health.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, health)) = *last {
            if at.elapsed() < fresh && !matches!(health, Health::Unknown | Health::Loading) {
                return health;
            }
        }
        let mut health = self.check(probe);
        // It could watch, and didn't notice in time: a PC under load can hold its updates back, so once more.
        if health == Health::Behind(Fix::Restart) && !matches!(*last, Some((_, Health::Behind(_)))) {
            health = self.check(probe);
        }
        *last = Some((Instant::now(), health));
        health
    }

    /// `health`, waiting up to `patience` while Everything loads its list.
    pub fn settled_health(&self, probe: &Path, fresh: Duration, patience: Duration) -> Health {
        let started = Instant::now();
        loop {
            let health = self.health(probe, fresh);
            if health != Health::Loading || started.elapsed() >= patience {
                return health;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// What `health` last found, without asking.
    pub fn last_health(&self) -> Health {
        if let Some(local) = local() {
            return local.health();
        }
        self.health.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).map_or(Health::Unknown, |(_, health)| health)
    }

    /// Forgets what `health` found (after Everything was restarted, say).
    pub fn forget_health(&self) {
        *self.health.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn check(&self, probe: &Path) -> Health {
        let status = self.status();
        if !status.available {
            return Health::NotRunning;
        }
        if !status.db_loaded {
            return Health::Loading;
        }
        let service = service::running();
        let can_watch = status.admin == Some(true) || service == Some(true);
        // 1.5 doesn't say whether it's administrator: only the probe tells.
        let unsure = can_watch || status.admin.is_none();
        let fix = match service {
            _ if can_watch => Fix::Restart,
            Some(_) => Fix::StartService,
            None => Fix::InstallService,
        };
        // Neither administrator nor with its service: it can't watch NTFS drives at all, which needs no probe to say.
        if status.admin == Some(false) && service != Some(true) {
            return Health::Behind(fix);
        }
        match self.notices(probe, if unsure { Duration::from_secs(30) } else { Duration::from_secs(4) }) {
            Some(true) => Health::Live,
            Some(false) => Health::Behind(fix),
            // It doesn't know the probe at all, so its folder may be left out of Everything's list: only what it says of itself.
            None if unsure => Health::Live,
            None => Health::Behind(fix),
        }
    }

    /// Whether Everything notices `probe` being written: yes within `patience`, no when it knows the file but not that
    /// it changed, nothing when it doesn't know the file (or it can't be written).
    pub fn notices(&self, probe: &Path, patience: Duration) -> Option<bool> {
        if let Some(local) = local() {
            return Some(local.health() == Health::Live);
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
        std::fs::write(probe, now.to_string()).ok()?;
        let path = probe.display().to_string();
        let name = probe.file_name()?.to_string_lossy().to_string();
        // By its name, not its path: a quoted path makes Everything look at every path (3.8 s of its CPU each, against
        // 0.44 s by name). The full path is checked below.
        let query = Query { count: 100, ..Query::listing(format!("wfn:\"{name}\""), Sort::ModifiedDescending) };
        let started = Instant::now();
        let mut known = false;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let found = self.ask(vec![query.clone()]).ok().into_iter().flatten().flat_map(|page| page.items)
                .find(|item| item.path().eq_ignore_ascii_case(&path));
            if let Some(item) = found {
                known = true;
                if item.modified.is_some_and(|modified| modified >= now - 1) {
                    return Some(true);
                }
            }
            if started.elapsed() >= patience {
                return known.then_some(false);
            }
        }
    }

    /// Whether Everything answers, and which (blocking: a round trip).
    pub fn status(&self) -> Status {
        let (sender, receiver) = channel();
        if self.jobs.send(Job::Status(sender)).is_err() {
            return Status::default();
        }
        receiver.recv().unwrap_or_default()
    }

    /// A result was opened: Everything counts it, which feeds its run-count and date-run sorts.
    pub fn ran(&self, path: &str) {
        let _ = self.jobs.send(Job::Ran(path.to_string()));
    }
}

#[derive(Default)]
struct Worker {
    /// SDK3's client while connected to Everything 1.5.
    client: Option<std::ptr::NonNull<ffi::Client>>,
    /// When connecting to 1.5 last failed: tried again after a while, not on every query.
    pipe_failed: Option<Instant>,
    buffer: Vec<u16>,
}

impl Worker {
    fn run(mut self, jobs: Receiver<Job>, latest: Arc<AtomicU64>) {
        self.buffer = vec![0u16; 32_768];
        while let Ok(job) = jobs.recv() {
            match job {
                Job::Search { generation, queries, reply } => {
                    let mut pages = Vec::with_capacity(queries.len());
                    let mut outcome = Ok(());
                    for query in &queries {
                        if generation.is_some_and(|generation| generation < latest.load(Ordering::SeqCst)) {
                            outcome = Err(Error::Superseded);
                            break;
                        }
                        match self.search(query) {
                            Ok(page) => pages.push(page),
                            Err(error) => {
                                outcome = Err(error);
                                break;
                            }
                        }
                    }
                    reply(outcome.map(|()| pages));
                }
                Job::Status(reply) => {
                    let _ = reply.send(self.status());
                }
                Job::Ran(_) if local().is_some() => {}
                Job::Ran(path) => {
                    let wide = wide(&path);
                    // SAFETY: the SDKs are only called on this thread; `wide` is NUL-terminated and outlives the call.
                    unsafe {
                        match self.client() {
                            Some(client) => {
                                ffi::Everything3_IncRunCountFromFilenameW(client, wide.as_ptr());
                            }
                            None => {
                                ffi::Everything_IncRunCountFromFileNameW(wide.as_ptr());
                            }
                        }
                    }
                }
            }
        }
        self.disconnect();
    }

    /// The SDK3 client, connecting when it's been a while since that last failed.
    fn client(&mut self) -> Option<*mut ffi::Client> {
        if let Some(client) = self.client {
            return Some(client.as_ptr());
        }
        if self.pipe_failed.is_some_and(|failed| failed.elapsed() < Duration::from_secs(5)) {
            return None;
        }
        // SAFETY: a null name is Everything 1.5's default instance; "1.5a" the alpha's.
        let client = unsafe {
            let client = ffi::Everything3_ConnectW(std::ptr::null());
            if client.is_null() { ffi::Everything3_ConnectW(wide("1.5a").as_ptr()) } else { client }
        };
        self.client = std::ptr::NonNull::new(client);
        if self.client.is_none() {
            self.pipe_failed = Some(Instant::now());
        }
        self.client.map(|client| client.as_ptr())
    }

    fn disconnect(&mut self) {
        if let Some(client) = self.client.take() {
            // SAFETY: a client this thread connected, destroyed once.
            unsafe { ffi::Everything3_DestroyClient(client.as_ptr()) };
        }
    }

    fn search(&mut self, query: &Query) -> Result<Page, Error> {
        let started = Instant::now();
        if let Some(local) = local() {
            let mut page = local.search(query)?;
            page.elapsed = started.elapsed();
            return Ok(page);
        }
        if disabled() {
            return Err(Error::NotRunning);
        }
        // Everything has no fuzzy search: each word's letters in order, anything between ("mcr11": *m*c*r*1*1*).
        let in_order;
        let query = if query.fuzzy {
            let words: Vec<String> = query.text.split_whitespace()
                .map(|word| format!("*{}*", word.chars().filter(|c| c.is_alphanumeric()).map(String::from).collect::<Vec<_>>().join("*")))
                .filter(|word| word.len() > 2)
                .collect();
            // Nothing to match (only punctuation): nothing found, as the index's own fuzzy search finds, rather than
            // an empty query, which Everything answers with every file.
            if words.is_empty() {
                return Ok(Page { total: 0, offset: query.offset, items: Vec::new(), elapsed: started.elapsed() });
            }
            in_order = Query { text: words.join(" "), fuzzy: false, regex: false, whole_word: false, ..query.clone() };
            &in_order
        } else {
            query
        };
        if let Some(client) = self.client() {
            match self.search_pipe(client, query) {
                Ok(mut page) => {
                    page.elapsed = started.elapsed();
                    return Ok(page);
                }
                // Everything 1.5 went away (quit, restarted): 1.4's IPC this time, the pipe again later.
                Err(_) => {
                    self.disconnect();
                    self.pipe_failed = Some(Instant::now());
                }
            }
        }
        let mut page = self.search_ipc(query)?;
        page.elapsed = started.elapsed();
        Ok(page)
    }

    /// Everything 1.5 through SDK3.
    fn search_pipe(&mut self, client: *mut ffi::Client, query: &Query) -> Result<Page, Error> {
        let (property, ascending) = query.sort.sdk3();
        let text = wide(&query.text);
        // SAFETY: the search state and result list are made and destroyed here; buffers outlive their calls.
        unsafe {
            let state = ffi::Everything3_CreateSearchState();
            if state.is_null() {
                return Err(Error::Failed("out of memory".into()));
            }
            ffi::Everything3_SetSearchTextW(state, text.as_ptr());
            ffi::Everything3_SetSearchMatchCase(state, query.match_case as i32);
            ffi::Everything3_SetSearchMatchPath(state, query.match_path as i32);
            ffi::Everything3_SetSearchMatchWholeWords(state, (query.whole_word && !query.regex) as i32);
            ffi::Everything3_SetSearchRegex(state, query.regex as i32);
            ffi::Everything3_AddSearchSort(state, property, ascending as i32);
            ffi::Everything3_AddSearchPropertyRequestHighlighted(state, PROPERTY_NAME);
            let search = query.fields == Fields::Search;
            for property in [PROPERTY_PATH, PROPERTY_SIZE, PROPERTY_DATE_MODIFIED] {
                ffi::Everything3_AddSearchPropertyRequest(state, property);
            }
            if search {
                for property in [PROPERTY_RUN_COUNT, PROPERTY_DATE_RUN] {
                    ffi::Everything3_AddSearchPropertyRequest(state, property);
                }
            }
            ffi::Everything3_SetSearchViewportOffset(state, query.offset as usize);
            ffi::Everything3_SetSearchViewportCount(state, query.count as usize);
            let list = ffi::Everything3_Search(client, state);
            ffi::Everything3_DestroySearchState(state);
            if list.is_null() {
                return Err(Error::Failed(format!("Everything 1.5: error {:#x}", ffi::Everything3_GetLastError())));
            }
            let total = ffi::Everything3_GetResultListCount(list) as u64;
            let count = ffi::Everything3_GetResultListViewportCount(list);
            let buffer = &mut self.buffer;
            let mut items = Vec::with_capacity(count);
            for index in 0..count {
                let length = ffi::Everything3_GetResultPropertyTextHighlightedW(list, index, PROPERTY_NAME,
                                                                                buffer.as_mut_ptr(), buffer.len());
                let marked = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
                let length = ffi::Everything3_GetResultPathW(list, index, buffer.as_mut_ptr(), buffer.len());
                let folder = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
                let is_folder = ffi::Everything3_IsFolderResult(list, index) != 0;
                let (run_count, date_run) = if search {
                    (ffi::Everything3_GetResultRunCount(list, index), ffi::Everything3_GetResultDateRun(list, index))
                } else {
                    (u32::MAX, u64::MAX)
                };
                items.push(item(&marked, folder, is_folder, ffi::Everything3_GetResultSize(list, index),
                                ffi::Everything3_GetResultDateModified(list, index), run_count, date_run));
            }
            ffi::Everything3_DestroyResultList(list);
            Ok(Page { total, offset: query.offset, items, elapsed: Duration::ZERO })
        }
    }

    /// Everything 1.4 through its IPC SDK.
    fn search_ipc(&mut self, query: &Query) -> Result<Page, Error> {
        let text = wide(&query.text);
        // SAFETY: the SDK's globals are only touched on this thread; its strings are read before the next query.
        unsafe {
            ffi::Everything_SetSearchW(text.as_ptr());
            let search = query.fields == Fields::Search;
            let listing = REQUEST_PATH | REQUEST_HIGHLIGHTED_FILE_NAME | REQUEST_SIZE | REQUEST_DATE_MODIFIED;
            ffi::Everything_SetRequestFlags(if search { listing | REQUEST_RUN_COUNT | REQUEST_DATE_RUN } else { listing });
            ffi::Everything_SetSort(query.sort.sdk1());
            ffi::Everything_SetMatchCase(query.match_case as i32);
            ffi::Everything_SetMatchPath(query.match_path as i32);
            ffi::Everything_SetMatchWholeWord((query.whole_word && !query.regex) as i32);
            ffi::Everything_SetRegex(query.regex as i32);
            ffi::Everything_SetOffset(query.offset);
            ffi::Everything_SetMax(query.count);
            if ffi::Everything_QueryW(1) == 0 {
                return Err(match ffi::Everything_GetLastError() {
                    2 => Error::NotRunning, // EVERYTHING_ERROR_IPC
                    code => Error::Failed(format!("Everything: error {code}")),
                });
            }
            let total = ffi::Everything_GetTotResults() as u64;
            let count = ffi::Everything_GetNumResults();
            let mut items = Vec::with_capacity(count as usize);
            for index in 0..count {
                let marked = from_wide_ptr(ffi::Everything_GetResultHighlightedFileNameW(index));
                let folder = from_wide_ptr(ffi::Everything_GetResultPathW(index));
                let is_folder = ffi::Everything_IsFolderResult(index) != 0;
                let mut size = 0i64;
                let size = if ffi::Everything_GetResultSize(index, &mut size) != 0 { size as u64 } else { u64::MAX };
                let mut modified = 0u64;
                let modified = if ffi::Everything_GetResultDateModified(index, &mut modified) != 0 { modified } else { u64::MAX };
                let (run_count, date_run) = if search {
                    let mut run = 0u64;
                    let run = if ffi::Everything_GetResultDateRun(index, &mut run) != 0 { run } else { u64::MAX };
                    (ffi::Everything_GetResultRunCount(index), run)
                } else {
                    (u32::MAX, u64::MAX)
                };
                items.push(item(&marked, folder, is_folder, size, modified, run_count, date_run));
            }
            Ok(Page { total, offset: query.offset, items, elapsed: Duration::ZERO })
        }
    }

    fn status(&mut self) -> Status {
        if let Some(local) = local() {
            return local.status();
        }
        if disabled() {
            return Status::default();
        }
        let installed = installed_everything();
        // SAFETY: as in the searches.
        unsafe {
            if let Some(client) = self.client() {
                let major = ffi::Everything3_GetMajorVersion(client);
                if major > 0 {
                    return Status {
                        available: true,
                        version: format!("{}.{}.{}.{}", major, ffi::Everything3_GetMinorVersion(client),
                                         ffi::Everything3_GetRevision(client), ffi::Everything3_GetBuildNumber(client)),
                        pipe: true,
                        db_loaded: ffi::Everything3_IsDBLoaded(client) != 0,
                        admin: None,
                        installed,
                    };
                }
                self.disconnect();
            }
            let major = ffi::Everything_GetMajorVersion();
            if major == 0 && ffi::Everything_GetLastError() != 0 {
                return Status { installed, ..Status::default() };
            }
            Status {
                available: true,
                version: format!("{}.{}.{}.{}", major, ffi::Everything_GetMinorVersion(), ffi::Everything_GetRevision(),
                                 ffi::Everything_GetBuildNumber()),
                pipe: false,
                db_loaded: ffi::Everything_IsDBLoaded() != 0,
                admin: Some(ffi::Everything_IsAdmin() != 0),
                installed,
            }
        }
    }
}

/// A result from what the SDKs return: missing values and folder sizes come as all ones.
fn item(marked: &str, folder: String, is_folder: bool, size: u64, modified: u64, run_count: u32, date_run: u64) -> Item {
    let (name, highlights) = parse_marks(marked);
    Item {
        name,
        folder,
        highlights,
        is_folder,
        size: (!is_folder && size != u64::MAX && (size as i64) >= 0).then_some(size),
        modified: unix_seconds(modified),
        run_count: if run_count == u32::MAX { 0 } else { run_count },
        date_run: unix_seconds(date_run),
    }
}

/// A FILETIME (100 ns since 1601) as seconds since 1970; none for 0 or all ones.
fn unix_seconds(filetime: u64) -> Option<i64> {
    (filetime != 0 && filetime != u64::MAX).then(|| (filetime as i64 - 116_444_736_000_000_000) / 10_000_000)
}

/// Everything's highlighting (`*` toggles it, `**` is a literal star) as plain text and the byte ranges marked.
pub fn parse_marks(marked: &str) -> (String, Vec<Range<usize>>) {
    let mut text = String::with_capacity(marked.len());
    let mut ranges = Vec::new();
    let mut open: Option<usize> = None;
    let mut chars = marked.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                text.push('*');
            } else if let Some(start) = open.take() {
                if start < text.len() {
                    ranges.push(start..text.len());
                }
            } else {
                open = Some(text.len());
            }
        } else {
            text.push(c);
        }
    }
    if let Some(start) = open {
        if start < text.len() {
            ranges.push(start..text.len());
        }
    }
    (text, ranges)
}

/// Where Everything.exe is installed: its uninstall entry, or the usual places.
pub fn installed_everything() -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for root in [registry::HKEY_LOCAL_MACHINE, registry::HKEY_CURRENT_USER] {
        if let Some(folder) = registry::string(root, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Everything",
                                               "InstallLocation") {
            candidates.push(std::path::Path::new(&folder).join("Everything.exe"));
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(folder) = std::env::var_os(variable) {
            let folder = std::path::PathBuf::from(folder);
            candidates.push(folder.join("Everything").join("Everything.exe"));
            candidates.push(folder.join("Everything 1.5a").join("Everything64.exe"));
            candidates.push(folder.join("Programs").join("Everything").join("Everything.exe"));
        }
    }
    candidates.into_iter().find(|path| path.is_file()).map(|path| path.display().to_string())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// # Safety
/// `pointer` is null or a NUL-terminated UTF-16 string valid during the call.
unsafe fn from_wide_ptr(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: NUL-terminated per the contract above.
    unsafe {
        let mut length = 0;
        while *pointer.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
    }
}

const REQUEST_PATH: u32 = 0x0000_0002;
const REQUEST_SIZE: u32 = 0x0000_0010;
const REQUEST_DATE_MODIFIED: u32 = 0x0000_0040;
#[cfg(test)]
const REQUEST_ATTRIBUTES: u32 = 0x0000_0100;
const REQUEST_RUN_COUNT: u32 = 0x0000_0400;
const REQUEST_DATE_RUN: u32 = 0x0000_0800;
const REQUEST_HIGHLIGHTED_FILE_NAME: u32 = 0x0000_2000;
const PROPERTY_NAME: u32 = 0;
const PROPERTY_PATH: u32 = 1;
const PROPERTY_SIZE: u32 = 2;
const PROPERTY_EXTENSION: u32 = 3;
const PROPERTY_DATE_MODIFIED: u32 = 5;
const PROPERTY_RUN_COUNT: u32 = 10;
const PROPERTY_DATE_RUN: u32 = 11;

/// Everything's service, without pulling in a Windows bindings crate.
mod service {
    #[repr(C)]
    #[derive(Default)]
    struct ServiceStatus {
        kind: u32,
        state: u32,
        controls: u32,
        exit_code: u32,
        service_exit_code: u32,
        checkpoint: u32,
        wait_hint: u32,
    }

    unsafe extern "system" {
        fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> isize;
        fn OpenServiceW(manager: isize, name: *const u16, access: u32) -> isize;
        fn QueryServiceStatus(service: isize, status: *mut ServiceStatus) -> i32;
        fn CloseServiceHandle(handle: isize) -> i32;
    }

    const SC_MANAGER_CONNECT: u32 = 0x1;
    const SERVICE_QUERY_STATUS: u32 = 0x4;
    const SERVICE_RUNNING: u32 = 4;

    /// Whether it runs; nothing when it isn't installed. (1.5's alpha names its own.)
    pub fn running() -> Option<bool> {
        // SAFETY: each handle opened here is closed here; the names outlive their calls.
        unsafe {
            let manager = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if manager == 0 {
                return None;
            }
            let mut found = None;
            for name in ["Everything", "Everything (1.5a)"] {
                let service = OpenServiceW(manager, super::wide(name).as_ptr(), SERVICE_QUERY_STATUS);
                if service == 0 {
                    continue;
                }
                let mut status = ServiceStatus::default();
                let running = QueryServiceStatus(service, &mut status) != 0 && status.state == SERVICE_RUNNING;
                CloseServiceHandle(service);
                found = Some(found.unwrap_or(false) || running);
            }
            CloseServiceHandle(manager);
            found
        }
    }
}

/// Reading the uninstall entry without pulling in a Windows bindings crate.
mod registry {
    pub type Hkey = isize;
    pub const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as i32 as isize;
    pub const HKEY_LOCAL_MACHINE: Hkey = 0x8000_0002u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 0x2;

    unsafe extern "system" {
        fn RegGetValueW(key: Hkey, subkey: *const u16, value: *const u16, flags: u32, kind: *mut u32,
                        data: *mut core::ffi::c_void, size: *mut u32) -> i32;
    }

    pub fn string(root: Hkey, key: &str, value: &str) -> Option<String> {
        let (key, value) = (super::wide(key), super::wide(value));
        let mut size = 0u32;
        // SAFETY: the first call asks the size, the second fills a buffer of that size.
        unsafe {
            if RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(),
                            std::ptr::null_mut(), &mut size) != 0 || size == 0 {
                return None;
            }
            let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
            if RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(),
                            buffer.as_mut_ptr().cast(), &mut size) != 0 {
                return None;
            }
            let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            Some(String::from_utf16_lossy(&buffer[..length]))
        }
    }
}

/// The SDK calls used (sdk/sdk1/include/Everything.h, sdk/sdk3/include/Everything3.h).
#[allow(non_snake_case)]
mod ffi {
    #[repr(C)]
    pub struct Client {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct SearchState {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct ResultList {
        _private: [u8; 0],
    }

    unsafe extern "system" {
        // Everything 1.4 (window messages).
        pub fn Everything_SetSearchW(search: *const u16);
        pub fn Everything_SetRequestFlags(flags: u32);
        pub fn Everything_SetSort(sort: u32);
        pub fn Everything_SetMatchCase(enable: i32);
        pub fn Everything_SetMatchPath(enable: i32);
        pub fn Everything_SetMatchWholeWord(enable: i32);
        pub fn Everything_SetRegex(enable: i32);
        pub fn Everything_SetOffset(offset: u32);
        pub fn Everything_SetMax(max: u32);
        pub fn Everything_QueryW(wait: i32) -> i32;
        pub fn Everything_GetLastError() -> u32;
        pub fn Everything_GetTotResults() -> u32;
        pub fn Everything_GetNumResults() -> u32;
        pub fn Everything_IsFolderResult(index: u32) -> i32;
        pub fn Everything_GetResultPathW(index: u32) -> *const u16;
        pub fn Everything_GetResultHighlightedFileNameW(index: u32) -> *const u16;
        pub fn Everything_GetResultSize(index: u32, size: *mut i64) -> i32;
        pub fn Everything_GetResultDateModified(index: u32, filetime: *mut u64) -> i32;
        pub fn Everything_GetResultDateRun(index: u32, filetime: *mut u64) -> i32;
        pub fn Everything_GetResultRunCount(index: u32) -> u32;
        pub fn Everything_GetMajorVersion() -> u32;
        pub fn Everything_GetMinorVersion() -> u32;
        pub fn Everything_GetRevision() -> u32;
        pub fn Everything_GetBuildNumber() -> u32;
        pub fn Everything_IsDBLoaded() -> i32;
        pub fn Everything_IsAdmin() -> i32;
        pub fn Everything_IncRunCountFromFileNameW(name: *const u16) -> u32;

        // Everything 1.5 (SDK3, named pipe).
        pub fn Everything3_ConnectW(instance: *const u16) -> *mut Client;
        pub fn Everything3_DestroyClient(client: *mut Client) -> i32;
        pub fn Everything3_GetLastError() -> u32;
        pub fn Everything3_GetMajorVersion(client: *mut Client) -> u32;
        pub fn Everything3_GetMinorVersion(client: *mut Client) -> u32;
        pub fn Everything3_GetRevision(client: *mut Client) -> u32;
        pub fn Everything3_GetBuildNumber(client: *mut Client) -> u32;
        pub fn Everything3_IsDBLoaded(client: *mut Client) -> i32;
        pub fn Everything3_IncRunCountFromFilenameW(client: *mut Client, name: *const u16) -> u32;
        pub fn Everything3_CreateSearchState() -> *mut SearchState;
        pub fn Everything3_DestroySearchState(state: *mut SearchState) -> i32;
        pub fn Everything3_SetSearchTextW(state: *mut SearchState, text: *const u16) -> i32;
        pub fn Everything3_SetSearchMatchCase(state: *mut SearchState, on: i32) -> i32;
        pub fn Everything3_SetSearchMatchPath(state: *mut SearchState, on: i32) -> i32;
        pub fn Everything3_SetSearchMatchWholeWords(state: *mut SearchState, on: i32) -> i32;
        pub fn Everything3_SetSearchRegex(state: *mut SearchState, on: i32) -> i32;
        pub fn Everything3_AddSearchSort(state: *mut SearchState, property: u32, ascending: i32) -> i32;
        pub fn Everything3_AddSearchPropertyRequest(state: *mut SearchState, property: u32) -> i32;
        pub fn Everything3_AddSearchPropertyRequestHighlighted(state: *mut SearchState, property: u32) -> i32;
        pub fn Everything3_SetSearchViewportOffset(state: *mut SearchState, offset: usize) -> i32;
        pub fn Everything3_SetSearchViewportCount(state: *mut SearchState, count: usize) -> i32;
        pub fn Everything3_Search(client: *mut Client, state: *mut SearchState) -> *mut ResultList;
        pub fn Everything3_DestroyResultList(list: *mut ResultList) -> i32;
        pub fn Everything3_GetResultListCount(list: *const ResultList) -> usize;
        pub fn Everything3_GetResultListViewportCount(list: *const ResultList) -> usize;
        pub fn Everything3_IsFolderResult(list: *const ResultList, index: usize) -> i32;
        pub fn Everything3_GetResultPathW(list: *const ResultList, index: usize, buffer: *mut u16, size: usize) -> usize;
        pub fn Everything3_GetResultPropertyTextHighlightedW(list: *const ResultList, index: usize, property: u32,
                                                             buffer: *mut u16, size: usize) -> usize;
        pub fn Everything3_GetResultSize(list: *const ResultList, index: usize) -> u64;
        pub fn Everything3_GetResultDateModified(list: *const ResultList, index: usize) -> u64;
        pub fn Everything3_GetResultRunCount(list: *const ResultList, index: usize) -> u32;
        pub fn Everything3_GetResultDateRun(list: *const ResultList, index: usize) -> u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks() {
        assert_eq!(parse_marks("*rep*ort.pdf"), ("report.pdf".into(), vec![0..3]));
        assert_eq!(parse_marks("a**b *c*"), ("a*b c".into(), vec![4..5]));
        assert_eq!(parse_marks("open *end"), ("open end".into(), vec![5..8]));
        assert_eq!(parse_marks("plain"), ("plain".into(), vec![]));
        assert_eq!(parse_marks("*é*t*é*"), ("été".into(), vec![0..2, 3..5]));
    }

    /// A listing leaves out what's slow to ask for, and comes a page at a time.
    #[test]
    fn listings() {
        let query = Query::listing(r#"file: "C:\x\""#, Sort::NameAscending);
        assert_eq!((query.fields, query.count, query.offset), (Fields::Listing, PAGE, 0));
        assert_eq!(Query::new("a", Sort::NameAscending, 50).fields, Fields::Search);
    }

    #[test]
    fn paths() {
        let item = |name: &str, folder: &str| Item { name: name.into(), folder: folder.into(), ..Item::default() };
        assert_eq!(item("C:", "").path(), r"C:\");
        assert_eq!(item("a.txt", r"C:\").path(), r"C:\a.txt");
        assert_eq!(item("a.txt", r"C:\x").path(), r"C:\x\a.txt");
    }

    #[test]
    fn filetimes() {
        assert_eq!(unix_seconds(116_444_736_000_000_000), Some(0));
        assert_eq!(unix_seconds(0), None);
        assert_eq!(unix_seconds(u64::MAX), None);
    }

    /// Which request flags cost time on Everything 1.4 (run with --ignored --nocapture).
    #[test]
    #[ignore]
    fn probe_flags() {
        let text = wide("explorer.exe");
        let sets = [
            ("name only", REQUEST_HIGHLIGHTED_FILE_NAME),
            ("+path", REQUEST_HIGHLIGHTED_FILE_NAME | REQUEST_PATH),
            ("+size+date", REQUEST_HIGHLIGHTED_FILE_NAME | REQUEST_PATH | REQUEST_SIZE | REQUEST_DATE_MODIFIED),
            ("+attributes", REQUEST_HIGHLIGHTED_FILE_NAME | REQUEST_PATH | REQUEST_ATTRIBUTES),
            ("+run count", REQUEST_HIGHLIGHTED_FILE_NAME | REQUEST_PATH | REQUEST_RUN_COUNT),
            ("+date run", REQUEST_HIGHLIGHTED_FILE_NAME | REQUEST_PATH | REQUEST_DATE_RUN),
            ("plain name+path", 0x1 | REQUEST_PATH),
        ];
        for (label, flags) in sets {
            for sort in [1u32, 14] {
                let mut times = Vec::new();
                for _ in 0..5 {
                    let started = Instant::now();
                    // SAFETY: test thread only.
                    unsafe {
                        ffi::Everything_SetSearchW(text.as_ptr());
                        ffi::Everything_SetRequestFlags(flags);
                        ffi::Everything_SetSort(sort);
                        ffi::Everything_SetOffset(0);
                        ffi::Everything_SetMax(50);
                        assert_ne!(ffi::Everything_QueryW(1), 0);
                    }
                    times.push(started.elapsed().as_secs_f64() * 1000.0);
                }
                times.sort_by(f64::total_cmp);
                println!("{label:16} sort {sort:2}: median {:.1} ms (min {:.1})", times[2], times[0]);
            }
        }
    }

    /// Against the Everything running here (skipped when there's none). Prints the round trips.
    #[test]
    fn live() {
        let everything = Everything::start();
        let status = everything.status();
        if !status.available {
            eprintln!("no Everything running: skipped");
            return;
        }
        let pages = everything
            .search_blocking(1, vec![Query::new("explorer.exe", Sort::NameAscending, 5),
                                     Query::new("windows", Sort::ModifiedDescending, 50)])
            .unwrap();
        assert!(pages[0].items.iter().any(|item| item.name.eq_ignore_ascii_case("explorer.exe")), "{:?}", pages[0]);
        assert!(!pages[0].items[0].highlights.is_empty());
        assert!(pages[0].items[0].path().contains('\\'));
        assert!(pages[1].total > 0);
        println!("Everything {} ({}): explorer.exe {:?}, windows by date {:?} of {}", status.version,
                 if status.pipe { "pipe" } else { "ipc" }, pages[0].elapsed, pages[1].elapsed, pages[1].total);

        // A newer batch drops an older one still waiting.
        let (sender, receiver) = channel();
        let first = sender.clone();
        everything.search(5, vec![Query::new("a", Sort::NameAscending, 1); 20], move |result| {
            let _ = first.send((5, result.map(|pages| pages.len())));
        });
        everything.search(6, vec![Query::new("b", Sort::NameAscending, 1)], move |result| {
            let _ = sender.send((6, result.map(|pages| pages.len())));
        });
        let mut replies: Vec<_> = receiver.iter().take(2).collect();
        replies.sort_by_key(|reply| reply.0);
        assert_eq!(replies[1], (6, Ok(1)));
        assert!(matches!(replies[0].1, Err(Error::Superseded) | Ok(20)));
    }
}
