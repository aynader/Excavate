//! The popup's behavior, on the UI thread. Each keystroke:
//!
//! 1. ranks the hot tier in-process and shows it in the same frame (with the previous keystroke's Everything results
//!    that still match, when the new text extends the old),
//! 2. sends Everything a cheap prefix query and the full query (the long tail),
//! 3. merges each answer as it comes, keeping the selected result selected while the order settles.
//!
//! "Show all" switches to a virtual list over every Everything result in a chosen order, fetched a page at a time.

use crate::contents::Contents;
use crate::format;
use crate::icons::{self, KindKey, Loader, Request};
use crate::settings::{Paths, Settings, log};
use crate::{Popup, Preview, Row, Theme, with_app};
use everything::{Everything, Item, Page, Sort};
use search::filter::kind_label;
use search::history::History;
use search::hot::{HotIndex, Sources};
use search::rank::{self, Candidate, Context, Origin, Ranked};
use search::plan::Scope;
use search::query::Matching;
use search::asked;
use search::describe::{self, Route};
use search::{Filter, Terms, plan};
use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use winsys::files::Known;

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or(0)
}

/// What a row stands for.
#[derive(Clone, Debug, Default)]
pub struct Target {
    pub path: String,
    pub name: String,
    pub is_folder: bool,
    pub size: Option<u64>,
    pub modified: Option<i64>,
    pub origin: Option<Origin>,
}

impl From<&Candidate> for Target {
    fn from(candidate: &Candidate) -> Target {
        Target { path: candidate.path(), name: candidate.name.clone(), is_folder: candidate.is_folder,
                 size: candidate.size, modified: candidate.modified, origin: candidate.origin }
    }
}

impl From<&Item> for Target {
    fn from(item: &Item) -> Target {
        Target { path: item.path(), name: item.name.clone(), is_folder: item.is_folder, size: item.size,
                 modified: item.modified, origin: Some(Origin::Everything) }
    }
}

/// Which of a keystroke's Everything queries answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Fast,
    Main,
    Refine,
    /// Files of the type the text names, by the rest of it (`plan::typed`).
    Typed,
    /// Names with any of the words, when none had them all (`plan::loose`).
    Loose,
    /// Names with the words that aren't joining ones (`plan::relaxed`), after those with all.
    Relaxed,
}

/// The search field's toggles for videos and recordings, after the two for images: the kind each chooses, and what of it
/// it searches, as ">vt" (the text videos show), "<v" (what they show) and ">a" (what's said in recordings) do.
const MEDIA_TOGGLES: [(Filter, Only); 3] = [
    (Filter::Video, Only { inside: true, extension: None, part: Some(asked::Part::Shown) }),
    (Filter::Video, Only { inside: false, extension: None, part: None }),
    (Filter::Audio, Only { inside: true, extension: None, part: None }),
];

/// What `>` or `<` and a kind other than images asked for (">v theory of computation"): that kind only (its chip is
/// chosen), by the words inside it or by what it shows or means; and maybe one extension of it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Only {
    inside: bool,
    extension: Option<&'static str>,
    /// `>vt` or `>vs`: only the text videos show, or only what's said in them.
    part: Option<asked::Part>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Ranked,
    All(Sort),
}

/// Timings of the last keystroke, for the footer and the log.
#[derive(Default)]
struct Timing {
    typed: Option<Instant>,
    hot: Option<Duration>,
    fast: Option<Duration>,
    main: Option<Duration>,
    everything_ms: Option<f64>,
    /// The content index's search (on its thread).
    contents: Option<Duration>,
    /// The search helper's, for images by what they show.
    seeing_ms: Option<f64>,
}

struct State {
    generation: u64,
    terms: Terms,
    filter: Filter,
    mode: Mode,
    /// Only words read from images: names are left out (the search field's image toggle).
    image_text: bool,
    /// Only images, by what they show (the second image toggle).
    image_shown: bool,
    /// The toggles for videos and recordings, clicked (`MEDIA_TOGGLES`): which one, and the kind only it asks for, as
    /// its typed prefix would (">vt", "<v", ">a"). Typing a prefix goes first.
    media: Option<usize>,
    /// A toggle the typing turned on rather than a click, and a click that turned it off: the typing doesn't turn it on
    /// again until the field is cleared.
    by_typing: Option<Typed>,
    refused: bool,
    /// Whether the text names a file or describes one.
    route: Route,
    /// The kind of file a description asks for ("that photo of my cake").
    wants: Option<describe::Kind>,
    /// The type that comes first: named in the text ("the pdf of my passport"), or the kind a description asks for.
    /// Files of it are ranked by the rest of the text (`core`: "passport").
    first: Option<asked::First>,
    core: Option<Terms>,
    /// The words without the joining ones ("passport" of "my passport"), when some were: names with only those come
    /// after names with all of them, and the words inside files are searched by those (`asked::relaxed`).
    relaxed: Option<Terms>,
    /// "Fuzzy match" chosen (beside "Best match"): names that fit the words loosely, the closest first, and nothing
    /// else (not the words inside files, nor what images show).
    fuzzy: bool,
    /// Functions answered here (`functions`: `content:`, `width:`, `length:`, `rc:`, `dr:`…), out of the text.
    extra: crate::functions::Extra,
    /// For `content:`: the files whose words have them (lowercased paths), once the content index answered.
    extra_content: Option<HashSet<String>>,
    /// What was typed, the functions answered here included (`terms` has the rest).
    typed: String,
    /// Names with those (`Part::Relaxed`).
    relaxed_found: Vec<Candidate>,
    /// `>` or `<` and a kind other than images; and whether its chip was chosen by the typing (it's undone with it).
    only: Option<Only>,
    filter_by_typing: bool,
    /// Names with any of the words (`Part::Loose`).
    loose: Vec<Candidate>,
    /// The helper's best, however little they stand out: shown when nothing else is found. And how many images it knows.
    closest: Vec<Candidate>,
    seen_images: Option<usize>,
    /// The list shows those, nothing else having been found.
    showing_closest: bool,
    /// The list shows names with some of the words, none having them all.
    showing_loose: bool,
    /// Images that fit the text by what they show, best first, and whether the helper is still being asked.
    seen: Vec<Candidate>,
    seeing: bool,
    /// For videos among them, the moment that fits best (by lowercased path), in seconds.
    seen_at: HashMap<String, f64>,
    /// For recordings found by what's said, when the words are said, in seconds.
    heard_at: HashMap<String, f64>,
    /// Everything's candidates for the current text (both queries), and its total for the full query.
    found: Vec<Candidate>,
    /// What's in `found` is for the text before: it stays on screen until the answer replaces it.
    found_stale: bool,
    total: Option<u64>,
    fast_done: bool,
    main_done: bool,
    refine_done: bool,
    /// S1: a keystroke's results wait for the own index's answer (a few ms), so they change once, not twice.
    holding: bool,
    plan: Option<plan::Plan>,
    /// The content index's matches for the current text, and the snippet of each (by lowercased path).
    content: Vec<Candidate>,
    snippets: HashMap<String, (content::Snippet, content::Source)>,
    /// What the list shows, best first.
    results: Vec<Ranked>,
    /// The person moved the selection since typing: keep their choice across re-ranking.
    moved: bool,
    visible: (usize, usize),
    timing: Timing,
    /// The status line, and a message that replaces it for a moment ("Copied").
    notice: Option<(String, Instant)>,
    everything: everything::Status,
    /// Whether its answers include what's new.
    health: everything::Health,
    /// While Excavate gets Everything seeing new files again: the step it's on.
    fixing: Option<String>,
}

impl State {
    /// The words searched inside files: those left once a type named and the joining words are taken out, else those
    /// without the joining words; None: all the words typed.
    fn contents_terms(&self) -> Option<&Terms> {
        self.core.as_ref().or(self.relaxed.as_ref())
    }
}

pub struct App {
    pub ui: slint::Weak<Popup>,
    pub settings: RefCell<Settings>,
    pub paths: Paths,
    pub everything: Arc<Everything>,
    icons: Loader,
    images: RefCell<HashMap<String, slint::Image>>,
    row_keys: RefCell<Vec<(String, KindKey, String)>>,
    hot: RefCell<Arc<HotIndex>>,
    hot_building: Cell<bool>,
    history: RefCell<History>,
    /// Sizes in pixels and lengths read from files' headers, for `width:`/`height:`/`length:` (lowercased path), and
    /// those being read.
    facts: RefCell<HashMap<String, FileFacts>>,
    facts_asked: RefCell<HashSet<String>>,
    state: RefCell<State>,
    rows: Rc<VecModel<Row>>,
    all: RefCell<Option<Rc<AllModel>>>,
    pub hwnd: Cell<isize>,
    pub scale: Cell<f32>,
    pub visible: Cell<bool>,
    pub hidden_at: Cell<Option<Instant>>,
    pub show_started: Cell<Option<Instant>>,
    pub frames: Cell<u64>,
    /// Windows is keeping the window off the screen until its first frame is out.
    pub cloaked: Cell<bool>,
    pub uncloak_timer: slint::Timer,
    /// Closed, but kept showing behind DWM's cloak rather than hidden (what's drawn in it stays, so opening it again
    /// only reveals it); and the window that was in front before it opened, which gets the keyboard back.
    pub parked: Cell<bool>,
    pub before: Cell<isize>,
    /// Frames drawn by the time everything closing changed was in (the empty search): one drawn after it means what
    /// the parked window holds is up to date.
    pub park_frames: Cell<u64>,
    /// The window is being created out of sight.
    pub warming: Cell<bool>,
    /// And should open once it has been.
    pub show_wanted: Cell<bool>,
    preview_timer: slint::Timer,
    /// The video playing in the preview, by its turn: a new turn (another selection, the popup hidden) stops it.
    playing: Arc<std::sync::atomic::AtomicU64>,
    /// What's playing in the preview: a video (muted until it's clicked) or a recording (when it's clicked).
    media: RefCell<Media>,
    /// Asks what takes Everything most of a second once the typing rests.
    slow_timer: slint::Timer,
    /// S1: how long a keystroke's results wait for the own index at most (`HOLD`).
    hold_timer: slint::Timer,
    /// Asks the search helper once the typing rests.
    seeing_timer: slint::Timer,
    pub seeker: crate::seeking::Seeker,
    notice_timer: slint::Timer,
    pub profile: String,
    pub contents: Contents,
    pub indexer: crate::indexer::Indexer,
    /// What the index knows about the file in the preview: its secrets' boxes, to blur.
    preview_marks: RefCell<Option<(String, Marks)>>,
    /// The file whose secrets the person revealed (with Windows Hello), shown as it is.
    pub revealed: RefCell<Option<String>>,
    /// Windows Hello's dialog is up: the popup stays open while it has the focus.
    pub verifying: Cell<bool>,
    /// Follows the indexer's progress, only while the popup is on screen.
    reading_timer: slint::Timer,
    /// The size the popup was opened at, in physical pixels, to tell if the person changed it.
    pub opened_size: Cell<(u32, u32)>,
    /// The wheel moved an enlarged image, and that hasn't been drawn yet.
    pub wheel_at: Cell<Option<Instant>>,
    /// The newest request for an enlarged image: older answers are dropped.
    enlarged_request: Cell<u64>,
    /// The file and the size in pixels of the enlarged image asked for last.
    enlarged_asked: RefCell<Option<(String, (u32, u32))>>,
    /// Waits for the wheel, or the window's edge, to rest.
    enlarged_timer: slint::Timer,
    /// Where names are searched.
    scope: RefCell<Scope>,
}

/// Privacy marks the indexer stored for a file: which kinds of secrets, and for a picture where (in its own pixels).
#[derive(Clone, Debug, Default)]
pub struct Marks {
    pub kinds: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub boxes: Vec<[i32; 4]>,
    /// A video's: where secrets show and when, [from ms, to ms, left, top, right, bottom] (ten-thousandths of the frame).
    pub video_boxes: Vec<[i64; 6]>,
    /// The first sweep's: how many similar images this one stands for, for now.
    pub similar: u32,
    /// How the file's metadata says to turn it. The boxes are from before turning, and previews are the right way up.
    pub orientation: u16,
    /// A PDF's secrets on its first page, in ten-thousandths of the page (left, top, right, bottom). None: not placed
    /// (the page is covered whole); empty: none is on the first page.
    pub pdf_boxes: Option<Vec<[i64; 4]>>,
}

impl Marks {
    fn parse(bytes: &[u8]) -> Marks {
        let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else { return Marks::default() };
        let kinds = json["kinds"].as_array()
            .map(|kinds| kinds.iter().filter_map(|kind| kind.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let picture = &json["picture"];
        let boxes = picture["boxes"].as_array().map(|boxes| {
            boxes.iter().filter_map(|b| {
                let b = b.as_array()?;
                Some([b.first()?.as_i64()? as i32, b.get(1)?.as_i64()? as i32, b.get(2)?.as_i64()? as i32, b.get(3)?.as_i64()? as i32])
            }).collect()
        }).unwrap_or_default();
        let pdf_boxes = json["pdf"]["boxes"].as_array().map(|boxes| {
            boxes.iter().filter_map(|b| {
                let b = b.as_array()?;
                Some([b.first()?.as_i64()?, b.get(1)?.as_i64()?, b.get(2)?.as_i64()?, b.get(3)?.as_i64()?])
            }).collect()
        });
        let video_boxes = json["video"]["boxes"].as_array().map(|boxes| {
            boxes.iter().filter_map(|b| {
                let b = b.as_array()?;
                let mut out = [0i64; 6];
                for (slot, value) in out.iter_mut().zip(b) {
                    *slot = value.as_i64()?;
                }
                (b.len() == 6).then_some(out)
            }).collect()
        }).unwrap_or_default();
        Marks {
            kinds,
            width: picture["w"].as_u64().unwrap_or(0) as u32,
            height: picture["h"].as_u64().unwrap_or(0) as u32,
            boxes,
            video_boxes,
            similar: json["similar"].as_u64().unwrap_or(0) as u32,
            orientation: 1,
            pdf_boxes,
        }
    }
}

/// How sure the helper's classifier of the kind a description asks for must be to replace the rules' guess: for a
/// description, and for what the rules take for a name (0.99 for a name: on held-out
/// searches, a lower bar gave a kind to one name of 31, "notes.txt" a document).
const KIND_SURE: f32 = 0.8;
const KIND_SURE_NAME: f32 = 0.99;

const ICON_SIZE: f32 = 32.0;
const PREVIEW_SIZE: f32 = 256.0;
/// How long the typing has to rest before a slow search is asked, in milliseconds.
const SLOW_WAIT: u64 = 150;
/// S1: the longest a keystroke's results wait for the own index (its answer takes 3 to 5 ms).
const HOLD: Duration = Duration::from_millis(8);
/// How long the typing has to rest before the search helper is asked about images by what they show, in milliseconds:
/// longer while names come first, shorter with images alone.
const SEEING_WAIT: u64 = 300;
const SEEING_WAIT_ALONE: u64 = 120;
/// Images shown with the toggle on; and with it off, how many may join the names (when they stand out enough:
/// `seeking::Answer::line`).
const SEEN_SHOWN: usize = 60;
const SEEN_JOINING: usize = 4;
/// For a description (`describe::Route`), how far above the rest is enough to join: file names' best images stood 2.1
/// to 3.5 deviations above the rest, descriptions' 3.4 to 9.0 (`seeking::Answer::line`).
const DESCRIBED_LINE: f32 = 3.0;
/// What shows when nothing else is found: the helper's best images and documents, however little they stand out.
const CLOSEST: usize = 12;

/// An image toggle turned on by the typing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Typed {
    /// "> " in front (the experimental setting): the text in images.
    Text,
    /// "< " in front: what images show.
    Shown,
    /// A description of an image: what images show, until it finds nothing.
    Described,
}
/// What an enlarged image can't use of the popup, in logical pixels: its header and footer, and a margin.
const ENLARGED_AROUND: (f32, f32) = (24.0, 106.0);
/// How long the wheel has to rest before an enlarged image is decoded for its new size, in milliseconds.
const ENLARGED_REST: u64 = 120;
/// The longest side of an image at actual size, in pixels: past this it's scaled down, at about 200 MB already.
const ENLARGED_LARGEST: u32 = 8192;
/// The longest side an enlarged video is decoded at, in pixels (a 1080p video's own).
const ENLARGED_VIDEO: u32 = 1920;
/// Zoomed in, a video is decoded larger, up to its own size and at most this.
const ENLARGED_VIDEO_LARGEST: u32 = 3840;
/// A PDF's page is rendered at most this large (its longer side): zoomed further, it's scaled.
const ENLARGED_PDF_LARGEST: u32 = 4096;

impl App {
    pub fn new(ui: &Popup, settings: Settings, paths: Paths) -> Rc<App> {
        let history = History::load(&paths.history());
        let everything = Everything::shared();
        let icons = Loader::start(3, |request, bitmap| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.icon_ready(request, bitmap)));
        });
        let profile = winsys::files::known(Known::Profile).map(|path| path.display().to_string().to_lowercase()).unwrap_or_default();
        let contents = Contents::start(paths.index());
        let scope = RefCell::new(Scope::with_apps(settings.names_within(), !settings.names_leave_out_apps));
        let app = Rc::new(App {
            ui: ui.as_weak(),
            settings: RefCell::new(settings),
            paths,
            everything,
            icons,
            images: RefCell::new(HashMap::new()),
            row_keys: RefCell::new(Vec::new()),
            hot: RefCell::new(Arc::new(HotIndex::empty())),
            hot_building: Cell::new(false),
            history: RefCell::new(history),
            facts: RefCell::new(HashMap::new()),
            facts_asked: RefCell::new(HashSet::new()),
            state: RefCell::new(State {
                generation: 0,
                terms: Terms::default(),
                filter: Filter::All,
                mode: Mode::Ranked,
                image_text: false,
                image_shown: false,
                media: None,
                by_typing: None,
                refused: false,
                route: Route::Name,
                wants: None,
                first: None,
                core: None,
                relaxed: None,
                relaxed_found: Vec::new(),
                fuzzy: false,
                extra: crate::functions::Extra::default(),
                extra_content: None,
                typed: String::new(),
                only: None,
                filter_by_typing: false,
                loose: Vec::new(),
                closest: Vec::new(),
                seen_images: None,
                showing_closest: false,
                showing_loose: false,
                seen: Vec::new(),
                seeing: false,
                seen_at: HashMap::new(),
                heard_at: HashMap::new(),
                found: Vec::new(),
                found_stale: false,
                total: None,
                fast_done: true,
                main_done: true,
                refine_done: true,
                holding: false,
                plan: None,
                content: Vec::new(),
                snippets: HashMap::new(),
                results: Vec::new(),
                moved: false,
                visible: (0, 14),
                timing: Timing::default(),
                notice: None,
                everything: everything::Status::default(),
                health: everything::Health::Unknown,
                fixing: None,
            }),
            rows: Rc::new(VecModel::default()),
            all: RefCell::new(None),
            hwnd: Cell::new(0),
            scale: Cell::new(1.0),
            visible: Cell::new(false),
            hidden_at: Cell::new(None),
            show_started: Cell::new(None),
            frames: Cell::new(0),
            cloaked: Cell::new(false),
            uncloak_timer: slint::Timer::default(),
            parked: Cell::new(false),
            before: Cell::new(0),
            park_frames: Cell::new(u64::MAX),
            warming: Cell::new(false),
            show_wanted: Cell::new(false),
            preview_timer: slint::Timer::default(),
            playing: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            media: RefCell::new(Media::default()),
            slow_timer: slint::Timer::default(),
            hold_timer: slint::Timer::default(),
            seeing_timer: slint::Timer::default(),
            seeker: crate::seeking::Seeker::new(),
            notice_timer: slint::Timer::default(),
            profile,
            contents,
            indexer: crate::indexer::Indexer::new(),
            preview_marks: RefCell::new(None),
            revealed: RefCell::new(None),
            verifying: Cell::new(false),
            reading_timer: slint::Timer::default(),
            opened_size: Cell::new((0, 0)),
            wheel_at: Cell::new(None),
            enlarged_request: Cell::new(0),
            enlarged_asked: RefCell::new(None),
            enlarged_timer: slint::Timer::default(),
            scope,
        });
        ui.set_rows(ModelRc::from(app.rows.clone()));
        app.show_filters();
        app.preview_size_changed();
        app.show_toggles();
        app
    }

    fn context(&self) -> Context {
        Context { now: now(), profile: self.profile.clone() }
    }

    /// The settings say something new about where names are searched.
    pub fn scope_changed(&self) {
        {
            let settings = self.settings.borrow();
            *self.scope.borrow_mut() = Scope::with_apps(settings.names_within(), !settings.names_leave_out_apps);
        }
        self.show_filters();
        self.search_again();
    }

    /// The chips, in order. With apps left out, there's no chip for them.
    fn filters(&self) -> Vec<Filter> {
        let no_apps = self.scope.borrow().leaves_out_apps();
        Filter::ALL.into_iter().filter(|filter| !(no_apps && *filter == Filter::Apps)).collect()
    }

    fn show_filters(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let filters = self.filters();
        ui.set_filters(ModelRc::new(VecModel::from(filters.iter().map(|filter| SharedString::from(filter.label())).collect::<Vec<_>>())));
        let mut state = self.state.borrow_mut();
        if !filters.contains(&state.filter) {
            state.filter = Filter::All;
        }
        ui.set_filter(filters.iter().position(|filter| *filter == state.filter).unwrap_or(0) as i32);
    }

    // ---- The hot tier -------------------------------------------------------------------------------------------

    /// Rebuilds the hot tier on a thread (COM for shortcuts there), unless one is being built.
    pub fn refresh_hot(&self) {
        if self.hot_building.replace(true) {
            return;
        }
        let settings = self.settings.borrow().clone();
        let history: Vec<(String, u32, i64)> = self.history.borrow().items().map(|item| (item.path.clone(), item.count, item.last)).collect();
        std::thread::Builder::new()
            .name("hot".into())
            .spawn(move || {
                winsys::com_sta();
                let sources = hot_sources(&settings, history);
                let index = HotIndex::build(&sources);
                log(&format!("hot tier: {} entries in {} ms", index.len(), index.build_time.as_millis()));
                let index = Arc::new(index);
                let _ = slint::invoke_from_event_loop(move || with_app(|app| app.hot_ready(index)));
            })
            .expect("can't start the hot tier's thread");
    }

    fn hot_ready(&self, index: Arc<HotIndex>) {
        *self.hot.borrow_mut() = index;
        self.hot_building.set(false);
        if self.visible.get() && self.state.borrow().mode == Mode::Ranked {
            self.present();
        }
    }

    pub fn hot_age(&self) -> Duration {
        self.hot.borrow().built.elapsed()
    }

    // ---- Typing -------------------------------------------------------------------------------------------------

    pub fn edited(&self, text: &str) {
        let terms = self.typed_terms(text);
        let generation = {
            let mut state = self.state.borrow_mut();
            // Typing on from the previous text: its Everything results that still match stay until the new ones come.
            let extends = !terms.only_everything() && !state.terms.only_everything() && !state.terms.is_empty()
                && terms.text.to_lowercase().starts_with(&state.terms.text.to_lowercase());
            if !extends {
                // Only Everything can say what matches now, in a moment: what's there stays until it does.
                if terms.only_everything() && state.terms.only_everything() && terms.text.chars().count() >= 2 {
                    state.found_stale = true;
                } else {
                    state.found.clear();
                    state.found_stale = false;
                }
                state.content.clear();
                state.snippets.clear();
                state.heard_at.clear();
            }
            // What fits by what it shows changes with every word: what's there stays only while images alone are shown.
            if !state.image_shown {
                state.seen.clear();
            }
            state.terms = terms;
            state.closest.clear();
            state.loose.clear();
            state.relaxed_found.clear();
            state.generation += 1;
            state.total = None;
            state.moved = false;
            state.timing = Timing { typed: Some(Instant::now()), ..Timing::default() };
            if let Mode::All(_) = state.mode {
                state.mode = Mode::Ranked;
            }
            state.generation
        };
        self.icons.clear();
        self.leave_all();
        // S1: with Excavate's own index answering (3 to 5 ms), the quick tier's results wait for it, up to HOLD: the
        // list changes once per keystroke instead of twice, and the rows don't jump. Everything (a round trip of
        // ~100 ms) isn't waited for.
        let hold = everything::is_local() && !self.state.borrow().terms.is_empty();
        self.state.borrow_mut().holding = hold;
        if hold {
            self.hold_timer.start(slint::TimerMode::SingleShot, HOLD, || with_app(|app| app.release_hold()));
        } else {
            self.present();
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_selected(0);
            ui.invoke_scroll_to_top();
        }
        self.ask_everything(generation);
        self.ask_contents(generation);
        self.ask_seeing(generation);
        // (Nothing to wait for: no plan was asked.)
        if hold && self.state.borrow().main_done && self.state.borrow().fast_done {
            self.release_hold();
        }
    }

    /// The held results are shown: the own index answered, or HOLD passed.
    fn release_hold(&self) {
        self.hold_timer.stop();
        if std::mem::take(&mut self.state.borrow_mut().holding) {
            self.present();
        }
    }

    /// Back to nothing typed, the All chip and Best match. The toggles for how names match stay.
    pub fn new_search(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        self.state.borrow_mut().filter = Filter::All;
        ui.set_filter(0);
        ui.set_query(SharedString::default());
        self.edited("");
    }

    pub fn filter_chosen(&self, index: usize) {
        let filter = self.filters().get(index).copied().unwrap_or_default();
        {
            let mut state = self.state.borrow_mut();
            if state.filter == filter {
                return;
            }
            state.filter = filter;
            state.filter_by_typing = false;
            if state.media.is_some_and(|media| MEDIA_TOGGLES[media].0 != filter) {
                state.media = None;
            }
            // Only words read from images, among files that aren't images, is nothing at all.
            if !holds_images(filter) {
                state.image_text = false;
                state.image_shown = false;
            }
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_filter(index as i32);
            ui.set_image_text(self.state.borrow().image_text);
            ui.set_image_shown(self.state.borrow().image_shown);
        }
        self.show_media();
        self.search_again();
    }

    /// The toggles for how names match, as they're left.
    fn matching(&self) -> Matching {
        let settings = self.settings.borrow();
        Matching { case: settings.match_case, whole_word: settings.match_whole_word, path: settings.match_path, regex: settings.match_regex }
    }

    pub fn show_toggles(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        // Turned off in Settings: its toggle goes, and with it what it was doing.
        if !self.settings.borrow().seeing_on && self.state.borrow().image_shown {
            self.state.borrow_mut().image_shown = false;
            self.search_again();
        }
        let matching = self.matching();
        ui.set_match_case(matching.case);
        ui.set_match_whole_word(matching.whole_word);
        ui.set_match_path(matching.path);
        ui.set_match_regex(matching.regex);
        ui.set_image_text(self.state.borrow().image_text);
        ui.set_image_shown(self.state.borrow().image_shown);
        ui.set_seeing_on(self.settings.borrow().seeing_on);
        // Turned off in Settings: likewise.
        let media = self.state.borrow().media;
        if let Some(which) = media {
            let offered = {
                let settings = self.settings.borrow();
                match which {
                    0 => settings.videos_on && settings.video_text,
                    1 => settings.videos_on && settings.seeing_on,
                    _ => settings.listening_on,
                }
            };
            if !offered {
                self.set_media(which);
            }
        }
        self.show_media();
    }

    /// A toggle in the search field, in the order they're shown.
    pub fn toggle_chosen(&self, index: i32) {
        // A toggle the typing turned on, turned off by a click: not again for this search.
        {
            let mut state = self.state.borrow_mut();
            if let Some(typed) = state.by_typing {
                let its = if typed == Typed::Text { 4 } else { 5 };
                if index == its {
                    state.by_typing = None;
                    state.refused = true;
                } else if index == 4 || index == 5 {
                    state.by_typing = None;
                }
            }
        }
        if index == 4 {
            let on = !self.state.borrow().image_text;
            self.set_image_text(on);
            return;
        }
        if index == 5 {
            let on = !self.state.borrow().image_shown;
            self.set_image_shown(on);
            return;
        }
        if (6..9).contains(&index) {
            self.set_media(index as usize - 6);
            return;
        }
        {
            let mut settings = self.settings.borrow_mut();
            let toggle = match index {
                0 => &mut settings.match_case,
                1 => &mut settings.match_whole_word,
                2 => &mut settings.match_path,
                _ => &mut settings.match_regex,
            };
            *toggle = !*toggle;
            settings.save(&self.paths);
        }
        self.show_toggles();
        self.search_again();
    }

    /// A toggle for videos or recordings (`MEDIA_TOGGLES`): on, its kind's chip is chosen and only what it says is
    /// searched (the image toggles go off); clicked again, off, back to everything.
    fn set_media(&self, which: usize) {
        let Some(ui) = self.ui.upgrade() else { return };
        let on = self.state.borrow().media != Some(which);
        {
            let mut state = self.state.borrow_mut();
            state.media = on.then_some(which);
            state.seen.clear();
            state.filter = if on { MEDIA_TOGGLES[which].0 } else { Filter::All };
            state.filter_by_typing = false;
            if on {
                (state.image_text, state.image_shown) = (false, false);
                state.mode = Mode::Ranked;
            }
        }
        if on {
            self.leave_all();
        }
        let filter = self.state.borrow().filter;
        ui.set_filter(self.filters().iter().position(|chip| *chip == filter).unwrap_or(0) as i32);
        self.show_media();
        ui.set_image_text(self.state.borrow().image_text);
        ui.set_image_shown(self.state.borrow().image_shown);
        self.search_again();
    }

    /// Which of the toggles for videos and recordings is on, and which are offered (by what's read).
    fn show_media(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let media = self.state.borrow().media;
        ui.set_media_on(media.map_or(-1, |media| media as i32));
        let settings = self.settings.borrow();
        ui.set_video_text_offered(settings.videos_on && settings.video_text);
        ui.set_video_shown_offered(settings.videos_on && settings.seeing_on);
        ui.set_said_offered(settings.listening_on);
    }

    fn set_image_text(&self, on: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        {
            let mut state = self.state.borrow_mut();
            state.image_text = on;
            if on {
                state.image_shown = false;
                state.media = None;
                if !holds_images(state.filter) {
                    state.filter = Filter::All;
                    ui.set_filter(0);
                }
                // A list of everything in some order is Everything's: names.
                state.mode = Mode::Ranked;
            }
        }
        if on {
            self.leave_all();
        }
        ui.set_image_text(on);
        ui.set_image_shown(self.state.borrow().image_shown);
        self.show_media();
        self.search_again();
    }

    /// Only images, by what they show (and not the words read from them): the other image toggle goes off.
    fn set_image_shown(&self, on: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        {
            let mut state = self.state.borrow_mut();
            state.image_shown = on;
            state.seen.clear();
            if on {
                state.image_text = false;
                state.media = None;
                if !holds_images(state.filter) {
                    state.filter = Filter::All;
                    ui.set_filter(0);
                }
                state.mode = Mode::Ranked;
            }
        }
        if on {
            self.leave_all();
        }
        ui.set_image_shown(on);
        ui.set_image_text(self.state.borrow().image_text);
        self.show_media();
        self.search_again();
    }

    /// Back to names too, when the popup closes.
    pub fn image_text_off(&self) {
        if self.state.borrow().image_text {
            self.set_image_text(false);
        }
        if self.state.borrow().image_shown {
            self.set_image_shown(false);
        }
        let media = self.state.borrow().media;
        if let Some(media) = media {
            self.set_media(media);
        }
    }

    /// The same text under other rules: a chip or a toggle changed.
    /// What was typed, as terms, with the image toggles the typing turns on: with the experimental setting, "> " in front
    /// searches the text in images and "< " what they show; and a description of an image ("that photo with a cake in
    /// it") searches images by what they show. Undone when the typing no longer asks for it.
    fn typed_terms(&self, text: &str) -> Terms {
        // Functions answered here, out of the text; `content:` alone searches by its words.
        let (rest, extra) = crate::functions::split(text);
        let rest = if rest.trim().is_empty() && !extra.content.is_empty() { extra.content.join(" ") } else { rest };
        let changed = self.state.borrow().extra != extra;
        {
            let mut state = self.state.borrow_mut();
            if changed {
                state.extra_content = None;
            }
            state.extra = extra;
            state.typed = text.to_string();
        }
        let text = rest.as_str();
        let prefixed = if self.settings.borrow().typed_toggles { asked::prefixed(text) } else { None };
        // A kind other than images after it (">v theory of computation"): that kind only, its chip chosen.
        let only = prefixed.and_then(|prefixed| prefixed.kind.filter(|(filter, _)| *filter != Filter::Pictures)
            .map(|(filter, extension)| (filter, Only { inside: prefixed.inside, extension, part: prefixed.part })));
        // What this search does, under the field.
        if let Some(ui) = self.ui.upgrade() {
            let help = prefixed.map(|prefixed| prefixed.help()).unwrap_or_default();
            ui.set_prefix_help_shown(!help.is_empty());
            ui.set_prefix_help(slint::StyledText::from_markdown(&help).unwrap_or_else(|_| slint::StyledText::from_plain_text(&help)));
        }
        let (wanted, terms) = match prefixed {
            Some(prefixed) if only.is_some() => (None, Terms::matching(prefixed.rest, self.matching())),
            Some(prefixed) => (Some(if prefixed.inside { Typed::Text } else { Typed::Shown }), Terms::matching(prefixed.rest, self.matching())),
            None => {
                let terms = Terms::matching(text, self.matching());
                let route = describe::route(&terms);
                let state = self.state.borrow();
                let settings = self.settings.borrow();
                // Not over a toggle clicked on, nor after a click turned this off.
                let clicked = (state.image_text || state.image_shown) && state.by_typing.is_none();
                let image = route == Route::Image && settings.seeing_on && settings.setup_done && !state.refused && !clicked
                    && state.mode == Mode::Ranked && holds_images(state.filter);
                (image.then_some(Typed::Described), terms)
            }
        };
        let mut state = self.state.borrow_mut();
        state.route = describe::route(&terms);
        state.wants = describe::wants(&terms);
        state.first = asked::named(&terms).or(state.wants.map(asked::First::Kind));
        state.core = state.first.and_then(|_| asked::core(&terms));
        state.relaxed = asked::relaxed(&terms);
        state.only = only.map(|(_, only)| only).or(state.media.map(|media| MEDIA_TOGGLES[media].1));
        let chip = match only {
            Some((filter, _)) if state.filter != filter => {
                state.filter = filter;
                state.filter_by_typing = true;
                Some(filter)
            }
            None if state.filter_by_typing => {
                state.filter = Filter::All;
                state.filter_by_typing = false;
                Some(Filter::All)
            }
            _ => None,
        };
        if let (Some(filter), Some(ui)) = (chip, self.ui.upgrade()) {
            ui.set_filter(self.filters().iter().position(|chip| *chip == filter).unwrap_or(0) as i32);
        }
        if terms.is_empty() {
            state.refused = false;
        }
        if wanted != state.by_typing {
            match state.by_typing {
                Some(Typed::Text) => state.image_text = false,
                Some(Typed::Shown | Typed::Described) => state.image_shown = false,
                None => {}
            }
            match wanted {
                Some(Typed::Text) => (state.image_text, state.image_shown) = (true, false),
                Some(Typed::Shown | Typed::Described) => (state.image_text, state.image_shown) = (false, true),
                None => {}
            }
            if wanted.is_some() && !holds_images(state.filter) {
                state.filter = Filter::All;
                if let Some(ui) = self.ui.upgrade() {
                    ui.set_filter(0);
                }
            }
            state.by_typing = wanted;
            if let Some(ui) = self.ui.upgrade() {
                ui.set_image_text(state.image_text);
                ui.set_image_shown(state.image_shown);
            }
        }
        terms
    }

    fn search_again(&self) {
        let text = self.ui.upgrade().map(|ui| ui.get_query().to_string()).unwrap_or_else(|| self.query_text());
        let terms = self.typed_terms(&text);
        let generation = {
            let mut state = self.state.borrow_mut();
            state.terms = terms;
            state.found.clear();
            state.found_stale = false;
            state.content.clear();
            state.snippets.clear();
            state.heard_at.clear();
            state.seen.clear();
            state.closest.clear();
            state.loose.clear();
            state.relaxed_found.clear();
            state.generation += 1;
            state.total = None;
            state.moved = false;
            state.timing = Timing { typed: Some(Instant::now()), ..Timing::default() };
            state.generation
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.set_selected(0);
            ui.invoke_scroll_to_top();
        }
        let mode = self.state.borrow().mode;
        match mode {
            Mode::Ranked => {
                self.present();
                self.ask_everything(generation);
                self.ask_contents(generation);
                self.ask_extra_content(generation);
                self.ask_seeing(generation);
            }
            Mode::All(sort) => self.enter_all(sort),
        }
    }

    fn ask_everything(&self, generation: u64) {
        self.slow_timer.stop();
        let (terms, filter, count, image_text) = {
            let state = self.state.borrow();
            (state.terms.clone(), state.filter, self.settings.borrow().ranked_results, state.image_text || state.image_shown || state.only.is_some())
        };
        // "Fuzzy match": one search, of names that fit loosely (Everything's syntax and regular expressions as usual).
        if self.fuzzy_now() {
            {
                let mut state = self.state.borrow_mut();
                state.fast_done = true;
                state.main_done = false;
                state.refine_done = true;
                state.plan = None;
            }
            let query = everything::Query { fuzzy: true, ..everything::Query::new(terms.text.clone(), everything::Sort::NameAscending, count.max(300)) };
            self.send(generation, Part::Main, query);
            return;
        }
        // Functions answered here: more names, as each is checked; typed alone, the files of their kind, newest first.
        let extra = self.state.borrow().extra.clone();
        if terms.is_empty() {
            if let Some(kinds) = extra.kinds_alone() {
                {
                    let mut state = self.state.borrow_mut();
                    state.fast_done = true;
                    state.main_done = false;
                    state.refine_done = true;
                    state.plan = None;
                }
                self.send(generation, Part::Main, everything::Query::new(kinds, everything::Sort::ModifiedDescending, EXTRA_POOL as u32));
                return;
            }
        }
        let count = if extra.is_empty() { count } else { count.max(EXTRA_POOL as u32) };
        let plan = if image_text { None } else { plan::ranked(&terms, filter, &self.scope.borrow(), count) };
        let Some(plan) = plan else {
            let mut state = self.state.borrow_mut();
            state.fast_done = true;
            state.main_done = true;
            state.refine_done = true;
            state.plan = None;
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            state.fast_done = plan.fast.is_none();
            state.main_done = false;
            state.refine_done = true;
            state.plan = Some(plan.clone());
        }
        if let Some(fast) = plan.fast {
            self.send(generation, Part::Fast, fast);
        }
        // A type named ("the pdf of my passport"): its files by the rest of the words too.
        let typed = {
            let state = self.state.borrow();
            state.first.zip(state.core.as_ref())
                .and_then(|(first, core)| plan::typed(core, &first.extensions(), filter, &self.scope.borrow(), count))
        };
        if let Some(typed) = typed {
            self.send(generation, Part::Typed, typed);
        }
        // "my passport": names with "passport" too, after those with both.
        let relaxed = self.state.borrow().relaxed.as_ref().and_then(|relaxed| plan::relaxed(relaxed, filter, &self.scope.borrow(), count));
        if let Some(relaxed) = relaxed {
            self.send(generation, Part::Relaxed, relaxed);
        }
        // Over all the names this takes most of a second and can't be stopped, so a letter typed meanwhile would wait for it: asked
        // when the typing rests. Within some folders or a kind it's quick.
        if terms.matching.only_everything() && filter == Filter::All && self.scope.borrow().everywhere() {
            let main = plan.main;
            self.slow_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(SLOW_WAIT), move || {
                let main = main.clone();
                with_app(|app| app.send(generation, Part::Main, main));
            });
        } else {
            self.send(generation, Part::Main, plan.main);
        }
    }

    /// Whether this search is a fuzzy one: chosen, and something to match loosely (not Everything's syntax, nor a
    /// regular expression, nor `>`/`<`).
    fn fuzzy_now(&self) -> bool {
        let state = self.state.borrow();
        state.fuzzy && !state.terms.is_empty() && !state.terms.syntax && !state.terms.matching.regex && state.only.is_none()
            && !state.image_text && !state.image_shown
    }

    /// `content:`: which files have its words inside (the content index), once per search.
    fn ask_extra_content(&self, generation: u64) {
        let words = {
            let state = self.state.borrow();
            if state.extra.content.is_empty() || state.extra_content.is_some() {
                return;
            }
            state.extra.content.clone()
        };
        let query = content::Query { words, prefix: true, inside_only: true, ..content::Query::default() };
        self.contents.search(generation, query, EXTRA_CONTENT, move |generation, hits, _| {
            let paths: HashSet<String> = hits.into_iter().map(|hit| hit.path.to_lowercase()).collect();
            let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                {
                    let mut state = app.state.borrow_mut();
                    if state.generation != generation {
                        return;
                    }
                    state.extra_content = Some(paths);
                }
                app.present();
            }));
        });
    }

    /// Sizes and lengths read from files' headers, on a few threads; the list shown again once they're in.
    fn ask_facts(&self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        self.facts_asked.borrow_mut().extend(paths.iter().map(|path| path.to_lowercase()));
        let threads = 4.min(paths.len());
        let chunk = paths.len().div_ceil(threads);
        let paths: Vec<Vec<String>> = paths.chunks(chunk).map(<[String]>::to_vec).collect();
        for part in paths {
            std::thread::spawn(move || {
                winsys::com_mta();
                let read: Vec<(String, FileFacts)> = part.into_iter().map(|path| {
                    let facts = if is_video(&path) {
                        match winsys::video::length_and_size(&path) {
                            Ok((length, size)) => FileFacts { size, length: Some(length) },
                            Err(_) => FileFacts::default(),
                        }
                    } else if is_picture(&path) {
                        FileFacts { size: winsys::imaging::size(&path), length: None }
                    } else {
                        FileFacts { size: None, length: winsys::video::length(&path).ok() }
                    };
                    (path.to_lowercase(), facts)
                }).collect();
                let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                    {
                        let mut facts = app.facts.borrow_mut();
                        let mut asked = app.facts_asked.borrow_mut();
                        for (path, read) in read {
                            asked.remove(&path);
                            facts.insert(path, read);
                        }
                    }
                    app.present();
                }));
            });
        }
    }

    /// The results that fit the functions answered here (`functions::Extra`), up to `limit`; the facts not known yet
    /// are asked for (those results show once they're in).
    fn with_extra(&self, results: Vec<Ranked>, extra: &crate::functions::Extra, content: Option<&HashSet<String>>, limit: usize) -> Vec<Ranked> {
        if extra.is_empty() {
            return results;
        }
        let wants_facts = extra.needs_size() || extra.needs_length();
        let mut ask: Vec<String> = Vec::new();
        let kept: Vec<Ranked> = {
            let facts = self.facts.borrow();
            let asked = self.facts_asked.borrow();
            let history = self.history.borrow();
            results.into_iter().filter(|ranked| {
                let candidate = &ranked.candidate;
                if candidate.is_folder {
                    return false;
                }
                let path = candidate.path();
                let key = path.to_lowercase();
                if !extra.content.is_empty() && !content.is_some_and(|content| content.contains(&key)) {
                    return false;
                }
                let known = if wants_facts {
                    // Only images, videos and recordings have a size or a length.
                    let media = is_picture(&candidate.name) || is_video(&candidate.name) || is_recording(&candidate.name);
                    if !media {
                        return false;
                    }
                    match facts.get(&key) {
                        Some(known) => Some(known),
                        None => {
                            if !asked.contains(&key) && ask.len() < EXTRA_POOL {
                                ask.push(path.clone());
                            }
                            return false;
                        }
                    }
                } else {
                    None
                };
                let runs = history.get(&path).map(|opened| (opened.count, opened.last));
                extra.fits(known.and_then(|known| known.size), known.and_then(|known| known.length), runs)
            }).take(limit).collect()
        };
        self.ask_facts(ask);
        kept
    }

    /// Asks the content index for files whose words match (documents, code, text in pictures).
    fn ask_contents(&self, generation: u64) {
        if self.fuzzy_now() {
            return;
        }
        let (terms, filter, image_text, only) = {
            let state = self.state.borrow();
            (state.terms.clone(), state.filter, state.image_text, state.only)
        };
        // Scanning all the stored text for a regular expression would be too slow. ("<v", what videos show: not words.)
        if terms.matching.regex || self.state.borrow().image_shown || only.is_some_and(|only| !only.inside) {
            return;
        }
        let kinds: Vec<&'static str> = match filter {
            _ if image_text => vec![content::kind::PICTURE],
            Filter::All => Vec::new(),
            Filter::Documents => content::kind::DOCUMENTS.to_vec(),
            Filter::Pictures => vec![content::kind::PICTURE],
            Filter::Code => vec![content::kind::CODE],
            // What's said in recordings; the text shown and what's said in videos.
            Filter::Audio => vec![content::kind::AUDIO],
            Filter::Video => vec![content::kind::VIDEO],
            // Apps, folders and archives have no words inside.
            _ => return,
        };
        let letters: usize = terms.words.iter().map(|word| word.chars().count()).sum();
        if terms.syntax || letters < 2 {
            return;
        }
        // Without the joining words (no image's text says "my" of "my passport"), nor the type named ("pdf that has
        // my resume": a resume's text rarely says "pdf"; its PDFs come first all the same, in `present`).
        let words = self.state.borrow().contents_terms().map_or_else(|| terms.words.clone(), |words| words.words.clone());
        let query = content::Query {
            words,
            prefix: !terms.matching.whole_word,
            kinds,
            case: terms.matching.case,
            whole_word: terms.matching.whole_word,
            inside_only: image_text || only.is_some(),
        };
        self.contents.search(generation, query, 60, move |generation, hits, took| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.contents_found(generation, hits, took)));
        });
    }

    fn contents_found(&self, generation: u64, hits: Vec<content::Hit>, took: Duration) {
        {
            let mut state = self.state.borrow_mut();
            if generation != state.generation || state.mode != Mode::Ranked {
                return;
            }
            let words = state.contents_terms().unwrap_or(&state.terms).words.len();
            state.content.clear();
            state.snippets.clear();
            state.heard_at.clear();
            // What a folder leaves out isn't found there, even what was read before it was left out.
            let kinds = self.settings.borrow().kinds();
            let part = state.only.and_then(|only| only.part);
            for hit in hits {
                // `>vt`: what videos show, not what's said in them; `>vs` the other way round.
                let video_part = match hit.source {
                    content::Source::Video => Some(asked::Part::Shown),
                    content::Source::Speech if hit.kind == content::kind::VIDEO => Some(asked::Part::Said),
                    _ => None,
                };
                if part.is_some() && video_part != part {
                    continue;
                }
                let kind = match hit.source {
                    content::Source::Picture => Some("image-text"),
                    content::Source::Speech if hit.kind == content::kind::VIDEO => Some("video-said"),
                    content::Source::Speech => Some("audio"),
                    content::Source::Video => Some("video-text"),
                    content::Source::Text | content::Source::Mixed => None,
                };
                if kind.is_some_and(|kind| !kinds.on(&hit.path, kind)) {
                    continue;
                }
                let (folder, name) = match hit.path.rsplit_once('\\') {
                    Some((folder, name)) => (folder.to_string(), name.to_string()),
                    None => (String::new(), hit.path.clone()),
                };
                let candidate = Candidate {
                    name,
                    folder,
                    is_folder: false,
                    size: Some(hit.size),
                    modified: Some(hit.modified),
                    origin: Some(Origin::Content),
                    content: (!hit.in_name_only).then(|| rank::content_strength(hit.score, words)),
                    ..Candidate::default()
                };
                if candidate.content.is_some() {
                    let mut snippet = hit.snippet;
                    // A recording or a video: when it's said or shown, first, and it opens there.
                    if let Some(at) = hit.moment {
                        let said = format!("At {}  ·  ", format::moment(at));
                        snippet.highlights = snippet.highlights.iter().map(|range| (range.start + said.len())..(range.end + said.len())).collect();
                        snippet.text = format!("{said}{}", snippet.text);
                        state.heard_at.insert(candidate.key(), at);
                    }
                    state.snippets.insert(candidate.key(), (snippet, hit.source));
                }
                state.content.push(candidate);
            }
            state.timing.contents = Some(took);
        }
        self.present();
    }

    /// Images by what they show, from the search helper once the typing rests: on their own with the toggle on, and
    /// otherwise the few that fit far better than the rest join the ranking.
    fn ask_seeing(&self, generation: u64) {
        self.seeing_timer.stop();
        if self.fuzzy_now() {
            self.state.borrow_mut().seeing = false;
            return;
        }
        let (text, only) = {
            let state = self.state.borrow();
            let settings = self.settings.borrow();
            let letters = state.terms.text.chars().filter(|c| c.is_alphanumeric()).count();
            // (">v", ">pdf": the words inside only; what things show or mean is "<".)
            let wanted = settings.seeing_on && settings.setup_done && state.mode == Mode::Ranked && !state.terms.syntax
                && !state.terms.matching.regex && !state.image_text && letters >= 3 && !state.only.is_some_and(|only| only.inside)
                && (state.image_shown || matches!(state.filter, Filter::All | Filter::Pictures | Filter::Video | Filter::Documents | Filter::Code | Filter::Audio));
            if !wanted {
                drop(state);
                self.state.borrow_mut().seeing = false;
                return;
            }
            (state.terms.text.clone(), state.image_shown || state.only.is_some_and(|only| !only.inside))
        };
        self.state.borrow_mut().seeing = true;
        let wait = if only { SEEING_WAIT_ALONE } else { SEEING_WAIT };
        self.seeing_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(wait), move || {
            let text = text.clone();
            with_app(|app| app.send_seeing(generation, text));
        });
    }

    fn send_seeing(&self, generation: u64, text: String) {
        let asked = text.clone();
        let wanted = {
            let state = self.state.borrow();
            // "<v": what videos show, not what they mean; "<a", "<pdf": what they mean.
            let shown = state.only.is_some_and(|only| !only.inside);
            crate::seeking::Wanted {
                images: state.image_shown || matches!(state.filter, Filter::All | Filter::Pictures | Filter::Video),
                documents: !state.image_shown && matches!(state.filter, Filter::All | Filter::Documents | Filter::Code | Filter::Audio | Filter::Video)
                    && !(shown && state.filter == Filter::Video),
                // Of the chip's kind only (and the typed extension), so its best aren't crowded out by others.
                extensions: match state.only.and_then(|only| only.extension) {
                    Some(extension) => vec![extension],
                    None if state.image_shown => Vec::new(),
                    None => state.filter.extensions().to_vec(),
                },
            }
        };
        self.seeker.ask(&self.paths, text, SEEN_SHOWN, wanted, move |answer| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.seen_found(generation, asked, answer)));
        });
    }

    fn seen_found(&self, generation: u64, text: String, answer: Result<crate::seeking::Answer, String>) {
        let again = {
            let mut state = self.state.borrow_mut();
            if generation != state.generation || state.mode != Mode::Ranked {
                return;
            }
            match answer {
                Ok(mut answer) => {
                    // The kind the description asks for, by the helper's classifier when it's sure:
                    // it replaces the rules' guess, "none" too. A name needs it surer.
                    if let Some((kind, sure)) = &answer.kind {
                        let needed = if describe::describes(&state.terms) { KIND_SURE } else { KIND_SURE_NAME };
                        if *sure >= needed {
                            state.wants = match kind.as_str() {
                                "image" => Some(describe::Kind::Image),
                                "video" => Some(describe::Kind::Video),
                                "audio" => Some(describe::Kind::Audio),
                                "document" => Some(describe::Kind::Document),
                                _ => None,
                            };
                            // A type named in so many words stays first.
                            if asked::named(&state.terms).is_none() {
                                state.first = state.wants.map(asked::First::Kind);
                                state.core = state.first.and_then(|_| asked::core(&state.terms));
                            }
                        }
                    }
                    // Videos come with their moment.
                    let kinds = self.settings.borrow().kinds();
                    answer.hits.retain(|hit| kinds.on(&hit.path, if hit.at.is_some() { "video-shown" } else { "image-shown" }));
                    let only = state.image_shown || state.only.is_some_and(|only| !only.inside);
                    // A description: what fits joins more readily than for a name.
                    let line = if state.route == Route::Name { answer.line() } else { answer.line().min(DESCRIBED_LINE) };
                    state.seen_at = answer.hits.iter().filter_map(|hit| Some((hit.path.to_lowercase(), hit.at?))).collect();
                    let documents_line = if state.route == Route::Name { answer.documents_line() } else { answer.documents_line().min(DESCRIBED_LINE) };
                    // Documents by what they mean join as images do, when they stand out as much.
                    let joining: Vec<(&crate::seeking::Hit, f32)> = if only {
                        // On their own: images (or videos) and documents together, by how far each stands out.
                        let mut all: Vec<(&crate::seeking::Hit, f32)> = answer.hits.iter().map(|hit| (hit, line))
                            .chain(answer.documents.iter().map(|hit| (hit, documents_line)))
                            .collect();
                        all.sort_by(|a, b| (b.0.score - b.1).total_cmp(&(a.0.score - a.1)));
                        all.truncate(SEEN_SHOWN);
                        all
                    } else {
                        answer.hits.iter().filter(|hit| hit.score >= line).take(SEEN_JOINING).map(|hit| (hit, line))
                            .chain(answer.documents.iter().filter(|hit| hit.score >= documents_line).take(SEEN_JOINING).map(|hit| (hit, documents_line)))
                            .collect()
                    };
                    let candidate = |(hit, line): (&crate::seeking::Hit, f32)| {
                        let (folder, name) = hit.path.rsplit_once('\\').map(|(folder, name)| (folder.to_string(), name.to_string()))
                            .unwrap_or_else(|| (String::new(), hit.path.clone()));
                        Candidate {
                            name,
                            folder,
                            size: hit.size,
                            modified: hit.modified,
                            origin: Some(Origin::Content),
                            // On the content scale: just past the line, a weak match by contents; far past it, a strong one.
                            content: Some((0.45 + (hit.score - line) / 4.0).clamp(0.3, 1.0)),
                            ..Candidate::default()
                        }
                    };
                    state.seen = joining.into_iter().map(candidate).collect();
                    // Shown only if nothing else is found: the best of each, however little they stand out.
                    let mut closest: Vec<(&crate::seeking::Hit, f32)> = answer.hits.iter().take(CLOSEST).map(|hit| (hit, line))
                        .chain(answer.documents.iter().take(SEEN_JOINING).map(|hit| (hit, documents_line)))
                        .collect();
                    closest.sort_by(|a, b| (b.0.score - b.1).total_cmp(&(a.0.score - a.1)));
                    state.closest = closest.into_iter().map(candidate).collect();
                    state.seen_images = Some(answer.images);
                    state.seeing = answer.waiting;
                    state.timing.seeing_ms = Some(answer.ms);
                    answer.waiting
                }
                Err(error) => {
                    log(&format!("images by what they show: {error}"));
                    state.seeing = false;
                    false
                }
            }
        };
        // A model's text half was still loading: its view joins in a moment.
        if again {
            self.seeing_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(600), move || {
                let text = text.clone();
                with_app(|app| app.send_seeing(generation, text));
            });
        }
        // Taken for a description of an image, it found none (none looked at yet, say): names, then.
        let nothing = {
            let state = self.state.borrow();
            !again && state.by_typing == Some(Typed::Described) && state.seen.is_empty()
        };
        if nothing {
            {
                let mut state = self.state.borrow_mut();
                state.by_typing = None;
                state.refused = true;
                state.image_shown = false;
            }
            if let Some(ui) = self.ui.upgrade() {
                ui.set_image_shown(false);
            }
            self.search_again();
            return;
        }
        self.present();
    }

    fn send(&self, generation: u64, part: Part, query: everything::Query) {
        self.everything.search(generation, vec![query], move |result| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.found(generation, part, result)));
        });
    }

    fn found(&self, generation: u64, part: Part, result: Result<Vec<Page>, everything::Error>) {
        let mut refine = None;
        let mut loose = None;
        {
            let mut state = self.state.borrow_mut();
            if generation != state.generation || state.mode != Mode::Ranked {
                return;
            }
            let elapsed = state.timing.typed.map(|typed| typed.elapsed());
            match result {
                Ok(pages) => {
                    let page = pages.into_iter().next().unwrap_or_default();
                    // For measuring (EXCAVATE_LOG_SEARCHES): each answer's round trip, from typing to here.
                    if std::env::var_os("EXCAVATE_LOG_SEARCHES").is_some() {
                        log(&format!("search {part:?} {:?}: {} of {} in {:.1} ms (round trip), {:.1} ms since typed", state.terms.text,
                                     page.items.len(), page.total, page.elapsed.as_secs_f64() * 1000.0,
                                     elapsed.map_or(0.0, |e| e.as_secs_f64() * 1000.0)));
                    }
                    if std::mem::take(&mut state.found_stale) {
                        state.found.clear();
                    }
                    match part {
                        Part::Fast => {
                            state.fast_done = true;
                            state.timing.fast = elapsed;
                        }
                        Part::Main => {
                            // Leftovers from the previous keystroke stay: they match the new text too (ranking checks).
                            state.main_done = true;
                            state.total = Some(page.total);
                            state.timing.main = elapsed;
                            state.timing.everything_ms = Some(page.elapsed.as_secs_f64() * 1000.0);
                            if let Some(plan) = state.plan.as_ref().filter(|plan| plan.wants_refine(page.total)) {
                                refine = plan.refine.clone();
                                state.refine_done = false;
                            }
                        }
                        Part::Refine => state.refine_done = true,
                        Part::Typed => {}
                        Part::Loose => {
                            state.loose = page.items.iter().map(candidate).collect();
                            drop(state);
                            self.present();
                            return;
                        }
                        Part::Relaxed => {
                            state.relaxed_found = page.items.iter().map(candidate).collect();
                            drop(state);
                            self.present();
                            return;
                        }
                    }
                    // Nothing has all the words: names with any of them, as a last try.
                    if part == Part::Main && page.total == 0 && !state.fuzzy {
                        let (scope, count) = (self.scope.borrow(), self.settings.borrow().ranked_results);
                        let terms = state.relaxed.as_ref().unwrap_or(&state.terms);
                        loose = plan::loose(terms, state.filter, &scope, count).or_else(|| plan::fuzzy(terms, state.filter, &scope, count));
                    }
                    let mut seen: HashSet<String> = state.found.iter().map(Candidate::key).collect();
                    for item in &page.items {
                        let candidate = candidate(item);
                        if seen.insert(candidate.key()) {
                            state.found.push(candidate);
                        }
                    }
                    state.everything.available = true;
                }
                Err(everything::Error::Superseded) => return,
                Err(error) => {
                    if std::mem::take(&mut state.found_stale) {
                        state.found.clear();
                    }
                    state.fast_done = true;
                    state.main_done = true;
                    state.refine_done = true;
                    state.everything.available = false;
                    log(&format!("everything: {error}"));
                }
            }
        }
        if let Some(query) = refine {
            self.send(generation, Part::Refine, query);
        }
        if let Some(query) = loose {
            self.send(generation, Part::Loose, query);
        }
        let (holding, done) = {
            let state = self.state.borrow();
            (state.holding, state.fast_done && state.main_done)
        };
        if holding {
            if done {
                self.release_hold();
            }
            return;
        }
        self.present();
    }

    /// Ranks what both tiers know and shows it.
    fn present(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let context = self.context();
        let selected_path = self.selected_target().map(|target| target.path.to_lowercase());
        let (results, hot_time, showing_closest, showing_loose) = {
            let state = self.state.borrow();
            let started = Instant::now();
            let mut closest = false;
            let mut loosely = false;
            // Apps always show; other names only from where they're searched.
            let scope = self.scope.borrow();
            // The chip's kind, and the extension typed after `>` or `<` (">pdf lease").
            let extension = state.only.and_then(|only| only.extension);
            let accepts = |candidate: &&Candidate| state.filter.accepts(&candidate.name, candidate.is_folder)
                && extension.is_none_or(|extension| asked::First::Extension(extension).holds(&candidate.name));
            let names = !(state.image_text || state.image_shown || state.only.is_some()) || state.terms.is_empty();
            let mut hot = if names { self.hot.borrow().search(&state.terms, state.filter, &context, 80) } else { Vec::new() };
            // A type named: its files the hot tier knows by the rest of the words too.
            if let (true, Some(first), Some(core)) = (names, state.first, state.core.as_ref()) {
                hot.extend(self.hot.borrow().search(core, first.filter(), &context, 40).into_iter().filter(|ranked| first.holds(&ranked.candidate.name)));
            }
            hot.retain(|ranked| (ranked.candidate.origin == Some(Origin::App) && !scope.leaves_out_apps()) || scope.accepts(&ranked.candidate.path()));
            // "my passport": what the hot tier knows by "passport" too.
            let hot_relaxed: Vec<Candidate> = match (names, state.relaxed.as_ref()) {
                (true, Some(relaxed)) => self.hot.borrow().search(relaxed, state.filter, &context, 40).into_iter()
                    .filter(|ranked| (ranked.candidate.origin == Some(Origin::App) && !scope.leaves_out_apps()) || scope.accepts(&ranked.candidate.path()))
                    .map(|ranked| ranked.candidate).collect(),
                _ => Vec::new(),
            };
            let limit = if state.terms.is_empty() { 40 } else { self.settings.borrow().ranked_results as usize };
            let in_order = |candidates: Vec<&Candidate>| -> Vec<Ranked> {
                candidates.into_iter().enumerate()
                    .map(|(place, candidate)| Ranked { candidate: candidate.clone(), score: -(place as f32), highlights: Vec::new() })
                    .collect()
            };
            let fuzzy = state.fuzzy && !state.terms.syntax && !state.terms.matching.regex && state.only.is_none()
                && !state.image_text && !state.image_shown;
            let matcher = if fuzzy && !state.terms.is_empty() { engine::fuzzy::Matcher::new(&state.terms.text) } else { None };
            let results = if state.terms.is_empty() && !state.extra.is_empty() {
                // Functions alone ("rc:>5", "width:>3000"): what the app knows, and the files of their kind, newest
                // first.
                let hot_tier = self.hot.borrow();
                let mut kept: HashSet<String> = HashSet::new();
                let mut all: Vec<&Candidate> = hot_tier.each(state.filter).chain(state.found.iter().filter(accepts))
                    .filter(|candidate| kept.insert(candidate.key()))
                    .collect();
                all.sort_by(|a, b| b.modified.cmp(&a.modified));
                in_order(all)
            } else if state.terms.is_empty() {
                hot
            } else if let Some(matcher) = matcher.as_ref() {
                // "Fuzzy match": the apps and recent files the app knows, and the names searched, by how closely each
                // fits (the index's order among equals: the most recently changed first); then the usual ranking.
                let hot_tier = self.hot.borrow();
                let known = hot_tier.each(state.filter)
                    .filter(|candidate| (candidate.origin == Some(Origin::App) && !scope.leaves_out_apps()) || scope.accepts(&candidate.path()));
                let mut kept: HashSet<String> = HashSet::new();
                let mut fits: Vec<(f32, f32, Ranked)> = known.chain(state.found.iter().filter(accepts))
                    .filter(|candidate| kept.insert(candidate.key()))
                    .filter_map(|candidate| {
                        let fit = matcher.score_name(&candidate.name)?;
                        let usual = rank::score(candidate, 0.5, &context);
                        Some((fit, usual, Ranked { candidate: candidate.clone(), score: fit, highlights: matcher.highlights(candidate.name.as_bytes()) }))
                    })
                    .collect();
                fits.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.total_cmp(&a.1)));
                fits.truncate(limit);
                fits.into_iter().map(|(_, _, ranked)| ranked).collect()
            } else if state.image_shown || state.only.is_some_and(|only| !only.inside) {
                // Images alone (or "<v", "<pdf": that kind alone), in the helper's order.
                in_order(state.seen.iter().filter(accepts).collect())
            } else {
                // ">v": the words inside alone.
                let inside = state.only.is_some();
                let candidates: Vec<Candidate> = hot.into_iter().map(|ranked| ranked.candidate).filter(|candidate| accepts(&candidate))
                    .chain(state.found.iter().chain(&state.content).chain(state.seen.iter().filter(|_| !inside)).filter(accepts).cloned())
                    .collect();
                // A type named ("the pdf of my passport"), or the kind a description asks for: its files first, ranked
                // by the rest of the words; the others after them.
                let ranked = match state.first {
                    Some(first) => {
                        let (typed, rest): (Vec<Candidate>, Vec<Candidate>) = candidates.iter().cloned().partition(|candidate| first.holds(&candidate.name));
                        let mut ranked = rank::rank(typed, state.core.as_ref().unwrap_or(&state.terms), &context, limit);
                        let room = limit.saturating_sub(ranked.len());
                        ranked.extend(rank::rank(rest, &state.terms, &context, room));
                        ranked
                    }
                    None => rank::rank(candidates.iter().cloned(), &state.terms, &context, limit),
                };
                // "my passport": after the names with both words, those with "passport" (and what's inside, already
                // searched by it), up to the limit.
                let ranked = match state.relaxed.as_ref() {
                    Some(relaxed) if ranked.len() < limit => {
                        let mut ranked = ranked;
                        let had: HashSet<String> = ranked.iter().map(|ranked| ranked.candidate.key()).collect();
                        let pool = candidates.into_iter().chain(hot_relaxed.into_iter().chain(state.relaxed_found.iter().cloned()).filter(|candidate| accepts(&candidate)));
                        let room = limit - ranked.len();
                        ranked.extend(rank::rank(pool, relaxed, &context, limit).into_iter().filter(|more| !had.contains(&more.candidate.key())).take(room));
                        ranked
                    }
                    _ => ranked,
                };
                // Nothing by name, nothing inside: what fits best by what it shows or means, rather than nothing; else
                // names with any of the words, best by the word that fits best.
                let done = state.main_done && state.fast_done && !state.seeing;
                let closest_here: Vec<&Candidate> = state.closest.iter().filter(accepts).collect();
                if ranked.is_empty() && done && !state.image_text && !inside && !closest_here.is_empty() {
                    closest = true;
                    in_order(closest_here)
                } else if ranked.is_empty() && state.terms.words.len() == 1 && state.main_done && names && !state.terms.syntax {
                    // One word nothing has: names a letter or so off it, nearest first (the hot tier's too, apps
                    // among them, a little further off: there are few of them).
                    let word = &state.terms.words[0];
                    let letters = word.chars().count();
                    let mut near: Vec<(usize, Candidate)> = state.loose.iter().filter(accepts)
                        .map(|candidate| (rank::closeness(word, &candidate.name), candidate.clone()))
                        .filter(|(distance, _)| *distance <= (letters / 3).max(1))
                        .collect();
                    if letters >= 4 {
                        near.extend(self.hot.borrow().closest(word, state.filter, letters / 2).into_iter()
                            .filter(|(_, candidate)| accepts(&candidate))
                            .filter(|(_, candidate)| (candidate.origin == Some(Origin::App) && !scope.leaves_out_apps()) || scope.accepts(&candidate.path())));
                    }
                    let mut kept: HashSet<String> = HashSet::new();
                    let mut near: Vec<(usize, Ranked)> = near.into_iter()
                        .filter(|(_, candidate)| kept.insert(candidate.key()))
                        .map(|(distance, candidate)| {
                            let score = rank::score(&candidate, 0.5, &context);
                            (distance, Ranked { candidate, score, highlights: Vec::new() })
                        })
                        .collect();
                    near.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.score.total_cmp(&a.1.score)));
                    near.truncate(limit);
                    loosely = !near.is_empty();
                    near.into_iter().map(|(_, ranked)| ranked).collect()
                } else if ranked.is_empty() && !state.loose.is_empty() {
                    loosely = true;
                    let mut best: HashMap<String, Ranked> = HashMap::new();
                    for word in &state.terms.words {
                        let one = Terms::matching(word, state.terms.matching);
                        for ranked in rank::rank(state.loose.iter().filter(accepts).cloned(), &one, &context, limit) {
                            let key = ranked.candidate.key();
                            if best.get(&key).is_none_or(|kept| kept.score < ranked.score) {
                                best.insert(key, ranked);
                            }
                        }
                    }
                    let mut loose: Vec<Ranked> = best.into_values().collect();
                    loose.sort_by(|a, b| b.score.total_cmp(&a.score));
                    loose.truncate(limit);
                    loose
                } else {
                    ranked
                }
            };
            let limit = self.settings.borrow().ranked_results as usize;
            let results = self.with_extra(results, &state.extra, state.extra_content.as_ref(), limit);
            (results, started.elapsed(), closest, loosely)
        };
        let selected = {
            let mut state = self.state.borrow_mut();
            if state.timing.hot.is_none() {
                state.timing.hot = Some(hot_time);
            }
            state.showing_closest = showing_closest;
            state.showing_loose = showing_loose;
            let keep = if state.moved {
                selected_path.and_then(|path| results.iter().position(|ranked| ranked.candidate.key() == path))
            } else {
                None
            };
            state.results = results;
            keep.unwrap_or(0)
        };
        self.fill_rows();
        ui.set_selected(selected as i32);
        self.update_status();
        self.schedule_preview();
    }

    /// The color of matched letters: the theme's, lighter on dark and deeper on light so it reads on both.
    fn accent(&self) -> String {
        let dark = self.ui.upgrade().is_some_and(|ui| ui.global::<Theme>().get_dark());
        crate::themes::current(&self.settings.borrow()).match_color(dark)
    }

    fn fill_rows(&self) {
        let state = self.state.borrow();
        let size = self.icon_pixels();
        let now = now();
        let twelve = self.twelve_hour();
        let accent = &self.accent();
        let images = self.images.borrow();
        let mut keys = Vec::with_capacity(state.results.len());
        // Found by the helper: by what they show (images, videos) or what they mean (the rest).
        let by_helper: HashSet<String> = state.seen.iter().chain(&state.closest).map(Candidate::key).collect();
        let inside = state.image_text || state.only.is_some_and(|only| only.inside);
        let rows: Vec<Row> = state
            .results
            .iter()
            .map(|ranked| {
                let candidate = &ranked.candidate;
                let path = candidate.path();
                let (key, kind) = icons::key(&path, &candidate.name, candidate.is_folder, size);
                let icon = images.get(&key).cloned();
                // Found by what's inside, not by its name: the words that matched say why. A video, by the moment that fits.
                // The words that matched first (with ">" they're all there is); else the moment it shows what was asked.
                let candidate_key = candidate.key();
                let words = if ranked.highlights.is_empty() || inside { state.snippets.get(&candidate_key) } else { None };
                let moment = if words.is_some() || inside { None } else {
                    state.seen_at.get(&candidate_key)
                        .map(|&at| (content::Snippet { text: format!("At {}", format::moment(at)), highlights: Vec::new(), at: None }, content::Source::Text))
                };
                let (snippet, badge) = match (words, moment.as_ref()) {
                    (Some(words), _) => (Some(words), None),
                    (None, Some(moment)) => (Some(moment), Some("Shown")),
                    (None, None) if ranked.highlights.is_empty() && by_helper.contains(&candidate_key) => {
                        (None, Some(if is_picture(&candidate.name) || is_video(&candidate.name) { "Shown" } else { "By meaning" }))
                    }
                    _ => (None, None),
                };
                keys.push((key, kind, path));
                row(candidate, &ranked.highlights, accent, icon, now, twelve, snippet, badge)
            })
            .collect();
        drop(images);
        *self.row_keys.borrow_mut() = keys;
        replace_rows(&self.rows, rows);
        let visible = state.visible;
        drop(state);
        self.request_icons(visible.0, visible.1);
    }

    /// Asks for the icons of the rows on screen (and a few past them).
    fn request_icons(&self, first: usize, count: usize) {
        let keys = self.row_keys.borrow();
        let images = self.images.borrow();
        let end = (first + count + 6).min(keys.len());
        // Requests go to the front of the queue, so ask for the last visible first and the first visible last.
        for (key, kind, path) in keys[first.min(end)..end].iter().rev() {
            if images.contains_key(key) {
                continue;
            }
            let size = if *kind == KindKey::Preview { (self.icon_pixels() as f32 * 1.5) as u32 } else { self.icon_pixels() };
            self.icons.request(Request { key: key.clone(), path: path.clone(), size, kind: *kind });
        }
    }

    pub fn visible_rows(&self, first: i32, count: i32) {
        let (first, count) = (first.max(0) as usize, count.max(1) as usize);
        self.state.borrow_mut().visible = (first, count);
        if self.all.borrow().is_some() {
            return;
        }
        self.request_icons(first, count);
    }

    fn icon_pixels(&self) -> u32 {
        (ICON_SIZE * self.scale.get()).round() as u32
    }

    fn icon_ready(&self, request: Request, bitmap: Option<winsys::icons::Bitmap>) {
        let image = match bitmap {
            Some(mut bitmap) => {
                if request.key.starts_with("preview|") {
                    self.protect(&request.path, &mut bitmap);
                }
                image(&bitmap)
            }
            None => {
                // The preview has nothing to show: the row's icon, then.
                if let Some(path) = request.key.strip_prefix("preview|") {
                    self.preview_fallback(path);
                    return;
                }
                // No thumbnail: fall back to the type's icon.
                if request.kind == KindKey::Preview {
                    let path = request.path.clone();
                    let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
                    let extension = search::filter::extension(&name).unwrap_or("").to_ascii_lowercase();
                    let size = self.icon_pixels();
                    let fallback = format!("{size}|.{extension}");
                    if let Some(image) = self.images.borrow().get(&fallback).cloned() {
                        self.images.borrow_mut().insert(request.key.clone(), image.clone());
                        self.place_icon(&request.key, image);
                    } else {
                        self.icons.request(Request { key: request.key.clone(), path, size, kind: KindKey::Icon });
                    }
                }
                return;
            }
        };
        if request.key.starts_with("preview|") {
            self.preview_image_ready(&request.path, image);
            return;
        }
        if self.images.borrow().len() > 3000 {
            self.images.borrow_mut().clear();
        }
        self.images.borrow_mut().insert(request.key.clone(), image.clone());
        self.place_icon(&request.key, image);
    }

    fn place_icon(&self, key: &str, image: slint::Image) {
        if let Some(all) = self.all.borrow().as_ref() {
            all.icon_ready(key, &image);
            return;
        }
        let keys = self.row_keys.borrow();
        for (index, (row_key, _, _)) in keys.iter().enumerate() {
            if row_key == key {
                if let Some(mut row) = self.rows.row_data(index) {
                    row.icon = image.clone();
                    row.has_icon = true;
                    self.rows.set_row_data(index, row);
                }
            }
        }
    }

    // ---- Status, preview ----------------------------------------------------------------------------------------

    pub fn update_status(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = self.state.borrow();
        // (With nothing found yet, the helper may still find what fits by what it shows.)
        let searching = !state.main_done || !state.fast_done || (state.seeing && (state.image_shown || state.results.is_empty()));
        let matching = state.terms.matching;
        let shown = match state.mode {
            Mode::Ranked => state.results.len() as u64,
            Mode::All(_) => self.all.borrow().as_ref().map(|all| all.total()).unwrap_or(0),
        };
        let status = if let Some((notice, _)) = &state.notice {
            notice.clone()
        } else if state.terms.is_empty() && state.mode == Mode::Ranked {
            String::new()
        } else if searching && (shown == 0 || state.found_stale) {
            "Searching…".into()
        } else {
            match (state.mode, state.total) {
                (Mode::Ranked, Some(total)) if total > shown => format!("Best {} of {}", group(shown), group(total)),
                _ => format!("{} result{}", group(shown), if shown == 1 { "" } else { "s" }),
            }
        };
        ui.set_status(status.into());
        let more = match (state.mode, state.total) {
            (Mode::Ranked, Some(total)) if total > shown && !state.terms.is_empty() => format!("Show all {} →", group(total)),
            _ => String::new(),
        };
        ui.set_more_label(more.into());
        let timing = &state.timing;
        let mut parts = Vec::new();
        if let Some(hot) = timing.hot {
            parts.push(format!("{:.1} ms", hot.as_secs_f64() * 1000.0));
        }
        let local = everything::is_local();
        if let Some(ms) = timing.everything_ms {
            parts.push(if local { format!("Names {ms:.0} ms") } else { format!("Everything {ms:.0} ms") });
        }
        if let Some(contents) = timing.contents {
            parts.push(format!("Contents {:.1} ms", contents.as_secs_f64() * 1000.0));
        }
        if let Some(ms) = timing.seeing_ms {
            parts.push(format!("Images {ms:.0} ms"));
        }
        ui.set_timing(if state.terms.is_empty() { SharedString::new() } else { parts.join(" · ").into() });
        ui.set_section(if state.terms.is_empty() && state.extra.is_empty() && shown > 0 {
            "Recent".into()
        } else if state.showing_closest {
            format!("Nothing called \u{201c}{}\u{201d}. Closest by what they show or mean", state.terms.text).into()
        } else if state.showing_loose && state.terms.words.len() == 1 {
            format!("Nothing called \u{201c}{}\u{201d}. Names like it", state.terms.text).into()
        } else if state.showing_loose {
            format!("Nothing has all of \u{201c}{}\u{201d}. Names with some of the words", state.terms.text).into()
        } else {
            SharedString::new()
        });

        let (title, detail) = if shown > 0 || searching {
            (String::new(), String::new())
        } else if !state.extra.is_empty() {
            (format!("Nothing fits \u{201c}{}\u{201d}", state.typed.trim()),
             "Sizes and lengths are read from the images, videos and recordings found; content: looks in the words Excavate has read; rc: and dr: count what was opened from Excavate.".into())
        } else if state.terms.is_empty() {
            let hotkey = self.settings.borrow().hotkey.clone();
            ("Find anything on this PC".into(),
             format!("Type a name, part of one, or initials (\"vsc\"). {hotkey} opens Excavate from anywhere."))
        } else if state.image_text {
            (format!("No image has the words \u{201c}{}\u{201d}", state.terms.text),
             "Only words read from images are searched while the image toggle is on.".into())
        } else if state.image_shown {
            (format!("No image shows \u{201c}{}\u{201d}", state.terms.text),
             "Only images are searched, by what they show, while this toggle is on. Try describing it another way.".into())
        } else if let Some(only) = state.only {
            let noun = kind_noun(state.filter, only.extension);
            if only.inside {
                (format!("No {noun} has the words \u{201c}{}\u{201d}", state.terms.text),
                 "Only the words inside are searched after > and a type. Take them out to search names too.".into())
            } else {
                (format!("No {noun} fits \u{201c}{}\u{201d}", state.terms.text),
                 "Only what they show or mean is searched after < and a type. Try describing it another way.".into())
            }
        } else if matching.only_everything() && !state.everything.available && local {
            ("This search needs the list of names".into(), "Full paths and regular expressions search Excavate's list of your drives, which isn't ready yet.".into())
        } else if matching.only_everything() && !state.everything.available {
            ("This search needs Everything".into(), "Full paths and regular expressions go through Everything, which isn't running.".into())
        } else if matching.only_everything() && state.terms.text.chars().count() < 2 {
            (String::new(), String::new())
        } else if matching.regex {
            (format!("Nothing matches \u{201c}{}\u{201d}", state.terms.text), "Check the regular expression. Ctrl+R turns it off.".into())
        } else if matching.case || matching.whole_word || matching.path {
            (format!("Nothing called \u{201c}{}\u{201d}", state.terms.text),
             on_toggles(matching))
        } else if state.route != Route::Name && state.seen_images == Some(0) {
            (format!("Nothing called \u{201c}{}\u{201d}", state.terms.text),
             "Excavate hasn't looked at what your images show yet. Settings, Activity, says how far it has got.".into())
        } else {
            (format!("Nothing called \u{201c}{}\u{201d}", state.terms.text),
             if local { "Check the spelling, try fewer letters, or use search syntax like ext:pdf or dm:today." }
             else { "Check the spelling, try fewer letters, or use Everything syntax like ext:pdf or dm:today." }.into())
        };
        ui.set_empty_title(title.into());
        ui.set_empty_detail(detail.into());

        let (banner, action) = if !self.settings.borrow().setup_done {
            ("Excavate isn't reading inside your files yet".to_string(), "Set up".to_string())
        } else if let Some(step) = &state.fixing {
            // (Everything is started again on the way: this, not that it isn't running.)
            (step.clone(), String::new())
        } else if !state.everything.available && local {
            ("Excavate's list of your drives isn't ready: only your folders are searched".to_string(), String::new())
        } else if !state.everything.available {
            if everything::installed_everything().is_some() {
                ("Everything isn't running: only your folders are searched".to_string(), "Start Everything".to_string())
            } else {
                ("Everything isn't installed: only your folders are searched".to_string(), "Get Everything".to_string())
            }
        } else if state.health == everything::Health::Loading && everything::is_local() {
            ("Excavate is listing your drives: names are found once it's done".to_string(), String::new())
        } else if state.health == everything::Health::Loading {
            ("Everything is loading its list of files: some names are missing until it's done".to_string(), String::new())
        } else if matches!(state.health, everything::Health::Behind(_)) && !local {
            ("Everything can't see new files, so they're missing here".to_string(), "Fix".to_string())
        } else {
            (String::new(), String::new())
        };
        let warning = self.settings.borrow().setup_done && state.fixing.is_none()
            && (!state.everything.available || matches!(state.health, everything::Health::Behind(_)));
        ui.set_banner(banner.into());
        ui.set_banner_action(action.into());
        ui.set_banner_warning(warning);
    }

    /// Whether times read "2:30 PM".
    pub fn twelve_hour(&self) -> bool {
        self.settings.borrow().twelve_hour(winsys::process::twelve_hour_clock())
    }

    /// Starts or stops following the indexer's progress (status.json, once a second while the popup is on screen).
    pub fn follow_reading(&self, on: bool) {
        if on {
            self.show_reading();
            self.reading_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), || with_app(|app| app.show_reading()));
        } else {
            self.reading_timer.stop();
        }
    }

    /// The footer's indicator and, with nothing typed, the progress card.
    fn show_reading(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let status = if self.settings.borrow().setup_done { config::status::Status::load(&self.paths) } else { None };
        if let Some(status) = &status {
            crate::setup::check_trial(self, status);
        }
        let progress = status.map(|status| crate::setup::progress(&status));
        // Paused by the person: said so, with or without work waiting, and a click resumes it.
        let paused = {
            let settings = self.settings.borrow();
            (settings.setup_done && settings.paused(now())).then(|| settings.pause_text(winsys::process::utc_offset(), settings.twelve_hour(winsys::process::twelve_hour_clock())))
        };
        ui.set_reading_paused(paused.is_some());
        if let Some(paused) = paused {
            ui.set_reading(format!("{paused} · Resume").into());
            ui.set_reading_progress(progress.map_or(-1.0, |progress| progress.fraction));
            return;
        }
        match progress.filter(|progress| progress.active) {
            Some(progress) => {
                ui.set_reading(progress.summary.into());
                ui.set_reading_progress(progress.fraction);
            }
            None => {
                ui.set_reading(SharedString::new());
                ui.set_reading_progress(0.0);
            }
        }
    }

    pub fn set_everything_status(&self, status: everything::Status) {
        self.state.borrow_mut().everything = status;
        self.update_status();
    }

    pub fn set_everything_health(&self, health: everything::Health) {
        self.state.borrow_mut().health = health;
        self.update_status();
    }

    pub fn everything_health(&self) -> everything::Health {
        self.state.borrow().health
    }

    /// The moment in a video found by what it shows, or in a recording by what's said, in seconds.
    pub fn moment(&self, path: &str) -> Option<f64> {
        let state = self.state.borrow();
        let key = path.to_lowercase();
        state.seen_at.get(&key).or_else(|| state.heard_at.get(&key)).copied()
    }

    /// The step a fix of Everything is on, or none when there's no fix running.
    pub fn set_everything_fixing(&self, step: Option<String>) {
        self.state.borrow_mut().fixing = step;
        self.update_status();
    }

    pub fn everything_fixing(&self) -> Option<String> {
        self.state.borrow().fixing.clone()
    }

    pub fn notice(&self, text: &str) {
        self.state.borrow_mut().notice = Some((text.to_string(), Instant::now()));
        self.update_status();
        self.notice_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(1600), || {
            with_app(|app| {
                app.state.borrow_mut().notice = None;
                app.update_status();
            });
        });
    }

    pub fn selection_moved(&self, _index: i32) {
        self.state.borrow_mut().moved = true;
        self.schedule_preview();
    }

    fn schedule_preview(&self) {
        // Coalesces fast arrowing: the preview follows once the selection rests for a moment.
        self.preview_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(45), || with_app(|app| app.load_preview()));
    }

    fn load_preview(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(target) = self.selected_target() else {
            ui.set_preview(Preview::default());
            return;
        };
        let now = now();
        let kind = if target.origin == Some(Origin::App) { "App" } else { kind_label(&target.name, target.is_folder) };
        let mut facts = Vec::new();
        if let Some(size) = target.size {
            facts.push(format::size(size));
        }
        if let Some(modified) = target.modified {
            facts.push(format!("Modified {}", format::when(modified, now, self.twelve_hour())));
        }
        if let Some(opened) = self.history.borrow().get(&target.path) {
            facts.push(format!("Opened {} time{}", opened.count, if opened.count == 1 { "" } else { "s" }));
        }
        let size = self.icon_pixels();
        let (icon_key, _) = icons::key(&target.path, &target.name, target.is_folder, size);
        let placeholder = self.images.borrow().get(&icon_key).cloned();
        let picture = !target.is_folder && is_picture(&target.name);
        let video = !target.is_folder && is_video(&target.name);
        let pdf_page = !target.is_folder && is_pdf(&target.name);
        let presenting = self.settings.borrow().blur_when_sharing && winsys::process::presenting();
        if ui.get_enlarged() {
            // Up and down go from image to image (and video, and PDF), but what's selected can change beneath it.
            if !(picture || video || pdf_page) || presenting {
                ui.invoke_close_enlarged();
            } else if !ui.get_preview().path.eq_ignore_ascii_case(&target.path) {
                ui.set_enlarged_sharp_ready(false);
                ui.set_enlarged_width(0);
                ui.set_enlarged_height(0);
            }
        }
        ui.set_preview(Preview {
            visible: true,
            title: format::display_name(&target.name).into(),
            kind: kind.into(),
            facts: facts.join("  ·  ").into(),
            path: target.path.clone().into(),
            // A picture waits for what the index knows (where its secrets are) before it's drawn. Anything else waits for
            // its own preview, a moment: the row's small icon, blown up meanwhile, would flash blurred before it.
            has_image: target.is_folder && placeholder.is_some(),
            image: if target.is_folder { placeholder.unwrap_or_default() } else { slint::Image::default() },
            text: SharedString::new(),
            has_text: false,
            text_label: SharedString::new(),
            hidden: if presenting { "Previews are hidden while you present".into() } else { SharedString::new() },
            can_reveal: false,
            can_enlarge: (picture || video || pdf_page) && !presenting,
            player: PLAYER_NONE,
            progress: 0.0,
            time: SharedString::new(),
        });
        *self.preview_marks.borrow_mut() = None;
        let pixels = (PREVIEW_SIZE * self.scale.get()).round() as u32;
        let image_request = Request { key: format!("preview|{}", target.path), path: target.path.clone(), size: pixels, kind: KindKey::Preview };
        let indexed = !target.is_folder && (picture || self.state.borrow().snippets.contains_key(&target.path.to_lowercase())
            || is_text(&target.name) || is_document(&target.name));
        if indexed {
            // What the index read from it (the words around the match, the text in a picture) and its privacy marks.
            let path = target.path.clone();
            self.contents.stored(path.clone(), move |stored| {
                let _ = slint::invoke_from_event_loop(move || with_app(|app| app.stored_ready(&path, stored)));
            });
        }
        // A PDF that's in the index waits for its marks too (its first page may show a secret).
        let pdf = !target.is_folder && is_pdf(&target.name);
        if !picture && !video && !(pdf && indexed) {
            self.request_preview(image_request.clone());
        }
        self.stop_video();
        let recording = !target.is_folder && is_recording(&target.name);
        *self.media.borrow_mut() = Media { path: target.path.clone(), video: !target.is_folder && is_video(&target.name), recording,
                                           from: self.moment(&target.path).unwrap_or(0.0), ..Media::default() };
        if video {
            let path = target.path.clone();
            self.contents.stored(path.clone(), move |stored| {
                let _ = slint::invoke_from_event_loop(move || with_app(|app| app.video_marked(&path, stored, presenting)));
            });
        }
        if recording && !presenting {
            self.media_shown(PLAYER_STOPPED);
            // How long it is, for the timeline, without decoding it.
            let path = target.path.clone();
            std::thread::spawn(move || {
                winsys::com_mta();
                if let Ok(length) = winsys::video::length(&path) {
                    let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                        let fresh = app.media.borrow().path == path;
                        if fresh {
                            app.media.borrow_mut().length = length;
                            app.media_shown(PLAYER_STOPPED);
                        }
                    }));
                }
            });
        }
        if !target.is_folder && is_text(&target.name) && target.size.is_none_or(|size| size < 20 << 20) {
            let path = target.path.clone();
            std::thread::spawn(move || {
                let text = winsys::files::text_head(&path, 12 * 1024).map(|text| first_lines(&text, 60));
                if let Some(text) = text {
                    let _ = slint::invoke_from_event_loop(move || with_app(|app| app.preview_text_ready(&path, text)));
                }
            });
        }
    }

    /// Asks for the preview's image: a PDF's first page by Windows' PDF renderer (Explorer has none without a PDF
    /// reader's thumbnail handler), anything else from the shell.
    fn request_preview(&self, request: Request) {
        if !is_pdf(&request.path) {
            self.icons.request(request);
            return;
        }
        std::thread::spawn(move || {
            winsys::com_mta();
            let page = winsys::pdf::first_page(&request.path, request.size)
                .map_err(|error| log(&format!("a PDF's first page: {error}")))
                .ok()
                .or_else(|| winsys::icons::image(&request.path, request.size, winsys::icons::Kind::Preview));
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.icon_ready(request, page)));
        });
    }

    /// Stops the video playing in the preview, if one is.
    pub fn stop_video(&self) {
        self.playing.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// A video's marks are in (or it has none): its poster is asked for and it starts playing, its secrets covered
    /// unless they're revealed.
    fn video_marked(&self, path: &str, stored: Option<content::Stored>, presenting: bool) {
        if self.media.borrow().path != path {
            return;
        }
        let marks = stored.as_ref().map(|stored| Marks::parse(&stored.marks)).unwrap_or_default();
        let hides = self.hides(path);
        {
            let mut media = self.media.borrow_mut();
            media.secrets = if hides { marks.video_boxes.clone() } else { Vec::new() };
            media.marked = true;
        }
        *self.preview_marks.borrow_mut() = Some((path.to_string(), marks.clone()));
        if let Some(ui) = self.ui.upgrade() {
            let mut preview = ui.get_preview();
            if preview.path.as_str() == path && !marks.video_boxes.is_empty() && hides && preview.hidden.is_empty() {
                preview.hidden = describe_hidden(&marks.kinds).into();
                preview.can_reveal = !presenting;
                ui.set_preview(preview);
            }
        }
        let pixels = (PREVIEW_SIZE * self.scale.get()).round() as u32;
        self.icons.request(Request { key: format!("preview|{path}"), path: path.to_string(), size: pixels, kind: KindKey::Preview });
        if !presenting {
            self.play_video(path, false, true);
        }
    }

    /// The preview's video or recording was clicked: a video's sound on or off (from where it is), a recording
    /// played from its moment (or where it was stopped) or stopped.
    pub fn media_clicked(&self) {
        let media = self.media.borrow().clone();
        if media.video {
            self.stop_video();
            self.media.borrow_mut().from = media.at;
            self.play_video(&media.path, !media.sound, false);
        } else if media.recording {
            if media.sound {
                self.stop_video();
                let mut kept = self.media.borrow_mut();
                kept.sound = false;
                kept.from = kept.at;
                drop(kept);
                self.media_shown(PLAYER_STOPPED);
            } else {
                self.play_recording(&media.path);
            }
        }
    }

    /// Plays a recording in the preview from its moment (or where it was stopped), until it ends or another turn
    /// comes; the timeline follows it.
    fn play_recording(&self, path: &str) {
        use std::sync::atomic::Ordering;
        let turn = self.playing.fetch_add(1, Ordering::SeqCst) + 1;
        let playing = self.playing.clone();
        let from = {
            let mut media = self.media.borrow_mut();
            media.sound = true;
            // At the end: from the start again.
            if media.length > 0.0 && media.from >= media.length - 0.5 { 0.0 } else { media.from }
        };
        self.media_shown(PLAYER_PLAYING);
        let path = path.to_string();
        let _ = std::thread::Builder::new().name("preview sound".into()).spawn(move || {
            winsys::com_mta();
            let position = Arc::new(winsys::sound::Position::new());
            // The timeline, four times a second.
            let ticker = {
                let (position, playing, path) = (position.clone(), playing.clone(), path.clone());
                std::thread::spawn(move || {
                    while playing.load(Ordering::SeqCst) == turn {
                        std::thread::sleep(Duration::from_millis(250));
                        if let Some(at) = position.get() {
                            let path = path.clone();
                            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.media_at(turn, &path, at)));
                        }
                    }
                })
            };
            let ended = winsys::sound::play(&path, from, &|| playing.load(Ordering::SeqCst) == turn, &position);
            if let Err(error) = &ended {
                log(&format!("preview sound: {error}"));
            }
            let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                if app.playing.load(Ordering::SeqCst) == turn && app.media.borrow().path == path {
                    let mut media = app.media.borrow_mut();
                    media.sound = false;
                    if matches!(ended, Ok(true)) {
                        media.at = media.length;
                    }
                    media.from = media.at;
                    drop(media);
                    app.media_shown(PLAYER_STOPPED);
                }
            }));
            let _ = ticker.join();
        });
    }

    /// Where what's playing in the preview is now.
    fn media_at(&self, turn: u64, path: &str, at: f64) {
        if self.playing.load(std::sync::atomic::Ordering::SeqCst) != turn || self.media.borrow().path != path {
            return;
        }
        self.media.borrow_mut().at = at;
        let state = if self.media.borrow().sound { PLAYER_PLAYING } else { PLAYER_STOPPED };
        let state = if self.media.borrow().video { if self.media.borrow().sound { PLAYER_SOUND } else { PLAYER_MUTED } } else { state };
        self.media_shown(state);
    }

    /// The preview's player: its state, the timeline and the time.
    fn media_shown(&self, state: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preview = ui.get_preview();
        let media = self.media.borrow();
        if preview.path.as_str() != media.path {
            return;
        }
        let at = if media.sound || media.video { media.at } else { media.from };
        preview.player = state;
        preview.progress = if media.length > 0.0 { (at / media.length).clamp(0.0, 1.0) as f32 } else { 0.0 };
        preview.time = if media.length > 0.0 { format!("{} / {}", clock(at), clock(media.length)).into() } else { SharedString::new() };
        ui.set_preview(preview);
    }

    /// Plays a video in the preview from the moment that matched (or its start, or where it was when its sound was
    /// turned on), over and over: decoded at the preview's size on the graphics hardware when Windows can, at its own
    /// pace, or with its sound and kept to it. Frames the window hasn't drawn yet aren't piled up: the next waits for
    /// the last to be shown.
    fn play_video(&self, path: &str, sound: bool, wait: bool) {
        use std::sync::atomic::Ordering;
        let turn = self.playing.load(Ordering::SeqCst);
        let playing = self.playing.clone();
        let from = self.media.borrow().from;
        let secrets = self.media.borrow().secrets.clone();
        self.media.borrow_mut().sound = sound;
        self.media_shown(if sound { PLAYER_SOUND } else { PLAYER_MUTED });
        // Enlarged: the size it's shown at (`enlarged_video_side`), not more, so nothing larger than shown is decoded.
        let side = match self.ui.upgrade().filter(|ui| ui.get_enlarged()) {
            Some(ui) => self.enlarged_video_side(&ui),
            None => ((PREVIEW_SIZE * self.scale.get()).round() as u32).max(160),
        };
        self.media.borrow_mut().side = side;
        let path = path.to_string();
        let shown = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let _ = std::thread::Builder::new().name("preview video".into()).spawn(move || {
            // A moment's rest first: arrowing past a video doesn't start it.
            if wait && !sound {
                std::thread::sleep(Duration::from_millis(250));
            }
            if playing.load(Ordering::SeqCst) != turn {
                return;
            }
            winsys::com_mta();
            // How long it is, for the timeline, and its own size, for zooming in on it.
            if let Ok((length, native)) = winsys::video::length_and_size(&path) {
                let path = path.clone();
                let _ = slint::invoke_from_event_loop(move || with_app(|app| {
                    if app.media.borrow().path == path {
                        let mut media = app.media.borrow_mut();
                        media.length = length;
                        media.native = native.or(media.native);
                    }
                }));
            }
            let mut from = from;
            loop {
                let position = winsys::sound::Position::new();
                let ended = std::thread::scope(|scope| {
                    if sound {
                        let (path, playing, position) = (&path, &playing, &position);
                        scope.spawn(move || {
                            winsys::com_mta();
                            if let Err(error) = winsys::sound::play(path, from, &|| playing.load(Ordering::SeqCst) == turn, position) {
                                log(&format!("preview sound: {error}"));
                            }
                        });
                    }
                    winsys::video::play_paced(&path, from, side, sound.then_some(&position), &mut |frame| {
                    if playing.load(Ordering::SeqCst) != turn {
                        return false;
                    }
                    if !shown.load(Ordering::SeqCst) {
                        // The window hasn't drawn the last one yet: this frame is skipped.
                        return true;
                    }
                    let seconds = frame.seconds;
                    let mut picture = frame.picture;
                    cover_secrets(&mut picture, &secrets, seconds);
                    let mut rgba = Vec::with_capacity(picture.pixels.len() / 3 * 4);
                    for pixel in picture.pixels.chunks_exact(3) {
                        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                    }
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&rgba, picture.width, picture.height);
                    shown.store(false, Ordering::SeqCst);
                    let (path, shown) = (path.clone(), shown.clone());
                    let _ = slint::invoke_from_event_loop(move || {
                        with_app(|app| {
                            app.video_frame(turn, &path, buffer);
                            app.media_at(turn, &path, seconds);
                        });
                        shown.store(true, Ordering::SeqCst);
                    });
                    true
                    })
                });
                match ended.map(|paced| paced.ended) {
                    // To the end: again, from the start.
                    Ok(true) if playing.load(Ordering::SeqCst) == turn => from = 0.0,
                    Ok(_) => return,
                    Err(error) => {
                        log(&format!("preview video: {error}"));
                        return;
                    }
                }
            }
        });
    }

    fn video_frame(&self, turn: u64, path: &str, buffer: slint::SharedPixelBuffer<slint::Rgba8Pixel>) {
        if self.playing.load(std::sync::atomic::Ordering::SeqCst) != turn {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preview = ui.get_preview();
        // (A video doesn't play while presenting; one with secrets plays with them covered, and says so.)
        if preview.path.as_str() != path {
            return;
        }
        if ui.get_enlarged() {
            // Zoomed by its own size: "Actual size" is the video's, whatever size its frames are decoded at now.
            let (width, height) = self.media.borrow().native.unwrap_or((buffer.width(), buffer.height()));
            ui.set_enlarged_width(width as i32);
            ui.set_enlarged_height(height as i32);
            ui.set_enlarged_sharp(slint::Image::from_rgba8(buffer));
            ui.set_enlarged_sharp_ready(true);
            return;
        }
        preview.image = slint::Image::from_rgba8(buffer);
        preview.has_image = true;
        ui.set_preview(preview);
    }

    /// The index's copy of the file in the preview: its privacy marks, and for a match by contents (or a picture) the
    /// words read from it.
    fn stored_ready(&self, path: &str, stored: Option<content::Stored>) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preview = ui.get_preview();
        if preview.path.as_str() != path {
            return;
        }
        let mut marks = stored.as_ref().map(|stored| Marks::parse(&stored.marks)).unwrap_or_default();
        let picture = is_picture(path);
        if !marks.boxes.is_empty() {
            marks.orientation = winsys::imaging::orientation(path);
        }
        *self.preview_marks.borrow_mut() = Some((path.to_string(), marks.clone()));
        if marks.similar > 0 {
            preview.facts = format!("{}  ·  + {} similar", preview.facts, group(u64::from(marks.similar))).into();
        }
        if picture || is_pdf(path) {
            let pixels = (PREVIEW_SIZE * self.scale.get()).round() as u32;
            self.request_preview(Request { key: format!("preview|{path}"), path: path.to_string(), size: pixels, kind: KindKey::Preview });
        }
        let content_hit = self.state.borrow().snippets.contains_key(&path.to_lowercase());
        if let Some(stored) = stored.filter(|stored| !stored.text.trim().is_empty()) {
            if content_hit || (picture && !preview.has_text) {
                // The part that matched (secrets in it were replaced by labels when it was indexed).
                let matching = self.state.borrow().terms.matching;
                let words = content::snippet::Words::new(&self.state.borrow().terms.words, !matching.whole_word).exactly(matching.case, matching.whole_word);
                let excerpt = content::snippet::make(&stored.text, &words, 900);
                preview.text = excerpt.text.into();
                preview.has_text = true;
                preview.text_label = if picture { "Text in this image".into() } else { "Where it matched".into() };
            }
        }
        if !marks.kinds.is_empty() && preview.hidden.is_empty() && self.hides(path) {
            preview.hidden = describe_hidden(&marks.kinds).into();
            // (A PDF's secrets are covered on its first page, or all of it when they have no place there.)
            preview.can_reveal = (picture && !marks.boxes.is_empty()) || is_pdf(path);
        }
        ui.set_preview(preview);
        self.load_enlarged();
    }

    /// Whether secrets in this file are hidden now (not revealed, and the setting is on).
    fn hides(&self, path: &str) -> bool {
        self.settings.borrow().blur_secrets && self.revealed.borrow().as_deref().is_none_or(|revealed| !revealed.eq_ignore_ascii_case(path))
    }

    /// Blurs what shouldn't show in a preview picture: everything while presenting, else the lines with secrets.
    fn protect(&self, path: &str, bitmap: &mut winsys::icons::Bitmap) {
        if self.settings.borrow().blur_when_sharing && winsys::process::presenting() {
            crate::blur::pixelate(bitmap, &[[0, 0, bitmap.width as i32, bitmap.height as i32]], (bitmap.width, bitmap.height));
            return;
        }
        if !self.hides(path) {
            return;
        }
        let marks = self.preview_marks.borrow();
        if let Some((_, marks)) = marks.as_ref().filter(|(marked, _)| marked.eq_ignore_ascii_case(path)) {
            if !marks.boxes.is_empty() && marks.width > 0 {
                let (regions, size) = crate::blur::turned(&marks.boxes, (marks.width, marks.height), marks.orientation);
                crate::blur::pixelate(bitmap, &regions, size);
            }
            // A PDF's secrets on its first page: there, a little wider than their letters; not placed (an older read, a
            // turned page), all of the page.
            if is_pdf(path) && !marks.kinds.is_empty() && marks.boxes.is_empty() {
                match &marks.pdf_boxes {
                    Some(places) => {
                        let (width, height) = (i64::from(bitmap.width), i64::from(bitmap.height));
                        let regions: Vec<[i32; 4]> = places.iter().map(|b| {
                            let pad = ((b[3] - b[1]) * height / 10_000 / 3).max(2);
                            [(b[0] * width / 10_000 - pad).max(0) as i32, (b[1] * height / 10_000 - pad).max(0) as i32,
                             (b[2] * width / 10_000 + pad).min(width) as i32, (b[3] * height / 10_000 + pad).min(height) as i32]
                        }).collect();
                        crate::blur::pixelate(bitmap, &regions, (bitmap.width, bitmap.height));
                    }
                    None => crate::blur::pixelate(bitmap, &[[0, 0, bitmap.width as i32, bitmap.height as i32]], (bitmap.width, bitmap.height)),
                }
            }
            // A video's poster is Explorer's, from a moment it chose: every place a secret ever shows is covered.
            if !marks.video_boxes.is_empty() {
                let (width, height) = (i64::from(bitmap.width), i64::from(bitmap.height));
                let regions: Vec<[i32; 4]> = marks.video_boxes.iter().map(|b| {
                    let pad = ((b[5] - b[3]) * height / 10_000 / 4).max(2);
                    [(b[2] * width / 10_000 - pad).max(0) as i32, (b[3] * height / 10_000 - pad).max(0) as i32,
                     (b[4] * width / 10_000 + pad).min(width) as i32, (b[5] * height / 10_000 + pad).min(height) as i32]
                }).collect();
                crate::blur::pixelate(bitmap, &regions, (bitmap.width, bitmap.height));
            }
        }
    }

    /// The preview's size, or none, as chosen in Settings (Ctrl+P still shows and hides it).
    pub fn preview_size_changed(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let settings = self.settings.borrow();
        let (width, image) = match settings.preview_size.as_str() {
            "small" => (260.0, 140.0),
            "large" => (420.0, 260.0),
            _ => (320.0, 188.0),
        };
        ui.set_show_preview(settings.preview);
        ui.set_preview_width(width);
        ui.set_preview_image(image);
        // As last dragged, if it was (and no size has been chosen in Settings since).
        ui.set_preview_dragged(settings.preview_dragged_width.max(0.0));
        ui.set_preview_image_dragged(settings.preview_dragged_image.max(0.0));
    }

    /// The preview of `path` has no image of its own: the row's icon, if the preview still shows it.
    fn preview_fallback(&self, path: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preview = ui.get_preview();
        if preview.path.as_str() != path || preview.has_image {
            return;
        }
        let name = path.rsplit('\\').next().unwrap_or(path);
        let (key, _) = icons::key(path, name, false, self.icon_pixels());
        if let Some(image) = self.images.borrow().get(&key).cloned() {
            preview.image = image;
            preview.has_image = true;
            ui.set_preview(preview);
        }
    }

    fn preview_image_ready(&self, path: &str, image: slint::Image) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preview = ui.get_preview();
        if preview.path.as_str() == path {
            preview.image = image;
            preview.has_image = true;
            ui.set_preview(preview);
        }
    }

    /// The enlarged preview opened or closed.
    pub fn enlarged_changed(&self, on: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        // A video plays again from where it is, at the size it's now shown.
        let video = self.media.borrow().clone();
        let replay = video.video && video.marked && ui.get_preview().player != PLAYER_NONE;
        if replay {
            self.stop_video();
            self.media.borrow_mut().from = video.at;
        }
        if on {
            // Whatever comes in from Everything meanwhile, what's enlarged stays selected.
            self.state.borrow_mut().moved = true;
            if replay {
                self.play_video(&video.path, video.sound, false);
            }
            self.load_enlarged();
            return;
        }
        self.enlarged_timer.stop();
        self.enlarged_request.set(self.enlarged_request.get() + 1);
        self.enlarged_asked.borrow_mut().take();
        ui.set_enlarged_sharp(slint::Image::default());
        ui.set_enlarged_sharp_ready(false);
        ui.set_enlarged_width(0);
        ui.set_enlarged_height(0);
        if replay {
            self.play_video(&video.path, video.sound, false);
        }
    }

    /// Up or down to the next image, past whatever isn't one.
    pub fn enlarged_move(&self, by: i32) {
        let Some(ui) = self.ui.upgrade() else { return };
        let count = ui.get_rows().row_count() as i32;
        let mut at = ui.get_selected();
        loop {
            at += by;
            if at < 0 || at >= count {
                return;
            }
            // (Further along a long list and not fetched yet: this is as far as it goes for now.)
            let Some(target) = self.target(at as usize) else { return };
            if !target.is_folder && (is_picture(&target.name) || is_video(&target.name) || is_pdf(&target.name)) {
                break;
            }
        }
        ui.set_enlarged_zoom(0.0);
        ui.set_selected(at);
        ui.invoke_reveal_selected();
        self.selection_moved(at);
    }

    /// The enlarged image changed size. The wheel may have more to come: it's decoded for the new size once that rests.
    pub fn enlarged_zoomed(&self, settling: bool) {
        if settling {
            self.enlarged_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(ENLARGED_REST), || with_app(|app| app.load_enlarged()));
        } else {
            self.enlarged_timer.stop();
            self.load_enlarged();
        }
    }

    /// The size in pixels to decode the enlarged image at: the room it fits in, what it's zoomed to below actual size, or all of it.
    fn enlarged_bounds(&self, ui: &Popup) -> (u32, u32) {
        let zoom = ui.get_enlarged_zoom();
        let file = (ui.get_enlarged_width() as f32, ui.get_enlarged_height() as f32);
        if zoom >= 1.0 {
            return (ENLARGED_LARGEST, ENLARGED_LARGEST);
        }
        if zoom > 0.0 && file.0 > 0.0 {
            return ((file.0 * zoom).ceil() as u32, (file.1 * zoom).ceil() as u32);
        }
        let (size, scale) = (ui.window().size(), ui.window().scale_factor());
        ((size.width as f32 - ENLARGED_AROUND.0 * scale).ceil().max(1.0) as u32, (size.height as f32 - ENLARGED_AROUND.1 * scale).ceil().max(1.0) as u32)
    }

    /// Asks for the selected image at the size the enlarged preview shows it, on a thread. Secrets are covered there, so nothing
    /// uncovered ever reaches the window. It waits for what the index knows about them: `stored_ready` comes back here.
    pub fn load_enlarged(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if !ui.get_enlarged() {
            return;
        }
        let Some(target) = self.selected_target() else { return };
        // A video's frames come from its player, at this size (`play_video`): zoomed in or out far enough, it plays on
        // from where it is, decoded at the new size.
        if !target.is_folder && is_video(&target.name) {
            let media = self.media.borrow().clone();
            if media.video && media.marked && media.path.eq_ignore_ascii_case(&target.path) && ui.get_preview().player != PLAYER_NONE {
                let wanted = self.enlarged_video_side(&ui);
                if wanted > media.side + media.side / 4 || wanted * 5 / 3 < media.side {
                    self.stop_video();
                    self.media.borrow_mut().from = media.at;
                    self.play_video(&media.path, media.sound, false);
                }
            }
            return;
        }
        if !target.is_folder && is_pdf(&target.name) {
            self.load_enlarged_page(&ui, target.path);
            return;
        }
        let path = target.path;
        let covered = {
            let marks = self.preview_marks.borrow();
            let Some((_, marks)) = marks.as_ref().filter(|(marked, _)| marked.eq_ignore_ascii_case(&path)) else { return };
            (self.hides(&path) && !marks.boxes.is_empty() && marks.width > 0).then(|| (marks.boxes.clone(), (marks.width, marks.height)))
        };
        let bounds = self.enlarged_bounds(&ui);
        let asked = (path.clone(), bounds);
        if self.enlarged_asked.borrow().as_ref() == Some(&asked) {
            return;
        }
        *self.enlarged_asked.borrow_mut() = Some(asked);
        let request = self.enlarged_request.get() + 1;
        self.enlarged_request.set(request);
        let started = Instant::now();
        std::thread::spawn(move || {
            winsys::com_mta();
            let mut shown = match winsys::imaging::decode_for_showing(&path, bounds) {
                Ok(shown) => shown,
                // The small one stays.
                Err(error) => {
                    log(&format!("enlarging an image: {error}"));
                    return;
                }
            };
            if let Some((regions, size)) = covered {
                let (regions, size) = crate::blur::turned(&regions, size, shown.orientation);
                crate::blur::pixelate(&mut shown.bitmap, &regions, size);
            }
            let size = shown.size;
            let bitmap = shown.bitmap;
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bitmap.rgba, bitmap.width, bitmap.height);
            let took = started.elapsed();
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.enlarged_ready(request, buffer, size, took)));
        });
    }

    /// The longer side to decode an enlarged video at: as shown, up to its own size (and `ENLARGED_VIDEO_LARGEST`); to
    /// fit, at most `ENLARGED_VIDEO` until its size is known.
    fn enlarged_video_side(&self, ui: &Popup) -> u32 {
        let (width, height) = self.enlarged_bounds(ui);
        let own = self.media.borrow().native.map_or(ENLARGED_VIDEO, |(width, height)| width.max(height));
        width.max(height).min(own).min(ENLARGED_VIDEO_LARGEST).max(160)
    }

    /// A PDF's first page, enlarged: rendered at the size it's shown (to fit, or zoomed, up to `ENLARGED_PDF_LARGEST`),
    /// on a thread, its secrets covered there. Its "actual size" is the page's at 100% (96 per inch, by the display's
    /// scale).
    fn load_enlarged_page(&self, ui: &Popup, path: String) {
        let covered = {
            let marks = self.preview_marks.borrow();
            let Some((_, marks)) = marks.as_ref().filter(|(marked, _)| marked.eq_ignore_ascii_case(&path)) else { return };
            self.hides(&path) && !marks.kinds.is_empty()
        };
        let zoom = ui.get_enlarged_zoom();
        let scale = ui.window().scale_factor();
        let room = self.enlarged_bounds(ui);
        let key = if zoom > 0.0 { (0, (zoom * 1000.0) as u32) } else { room };
        let asked = (path.clone(), key);
        if self.enlarged_asked.borrow().as_ref() == Some(&asked) {
            return;
        }
        *self.enlarged_asked.borrow_mut() = Some(asked);
        let request = self.enlarged_request.get() + 1;
        self.enlarged_request.set(request);
        let started = Instant::now();
        std::thread::spawn(move || {
            winsys::com_mta();
            let side = |(width, height): (f32, f32)| {
                let (width, height) = (width * scale, height * scale);
                let shown = if zoom > 0.0 { zoom } else { (room.0 as f32 / width).min(room.1 as f32 / height).min(2.0) };
                ((width.max(height) * shown).ceil() as u32).clamp(64, ENLARGED_PDF_LARGEST)
            };
            let (mut bitmap, page) = match winsys::pdf::first_page_sized(&path, &side) {
                Ok(rendered) => rendered,
                Err(error) => {
                    log(&format!("enlarging a PDF: {error}"));
                    return;
                }
            };
            if covered {
                let (width, height) = (bitmap.width, bitmap.height);
                crate::blur::pixelate(&mut bitmap, &[[0, 0, width as i32, height as i32]], (width, height));
            }
            let size = ((page.0 * scale).round() as u32, (page.1 * scale).round() as u32);
            let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bitmap.rgba, bitmap.width, bitmap.height);
            let took = started.elapsed();
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.enlarged_ready(request, buffer, size, took)));
        });
    }

    fn enlarged_ready(&self, request: u64, buffer: slint::SharedPixelBuffer<slint::Rgba8Pixel>, size: (u32, u32), took: Duration) {
        let Some(ui) = self.ui.upgrade() else { return };
        if request != self.enlarged_request.get() || !ui.get_enlarged() {
            return;
        }
        log(&format!("enlarged image ready in {:.1} ms", took.as_secs_f64() * 1000.0));
        ui.set_enlarged_sharp(slint::Image::from_rgba8_premultiplied(buffer));
        ui.set_enlarged_width(size.0 as i32);
        ui.set_enlarged_height(size.1 as i32);
        ui.set_enlarged_sharp_ready(true);
    }

    fn preview_text_ready(&self, path: &str, text: String) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut preview = ui.get_preview();
        if preview.path.as_str() != path || !preview.text_label.is_empty() {
            return;
        }
        if self.settings.borrow().blur_when_sharing && winsys::process::presenting() {
            return;
        }
        let secrets = privacy::find(&text);
        let shown = if !secrets.is_empty() && self.hides(path) {
            let kinds: Vec<String> = secrets.iter().map(|secret| secret.kind.name().to_string()).collect();
            preview.hidden = describe_hidden(&kinds).into();
            preview.can_reveal = true;
            privacy::mask(&text, &secrets)
        } else {
            text
        };
        preview.text = shown.into();
        preview.has_text = true;
        ui.set_preview(preview);
    }

    /// Shows what was hidden in the selected file, once Windows Hello says it's the person.
    pub fn reveal(&self) {
        let Some(target) = self.selected_target() else { return };
        let owner = self.hwnd.get();
        self.verifying.set(true);
        let path = target.path.clone();
        std::thread::spawn(move || {
            winsys::com_mta();
            let verified = winsys::hello::verify(owner, "Show what Excavate hid in this file");
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.reveal_answered(&path, verified)));
        });
    }

    fn reveal_answered(&self, path: &str, verified: Result<winsys::hello::Verified, String>) {
        self.verifying.set(false);
        crate::popup::take_back_keyboard(self);
        match verified {
            Ok(winsys::hello::Verified::Yes) | Ok(winsys::hello::Verified::Unavailable) => {
                *self.revealed.borrow_mut() = Some(path.to_string());
                self.enlarged_asked.borrow_mut().take();
                self.load_preview();
            }
            Ok(winsys::hello::Verified::No) => self.notice("Not verified: still hidden"),
            Err(error) => {
                log(&format!("Windows Hello: {error}"));
                self.notice("Couldn't ask Windows Hello");
            }
        }
    }

    pub fn selected_target(&self) -> Option<Target> {
        let ui = self.ui.upgrade()?;
        let index = ui.get_selected().max(0) as usize;
        self.target(index)
    }

    pub fn target(&self, index: usize) -> Option<Target> {
        if let Some(all) = self.all.borrow().as_ref() {
            return all.target(index);
        }
        self.state.borrow().results.get(index).map(|ranked| Target::from(&ranked.candidate))
    }

    pub fn record_open(&self, target: &Target) {
        let query = self.state.borrow().terms.text.clone();
        let mut history = self.history.borrow_mut();
        history.record(&target.path, &query, now());
        let _ = history.save();
        self.everything.ran(&target.path);
    }

    /// What was typed (with the functions answered here, which the terms leave out).
    pub fn query_text(&self) -> String {
        self.state.borrow().typed.clone()
    }

    // ---- For measurements (`--bench`) ---------------------------------------------------------------------------

    pub fn everything_done(&self) -> bool {
        let state = self.state.borrow();
        state.main_done && state.fast_done && state.refine_done
    }

    pub fn main_done(&self) -> bool {
        let state = self.state.borrow();
        state.main_done && state.fast_done
    }

    pub fn everything_ms(&self) -> Option<f64> {
        self.state.borrow().timing.everything_ms
    }

    pub fn result_count(&self) -> usize {
        self.state.borrow().results.len()
    }

    pub fn total(&self) -> Option<u64> {
        self.state.borrow().total
    }

    pub fn top(&self, count: usize) -> Vec<String> {
        self.state.borrow().results.iter().take(count).map(|ranked| ranked.candidate.path()).collect()
    }

    pub fn hot_len(&self) -> usize {
        self.hot.borrow().len()
    }

    pub fn hot_build_ms(&self) -> u128 {
        self.hot.borrow().build_time.as_millis()
    }

    // ---- Show all -----------------------------------------------------------------------------------------------

    pub fn sort_label(&self) -> &'static str {
        match self.state.borrow().mode {
            Mode::Ranked if self.state.borrow().fuzzy => "Fuzzy match",
            Mode::Ranked => "Best match",
            Mode::All(Sort::NameAscending) => "Name",
            Mode::All(Sort::ModifiedDescending) => "Date modified",
            Mode::All(Sort::SizeDescending) => "Size",
            Mode::All(Sort::PathAscending) => "Path",
            Mode::All(Sort::DateRunDescending) => "Recently opened",
            Mode::All(_) => "Sorted",
        }
    }

    /// "Fuzzy match" chosen, or "Best match" again (`on` false); the search runs again.
    pub fn choose_fuzzy(&self, on: bool) {
        self.state.borrow_mut().fuzzy = on;
        if on {
            // Names only: the toggles for images' text and what they show step aside.
            let mut state = self.state.borrow_mut();
            state.image_text = false;
            state.image_shown = false;
            drop(state);
            self.show_toggles();
        }
        if self.state.borrow().mode == Mode::Ranked {
            self.search_again();
        } else {
            self.choose_sort(None);
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sort_label(self.sort_label().into());
        }
    }

    pub fn choose_sort(&self, sort: Option<Sort>) {
        // A list in some order is of names, which the image toggle leaves out.
        if sort.is_some() {
            self.state.borrow_mut().image_text = false;
            self.show_toggles();
        }
        if sort.is_some() {
            self.state.borrow_mut().fuzzy = false;
        }
        match sort {
            None => {
                let generation = {
                    let mut state = self.state.borrow_mut();
                    state.mode = Mode::Ranked;
                    state.generation += 1;
                    state.generation
                };
                self.leave_all();
                self.present();
                self.ask_everything(generation);
                self.ask_contents(generation);
            }
            Some(sort) => self.enter_all(sort),
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sort_label(self.sort_label().into());
        }
    }

    pub fn show_all(&self) {
        self.enter_all(Sort::NameAscending);
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sort_label(self.sort_label().into());
        }
    }

    fn enter_all(&self, sort: Sort) {
        let Some(ui) = self.ui.upgrade() else { return };
        let (terms, filter, generation) = {
            let mut state = self.state.borrow_mut();
            state.mode = Mode::All(sort);
            state.generation += 1;
            (state.terms.clone(), state.filter, state.generation)
        };
        let model = Rc::new(AllModel::new(terms, filter, self.scope.borrow().clone(), sort, generation, self.everything.clone(), self.icon_pixels()));
        *self.all.borrow_mut() = Some(model.clone());
        ui.set_rows(ModelRc::from(model.clone()));
        ui.set_selected(0);
        ui.invoke_scroll_to_top();
        model.request_page(0);
        self.update_status();
    }

    fn leave_all(&self) {
        if self.all.borrow_mut().take().is_some() {
            if let Some(ui) = self.ui.upgrade() {
                ui.set_rows(ModelRc::from(self.rows.clone()));
                ui.set_sort_label("Best match".into());
            }
        }
    }

    pub fn page_ready(&self, generation: u64, result: Result<Vec<Page>, everything::Error>) {
        let all = self.all.borrow().clone();
        let Some(all) = all else { return };
        if all.generation != generation {
            return;
        }
        match result {
            Ok(pages) => {
                for page in pages {
                    let (images, accent) = (self.images.borrow(), self.accent());
                    all.insert(page, &images, &accent, self.twelve_hour());
                }
                self.request_all_icons(&all);
            }
            Err(everything::Error::Superseded) => {}
            Err(error) => log(&format!("everything page: {error}")),
        }
        self.update_status();
        self.schedule_preview();
    }

    fn request_all_icons(&self, all: &AllModel) {
        let (first, count) = self.state.borrow().visible;
        let images = self.images.borrow();
        for (key, kind, path) in all.keys(first, count + 6) {
            if !images.contains_key(&key) {
                let size = if kind == KindKey::Preview { (self.icon_pixels() as f32 * 1.5) as u32 } else { self.icon_pixels() };
                self.icons.request(Request { key, path, size, kind });
            }
        }
    }
}

/// Whether words read from images can be among what this chip shows.
fn holds_images(filter: Filter) -> bool {
    matches!(filter, Filter::All | Filter::Pictures)
}

/// The toggles that narrow a search, said when it finds nothing.
fn on_toggles(matching: Matching) -> String {
    let names = [(matching.case, "Match case"), (matching.whole_word, "Whole words"), (matching.path, "Match path")];
    let names: Vec<&str> = names.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
    match names.as_slice() {
        [one] => format!("{one} is on, which may be why."),
        [first, second] => format!("{first} and {second} are on, which may be why."),
        _ => format!("{}, and {} are on, which may be why.", names[..names.len() - 1].join(", "), names[names.len() - 1]),
    }
}

fn group(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn candidate(item: &Item) -> Candidate {
    Candidate {
        content: None,
        name: item.name.clone(),
        folder: item.folder.clone(),
        is_folder: item.is_folder,
        size: item.size,
        modified: item.modified,
        opens: item.run_count,
        opened: item.date_run,
        hidden: false,
        origin: Some(Origin::Everything),
    }
}

fn image(bitmap: &winsys::icons::Bitmap) -> slint::Image {
    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&bitmap.rgba, bitmap.width, bitmap.height);
    slint::Image::from_rgba8_premultiplied(buffer)
}

fn row(candidate: &Candidate, highlights: &[std::ops::Range<usize>], accent: &str, icon: Option<slint::Image>, now: i64, twelve: bool,
       snippet: Option<&(content::Snippet, content::Source)>, found_by: Option<&'static str>) -> Row {
    let markup = format::styled_name(&candidate.name, highlights, accent);
    let name = slint::StyledText::from_markdown(&markup)
        .unwrap_or_else(|_| slint::StyledText::from_plain_text(format::display_name(&candidate.name)));
    let app = candidate.origin == Some(Origin::App);
    let detail = if app { "App".to_string() } else { format::folder(&candidate.folder, 90) };
    let mut meta = Vec::new();
    if !app {
        if let Some(size) = candidate.size {
            meta.push(format::size(size));
        }
        if let Some(modified) = candidate.modified {
            meta.push(format::when(modified, now, twelve));
        }
    }
    let (styled_detail, has_snippet, badge) = match snippet {
        Some((snippet, source)) => {
            // "Taxes · …the kitchen renovation quote…": where it is, and why it matched.
            let place = candidate.folder.rsplit('\\').next().unwrap_or("");
            // Right to left: the line is set that way from its start (a right-to-left mark), so where it is and the
            // words before the match sit at the row's right edge, as they're read.
            let rtl = right_to_left(&snippet.text);
            let mark = if rtl { "\u{200F}" } else { "" };
            // Right to left, the row is cut on its left, which is where a line's later words are: the snippet starts a
            // few words before the match, so the match is in view (an Arabic match was cut off).
            let (text, highlights) = if rtl { lead_in(&snippet.text, &snippet.highlights, RTL_LEAD) } else { (snippet.text.clone(), snippet.highlights.clone()) };
            let prefix = if place.is_empty() { mark.to_string() } else { format!("{mark}{place}  ·  ") };
            let shifted: Vec<std::ops::Range<usize>> =
                highlights.iter().map(|range| (range.start + prefix.len())..(range.end + prefix.len())).collect();
            let markup = format::styled_plain(&format!("{prefix}{text}"), &shifted, accent);
            let styled = slint::StyledText::from_markdown(&markup).unwrap_or_else(|_| slint::StyledText::from_plain_text(&snippet.text));
            // A video found by what it shows, at its moment.
            let badge = if let Some(found_by) = found_by { found_by } else { match source {
                content::Source::Text => "In text",
                content::Source::Picture => "In image",
                content::Source::Mixed => "In scan",
                content::Source::Speech => "Said",
                content::Source::Video => "In video",
            } };
            (styled, true, SharedString::from(badge))
        }
        // Found by what it shows or means, with no words to show for it.
        None => (slint::StyledText::default(), false, found_by.map(SharedString::from).unwrap_or_default()),
    };
    Row {
        name,
        plain_name: format::display_name(&candidate.name).into(),
        detail: detail.into(),
        snippet: styled_detail,
        has_snippet,
        rtl: snippet.is_some_and(|(snippet, _)| right_to_left(&snippet.text)),
        meta: meta.join("  ·  ").into(),
        badge,
        has_icon: icon.is_some(),
        icon: icon.unwrap_or_default(),
    }
}

fn is_text(name: &str) -> bool {
    let Some(extension) = search::filter::extension(name) else {
        return matches!(name, "README" | "LICENSE" | "Makefile" | "Dockerfile" | ".gitignore" | ".env.example");
    };
    let extension = extension.to_ascii_lowercase();
    search::filter::CODE.contains(&extension.as_str())
        || ["txt", "md", "markdown", "log", "csv", "tsv", "ini", "cfg", "conf", "srt", "vtt", "reg", "nfo", "rst", "adoc", "tex"]
            .contains(&extension.as_str())
}

/// Names checked against functions answered here (`width:`…), at most; with only those typed, the newest of their kind.
const EXTRA_POOL: usize = 2000;
/// Files `content:` looks through, at most.
const EXTRA_CONTENT: usize = 5000;

/// A file's size in pixels and length, read from its header (`width:`, `height:`, `length:`).
#[derive(Clone, Copy, Debug, Default)]
struct FileFacts {
    size: Option<(u32, u32)>,
    length: Option<f64>,
}

/// The list's rows replaced by `rows`, in place: a row that's the same stays as it is, and the others change where they
/// are. Replaced whole, the list makes its rows anew, and a double-click whose two clicks came either side of results
/// arriving (the image helper's, the words inside, more names) was two single clicks, so a file took 2 to 4 clicks to
/// open.
fn replace_rows(model: &VecModel<Row>, rows: Vec<Row>) {
    let old = model.row_count();
    if old == 0 || rows.is_empty() {
        model.set_vec(rows);
        return;
    }
    let common = old.min(rows.len());
    let mut rows = rows.into_iter();
    for (index, row) in rows.by_ref().take(common).enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
    for index in (common..old).rev() {
        model.remove(index);
    }
    model.extend(rows);
}

/// Whether text reads right to left, by its first letter that has a direction (Hebrew, Arabic and their kin).
/// Letters of a right-to-left snippet kept before its first match.
const RTL_LEAD: usize = 24;

/// `text` from a word boundary at most `lead` letters before its first highlight ("…" in front when cut), and the
/// highlights moved with it.
fn lead_in(text: &str, highlights: &[std::ops::Range<usize>], lead: usize) -> (String, Vec<std::ops::Range<usize>>) {
    let Some(first) = highlights.iter().map(|range| range.start).min() else { return (text.to_string(), highlights.to_vec()) };
    let before = &text[..first.min(text.len())];
    if before.chars().count() <= lead {
        return (text.to_string(), highlights.to_vec());
    }
    // `lead` letters back, then on to the next word's start.
    let back = before.char_indices().rev().nth(lead - 1).map_or(0, |(at, _)| at);
    // (Past the whole space: some are more than a byte, as a no-break space.)
    let cut = before[back..].char_indices().find(|(_, c)| c.is_whitespace())
        .map_or(back, |(space, c)| back + space + c.len_utf8()).min(first);
    let shown = format!("…{}", &text[cut..]);
    let moved = "…".len() as isize - cut as isize;
    let highlights = highlights.iter()
        .filter(|range| range.start >= cut)
        .map(|range| ((range.start as isize + moved) as usize)..((range.end as isize + moved) as usize))
        .collect();
    (shown, highlights)
}

fn right_to_left(text: &str) -> bool {
    text.chars().find(|c| c.is_alphabetic()).is_some_and(|c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

/// The preview's player (`Preview::player`): nothing to play, a video playing muted or with its sound, a recording
/// stopped or playing.
pub const PLAYER_NONE: i32 = 0;
pub const PLAYER_MUTED: i32 = 1;
pub const PLAYER_SOUND: i32 = 2;
pub const PLAYER_STOPPED: i32 = 3;
pub const PLAYER_PLAYING: i32 = 4;

/// What's playing in the preview.
#[derive(Clone, Debug, Default)]
struct Media {
    path: String,
    video: bool,
    recording: bool,
    /// Where it plays from next (the moment that matched, or where it was stopped).
    from: f64,
    /// Where it is now, and how long it is (0 until known).
    at: f64,
    length: f64,
    /// A video's sound is on, or a recording is playing.
    sound: bool,
    /// A video's secrets to cover as it plays (`Marks::video_boxes`), when they're hidden.
    secrets: Vec<[i64; 6]>,
    /// Its marks are in: it may be drawn.
    marked: bool,
    /// A video's own size in pixels, once known, and the longer side it's decoded at now.
    native: Option<(u32, u32)>,
    side: u32,
}

/// Covers a frame's secrets showing at `seconds` (a video's `Marks::video_boxes`): each box, a little larger, pixelated
/// in blocks of half its height.
fn cover_secrets(picture: &mut winsys::imaging::Rgb, secrets: &[[i64; 6]], seconds: f64) {
    let ms = (seconds * 1000.0) as i64;
    let (width, height) = (picture.width as i64, picture.height as i64);
    for secret in secrets.iter().filter(|secret| secret[0] <= ms && ms <= secret[1] + 250) {
        let (left, top, right, bottom) = (secret[2] * width / 10_000, secret[3] * height / 10_000, secret[4] * width / 10_000, secret[5] * height / 10_000);
        let pad = ((bottom - top) / 4).max(2);
        let (left, top, right, bottom) = ((left - pad).max(0), (top - pad).max(0), (right + pad).min(width), (bottom + pad).min(height));
        let block = ((bottom - top) / 2).max(4);
        let mut y = top;
        while y < bottom {
            let mut x = left;
            while x < right {
                let (x1, y1) = ((x + block).min(right), (y + block).min(bottom));
                let mut sum = [0u64; 3];
                let mut count = 0u64;
                for row in y..y1 {
                    for column in x..x1 {
                        let at = ((row * width + column) * 3) as usize;
                        for c in 0..3 {
                            sum[c] += u64::from(picture.pixels[at + c]);
                        }
                        count += 1;
                    }
                }
                let mean = sum.map(|value| (value / count.max(1)) as u8);
                for row in y..y1 {
                    for column in x..x1 {
                        let at = ((row * width + column) * 3) as usize;
                        picture.pixels[at..at + 3].copy_from_slice(&mean);
                    }
                }
                x = x1;
            }
            y += block;
        }
    }
}

/// "1:23", "1:02:03".
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

fn is_recording(name: &str) -> bool {
    search::filter::extension(name).is_some_and(|extension| search::filter::AUDIO.contains(&extension.to_ascii_lowercase().as_str()))
}

/// What a kind is called in "No video has the words…".
fn kind_noun(filter: Filter, extension: Option<&str>) -> String {
    if let Some(extension) = extension {
        return extension.to_uppercase();
    }
    match filter {
        Filter::Video => "video",
        Filter::Audio => "recording",
        Filter::Documents => "document",
        Filter::Code => "code file",
        Filter::Pictures => "image",
        _ => "file",
    }
    .into()
}

fn is_pdf(name: &str) -> bool {
    search::filter::extension(name).is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn is_video(name: &str) -> bool {
    search::filter::extension(name).is_some_and(|extension| search::filter::VIDEO.contains(&extension.to_ascii_lowercase().as_str()))
}

fn is_picture(name: &str) -> bool {
    search::filter::extension(name).is_some_and(|extension| extract_pictures().contains(&extension.to_ascii_lowercase().as_str()))
}

/// The pictures the indexer reads (see the extract crate's list; kept in step here without depending on it).
fn extract_pictures() -> &'static [&'static str] {
    &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "gif", "heic", "heif"]
}

fn is_document(name: &str) -> bool {
    search::filter::extension(name).is_some_and(|extension| {
        ["pdf", "docx", "docm", "xlsx", "xlsm", "pptx", "pptm", "odt", "ods", "odp", "epub", "eml", "msg", "rtf", "html", "htm", "ipynb"]
            .contains(&extension.to_ascii_lowercase().as_str())
    })
}

/// "A password is hidden", "A card number and an API key are hidden".
fn describe_hidden(kinds: &[String]) -> String {
    let mut names: Vec<&str> = kinds.iter().map(|kind| match kind.as_str() {
        "card" => "a card number",
        "iban" => "an IBAN",
        "id" => "an ID number",
        "password" => "a password",
        "key" => "an API key",
        "private-key" => "a private key",
        "document" => "a passport or ID",
        "seed" => "a recovery phrase",
        _ => "a secret",
    }).collect();
    names.dedup();
    let list = match names.len() {
        0 => return String::new(),
        1 => names[0].to_string(),
        2 => format!("{} and {}", names[0], names[1]),
        _ => format!("{}, and {}", names[..names.len() - 1].join(", "), names[names.len() - 1]),
    };
    let mut sentence = format!("{list} {} hidden", if names.len() == 1 { "is" } else { "are" });
    sentence[..1].make_ascii_uppercase();
    sentence
}

fn first_lines(text: &str, lines: usize) -> String {
    text.lines().take(lines).map(|line| {
        let line = line.replace('\t', "    ");
        if line.chars().count() > 160 { line.chars().take(160).collect() } else { line }
    }).collect::<Vec<_>>().join("\n")
}

/// The hot tier's sources on this PC.
fn hot_sources(settings: &Settings, history: Vec<(String, u32, i64)>) -> Sources {
    let programs = [Known::Programs, Known::CommonPrograms].into_iter().filter_map(winsys::files::known).collect();
    let mut recent = Vec::new();
    if let Some(folder) = winsys::files::known(Known::Recent) {
        let mut links: Vec<(std::path::PathBuf, i64)> = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")))
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
                Some((entry.path(), modified))
            })
            .collect();
        links.sort_by(|a, b| b.1.cmp(&a.1));
        for (link, opened) in links.into_iter().take(200) {
            if let Some(target) = winsys::files::shortcut_target(&link.display().to_string()) {
                recent.push((target, opened));
            }
        }
    }
    let mut folders: Vec<std::path::PathBuf> = [Known::Desktop, Known::Documents, Known::Downloads, Known::Pictures, Known::Videos, Known::Music]
        .into_iter()
        .filter_map(winsys::files::known)
        .collect();
    if let Some(onedrive) = std::env::var_os("OneDrive").map(std::path::PathBuf::from) {
        if !folders.iter().any(|folder| folder.starts_with(&onedrive)) {
            folders.push(onedrive);
        }
    }
    // For tests in a sandbox (`EXCAVATE_TEST_FOLDERS`, separated by ;): only those folders, and none of the person's
    // recent files, so a test popup never shows a file of theirs.
    if let Some(only) = std::env::var_os("EXCAVATE_TEST_FOLDERS") {
        let only = only.to_string_lossy().split(';').filter(|folder| !folder.is_empty()).map(std::path::PathBuf::from).collect();
        return Sources { programs, recent: Vec::new(), history, folders: only, depth: settings.folder_depth, cap: settings.folder_cap };
    }
    Sources { programs, recent, history, folders, depth: settings.folder_depth, cap: settings.folder_cap }
}

/// Every Everything result in one order, as a virtual list: rows are fetched a page at a time when they come into
/// view, so a list of ten million costs the same as a list of ten.
pub struct AllModel {
    terms: Terms,
    filter: Filter,
    scope: Scope,
    sort: Sort,
    pub generation: u64,
    everything: Arc<Everything>,
    icon_size: u32,
    total: Cell<u64>,
    pages: RefCell<HashMap<u32, Vec<(Item, Row, String, KindKey)>>>,
    requested: RefCell<HashSet<u32>>,
    notify: ModelNotify,
}

const PAGE: u32 = 100;

impl AllModel {
    fn new(terms: Terms, filter: Filter, scope: Scope, sort: Sort, generation: u64, everything: Arc<Everything>, icon_size: u32) -> AllModel {
        AllModel {
            terms, filter, scope, sort, generation, everything, icon_size,
            total: Cell::new(0),
            pages: RefCell::new(HashMap::new()),
            requested: RefCell::new(HashSet::new()),
            notify: ModelNotify::default(),
        }
    }

    pub fn total(&self) -> u64 {
        self.total.get()
    }

    fn request_page(&self, page: u32) {
        if !self.requested.borrow_mut().insert(page) {
            return;
        }
        let query = plan::exhaustive(&self.terms, self.filter, &self.scope, self.sort, page * PAGE, PAGE);
        let generation = self.generation;
        self.everything.search(generation, vec![query], move |result| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.page_ready(generation, result)));
        });
    }

    fn insert(&self, page: Page, images: &HashMap<String, slint::Image>, accent: &str, twelve: bool) {
        let first_load = self.total.get() == 0 && self.pages.borrow().is_empty();
        let total = page.total.min(u64::from(u32::MAX));
        let number = page.offset / PAGE;
        let now = now();
        let rows: Vec<(Item, Row, String, KindKey)> = page.items.into_iter().map(|item| {
            let candidate = candidate(&item);
            let path = item.path();
            let (key, kind) = icons::key(&path, &item.name, item.is_folder, self.icon_size);
            let row = row(&candidate, &item.highlights, accent, images.get(&key).cloned(), now, twelve, None, None);
            (item, row, key, kind)
        }).collect();
        let count = rows.len();
        self.pages.borrow_mut().insert(number, rows);
        if first_load || self.total.get() != total {
            self.total.set(total);
            self.notify.reset();
        } else {
            for index in 0..count {
                self.notify.row_changed((number * PAGE) as usize + index);
            }
        }
    }

    fn target(&self, index: usize) -> Option<Target> {
        let pages = self.pages.borrow();
        let page = pages.get(&((index as u32) / PAGE))?;
        page.get(index % PAGE as usize).map(|(item, ..)| Target::from(item))
    }

    fn keys(&self, first: usize, count: usize) -> Vec<(String, KindKey, String)> {
        let pages = self.pages.borrow();
        (first..first + count).filter_map(|index| {
            let page = pages.get(&((index as u32) / PAGE))?;
            page.get(index % PAGE as usize).map(|(item, _, key, kind)| (key.clone(), *kind, item.path()))
        }).collect()
    }

    fn icon_ready(&self, key: &str, image: &slint::Image) {
        let mut changed = Vec::new();
        for (number, page) in self.pages.borrow_mut().iter_mut() {
            for (index, (_, row, row_key, _)) in page.iter_mut().enumerate() {
                if row_key == key {
                    row.icon = image.clone();
                    row.has_icon = true;
                    changed.push((number * PAGE) as usize + index);
                }
            }
        }
        for index in changed {
            self.notify.row_changed(index);
        }
    }
}

impl Model for AllModel {
    type Data = Row;

    fn row_count(&self) -> usize {
        self.total.get() as usize
    }

    fn row_data(&self, row: usize) -> Option<Row> {
        let number = (row as u32) / PAGE;
        if let Some(page) = self.pages.borrow().get(&number) {
            return page.get(row % PAGE as usize).map(|(_, row, ..)| row.clone());
        }
        self.request_page(number);
        Some(Row { plain_name: "…".into(), name: slint::StyledText::from_plain_text("…"), ..Row::default() })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

#[cfg(test)]
mod rtl_tests {
    #[test]
    fn right_to_left_snippets_start_near_the_match() {
        let text = "هذا نص طويل جدا قبل الكلمة التي نبحث عنها في هذا السطر الطويل جدا كلمة مهمة هنا";
        let at = text.find("مهمة").unwrap();
        let (shown, highlights) = super::lead_in(text, &[at..at + "مهمة".len()], 24);
        assert!(shown.starts_with('…'), "{shown}");
        assert_eq!(&shown[highlights[0].clone()], "مهمة");
        assert!(shown[..highlights[0].start].chars().count() <= 26, "{shown}");
        // Near the start already: as it was.
        let (same, _) = super::lead_in("كلمة مهمة", &[0..8], 24);
        assert_eq!(same, "كلمة مهمة");
    }
}
