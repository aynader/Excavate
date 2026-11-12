//! The search helper (`excavate-indexer --seek`): finds images by what they show for the popup, which never loads a model
//! itself and so stays small. Started by Excavate when someone pauses typing, it holds each chosen model's text half
//! (EmbeddingGemma 2 or Qwen in llama.cpp's server, SigLIP 2 in ONNX Runtime) and every image's vector, answers on a port
//! of this PC only, and ends after a few minutes without a search.
//!
//! `POST /search {"text": "a pie", "limit": 60}` answers `{"hits": [{"path", "score", "similarity"}], "ready": [...],
//! "waiting": [...], "ms"}`: models whose text half is still loading are in `waiting`, and the popup asks again when
//! they're ready (Thorough shows EmbeddingGemma 2's ranking at once, the second model's merges in).
//!
//! A time or a place in the description ("last winter", "Prague") is taken out (`seeing::query`) and answered from when
//! and where each image was taken (`seeing::facts`); the rest goes to the models. `filtered` says that happened.

use crate::deep::Server;
use crate::looking::{self, SiglipText};
use crate::models;
use config::{Paths, Settings, log};
use seeing::{Engine, Vectors};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// It ends this long after the last search.
const STAYS: Duration = Duration::from_secs(300);

enum Encoder {
    Llama(Server),
    Siglip(SiglipText),
}

#[derive(Default)]
enum Slot {
    #[default]
    Loading,
    Ready(Encoder),
    Failed(String),
}

struct Model {
    engine: Engine,
    slot: Mutex<Slot>,
    vectors: Mutex<Option<Vectors>>,
    /// Each video's frames averaged (`Vectors::video_means`), and the vectors' version they were made from.
    means: Mutex<((u32, u64), Vec<(String, Vec<f32>)>)>,
    /// Descriptions already turned into vectors (the popup asks again while a second model loads).
    cache: Mutex<HashMap<String, Vec<f32>>>,
}

pub fn serve(paths: &Paths) {
    if !winsys::single_instance(&config::instance_name("Seeker.Running")) {
        return;
    }
    let settings = Settings::load(paths);
    winsys::gpu::choose(&settings.graphics_card);
    let gpu = winsys::gpu::capable().is_some();
    let engines: Vec<Engine> = seeing::engines(settings.seeing_on, &settings.seeing, &settings.seeing_deep);
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            log(&format!("seeker: can't listen: {error}"));
            return;
        }
    };
    let port = listener.local_addr().map(|address| address.port()).unwrap_or(0);
    let said = serde_json::json!({ "port": port, "pid": std::process::id(), "engines": engines.iter().map(|engine| engine.name()).collect::<Vec<_>>() });
    let _ = config::write_atomic(&paths.seeker(), said.to_string().as_bytes());
    log(&format!("seeker: listening on {port} for {:?}", engines.iter().map(|engine| engine.name()).collect::<Vec<_>>()));

    let models_folder = paths.models();
    let folder = paths.seeing();
    let models: Vec<Arc<Model>> = engines.iter().map(|&engine| Arc::new(Model {
        engine,
        slot: Mutex::new(Slot::Loading),
        vectors: Mutex::new(Vectors::load(&folder, engine)),
        means: Mutex::new(((u32::MAX, 0), Vec::new())),
        cache: Mutex::new(HashMap::new()),
    })).collect();
    // The text halves load side by side; a search uses whichever are ready.
    for model in &models {
        let (model, models_folder) = (model.clone(), models_folder.clone());
        std::thread::spawn(move || {
            let started = Instant::now();
            let loaded = load(&models_folder, model.engine, gpu);
            match &loaded {
                Ok(_) => log(&format!("seeker: {} ready in {} ms", model.engine.name(), started.elapsed().as_millis())),
                Err(error) => log(&format!("seeker: {}: {error}", model.engine.name())),
            }
            *model.slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = match loaded {
                Ok(encoder) => Slot::Ready(encoder),
                Err(error) => Slot::Failed(error),
            };
        });
    }

    let facts = Mutex::new(seeing::facts::Facts::load(&folder));
    // Documents by what they mean: EmbeddingGemma 2's vectors of their text, scored with its description's vector.
    let meaning: Mutex<Option<Vectors>> = Mutex::new(None);
    let last = Arc::new(Mutex::new(Instant::now()));
    {
        let (last, file) = (last.clone(), paths.seeker());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).elapsed() > STAYS {
                log("seeker: no searches for a while, ending");
                let _ = std::fs::remove_file(&file);
                // (llama.cpp's servers end with it: they're in its job object.)
                std::process::exit(0);
            }
        });
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        *last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
        if let Err(error) = answer(stream, &models, &folder, &facts, &meaning) {
            log(&format!("seeker: {error}"));
        }
        *last.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }
}

fn load(models: &Path, engine: Engine, gpu: bool) -> Result<Encoder, String> {
    if !models::seeing_present(models, engine) {
        return Err("its model isn't downloaded yet".into());
    }
    let folder = models::seeing_folder(models, engine);
    let engine = engine.model();
    match engine {
        Engine::Siglip => SiglipText::load(models, 4).map(Encoder::Siglip),
        Engine::Gemma | Engine::Qwen | Engine::GemmaText => {
            let model = folder.join(if engine == Engine::Gemma { models::GEMMA_MODEL } else { models::QWEN_MODEL }).display().to_string();
            let pooling = if engine == Engine::Gemma { "mean" } else { "last" };
            // The text half only (no image encoder): on the graphics card when there's one, else 0.1 s a search on the CPU.
            let layers = if gpu { "99" } else { "0" };
            let args = ["-m", &model, "--embedding", "--pooling", pooling, "-c", "2048", "-b", "2048", "-ub", "2048", "-np", "1", "-ngl", layers];
            Server::launch(models, &args, false, &format!("seeker {}", engine.name()), &|| false).map(Encoder::Llama)
        }
    }
}

/// One description's vector from one model, unit length.
fn encode(model: &Model, text: &str) -> Option<Result<Vec<f32>, String>> {
    if let Some(vector) = model.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(text) {
        return Some(Ok(vector.clone()));
    }
    let mut slot = model.slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let result = match &mut *slot {
        Slot::Loading => return None,
        Slot::Failed(error) => Err(error.clone()),
        Slot::Ready(Encoder::Siglip(siglip)) => siglip.text(text),
        Slot::Ready(Encoder::Llama(server)) => {
            let body = serde_json::json!({ "content": [looking::query_prompt(model.engine, text)] });
            server.ask("embeddings", Some(&body), Duration::from_secs(30)).and_then(|answer| looking::vector(&answer))
        }
    }
    .map(|vector| seeing::normalized(&vector));
    drop(slot);
    if let Ok(vector) = &result {
        let mut cache = model.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(text.to_string(), vector.clone());
    }
    Some(result)
}

/// What a search found.
struct Found {
    hits: Vec<seeing::Hit>,
    ready: Vec<&'static str>,
    waiting: Vec<&'static str>,
    /// How many images there are.
    images: usize,
    /// A time or a place in the description narrowed them down.
    filtered: bool,
}

/// Whether a path (an image, or a video's frame) has one of these extensions (none: any).
fn of_kind(path: &str, extensions: &[String]) -> bool {
    let path = path.split("#t=").next().unwrap_or(path);
    extensions.is_empty() || path.rsplit_once('.').is_some_and(|(_, its)| extensions.iter().any(|extension| extension.eq_ignore_ascii_case(its)))
}

/// Finds images for `text`, a time or a place in it answered from the images' facts; only of these extensions, when
/// there are any (the app's chip: "Video", or ">v").
fn find(models: &[Arc<Model>], folder: &PathBuf, facts: &Mutex<seeing::facts::Facts>, text: &str, limit: usize, extensions: &[String]) -> Found {
    let understood = {
        let mut facts = facts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        facts.refresh();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|since| since.as_secs() as i64).unwrap_or(0);
        seeing::query::understand(text, now + winsys::process::utc_offset(), &facts.place_names())
    };
    if understood.when.is_none() && understood.place.is_none() {
        let (hits, ready, waiting, images) = search(models, folder, text, limit, extensions);
        return Found { hits, ready, waiting, images, filtered: false };
    }
    let fits = |path: &str| {
        let facts = facts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        facts.get(path).is_some_and(|fact| {
            understood.when.is_none_or(|(from, to)| (from..to).contains(&fact.when))
                && understood.place.as_ref().is_none_or(|place| fact.place.as_ref().is_some_and(|known| known.names().any(|name| name == *place)))
        })
    };
    if understood.rest.is_empty() {
        // Only a time or a place: those images, the latest first, all standing out.
        let facts = facts.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let images = facts.by_path.len();
        let mut chosen: Vec<(String, i64)> = facts.by_path.iter().map(|(path, fact)| (path.clone(), fact.when)).collect();
        drop(facts);
        chosen.retain(|(path, _)| fits(path) && of_kind(path, extensions));
        chosen.sort_by(|a, b| b.1.cmp(&a.1));
        let hits = chosen.into_iter().take(limit).enumerate()
            .map(|(place, (path, _))| seeing::Hit { path, score: 9.0 - place as f32 * 0.001, similarity: 1.0 })
            .collect();
        return Found { hits, ready: Vec::new(), waiting: Vec::new(), images, filtered: true };
    }
    let (mut hits, ready, waiting, images) = search(models, folder, &understood.rest, usize::MAX, extensions);
    hits.retain(|hit| fits(&hit.path));
    hits.truncate(limit);
    Found { hits, ready, waiting, images, filtered: true }
}

/// Finds images for `text` by what they show: (hits, the models it used, the models still loading, how many images).
fn search(models: &[Arc<Model>], folder: &PathBuf, text: &str, limit: usize, extensions: &[String]) -> (Vec<seeing::Hit>, Vec<&'static str>, Vec<&'static str>, usize) {
    let (mut ready, mut waiting) = (Vec::new(), Vec::new());
    let mut queries: Vec<(&Arc<Model>, Vec<f32>)> = Vec::new();
    for model in models {
        match encode(model, text) {
            None => waiting.push(model.engine.name()),
            Some(Ok(vector)) => queries.push((model, vector)),
            Some(Err(_)) => {}
        }
    }
    let mut guards = Vec::new();
    for (model, _) in &queries {
        let mut vectors = model.vectors.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match vectors.as_mut() {
            Some(vectors) => vectors.refresh(),
            None => *vectors = Vectors::load(folder, model.engine),
        }
        guards.push(vectors);
    }
    let mut scored = Vec::new();
    let mut bonuses: Vec<HashMap<String, f32>> = Vec::new();
    let mut images = 0;
    for ((model, query), guard) in queries.iter().zip(&guards) {
        if let Some(vectors) = guard.as_ref().filter(|vectors| !vectors.is_empty()) {
            ready.push(model.engine.name());
            images = images.max(vectors.len());
            let scores = vectors.scores(query);
            // Each video's frames together, kept until the vectors change.
            let mut means = model.means.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if means.0 != vectors.version() {
                *means = (vectors.version(), vectors.video_means());
            }
            bonuses.push(seeing::video_bonus(&means.1, &scores, query));
            scored.push((vectors, scores));
        }
    }
    // A video's best moment stands for it, with what its frames together add.
    let mut hits = seeing::rank(&scored, &bonuses, if extensions.is_empty() { limit } else { usize::MAX });
    hits.retain(|hit| of_kind(&hit.path, extensions));
    hits.truncate(limit);
    (hits, ready, waiting, images)
}

/// Documents whose meaning fits `text`, each by its best piece: (path, standing), and how many pieces there are.
fn documents(models: &[Arc<Model>], folder: &PathBuf, meaning: &Mutex<Option<Vectors>>, text: &str, limit: usize, extensions: &[String]) -> (Vec<seeing::Hit>, usize) {
    let Some(gemma) = models.iter().find(|model| model.engine == Engine::Gemma) else { return (Vec::new(), 0) };
    let Some(Ok(query)) = encode(gemma, text) else { return (Vec::new(), 0) };
    let mut vectors = meaning.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match vectors.as_mut() {
        Some(vectors) => vectors.refresh(),
        None => *vectors = Vectors::load(folder, Engine::GemmaText),
    }
    let Some(vectors) = vectors.as_ref().filter(|vectors| !vectors.is_empty()) else { return (Vec::new(), 0) };
    let mut seen = std::collections::HashSet::new();
    let mut hits = seeing::combine(&[(vectors, vectors.scores(&query))], if extensions.is_empty() { limit.saturating_mul(8) } else { usize::MAX });
    for hit in &mut hits {
        if let Some((path, _)) = hit.path.rsplit_once("#c=") {
            hit.path = path.to_string();
        }
    }
    hits.retain(|hit| of_kind(&hit.path, extensions) && seen.insert(hit.path.to_lowercase()));
    hits.truncate(limit);
    (hits, vectors.len())
}

/// Whether a request is Excavate's own: addressed to this port by its number (a web page that renamed its own site to
/// this PC, DNS rebinding, still says its own name), and not from a web page (a browser says where it comes from).
fn from_this_pc(host: &str, origin: bool, port: u16) -> bool {
    !origin && (host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}"))
}

fn answer(stream: TcpStream, models: &[Arc<Model>], folder: &PathBuf, facts: &Mutex<seeing::facts::Facts>, meaning: &Mutex<Option<Vectors>>)
          -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    let mut request = String::new();
    reader.read_line(&mut request).map_err(|error| error.to_string())?;
    let mut length = 0usize;
    let (mut host, mut origin) = (String::new(), false);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|error| error.to_string())? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("host") {
                host = value.trim().to_ascii_lowercase();
            } else if name.eq_ignore_ascii_case("origin") {
                origin = true;
            }
        }
    }
    let mut body = vec![0u8; length.min(1 << 20)];
    reader.read_exact(&mut body).map_err(|error| error.to_string())?;
    let port = stream.local_addr().map(|address| address.port()).unwrap_or(0);
    let (status, reply) = if !from_this_pc(&host, origin, port) {
        ("403 Forbidden", serde_json::json!({ "error": "not for web pages" }))
    } else if request.starts_with("POST /search") {
        let asked: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        // What asks for an image taken out ("that one photo with a cake in it"), unless asked as typed ("raw", for measuring).
        let typed = asked["text"].as_str().unwrap_or("").trim();
        let tidied = if asked["raw"].as_bool() == Some(true) { typed.to_string() } else { seeing::query::tidy(typed) };
        let text = tidied.as_str();
        let limit = asked["limit"].as_u64().unwrap_or(60).clamp(1, 1000) as usize;
        let started = Instant::now();
        let extensions: Vec<String> = asked["extensions"].as_array()
            .map(|extensions| extensions.iter().filter_map(|extension| extension.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        // Images unless only documents are wanted, documents when they are.
        let found = if text.is_empty() || asked["images"].as_bool() == Some(false) {
            Found { hits: Vec::new(), ready: Vec::new(), waiting: Vec::new(), images: 0, filtered: false }
        } else {
            find(models, folder, facts, text, limit, &extensions)
        };
        let (texts, pieces) = if !text.is_empty() && asked["documents"].as_bool() == Some(true) {
            documents(models, folder, meaning, text, limit, &extensions)
        } else {
            (Vec::new(), 0)
        };
        let texts: Vec<serde_json::Value> = texts.into_iter().map(|hit| serde_json::json!({ "path": hit.path, "score": hit.score })).collect();
        // The kind it asks for, from EmbeddingGemma 2's vector of it as typed (as it was trained): the one just
        // made when tidying left it as it was, else one more (a few milliseconds on the graphics card).
        let kind_started = Instant::now();
        let kind = (!typed.is_empty()).then(|| models.iter().find(|model| matches!(model.engine, Engine::Gemma | Engine::GemmaText)))
            .flatten()
            .and_then(|model| match encode(model, typed) {
                Some(Ok(vector)) => seeing::query::kind_of(&vector),
                _ => None,
            })
            .map(|(kind, sure)| serde_json::json!({ "kind": kind, "p": sure, "ms": kind_started.elapsed().as_secs_f64() * 1000.0 }));
        let hits: Vec<serde_json::Value> = found.hits.into_iter().map(|hit| {
            // A video's frame: the video, and the moment.
            let (path, at) = match hit.path.rsplit_once("#t=") {
                Some((path, at)) => (path.to_string(), at.parse::<f64>().ok()),
                None => (hit.path, None),
            };
            serde_json::json!({ "path": path, "at": at, "score": hit.score, "similarity": hit.similarity })
        }).collect();
        ("200 OK", serde_json::json!({ "hits": hits, "ready": found.ready, "waiting": found.waiting, "images": found.images, "filtered": found.filtered,
                                       "documents": texts, "pieces": pieces, "kind": kind, "ms": started.elapsed().as_secs_f64() * 1000.0 }))
    } else if request.starts_with("GET /health") {
        ("200 OK", serde_json::json!({ "status": "ok" }))
    } else {
        ("404 Not Found", serde_json::json!({ "error": "unknown" }))
    };
    let reply = reply.to_string();
    let mut stream = stream;
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What Excavate's own client (WinHTTP) sends passes: its Host header, read off a real request.
    #[test]
    fn excavates_client_passes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let (mut host, mut origin) = (String::new(), false);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("host") { host = value.trim().to_ascii_lowercase(); }
                    if name.trim().eq_ignore_ascii_case("origin") { origin = true; }
                }
            }
            let mut stream = stream;
            let _ = write!(stream, "HTTP/1.1 200 OK
Content-Length: 2
Connection: close

{{}}");
            (host, origin)
        });
        let _ = winsys::http::local(&format!("http://127.0.0.1:{port}/health"), &[], Duration::from_secs(5));
        let (host, origin) = server.join().unwrap();
        assert!(from_this_pc(&host, origin, port), "Host {host:?}, Origin {origin}");
    }

    #[test]
    fn only_excavates_own_requests() {
        assert!(from_this_pc("127.0.0.1:50123", false, 50123));
        assert!(from_this_pc("localhost:50123", false, 50123));
        assert!(!from_this_pc("rebound.example:50123", false, 50123), "DNS rebinding");
        assert!(!from_this_pc("127.0.0.1:50123", true, 50123), "a web page");
        assert!(!from_this_pc("127.0.0.1:1", false, 50123));
        assert!(!from_this_pc("", false, 50123));
    }
}
