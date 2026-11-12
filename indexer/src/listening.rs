//! Recordings by what's said in them: Excavate's listener (tools/listen: whisper.cpp's Whisper or Parakeet, one model
//! loaded, a process of its own) hears each recording in the chosen folders, newest first, and the words go into the
//! content index with when they were said (`marks`: `{"times": [[byte, ms], ...]}`), so a result opens at the moment.
//! Windows decodes the sound (`winsys::audio`, every format it plays); the listener gets it ten minutes at a time.

use crate::catalog::Catalog;
use crate::models::{self, File};
use crate::work::{self, Context, Need, Write};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write as _};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Sound handed over at a time: ten minutes (38 MB of samples), so a long recording never fills memory and a pause or a
/// stop comes between pieces.
const PIECE_SECONDS: usize = 600;

/// The model that listens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ear {
    /// Quick: Parakeet TDT 0.6B v3, 25 European languages, on the processor.
    Parakeet,
    /// Balanced: Whisper large-v3-turbo, nearly every language, on the graphics card when there's one able.
    Turbo,
    /// Thorough: Whisper large-v3, on the graphics card.
    Large,
}

impl Ear {
    /// Every model's name, as the catalog keeps who heard what.
    pub const NAMES: [&'static str; 3] = ["parakeet", "whisper-turbo", "whisper-large"];

    pub fn of(choice: &str) -> Ear {
        match choice {
            "quick" => Ear::Parakeet,
            "deep" => Ear::Large,
            _ => Ear::Turbo,
        }
    }

    pub fn name(self) -> &'static str {
        Ear::NAMES[self as usize]
    }

    /// What's said in videos, as the catalog keeps who heard it (apart from recordings).
    pub const SAID_NAMES: [&'static str; 3] = ["said-parakeet", "said-whisper-turbo", "said-whisper-large"];

    pub fn said_name(self) -> &'static str {
        Ear::SAID_NAMES[self as usize]
    }

    pub fn label(self) -> &'static str {
        match self {
            Ear::Parakeet => "Parakeet",
            Ear::Turbo => "Whisper turbo",
            Ear::Large => "Whisper large-v3",
        }
    }

    fn file(self) -> &'static File {
        match self {
            Ear::Parakeet => &models::PARAKEET,
            Ear::Turbo => &models::WHISPER_TURBO,
            Ear::Large => &models::WHISPER_LARGE,
        }
    }

    fn whisper(self) -> bool {
        self != Ear::Parakeet
    }

    /// The Audio page's choice it is: "quick", "balanced" or "deep".
    pub fn choice(self) -> &'static str {
        match self {
            Ear::Parakeet => "quick",
            Ear::Turbo => "balanced",
            Ear::Large => "deep",
        }
    }
}

/// How it runs: the model, and the graphics card's way in ("vulkan" or "cuda"), or none for the processor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub ear: Ear,
    pub gpu: Option<&'static str>,
}

impl Plan {
    /// The choice as this PC can take it: Balanced on the graphics card when there's one able, else the processor;
    /// Thorough only with one (settings offers it only then; without, it's Balanced); CUDA only on NVIDIA's, published.
    pub fn of(settings: &config::Settings, measured: &config::devices::Measured) -> Plan {
        let card = winsys::gpu::capable();
        let ear = match Ear::of(&settings.listening) {
            Ear::Large if card.is_none() => Ear::Turbo,
            ear => ear,
        };
        let nvidia = card.as_ref().is_some_and(|card| card.name.to_ascii_lowercase().contains("nvidia"));
        let gpu = match settings.listen_device(ear.choice(), card.is_some(), nvidia && models::listen_cuda_offered(), measured) {
            "cuda" => Some("cuda"),
            "vulkan" => Some("vulkan"),
            _ => None,
        };
        Plan { ear, gpu }
    }

    fn cuda(&self) -> bool {
        self.gpu == Some("cuda")
    }

    pub fn device_label(&self) -> &'static str {
        match self.gpu {
            Some("cuda") => "CUDA",
            Some(_) => "Vulkan",
            None => "CPU",
        }
    }
}

/// Times hearing a minute of `path` on the processor and on the graphics card. Where it was fastest: None
/// for the processor, or the graphics card's way; None altogether when it couldn't be measured.
fn measure_devices(context: &Context, models: &Path, plan: &Plan, key: &str, path: &str) -> Option<Option<&'static str>> {
    let way = plan.gpu.or(Some("vulkan"));
    let rate = winsys::audio::RATE as usize;
    let mut minute: Vec<f32> = Vec::with_capacity(70 * rate);
    let _ = winsys::audio::samples(path, &mut |samples| {
        minute.extend_from_slice(samples);
        minute.len() < 65 * rate
    });
    // Five seconds to warm up, then the minute.
    if minute.len() < 65 * rate {
        return None;
    }
    let pieces = [minute[..5 * rate].to_vec(), minute[5 * rate..65 * rate].to_vec()];
    let languages = languages();
    let devices: Vec<&str> = if plan.gpu.is_some() { vec![way.unwrap_or("vulkan"), "cpu"] } else { vec!["cpu", way.unwrap_or("vulkan")] };
    let times = crate::devices::race(&devices, &pieces,
        |device| Listener::start(models, &Plan { ear: plan.ear, gpu: (device != "cpu").then_some(way.unwrap_or("vulkan")) }),
        |listener, samples| listener.hear(samples, 0, &languages).map(|_| ()),
        &|| context.control.stopping());
    if times.len() < 2 {
        return None;
    }
    let fastest = crate::devices::keep(&context.paths, key, times, "a minute")?;
    Some((fastest != "cpu").then_some(way.unwrap_or("vulkan")))
}

/// The person's languages for Whisper ("en,ar"), from Windows' language settings: it chooses among these when it isn't
/// sure (told, Hindi's mistakes halve).
pub fn languages() -> String {
    let mut codes: Vec<String> = Vec::new();
    for tag in winsys::registry::languages() {
        let code = tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        if (2..=3).contains(&code.len()) && !codes.contains(&code) {
            codes.push(code);
        }
    }
    if codes.is_empty() { "-".into() } else { codes.join(",") }
}

// ---- The listener ---------------------------------------------------------------------------------------------------

/// What it heard in a piece of sound: (from ms, to ms, words).
struct Heard {
    pieces: Vec<(i64, i64, String)>,
    /// How much of it was speech, as the voice detector found (ms), and the detector's time and the whole time.
    speech_ms: i64,
    vad_ms: i64,
    ms: i64,
}

/// The listener's process, the model loaded.
struct Listener {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
    _job: Option<winsys::process::Job>,
    on_gpu: bool,
    /// Below normal ("Listen to everything now"), else idle: what it was last set to.
    hurried: bool,
}

impl Listener {
    fn start(models: &Path, plan: &Plan) -> Result<Listener, String> {
        let speech = models::speech_folder(models);
        let folder = models::listen_folder(models, plan.cuda());
        // (A PC without Microsoft's C++ runtime: the copy beside Excavate's programs, or it can't start.)
        models::with_runtime(&folder);
        let exe = folder.join("excavate-listen.exe");
        Listener::launch(&exe, &speech.join(plan.ear.file().name), plan.ear.whisper(), &speech.join(models::SILERO.name), plan.gpu)
    }

    /// The listener at `exe` with `model` loaded (Whisper's or Parakeet's), Silero's voice detector first.
    fn launch(exe: &Path, model: &Path, whisper: bool, vad: &Path, gpu: Option<&str>) -> Result<Listener, String> {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
        let mut command = Command::new(exe);
        command.creation_flags(CREATE_NO_WINDOW | IDLE_PRIORITY_CLASS).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        // Vulkan lists a processor's own graphics first, and whisper.cpp would take it: only the dedicated card is shown.
        if gpu.is_some() {
            match dedicated_vulkan(exe) {
                Some(index) => {
                    command.env("GGML_VK_VISIBLE_DEVICES", index.to_string());
                }
                None if gpu == Some("vulkan") => return Err("Vulkan doesn't see a graphics card of its own".into()),
                None => {}
            }
        }
        command.arg(if whisper { "--whisper" } else { "--parakeet" }).arg(model)
            .args(["--backend", gpu.unwrap_or("cpu"), "--threads", &crate::ocr::threads().to_string()])
            .arg("--vad").arg(vad);
        let mut child = command.spawn().map_err(|error| format!("can't start the listener: {error}"))?;
        let job = winsys::process::Job::new();
        if let Some(job) = &job {
            job.add(child.id());
        }
        let input = child.stdin.take().ok_or("no input")?;
        let output = child.stdout.take().ok_or("no output")?;
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("listener".into()).spawn(move || {
            // (Read as bytes: a listener that sends a broken character mustn't look stopped.)
            let mut output = BufReader::new(output);
            let mut line = Vec::new();
            loop {
                line.clear();
                match output.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if sender.send(String::from_utf8_lossy(&line).trim_end().to_string()).is_err() {
                    break;
                }
            }
        }).map_err(|error| error.to_string())?;
        let mut listener = Listener { child, input, lines, _job: job, on_gpu: gpu.is_some(), hurried: false };
        // Loading a model takes seconds; a few minutes means something's wrong.
        let ready = listener.answer(Duration::from_secs(180))?;
        if ready.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(ready["error"].as_str().unwrap_or("the listener didn't start").to_string());
        }
        Ok(listener)
    }

    fn process(&self) -> u32 {
        self.child.id()
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn answer(&mut self, wait: Duration) -> Result<serde_json::Value, String> {
        match self.lines.recv_timeout(wait) {
            Ok(line) => serde_json::from_str(&line).map_err(|error| format!("the listener's answer: {error}")),
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                Err("the listener took too long".into())
            }
            Err(RecvTimeoutError::Disconnected) => Err(match self.child.try_wait() {
                Ok(Some(status)) => format!("the listener stopped ({status})"),
                _ => "the listener stopped".into(),
            }),
        }
    }

    /// What's said in `samples` (16 kHz mono), `offset_ms` into the recording.
    fn hear(&mut self, samples: &[f32], offset_ms: i64, languages: &str) -> Result<Heard, String> {
        let mut bytes = Vec::with_capacity(samples.len() * 4 + 64);
        bytes.extend_from_slice(format!("LISTEN {} auto {languages}\n", samples.len()).as_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.input.write_all(&bytes).and_then(|()| self.input.flush()).map_err(|error| format!("the listener: {error}"))?;
        // On the processor a piece can take a while (Whisper turbo there: a quarter of its length); far longer is stuck.
        let seconds = samples.len() as u64 / u64::from(winsys::audio::RATE);
        let wait = Duration::from_secs(60 + if self.on_gpu { seconds / 4 } else { seconds * 2 });
        let answer = self.answer(wait)?;
        if let Some(error) = answer["error"].as_str() {
            return Err(error.to_string());
        }
        let pieces = answer["pieces"].as_array().map(|pieces| {
            pieces.iter().filter_map(|piece| {
                let text = piece["text"].as_str()?.trim();
                (!text.is_empty()).then(|| (offset_ms + piece["t0"].as_i64().unwrap_or(0), offset_ms + piece["t1"].as_i64().unwrap_or(0), text.to_string()))
            }).collect()
        }).unwrap_or_default();
        let number = |name: &str| answer[name].as_i64().unwrap_or(0);
        Ok(Heard { pieces, speech_ms: answer["speech_ms"].as_i64().unwrap_or(-1), vad_ms: number("vad_ms"), ms: number("ms") })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The dedicated card's place in Vulkan's own list (the listener's `--list`: Vulkan's devices first, in its order).
fn dedicated_vulkan(exe: &Path) -> Option<usize> {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new(exe).arg("--list").creation_flags(CREATE_NO_WINDOW).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    // "0\tVulkan\tNVIDIA GeForce RTX 4080 Laptop GPU\tgpu\t11995": its place among Vulkan's, by the name of the card
    // Excavate runs on; else the first card of its own ("igpu" is a processor's).
    let vulkan: Vec<&str> = text.lines().filter(|line| line.split('\t').nth(1) == Some("Vulkan")).collect();
    let plain = |text: &str| text.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let card = winsys::gpu::capable().map(|card| plain(&card.name));
    let named = card.and_then(|card| vulkan.iter().position(|line| line.split('\t').nth(2).is_some_and(|name| plain(name) == card)));
    let index = named.or_else(|| vulkan.iter().position(|line| line.split('\t').nth(3) == Some("gpu")));
    config::log(&format!("listener: Vulkan device {}", index.map_or("none".to_string(), |index| format!("{index}: {}", vulkan[index].split('\t').nth(2).unwrap_or("")))));
    index
}

// ---- What it heard, for the index ------------------------------------------------------------------------------------

/// The words, one piece a line, and where each piece starts in them with its moment: `[[byte, ms], ...]`.
pub fn transcript(pieces: &[(i64, i64, String)]) -> (String, Vec<(usize, i64)>) {
    let mut body = String::new();
    let mut times = Vec::with_capacity(pieces.len());
    for (t0, _, text) in pieces {
        if !body.is_empty() {
            body.push('\n');
        }
        times.push((body.len(), *t0));
        body.push_str(text);
    }
    (body, times)
}

// ---- The lane -------------------------------------------------------------------------------------------------------

/// Listens to recordings while its gate is open and the PC can spare it. `relisten_at`: every recording heard before
/// it is heard again ("Listen to my recordings again").
/// `recordings`, `videos`: what it listens to; recordings first (they're mostly speech), then what's said in videos.
pub fn lane(mut context: Context, catalog: Arc<Catalog>, models: PathBuf, mut plan: Plan, relisten_at: i64, recordings: bool, videos: bool) {
    winsys::com_mta();
    let ear = plan.ear;
    let said = ear.said_name();
    let need = if plan.gpu.is_some() { Need::Gpu } else { Need::Cpu };
    let languages = if ear.whisper() { languages() } else { "-".into() };
    config::log(&format!("listening: {} on {}, languages {languages}", ear.label(), plan.device_label()));
    let mut listener: Option<Listener> = None;
    let mut idle_since = Instant::now();
    let mut tidied = Instant::now() - Duration::from_secs(3600);
    let mut listed = Instant::now();
    let mut minutes: VecDeque<f64> = VecDeque::new();
    let set_server = |listener: &Option<Listener>| {
        let process = listener.as_ref().filter(|listener| listener.on_gpu).map_or(0, Listener::process);
        context.shared.listen_server.store(process, Ordering::Relaxed);
    };
    let status = |change: &dyn Fn(&mut config::status::Listening)| {
        if let Some(listening) = context.shared.status().listening.as_mut() {
            change(listening);
        }
    };
    let noting = |reason: Option<&str>| status(&|listening| listening.waiting = reason.unwrap_or_default().to_string());
    let mut failures = 0usize;
    let mut upgrade_tried = false;
    loop {
        if context.control.stopping() {
            break;
        }
        // New recordings since the session's listing, as Everything has them (every ten minutes; no walk). Only added:
        // what's gone goes at the next session's listing, which sees every folder.
        if recordings && listed.elapsed() > Duration::from_secs(600) {
            let everything = everything::Everything::shared();
            if everything.status().available {
                let recordings: Vec<(String, u64, i64)> = context.roots.iter()
                    .filter(|root| context.kinds.on(root, "audio"))
                    .flat_map(|root| crate::scan::list_recordings(root, &context.exclusions, Some(&everything), false))
                    .collect();
                catalog.recordings_add(&recordings);
            }
            listed = Instant::now();
        }
        // Gone from the chosen folders: its words go too (now and then, and at the start). (Videos gone: at the listing.)
        if recordings && tidied.elapsed() > Duration::from_secs(300) {
            let gone = catalog.heard_gone(&Ear::NAMES);
            for id in &gone {
                let _ = context.sender.send(Write::Remove(id.clone()));
                catalog.heard_forget(id, &Ear::NAMES);
            }
            if !gone.is_empty() {
                config::log(&format!("listening: {} recordings gone", gone.len()));
            }
            tidied = Instant::now();
        }
        if !context.listen_gate.open() {
            listener = None;
            set_server(&listener);
            status(&|listening| listening.running = false);
            if !context.control.sleep(Duration::from_secs(10)) {
                break;
            }
            continue;
        }
        let next = recordings.then(|| catalog.recording_next(ear.name(), relisten_at, 1).pop()).flatten().map(|file| (file, false))
            .or_else(|| videos.then(|| catalog.video_next(said, relisten_at, 1, config::video_kind::SAID).pop()).flatten().map(|file| (file, true)));
        let Some(((path, size, modified), video)) = next else {
            if listener.is_some() && idle_since.elapsed() > Duration::from_secs(60) {
                listener = None;
                set_server(&listener);
            }
            status(&|listening| {
                listening.running = listener.is_some();
                listening.current.clear();
            });
            if !context.control.sleep(Duration::from_secs(5)) {
                break;
            }
            continue;
        };
        // What it needs, once. A download takes a while, and waits while listening is paused.
        if !models::speech_present(&models, ear.file(), plan.cuda()) {
            if let Some(why) = work::listen_pause(&context, Need::Little) {
                noting(Some(&why));
                if !context.control.sleep(Duration::from_secs(10)) {
                    break;
                }
                continue;
            }
            let total = models::speech_download_size(&models, ear.file(), plan.cuda());
            status(&|listening| listening.model = config::status::Model { state: "downloading".into(), name: ear.label().into(), total, ..Default::default() });
            let shared = context.shared.clone();
            let result = models::speech_ensure(&models, ear.file(), plan.cuda(), context.control, &mut |received, total| {
                if let Some(listening) = shared.status().listening.as_mut() {
                    listening.model.received = received;
                    listening.model.total = total;
                }
            });
            if let Err(error) = result {
                config::log(&format!("listening, {}: {error}", ear.label()));
                status(&|listening| listening.model = config::status::Model { state: "failed".into(), name: ear.label().into(), error: error.clone(), ..Default::default() });
                let wait = [30, 120, 600][failures.min(2)];
                failures += 1;
                if !context.control.sleep(Duration::from_secs(wait)) {
                    break;
                }
                continue;
            }
            failures = 0;
            status(&|listening| listening.model = config::status::Model { state: "ready".into(), ..Default::default() });
        }
        context.shared.wants_load.store(true, Ordering::Relaxed);
        let _busy = work::Busy::new(&context.shared);
        if !work::wait_listening(&context, need, &noting) {
            break;
        }
        // Who heard what: a video's speech is kept apart from recordings.
        let engine = if video { said } else { ear.name() };
        // Gone, changed since it was listed, kept online only, or no longer in what's read: not now.
        let current = std::fs::metadata(&path).ok().filter(|metadata| metadata.is_file()).map(|metadata| (metadata.len(), crate::scan::seconds(&metadata)));
        if current != Some((size, modified)) || crate::scan::online_only(&path) || !crate::scan::in_scope(&path, &context.roots, &context.exclusions)
            || (video && !context.kinds.on(&path, "video-said")) {
            catalog.seen_set(&path, engine, modified, work::now(), false);
            continue;
        }
        if listener.as_mut().is_none_or(|listener| !listener.running()) {
            // (One that stopped is let go first: its program isn't held open.)
            drop(listener.take());
            // A listener from before this build: brought up to date, once a session (it can't be had yet: the one there
            // goes on).
            if !upgrade_tried {
                upgrade_tried = true;
                match models::listen_upgrade(&models, plan.cuda(), context.control) {
                    Ok(true) => config::log("listening: the listener was brought up to date"),
                    Ok(false) => {}
                    Err(error) => config::log(&format!("listening: the listener couldn't be brought up to date ({error}); the one there goes on")),
                }
            }
            status(&|listening| listening.running = true);
            // The first time on this PC: where it hears fastest, on a minute of this file.
            if context.listen_measure.is_some() && crate::devices::quiet(&context) {
                if let Some(key) = context.listen_measure.take() {
                    if let Some(gpu) = measure_devices(&context, &models, &plan, &key, &path) {
                        if gpu != plan.gpu {
                            plan.gpu = gpu;
                            let label = plan.device_label();
                            status(&|listening| listening.device = label.into());
                        }
                    }
                }
            }
            match Listener::start(&models, &plan) {
                Ok(started) => {
                    listener = Some(started);
                    // (A download or a start that failed before is past.)
                    status(&|listening| if listening.model.state == "failed" { listening.model = config::status::Model::default() });
                }
                Err(error) => {
                    config::log(&format!("listening, {}: {error}", ear.label()));
                    status(&|listening| listening.model = config::status::Model { state: "failed".into(), name: ear.label().into(), error: error.clone(), ..Default::default() });
                    listener = None;
                    set_server(&listener);
                    if !context.control.sleep(Duration::from_secs(600)) {
                        return;
                    }
                    continue;
                }
            }
            set_server(&listener);
        }
        let Some(open) = listener.as_mut() else { continue };
        // "Listen to everything now": ahead of what's merely in the background (below normal), as "Read everything now"
        // is; else idle. Asked each recording, as it can be switched while the listener runs.
        let hurry = context.listen_gate.all();
        if hurry != open.hurried && winsys::process::hurry_process(open.child.id(), hurry) {
            open.hurried = hurry;
        }
        status(&|listening| listening.current = path.clone());
        let started = Instant::now();
        let (mut pieces, mut failed, mut stopped, mut skipped) = (Vec::new(), None, false, false);
        let mut buffer: Vec<f32> = Vec::with_capacity(PIECE_SECONDS * winsys::audio::RATE as usize);
        let mut offset_ms = 0i64;
        let piece_len = PIECE_SECONDS * winsys::audio::RATE as usize;
        let mut hear = |buffer: &mut Vec<f32>, offset_ms: &mut i64, open: &mut Listener| -> Result<(), String> {
            let heard = open.hear(buffer, *offset_ms, &languages)?;
            pieces.extend(heard.pieces);
            *offset_ms += (buffer.len() as i64 * 1000) / i64::from(winsys::audio::RATE);
            buffer.clear();
            Ok(())
        };
        let decoded = winsys::audio::samples(&path, &mut |samples| {
            buffer.extend_from_slice(samples);
            if buffer.len() < piece_len {
                return true;
            }
            // Let go by "Skip".
            if context.control.skip_asked(&path) {
                skipped = true;
                return false;
            }
            // Something else needs the PC: the recording is heard again from the start later.
            if context.control.stopping() || !context.listen_gate.open() || work::listen_pause(&context, need).is_some() {
                stopped = true;
                return false;
            }
            match hear(&mut buffer, &mut offset_ms, open) {
                Ok(()) => true,
                Err(error) => {
                    failed = Some(error);
                    false
                }
            }
        });
        if skipped {
            context.control.skipped();
            config::log(&format!("listening: {path}: skipped"));
            catalog.seen_set(&path, engine, modified, work::now(), false);
            catalog.skipped_add(&path, "what's said");
        }
        let result = match (decoded, failed) {
            _ if stopped || skipped => None,
            // A video with no soundtrack: nothing to hear, at once (its picture is never decoded).
            (Err(error), None) if video && error == winsys::audio::NO_SOUND => Some(Ok(0.0)),
            (Err(error), _) | (_, Some(error)) => Some(Err(error)),
            (Ok(seconds), None) => Some(if buffer.is_empty() { Ok(seconds) } else { hear(&mut buffer, &mut offset_ms, open).map(|()| seconds) }),
        };
        match result {
            None => {}
            Some(Ok(seconds)) => {
                let took = started.elapsed().as_secs_f64();
                // Secrets covered piece by piece, so where each piece starts stays true.
                let mut secrets = Vec::new();
                let pieces: Vec<(i64, i64, String)> = pieces.into_iter().map(|(from, to, text)| {
                    let (text, found) = work::private(&text);
                    secrets.extend(found);
                    (from, to, text)
                }).collect();
                let (body, times) = transcript(&pieces);
                let mut marks = serde_json::json!({ "times": times });
                if !secrets.is_empty() {
                    marks["kinds"] = work::kinds(&secrets).into();
                }
                let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
                let doc = content::Doc {
                    path: path.clone(),
                    name,
                    kind: if video { content::kind::VIDEO } else { content::kind::AUDIO },
                    modified,
                    size,
                    source: content::Source::Speech,
                    body,
                    marks: serde_json::to_vec(&marks).unwrap_or_default(),
                };
                // A video where no one speaks: nothing of it in the index for that (what it shows stays).
                if video && pieces.is_empty() {
                    let _ = context.sender.send(Write::Unindex(content::entry_id(&path, true)));
                } else {
                    let _ = context.sender.send(Write::Words(Box::new(doc)));
                }
                catalog.seen_set(&path, engine, modified, work::now(), true);
                config::log(&format!("listening: {path}: {:.0} s of sound, {} pieces, in {took:.1} s", seconds, pieces.len()));
                // (How long a minute of sound takes: recordings', mostly speech.)
                if seconds > 5.0 && !video {
                    minutes.push_back(took / seconds * 60_000.0);
                    if minutes.len() > 32 {
                        minutes.pop_front();
                    }
                    let mut sorted: Vec<f64> = minutes.iter().copied().collect();
                    sorted.sort_by(f64::total_cmp);
                    let median = sorted[sorted.len() / 2].round();
                    status(&|listening| listening.ms_a_minute = Some(median));
                }
            }
            Some(Err(error)) => {
                config::log(&format!("listening on {path}: {error}"));
                catalog.seen_set(&path, engine, modified, work::now(), false);
                // (A listener that stopped is started again for the next one.)
            }
        }
        idle_since = Instant::now();
    }
    set_server(&None);
}

// ---- Measuring ------------------------------------------------------------------------------------------------------

/// `--listen <file> <listener folder> <model> <whisper|parakeet> <cpu|vulkan|cuda> <silero model>`: what the lane does
/// with one recording or video (Windows decodes its sound, the listener hears it ten minutes at a time), timed: the
/// decoding (this process's CPU), the listener's time and CPU, how much was speech; printed as JSON with what was heard.
pub fn measure() {
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|arg| arg == "--listen").unwrap_or(0);
    let [file, folder, model, kind, backend, vad] = std::array::from_fn(|i| args.get(at + 1 + i).cloned().unwrap_or_default());
    if vad.is_empty() {
        eprintln!("--listen <file> <listener folder> <model> <whisper|parakeet> <cpu|vulkan|cuda> <silero model>");
        return;
    }
    winsys::com_mta();
    let gpu = match backend.as_str() {
        "vulkan" => Some("vulkan"),
        "cuda" => Some("cuda"),
        _ => None,
    };
    let languages = if kind == "whisper" { languages() } else { "-".into() };
    let cpu_before = winsys::load::own_cpu();
    let started = Instant::now();
    let mut samples: Vec<f32> = Vec::new();
    let decoded = winsys::audio::samples(&file, &mut |piece| {
        samples.extend_from_slice(piece);
        true
    });
    let decode_s = started.elapsed().as_secs_f64();
    let decode_cpu_s = winsys::load::own_cpu().saturating_sub(cpu_before).as_secs_f64();
    let seconds = match decoded {
        Ok(seconds) => seconds,
        Err(error) => {
            println!("{}", serde_json::json!({ "file": file, "error": error, "decode_s": decode_s, "decode_cpu_s": decode_cpu_s }));
            return;
        }
    };
    let loading = Instant::now();
    let mut listener = match Listener::launch(Path::new(&folder).join("excavate-listen.exe").as_path(), Path::new(&model), kind == "whisper", Path::new(&vad), gpu) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    let load_s = loading.elapsed().as_secs_f64();
    let listener_cpu_loaded = winsys::load::process_cpu(listener.process());
    let hearing = Instant::now();
    let (mut pieces, mut parts) = (Vec::new(), Vec::new());
    let piece_len = PIECE_SECONDS * winsys::audio::RATE as usize;
    for (i, chunk) in samples.chunks(piece_len).enumerate() {
        let offset_ms = (i * PIECE_SECONDS * 1000) as i64;
        match listener.hear(chunk, offset_ms, &languages) {
            Ok(heard) => {
                parts.push(serde_json::json!({ "ms": heard.ms, "speech_ms": heard.speech_ms, "vad_ms": heard.vad_ms, "seconds": chunk.len() / winsys::audio::RATE as usize }));
                pieces.extend(heard.pieces);
            }
            Err(error) => {
                eprintln!("{error}");
                return;
            }
        }
    }
    let listen_s = hearing.elapsed().as_secs_f64();
    let listen_cpu_s = winsys::load::process_cpu(listener.process()).saturating_sub(listener_cpu_loaded).as_secs_f64();
    let pieces: Vec<serde_json::Value> = pieces.iter().map(|(t0, t1, text)| serde_json::json!({ "t0": t0, "t1": t1, "text": text })).collect();
    println!("{}", serde_json::json!({
        "file": file, "seconds": seconds, "decode_s": decode_s, "decode_cpu_s": decode_cpu_s, "load_s": load_s,
        "listen_s": listen_s, "listen_cpu_s": listen_cpu_s, "parts": parts, "pieces": pieces,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices() {
        assert_eq!(Ear::of("quick"), Ear::Parakeet);
        assert_eq!(Ear::of("balanced"), Ear::Turbo);
        assert_eq!(Ear::of("deep"), Ear::Large);
        assert_eq!(Ear::Large.name(), "whisper-large");
    }

    #[test]
    fn words_with_their_moments() {
        let pieces = vec![(0, 2000, "Hello there.".to_string()), (2400, 5000, "The budget is due Friday.".to_string())];
        let (body, times) = transcript(&pieces);
        assert_eq!(body, "Hello there.\nThe budget is due Friday.");
        assert_eq!(times, [(0, 0), (13, 2400)]);
        assert_eq!(&body[times[1].0..times[1].0 + 3], "The");
    }
}
