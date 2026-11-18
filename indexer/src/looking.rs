//! Images by what they show: each chosen model looks at the images the first pass has read, newest first, and keeps a
//! vector for each (`seeing::Writer`, in Excavate's `seeing` folder). The search helper (`seek`) scores descriptions
//! against them. EmbeddingGemma 2 and Qwen3-VL-Embedding run in llama.cpp's server on the graphics card, as Deep does;
//! SigLIP 2 in ONNX Runtime on the CPU, as the first pass's OCR does.

use crate::catalog::Catalog;
use crate::deep::Server;
use crate::models;
use crate::scan::FileInfo;
use crate::work::{self, Context, Need, Write};
use extract::Reader;
use seeing::Engine;
use seeing::sensitive::Shows;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Images are decoded at most this large for a model on the graphics card (the longer side); it shrinks them further
/// to its own budget (140 image tokens for EmbeddingGemma 2, up to 256 for Qwen).
const MAX_SIDE: u32 = 1024;

/// What EmbeddingGemma 2's model card asks of a description; images take no prefix.
pub const GEMMA_QUERY: &str = "task: search result | query: ";
/// llama.cpp's server's slots for the models that look (`Eye::open`), each with 2,048 tokens of context, and the most
/// tokens it takes in one step. `EXCAVATE_MEANING_SLOTS` and `EXCAVATE_MEANING_UBATCH` override them, for measuring:
/// 8 slots were no faster (470 to 673 documents a minute against 558 to 718 with 4) and shuffled more near ties among a
/// document's nearest neighbours (45 of 60 top tens the same, against 51 to 59 between runs with 4); 16 slots and
/// batches of 32 or 64 were slower (448 to 600), 2026-11-15.
const SLOTS: usize = 4;
const SLOT_TOKENS: usize = 2048;
const UBATCH: usize = 4096;

/// Documents sent for their meaning in one request at Full (`side_by_side`); `EXCAVATE_MEANING_BATCH` for measuring.
const MEANING_BATCH: usize = 16;

fn knob(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).filter(|&value| value > 0).unwrap_or(default)
}

fn meaning_batch() -> usize {
    knob("EXCAVATE_MEANING_BATCH", MEANING_BATCH)
}

/// Qwen3-VL-Embedding's instructions, as its own code gives them.
const QWEN_IMAGE: &str = "Represent the user's input.";
const QWEN_QUERY: &str = "Retrieve images relevant to the user's query.";

/// Qwen's 8B is tried on its first images once downloaded: an image's median time past this and it's far too slow
/// for a library (it took 0.27 s on the benchmark's graphics card), and the choice goes back to the lighter partner.
const TOO_SLOW_MS: f64 = 2000.0;
const TRIED: usize = 10;

fn trial_key(engine: Engine) -> String {
    format!("trial-{}", engine.name())
}

/// How a model is spoken of while it downloads.
pub fn label(engine: Engine) -> &'static str {
    match engine {
        Engine::Siglip => "SigLIP 2",
        Engine::Gemma | Engine::GemmaText => "EmbeddingGemma 2",
        Engine::Qwen => "Qwen3-VL-Embedding-8B",
    }
}

// ---- The models -----------------------------------------------------------------------------------------------------

/// A model ready to look at images.
pub enum Eye {
    Llama { server: Server, engine: Engine, marker: String },
    Siglip(SiglipImages),
}

impl Eye {
    /// `gpu`: on the graphics card (llama.cpp's Vulkan, or DirectML for SigLIP 2); else on the processor.
    pub fn open(models: &Path, engine: Engine, gpu: bool, stopping: &dyn Fn() -> bool) -> Result<Eye, String> {
        let engine = engine.model();
        match engine {
            Engine::Siglip => Ok(Eye::Siglip(SiglipImages::load(models, crate::ocr::threads(), gpu)?)),
            Engine::Gemma | Engine::Qwen | Engine::GemmaText => {
                let folder = models::seeing_folder(models, engine);
                let (model, vision) = if engine == Engine::Gemma { (models::GEMMA_MODEL, models::GEMMA_VISION) } else { (models::QWEN_MODEL, models::QWEN_VISION) };
                let (model, vision) = (folder.join(model).display().to_string(), folder.join(vision).display().to_string());
                // Four slots of 2,048 tokens (a document's piece takes ~400, an image 140 to 256): at full speed a
                // batch of pieces goes through them side by side (`Eye::read`); one at a time otherwise, as with one.
                let slots = knob("EXCAVATE_MEANING_SLOTS", SLOTS);
                let (slots, context, batch) = (slots.to_string(), (slots * SLOT_TOKENS).to_string(), knob("EXCAVATE_MEANING_UBATCH", UBATCH).to_string());
                let mut args = vec!["-m", &model, "--mmproj", &vision, "--embedding", "-c", &context, "-b", &batch, "-ub", &batch, "-np", &slots, "-ngl", if gpu { "99" } else { "0" }];
                if !gpu {
                    args.extend(["--no-mmproj-offload", "--device", "none"]);
                }
                // EmbeddingGemma 2 takes a fixed budget per image; Qwen's grows with the image up to its own.
                if engine == Engine::Gemma {
                    args.extend(["--pooling", "mean", "--image-min-tokens", "140", "--image-max-tokens", "140"]);
                } else {
                    args.extend(["--pooling", "last", "--image-max-tokens", "256"]);
                }
                let server = Server::launch(models, &args, true, label(engine), stopping)?;
                // The marker for where an image goes is new with each server.
                let props = server.ask("props", None, Duration::from_secs(10))?;
                let marker = props["media_marker"].as_str().unwrap_or("<__media__>").to_string();
                Ok(Eye::Llama { server, engine, marker })
            }
        }
    }

    /// The llama.cpp server's process, to leave out of what the PC is judged by.
    pub fn process(&self) -> u32 {
        match self {
            Eye::Llama { server, .. } => server.process(),
            Eye::Siglip(_) => 0,
        }
    }

    pub fn running(&mut self) -> bool {
        match self {
            Eye::Llama { server, .. } => server.running(),
            Eye::Siglip(_) => true,
        }
    }

    /// How large an image it's handed (the longer side).
    pub fn side(&self) -> u32 {
        if matches!(self, Eye::Siglip(_)) { 448 } else { MAX_SIDE }
    }

    /// An image's vector. COM must be initialized on the calling thread.
    pub fn look(&mut self, path: &str) -> Result<Vec<f32>, String> {
        let image = decode(path, self.side())?;
        self.look_at(image)
    }

    /// Texts' vectors, one each (documents' pieces, for their meaning; a search's descriptions): EmbeddingGemma 2's
    /// only. `side_by_side`: all in one request, which the server spreads over its slots; else one text at a time, as
    /// it reads at its usual pace.
    pub fn read(&mut self, texts: &[String], side_by_side: bool) -> Result<Vec<Vec<f32>>, String> {
        let Eye::Llama { server, .. } = self else { return Err("only EmbeddingGemma 2 reads documents for their meaning".into()) };
        let mut vectors = Vec::with_capacity(texts.len());
        for group in texts.chunks(if side_by_side { texts.len().max(1) } else { 1 }) {
            let answer = server.ask("embeddings", Some(&serde_json::json!({ "content": group })), Duration::from_secs(120))?;
            let mut items: Vec<&serde_json::Value> = answer.as_array().map(|items| items.iter().collect()).unwrap_or_default();
            items.sort_by_key(|item| item["index"].as_u64().unwrap_or(0));
            if items.len() != group.len() {
                return Err(format!("{} vectors for {} texts", items.len(), group.len()));
            }
            for item in items {
                vectors.push(vector(&serde_json::Value::Array(vec![item.clone()]))?);
            }
        }
        Ok(vectors)
    }

    /// A decoded image's vector (a video's frame).
    pub fn look_at(&mut self, picture: image::RgbImage) -> Result<Vec<f32>, String> {
        match self {
            Eye::Siglip(siglip) => siglip.look(&picture),
            Eye::Llama { server, engine, marker } => {
                // PPM: nothing to compress, for a trip inside this PC.
                let mut image = format!("P6\n{} {}\n255\n", picture.width(), picture.height()).into_bytes();
                image.extend_from_slice(picture.as_raw());
                let prompt = if *engine == Engine::Gemma { marker.clone() } else { qwen_prompt(QWEN_IMAGE, marker) };
                let body = serde_json::json!({ "content": [{ "prompt_string": prompt, "multimodal_data": [crate::deep::base64(&image)] }] });
                vector(&server.ask("embeddings", Some(&body), Duration::from_secs(120))?)
            }
        }
    }
}

/// Qwen3-VL-Embedding's chat template, an instruction and an input.
pub fn qwen_prompt(instruction: &str, input: &str) -> String {
    format!("<|im_start|>system\n{instruction}<|im_end|>\n<|im_start|>user\n{input}<|im_end|>\n<|im_start|>assistant\n")
}

/// A description as each model wants it.
pub fn query_prompt(engine: Engine, text: &str) -> String {
    match engine {
        Engine::Gemma | Engine::GemmaText => format!("{GEMMA_QUERY}{text}"),
        Engine::Qwen => qwen_prompt(QWEN_QUERY, text),
        Engine::Siglip => text.to_string(),
    }
}

/// The vector out of llama.cpp's answer: `[{ "embedding": [...] }]`, or a list of per-token vectors, the last pooled.
pub fn vector(answer: &serde_json::Value) -> Result<Vec<f32>, String> {
    let embedding = &answer[0]["embedding"];
    let numbers = match embedding.as_array() {
        Some(rows) if rows.first().is_some_and(serde_json::Value::is_array) => rows.last().unwrap_or(&serde_json::Value::Null),
        Some(_) => embedding,
        None => return Err(format!("no vector in llama.cpp's answer: {}", &answer.to_string()[..answer.to_string().len().min(200)])),
    };
    let values: Vec<f32> = numbers.as_array().map(|values| values.iter().filter_map(|value| value.as_f64().map(|value| value as f32)).collect()).unwrap_or_default();
    if values.is_empty() { Err("an empty vector".into()) } else { Ok(values) }
}

/// An image decoded, at most `max_side` on its longer side, the right way up (its metadata's orientation applied).
fn decode(path: &str, max_side: u32) -> Result<image::RgbImage, String> {
    let (picture, _) = winsys::imaging::decode_upright(path, max_side)?;
    image::RgbImage::from_raw(picture.width, picture.height, picture.pixels).ok_or_else(|| "a picture of the wrong size".to_string())
}

fn session(path: &Path, threads: usize, gpu: bool) -> Result<ort::session::Session, String> {
    let error = |error: ort::Error<ort::session::builder::SessionBuilder>| error.to_string();
    let mut builder = ort::session::Session::builder().map_err(|error| error.to_string())?;
    if gpu {
        // DirectML wants its graph run in order, without ONNX Runtime's memory patterns.
        builder = builder
            .with_execution_providers([ort::ep::DirectML::default().with_device_id(crate::ocr::dml_device()).build()]).map_err(error)?
            .with_memory_pattern(false).map_err(error)?
            .with_parallel_execution(false).map_err(error)?;
    }
    builder
        .with_intra_threads(threads).map_err(error)?
        .with_inter_threads(1).map_err(error)?
        .with_config_entry("session.intra_op.allow_spinning", "0").map_err(error)?
        .commit_from_file(path)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// SigLIP 2's image half: 224 × 224, squashed (bicubic), scaled to -1..1.
pub struct SiglipImages {
    session: ort::session::Session,
    input: String,
}

impl SiglipImages {
    pub fn load(models: &Path, threads: usize, gpu: bool) -> Result<SiglipImages, String> {
        let session = session(&models::seeing_folder(models, Engine::Siglip).join(models::SIGLIP_VISION), threads, gpu)?;
        let input = session.inputs().first().map(|input| input.name().to_string()).ok_or("SigLIP 2 has no input")?;
        Ok(SiglipImages { session, input })
    }

    fn look(&mut self, image: &image::RgbImage) -> Result<Vec<f32>, String> {
        const SIDE: u32 = 224;
        let image = image::imageops::resize(image, SIDE, SIDE, image::imageops::FilterType::CatmullRom);
        let plane = (SIDE * SIDE) as usize;
        let mut data = vec![0f32; 3 * plane];
        for (index, pixel) in image.pixels().enumerate() {
            for channel in 0..3 {
                data[channel * plane + index] = (f32::from(pixel[channel]) / 255.0 - 0.5) / 0.5;
            }
        }
        let tensor = ort::value::Tensor::from_array(([1usize, 3, SIDE as usize, SIDE as usize], data)).map_err(|error| error.to_string())?;
        let outputs = self.session.run(ort::inputs![self.input.as_str() => tensor]).map_err(|error| error.to_string())?;
        let (_, values) = outputs[0].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        Ok(values.to_vec())
    }
}

/// SigLIP 2's text half, for the search helper: cleaned as open_clip's "canonicalize" does, 64 tokens padded with 0.
pub struct SiglipText {
    session: ort::session::Session,
    tokenizer: tokenizers::Tokenizer,
    input: String,
    wide: bool,
}

impl SiglipText {
    pub fn load(models: &Path, threads: usize) -> Result<SiglipText, String> {
        let folder = models::seeing_folder(models, Engine::Siglip);
        let session = session(&folder.join(models::SIGLIP_TEXT), threads, false)?;
        let input = session.inputs().first().ok_or("SigLIP 2's text half has no input")?;
        let wide = input.dtype().tensor_type() == Some(ort::value::TensorElementType::Int64);
        let input = input.name().to_string();
        let tokenizer = tokenizers::Tokenizer::from_file(folder.join(models::SIGLIP_TOKENIZER)).map_err(|error| error.to_string())?;
        Ok(SiglipText { session, tokenizer, input, wide })
    }

    pub fn text(&mut self, text: &str) -> Result<Vec<f32>, String> {
        const CONTEXT: usize = 64;
        let encoding = self.tokenizer.encode(canonicalize(text), true).map_err(|error| error.to_string())?;
        let mut ids: Vec<i64> = encoding.get_ids().iter().take(CONTEXT).map(|&id| i64::from(id)).collect();
        ids.resize(CONTEXT, 0);
        let outputs = if self.wide {
            let tensor = ort::value::Tensor::from_array(([1usize, CONTEXT], ids)).map_err(|error| error.to_string())?;
            self.session.run(ort::inputs![self.input.as_str() => tensor])
        } else {
            let ids: Vec<i32> = ids.into_iter().map(|id| id as i32).collect();
            let tensor = ort::value::Tensor::from_array(([1usize, CONTEXT], ids)).map_err(|error| error.to_string())?;
            self.session.run(ort::inputs![self.input.as_str() => tensor])
        }
        .map_err(|error| error.to_string())?;
        let (_, values) = outputs[0].try_extract_tensor::<f32>().map_err(|error| error.to_string())?;
        Ok(values.to_vec())
    }
}

/// open_clip's "canonicalize", which SigLIP 2 was trained with: HTML entities, `_` as a space, no punctuation, lowercase,
/// single spaces.
pub fn canonicalize(text: &str) -> String {
    let unescaped = text.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">");
    let cleaned: String = unescaped.trim().replace('_', " ").chars().filter(|c| !c.is_ascii_punctuation()).collect::<String>().to_lowercase();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- The lane -------------------------------------------------------------------------------------------------------

/// When and where an image was taken: its EXIF metadata (or, without it, when the file last changed), and the place
/// nearest its GPS position. COM must be initialized on the calling thread.
fn fact_of(path: &str, modified: i64, places: &seeing::places::Places) -> seeing::facts::Fact {
    let facts = winsys::imaging::facts(path);
    seeing::facts::Fact {
        when: facts.taken.unwrap_or(modified + winsys::process::utc_offset()),
        taken: facts.taken.is_some(),
        place: facts.position.and_then(|(latitude, longitude)| places.nearest(latitude, longitude)),
    }
}


/// A document's text in pieces for its meaning, as EmbeddingGemma 2's model card has documents ("title: … | text: …"):
/// about 1,500 characters each, at most 8 (the first 12,000 characters: most of what a document is about).
pub fn pieces(name: &str, text: &str) -> Vec<String> {
    const PIECE: usize = 1500;
    const MOST: usize = 8;
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && out.len() < MOST {
        let mut end = rest.len().min(PIECE);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        // At a space, not mid-word, unless there's none.
        if end < rest.len()
            && let Some(space) = rest[..end].rfind(char::is_whitespace).filter(|&space| space > PIECE / 2) {
                end = space;
            }
        let piece: String = rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
        if !piece.is_empty() {
            out.push(format!("title: {name} | text: {piece}"));
        }
        rest = rest[end..].trim_start();
    }
    out
}

/// Where a frame's vector is kept: its video's path and the moment, "C:\\Videos\\trip.mp4#t=125.0".
pub fn frame_key(path: &str, seconds: f64) -> String {
    format!("{path}#t={seconds:.1}")
}

/// Times looking at `paths` on the processor and on the graphics card. Whether the graphics card was faster (None
/// when it couldn't be measured).
fn measure(context: &Context, models: &Path, engine: Engine, gpu: bool, key: &str, paths: &[String]) -> Option<bool> {
    let devices: [&str; 2] = if gpu { ["gpu", "cpu"] } else { ["cpu", "gpu"] };
    let stopping = || context.control.stopping();
    let times = crate::devices::race(&devices, paths, |device| Eye::open(models, engine, device == "gpu", &stopping),
        |eye, path| eye.look(path).map(|_| ()), &stopping);
    if times.len() < 2 {
        return None;
    }
    crate::devices::keep(&context.paths, key, times, "an image").map(|fastest| fastest == "gpu")
}

/// One model's lane. `after`: a model that goes first, waited for (Thorough's second looks once the first is done).
/// `slot`: its place in the status's list. Images changed since a model looked are looked at again, and every image
/// looked at before `relook_at` ("Look again").
#[allow(clippy::too_many_arguments)]
pub fn lane(mut context: Context, catalog: Arc<Catalog>, models: PathBuf, folder: PathBuf, index: PathBuf, engine: Engine, after: Option<Engine>,
            relook_at: i64, slot: usize) {
    winsys::com_mta();
    // Where it runs, as chosen: what it waits to be quiet.
    let mut gpu = if engine == Engine::Siglip { context.siglip_gpu } else { context.seeing_gpu };
    let mut need = if gpu { Need::Gpu } else { Need::Cpu };
    // Only the model the choice is measured by: SigLIP 2 alone (Quick), or EmbeddingGemma 2.
    let measuring = match engine {
        Engine::Siglip => context.seeing_measure.as_deref() == Some("seeing:siglip") && context.siglip_gpu == context.seeing_gpu,
        Engine::Gemma => context.seeing_measure.as_deref() == Some("seeing:gemma"),
        _ => false,
    };
    if !measuring {
        context.seeing_measure = None;
    }
    let mut writer = match seeing::Writer::open(&folder, engine) {
        Ok(writer) => writer,
        Err(error) => {
            config::log(&format!("{}: its vectors can't be kept: {error}", engine.name()));
            return;
        }
    };
    whole_from(slot, writer.sensitive());
    // EmbeddingGemma 2's lane also reads documents for their meaning, into vectors of their own. Their writer is open
    // only while there are documents to read, and a minute after: it knows every piece of every document read, which
    // held 55 MB for a quarter of a million pieces while nothing was being read.
    let mut reads_meaning = engine == Engine::Gemma;
    let mut meaning: Option<seeing::Writer> = None;
    let mut meaning_used = Instant::now();
    // Opened, and what's gone from what's read taken out (searches leave a gone file out meanwhile).
    let open_meaning = || {
        let mut writer = seeing::Writer::open(&folder, Engine::GemmaText)
            .map_err(|error| config::log(&format!("{}: its vectors can't be kept: {error}", Engine::GemmaText.name()))).ok()?;
        let documents = catalog.text_ids();
        let gone: Vec<String> = writer.paths().filter(|path| !documents.contains(path.split("#c=").next().unwrap_or(path))).collect();
        for path in &gone {
            let _ = writer.remove(path);
        }
        Some(writer)
    };
    let mut contents: Option<content::Reader> = None;
    // The first model's lane also keeps when and where each image was taken.
    let mut facts = if slot == 0 { seeing::facts::Writer::open(&folder).ok() } else { None };
    let places = facts.as_ref().map(|_| seeing::places::Places::load());
    let mut eye: Option<Eye> = None;
    let mut idle_since = Instant::now();
    let mut tidied = Instant::now() - Duration::from_secs(3600);
    let mut times: VecDeque<f64> = VecDeque::new();
    // Qwen's 8B until it has shown it fits and keeps up on this PC.
    let mut on_trial = engine == Engine::Qwen && catalog.meta(&trial_key(engine)) == 0;
    let set_server = |eye: &Option<Eye>| {
        if engine.on_gpu() {
            // (Left out of the PC's load, wherever it runs.)
            context.shared.seeing_server.store(eye.as_ref().map_or(0, Eye::process), Ordering::Relaxed);
        }
    };
    let status = |change: &dyn Fn(&mut config::status::Looking)| {
        if let Some(looking) = context.shared.status().looking.get_mut(slot) {
            change(looking);
        }
    };
    let noting = |reason: Option<&str>| status(&|looking| looking.waiting = reason.unwrap_or_default().to_string());
    status(&|looking| looking.gpu = gpu);
    // A failed download is tried again soon, then less often: half a minute, 2 minutes, then every 10.
    let mut failures = 0usize;
    let mut crashes = work::Crashes::default();
    loop {
        if context.control.stopping() {
            break;
        }
        // Gone from what's read: its vector goes too (now and then, and at the start).
        if tidied.elapsed() > Duration::from_secs(300) {
            let pictures = catalog.picture_ids();
            // (A video's frames go with their video.)
            let gone: Vec<String> = writer.paths().filter(|path| !pictures.contains(path.split("#t=").next().unwrap_or(path))).collect();
            for path in &gone {
                let _ = writer.remove(path);
                note_whole(slot, path, None);
                if let Some(facts) = facts.as_mut() {
                    let _ = facts.remove(path);
                }
            }
            if !gone.is_empty() {
                config::log(&format!("{}: {} images gone", engine.name(), gone.len()));
            }
            // Images blurred whole for what they show keep their box: a first pass reading one again (a rules update,
            // "Read again") writes its marks afresh.
            let sensitive: Vec<(String, Shows)> = writer.sensitive().map(|(path, shows)| (path.to_string(), shows)).collect();
            if !sensitive.is_empty() && let Some(reader) = refreshed(&mut contents, &index) {
                for (path, shows) in sensitive {
                    if let Some(entry) = catalog.get(&path) {
                        mark_whole(&context, reader, &path, entry.size, entry.modified, shows);
                    }
                }
            }
            if let Some(meaning) = meaning.as_mut() {
                let documents = catalog.text_ids();
                let gone: Vec<String> = meaning.paths().filter(|path| !documents.contains(path.split("#c=").next().unwrap_or(path))).collect();
                for path in &gone {
                    let _ = meaning.remove(path);
                }
            }
            tidied = Instant::now();
        }
        if !context.seeing_gate.open() || after.is_some_and(|first| catalog.seen_counts(first.name(), relook_at).1 > 0) {
            eye = None;
            set_server(&eye);
            status(&|looking| {
                looking.running = false;
                looking.current.clear();
            });
            if !context.control.sleep(Duration::from_secs(10)) {
                break;
            }
            continue;
        }
        let batch = catalog.seen_next(engine.name(), relook_at, 32);
        // Videos once the images are done, one at a time. SigLIP beside EmbeddingGemma 2 leaves them to it: on videos
        // it added nothing (MSR-VTT) and cost 70 ms of processor a frame.
        let videos_too = context.videos_shown && (slot == 0 || engine != Engine::Siglip);
        let video = if batch.is_empty() && videos_too { catalog.video_next(engine.name(), relook_at.max(context.relook_videos_at), 1, config::video_kind::SHOWN).pop() } else { None };
        // Then documents, for their meaning.
        let documents = if batch.is_empty() && video.is_none() && reads_meaning {
            catalog.text_next(Engine::GemmaText.name(), relook_at, meaning_batch())
        } else {
            Vec::new()
        };
        if batch.is_empty() && video.is_none() && documents.is_empty() {
            // Images looked at before facts were kept: theirs, a few hundred at a time while there's nothing else.
            // Not a video's frames ("…mp4#t=12.0"): a frame isn't a file, so it had none to read and was kept as
            // taken in 1970; those are taken out again.
            if let (Some(facts), Some(places)) = (facts.as_mut(), places.as_ref()) {
                let frames: Vec<String> = writer.paths().filter(|path| path.contains("#t=") && facts.has(path)).take(300).collect();
                for path in frames {
                    let _ = facts.remove(&path);
                }
                let missing: Vec<String> = writer.paths().filter(|path| !path.contains("#t=") && !facts.has(path)).take(300).collect();
                for path in missing {
                    let modified = std::fs::metadata(&path).ok().map(|metadata| crate::scan::seconds(&metadata)).unwrap_or(0);
                    let _ = facts.add(&path, &fact_of(&path, modified, places));
                }
            }
            if eye.is_some() && idle_since.elapsed() > Duration::from_secs(60) {
                eye = None;
                set_server(&eye);
            }
            if meaning.is_some() && meaning_used.elapsed() > Duration::from_secs(60) {
                meaning = None;
            }
            // Nothing to look at: nothing being looked at, whether or not the model stays loaded a while.
            status(&|looking| {
                looking.running = eye.is_some();
                looking.current.clear();
            });
            if !context.control.sleep(Duration::from_secs(5)) {
                break;
            }
            continue;
        }
        // What it needs, once. A download takes a while.
        if !models::seeing_present(&models, engine) {
            let total = models::seeing_download_size(&models, engine);
            status(&|looking| looking.model = config::status::Model { state: "downloading".into(), name: label(engine).into(), total, ..Default::default() });
            let shared = context.shared.clone();
            let result = models::seeing_ensure(&models, engine, context.control, &mut |received, total| {
                if let Some(looking) = shared.status().looking.get_mut(slot) {
                    looking.model.received = received;
                    looking.model.total = total;
                }
            });
            if let Err(error) = result {
                // Reading starting over: the download goes on, and the next session takes it up (not a failure).
                if context.control.stopping() {
                    break;
                }
                config::log(&format!("{}'s model: {error}", engine.name()));
                status(&|looking| looking.model = config::status::Model { state: "failed".into(), name: label(engine).into(), error: error.clone(), ..Default::default() });
                let wait = [30, 120, 600][failures.min(2)];
                failures += 1;
                if !context.control.sleep(Duration::from_secs(wait)) {
                    break;
                }
                continue;
            }
            failures = 0;
            status(&|looking| looking.model = config::status::Model { state: "ready".into(), ..Default::default() });
        }
        context.shared.wants_load.store(true, Ordering::Relaxed);
        if !documents.is_empty() {
            if meaning.is_none() {
                meaning = open_meaning();
                // (As before: a writer that can't be opened leaves documents' meaning for the next session.)
                reads_meaning = meaning.is_some();
            }
            meaning_used = Instant::now();
        }
        if let (false, Some(writer)) = (documents.is_empty(), meaning.as_mut()) {
            let _busy = work::Busy::new(&context.shared);
            if !work::wait_seeing(&context, need, &noting) {
                break;
            }
            // (Counted as the documents it gets through.)
            let mut batch = context.shared.paces.item(crate::pace::Lane::Meaning);
            batch.count = 0;
            if eye.as_mut().is_none_or(|eye| !eye.running()) {
                status(&|looking| looking.running = true);
                match Eye::open(&models, engine, gpu, &|| context.control.stopping()) {
                    Ok(opened) => eye = Some(opened),
                    Err(error) => {
                        config::log(&format!("{}: {error}", engine.name()));
                        if !context.control.sleep(Duration::from_secs(600)) {
                            return;
                        }
                        continue;
                    }
                }
                set_server(&eye);
            }
            let Some(open) = eye.as_mut() else { continue };
            match contents.as_mut() {
                Some(reader) => reader.refresh(),
                None => contents = content::Reader::open(&index).ok(),
            }
            let Some(reader) = contents.as_mut() else { continue };
            // At full speed, a batch's pieces in one request through the server's slots side by side, with the next
            // batch made ready and the last one's vectors written meanwhile.
            if context.shared.speed(&context.seeing_gate) == config::Speed::Full {
                drop(batch);
                let relook_videos = videos_too.then(|| relook_at.max(context.relook_videos_at));
                side_by_side(&context, &catalog, open, writer, reader, documents, engine, (relook_at, relook_videos), need, &mut crashes);
                idle_since = Instant::now();
                continue;
            }
            // Else one document at a time, its pieces one by one.
            for (path, modified, texts) in prepare(reader, documents) {
                if context.control.stopping() || !context.seeing_gate.open() || work::seeing_pause(&context, need).is_some() {
                    break;
                }
                let result = if texts.is_empty() { Ok(Vec::new()) } else { open.read(&texts, false) };
                let ok = match result {
                    Ok(vectors) => keep(writer, &path, modified, Some(&vectors)),
                    Err(error) => {
                        config::log(&format!("{} on {path}: {error}", Engine::GemmaText.name()));
                        // Its server stopped on it: started again for it once before it counts as the file's doing.
                        if !open.running() && !crashes.blame(&path) {
                            keep(writer, &path, modified, None);
                            break;
                        }
                        keep(writer, &path, modified, None)
                    }
                };
                catalog.seen_set(&path, Engine::GemmaText.name(), modified, work::now(), ok);
                batch.count += 1;
            }
            idle_since = Instant::now();
            continue;
        }
        if let Some((path, _, modified)) = video {
            let _busy = work::Busy::new(&context.shared);
            if !work::wait_seeing(&context, need, &noting) {
                break;
            }
            let _item = (slot == 0).then(|| context.shared.paces.item(crate::pace::Lane::Shown));
            if eye.as_mut().is_none_or(|eye| !eye.running()) {
                status(&|looking| looking.running = true);
                match Eye::open(&models, engine, gpu, &|| context.control.stopping()) {
                    Ok(opened) => eye = Some(opened),
                    Err(error) => {
                        config::log(&format!("{}: {error}", engine.name()));
                        if !context.control.sleep(Duration::from_secs(600)) {
                            return;
                        }
                        continue;
                    }
                }
                set_server(&eye);
            }
            let Some(open) = eye.as_mut() else { continue };
            status(&|looking| looking.current = path.clone());
            let started = Instant::now();
            let length = winsys::video::duration(&path).unwrap_or(0.0);
            // How often, as chosen on the Videos page (Balanced: every 5 s, at most 720 frames, an hour's video).
            let (frame_every, most_frames) = context.video_pace;
            let every = (length / most_frames).max(frame_every);
            // Looked at before: its frames then go.
            let old: Vec<String> = writer.parts_of(&path).into_iter().filter(|key| key.contains("#t=")).collect();
            for key in old {
                let _ = writer.remove(&key);
            }
            let (mut frames, mut stopped, mut failed, mut skipped) = (0, false, None, false);
            let side = open.side();
            // The next frames are decoded while the model looks at this one, two at most waiting between them (on the
            // processor's graphics when there's a graphics card beside them, which is then the models' alone:
            // `winsys::video::Decoder`).
            let result = std::thread::scope(|scope| {
                let (frames_out, frames_in) = std::sync::mpsc::sync_channel::<winsys::video::Frame>(2);
                let decoding = {
                    let path = &path;
                    scope.spawn(move || {
                        winsys::com_mta();
                        winsys::video::frames(path, every, side, &mut |frame| frames_out.send(frame).is_ok())
                    })
                };
                for frame in frames_in.iter() {
                    // Let go by "Skip".
                    if context.control.skip_asked(&path) {
                        skipped = true;
                        break;
                    }
                    // Something else needs the PC: the video is looked at again from the start later.
                    if context.control.stopping() || !context.seeing_gate.open() || work::seeing_pause(&context, need).is_some() {
                        stopped = true;
                        break;
                    }
                    let Some(image) = image::RgbImage::from_raw(frame.picture.width, frame.picture.height, frame.picture.pixels) else { continue };
                    match open.look_at(image).and_then(|vector| writer.add(&frame_key(&path, frame.seconds), modified, &vector).map_err(|error| error.to_string())) {
                        Ok(()) => frames += 1,
                        Err(error) => {
                            failed = Some(error);
                            break;
                        }
                    }
                }
                // (Stopping early: the decoder's next send fails, and it ends.)
                drop(frames_in);
                decoding.join().unwrap_or_else(|_| Err("the video's decoder stopped".into()))
            });
            match (result, failed) {
                _ if skipped => {
                    context.control.skipped();
                    config::log(&format!("{}: {path}: skipped", engine.name()));
                    catalog.seen_set(&path, engine.name(), modified, work::now(), false);
                    catalog.skipped_add(&path, "what it shows");
                }
                _ if stopped => {}
                (Ok(decoded), None) => {
                    let stalled = decoded.stalled.map(|on| format!(" (it stalled on {}: {} s without a frame)", on.name(), winsys::video::STALL.as_secs())).unwrap_or_default();
                    config::log(&format!("{}: {path}: {frames} frames in {:.1} s, decoded on {}{stalled}", engine.name(), started.elapsed().as_secs_f64(),
                                         decoded.decoder.name()));
                    catalog.seen_set(&path, engine.name(), modified, work::now(), true);
                }
                (Err(error), _) | (_, Some(error)) => {
                    config::log(&format!("{} on {path}: {error}", engine.name()));
                    // Its server stopped on it: started again for it once before it counts as the video's doing.
                    if open.running() || crashes.blame(&path) {
                        catalog.seen_set(&path, engine.name(), modified, work::now(), false);
                    }
                }
            }
            idle_since = Instant::now();
            continue;
        }
        // The first time on this PC: where it looks fastest, on these images.
        if context.seeing_measure.is_some() && batch.len() >= 4 && crate::devices::quiet(&context)
            && let Some(key) = context.seeing_measure.take() {
                // (Its own models are opened to measure: the one open now makes room.)
                eye = None;
                set_server(&eye);
                let paths: Vec<String> = batch.iter().take(crate::devices::ITEMS + 1).map(|(path, ..)| path.clone()).collect();
                if let Some(faster) = measure(&context, &models, engine, gpu, &key, &paths)
                    && faster != gpu {
                        gpu = faster;
                        need = if gpu { Need::Gpu } else { Need::Cpu };
                        context.shared.seeing_gpu.store(gpu, Ordering::Relaxed);
                        status(&|looking| looking.gpu = gpu);
                    }
            }
        for (path, size, modified) in batch {
            if context.control.stopping() || !context.seeing_gate.open() {
                break;
            }
            if !crate::scan::in_scope(&path, &context.roots, &context.exclusions) {
                catalog.seen_set(&path, engine.name(), modified, work::now(), false);
                continue;
            }
            // Something else needs the graphics card (or the processor), or it's paused: it's theirs, at once.
            if work::seeing_pause(&context, need).is_some() && (gpu || engine.on_gpu()) {
                eye = None;
                set_server(&eye);
                status(&|looking| looking.running = false);
            }
            let _busy = work::Busy::new(&context.shared);
            if !work::wait_seeing(&context, need, &noting) {
                break;
            }
            let _item = (slot == 0).then(|| context.shared.paces.item(crate::pace::Lane::Shown));
            if eye.as_mut().is_none_or(|eye| !eye.running()) {
                status(&|looking| looking.running = true);
                match Eye::open(&models, engine, gpu, &|| context.control.stopping()) {
                    Ok(opened) => eye = Some(opened),
                    Err(error) if on_trial => {
                        config::log(&format!("{}: on trial, it didn't start: {error}", engine.name()));
                        let error = "It didn't fit in the graphics card's memory.".to_string();
                        status(&|looking| looking.model = config::status::Model { state: "unsuitable".into(), name: label(engine).into(), error: error.clone(), ..Default::default() });
                        set_server(&None);
                        return;
                    }
                    Err(error) => {
                        config::log(&format!("{}: {error}", engine.name()));
                        status(&|looking| looking.model = config::status::Model { state: "failed".into(), name: label(engine).into(), error: error.clone(), ..Default::default() });
                        eye = None;
                        if !context.control.sleep(Duration::from_secs(600)) {
                            return;
                        }
                        break;
                    }
                }
                set_server(&eye);
            }
            let Some(open) = eye.as_mut() else { break };
            let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
            let file = FileInfo { name, path: path.clone(), size, modified, reader: Reader::Picture };
            // Changed since the first pass read it: that pass comes first again.
            let Some((file_size, file_modified)) = work::current(&context, &file) else { continue };
            if file_size != size || file_modified != modified {
                continue;
            }
            status(&|looking| looking.current = path.clone());
            let started = Instant::now();
            match open.look(&path) {
                Ok(vector) => match writer.add(&path, modified, &vector) {
                    Ok(()) => {
                        catalog.seen_set(&path, engine.name(), modified, work::now(), true);
                        note_whole(slot, &path, writer.shows(&path));
                        if let Some(shows) = writer.shows(&path) && let Some(reader) = refreshed(&mut contents, &index) {
                            mark_whole(&context, reader, &path, size, modified, shows);
                        }
                        if let (Some(facts), Some(places)) = (facts.as_mut(), places.as_ref()) {
                            let _ = facts.add(&path, &fact_of(&path, modified, places));
                        }
                        times.push_back(started.elapsed().as_secs_f64() * 1000.0);
                        if times.len() > 64 {
                            times.pop_front();
                        }
                        let mut sorted: Vec<f64> = times.iter().copied().collect();
                        sorted.sort_by(f64::total_cmp);
                        let median = (sorted[sorted.len() / 2] * 10.0).round() / 10.0;
                        status(&|looking| looking.ms = Some(median));
                        if on_trial && times.len() >= TRIED {
                            if median > TOO_SLOW_MS {
                                config::log(&format!("{}: on trial, {median} ms an image: too slow", engine.name()));
                                let error = format!("It took {:.1} s an image here, too long for a whole library.", median / 1000.0);
                                status(&|looking| looking.model = config::status::Model { state: "unsuitable".into(), name: label(engine).into(), error: error.clone(), ..Default::default() });
                                set_server(&None);
                                return;
                            }
                            config::log(&format!("{}: passed its trial, {median} ms an image", engine.name()));
                            catalog.set_meta(&trial_key(engine), 1);
                            on_trial = false;
                        }
                    }
                    Err(error) => {
                        config::log(&format!("{}: keeping {path}'s vector: {error}", engine.name()));
                        if !context.control.sleep(Duration::from_secs(60)) {
                            return;
                        }
                        break;
                    }
                },
                Err(error) => {
                    config::log(&format!("{} on {path}: {error}", engine.name()));
                    // Its server stopped on it: started again for it once before it counts as the image's doing.
                    if open.running() || crashes.blame(&path) {
                        catalog.seen_set(&path, engine.name(), modified, work::now(), false);
                    } else {
                        break;
                    }
                }
            }
            idle_since = Instant::now();
        }
    }
    set_server(&None);
}

/// Documents with their stored text in pieces (`pieces`): path, date, pieces.
type Prepared = Vec<(String, i64, Vec<String>)>;

/// Documents' text as the index has it now, in pieces.
fn prepare(reader: &content::Reader, documents: Vec<(String, u64, i64)>) -> Prepared {
    documents.into_iter().map(|(path, _, modified)| {
        let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        let texts = reader.stored(&path).map(|stored| pieces(&name, &stored.text)).unwrap_or_default();
        (path, modified, texts)
    }).collect()
}

/// A document's vectors in place of those it had: false when there are none (it failed) or they couldn't be kept.
fn keep(writer: &mut seeing::Writer, path: &str, modified: i64, vectors: Option<&[Vec<f32>]>) -> bool {
    let old: Vec<String> = writer.parts_of(path).into_iter().filter(|key| key.contains("#c=")).collect();
    for key in old {
        let _ = writer.remove(&key);
    }
    let Some(vectors) = vectors else { return false };
    vectors.iter().enumerate().all(|(piece, vector)| writer.add(&format!("{path}#c={piece}"), modified, vector).is_ok())
}

/// At Full: documents for their meaning a batch at a time (`meaning_batch`), all of a batch's pieces in one request
/// that the server spreads over its slots. While the graphics card works on one batch, a thread beside it writes the
/// last batch's vectors and records them, then makes the next batch ready (the catalog asked, the stored text read and
/// cut in pieces): before, the card waited for both. The requests are the same as before, so the vectors are. Until
/// the documents run out, images or videos wait (they come first: back to the lane's loop), or it's stopped, paused or
/// the schedule closes. `relook`: when images were last asked to be looked at again, and videos (None: no videos for
/// this lane).
#[allow(clippy::too_many_arguments)]
fn side_by_side(context: &Context, catalog: &Catalog, eye: &mut Eye, writer: &mut seeing::Writer, reader: &mut content::Reader, first: Vec<(String, u64, i64)>,
                engine: Engine, relook: (i64, Option<i64>), need: Need, crashes: &mut work::Crashes) {
    let (relook_at, relook_videos) = relook;
    let size = meaning_batch();
    // (The catalog hears of a document once its words are committed: the index's reader takes in what was committed
    // before it's asked, else a document read since would have no text here and be kept without vectors.)
    reader.refresh_now();
    let mut batch = prepare(reader, first);
    // The last batch's documents and vectors, to be written.
    let mut done: Option<(Prepared, Vec<Vec<f32>>)> = None;
    let record = |writer: &mut seeing::Writer, (documents, vectors): (Prepared, Vec<Vec<f32>>)| {
        let mut vectors = vectors.into_iter();
        for (path, modified, texts) in documents {
            let mine: Vec<Vec<f32>> = vectors.by_ref().take(texts.len()).collect();
            let ok = keep(writer, &path, modified, Some(&mine));
            catalog.seen_set(&path, Engine::GemmaText.name(), modified, work::now(), ok);
        }
    };
    while !batch.is_empty() {
        if context.control.stopping() || !context.seeing_gate.open() || work::seeing_pause(context, need).is_some() {
            break;
        }
        // (Counted as the documents it gets through; in hand, the whole batch.)
        let mut item = context.shared.paces.item(crate::pace::Lane::Meaning);
        item.count = 0;
        context.shared.paces.holding(batch.len() as u32);
        let all: Vec<String> = batch.iter().flat_map(|(_, _, texts)| texts.iter().cloned()).collect();
        let in_flight: HashSet<String> = batch.iter().map(|(path, ..)| path.to_lowercase()).collect();
        let last = done.take();
        let (answer, next) = std::thread::scope(|scope| {
            let beside = scope.spawn(|| {
                if let Some(last) = last {
                    record(&mut *writer, last);
                }
                if !catalog.seen_next(engine.name(), relook_at, 1).is_empty()
                    || relook_videos.is_some_and(|relook| !catalog.video_next(engine.name(), relook, 1, config::video_kind::SHOWN).is_empty()) {
                    return Prepared::new();
                }
                let next = catalog.text_next(Engine::GemmaText.name(), relook_at, size * 2).into_iter()
                    .filter(|(path, ..)| !in_flight.contains(&path.to_lowercase())).take(size).collect();
                reader.refresh_now();
                prepare(reader, next)
            });
            let answer = if all.is_empty() { Ok(Vec::new()) } else { eye.read(&all, true) };
            (answer, beside.join().unwrap_or_default())
        });
        match answer {
            Ok(vectors) => {
                item.count = batch.len() as u64;
                done = Some((batch, vectors));
            }
            // Each document on its own after all, to find the one it was.
            Err(error) => {
                config::log(&format!("{} on a batch of {}: {error}", Engine::GemmaText.name(), batch.len()));
                let mut server_stopped = false;
                for (path, modified, texts) in batch {
                    if context.control.stopping() || !context.seeing_gate.open() || work::seeing_pause(context, need).is_some() {
                        break;
                    }
                    let result = if texts.is_empty() { Ok(Vec::new()) } else { eye.read(&texts, true) };
                    let ok = match result {
                        Ok(vectors) => keep(writer, &path, modified, Some(&vectors)),
                        Err(error) => {
                            config::log(&format!("{} on {path}: {error}", Engine::GemmaText.name()));
                            // Its server stopped on it: started again for it once before it counts as the file's doing.
                            if !eye.running() && !crashes.blame(&path) {
                                keep(writer, &path, modified, None);
                                server_stopped = true;
                                break;
                            }
                            keep(writer, &path, modified, None)
                        }
                    };
                    catalog.seen_set(&path, Engine::GemmaText.name(), modified, work::now(), ok);
                    item.count += 1;
                }
                if server_stopped {
                    break;
                }
            }
        }
        batch = next;
    }
    if let Some(last) = done {
        record(writer, last);
    }
}

/// The index's reader, opened once, with what was committed since.
fn refreshed<'a>(contents: &'a mut Option<content::Reader>, index: &Path) -> Option<&'a content::Reader> {
    match contents.as_mut() {
        Some(reader) => reader.refresh(),
        None => *contents = content::Reader::open(index).ok(),
    }
    contents.as_ref()
}

/// The images each lane's model sees as a passport, an ID or a card, by lowercase path and lane: whatever writes an
/// image's marks afresh (a rules update, "Read again") keeps the whole-picture box at once (`keep_whole`), rather than
/// leaving the image unblurred until the next tidy.
static WHOLE: Mutex<BTreeMap<(String, usize), Shows>> = Mutex::new(BTreeMap::new());

fn whole() -> std::sync::MutexGuard<'static, BTreeMap<(String, usize), Shows>> {
    WHOLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A lane's images blurred whole, as its store has them when it starts.
fn whole_from<'a>(slot: usize, sensitive: impl Iterator<Item = (&'a str, Shows)>) {
    let mut whole = whole();
    whole.retain(|(_, lane), _| *lane != slot);
    for (path, shows) in sensitive {
        whole.insert((path.to_lowercase(), slot), shows);
    }
}

/// What a lane's model sees in an image after looking again (none: nothing that blurs it whole, or gone).
fn note_whole(slot: usize, path: &str, shows: Option<Shows>) {
    let key = (path.to_lowercase(), slot);
    match shows {
        Some(shows) => whole().insert(key, shows),
        None => whole().remove(&key),
    };
}

/// A file's marks as they're written (`work`'s writer): an image a model sees as a passport, an ID or a card keeps its
/// box over the whole picture.
pub fn keep_whole(doc: &mut content::Doc) {
    let path = doc.path.to_lowercase();
    let Some(shows) = whole().range((path.clone(), 0)..=(path, usize::MAX)).map(|(_, shows)| *shows).next() else { return };
    let kind = match shows {
        Shows::Document => privacy::Kind::Document.name(),
        Shows::Card => privacy::Kind::Card.name(),
    };
    if let Some(marks) = whole_marks(&doc.marks, kind, || winsys::imaging::size(&doc.path)) {
        doc.marks = marks;
    }
}

/// An image the model sees as a passport, an ID card, a driving licence or a bank card (`seeing::sensitive`): a box over
/// all of it goes in its privacy marks, as for a secret only Deep read, so the preview blurs the whole picture until
/// Reveal and says what's hidden. Nothing when it has that box already. An image with no words read gets an entry of
/// its own, without words. (No new field: the marks' format is the same.)
fn mark_whole(context: &Context, reader: &content::Reader, path: &str, size: u64, modified: i64, shows: Shows) {
    let kind = match shows {
        Shows::Document => privacy::Kind::Document.name(),
        Shows::Card => privacy::Kind::Card.name(),
    };
    let stored = reader.stored(path);
    let Some(marks) = whole_marks(stored.as_ref().map_or(&[][..], |stored| &stored.marks), kind, || winsys::imaging::size(path)) else { return };
    let (body, source) = stored.map(|stored| (stored.text, stored.source.unwrap_or(content::Source::Picture))).unwrap_or((String::new(), content::Source::Picture));
    let doc = content::Doc {
        path: path.to_string(),
        name: path.rsplit('\\').next().unwrap_or(path).to_string(),
        kind: Reader::Picture.kind(),
        modified,
        size,
        source,
        body,
        marks,
    };
    config::log(&format!("{path}: blurred whole, it shows {}", if shows == Shows::Card { "a bank card" } else { "a passport or ID" }));
    let _ = context.sender.send(Write::Words(Box::new(doc)));
}

/// A file's privacy marks with a box over the whole picture and `kind` among its kinds; none when they have both already
/// (or the picture's size can't be told). The size is the one its other boxes were placed in, else `size()`'s (as stored).
fn whole_marks(marks: &[u8], kind: &str, size: impl FnOnce() -> Option<(u32, u32)>) -> Option<Vec<u8>> {
    let mut marks: serde_json::Value = serde_json::from_slice(marks).ok().filter(serde_json::Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    let mut kinds: Vec<String> = marks["kinds"].as_array().map(|kinds| kinds.iter().filter_map(|kind| kind.as_str().map(String::from)).collect()).unwrap_or_default();
    let known = (marks["picture"]["w"].as_u64().unwrap_or(0) as u32, marks["picture"]["h"].as_u64().unwrap_or(0) as u32);
    let (width, height) = Some(known).filter(|&(width, height)| width > 0 && height > 0).or_else(size)?;
    let whole = serde_json::json!([0, 0, width, height]);
    if kinds.iter().any(|known| known == kind) && marks["picture"]["boxes"].as_array().is_some_and(|boxes| boxes.contains(&whole)) {
        return None;
    }
    kinds.push(kind.to_string());
    kinds.sort_unstable();
    kinds.dedup();
    marks["kinds"] = serde_json::json!(kinds);
    marks["picture"] = serde_json::json!({ "w": width, "h": height, "boxes": [whole] });
    serde_json::to_vec(&marks).ok()
}

/// `--seeing-bench <list.txt> <models> <siglip|gemma> <out> [gpu]`: each image in the list (a path a line) looked at as
/// the lane does, and each description in `<out>.prompts` (a line each) as the search helper has it; writes `<out>.bin`
/// (unit-length f32 vectors, the descriptions' first, then the images') and `<out>.json` (their names, the time each
/// image took). For the privacy benchmark's whole-image sensitivity.
pub fn seeing_bench() {
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|arg| arg == "--seeing-bench").unwrap_or(0);
    let (Some(list), Some(models), Some(engine), Some(out)) = (args.get(at + 1), args.get(at + 2), args.get(at + 3), args.get(at + 4)) else { return };
    let gpu = args.get(at + 5).is_some_and(|arg| arg == "gpu");
    let engine = if engine == "gemma" { Engine::Gemma } else { Engine::Siglip };
    let models = Path::new(models);
    winsys::com_mta();
    let prompts: Vec<String> = std::fs::read_to_string(format!("{out}.prompts")).unwrap_or_default().lines()
        .map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect();
    let mut eye = match Eye::open(models, engine, gpu, &|| false) {
        Ok(eye) => eye,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    let texts = match &mut eye {
        Eye::Siglip(_) => SiglipText::load(models, crate::ocr::threads())
            .and_then(|mut siglip| prompts.iter().map(|prompt| siglip.text(prompt)).collect::<Result<Vec<_>, _>>()),
        Eye::Llama { .. } => eye.read(&prompts.iter().map(|prompt| query_prompt(engine, prompt)).collect::<Vec<_>>(), true),
    };
    match texts {
        Ok(texts) => vectors.extend(texts.iter().map(|vector| seeing::normalized(vector))),
        Err(error) => {
            eprintln!("descriptions: {error}");
            return;
        }
    }
    let paths: Vec<String> = std::fs::read_to_string(list).unwrap_or_default().lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_string).collect();
    let mut names = Vec::new();
    let mut times = Vec::new();
    for path in paths {
        let started = Instant::now();
        match eye.look(&path) {
            Ok(vector) => {
                vectors.push(seeing::normalized(&vector));
                times.push(started.elapsed().as_secs_f64() * 1000.0);
                names.push(path);
            }
            Err(error) => eprintln!("{path}: {error}"),
        }
    }
    let bytes: Vec<u8> = vectors.iter().flatten().flat_map(|value| value.to_le_bytes()).collect();
    let _ = std::fs::write(format!("{out}.bin"), bytes);
    let _ = std::fs::write(format!("{out}.json"), serde_json::json!({ "dims": engine.dims(), "prompts": prompts, "images": names, "ms": times }).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_cleaned_as_siglip_learned_them() {
        assert_eq!(canonicalize("  A dog_on the  beach!  "), "a dog on the beach");
        assert_eq!(canonicalize("Tom &amp; Jerry's \"house\""), "tom  jerrys house".split_whitespace().collect::<Vec<_>>().join(" "));
    }

    #[test]
    fn documents_in_pieces() {
        assert!(pieces("empty.txt", "   ").is_empty());
        assert_eq!(pieces("a.txt", "Kitchen  renovation\nquote"), ["title: a.txt | text: Kitchen renovation quote"]);
        let long = "word ".repeat(5000);
        let all = pieces("long.txt", &long);
        assert_eq!(all.len(), 8);
        assert!(all.iter().all(|piece| piece.len() <= 1500 + 30 && !piece.ends_with(" wo")));
    }

    #[test]
    fn a_whole_picture_in_the_marks() {
        let parse = |bytes: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap();
        // An image with no marks (or no words): the size as stored.
        let marks = parse(whole_marks(b"", "document", || Some((800, 600))).unwrap());
        assert_eq!(marks, serde_json::json!({ "kinds": ["document"], "picture": { "w": 800, "h": 600, "boxes": [[0, 0, 800, 600]] } }));
        // With a secret already boxed: its size, its other kinds and the rest kept, the box over all of it.
        let before = serde_json::json!({ "kinds": ["password"], "picture": { "w": 1000, "h": 500, "boxes": [[10, 10, 90, 30]] }, "similar": 3 });
        let marks = parse(whole_marks(&serde_json::to_vec(&before).unwrap(), "card", || None).unwrap());
        assert_eq!(marks["kinds"], serde_json::json!(["card", "password"]));
        assert_eq!(marks["picture"]["boxes"], serde_json::json!([[0, 0, 1000, 500]]));
        assert_eq!(marks["similar"], 3);
        // Already so: nothing to write.
        assert!(whole_marks(&serde_json::to_vec(&marks).unwrap(), "card", || None).is_none());
        // No size to be had.
        assert!(whole_marks(b"", "document", || None).is_none());
    }

    #[test]
    fn a_re_read_keeps_the_whole_picture_box() {
        let doc = |path: &str, marks: &[u8]| content::Doc {
            path: path.to_string(),
            name: String::new(),
            kind: "image",
            modified: 0,
            size: 0,
            source: content::Source::Picture,
            body: String::new(),
            marks: marks.to_vec(),
        };
        let before = serde_json::json!({ "kinds": ["password"], "picture": { "w": 640, "h": 480, "boxes": [[1, 2, 3, 4]] } });
        let before = serde_json::to_vec(&before).unwrap();
        // Not seen as an ID: its marks as written.
        let mut plain = doc(r"C:\Test\Plain.png", &before);
        keep_whole(&mut plain);
        assert_eq!(plain.marks, before);
        // Seen as an ID by one lane: the box is back as soon as the marks are written again.
        note_whole(7, r"C:\Test\ID.png", Some(Shows::Document));
        let mut id = doc(r"c:\test\id.PNG", &before);
        keep_whole(&mut id);
        let marks = serde_json::from_slice::<serde_json::Value>(&id.marks).unwrap();
        assert_eq!(marks["kinds"], serde_json::json!(["document", "password"]));
        assert_eq!(marks["picture"]["boxes"], serde_json::json!([[0, 0, 640, 480]]));
        // Looked at again and no longer one: left as written.
        note_whole(7, r"C:\Test\ID.png", None);
        let mut id = doc(r"C:\Test\ID.png", &before);
        keep_whole(&mut id);
        assert_eq!(id.marks, before);
    }

    #[test]
    fn vectors_out_of_llama() {
        let flat = serde_json::json!([{ "index": 0, "embedding": [0.5, -0.25] }]);
        assert_eq!(vector(&flat).unwrap(), [0.5, -0.25]);
        let nested = serde_json::json!([{ "index": 0, "embedding": [[0.1, 0.2], [0.3, 0.4]] }]);
        assert_eq!(vector(&nested).unwrap(), [0.3, 0.4]);
        assert!(vector(&serde_json::json!({ "error": "x" })).is_err());
    }
}
