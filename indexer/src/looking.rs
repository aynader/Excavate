//! Images by what they show: each chosen model looks at the images the first pass has read, newest first, and keeps a
//! vector for each (`seeing::Writer`, in Excavate's `seeing` folder). The search helper (`seek`) scores descriptions
//! against them. EmbeddingGemma 2 and Qwen3-VL-Embedding run in llama.cpp's server on the graphics card, as Deep does;
//! SigLIP 2 in ONNX Runtime on the CPU, as the first pass's OCR does.

use crate::catalog::Catalog;
use crate::deep::Server;
use crate::models;
use crate::scan::FileInfo;
use crate::work::{self, Context, Need};
use extract::Reader;
use seeing::Engine;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Images are decoded at most this large for a model on the graphics card (the longer side); it shrinks them further
/// to its own budget (140 image tokens for EmbeddingGemma 2, up to 256 for Qwen).
const MAX_SIDE: u32 = 1024;

/// What EmbeddingGemma 2's model card asks of a description; images take no prefix.
pub const GEMMA_QUERY: &str = "task: search result | query: ";
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
                let mut args = vec!["-m", &model, "--mmproj", &vision, "--embedding", "-c", "8192", "-b", "4096", "-ub", "4096", "-np", "1", "-ngl", if gpu { "99" } else { "0" }];
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

    /// Texts' vectors, one each (documents' pieces, for their meaning): EmbeddingGemma 2's only.
    pub fn read(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let Eye::Llama { server, .. } = self else { return Err("only EmbeddingGemma 2 reads documents for their meaning".into()) };
        let answer = server.ask("embeddings", Some(&serde_json::json!({ "content": texts })), Duration::from_secs(120))?;
        let mut items: Vec<&serde_json::Value> = answer.as_array().map(|items| items.iter().collect()).unwrap_or_default();
        items.sort_by_key(|item| item["index"].as_u64().unwrap_or(0));
        items.into_iter().map(|item| vector(&serde_json::Value::Array(vec![item.clone()]))).collect()
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
        if end < rest.len() {
            if let Some(space) = rest[..end].rfind(char::is_whitespace).filter(|&space| space > PIECE / 2) {
                end = space;
            }
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
    // EmbeddingGemma 2's lane also reads documents for their meaning, into vectors of their own.
    let mut meaning = if engine == Engine::Gemma { seeing::Writer::open(&folder, Engine::GemmaText).ok() } else { None };
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
    // A failed download is tried again soon, then less often: half a minute, 2 minutes, then every 10.
    let mut failures = 0usize;
    loop {
        if context.control.stopping() {
            break;
        }
        // Gone from what's read: its vector goes too (now and then, and at the start).
        if tidied.elapsed() > Duration::from_secs(300) {
            let pictures = catalog.picture_ids();
            // (A video's frames go with their video.)
            let gone: Vec<String> = writer.paths().filter(|path| !pictures.contains(path.split("#t=").next().unwrap_or(path))).cloned().collect();
            for path in &gone {
                let _ = writer.remove(path);
                if let Some(facts) = facts.as_mut() {
                    let _ = facts.remove(path);
                }
            }
            if !gone.is_empty() {
                config::log(&format!("{}: {} images gone", engine.name(), gone.len()));
            }
            if let Some(meaning) = meaning.as_mut() {
                let documents = catalog.text_ids();
                let gone: Vec<String> = meaning.paths().filter(|path| !documents.contains(path.split("#c=").next().unwrap_or(path))).cloned().collect();
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
        let documents = if batch.is_empty() && video.is_none() && meaning.is_some() {
            catalog.text_next(Engine::GemmaText.name(), relook_at, 16)
        } else {
            Vec::new()
        };
        if batch.is_empty() && video.is_none() && documents.is_empty() {
            // Images looked at before facts were kept: theirs, a few hundred at a time while there's nothing else.
            // Not a video's frames ("…mp4#t=12.0"): a frame isn't a file, so it had none to read and was kept as
            // taken in 1970; those are taken out again.
            if let (Some(facts), Some(places)) = (facts.as_mut(), places.as_ref()) {
                let frames: Vec<String> = writer.paths().filter(|path| path.contains("#t=") && facts.has(path)).take(300).cloned().collect();
                for path in frames {
                    let _ = facts.remove(&path);
                }
                let missing: Vec<String> = writer.paths().filter(|path| !path.contains("#t=") && !facts.has(path)).take(300).cloned().collect();
                for path in missing {
                    let modified = std::fs::metadata(&path).ok().map(|metadata| crate::scan::seconds(&metadata)).unwrap_or(0);
                    let _ = facts.add(&path, &fact_of(&path, modified, places));
                }
            }
            if eye.is_some() && idle_since.elapsed() > Duration::from_secs(60) {
                eye = None;
                set_server(&eye);
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
        if let (false, Some(writer)) = (documents.is_empty(), meaning.as_mut()) {
            let _busy = work::Busy::new(&context.shared);
            if !work::wait_seeing(&context, need, &noting) {
                break;
            }
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
            let Some(reader) = contents.as_ref() else { continue };
            for (path, _, modified) in documents {
                if context.control.stopping() || !context.seeing_gate.open() || work::seeing_pause(&context, need).is_some() {
                    break;
                }
                let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
                let texts = reader.stored(&path).map(|stored| pieces(&name, &stored.text)).unwrap_or_default();
                let before = format!("{}#c=", path.to_lowercase());
                let old: Vec<String> = writer.paths().filter(|key| key.starts_with(&before)).cloned().collect();
                for key in old {
                    let _ = writer.remove(&key);
                }
                let result = if texts.is_empty() { Ok(Vec::new()) } else { open.read(&texts) };
                let ok = match result {
                    Ok(vectors) => vectors.iter().enumerate().all(|(piece, vector)| writer.add(&format!("{path}#c={piece}"), modified, vector).is_ok()),
                    Err(error) => {
                        config::log(&format!("{} on {path}: {error}", Engine::GemmaText.name()));
                        false
                    }
                };
                catalog.seen_set(&path, Engine::GemmaText.name(), modified, work::now(), ok);
            }
            idle_since = Instant::now();
            continue;
        }
        if let Some((path, _, modified)) = video {
            let _busy = work::Busy::new(&context.shared);
            if !work::wait_seeing(&context, need, &noting) {
                break;
            }
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
            let before = format!("{}#t=", path.to_lowercase());
            let old: Vec<String> = writer.paths().filter(|key| key.starts_with(&before)).cloned().collect();
            for key in old {
                let _ = writer.remove(&key);
            }
            let (mut frames, mut stopped, mut failed, mut skipped) = (0, false, None, false);
            let side = open.side();
            // The next frames are decoded (on the graphics hardware Windows draws with, mostly) while the model looks
            // at this one: two at most wait between them.
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
                (Ok(_), None) => {
                    config::log(&format!("{}: {path}: {frames} frames in {:.1} s", engine.name(), started.elapsed().as_secs_f64()));
                    catalog.seen_set(&path, engine.name(), modified, work::now(), true);
                }
                (Err(error), _) | (_, Some(error)) => {
                    config::log(&format!("{} on {path}: {error}", engine.name()));
                    catalog.seen_set(&path, engine.name(), modified, work::now(), false);
                }
            }
            idle_since = Instant::now();
            continue;
        }
        // The first time on this PC: where it looks fastest, on these images.
        if context.seeing_measure.is_some() && batch.len() >= 4 && crate::devices::quiet(&context) {
            if let Some(key) = context.seeing_measure.take() {
                // (Its own models are opened to measure: the one open now makes room.)
                eye = None;
                set_server(&eye);
                let paths: Vec<String> = batch.iter().take(crate::devices::ITEMS + 1).map(|(path, ..)| path.clone()).collect();
                if let Some(faster) = measure(&context, &models, engine, gpu, &key, &paths) {
                    if faster != gpu {
                        gpu = faster;
                        need = if gpu { Need::Gpu } else { Need::Cpu };
                        context.shared.seeing_gpu.store(gpu, Ordering::Relaxed);
                    }
                }
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
                    catalog.seen_set(&path, engine.name(), modified, work::now(), false);
                }
            }
            idle_since = Instant::now();
        }
    }
    set_server(&None);
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
    fn vectors_out_of_llama() {
        let flat = serde_json::json!([{ "index": 0, "embedding": [0.5, -0.25] }]);
        assert_eq!(vector(&flat).unwrap(), [0.5, -0.25]);
        let nested = serde_json::json!([{ "index": 0, "embedding": [[0.1, 0.2], [0.3, 0.4]] }]);
        assert_eq!(vector(&nested).unwrap(), [0.3, 0.4]);
        assert!(vector(&serde_json::json!({ "error": "x" })).is_err());
    }
}
