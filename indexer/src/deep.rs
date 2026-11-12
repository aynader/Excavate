//! Deep: PaddleOCR-VL-1.5 reading pictures on the graphics card, through llama.cpp's server. The server is its own
//! process, on this PC only, started when there are pictures for it and stopped after, which gives the graphics card's
//! memory back. It ends with the indexer even if the indexer is killed (a job object).

use crate::models;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// What the catalog records for a picture Deep has read.
pub const ENGINE: &str = "paddleocr-vl-1.5";

/// Pictures are decoded at most this large for it (the longer side).
pub const MAX_SIDE: u32 = 2048;

/// PaddleOCR-VL's prompt for plain text recognition (its makers').
const PROMPT: &str = "OCR:";

/// The longest answer, in tokens. A real page is far below it; a model repeating itself stops here.
const MAX_TOKENS: u32 = 3000;

pub struct Server {
    child: Child,
    port: u16,
    _job: Option<winsys::process::Job>,
    /// What it's for, as its log lines say ("Deep", an image model, the search helper's).
    what: String,
    /// The key it asks of every request (random, new each time it starts): nothing else on this PC, a web page
    /// renamed to 127.0.0.1 included, can use it or read what it answers.
    key: String,
}

/// The card Excavate chose (`winsys::gpu::capable`, the one with the most memory of its own) as llama.cpp's Vulkan
/// lists it, "Vulkan1", found by its name; asked once. None when there's no such card or Vulkan doesn't list it (then
/// llama.cpp picks, and it prefers a card of its own to a processor's graphics).
fn vulkan_device(models: &Path) -> Option<String> {
    // By the card's name: asked again only when the chosen card changes.
    static DEVICE: std::sync::Mutex<Option<(String, Option<String>)>> = std::sync::Mutex::new(None);
    let card = winsys::gpu::capable()?;
    let mut known = DEVICE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, device)) = known.as_ref().filter(|(name, _)| *name == card.name) {
        return device.clone();
    }
    let device = (|| {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = Command::new(models::llama_folder(models).join("llama-server.exe")).arg("--list-devices")
            .creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).output().ok()?;
        let listed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let device = vulkan_named(&listed, &card.name);
        match &device {
            Some(device) => config::log(&format!("llama.cpp: the graphics card is {device} ({})", card.name)),
            None => config::log(&format!("llama.cpp: {} isn't among its devices; it picks", card.name)),
        }
        device
    })();
    *known = Some((card.name.clone(), device.clone()));
    device
}

/// From llama.cpp's `--list-devices` ("  Vulkan1: NVIDIA GeForce RTX 4080 Laptop GPU (11995 MiB, 11227 MiB free)"), the
/// device with this name (letters and digits compared, case aside).
fn vulkan_named(listed: &str, name: &str) -> Option<String> {
    let plain = |text: &str| text.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let wanted = plain(name);
    listed.lines().filter_map(|line| line.trim().split_once(": ")).find(|(id, rest)| {
        id.starts_with("Vulkan") && plain(rest.rsplit_once(" (").map_or(rest, |(device, _)| device)) == wanted
    }).map(|(id, _)| id.to_string())
}

impl Server {
    /// Starts the server and waits until the model is loaded (about ten seconds). `stopping` cuts the wait short.
    /// `gpu`: on the graphics card; else on the processor (27 to 38 s an image).
    pub fn start(models: &Path, gpu: bool, stopping: &dyn Fn() -> bool) -> Result<Server, String> {
        let folder = models::deep_folder(models);
        let model = folder.join(models::DEEP_MODEL).display().to_string();
        let vision = folder.join(models::DEEP_VISION).display().to_string();
        let layers = if gpu { "99" } else { "0" };
        let mut args = vec!["-m", &model, "--mmproj", &vision, "-c", "8192", "-np", "1", "-ngl", layers, "--temp", "0"];
        if !gpu {
            args.extend(["--no-mmproj-offload", "--device", "none"]);
        }
        Server::launch(models, &args, true, "Deep", stopping)
    }

    /// Starts llama.cpp's server with `args` (the model and what it's for; where it listens is added), at idle priority
    /// when `idle` (the reader's work; a search's isn't), and waits until the model is loaded. `stopping` cuts it short.
    pub fn launch(models: &Path, args: &[&str], idle: bool, what: &str, stopping: &dyn Fn() -> bool) -> Result<Server, String> {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
        let port = free_port()?;
        let key = winsys::signature::random_hex(16);
        // (A server unpacked before its folder got the C++ runtime gets it now.)
        models::with_runtime(&models::llama_folder(models));
        // On the graphics card: the one Excavate chose, named (Vulkan lists a processor's own graphics too, often first).
        let on_card = args.windows(2).any(|pair| pair[0] == "-ngl" && pair[1] != "0") && !args.contains(&"--device");
        let device = if on_card { vulkan_device(models) } else { None };
        let child = Command::new(models::llama_folder(models).join("llama-server.exe"))
            .args(args)
            .args(device.iter().flat_map(|device| ["--device", device.as_str()]))
            .args(["--host", "127.0.0.1", "--port", &port.to_string(), "--no-webui", "--log-disable", "--api-key", &key])
            .creation_flags(CREATE_NO_WINDOW | if idle { IDLE_PRIORITY_CLASS } else { 0 })
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("can't start llama.cpp: {error}"))?;
        let job = winsys::process::Job::new();
        if let Some(job) = &job {
            job.add(child.id());
        }
        let mut server = Server { child, port, _job: job, what: what.to_string(), key };
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(180) {
            if stopping() {
                return Err("stopping".into());
            }
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!("llama.cpp stopped ({status}): the graphics card may not have enough memory"));
            }
            if matches!(server.request("health", &[], Duration::from_secs(2)), Ok((200, _))) {
                config::log(&format!("{what}: the server is ready on port {port}, {} ms", started.elapsed().as_millis()));
                return Ok(server);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err("llama.cpp didn't get ready in three minutes".into())
    }

    /// A request to it, with its key.
    fn request(&self, path: &str, body: &[u8], timeout: Duration) -> Result<(u64, Vec<u8>), String> {
        winsys::http::local_with_key(&format!("http://127.0.0.1:{}/{path}", self.port), body, timeout, Some(&self.key))
    }

    /// A request to the server, its answer as JSON (a GET with no body).
    pub fn ask(&self, path: &str, body: Option<&serde_json::Value>, timeout: Duration) -> Result<serde_json::Value, String> {
        let body = match body {
            Some(body) => serde_json::to_vec(body).map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        let (status, answer) = self.request(path, &body, timeout)?;
        if status != 200 {
            return Err(format!("llama.cpp answered {status}: {}", String::from_utf8_lossy(&answer[..answer.len().min(200)])));
        }
        serde_json::from_slice(&answer).map_err(|error| error.to_string())
    }

    /// Its process, to leave out of what the PC is judged by.
    pub fn process(&self) -> u32 {
        self.child.id()
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// The text in a picture (8-bit RGB).
    pub fn read(&self, width: u32, height: u32, pixels: &[u8]) -> Result<String, String> {
        // PPM: no compression to spend time on, for a trip inside this PC.
        let mut image = format!("P6\n{width} {height}\n255\n").into_bytes();
        image.extend_from_slice(pixels);
        let body = serde_json::json!({
            "messages": [{ "role": "user", "content": [
                { "type": "image_url", "image_url": { "url": format!("data:image/x-portable-pixmap;base64,{}", base64(&image)) } },
                { "type": "text", "text": PROMPT },
            ] }],
            "temperature": 0,
            "max_tokens": MAX_TOKENS,
        });
        let body = serde_json::to_vec(&body).map_err(|error| error.to_string())?;
        let (status, answer) = self.request("v1/chat/completions", &body, Duration::from_secs(180))?;
        if status != 200 {
            return Err(format!("llama.cpp answered {status}: {}", String::from_utf8_lossy(&answer[..answer.len().min(200)])));
        }
        let answer: serde_json::Value = serde_json::from_slice(&answer).map_err(|error| error.to_string())?;
        Ok(clean(answer["choices"][0]["message"]["content"].as_str().unwrap_or("")))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        config::log(&format!("{}: the server stopped", self.what));
    }
}

/// A port nobody's listening on right now.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener.local_addr().map(|address| address.port()).map_err(|error| error.to_string())
}

/// The model's table and layout marks out of its text.
pub fn clean(text: &str) -> String {
    let mut out = text.replace("<nl>", "\n");
    for mark in ["<fcel>", "<lcel>", "<ucel>", "<xcel>", "<ecel>", "<|LOC_", "<|im_end|>", "</s>"] {
        out = out.replace(mark, " ");
    }
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines.join("\n")
}

pub fn base64(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(LETTERS[(value >> 18) as usize & 63]);
        out.push(LETTERS[(value >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 { LETTERS[(value >> 6) as usize & 63] } else { b'=' });
        out.push(if chunk.len() > 2 { LETTERS[value as usize & 63] } else { b'=' });
    }
    // SAFETY: only ASCII letters from the table above.
    unsafe { String::from_utf8_unchecked(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn cleans() {
        assert_eq!(clean("<fcel>Name<fcel>Total<nl><fcel>Tea<fcel>4<nl>"), "Name Total\nTea 4");
        assert_eq!(clean("  plain line \n\n<div>kept</div>"), "plain line\n<div>kept</div>");
    }
}

#[cfg(test)]
mod device_tests {
    #[test]
    fn finds_the_card_by_name() {
        let listed = "Available devices:
  Vulkan0: Intel(R) RaptorLake-S Mobile Graphics Controller (16253 MiB, 15485 MiB free)
  Vulkan1: NVIDIA GeForce RTX 4080 Laptop GPU (11995 MiB, 11227 MiB free)
";
        assert_eq!(super::vulkan_named(listed, "NVIDIA GeForce RTX 4080 Laptop GPU").as_deref(), Some("Vulkan1"));
        assert_eq!(super::vulkan_named(listed, "AMD Radeon RX 6600"), None);
    }
}
