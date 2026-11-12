//! Settings (a small JSON file people can also edit by hand), where Excavate keeps its files, its log, and what's
//! left out of reading. Shared by the app and the indexer, which read the same file.

pub mod devices;
pub mod status;

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    /// The global shortcut that opens the popup ("Alt+S", "Ctrl+Shift+Space", "Win+F1"…).
    pub hotkey: String,
    /// Light or dark: "system" (like Windows), "light" or "dark".
    pub theme: String,
    /// The theme's colors: one of the nine ("Lantern", "Tarp"…) or one made in the Workshop.
    pub theme_name: String,
    /// Themes made in the Workshop.
    pub own_themes: Vec<OwnTheme>,
    /// The preview pane beside the results (Ctrl+P toggles it).
    pub preview: bool,
    /// The search field's toggles for how names match, as in Everything: they stay as they're left.
    pub match_case: bool,
    pub match_whole_word: bool,
    pub match_path: bool,
    pub match_regex: bool,
    /// The popup opens empty, on the All chip and Best match, rather than where it was left.
    pub new_search_each_time: bool,
    /// Experimental: "> cake" searches the text in images, "< cake" what images show (off: Everything's < > group).
    pub typed_toggles: bool,
    /// Experimental: names are found by Excavate's own index of the drives instead of Everything (`engine`).
    pub own_index: bool,
    /// Excavate asked Everything to exit when its own index was chosen: it starts it again when Everything is chosen.
    pub everything_closed: bool,
    /// The popup's size in logical pixels, so it means the same on a monitor of any scale.
    pub popup_width: u32,
    pub popup_height: u32,
    /// Windows 11's acrylic behind the popup.
    pub acrylic: bool,
    /// Slint renderer: "femtovg" (GPU, OpenGL) or "software".
    pub renderer: String,
    /// How times read: "windows" (as Windows has them), "12-hour" or "24-hour".
    pub time_format: String,
    /// The monitor Excavate opens on: an ID that outlasts Windows renumbering it. Empty: the one the mouse is on.
    pub monitor: String,
    /// What that monitor is called, to say so while it's unplugged.
    pub monitor_name: String,
    /// The graphics card every model runs on (`winsys::gpu::choose`): empty for the best one Excavate finds (a card
    /// of its own over a processor's graphics), "none" for the processor alone, or a card's name as Windows gives it.
    pub graphics_card: String,
    /// Results the ranked view asks Everything for.
    pub ranked_results: u32,
    /// Hot tier: how deep into the person's folders, and how many entries at most.
    pub folder_depth: usize,
    pub folder_cap: usize,
    /// Set after the first run (when start-with-Windows is first turned on).
    pub first_run_done: bool,

    /// Onboarding finished. Nothing is read before.
    pub setup_done: bool,
    /// Folders whose contents are read: the words in documents, code and pictures.
    pub folders: Vec<String>,
    /// Folders listed on the Folders page but not read (unchecked). None in settings saved before every folder could
    /// be removed: the page then lists Windows' four folders as it did, those not read unchecked.
    pub folders_off: Option<Vec<String>>,
    /// Left out of reading: folders (full paths) and names, with * and ? ("node_modules", "*.tmp").
    pub exclude: Vec<String>,
    /// How text in pictures is read: "quick", "balanced" or "deep".
    pub reading: String,
    /// Whether text in pictures is read at all (the choice above is kept while it's off).
    pub pictures_on: bool,
    /// Set by "Re-read my pictures" (seconds since 1970): pictures read before are read again.
    pub reread_pictures_at: i64,
    /// Whether images are found by what they show (the choice below is kept while it's off).
    pub seeing_on: bool,
    /// How: "quick", "balanced" or "deep" (Thorough).
    pub seeing: String,
    /// Thorough's second model: "siglip" (a little better) or "qwen" (most accurate).
    pub seeing_deep: String,
    /// Set by "Look at my images again" (seconds since 1970): images looked at before are looked at again.
    pub relook_images_at: i64,
    /// How long Everything took to make its list of files again when it was last fixed, in seconds (0: never).
    pub everything_rebuild_seconds: u32,
    /// Why the choice for what images show changed by itself (Qwen's 8B didn't suit this PC), until it's changed again.
    pub seeing_note: String,
    /// When new pictures are read: "arrive", "scheduled" or "manual". Documents never wait.
    pub schedule: String,
    /// How often when scheduled: "day", "week" or "month".
    pub schedule_every: String,
    /// At what time of day when scheduled ("03:00").
    pub schedule_at: String,
    /// Set by "Read new pictures now" (seconds since 1970).
    pub read_pictures_at: i64,
    /// The version whose launch animation has played.
    pub intro_version: String,
    /// Passwords, card numbers, IDs and keys are blurred in previews until revealed.
    pub blur_secrets: bool,
    /// Every preview is blurred while the screen is shared or presenting.
    pub blur_when_sharing: bool,
    /// Excavate asks its GitHub page once a day for a newer version, and gets it ready for the next start (About). Off
    /// until turned on: nothing else is downloaded but the models chosen.
    pub auto_update: bool,
    /// When updates were last looked for (seconds since 1970).
    pub update_checked_at: i64,
    /// Keep reading while on battery power.
    pub read_on_battery: bool,
    /// Where names are searched: "everything" (its whole index) or "folders" (only `name_folders`). A filter on Excavate's
    /// own searches: Everything goes on indexing everything.
    pub names_scope: String,
    pub name_folders: Vec<String>,
    /// With names limited to folders: apps are left out too, rather than showing from anywhere.
    pub names_leave_out_apps: bool,
    /// Reading paused by the person: until this time (seconds since 1970), or -1 until turned back on. 0: not paused.
    pub paused_until: i64,
    /// Set by "Read everything now" (seconds since 1970), 0 by "Stop": every image in one sitting, without waiting for a
    /// quiet moment.
    pub read_all_at: i64,
    /// The same for finding images by what they show, apart from reading their text: when new images are looked at,
    /// "Look at new images now", "Read everything now", and whether on battery power.
    pub seeing_schedule: String,
    pub seeing_schedule_every: String,
    pub seeing_schedule_at: String,
    pub look_new_at: i64,
    pub look_all_at: i64,
    pub look_on_battery: bool,
    /// The size of the search window's preview beside the results: "small", "medium" or "large" (`preview` shows it).
    pub preview_size: String,
    /// The preview as last dragged by its edge or its image's corner, in logical pixels: its width and its image's
    /// height. 0: as `preview_size` has it; choosing a
    /// size in Settings, or a double-click on the edge or the corner, goes back to that.
    pub preview_dragged_width: f32,
    pub preview_dragged_image: f32,
    /// Whether recordings are found by what's said in them (the choice below is kept while it's off).
    pub listening_on: bool,
    /// How: "quick" (Parakeet), "balanced" (Whisper turbo, on the graphics card when there is one) or "deep" (Thorough:
    /// Whisper large-v3, on the graphics card).
    pub listening: String,
    /// How the graphics card runs them: "vulkan" (any card) or "cuda" (NVIDIA's own, a larger download).
    pub listening_gpu: String,
    /// Where each choice runs, chosen on its card: "cpu", "vulkan" or "cuda", by the choice's id ("quick", "balanced",
    /// "deep"). A choice not in it runs where `listen_device` says by default.
    pub listening_devices: std::collections::BTreeMap<String, String>,
    /// Where each Images choice runs, chosen on its card: "cpu" or "gpu", by the choice's id; for reading text in
    /// images and for what they show. A choice not in it runs where `reading_device` / `seeing_device` says by default.
    pub reading_devices: std::collections::BTreeMap<String, String>,
    pub seeing_devices: std::collections::BTreeMap<String, String>,
    /// Kinds left out of particular chosen folders, by the folder (lowercased): any of `FOLDER_KINDS`. Every kind is
    /// read in every folder unless it's here (and on, on its own page).
    pub folder_kinds: std::collections::BTreeMap<String, Vec<String>>,
    /// Whether videos are looked at for what they show (by the models chosen for images), and how closely: "quick",
    /// "balanced" or "deep" (Thorough). "Look at my videos again" sets `relook_videos_at`.
    pub videos_on: bool,
    pub video: String,
    pub relook_videos_at: i64,
    /// Whether the text videos show is read (slides, screen recordings, captions, signs), each line at its moment, at
    /// its own pace: "quick", "balanced" or "deep" (Thorough). Settings from before it had its own follow the Videos
    /// choice until one is made (`video_text_choice`).
    pub video_text: bool,
    pub video_text_pace: Option<String>,
    /// Whether what's said in videos is heard, by the listener chosen for recordings (`listening`, on its device), on
    /// the recordings' schedule, after them; kept apart from recordings in the index and on Activity.
    pub video_speech: bool,
    /// Set by "Listen to my recordings again" (seconds since 1970): recordings listened to before are done again.
    pub relisten_at: i64,
    /// When new recordings are listened to, "Listen to everything now", and whether on battery power, as for images.
    pub listen_schedule: String,
    pub listen_schedule_every: String,
    pub listen_schedule_at: String,
    pub listen_new_at: i64,
    pub listen_all_at: i64,
    pub listen_on_battery: bool,
}

/// A theme made in the Workshop: its accent's hue, and its surfaces ("warm", "plain" or "cool").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct OwnTheme {
    pub name: String,
    pub hue: u16,
    pub surfaces: String,
}

impl Default for OwnTheme {
    fn default() -> OwnTheme {
        OwnTheme { name: "My theme".into(), hue: 200, surfaces: "warm".into() }
    }
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            hotkey: "Alt+S".into(),
            theme: "system".into(),
            theme_name: "Lantern".into(),
            own_themes: Vec::new(),
            preview: true,
            match_case: false,
            match_whole_word: false,
            match_path: false,
            match_regex: false,
            new_search_each_time: true,
            typed_toggles: false,
            own_index: false,
            everything_closed: false,
            popup_width: POPUP_SIZES[1].1,
            popup_height: POPUP_SIZES[1].2,
            acrylic: false,
            renderer: "software".into(),
            time_format: "windows".into(),
            monitor: String::new(),
            graphics_card: String::new(),
            monitor_name: String::new(),
            ranked_results: 300,
            folder_depth: 4,
            folder_cap: 60_000,
            first_run_done: false,
            setup_done: false,
            folders: Vec::new(),
            folders_off: None,
            exclude: DEFAULT_EXCLUDE.iter().map(|name| name.to_string()).collect(),
            reading: "balanced".into(),
            pictures_on: true,
            reread_pictures_at: 0,
            seeing_on: true,
            seeing: "balanced".into(),
            seeing_deep: "siglip".into(),
            relook_images_at: 0,
            everything_rebuild_seconds: 0,
            seeing_note: String::new(),
            schedule: "arrive".into(),
            schedule_every: "day".into(),
            schedule_at: "03:00".into(),
            read_pictures_at: 0,
            intro_version: String::new(),
            blur_secrets: true,
            blur_when_sharing: true,
            auto_update: false,
            update_checked_at: 0,
            read_on_battery: false,
            names_scope: "everything".into(),
            name_folders: Vec::new(),
            names_leave_out_apps: false,
            paused_until: 0,
            read_all_at: 0,
            seeing_schedule: "arrive".into(),
            seeing_schedule_every: "day".into(),
            seeing_schedule_at: "03:00".into(),
            look_new_at: 0,
            look_all_at: 0,
            look_on_battery: false,
            preview_size: "medium".into(),
            preview_dragged_width: 0.0,
            preview_dragged_image: 0.0,
            listening_on: true,
            listening: "balanced".into(),
            listening_gpu: "vulkan".into(),
            listening_devices: std::collections::BTreeMap::new(),
            reading_devices: std::collections::BTreeMap::new(),
            seeing_devices: std::collections::BTreeMap::new(),
            folder_kinds: std::collections::BTreeMap::new(),
            videos_on: true,
            video: "balanced".into(),
            relook_videos_at: 0,
            video_text: true,
            video_speech: true,
            video_text_pace: None,
            relisten_at: 0,
            listen_schedule: "arrive".into(),
            listen_schedule_every: "day".into(),
            listen_schedule_at: "03:00".into(),
            listen_new_at: 0,
            listen_all_at: 0,
            listen_on_battery: false,
        }
    }
}

/// The popup's sizes to choose from, in logical pixels.
pub const POPUP_SIZES: [(&str, u32, u32); 3] = [("Small", 800, 500), ("Medium", 960, 600), ("Large", 1200, 750)];

/// A system-wide name for one of Excavate's objects (a mutex, an event): `Local\Excavate.<what>`, or with a suffix
/// of its own under `EXCAVATE_HOME`, so a sandboxed Excavate runs beside the real one without either hearing the other.
pub fn instance_name(what: &str) -> String {
    match std::env::var("EXCAVATE_HOME") {
        Ok(home) => {
            let hash = home.to_lowercase().bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
            format!(r"Local\Excavate.{what}.{hash:x}")
        }
        Err(_) => format!(r"Local\Excavate.{what}"),
    }
}

/// The named events Excavate signals its indexer with: finish and stop; settings changed (start over with them).
pub fn indexer_quit_event() -> String {
    instance_name("Indexer.Quit")
}

pub fn indexer_settings_event() -> String {
    instance_name("Indexer.Settings")
}

/// "Skip": the file named in `Paths::skip` is let go, if it's being read now (Activity's "Right now").
pub fn indexer_skip_event() -> String {
    instance_name("Indexer.Skip")
}

/// Whether the listener's CUDA add-on is published (indexer/src/models.rs, `LISTEN_CUDA`): settings offers CUDA then.
pub const LISTEN_CUDA_PUBLISHED: bool = true;

/// Folders that hold what programs download, build or cache, not what people write: left out unless removed.
pub const DEFAULT_EXCLUDE: &[&str] = &[
    "node_modules", ".git", ".svn", ".hg", "__pycache__", ".venv", "venv", "site-packages", ".gradle", ".idea", ".vs",
    ".cache", ".next", ".nuxt", "target", "build", "dist", "obj", "bin", "vendor", "AppData", "$RECYCLE.BIN",
    "System Volume Information", "*.tmp", "~$*",
];

/// How text in pictures is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Off,
    /// PP-OCRv6 tiny on the CPU.
    Quick,
    /// PP-OCRv6 small on the CPU.
    Balanced,
    /// Balanced first, then a vision-language model on the GPU.
    Deep,
}

impl Reading {
    pub fn parse(text: &str) -> Reading {
        match text.to_ascii_lowercase().as_str() {
            "off" => Reading::Off,
            "quick" => Reading::Quick,
            "deep" | "thorough" => Reading::Deep,
            _ => Reading::Balanced,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Reading::Off => "off",
            Reading::Quick => "quick",
            Reading::Balanced => "balanced",
            Reading::Deep => "deep",
        }
    }
}

/// When new pictures are read (documents never wait).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schedule {
    Arrive,
    Scheduled,
    Manual,
}

impl Schedule {
    pub fn of(text: &str) -> Schedule {
        match text {
            "scheduled" => Schedule::Scheduled,
            "manual" => Schedule::Manual,
            _ => Schedule::Arrive,
        }
    }
}

/// "03:00" as minutes after midnight.
fn minutes_of(at: &str) -> u32 {
    let (hours, minutes) = at.split_once(':').unwrap_or(("3", "0"));
    let (hours, minutes): (u32, u32) = (hours.trim().parse().unwrap_or(3), minutes.trim().parse().unwrap_or(0));
    hours.min(23) * 60 + minutes.min(59)
}

/// What can be left out of a chosen folder: the text in its images, what its images show, the text in its videos, what
/// its videos show, what's said in its videos, and what's said in its recordings.
pub const FOLDER_KINDS: [&str; 6] = ["image-text", "image-shown", "video-text", "video-shown", "video-said", "audio"];

/// The video kinds as bits, as the indexer's catalog keeps them with each video.
pub mod video_kind {
    pub const TEXT: u8 = 1;
    pub const SHOWN: u8 = 2;
    pub const SAID: u8 = 4;
    pub const ALL: u8 = 7;
}

/// How a folder is keyed in `folder_kinds`.
pub fn folder_key(folder: &str) -> String {
    folder.trim_end_matches('\\').to_lowercase()
}

/// The chosen folders with the kinds each leaves out.
#[derive(Clone, Debug, Default)]
pub struct FolderKinds {
    folders: Vec<(String, Vec<String>)>,
}

impl FolderKinds {
    /// Whether a kind is read for this path: the innermost chosen folder holding it decides.
    pub fn on(&self, path: &str, kind: &str) -> bool {
        let lower = folder_key(path);
        let holds = |folder: &&(String, Vec<String>)| lower == folder.0 || lower.strip_prefix(folder.0.as_str()).is_some_and(|rest| rest.starts_with('\\'));
        self.folders.iter().filter(holds).max_by_key(|folder| folder.0.len()).is_none_or(|folder| !folder.1.iter().any(|off| off == kind))
    }

    /// The video kinds read for this path, as `video_kind` bits.
    pub fn video_bits(&self, path: &str) -> u8 {
        [("video-text", video_kind::TEXT), ("video-shown", video_kind::SHOWN), ("video-said", video_kind::SAID)]
            .into_iter()
            .filter(|(kind, _)| self.on(path, kind))
            .fold(0, |bits, (_, bit)| bits | bit)
    }

    /// The chosen folders (lowercased) that leave a kind out.
    pub fn off(&self, kind: &str) -> Vec<String> {
        self.folders.iter().filter(|folder| folder.1.iter().any(|off| off == kind)).map(|folder| folder.0.clone()).collect()
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Settings {
        let Ok(bytes) = std::fs::read(paths.settings()) else { return Settings::default() };
        match serde_json::from_slice::<Settings>(&bytes) {
            Ok(settings) => settings,
            Err(error) => {
                log(&format!("settings.json: {error}; using the defaults"));
                Settings::default()
            }
        }
    }

    pub fn save(&self, paths: &Paths) {
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let temporary = paths.settings().with_extension("json.tmp");
            if std::fs::write(&temporary, json).is_ok() {
                let _ = std::fs::rename(&temporary, paths.settings());
            }
        }
    }

    pub fn reading(&self) -> Reading {
        if self.pictures_on { Reading::parse(&self.reading) } else { Reading::Off }
    }

    pub fn schedule(&self) -> Schedule {
        Schedule::of(&self.schedule)
    }

    /// The scheduled time as minutes after midnight.
    pub fn schedule_minutes(&self) -> u32 {
        minutes_of(&self.schedule_at)
    }

    /// When new images are looked at for what they show.
    pub fn seeing_schedule(&self) -> Schedule {
        Schedule::of(&self.seeing_schedule)
    }

    pub fn seeing_schedule_minutes(&self) -> u32 {
        minutes_of(&self.seeing_schedule_at)
    }

    pub fn listen_schedule(&self) -> Schedule {
        Schedule::of(&self.listen_schedule)
    }

    pub fn listen_schedule_minutes(&self) -> u32 {
        minutes_of(&self.listen_schedule_at)
    }

    /// Where a listening choice runs on this PC: "cpu", "vulkan" or "cuda". `card` says there's a graphics card able
    /// to, `cuda` that CUDA can be had for it (NVIDIA's, and the add-on published). As chosen on its card; else where
    /// it was measured fastest on this PC (`measured`); else Quick on the processor and the others on the graphics
    /// card. Thorough never on the processor.
    pub fn listen_device(&self, choice: &str, card: bool, cuda: bool, measured: &devices::Measured) -> &'static str {
        let gpu_way = |way: &str| if way == "cuda" && cuda { "cuda" } else { "vulkan" };
        if !card {
            return "cpu";
        }
        let chosen = self.listening_devices.get(choice).map(String::as_str);
        match chosen.or_else(|| measured.fastest(&format!("listening:{choice}"))) {
            Some("cpu") if choice != "deep" => "cpu",
            Some(way @ ("vulkan" | "cuda")) => gpu_way(way),
            _ if choice == "quick" => "cpu",
            _ => gpu_way(&self.listening_gpu),
        }
    }

    /// Where a choice for reading text in images runs: "cpu" or "gpu". `card`: there's a graphics card able to.
    /// PP-OCRv6 on the processor unless Balanced's card chose the graphics card: through DirectML it's about twice as
    /// fast, but holds on to memory for each new image size (measured on an RTX 4080 Laptop: 2.4 GB of memory and 4.6
    /// GB of the card's for 60 images, against 215 MB on the processor), so it's never the default nor measured
    /// (an old measurement in devices.json is ignored). Quick's is on the processor only. Thorough's second read on the
    /// graphics card unless chosen otherwise (27 to 38 s an image on the processor).
    pub fn reading_device(&self, choice: &str, card: bool) -> &'static str {
        let chosen = self.reading_devices.get(choice).map(String::as_str);
        match choice {
            "quick" => "cpu",
            "deep" if chosen == Some("cpu") || !card => "cpu",
            "deep" => "gpu",
            _ if chosen == Some("gpu") && card => "gpu",
            _ => "cpu",
        }
    }

    /// Where a choice for finding images by what they show runs: "cpu" or "gpu". By default on the graphics card when
    /// there's one, where each was fastest: SigLIP 2 (Quick) 19 ms an image through DirectML against 207 on a busy
    /// processor, EmbeddingGemma 2 133 ms against 3.2 s.
    ///
    /// Without a choice on its card, where its first model was measured fastest on this PC (`measured`) comes first.
    pub fn seeing_device(&self, choice: &str, card: bool, measured: &devices::Measured) -> &'static str {
        let chosen = self.seeing_devices.get(choice).map(String::as_str);
        match chosen.or_else(|| measured.fastest(devices::seeing_key(choice))) {
            Some("gpu") if card => "gpu",
            Some("cpu") => "cpu",
            _ if card => "gpu",
            _ => "cpu",
        }
    }

    /// How often a video's frames are looked at (seconds), and at most how many (a longer video's are further apart).
    pub fn video_pace(&self) -> (f64, f64) {
        match self.video.as_str() {
            "quick" => (30.0, 120.0),
            "deep" => (2.0, 1800.0),
            _ => (5.0, 720.0),
        }
    }

    /// How closely text in videos is read: its own choice, or the Videos choice for settings from before it had one.
    pub fn video_text_choice(&self) -> &str {
        self.video_text_pace.as_deref().unwrap_or(&self.video)
    }

    /// How often a video is looked at for new text (seconds): its keyframes, and the frames between two whose text
    /// differs.
    pub fn video_text_every(&self) -> f64 {
        match self.video_text_choice() {
            "quick" => 4.0,
            "deep" => 1.0,
            _ => 2.0,
        }
    }

    /// Which kinds each chosen folder leaves out.
    pub fn kinds(&self) -> FolderKinds {
        let folders = self.folders.iter().map(|folder| {
            let key = folder_key(folder);
            let off = self.folder_kinds.get(&key).cloned().unwrap_or_default();
            (key, off)
        });
        FolderKinds { folders: folders.collect() }
    }

    pub fn exclusions(&self) -> Exclusions {
        Exclusions::new(&self.exclude)
    }

    /// The folders names are limited to, when they are.
    pub fn names_within(&self) -> &[String] {
        if self.names_scope == "folders" { &self.name_folders } else { &[] }
    }

    /// The popup's size in words: "Medium · 960 × 600", or "Custom · 1100 × 700" for one dragged by hand.
    pub fn popup_size_label(&self) -> String {
        let (width, height) = (self.popup_width, self.popup_height);
        let name = POPUP_SIZES.iter().find(|size| (size.1, size.2) == (width, height)).map_or("Custom", |size| size.0);
        format!("{name} \u{b7} {width} \u{d7} {height}")
    }

    /// Whether the person has paused reading, `now` (seconds since 1970).
    pub fn paused(&self, now: i64) -> bool {
        self.paused_until == -1 || self.paused_until > now
    }

    /// Whether times read "2:30 PM". `windows`: whether Windows has them that way.
    pub fn twelve_hour(&self, windows: bool) -> bool {
        match self.time_format.as_str() {
            "12-hour" => true,
            "24-hour" => false,
            _ => windows,
        }
    }

    /// What the pause says: "Paused until 4:30 PM", "Paused". `offset`: local time minus UTC, in seconds.
    pub fn pause_text(&self, offset: i64, twelve: bool) -> String {
        if self.paused_until <= 0 {
            return "Paused".into();
        }
        format!("Paused until {}", clock_at(self.paused_until, offset, twelve))
    }
}

/// A time of day from minutes after midnight: "14:30", or "2:30 PM".
pub fn clock(minutes: i64, twelve: bool) -> String {
    let (hours, minutes) = (minutes.div_euclid(60).rem_euclid(24), minutes.rem_euclid(60));
    if !twelve {
        return format!("{hours:02}:{minutes:02}");
    }
    let hour = if hours % 12 == 0 { 12 } else { hours % 12 };
    format!("{hour}:{minutes:02} {}", if hours < 12 { "AM" } else { "PM" })
}

/// A moment (seconds since 1970) as its time of day. `offset`: local time minus UTC, in seconds.
pub fn clock_at(seconds: i64, offset: i64, twelve: bool) -> String {
    clock((seconds + offset).rem_euclid(86_400) / 60, twelve)
}

/// When "until tomorrow" ends: the next 06:00, at least an hour away. `offset`: local time minus UTC, in seconds.
pub fn tomorrow_morning(now: i64, offset: i64) -> i64 {
    let local = now + offset;
    let morning = local.div_euclid(86_400) * 86_400 + 6 * 3600;
    let next = if morning >= local + 3600 { morning } else { morning + 86_400 };
    next - offset
}

#[derive(Clone, Debug)]
pub struct Paths {
    /// %APPDATA%\Excavate: settings and history (they roam with the profile).
    pub roaming: PathBuf,
    /// %LOCALAPPDATA%\Excavate: the index, models, logs.
    pub local: PathBuf,
}

impl Default for Paths {
    fn default() -> Paths {
        Paths::new()
    }
}

impl Paths {
    /// The usual places, or everything under `EXCAVATE_HOME` when it's set (tests, benchmarks, a second profile).
    pub fn new() -> Paths {
        if let Some(home) = std::env::var_os("EXCAVATE_HOME").map(PathBuf::from) {
            let paths = Paths { roaming: home.join("roaming"), local: home.join("local") };
            let _ = std::fs::create_dir_all(&paths.roaming);
            let _ = std::fs::create_dir_all(paths.local.join("logs"));
            return paths;
        }
        let roaming = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Excavate");
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Excavate");
        let _ = std::fs::create_dir_all(&roaming);
        let _ = std::fs::create_dir_all(local.join("logs"));
        Paths { roaming, local }
    }

    pub fn settings(&self) -> PathBuf {
        self.roaming.join("settings.json")
    }

    /// Updates downloaded and checked, each in a folder named for its version, until they're used.
    pub fn updates(&self) -> PathBuf {
        self.local.join("updates")
    }

    /// Where each model runs fastest on this PC (`devices::Measured`): local, as it's this PC's.
    pub fn devices(&self) -> PathBuf {
        self.local.join("devices.json")
    }

    pub fn history(&self) -> PathBuf {
        self.roaming.join("history.json")
    }

    pub fn log(&self) -> PathBuf {
        self.local.join("logs").join(format!("{}.log", log_name()))
    }

    /// The full-text index (tantivy), written by the indexer, read by the popup.
    pub fn index(&self) -> PathBuf {
        self.local.join("index")
    }

    /// What the indexer knows about each file (SQLite).
    pub fn catalog(&self) -> PathBuf {
        self.local.join("catalog.db")
    }

    /// Downloaded models, one folder each.
    pub fn models(&self) -> PathBuf {
        self.local.join("models")
    }

    /// Each model's vectors of the images it has looked at (`seeing`).
    pub fn seeing(&self) -> PathBuf {
        self.local.join("seeing")
    }

    /// Where the search helper says which port it listens on.
    pub fn seeker(&self) -> PathBuf {
        self.local.join("seeker.json")
    }

    /// The indexer's progress, rewritten while it works (JSON).
    /// The file Activity's "Skip" asks the indexer to let go (`indexer_skip_event`).
    pub fn skip(&self) -> PathBuf {
        self.local.join("skip.txt")
    }

    pub fn status(&self) -> PathBuf {
        self.local.join("status.json")
    }

    /// Excavate's own index of the drives' names, when it's switched on (`engine::names`).
    pub fn names(&self) -> PathBuf {
        self.local.join("names.idx")
    }

    /// A file each process rewrites now and then to see whether Everything notices (`Everything::health`).
    pub fn everything_probe(&self, process: &str) -> PathBuf {
        self.local.join(format!("everything-check-{process}"))
    }
}

/// How far a regular expression that needs backtracking may go on one name before it counts as no match, so a bad one
/// can't hold up reading. (Expressions that don't need it run in linear time, without a limit.)
const BACKTRACK_LIMIT: usize = 100_000;

/// A regular expression from what's left out.
#[derive(Clone, Debug)]
struct Expression {
    regex: fancy_regex::Regex,
    /// Said once: it failed on a name.
    complained: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Expression {
    fn new(pattern: &str) -> Result<Expression, String> {
        // Not case-sensitive, like Windows' file names.
        let regex = fancy_regex::RegexBuilder::new(&format!("(?i){pattern}")).backtrack_limit(BACKTRACK_LIMIT).build().map_err(|error| error.to_string())?;
        Ok(Expression { regex, complained: Default::default() })
    }

    fn matches(&self, text: &str) -> bool {
        self.regex.is_match(text).unwrap_or_else(|error| {
            if !self.complained.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log(&format!("left out: {}: {error}; it counts as no match", self.regex.as_str()));
            }
            false
        })
    }
}

/// A regular expression's pattern, if the entry is one ("regex:…", the prefix Everything uses too).
fn regex_pattern(entry: &str) -> Option<&str> {
    let (prefix, pattern) = entry.split_at_checked(6)?;
    prefix.eq_ignore_ascii_case("regex:").then_some(pattern)
}

/// Whether something can go in the list of what's left out: a regular expression has to make sense.
pub fn check_exclusion(entry: &str) -> Result<(), String> {
    match regex_pattern(entry.trim().trim_matches('"')) {
        Some(pattern) if pattern.is_empty() => Err("Write the expression after regex:".into()),
        Some(pattern) => Expression::new(pattern).map(|_| ()).map_err(|error| format!("That regular expression doesn't work: {error}")),
        None => Ok(()),
    }
}

/// Folders and names left out of reading.
#[derive(Clone, Debug, Default)]
pub struct Exclusions {
    /// Full folder paths, lowercased, ending in a backslash.
    folders: Vec<String>,
    /// Exact names of a folder or file, lowercased.
    names: Vec<String>,
    /// Names with * or ?, lowercased.
    patterns: Vec<String>,
    /// Paths with * or ?, lowercased.
    path_patterns: Vec<String>,
    /// Regular expressions, tried on each name.
    name_expressions: Vec<Expression>,
    /// Regular expressions with a backslash of their own (`\\`), tried on the whole path.
    path_expressions: Vec<Expression>,
}

impl Exclusions {
    pub fn new(entries: &[String]) -> Exclusions {
        let mut exclusions = Exclusions::default();
        for entry in entries {
            let entry = entry.trim().trim_matches('"');
            if let Some(pattern) = regex_pattern(entry) {
                // (Checked when it was added; a file edited by hand could hold anything.)
                match Expression::new(pattern) {
                    Ok(expression) if pattern.contains(r"\\") => exclusions.path_expressions.push(expression),
                    Ok(expression) => exclusions.name_expressions.push(expression),
                    Err(error) => log(&format!("left out: {pattern}: {error}")),
                }
                continue;
            }
            let entry = entry.to_lowercase();
            if entry.is_empty() {
                continue;
            }
            let wild = entry.contains('*') || entry.contains('?');
            if entry.contains('\\') || entry.contains(':') {
                let mut folder = entry.replace('/', "\\");
                if wild {
                    exclusions.path_patterns.push(folder);
                    continue;
                }
                if !folder.ends_with('\\') {
                    folder.push('\\');
                }
                exclusions.folders.push(folder);
            } else if wild {
                exclusions.patterns.push(entry);
            } else {
                exclusions.names.push(entry);
            }
        }
        exclusions
    }

    /// Whether a path (a file or a folder) is left out: under an excluded folder, matching a pattern for whole paths, or
    /// any part of it is an excluded name.
    pub fn excludes(&self, path: &str) -> bool {
        let lower = path.to_lowercase();
        if self.folders.iter().any(|folder| lower.starts_with(folder.as_str()) || lower == folder[..folder.len() - 1]) {
            return true;
        }
        if self.path_patterns.iter().any(|pattern| wildcard(pattern, &lower)) || self.path_expressions.iter().any(|expression| expression.matches(path)) {
            return true;
        }
        lower.split('\\').skip(1).any(|part| self.excludes_name_lower(part))
            || (!self.name_expressions.is_empty() && path.split('\\').skip(1).any(|part| self.matches_expression(part)))
    }

    /// Whether a folder or file name is left out (for walking a folder without building full paths).
    pub fn excludes_name(&self, name: &str) -> bool {
        self.excludes_name_lower(&name.to_lowercase()) || self.matches_expression(name)
    }

    fn excludes_name_lower(&self, name: &str) -> bool {
        !name.is_empty()
            && (self.names.iter().any(|excluded| excluded == name) || self.patterns.iter().any(|pattern| wildcard(pattern, name)))
    }

    fn matches_expression(&self, name: &str) -> bool {
        !name.is_empty() && self.name_expressions.iter().any(|expression| expression.matches(name))
    }

    /// The exclusions Everything can apply itself, as search syntax (the rest are applied to its results).
    pub fn everything(&self) -> String {
        let mut syntax = String::new();
        for name in &self.names {
            syntax.push_str(&format!(" !\"\\{name}\\\""));
        }
        for folder in &self.folders {
            syntax.push_str(&format!(" !\"{folder}\""));
        }
        syntax
    }
}

/// `*` any run of characters, `?` one character.
fn wildcard(pattern: &str, text: &str) -> bool {
    let (mut p, mut t) = (pattern.chars(), text.chars());
    // Where to go on from when what's after the last `*` stops matching.
    let mut star = None;
    loop {
        let before = t.clone();
        match (p.next(), t.next()) {
            (Some('*'), _) => {
                t = before.clone();
                star = Some((p.clone(), before));
            }
            (Some('?'), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => {
                let Some((after, from)) = star.as_mut() else { return false };
                if from.next().is_none() {
                    return false;
                }
                p = after.clone();
                t = from.clone();
            }
        }
    }
}

static LOG_NAME: OnceLock<&'static str> = OnceLock::new();

/// Names this process's log ("excavate", "indexer"). Call once, first thing.
pub fn set_log_name(name: &'static str) {
    let _ = LOG_NAME.set(name);
}

fn log_name() -> &'static str {
    LOG_NAME.get().copied().unwrap_or("excavate")
}

/// Appends a line to the log (rotated at 1 MB). Logging never fails the caller.
pub fn log(line: &str) {
    use std::sync::Mutex;
    static FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let path = Paths::new().log();
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 1 << 20) {
            let _ = std::fs::rename(&path, path.with_extension("old.log"));
        }
        Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(path).ok())
    });
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    if let Some(file) = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_mut() {
        let _ = writeln!(file, "{seconds:.3} {line}");
    }
    if cfg!(debug_assertions) {
        eprintln!("{line}");
    }
}

/// Writes a file whole or not at all (a temporary file, then a rename), so a reader never sees half of it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_kinds() {
        let mut settings = Settings { folders: vec![r"C:\Users\a\Documents".into(), r"C:\Users\a\Documents\Calls\".into(), r"D:\Photos".into()], ..Settings::default() };
        settings.folder_kinds.insert(r"c:\users\a\documents".into(), vec!["audio".into(), "image-text".into()]);
        settings.folder_kinds.insert(r"c:\users\a\documents\calls".into(), vec!["video-text".into(), "video-said".into()]);
        let kinds = settings.kinds();
        assert!(!kinds.on(r"C:\Users\a\Documents\memo.m4a", "audio"));
        assert!(!kinds.on(r"C:\Users\a\Documents\scan.png", "image-text"));
        assert!(kinds.on(r"C:\Users\a\Documents\scan.png", "image-shown"));
        // The inner chosen folder decides for what's in it.
        assert!(kinds.on(r"C:\Users\a\Documents\Calls\call.m4a", "audio"));
        assert!(!kinds.on(r"C:\Users\a\Documents\Calls\call.mp4", "video-text"));
        assert!(kinds.on(r"C:\Users\a\Documents\Calls\call.mp4", "video-shown"));
        assert_eq!(kinds.video_bits(r"C:\Users\a\Documents\Calls\call.mp4"), video_kind::SHOWN);
        assert_eq!(kinds.video_bits(r"C:\Users\a\Documents\clip.mp4"), video_kind::ALL);
        // A name that only starts the same isn't inside.
        assert!(kinds.on(r"C:\Users\a\Documents2\memo.m4a", "audio"));
        assert!(kinds.on(r"D:\Photos\a.jpg", "image-text"));
        assert_eq!(kinds.off("audio"), [r"c:\users\a\documents".to_string()]);
    }

    #[test]
    fn listening_devices() {
        let none = devices::Measured::default();
        let mut settings = Settings::default();
        // Defaults: Quick on the processor, the others on the graphics card by Vulkan; nothing but the processor without one.
        assert_eq!(settings.listen_device("quick", true, true, &none), "cpu");
        assert_eq!(settings.listen_device("balanced", true, true, &none), "vulkan");
        assert_eq!(settings.listen_device("deep", true, false, &none), "vulkan");
        assert_eq!(settings.listen_device("balanced", false, true, &none), "cpu");
        // Chosen on the cards; CUDA only where it can be had, and Thorough never on the processor.
        settings.listening_devices.insert("quick".into(), "vulkan".into());
        settings.listening_devices.insert("balanced".into(), "cuda".into());
        settings.listening_devices.insert("deep".into(), "cpu".into());
        assert_eq!(settings.listen_device("quick", true, false, &none), "vulkan");
        assert_eq!(settings.listen_device("balanced", true, true, &none), "cuda");
        assert_eq!(settings.listen_device("balanced", true, false, &none), "vulkan");
        assert_eq!(settings.listen_device("deep", true, true, &none), "vulkan");
        // The older single choice is the default way, also for Thorough when it was set to the processor.
        settings.listening_gpu = "cuda".into();
        assert_eq!(settings.listen_device("deep", true, true, &none), "cuda");
        settings.listening_devices.clear();
        assert_eq!(settings.listen_device("deep", true, true, &none), "cuda");
    }

    #[test]
    fn images_devices() {
        let none = devices::Measured::default();
        let mut settings = Settings::default();
        // Defaults: text on the processor but Thorough's; what they show on the graphics card.
        assert_eq!([settings.reading_device("quick", true), settings.reading_device("balanced", true), settings.reading_device("deep", true)], ["cpu", "cpu", "gpu"]);
        assert_eq!([settings.seeing_device("quick", true, &none), settings.seeing_device("balanced", true, &none), settings.seeing_device("deep", true, &none)], ["gpu", "gpu", "gpu"]);
        assert_eq!([settings.reading_device("deep", false), settings.seeing_device("balanced", false, &none)], ["cpu", "cpu"]);
        // Chosen on the cards; the graphics card only when there's one. Text in images never on it but Thorough's,
        // even when it was chosen before.
        settings.reading_devices.insert("quick".into(), "gpu".into());
        settings.seeing_devices.insert("balanced".into(), "cpu".into());
        assert_eq!(settings.reading_device("quick", true), "cpu");
        settings.reading_devices.insert("deep".into(), "cpu".into());
        assert_eq!(settings.reading_device("deep", true), "cpu");
        assert_eq!(settings.seeing_device("balanced", true, &none), "cpu");
    }

    #[test]
    fn popup_size() {
        let mut settings = Settings::default();
        assert_eq!(settings.popup_size_label(), "Medium \u{b7} 960 \u{d7} 600");
        settings.popup_width = 800;
        settings.popup_height = 500;
        assert_eq!(settings.popup_size_label(), "Small \u{b7} 800 \u{d7} 500");
        settings.popup_width = 1100;
        settings.popup_height = 700;
        assert_eq!(settings.popup_size_label(), "Custom \u{b7} 1100 \u{d7} 700");
    }

    #[test]
    fn exclusions() {
        let exclusions = Exclusions::new(&[
            "node_modules".into(), "*.tmp".into(), "~$*".into(), r"C:\Users\me\Documents\Private".into(),
        ]);
        assert!(exclusions.excludes(r"C:\code\app\node_modules\left-pad\index.js"));
        assert!(exclusions.excludes(r"C:\code\app\node_modules"));
        assert!(!exclusions.excludes(r"C:\code\app\node_modules_notes.txt"));
        assert!(exclusions.excludes(r"C:\Users\me\Desktop\scratch.TMP"));
        assert!(exclusions.excludes(r"C:\Users\me\Documents\~$report.docx"));
        assert!(exclusions.excludes(r"C:\Users\me\Documents\Private\taxes.pdf"));
        assert!(exclusions.excludes(r"C:\Users\me\Documents\private"));
        assert!(!exclusions.excludes(r"C:\Users\me\Documents\Private notes.txt"));
        assert!(!exclusions.excludes(r"C:\Users\me\Documents\report.docx"));
        assert!(exclusions.everything().contains(r#"!"\node_modules\""#));
    }

    #[test]
    fn exclusions_by_pattern() {
        let exclusions = Exclusions::new(&[
            r"regex:^IMG_\d{4}\.jpe?g$".into(),
            r"regex:(?<!keep)\.bak$".into(),
            r"regex:^C:\\Temp\\.*\.log$".into(),
            r"C:\Scratch\*.log".into(),
            r"regex:(a+)+$".into(),
        ]);
        // On each name, whatever its case, and on a folder's name too.
        assert!(exclusions.excludes(r"C:\Pictures\IMG_2041.jpg"));
        assert!(exclusions.excludes(r"C:\Pictures\img_2041.JPEG"));
        assert!(exclusions.excludes_name("IMG_2041.jpg"));
        assert!(!exclusions.excludes(r"C:\Pictures\IMG_20411.jpg"));
        // What only the fancy engine does: a look behind.
        assert!(exclusions.excludes(r"C:\Work\report.bak"));
        assert!(!exclusions.excludes(r"C:\Work\keep.bak"));
        // A backslash of its own: on the whole path.
        assert!(exclusions.excludes(r"C:\temp\deep\run.LOG"));
        assert!(!exclusions.excludes(r"D:\Temp\run.log"));
        // A wildcard with a path, which used to do nothing.
        assert!(exclusions.excludes(r"C:\Scratch\sub\run.log"));
        assert!(!exclusions.excludes(r"C:\Scratch\run.txt"));
        assert!(!exclusions.everything().contains("regex") && !exclusions.everything().contains("scratch"));
        // Too much backtracking counts as no match, and doesn't hang.
        let long = format!(r"C:\Work\{}b", "a".repeat(40));
        let started = std::time::Instant::now();
        assert!(!exclusions.excludes(&long));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn checking_what_goes_in_the_list() {
        assert!(check_exclusion("node_modules").is_ok());
        assert!(check_exclusion(r"C:\temp\*.log").is_ok());
        assert!(check_exclusion(r"regex:^IMG_\d+$").is_ok());
        assert!(check_exclusion(r"REGEX:(?<=a)b").is_ok());
        assert!(check_exclusion("regex:(unclosed").is_err());
        assert!(check_exclusion("regex:").is_err());
        // One that doesn't compile is skipped when loading, not a failure.
        assert!(!Exclusions::new(&["regex:(unclosed".into()]).excludes(r"C:\unclosed"));
    }

    #[test]
    #[ignore = "a measurement: cargo test -p config --release -- --ignored --nocapture exclusion_cost"]
    fn exclusion_cost() {
        let paths: Vec<String> = (0..3_000_000).map(|index| format!(r"C:\Users\someone\Documents\project {}\sub folder {}\file name {index}.docx", index % 500, index % 37)).collect();
        let defaults: Vec<String> = DEFAULT_EXCLUDE.iter().map(|name| name.to_string()).collect();
        let mut plain = defaults.clone();
        plain.push(r"regex:^IMG_\d{4}\.jpe?g$".into());
        let mut fancy = defaults.clone();
        fancy.push(r"regex:(?<!keep)\.bak$".into());
        let mut whole = defaults.clone();
        whole.push(r"regex:^C:\\Temp\\.*\.log$".into());
        for (label, entries) in [("defaults only", &defaults), ("and a simple expression", &plain), ("and a lookbehind", &fancy), ("and a whole-path expression", &whole)] {
            let exclusions = Exclusions::new(entries);
            let started = std::time::Instant::now();
            let left_out = paths.iter().filter(|path| exclusions.excludes(path)).count();
            println!("{label}: {:.0} ns a file, {:.2} s for 3 million ({left_out} left out)", started.elapsed().as_nanos() as f64 / paths.len() as f64, started.elapsed().as_secs_f64());
        }
    }

    #[test]
    fn pausing() {
        let mut settings = Settings::default();
        assert!(!settings.paused(100));
        settings.paused_until = -1;
        assert!(settings.paused(100));
        assert_eq!(settings.pause_text(0, false), "Paused");
        settings.paused_until = 200;
        assert!(settings.paused(100) && !settings.paused(200));
        // 16:30 UTC, shown two hours ahead.
        settings.paused_until = 16 * 3600 + 30 * 60;
        assert_eq!(settings.pause_text(2 * 3600, false), "Paused until 18:30");
        assert_eq!(settings.pause_text(2 * 3600, true), "Paused until 6:30 PM");
        // At 23:00, the next 06:00; at 05:30, not half an hour away, but the day after.
        let day = 20_000 * 86_400;
        assert_eq!(tomorrow_morning(day + 23 * 3600, 0), day + 86_400 + 6 * 3600);
        assert_eq!(tomorrow_morning(day + 5 * 3600 + 1800, 0), day + 86_400 + 6 * 3600);
        assert_eq!(tomorrow_morning(day + 3600, 0), day + 6 * 3600);
    }

    #[test]
    fn telling_the_time() {
        assert_eq!(clock(14 * 60 + 2, false), "14:02");
        assert_eq!(clock(14 * 60 + 2, true), "2:02 PM");
        assert_eq!(clock(5, false), "00:05");
        assert_eq!(clock(5, true), "12:05 AM");
        assert_eq!(clock(12 * 60, true), "12:00 PM");
        assert_eq!(clock(23 * 60 + 59, true), "11:59 PM");
        // As Windows has it, or as the setting says.
        let mut settings = Settings::default();
        assert!(settings.twelve_hour(true) && !settings.twelve_hour(false));
        settings.time_format = "24-hour".into();
        assert!(!settings.twelve_hour(true));
        settings.time_format = "12-hour".into();
        assert!(settings.twelve_hour(false));
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*.tmp", "a.tmp"));
        assert!(wildcard("~$*", "~$doc.docx"));
        assert!(wildcard("a?c", "abc"));
        assert!(!wildcard("a?c", "ac"));
        assert!(wildcard("*", ""));
        assert!(wildcard("*a*b", "xaxab"));
        assert!(wildcard("a**", "a"));
        assert!(!wildcard("*.tmp", "a.tmpx"));
        assert!(!wildcard("abc", "ab"));
        assert!(!wildcard("ab", "abc"));
        assert!(wildcard("é?", "éa"));
        assert!(wildcard(r"c:\scratch\*.log", r"c:\scratch\sub\run.log"));
    }
}

#[cfg(test)]
mod measured_tests {
    use super::*;

    #[test]
    fn a_measurement_comes_after_a_choice() {
        let mut settings = Settings::default();
        let mut measured = devices::Measured::default();
        let ms = |cpu: f64, gpu: f64| std::collections::BTreeMap::from([("cpu".to_string(), cpu), ("gpu".to_string(), gpu)]);
        measured.record("reading:balanced", ms(900.0, 120.0), 1);
        measured.record("seeing:siglip", ms(30.0, 80.0), 1);
        measured.record("listening:balanced", std::collections::BTreeMap::from([("cpu".to_string(), 9000.0), ("vulkan".to_string(), 700.0)]), 1);
        // Text in images isn't followed from a measurement: the processor, unless chosen on Balanced's card.
        assert_eq!(settings.reading_device("balanced", true), "cpu");
        assert_eq!(settings.seeing_device("quick", true, &measured), "cpu");
        assert_eq!(settings.listen_device("balanced", true, false, &measured), "vulkan");
        settings.reading_devices.insert("balanced".into(), "gpu".into());
        assert_eq!(settings.reading_device("balanced", true), "gpu");
        assert_eq!(settings.reading_device("balanced", false), "cpu");
    }
}
