// Excavate Reader and Excavate Helper, one program: `main.rs` and `helper.rs` each include it (two executables, so
// Task Manager names each by what it does). Its description is in `main.rs`.

mod catalog;
mod codes;
mod control;
mod deep;
mod devices;
mod listening;
mod looking;
mod models;
mod ocr;
mod pace;
mod documents;
mod scan;
mod seek;
mod video_text;
mod work;

use catalog::Catalog;
use config::status::{Folder, Status};
use config::{Paths, Reading, Settings, log};
use control::Control;
use scan::FileInfo;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use work::{Context, Gate, Job, Queue, Shared, Write, Writes};
use winsys::watch::{Change, Watcher};


/// Read again once: files whose stored text shows a secret the detector finds since (values under their labels; then
/// one-time and backup codes, tokens, 2FA setup keys: `-2`; IBANs under their label whose check digits fail: `-3`; QR
/// codes for signing in or paying, a code alone on its line, SIM PINs, unlabelled tokens: `-4`).
const UNMARKED_SECRETS_REREAD: &str = "unmarked-secrets-reread-4";
/// When the pictures and videos read before 0.1.5 began to be read again, once, for where their lines are (the preview
/// outlines the words searched for along them).
const LINES_KEPT_REREAD: &str = "lines-kept-reread-at";

/// ONNX Runtime (OCR, SigLIP 2), from Microsoft's onnxruntime.dll beside this program, before anything uses it.
fn onnx_runtime() {
    let Some(dll) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|folder| folder.join("onnxruntime.dll"))) else { return };
    match ort::init_from(&dll) {
        Ok(environment) => {
            let _ = environment.commit();
        }
        // (Reading what needs it then fails with this in the log; documents and everything else go on.)
        Err(error) => log(&format!("ONNX Runtime isn't there ({}): {error}", dll.display())),
    }
}

fn main() {
    // Every panic in the log, with its thread and place: a worker that ended on one used to leave no trace.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        config::log(&format!("panic on thread {}: {info}", thread.name().unwrap_or("?")));
        default_hook(info);
    }));
    // EXCAVATE_MEMORY_TRACE: this process's private memory in the log, each second for two minutes, then each 30 s.
    if std::env::var_os("EXCAVATE_MEMORY_TRACE").is_some() {
        std::thread::spawn(|| {
            let started = Instant::now();
            loop {
                let (working, private) = winsys::process_memory();
                config::log(&format!("memory: {} MB private, {} MB working set", private >> 20, working >> 20));
                std::thread::sleep(Duration::from_secs(if started.elapsed() < Duration::from_secs(120) { 1 } else { 30 }));
            }
        });
    }
    // `--read-pdfs`: a PDF reader for one of the documents' workers at Full speed (pdfs.rs).
    if std::env::args().any(|arg| arg == documents::ARGUMENT || arg == documents::OLD_ARGUMENT) {
        documents::serve();
        return;
    }
    // The log's name first, before anything can log: the measuring tools write `bench.log`, never the app's own
    // `excavate.log` (they once did, from a run without EXCAVATE_HOME).
    const MEASURING: [&str; 5] = ["--video-text", "--privacy-bench", "--ocr-memory", "--seeing-bench", "--listen"];
    config::set_log_name(if std::env::args().any(|arg| arg == "--seek") {
        "seeker"
    } else if std::env::args().any(|arg| MEASURING.contains(&arg.as_str())) {
        "bench"
    } else {
        "indexer"
    });
    onnx_runtime();
    // `--seek`: the search helper for images by what they show (seek.rs), at normal priority: someone is waiting.
    if std::env::args().any(|arg| arg == "--seek") {
        winsys::com_mta();
        seek::serve(&Paths::new());
        winsys::process::exit_now(0);
    }
    // `--video-text <video> [every]`: measures reading a video's text (video_text.rs), and prints what it found.
    if std::env::args().any(|arg| arg == "--video-text") {
        video_text::measure(&Paths::new());
        return;
    }
    // `--privacy-bench <specimens> <models> <out.jsonl> [tiny]`: what the preview would blur in each image (work.rs).
    if std::env::args().any(|arg| arg == "--privacy-bench") {
        work::privacy_bench();
        return;
    }
    // `--ocr-memory <list> <models> <out.tsv> <gpu|cpu> [tiny]`: the image reader's memory over many pictures (work.rs).
    if std::env::args().any(|arg| arg == "--ocr-memory") {
        work::ocr_memory();
        return;
    }
    // `--seeing-bench <list> <models> <siglip|gemma> <out> [gpu]`: images' and descriptions' vectors (looking.rs).
    if std::env::args().any(|arg| arg == "--seeing-bench") {
        looking::seeing_bench();
        return;
    }
    // `--listen <file> <listener folder> <model> <whisper|parakeet> <backend> <silero>`: measures hearing one file.
    if std::env::args().any(|arg| arg == "--listen") {
        listening::measure();
        return;
    }
    // `--build-names <file> [drive…]`: the drives' index built from their own file tables (engine::names::
    // build_from_tables), run with the administrator rights Windows asked the person for; then it ends. Exit code 0
    // when the index was saved.
    if let Some(at) = std::env::args().position(|arg| arg == "--build-names") {
        let args: Vec<String> = std::env::args().skip(at + 1).collect();
        let Some(file) = args.first() else { winsys::process::exit_now(2) };
        let drives = (args.len() > 1).then(|| args[1..].to_vec());
        match engine::names::build_from_tables(Path::new(file), drives) {
            Ok(built) => {
                log(&format!("--build-names: {} entries in {:.1} s; read {:?}, listed {:?}", built.entries, built.took.as_secs_f64(), built.read, built.walked));
                println!("{} entries in {:.1} s; read from the file table: {:?}; listed folder by folder: {:?}", built.entries, built.took.as_secs_f64(), built.read, built.walked);
                winsys::process::exit_now(0);
            }
            Err(error) => {
                log(&format!("--build-names failed: {error}"));
                println!("failed: {error}");
                winsys::process::exit_now(1);
            }
        }
    }
    // `--quit`: asks a running indexer to finish and stop (for scripts and tests).
    if std::env::args().any(|arg| arg == "--quit") {
        let heard = winsys::process::signal(&config::indexer_quit_event());
        log(&format!("--quit: {}", if heard { "signalled" } else { "no indexer running" }));
        return;
    }
    if !winsys::single_instance(&config::instance_name("Indexer.Running")) {
        return;
    }
    if std::env::var_os("EXCAVATE_FOREGROUND").is_none() {
        winsys::process::run_in_background();
    }
    let control: &'static Control = Box::leak(Box::default());
    control::listen(control);
    let paths = Paths::new();
    log(&format!("indexer {} started", env!("CARGO_PKG_VERSION")));
    while !control.quitting() {
        control.restarted();
        let settings = Settings::load(&paths);
        winsys::gpu::choose(&settings.graphics_card);
        if !settings.setup_done || settings.folders.is_empty() {
            Status { state: "waiting".into(), detail: "Waiting for setup".into(), updated: work::now(), ..Status::default() }.save(&paths);
            while !control.stopping() {
                control.sleep(Duration::from_secs(3600));
            }
            continue;
        }
        // From here a pause or "Full speed" is taken in as reading goes; anything else starts it over.
        control.begin(&settings);
        let result = session(&paths, &settings, control);
        control.end();
        if let Err(error) = result {
            log(&format!("indexer: {error}"));
            Status { state: "stopped".into(), detail: error, updated: work::now(), ..Status::default() }.save(&paths);
            control.sleep(Duration::from_secs(60));
        }
    }
    let mut status = Status::load(&paths).unwrap_or_default();
    status.state = "stopped".into();
    status.updated = work::now();
    status.save(&paths);
    log("indexer stopped");
    // (Its models may hold the GPU through DirectML or video decoding: winsys::process::exit_now says why.)
    winsys::process::exit_now(0);
}

/// A lane's thread that a panic doesn't end. Before, a panic past a lane's own guards ended its thread with nothing
/// said, and that kind of reading stopped until Excavate was restarted (and again at once, on the same file): 0.1.3's
/// documents stood still for hours at 43,098 files. Now the panic is in the log (the hook in `main`) and the lane
/// starts again after a wait that grows (5 s, 30 s, 5 min, then every 15 min), so a file that keeps doing it can't
/// keep the processor busy.
fn lasting(control: &'static Control, run: impl Fn() + Send + 'static) -> impl FnOnce() + Send + 'static {
    move || {
        for wait in [5u64, 30, 300].into_iter().chain(std::iter::repeat(900)) {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(&run)).is_ok() || control.stopping() {
                return;
            }
            let name = std::thread::current().name().unwrap_or("a lane").to_string();
            log(&format!("{name}: stopped on a panic (above); started again in {wait} s"));
            if !control.sleep(Duration::from_secs(wait)) {
                return;
            }
        }
    }
}

/// With EXCAVATE_MEMORY_TRACE: this process's private memory now, in the log.
pub fn memory_note(what: &str) {
    if std::env::var_os("EXCAVATE_MEMORY_TRACE").is_some() {
        log(&format!("memory at {what}: {} MB private", winsys::process_memory().1 >> 20));
    }
}

fn session(paths: &Paths, settings: &Settings, control: &'static Control) -> Result<(), String> {
    let started = Instant::now();
    // Without Media Foundation (Windows' N editions), recordings and videos are left as they are rather than each
    // failing, to be read once the Media Feature Pack is added.
    let without_media;
    let settings = if winsys::media::available() {
        settings
    } else {
        log(&format!("media: {}: recordings and videos are left for now", winsys::media::MISSING));
        without_media = Settings { listening_on: false, video_speech: false, video_text: false, videos_on: false, ..settings.clone() };
        &without_media
    };
    memory_note("session");
    let catalog = Arc::new(Catalog::open(&paths.catalog())?);
    memory_note("catalog open");
    let (writer, fresh) = content::Writer::open(&paths.index(), 48_000_000)?;
    memory_note("content index open");
    if fresh {
        log("new index: everything will be read again");
        catalog.clear();
    }
    let roots: Vec<String> = settings.folders.iter().filter(|folder| Path::new(folder).is_dir()).cloned().collect();
    // Chosen folders that aren't there (deleted, moved, a drive unplugged): said so in the status.
    let gone: Vec<String> = settings.folders.iter().filter(|folder| !roots.contains(folder)).cloned().collect();
    // Excavate's own folders are never read (the index would be reading itself).
    let mut exclude = settings.exclude.clone();
    exclude.push(paths.local.display().to_string());
    exclude.push(paths.roaming.display().to_string());
    let exclusions = config::Exclusions::new(&exclude).with_chosen(&roots);
    let reading = settings.reading();
    let pictures = reading != Reading::Off;
    let size = if reading == Reading::Quick { models::Size::Tiny } else { models::Size::Small };
    // (Settings offers Deep only with a capable graphics card; the file could say otherwise, or the card have changed.)
    let deep = reading == Reading::Deep && winsys::gpu::capable().is_some();
    let gate = Arc::new(Gate::new(settings, catalog.clone()));
    let seeing_gate = Arc::new(Gate::seeing(settings, catalog.clone()));
    let listen_gate = Arc::new(Gate::listening(settings, catalog.clone()));
    if deep {
        catalog.deep_wants_all(deep::ENGINE);
    }

    let shared = Arc::new(Shared::default());
    // The pace (a pause, "Full speed", each kind's "everything now"), as the settings say now and each time it
    // changes, without starting over: the gates, the pause, and the priority that goes with them.
    {
        let (shared, gates) = (shared.clone(), [gate.clone(), seeing_gate.clone(), listen_gate.clone()]);
        control.on_pace(Arc::new(move |settings: &Settings| {
            let pause_text = settings.pause_text(winsys::process::utc_offset(), settings.twelve_hour(winsys::process::twelve_hour_clock()));
            *shared.pause.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = (settings.paused_until, pause_text);
            for (gate, all_at) in gates.iter().zip([settings.read_all_at, settings.look_all_at, settings.listen_all_at]) {
                gate.follow(all_at);
            }
            // The speed control's level, and the kinds paused on their own.
            shared.set_speed(settings.speed());
            *shared.kinds_paused.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = settings.paused_kinds.clone();
            let gates = [&*gates[0], &*gates[1], &*gates[2]];
            work::set_priority(&shared, &gates);
            let speeds: Vec<&str> = gates.iter().map(|gate| shared.speed(gate).name()).collect();
            log(&format!("pace: paused until {}; text in images {}, what they show {}, what's said {}; kinds paused: {}", settings.paused_until, speeds[0],
                         speeds[1], speeds[2], if settings.paused_kinds.is_empty() { "none".to_string() } else { settings.paused_kinds.join(", ") }));
        }));
    }
    shared.status().started = work::now();
    shared.status().deep = deep;
    shared.status().documents = writer.documents();
    shared.status().folders = roots.iter().map(|root| Folder { path: root.clone(), ..Folder::default() })
        .chain(gone.iter().map(|folder| Folder { path: folder.clone(), missing: true, ..Folder::default() })).collect();
    shared.status().engine = if pictures { size.label().to_string() } else { String::new() };
    // Images by what they show: the models the choice uses, where it's chosen to run (the processor without a graphics
    // card able to).
    let gpu = winsys::gpu::capable().is_some();
    // Where each runs fastest on this PC, as measured with this graphics card and driver: measured again after a
    // change of either.
    let mut measured = config::devices::Measured::load(paths);
    if measured.follow_adapter(&devices::adapter()) {
        log("a different graphics card or driver: where each runs fastest is measured again");
        measured.save(paths);
    }
    let engines: Vec<seeing::Engine> = seeing::engines(settings.seeing_on, &settings.seeing, &settings.seeing_deep);
    // Images are listed when their text is read or what they show is looked at (each has its own switch): with the
    // text off, they're recorded without it, so they can still be looked at.
    let images_listed = pictures || !engines.is_empty();
    let reading_choice = config::Reading::parse(&settings.reading).name();
    let (reading_gpu, deep_gpu) = (settings.reading_device(reading_choice, gpu) == "gpu", settings.reading_device("deep", gpu) == "gpu");
    let seeing_gpu = settings.seeing_device(&settings.seeing, gpu, &measured) == "gpu";
    // Text in videos: where reading text was chosen to run, when it was (the processor, to keep the graphics card free,
    // is followed); else its detector on the graphics card when there's one able to (a third of the processor time).
    let video_text_gpu = gpu && settings.reading_devices.get(reading_choice).is_none_or(|chosen| chosen == "gpu");
    // What's measured this time, if anything (not text in images: `Settings::reading_device`).
    let seeing_measure = Some(config::devices::seeing_key(&settings.seeing).to_string())
        .filter(|key| devices::wanted(&measured, key, settings.seeing_devices.contains_key(&settings.seeing), gpu));
    let siglip_gpu = seeing_gpu && engines.len() == 1;
    shared.deep_gpu.store(deep_gpu, Ordering::Relaxed);
    shared.seeing_gpu.store(seeing_gpu, Ordering::Relaxed);
    let on = |gpu: bool| if gpu { "graphics card" } else { "processor" };
    // (Which card: the one with the most memory of its own, DirectML's device by its place in Windows' list.)
    let card = winsys::gpu::capable().map_or_else(|| "none able to".to_string(), |card| format!("{} (DirectML device {})", card.name, card.index));
    config::log(&format!("where they run: text in images on the {}, Thorough's on the {}, what they show on the {}, text in videos on the {}; the graphics card: {card}",
                         on(reading_gpu), on(deep_gpu), on(seeing_gpu), on(video_text_gpu)));
    shared.status().looking = engines.iter().map(|engine| config::status::Looking { engine: engine.name().into(), ..Default::default() }).collect();
    // Recordings by what's said in them.
    // The listener, for recordings, and for what's said in videos (after them).
    let video_speech = settings.video_speech;
    let listen = (settings.listening_on || video_speech).then(|| listening::Plan::of(settings, &measured));
    let listen_measure = listen.as_ref().and_then(|plan| {
        let choice = plan.ear.choice();
        Some(format!("listening:{choice}")).filter(|key| choice != "deep" && devices::wanted(&measured, key, settings.listening_devices.contains_key(choice), gpu))
    });
    // Text in videos: read by the images' lane once their text is read.
    let video_text = pictures && settings.video_text;
    if video_text {
        shared.status().video_text = Some(config::status::VideoText::default());
    }
    if let Some(plan) = &listen {
        shared.status().listening = Some(config::status::Listening {
            engine: plan.ear.name().into(),
            device: plan.device_label().into(),
            recordings: settings.listening_on,
            videos: video_speech,
            ..Default::default()
        });
    }
    let queue = Arc::new(Queue::default());
    let (sender, receiver) = work::writes();
    let writer_thread = {
        let (catalog, shared) = (catalog.clone(), shared.clone());
        std::thread::Builder::new().name("writer".into()).spawn(move || work::write(receiver, writer, catalog, shared, control))
            .map_err(|error| error.to_string())?
    };
    let load_thread = {
        let (shared, gate, seeing_gate, listen_gate) = (shared.clone(), gate.clone(), seeing_gate.clone(), listen_gate.clone());
        std::thread::Builder::new().name("load".into()).spawn(move || work::watch_load(&shared, &[&gate, &seeing_gate, &listen_gate], control))
            .map_err(|error| error.to_string())?
    };
    // Which chosen folders count in the totals (once each).
    let outer = outermost(&roots, &exclusions);
    let status_thread = {
        let (catalog, shared, queue, paths, roots, gate, seeing_gate, listen_gate) =
            (catalog.clone(), shared.clone(), queue.clone(), paths.clone(), roots.clone(), gate.clone(), seeing_gate.clone(), listen_gate.clone());
        let (outer, gone) = (outer.clone(), gone.clone());
        let (engines, relook_at) = (engines.clone(), settings.relook_images_at);
        let heard = listen.as_ref().map(|plan| (plan.ear, settings.relisten_at, settings.listening_on, video_speech));
        std::thread::Builder::new().name("status".into())
            .spawn(move || report(&paths, &catalog, &shared, &queue, (&roots, &outer, &gone), [&gate, &seeing_gate, &listen_gate], deep, &engines, relook_at, heard, control))
            .map_err(|error| error.to_string())?
    };

    // What's there, and what changed since last time.
    shared.listing.store(true, Ordering::SeqCst);
    // Excavate's own index on: Everything is left alone here, and the chosen folders are walked.
    everything::set_disabled(settings.own_index);
    let everything = everything::Everything::shared();
    let probe = paths.everything_probe("indexer");
    // Everything still loading its list: waited for up to three minutes (`settled_health`), a stop cutting it short.
    let health = {
        let asked = Instant::now();
        loop {
            let health = everything.health(&probe, Duration::ZERO);
            if health != everything::Health::Loading || asked.elapsed() >= Duration::from_secs(180) || !control.sleep(Duration::from_secs(1)) {
                break health;
            }
        }
    };
    log(&format!("Everything's answers: {health:?}{}", if health == everything::Health::Live { "" } else { ": the folders are walked" }));
    let kinds = settings.kinds();
    catalog.set_unshown(kinds.off("image-shown"));
    let lines_kept_at = match catalog.meta(LINES_KEPT_REREAD) {
        0 => {
            let at = work::now();
            catalog.set_meta(LINES_KEPT_REREAD, at);
            let videos = catalog.seen_forget_engine(crate::video_text::ENGINE);
            log(&format!("pictures read before now, and {videos} videos' text, read again for where their lines are"));
            at
        }
        at => at,
    };
    let reread_at = settings.reread_pictures_at.max(lines_kept_at);
    let lister = Lister { catalog: &catalog, exclusions: &exclusions, pictures: images_listed, text_in_images: pictures,
        reread_at, kinds: &kinds, online: Default::default() };
    let mut seen = HashSet::new();
    let mut jobs = Vec::new();
    if catalog.meta(UNMARKED_SECRETS_REREAD) == 0 {
        let again = work::files_with_unmarked_secrets(&paths.index());
        for path in &again {
            catalog.forget(&catalog::id(path));
        }
        log(&format!("{} files read again for secrets found under their labels", again.len()));
        catalog.set_meta(UNMARKED_SECRETS_REREAD, 1);
    }
    let known = catalog.all_hashed();
    memory_note("the catalog's files known");
    let mut images: Vec<FileInfo> = Vec::new();
    let mut videos: Vec<(String, u64, i64, u8)> = Vec::new();
    let videos_wanted = (!engines.is_empty() && settings.videos_on) || video_text || video_speech;
    let mut recordings: Vec<(String, u64, i64)> = Vec::new();
    // A walk of a large tree takes minutes: the settings changing (or quitting) meanwhile ends it, and the session starts
    // over with them, rather than finishing a listing nobody wants any more.
    let stop = || control.stopping();
    for root in &roots {
        if stop() {
            break;
        }
        let listed = Instant::now();
        let files = scan::list(root, &exclusions, images_listed, (health == everything::Health::Live).then_some(&*everything), &stop);
        log(&format!("{root}: {} files to consider, listed in {} ms", files.len(), listed.elapsed().as_millis()));
        images.extend(files.iter().filter(|file| file.reader == extract::Reader::Picture).cloned());
        lister.compare(files, &known, &mut seen, &mut jobs);
        // Videos matter to the models that look at what they show, to reading the text they show, and to hearing what's
        // said in them.
        // Each with the kinds its folder reads (a folder chosen inside another decides for its own).
        if videos_wanted && kinds.video_bits(root) != 0 {
            let listed = scan::list_videos(root, &exclusions, (health == everything::Health::Live).then_some(&*everything), &stop);
            videos.extend(listed.into_iter().map(|(path, size, modified)| {
                let bits = kinds.video_bits(&path);
                (path, size, modified, bits)
            }));
        }
        // And recordings to the model that listens.
        if settings.listening_on && kinds.on(root, "audio") {
            recordings.extend(scan::list_recordings(root, &exclusions, (health == everything::Health::Live).then_some(&*everything), true, &stop));
        }
    }
    catalog.online_add(&lister.online.take());
    memory_note("listed");
    // Stopped part way: what was listed is only part of what's there, so nothing is taken for gone (that would remove
    // every file not reached yet from the index). The next session lists it all again.
    if stop() {
        log("listing stopped part way: starting over");
        shared.listing.store(false, Ordering::SeqCst);
        drop(sender);
        let _ = writer_thread.join();
        control.nudge();
        let _ = status_thread.join();
        let _ = load_thread.join();
        return Ok(());
    }
    // (Listening off: what's known stays, so turning it on again goes on from there; what's gone goes then.)
    if settings.listening_on {
        catalog.recordings_replace(&recordings);
        log(&format!("{} recordings", recordings.len()));
    }
    drop(recordings);
    catalog.videos_replace(&videos);
    // Videos no longer in the chosen folders (or whose kind is left out): their text and their speech go.
    if videos_wanted {
        let engines: Vec<&str> = std::iter::once(video_text::ENGINE).chain(listening::Ear::SAID_NAMES).collect();
        let gone = catalog.videos_gone(&engines);
        for id in &gone {
            let _ = sender.send(Write::Remove(id.clone()));
            catalog.heard_forget(id, &engines);
        }
        if !gone.is_empty() {
            log(&format!("{} videos gone", gone.len()));
        }
        // Videos whose folder no longer reads their text, or what's said in them: that entry goes, and is read again
        // if it's turned back on. (What they show is kept, and left out of searches by the app.)
        let said: Vec<&str> = listening::Ear::SAID_NAMES.to_vec();
        for (engines, kind, is_said) in [(vec![video_text::ENGINE], config::video_kind::TEXT, false), (said, config::video_kind::SAID, true)] {
            let left_out = catalog.videos_left_out(&engines, kind);
            for id in &left_out {
                let _ = sender.send(Write::Unindex(content::entry_id(id, is_said)));
                catalog.heard_forget(id, &engines);
            }
            if !left_out.is_empty() {
                log(&format!("{} videos' {} left out by their folder", left_out.len(), if is_said { "speech" } else { "text" }));
            }
        }
    }
    if !videos.is_empty() {
        log(&format!("{} videos", videos.len()));
    }
    // The first sweep: judged on every image, read or not, so an image's place stays the same from one session to the next.
    let places = {
        let grouped = Instant::now();
        let list: Vec<sweep::Image> = images.iter().map(|file| sweep::Image { path: &file.path, size: file.size, modified: file.modified }).collect();
        let places = sweep::places(&list);
        let first = sweep::first_sweep(&places);
        log(&format!("first sweep: {first} of {} images, grouped in {} ms", images.len(), grouped.elapsed().as_millis()));
        images.iter().map(|file| catalog::id(&file.path)).zip(places).collect::<HashMap<String, sweep::Place>>()
    };
    let image_count = images.len() as u64;
    drop(images);
    // What the catalog has that the listing hadn't: gone (the catalog read one id at a time).
    let gone_ids = catalog.ids_where(|id| !seen.contains(&catalog::hashed(id)));
    let gone = gone_ids.len();
    for id in gone_ids {
        let _ = sender.send(Write::Remove(id));
    }
    jobs.sort_by_key(|job| std::cmp::Reverse(job.modified));
    log(&format!("to read: {} (and {gone} gone), {} ms", jobs.len(), started.elapsed().as_millis()));
    for file in jobs {
        let place = places.get(&catalog::id(&file.path)).copied().unwrap_or_default();
        let job = Job { place, ..Job::new(file) };
        let priority = work::priority(&job);
        queue.push(job, priority);
    }
    drop(places);
    shared.status().files = seen.len() as u64;
    shared.status().images = image_count;
    // (Every file's name, twice: not kept for the session, which lasts as long as the reader runs.)
    drop((known, seen));
    memory_note("queued");
    shared.listing.store(false, Ordering::SeqCst);

    // Workers.
    let context = || Context {
        queue: queue.clone(),
        sender: sender.clone(),
        control,
        shared: shared.clone(),
        pictures,
        read_on_battery: settings.read_on_battery,
        gate: gate.clone(),
        seeing_gate: seeing_gate.clone(),
        look_on_battery: settings.look_on_battery,
        listen_gate: listen_gate.clone(),
        listen_on_battery: settings.listen_on_battery,
        roots: roots.clone(),
        exclusions: exclusions.clone(),
        kinds: kinds.clone(),
        video_pace: settings.video_pace(),
        relook_videos_at: settings.relook_videos_at,
        videos_shown: settings.videos_on,
        video_text,
        video_text_pace: settings.video_text_every(),
        reading_gpu,
        video_text_gpu,
        deep_gpu,
        seeing_gpu,
        siglip_gpu,
        paths: paths.clone(),
        seeing_measure: seeing_measure.clone(),
        listen_measure: listen_measure.clone(),
    };
    let mut workers = Vec::new();
    // Documents: one worker, and more that read only at "Full speed" (`Speed::text_workers`), waiting meanwhile.
    let cores = std::thread::available_parallelism().map_or(4, |cores| cores.get());
    let text_workers = config::Speed::ALL.iter().map(|speed| speed.text_workers(cores)).max().unwrap_or(1);
    for rank in 0..text_workers {
        let (context, catalog) = (context(), catalog.clone());
        let name = if rank == 0 { "text".to_string() } else { format!("text {}", rank + 1) };
        workers.push(std::thread::Builder::new().name(name).spawn(lasting(control, move || work::read_text(context.clone(), rank, catalog.clone()))).map_err(|error| error.to_string())?);
    }
    // Images: one reader, and more that read only at "Full speed" (`Speed::picture_workers`).
    let picture_workers = if images_listed { config::Speed::ALL.iter().map(|speed| speed.picture_workers(cores)).max().unwrap_or(1) } else { 0 };
    for rank in 0..picture_workers {
        let (context, models, catalog, index) = (context(), paths.models(), catalog.clone(), paths.index());
        let name = if rank == 0 { "pictures".to_string() } else { format!("pictures {}", rank + 1) };
        workers.push(std::thread::Builder::new().name(name)
            .spawn(lasting(control, move || work::read_pictures(context.clone(), models.clone(), size, catalog.clone(), index.clone(), rank)))
            .map_err(|error| error.to_string())?);
    }
    if deep {
        let (context, catalog, models, index) = (context(), catalog.clone(), paths.models(), paths.index());
        workers.push(std::thread::Builder::new().name("deep".into())
            .spawn(lasting(control, move || work::read_deep(context.clone(), catalog.clone(), models.clone(), index.clone())))
            .map_err(|error| error.to_string())?);
    }

    for (slot, &engine) in engines.iter().enumerate() {
        let (context, catalog, models, folder, index) = (context(), catalog.clone(), paths.models(), paths.seeing(), paths.index());
        // Thorough's second model looks once the first is done, unless it's on the CPU and the first on the graphics card.
        let on_gpu = |engine: seeing::Engine| if engine == seeing::Engine::Siglip { siglip_gpu } else { seeing_gpu };
        let after = (slot > 0 && on_gpu(engine) == on_gpu(engines[0])).then(|| engines[0]);
        let relook_at = settings.relook_images_at;
        workers.push(std::thread::Builder::new().name(format!("looking {}", engine.name()))
            .spawn(lasting(control, move || looking::lane(context.clone(), catalog.clone(), models.clone(), folder.clone(), index.clone(), engine, after, relook_at, slot)))
            .map_err(|error| error.to_string())?);
    }

    if let Some(plan) = listen.clone() {
        let (context, catalog, models, relisten_at, recordings) = (context(), catalog.clone(), paths.models(), settings.relisten_at, settings.listening_on);
        workers.push(std::thread::Builder::new().name("listening".into())
            .spawn(lasting(control, move || listening::lane(context.clone(), catalog.clone(), models.clone(), plan.clone(), relisten_at, recordings, video_speech)))
            .map_err(|error| error.to_string())?);
    }

    // Changes, as Windows reports them.
    let (changes_in, changes_out) = std::sync::mpsc::channel::<Vec<Change>>();
    // Activity's "Try again", "Read again" and "Download and read": what kept them from being read is cleared, and
    // they go to the follower below as if they had just changed (`retry`).
    control.on_retry(retry(catalog.clone(), paths.index(), changes_in.clone(), control));
    let watcher = Watcher::start(&roots.iter().map(PathBuf::from).collect::<Vec<_>>(), move |changes| {
        let _ = changes_in.send(changes);
    });
    log(&format!("watching {} folders", watcher.len()));
    memory_note("watching");
    let follower = {
        let (queue, sender, catalog, exclusions, everything, probe) = (queue.clone(), sender.clone(), catalog.clone(), exclusions.clone(), everything, probe);
        let roots = roots.clone();
        let kinds = kinds.clone();
        std::thread::Builder::new()
            .name("changes".into())
            .spawn(move || {
                let lister = Lister { catalog: &catalog, exclusions: &exclusions, pictures: images_listed, text_in_images: pictures, reread_at, kinds: &kinds,
                                      online: Default::default() };
                follow(changes_out, &queue, &sender, &lister, &roots, &everything, &probe, control)
            })
            .map_err(|error| error.to_string())?
    };

    while !control.stopping() {
        control.sleep(Duration::from_secs(5));
    }
    drop(watcher);
    control.stop_retrying();
    queue.close();
    for worker in workers {
        let _ = worker.join();
    }
    let _ = follower.join();
    drop(sender);
    let _ = writer_thread.join();
    control.nudge();
    let _ = status_thread.join();
    let _ = load_thread.join();
    Ok(())
}

struct Lister<'a> {
    catalog: &'a Catalog,
    exclusions: &'a config::Exclusions,
    /// Images listed at all.
    pictures: bool,
    /// Their text read (else recorded without it).
    text_in_images: bool,
    reread_at: i64,
    kinds: &'a config::FolderKinds,
    /// Files kept online only, found while comparing: listed among what couldn't be read (`Catalog::online_add`).
    online: std::cell::RefCell<Vec<String>>,
}

/// What a session does with Activity's requests to read files again: the catalog cleared of what kept them, and each
/// file handed to the follower as changed (listed again, read as new). Files kept online only are brought down first,
/// on a thread of their own: reading a cloud file's contents has Windows fetch it, and it's then on this PC.
fn retry(catalog: Arc<Catalog>, index: PathBuf, changes: std::sync::mpsc::Sender<Vec<Change>>, control: &'static Control) -> control::Retrier {
    Arc::new(move |retries: Vec<config::Retry>| {
        for retry in retries {
            // Settings → Privacy's "Check everything again": the text already read, looked through for secrets afresh,
            // and the files with one it didn't cover read again.
            if retry.why == config::why::SECRETS {
                let again = work::files_with_unmarked_secrets(&index);
                log(&format!("check for secrets again: {} files to read again", again.len()));
                for path in &again {
                    catalog.forget(&catalog::id(path));
                }
                if !again.is_empty() {
                    let _ = changes.send(again.into_iter().map(|path| Change::Changed(PathBuf::from(path))).collect());
                }
                continue;
            }
            let again = catalog.retry(&retry.why, retry.path.as_deref());
            log(&format!("read again ({}): {} files to list again", retry.why, again.len()));
            if retry.why == config::why::ONLINE {
                let (catalog, changes) = (catalog.clone(), changes.clone());
                let _ = std::thread::Builder::new().name("bringing down".into()).spawn(move || {
                    winsys::process::background_thread_as(true);
                    for path in again {
                        // (Stopping: the rest waits for the next "Download and read"; the follower can end.)
                        if control.stopping() {
                            break;
                        }
                        let brought = std::fs::File::open(&path).and_then(|mut file| std::io::copy(&mut file, &mut std::io::sink()));
                        match brought {
                            Ok(bytes) if !scan::online_only(&path) => {
                                log(&format!("brought down {path} ({bytes} bytes) to read it"));
                                catalog.online_done(&path);
                                let _ = changes.send(vec![Change::Changed(PathBuf::from(&path))]);
                            }
                            Ok(_) => log(&format!("{path}: still online only after reading it")),
                            Err(error) => log(&format!("{path}: couldn't bring it down: {error}")),
                        }
                    }
                });
            } else if !again.is_empty() {
                let _ = changes.send(again.into_iter().map(|path| Change::Changed(PathBuf::from(path))).collect());
            }
        }
    })
}

impl Lister<'_> {
    /// Whether a file needs reading, given what the catalog has.
    fn needs_reading(&self, file: &FileInfo, entry: Option<&catalog::Entry>) -> bool {
        let Some(entry) = entry else { return true };
        if entry.size != file.size || entry.modified != file.modified {
            return true;
        }
        if file.reader == extract::Reader::Picture && self.text_in_images && entry.read_at < self.reread_at {
            return true;
        }
        // Its text left out before, and its folder reads it now.
        if file.reader == extract::Reader::Picture && entry.skipped && self.text_in_images && self.kinds.on(&file.path, "image-text") {
            return true;
        }
        // Failures are tried again after a week (a newer reader may do better).
        entry.state == catalog::State::Failed && entry.read_at < work::now() - 7 * 86_400
    }

    fn compare(&self, files: Vec<FileInfo>, known: &HashMap<u64, catalog::Entry>, seen: &mut HashSet<u64>, jobs: &mut Vec<FileInfo>) {
        for file in files {
            let id = catalog::hashed(&catalog::id(&file.path));
            if self.needs_reading(&file, known.get(&id)) {
                // Kept online only: not read (it would be downloaded) unless Activity's "Download and read" asks, and not
                // counted, as when walking; listed among what couldn't be read.
                if scan::online_only(&file.path) {
                    self.online.borrow_mut().push(file.path);
                    continue;
                }
                jobs.push(file);
            }
            seen.insert(id);
        }
    }
}

/// Follows Windows' change reports: each batch settles for a moment (an app saving writes several times), then
/// what changed is read first.
fn follow(changes: std::sync::mpsc::Receiver<Vec<Change>>, queue: &Queue, sender: &Writes, lister: &Lister,
          roots: &[String], everything: &everything::Everything, probe: &Path, control: &Control) {
    let mut pending: HashMap<PathBuf, Change> = HashMap::new();
    let mut since = Instant::now();
    loop {
        if control.stopping() {
            return;
        }
        // Nothing settling: wait for Windows' next report (dropping the watcher at a stop ends its threads, and with
        // them the channel). Otherwise just until the batch has settled.
        let received = if pending.is_empty() {
            changes.recv().map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected)
        } else {
            changes.recv_timeout(Duration::from_millis(1500).saturating_sub(since.elapsed()).max(Duration::from_millis(50)))
        };
        match received {
            Ok(batch) => {
                if pending.is_empty() {
                    since = Instant::now();
                }
                for change in batch {
                    let path = match &change {
                        Change::Changed(path) | Change::Removed(path) | Change::Lost(path) => path.clone(),
                    };
                    pending.insert(path, change);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if pending.is_empty() || since.elapsed() < Duration::from_millis(1500) {
            continue;
        }
        for (path, change) in pending.drain() {
            match change {
                Change::Changed(_) => {
                    if path.is_dir() {
                        // A folder moved or copied in: what's in it.
                        let mut files = Vec::new();
                        scan::walk(&path, lister.exclusions, lister.pictures, &mut files, &|| control.stopping());
                        for file in files {
                            queue.push(Job::new(file), work::NOW);
                        }
                    } else if let Some(file) = scan::file(&path, lister.exclusions, lister.pictures) {
                        let entry = lister.catalog.get(&file.path);
                        if lister.needs_reading(&file, entry.as_ref()) {
                            queue.push(Job::new(file), work::NOW);
                        }
                    }
                }
                Change::Removed(_) => {
                    let text = path.display().to_string();
                    let _ = sender.send(Write::Remove(text.clone()));
                    for inside in lister.catalog.under(&text) {
                        let _ = sender.send(Write::Remove(inside));
                    }
                }
                Change::Lost(root) => {
                    // Too many changes at once for Windows to report: look again.
                    let root = root.display().to_string();
                    log(&format!("{root}: changes were lost; listing it again"));
                    if roots.iter().any(|known| known.eq_ignore_ascii_case(&root)) {
                        let known = lister.catalog.all_hashed();
                        let mut seen = HashSet::new();
                        let mut jobs = Vec::new();
                        let live = everything.health(probe, Duration::from_secs(600)) == everything::Health::Live;
                        lister.compare(scan::list(&root, lister.exclusions, lister.pictures, live.then_some(everything), &|| control.stopping()), &known, &mut seen, &mut jobs);
                        lister.catalog.online_add(&lister.online.take());
                        for file in jobs {
                            queue.push(Job::new(file), work::NOW - 1);
                        }
                    }
                }
            }
        }
    }
}

/// For each chosen folder, whether it's inside none of the others (a folder chosen inside another: false), unless an
/// excluded folder between them keeps it out of that one's reading: then it counts on its own. The same folder twice
/// counts once, the first.
fn outermost(roots: &[String], exclusions: &config::Exclusions) -> Vec<bool> {
    let key = |root: &String| format!("{}\\", root.trim_end_matches('\\').to_lowercase());
    let keys: Vec<String> = roots.iter().map(key).collect();
    keys.iter().enumerate().map(|(i, mine)| {
        !keys.iter().enumerate().any(|(j, other)| j != i && mine.starts_with(other.as_str()) && (mine != other || j < i)
            && !exclusions.cuts_off(other, mine))
    }).collect()
}

/// Writes status.json every second while working (once when idle), with each folder's progress. Also the one that
/// sees everything waiting has been read, for the schedule.
#[allow(clippy::too_many_arguments)]
fn report(paths: &Paths, catalog: &Catalog, shared: &Shared, queue: &Queue, (roots, outer, gone): (&[String], &[bool], &[String]), [gate, seeing_gate, listen_gate]: [&Gate; 3],
          deep: bool, engines: &[seeing::Engine], relook_at: i64, heard: Option<(listening::Ear, i64, bool, bool)>, control: &Control) {
    let mut last = String::new();
    let mut folders_at = Instant::now() - Duration::from_secs(60);
    // The catalog as last counted: idle, nothing changes it, so there's nothing to count again.
    let mut counted = None;
    let mut quiet_since = Instant::now();
    let mut seeing_quiet_since = Instant::now();
    let mut listen_quiet_since = Instant::now();
    let mut deep_queued = 0;
    let mut looking_queued = 0;
    let mut listen_queued = 0;
    let mut video_text_queued = 0;
    // The model servers' priority as last set: (process, priority), Thorough's and what images show's.
    let mut set = [(0u32, winsys::process::Priority::Idle); 2];
    // Windows kept awake while a kind at Full speed has work left (`work::keep_awake`); let go when dropped.
    let mut awake = winsys::power::KeepAwake::new("Excavate is reading your files at full speed");
    // The CPU time each of the indexer's processes had at the last look, and how many threads they kept busy lately.
    let mut cpu_before: HashMap<u32, Duration> = HashMap::new();
    let (mut cpu_at, mut cpu_threads) = (Instant::now(), 0.0f64);
    let threads = std::thread::available_parallelism().map_or(0, |cores| cores.get() as u32);
    loop {
        let stopping = control.stopping();
        // Files a model's reader stopped on twice: among what couldn't be read.
        for path in work::take_blamed() {
            catalog.skipped_add(&path, config::SKIP_STOPPED);
        }
        // "Everything now" (Activity's "Full speed" among them) for their kinds: the servers go to normal priority, as
        // the reader does, rather than idle, so what the person runs doesn't push them aside; back when it's over.
        if std::env::var_os("EXCAVATE_FOREGROUND").is_none() {
            let wanted = [(shared.server.load(Ordering::Relaxed), work::process_priority(shared.speed(gate))),
                (shared.seeing_server.load(Ordering::Relaxed), work::process_priority(shared.speed(seeing_gate)))];
            for (last, now) in set.iter_mut().zip(wanted) {
                if now.0 != 0 && *last != now && winsys::process::set_process_priority(now.0, now.1) {
                    *last = now;
                }
            }
        }
        let (text, queued_pictures, later) = queue.lengths();
        let open = gate.open();
        let seeing_open = seeing_gate.open();
        let listen_open = listen_gate.open();
        // Pictures waiting for their time aren't work to do now.
        let pictures = if open { queued_pictures } else { 0 };
        let busy = shared.busy.load(Ordering::SeqCst) > 0;
        let listing = shared.listing.load(Ordering::SeqCst);
        // The counts below are of the catalog as it is now (else "everything now" would be judged done on counts from
        // before the recordings and images were in it: at the start, before anything was counted at all).
        let counts_current = counted == Some(catalog.version());
        // Nothing heavy to do: nothing to look at the PC for.
        if pictures == 0 && deep_queued == 0 && looking_queued == 0 && listen_queued == 0 && (!open || video_text_queued == 0) {
            shared.wants_load.store(false, Ordering::Relaxed);
        }
        if gate.all() && !listing && counts_current && !busy && text + queued_pictures + deep_queued + video_text_queued == 0 {
            log("everything has been read, in one sitting");
            gate.all_finished();
            work::set_priority(shared, &[gate, seeing_gate, listen_gate]);
        }
        // (What images show is looked at once their text is read.)
        if seeing_gate.all() && !listing && counts_current && queued_pictures + looking_queued == 0 {
            log("every image has been looked at, in one sitting");
            seeing_gate.all_finished();
            work::set_priority(shared, &[gate, seeing_gate, listen_gate]);
        }
        if listen_gate.all() && !listing && counts_current && listen_queued == 0 {
            log("every recording has been listened to, in one sitting");
            listen_gate.all_finished();
            work::set_priority(shared, &[gate, seeing_gate, listen_gate]);
        }
        // The first time, what the first sweep set aside waits for the schedule too.
        let to_read = if gate.first_run() { pictures.saturating_sub(later) } else { pictures };
        if busy || to_read > 0 || (open && deep_queued + video_text_queued > 0) || listing {
            quiet_since = Instant::now();
        } else if open && gate.waits() && !gate.all() && quiet_since.elapsed() > Duration::from_secs(60) {
            log("everything waiting has been read: pictures wait for their time again");
            gate.finished();
        }
        if (seeing_open && looking_queued > 0) || listing {
            seeing_quiet_since = Instant::now();
        } else if seeing_open && seeing_gate.waits() && !seeing_gate.all() && seeing_quiet_since.elapsed() > Duration::from_secs(60) {
            log("every image waiting has been looked at: new ones wait for their time again");
            seeing_gate.finished();
        }
        if (listen_open && listen_queued > 0) || listing {
            listen_quiet_since = Instant::now();
        } else if listen_open && listen_gate.waits() && !listen_gate.all() && listen_quiet_since.elapsed() > Duration::from_secs(60) {
            log("every recording waiting has been listened to: new ones wait for their time again");
            listen_gate.finished();
        }
        // Counts every 3 s while the catalog changes; at once when the work runs out (so "idle" comes with the final
        // numbers). Each count reads every row under each folder, so an unchanged catalog isn't counted again.
        let version = catalog.version();
        let changed = counted != Some(version);
        if (changed && folders_at.elapsed() >= Duration::from_secs(3)) || stopping || (text + pictures == 0 && !busy && last.contains("\"reading\"")) {
            counted = Some(version);
            let folders: Vec<Folder> = roots.iter().map(|root| {
                let counts = catalog.counts(root);
                // Which step each folder is on: the first model that looks, the one that listens.
                let (seen, to_see) = engines.first().map_or((0, 0), |engine| catalog.seen_counts_in(root, engine.name(), relook_at));
                let (heard, to_hear) = match heard {
                    Some((ear, relisten_at, true, _)) => catalog.recording_counts_in(root, ear.name(), relisten_at),
                    _ => (0, 0),
                };
                // (Gone, or its drive unplugged: said so, not "0 files".)
                let missing = !Path::new(root).is_dir();
                Folder { path: root.clone(), files: counts.files, read: counts.read, failed: counts.failed, pictures: counts.pictures, seen, to_see, heard, to_hear, missing }
            }).chain(gone.iter().map(|folder| Folder { path: folder.clone(), missing: !Path::new(folder).is_dir(), ..Folder::default() })).collect();
            let mut status = shared.status();
            // A folder chosen inside another is in that one's counts already: once each in the totals.
            let total = |count: fn(&Folder) -> u64| folders.iter().zip(outer).filter(|(_, outer)| **outer).map(|(folder, _)| count(folder)).sum::<u64>();
            status.read = total(|folder| folder.read);
            status.failed = total(|folder| folder.failed);
            status.images_read = total(|folder| folder.pictures);
            status.folders = folders;
            status.errors = catalog.errors(12);
            (status.problems, status.problem_files) = catalog.problems();
            if deep {
                deep_queued = catalog.deep_count();
                status.deep_done = catalog.deep_done(deep::ENGINE);
            }
            status.deep_queued = deep_queued;
            looking_queued = 0;
            for (slot, (looking, engine)) in status.looking.iter_mut().zip(engines).enumerate() {
                (looking.done, looking.queued) = catalog.seen_counts(engine.name(), relook_at);
                // (SigLIP beside another model doesn't look at videos: looking.rs.)
                if slot == 0 || *engine != seeing::Engine::Siglip {
                    (looking.videos_done, looking.videos_queued) = catalog.video_counts(engine.name(), relook_at, config::video_kind::SHOWN);
                }
                if *engine == seeing::Engine::Gemma {
                    (looking.documents_done, looking.documents_queued) = catalog.text_counts(seeing::Engine::GemmaText.name(), relook_at);
                }
                looking_queued += looking.queued + looking.videos_queued + looking.documents_queued;
            }
            if let Some(reading) = status.video_text.as_mut() {
                (reading.done, reading.queued) = catalog.video_counts(video_text::ENGINE, 0, config::video_kind::TEXT);
                video_text_queued = reading.queued;
            }
            if let (Some((ear, relisten_at, recordings, videos)), Some(listening)) = (heard, status.listening.as_mut()) {
                if recordings {
                    (listening.done, listening.queued) = catalog.recording_counts(ear.name(), relisten_at);
                }
                if videos {
                    (listening.videos_done, listening.videos_queued) = catalog.video_counts(ear.said_name(), relisten_at, config::video_kind::SAID);
                }
                listen_queued = listening.queued + listening.videos_queued;
            }
            folders_at = Instant::now();
        }
        let snapshot = {
            let mut status = shared.status();
            status.queued_text = text;
            status.queued_pictures = queued_pictures;
            status.queued_scanned = queue.scanned_queued();
            status.queued_later = later;
            status.reading_all = gate.all();
            status.pictures_wait = !open;
            status.pictures_at = gate.next();
            status.seeing_wait = !seeing_open;
            status.seeing_at = seeing_gate.next();
            status.seeing_all = seeing_gate.all();
            status.listen_wait = !listen_open;
            status.listen_at = listen_gate.next();
            status.listen_all = listen_gate.all();
            let deep_waiting = if open { deep_queued } else { 0 } + if seeing_open { looking_queued } else { 0 }
                + if listen_open { listen_queued } else { 0 };
            // Why it isn't reading, if it isn't: documents and code go on while the models wait. A kind paused on its own
            // doesn't say so for all of reading, unless every kind with work is.
            let reason = {
                let paused = shared.paused.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                status.waits = [
                    if text > 0 { paused[0].clone().unwrap_or_default() } else { String::new() },
                    if pictures > 0 { paused[1].clone().unwrap_or_default() } else { String::new() },
                    if deep_waiting > 0 { paused[2].clone().unwrap_or_default() } else { String::new() },
                ];
                let with_work: Vec<&Option<String>> = [(text > 0, &paused[0]), (pictures > 0, &paused[1]), (deep_waiting > 0, &paused[2])]
                    .into_iter().filter(|(work, _)| *work).map(|(_, reason)| reason).collect();
                let kind_paused = |reason: &&Option<String>| reason.as_deref() == Some(config::status::KIND_PAUSED);
                if !with_work.is_empty() && with_work.iter().all(kind_paused) {
                    Some(config::status::KIND_PAUSED.to_string())
                } else {
                    with_work.into_iter().find(|reason| !kind_paused(reason)).cloned().flatten()
                }
            };
            status.deep_running = shared.deep_on.load(Ordering::SeqCst);
            status.memory_mb = winsys::process_memory().1 >> 20;
            // Threads kept busy: the CPU time used since the last look, by this process and the model servers it runs
            // (one only counted from its second look, so its time before isn't taken for now's), averaged a little.
            // And the processes it started (each documents' worker reads in one: `documents`).
            let mut processes = vec![std::process::id(), shared.server.load(Ordering::Relaxed), shared.seeing_server.load(Ordering::Relaxed),
                                     shared.listen_server.load(Ordering::Relaxed)];
            processes.extend(winsys::process::children(std::process::id()));
            let now: HashMap<u32, Duration> = processes.iter().filter(|&&process| process != 0).map(|&process| (process, winsys::load::process_cpu(process))).collect();
            let used: Duration = now.iter().filter_map(|(process, time)| cpu_before.get(process).map(|before| time.saturating_sub(*before))).sum();
            let wall = cpu_at.elapsed().as_secs_f64();
            if wall > 0.2 {
                let busy = used.as_secs_f64() / wall;
                cpu_threads = if cpu_threads == 0.0 { busy } else { cpu_threads * 0.6 + busy * 0.4 };
                (cpu_before, cpu_at) = (now, Instant::now());
            }
            status.cpu_threads = cpu_threads.round() as u32;
            status.threads = threads;
            shared.paces.show(&mut status);
            (status.state, status.detail) = if stopping {
                ("stopped".into(), String::new())
            } else if listing {
                ("listing".into(), "Looking through your folders".into())
            } else if let Some(reason) = reason.filter(|reason| !reason.starts_with("Going slower")) {
                ("paused".into(), reason)
            } else if text + pictures + deep_waiting > 0 || busy {
                ("reading".into(), String::new())
            } else {
                ("idle".into(), String::new())
            };
            if status.state == "idle" {
                status.current.clear();
                status.deep_current.clear();
            }
            status.clone()
        };
        if let Some(awake) = awake.as_mut() {
            let left = [text + pictures + deep_queued + video_text_queued + u64::from(busy || listing), looking_queued, listen_queued];
            let wanted = !stopping && work::keep_awake(shared, [gate, seeing_gate, listen_gate], left);
            if awake.is_on() != wanted && awake.set(wanted) {
                log(if wanted { "full speed with work left: Windows kept awake" } else { "Windows may sleep again" });
            }
        }
        let comparable = serde_json::to_string(&snapshot).unwrap_or_default();
        if comparable != last {
            let mut stamped = snapshot;
            stamped.updated = work::now();
            stamped.save(paths);
            last = comparable;
        }
        if stopping {
            return;
        }
        control.sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::outermost;

    #[test]
    fn a_folder_inside_another_counts_once() {
        let roots = [r"C:\Users\me\Desktop\Home\Photos", r"C:\Users\me\Desktop", r"C:\Users\me\Desktop2", r"c:\users\me\desktop\"]
            .map(String::from);
        let none = config::Exclusions::new(&[]);
        assert_eq!(outermost(&roots, &none), [false, true, true, false]);
        assert_eq!(outermost(&[r"D:\".to_string(), r"D:\Music".to_string()], &none), [true, false]);
        // Desktop, but not Desktop\Home: a folder chosen in Home is read on its own, so it counts on its own.
        let home = config::Exclusions::new(&[r"C:\Users\me\Desktop\Home".to_string()]);
        assert_eq!(outermost(&roots, &home), [true, true, true, false]);
    }
}
