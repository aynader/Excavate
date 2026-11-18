//! The models Excavate reads with: downloaded once, checked against their SHA-256, kept in %LOCALAPPDATA%\Excavate.
//!
//! - Text in images, Quick and Balanced: PaddleOCR's PP-OCRv6 exports, from oar-ocr's release (6 and 31 MB).
//! - Text in images, Deep: PaddleOCR-VL-1.5 as GGUF, and Excavate's own copy of llama.cpp's server to run it (1.7 GB).
//! - Images by what they show: SigLIP 2, EmbeddingGemma 2 or Qwen3-VL-Embedding (with llama.cpp's server).
//! - Recordings by what's said in them: the listener and its speech models.
//!
//! A download that stops continues where it was the next time. A download goes on through a restart of reading (the
//! settings changed, "Full speed", a pause): it runs on a thread of its own, and the next session's lane takes it up
//! (`in_background`).

use crate::control::Control;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

const BASE: &str = "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/";

pub struct File {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// Its address, when it isn't in oar-ocr's release.
    pub url: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Tiny,
    Small,
}

impl Size {
    pub fn files(self) -> [File; 3] {
        match self {
            Size::Tiny => [
                File { name: "pp-ocrv6_tiny_det.onnx", size: 1_780_590, sha256: "193bab7a04fca699a6c82e6abb5b81bdb28177f0abd4062552b04908dafb19f8", url: None },
                File { name: "pp-ocrv6_tiny_rec.onnx", size: 4_462_639, sha256: "9ef676d6ed3c88256a2d92c640c44f25b0c40947e111b14b8be8f594091563e6", url: None },
                File { name: "ppocrv6_tiny_dict.txt", size: 27_156, sha256: "c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd", url: None },
            ],
            Size::Small => [
                File { name: "pp-ocrv6_small_det.onnx", size: 9_880_512, sha256: "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e", url: None },
                File { name: "pp-ocrv6_small_rec.onnx", size: 21_159_378, sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634", url: None },
                File { name: "ppocrv6_dict.txt", size: 74_947, sha256: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d", url: None },
            ],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Size::Tiny => "PP-OCRv6 tiny",
            Size::Small => "PP-OCRv6 small",
        }
    }

    /// What the catalog records as the engine that read a picture.
    pub fn engine(self) -> &'static str {
        match self {
            Size::Tiny => "ppocrv6-tiny",
            Size::Small => "ppocrv6-small",
        }
    }

    pub fn download_size(self) -> u64 {
        self.files().iter().map(|file| file.size).sum()
    }
}

pub fn folder(models: &Path) -> PathBuf {
    models.join("ppocrv6")
}

/// Whether every file is there (by size; the hash was checked when it was downloaded).
pub fn present(models: &Path, size: Size) -> bool {
    all_there(&folder(models), &size.files())
}

fn all_there(folder: &Path, files: &[File]) -> bool {
    files.iter().all(|file| std::fs::metadata(folder.join(file.name)).is_ok_and(|metadata| metadata.len() == file.size))
}

/// Downloads what's missing. `progress(received, total)` over all files.
pub fn ensure(models: &Path, size: Size, control: &'static Control, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let folder = folder(models);
    in_background(&format!("text in images {}", size.label()), control, progress, move |cancelled, progress| fetch(&folder, &size.files(), cancelled, progress))
}

// ---- Deep ----------------------------------------------------------------------------------------------------------

/// llama.cpp's release this was measured with: its Vulkan build, for any graphics card.
const LLAMA: File = File {
    name: "llama-b12001-bin-win-vulkan-x64.zip",
    size: 37_490_060,
    sha256: "9fb42ffa935d334619bcf8f3fff8d0c55d6f6b80fa328b11119e7e736ecb4ed2",
    url: Some("https://github.com/ggml-org/llama.cpp/releases/download/b12001/llama-b12001-bin-win-vulkan-x64.zip"),
};

pub const DEEP_MODEL: &str = "PaddleOCR-VL-1.5.gguf";
pub const DEEP_VISION: &str = "PaddleOCR-VL-1.5-mmproj.gguf";

const DEEP_FILES: [File; 2] = [
    File {
        name: DEEP_MODEL,
        size: 935_768_992,
        sha256: "299051d54faa065abc505cc39b8383ea338fd3020c775ea3e0ba514a7022328c",
        url: Some("https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.5-GGUF/resolve/main/PaddleOCR-VL-1.5.gguf"),
    },
    File {
        name: DEEP_VISION,
        size: 881_770_496,
        sha256: "e7f1a72400fba517046f90d964e2fa0f4dac7781ee3b1bc5d2022f5f8cecbd87",
        url: Some("https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.5-GGUF/resolve/main/PaddleOCR-VL-1.5-mmproj.gguf"),
    },
];

pub fn deep_folder(models: &Path) -> PathBuf {
    models.join("paddleocr-vl-1.5")
}

/// Where the server is, unpacked (one folder per build, so a new build never mixes with an old one).
pub fn llama_folder(models: &Path) -> PathBuf {
    models.join("llama").join(LLAMA.name.trim_end_matches(".zip"))
}

pub fn deep_present(models: &Path) -> bool {
    all_there(&deep_folder(models), &DEEP_FILES) && llama_folder(models).join("llama-server.exe").is_file()
}

/// What Deep still has to download, in bytes, once for the whole of it.
pub fn deep_download_size() -> u64 {
    LLAMA.size + DEEP_FILES.iter().map(|file| file.size).sum::<u64>()
}

/// Downloads and unpacks what Deep needs. `progress(received, total)` over all of it.
pub fn deep_ensure(models: &Path, control: &'static Control, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let models = models.to_path_buf();
    in_background("Thorough", control, progress, move |cancelled, progress| {
        let total = deep_download_size();
        ensure_llama(&models, cancelled, &mut |received| progress(received, total))?;
        let before = LLAMA.size;
        fetch(&deep_folder(&models), &DEEP_FILES, cancelled, &mut |received, _| progress(before + received, total))
    })
}

/// llama.cpp's server, downloaded and unpacked once: Deep and the models that look at images both need it, and ask
/// for it each from its own lane, at the same time on a first run. One at a time (two downloads into one file made a
/// file of neither, its checksum wrong); the second finds it there.
fn ensure_llama(models: &Path, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = lock(&ONE_AT_A_TIME);
    let unpacked = llama_folder(models);
    if unpacked.join("llama-server.exe").is_file() {
        return Ok(());
    }
    let downloads = models.join("llama");
    fetch(&downloads, &[LLAMA], cancelled, &mut |received, _| progress(received))?;
    let zip = downloads.join(LLAMA.name);
    unpack_server(&zip, &unpacked)?;
    let _ = std::fs::remove_file(zip);
    Ok(())
}

/// Microsoft's C++ runtime, which Excavate's programs and llama.cpp's need and a fresh Windows may not have: it comes
/// beside Excavate's programs (its release has it), and goes beside llama.cpp's server, whose package hasn't.
/// (vcomp140.dll: its OpenMP library, which the listener's whisper.cpp needs.)
pub const RUNTIME: [&str; 5] = ["msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll", "vcomp140.dll"];

/// Only the server and the libraries it runs on, out of llama.cpp's many tools, with the C++ runtime beside it.
fn unpack_server(zip: &Path, folder: &Path) -> Result<(), String> {
    let needed = |name: &str| {
        let name = name.to_ascii_lowercase();
        name == "llama-server.exe" || name == "llama-server-impl.dll" || name == "llama-common.dll" || name == "llama.dll"
            || name == "mtmd.dll" || name == "libomp.dll" || (name.starts_with("ggml") && name.ends_with(".dll"))
    };
    let finish = |temporary: &Path| {
        with_runtime(temporary);
        Ok(())
    };
    unpack(&[zip.to_path_buf()], folder, &needed, ("llama-server.exe", "llama.cpp's package doesn't have its server"), &finish)
}

/// The C++ runtime beside Excavate's programs copied into `folder` where it isn't there (a PC that has it installed,
/// as one that builds Excavate does, finds it anyway).
pub fn with_runtime(folder: &Path) {
    let Some(ours) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) else { return };
    for name in RUNTIME {
        if ours.join(name).is_file() && !folder.join(name).is_file() {
            let _ = std::fs::copy(ours.join(name), folder.join(name));
        }
    }
}

// ---- Images by what they show --------------------------------------------------------------------------------------

/// Where our SigLIP 2 conversion is hosted on Hugging Face (account/repository).
macro_rules! siglip_repo {
    () => {
        "aynad/siglip2-base-excavate"
    };
}

/// EmbeddingGemma 2 (Balanced, and Thorough's first): ggml-org's Q8_0 and its image encoder. 865 MB.
pub const GEMMA_MODEL: &str = "embeddinggemma-2-Q8_0.gguf";
pub const GEMMA_VISION: &str = "mmproj-embeddinggemma-2-Q8_0.gguf";
const GEMMA_FILES: [File; 2] = [
    File {
        name: GEMMA_MODEL,
        size: 309_855_456,
        sha256: "2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135",
        url: Some("https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf"),
    },
    File {
        name: GEMMA_VISION,
        size: 554_821_056,
        sha256: "0531309472a6bc6f4b146c3736eee7795ec9db1903c25490d3c937490cef6dd8",
        url: Some("https://huggingface.co/ggml-org/embeddinggemma-2-GGUF/resolve/main/mmproj-embeddinggemma-2-Q8_0.gguf"),
    },
];

/// Qwen3-VL-Embedding-8B (Thorough's most accurate partner): mradermacher's Q4_K_M and Q8_0 image encoder. 5.4 GB.
pub const QWEN_MODEL: &str = "Qwen3-VL-Embedding-8B.Q4_K_M.gguf";
pub const QWEN_VISION: &str = "Qwen3-VL-Embedding-8B.mmproj-Q8_0.gguf";
const QWEN_FILES: [File; 2] = [
    File {
        name: QWEN_MODEL,
        size: 4_677_725_440,
        sha256: "a81ec5d9211f03a51369ab1e858f750eabc84a41bec19e129f3d4a0659faf1c7",
        url: Some("https://huggingface.co/mradermacher/Qwen3-VL-Embedding-8B-GGUF/resolve/main/Qwen3-VL-Embedding-8B.Q4_K_M.gguf"),
    },
    File {
        name: QWEN_VISION,
        size: 752_290_208,
        sha256: "36c44edfbe487f743128e369e53966f2e51e1641e604064f68a443c864b5bb9c",
        url: Some("https://huggingface.co/mradermacher/Qwen3-VL-Embedding-8B-GGUF/resolve/main/Qwen3-VL-Embedding-8B.mmproj-Q8_0.gguf"),
    },
];

/// SigLIP 2 base (Quick, and Thorough's lighter partner): our conversion of Immich's ONNX export of the open_clip weights
/// (the image half's weights stored at 16 bits, the text half's token table at 8; 726 MB
/// instead of 1.5 GB, the same results), hosted on Hugging Face with its model card and license.
pub const SIGLIP_VISION: &str = "visual.onnx";
pub const SIGLIP_TEXT: &str = "textual.onnx";
pub const SIGLIP_TOKENIZER: &str = "tokenizer.json";
const SIGLIP_FILES: [File; 3] = [
    File {
        name: SIGLIP_VISION,
        size: 186_039_306,
        sha256: "75c7f1aa9bb8941e77e8451254312b1c816a62bed3c8b2d54ed7b5b4a0251427",
        url: Some(concat!("https://huggingface.co/", siglip_repo!(), "/resolve/main/visual/model.onnx")),
    },
    File {
        name: SIGLIP_TEXT,
        size: 539_694_384,
        sha256: "e5d6a330df692bd28eae0286d2bfe8c298248dfca7998c929344ddbcfd5cfea8",
        url: Some(concat!("https://huggingface.co/", siglip_repo!(), "/resolve/main/textual/model.onnx")),
    },
    File {
        name: SIGLIP_TOKENIZER,
        size: 34_362_885,
        sha256: "220c63d496e0c14e63eb656c91e0215e926202e4c74b1f089e09f1920d779b04",
        url: Some(concat!("https://huggingface.co/", siglip_repo!(), "/resolve/main/textual/tokenizer.json")),
    },
];

/// Where a model that looks at images keeps its files.
pub fn seeing_folder(models: &Path, engine: seeing::Engine) -> PathBuf {
    models.join(engine.model().name())
}

fn seeing_files(engine: seeing::Engine) -> &'static [File] {
    match engine {
        seeing::Engine::Gemma | seeing::Engine::GemmaText => &GEMMA_FILES,
        seeing::Engine::Qwen => &QWEN_FILES,
        seeing::Engine::Siglip => &SIGLIP_FILES,
    }
}

/// Whether a model's files are all there (and llama.cpp's server, for the ones it runs).
pub fn seeing_present(models: &Path, engine: seeing::Engine) -> bool {
    all_there(&seeing_folder(models, engine), seeing_files(engine))
        && (!engine.on_gpu() || llama_folder(models).join("llama-server.exe").is_file())
}

/// What a model still has to download, in bytes (llama.cpp's server included when it isn't there).
pub fn seeing_download_size(models: &Path, engine: seeing::Engine) -> u64 {
    let folder = seeing_folder(models, engine);
    let files: u64 = seeing_files(engine).iter()
        .filter(|file| !std::fs::metadata(folder.join(file.name)).is_ok_and(|metadata| metadata.len() == file.size))
        .map(|file| file.size)
        .sum();
    let server = if engine.on_gpu() && !llama_folder(models).join("llama-server.exe").is_file() { LLAMA.size } else { 0 };
    files + server
}

/// Downloads what a model needs. `progress(received, total)` over all of it.
pub fn seeing_ensure(models: &Path, engine: seeing::Engine, control: &'static Control, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let models = models.to_path_buf();
    in_background(&format!("seeing {}", engine.name()), control, progress, move |cancelled, progress| {
        let total = seeing_download_size(&models, engine);
        let mut before = 0;
        if engine.on_gpu() && !llama_folder(&models).join("llama-server.exe").is_file() {
            ensure_llama(&models, cancelled, &mut |received| progress(received, total))?;
            before += LLAMA.size;
        }
        // Only what's missing counts toward the total, as `seeing_download_size` has it.
        let folder = seeing_folder(&models, engine);
        let had: u64 = seeing_files(engine).iter()
            .filter(|file| std::fs::metadata(folder.join(file.name)).is_ok_and(|metadata| metadata.len() == file.size))
            .map(|file| file.size)
            .sum();
        fetch(&folder, seeing_files(engine), cancelled, &mut |received, _| progress(before + received.saturating_sub(had), total))
    })
}

// ---- Recordings by what's said in them ------------------------------------------------------------------------------

/// Where Excavate's listener (tools/listen) is hosted on Hugging Face (account/repository).
macro_rules! listen_repo {
    () => {
        "aynad/excavate-listen"
    };
}

/// The listener on whisper.cpp b5454, build 3 (Silero's voice detector first, for both models; version details in every
/// file): every processor variant and Vulkan (any graphics card). 18 MB.
const LISTEN: File = File {
    name: "excavate-listen-b5454-3-vulkan.zip",
    size: 18_137_623,
    sha256: "9483999b518957eab9f2a793f1656986a260b74a3c6f45c3548e4f50375936bb",
    url: Some(concat!("https://huggingface.co/", listen_repo!(), "/resolve/main/excavate-listen-b5454-3-vulkan.zip")),
};

/// The listener's build, as its package notes it (`BUILD_NOTE`; 1 before there was a note). One unpacked before is
/// brought up to date (`listen_upgrade`).
const LISTEN_BUILD: u32 = 3;
const BUILD_NOTE: &str = "excavate-listen-build.txt";

fn listen_build(folder: &Path) -> u32 {
    std::fs::read_to_string(folder.join(BUILD_NOTE)).ok().and_then(|text| text.trim().parse().ok()).unwrap_or(1)
}

/// A listener unpacked before this build: this build's package in its place, what was beside it kept (CUDA's add-on,
/// so it isn't downloaded again). Ok(false): nothing to do. Its process mustn't be running.
pub fn listen_upgrade(models: &Path, cuda: bool, control: &Control) -> Result<bool, String> {
    let unpacked = listen_folder(models, cuda);
    if !unpacked.join("excavate-listen.exe").is_file() || listen_build(&unpacked) >= LISTEN_BUILD {
        return Ok(false);
    }
    let downloads = models.join("listen");
    fetch(&downloads, &[LISTEN], &|| control.stopping(), &mut |_, _| {})?;
    let zip = downloads.join(LISTEN.name);
    let result = unpack_all(std::slice::from_ref(&zip), &unpacked);
    let _ = std::fs::remove_file(zip);
    result.map(|()| true)
}

/// What the graphics card runs them with, when it's NVIDIA's and CUDA is chosen: its library (Turing to Blackwell),
/// NVIDIA's CUDA 12.9 runtime and cuBLAS. 660 MB.
const LISTEN_CUDA: Option<File> = Some(File {
    name: "excavate-listen-b5454-cuda12.zip",
    size: 661_525_880,
    sha256: "be19ebc8ad60809199bc717132458744e38e14236eb40d519cb90dec7e76dea5",
    url: Some(concat!("https://huggingface.co/", listen_repo!(), "/resolve/main/excavate-listen-b5454-cuda12.zip")),
});

/// The models, as ggml-org publishes them for whisper.cpp.
pub const PARAKEET: File = File {
    name: "ggml-parakeet-tdt-0.6b-v3-q8_0.bin",
    size: 668_757_119,
    sha256: "4d64e9e96c2792186d072fde0034df0ad670cf680a2f53069052ead827fd600e",
    url: Some("https://huggingface.co/ggml-org/parakeet-GGUF/resolve/main/ggml-parakeet-tdt-0.6b-v3-q8_0.bin"),
};
pub const WHISPER_TURBO: File = File {
    name: "ggml-large-v3-turbo-q8_0.bin",
    size: 874_188_075,
    sha256: "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1",
    url: Some("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin"),
};
pub const WHISPER_LARGE: File = File {
    name: "ggml-large-v3-q5_0.bin",
    size: 1_081_140_203,
    sha256: "d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1",
    url: Some("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin"),
};
/// Silero's voice detector, so Whisper only hears speech (silence and music make it invent words). 0.9 MB.
pub const SILERO: File = File {
    name: "ggml-silero-v6.2.0.bin",
    size: 885_098,
    sha256: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
    url: Some("https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin"),
};

pub fn speech_folder(models: &Path) -> PathBuf {
    models.join("speech")
}

/// Where the listener is, unpacked: one folder per build, and one with CUDA's add-on beside it.
pub fn listen_folder(models: &Path, cuda: bool) -> PathBuf {
    models.join("listen").join(if cuda { "b5454-cuda" } else { "b5454" })
}

/// Whether CUDA can be had for listening (its add-on is published; `config::LISTEN_CUDA_PUBLISHED` says so to settings).
pub fn listen_cuda_offered() -> bool {
    LISTEN_CUDA.is_some() && config::LISTEN_CUDA_PUBLISHED
}

/// The model and Silero's voice detector, which both listeners use first (silence and music then cost nothing).
fn speech_files(model: &File) -> Vec<&File> {
    vec![model, &SILERO]
}

pub fn speech_present(models: &Path, model: &File, cuda: bool) -> bool {
    speech_files(model).iter().all(|file| std::fs::metadata(speech_folder(models).join(file.name)).is_ok_and(|metadata| metadata.len() == file.size))
        && listen_folder(models, cuda).join("excavate-listen.exe").is_file()
}

/// What listening still has to download, in bytes.
pub fn speech_download_size(models: &Path, model: &File, cuda: bool) -> u64 {
    let folder = speech_folder(models);
    let files: u64 = speech_files(model).iter()
        .filter(|file| !std::fs::metadata(folder.join(file.name)).is_ok_and(|metadata| metadata.len() == file.size))
        .map(|file| file.size)
        .sum();
    let listener = if listen_folder(models, cuda).join("excavate-listen.exe").is_file() {
        0
    } else {
        LISTEN.size + if cuda { LISTEN_CUDA.as_ref().map_or(0, |file| file.size) } else { 0 }
    };
    files + listener
}

/// Downloads and unpacks what listening needs. `progress(received, total)` over all of it.
pub fn speech_ensure(models: &Path, model: &'static File, cuda: bool, control: &'static Control, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let models = models.to_path_buf();
    in_background(&format!("listening {} {cuda}", model.name), control, progress, move |cancelled, progress| {
        let total = speech_download_size(&models, model, cuda);
        let mut before = 0;
        let unpacked = listen_folder(&models, cuda);
        if !unpacked.join("excavate-listen.exe").is_file() {
            let downloads = models.join("listen");
            let mut packages = vec![LISTEN];
            if cuda {
                packages.push(LISTEN_CUDA.ok_or("CUDA's add-on isn't published")?);
            }
            let size: u64 = packages.iter().map(|file| file.size).sum();
            fetch(&downloads, &packages, cancelled, &mut |received, _| progress(received, total))?;
            let zips: Vec<PathBuf> = packages.iter().map(|file| downloads.join(file.name)).collect();
            unpack_all(&zips, &unpacked)?;
            for zip in zips {
                let _ = std::fs::remove_file(zip);
            }
            before += size;
        }
        let folder = speech_folder(&models);
        let files = speech_files(model);
        let had: u64 = files.iter()
            .filter(|file| std::fs::metadata(folder.join(file.name)).is_ok_and(|metadata| metadata.len() == file.size))
            .map(|file| file.size)
            .sum();
        let files: Vec<File> = files.into_iter().map(|file| File { ..*file }).collect();
        fetch(&folder, &files, cancelled, &mut |received, _| progress(before + received.saturating_sub(had), total))
    })
}

/// Every file of the packages, side by side in `folder` (a later package's file wins).
fn unpack_all(zips: &[PathBuf], folder: &Path) -> Result<(), String> {
    // What the folder had that these packages don't bring stays (an earlier build's CUDA add-on).
    let keep_earlier = |temporary: &Path| {
        if let Ok(earlier) = std::fs::read_dir(folder) {
            for entry in earlier.flatten() {
                let kept = temporary.join(entry.file_name());
                if entry.path().is_file() && !kept.exists() {
                    std::fs::copy(entry.path(), kept).map_err(|error| error.to_string())?;
                }
            }
        }
        Ok(())
    };
    unpack(zips, folder, &|_| true, ("excavate-listen.exe", "the listener's package doesn't have it"), &keep_earlier)
}

/// The packages' files that `wanted` names, side by side (a later package's file wins, folders inside them ignored),
/// unpacked beside `folder` first and put in its place only once whole. `program` (the file that must be there, the
/// error when it isn't); `finish` has the unpacked folder before it takes `folder`'s place.
fn unpack(zips: &[PathBuf], folder: &Path, wanted: &dyn Fn(&str) -> bool, program: (&str, &str), finish: &dyn Fn(&Path) -> Result<(), String>) -> Result<(), String> {
    let temporary = folder.with_extension("unpacking");
    let _ = std::fs::remove_dir_all(&temporary);
    std::fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
    for zip in zips {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(zip).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
            let Some(name) = entry.enclosed_name().and_then(|path| path.file_name().map(|name| name.to_string_lossy().to_string())) else { continue };
            if !wanted(&name) {
                continue;
            }
            let mut out = std::fs::File::create(temporary.join(&name)).map_err(|error| error.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|error| error.to_string())?;
        }
    }
    if !temporary.join(program.0).is_file() {
        return Err(program.1.into());
    }
    finish(&temporary)?;
    let _ = std::fs::remove_dir_all(folder);
    std::fs::rename(&temporary, folder).map_err(|error| error.to_string())
}

// ---- Downloading ---------------------------------------------------------------------------------------------------

/// A download on a thread of its own.
struct Job {
    /// (received, total), as it goes.
    progress: Mutex<(u64, u64)>,
    /// How it went, once it's over.
    result: Mutex<Option<Result<(), String>>>,
    /// When a lane last waited for it.
    wanted: Mutex<Instant>,
}

/// Downloads under way, by what they're for.
static JOBS: LazyLock<Mutex<HashMap<String, Arc<Job>>>> = LazyLock::new(Mutex::default);

/// A download nobody has waited for this long stops (its part is kept): the choice changed, and no lane asks for it
/// any more. Reading starting over takes a few seconds before the new session's lane asks again; longer while a large
/// folder is walked, and then it stops and later continues from its part, as it always did.
const UNWANTED: Duration = Duration::from_secs(90);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs `work` (a download) on a thread of its own, or takes up the one already running for `key`, and waits for it,
/// telling `progress`. Reading stopping to start over (the settings changed, "Full speed", a pause) ends the wait, not
/// the download: the next session's lane takes it up where it is. Before, it was cancelled, its progress went back to
/// nothing, and the next session hashed its part again before going on. Quitting stops it.
fn in_background(key: &str, control: &'static Control, progress: &mut dyn FnMut(u64, u64),
                 work: impl FnOnce(&dyn Fn() -> bool, &mut dyn FnMut(u64, u64)) -> Result<(), String> + Send + 'static) -> Result<(), String> {
    in_background_with(key, control, UNWANTED, progress, work)
}

fn in_background_with(key: &str, control: &'static Control, unwanted: Duration, progress: &mut dyn FnMut(u64, u64),
                      work: impl FnOnce(&dyn Fn() -> bool, &mut dyn FnMut(u64, u64)) -> Result<(), String> + Send + 'static) -> Result<(), String> {
    let job = {
        let mut jobs = lock(&JOBS);
        match jobs.get(key) {
            Some(job) => job.clone(),
            None => {
                let job = Arc::new(Job { progress: Mutex::new((0, 0)), result: Mutex::new(None), wanted: Mutex::new(Instant::now()) });
                let (running, key_owned) = (job.clone(), key.to_string());
                let spawned = std::thread::Builder::new().name("download".into()).spawn(move || {
                    let cancelled = || control.quitting() || lock(&running.wanted).elapsed() > unwanted;
                    let result = work(&cancelled, &mut |received, total| *lock(&running.progress) = (received, total));
                    *lock(&running.result) = Some(result);
                    let mut jobs = lock(&JOBS);
                    if jobs.get(&key_owned).is_some_and(|job| Arc::ptr_eq(job, &running)) {
                        jobs.remove(&key_owned);
                    }
                });
                if let Err(error) = spawned {
                    return Err(error.to_string());
                }
                jobs.insert(key.to_string(), job.clone());
                job
            }
        }
    };
    loop {
        *lock(&job.wanted) = Instant::now();
        let (received, total) = *lock(&job.progress);
        if total > 0 {
            progress(received, total);
        }
        if let Some(result) = lock(&job.result).clone() {
            return result;
        }
        if !control.sleep(Duration::from_millis(250)) {
            return Err("stopping".into());
        }
    }
}

fn fetch(folder: &Path, files: &[File], cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let total: u64 = files.iter().map(|file| file.size).sum();
    let mut done = 0u64;
    for file in files {
        let target = folder.join(file.name);
        if std::fs::metadata(&target).is_ok_and(|metadata| metadata.len() == file.size) {
            done += file.size;
            progress(done, total);
            continue;
        }
        let url = file.url.map(str::to_string).unwrap_or_else(|| format!("{BASE}{}", file.name));
        config::log(&format!("downloading {url}"));
        let before = done;
        download(file, &url, &target, cancelled, &mut |received| progress(before + received, total))
            .map_err(|error| format!("{}: {error}", file.name))?;
        done = before + file.size;
        progress(done, total);
    }
    Ok(())
}

/// One file, continuing a part that's already there.
fn download(file: &File, url: &str, target: &Path, cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64)) -> Result<(), String> {
    let partial = target.with_extension(format!("{}.part", target.extension().and_then(|extension| extension.to_str()).unwrap_or("")));
    let mut hasher = Sha256::new();
    // What's there already goes into the hash; too long (or unreadable), it's not worth keeping. Stopping while it's
    // hashed keeps it: the next start continues it (a 4.7 GB model isn't fetched again).
    let had = std::fs::metadata(&partial).map(|metadata| metadata.len()).unwrap_or(0);
    let kept = had > 0 && had < file.size && match hash_into(&partial, &mut hasher, cancelled) {
        Ok(()) => true,
        Err(_) if cancelled() => return Err("stopping".into()),
        Err(_) => false,
    };
    let have = if kept {
        had
    } else {
        let _ = std::fs::remove_file(&partial);
        hasher = Sha256::new();
        0
    };
    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&partial).map_err(|error| error.to_string())?;
    let mut writer = HashingWriter { file: &mut out, hasher: &mut hasher };
    let mut report = |received: u64, _total: u64| progress(received);
    let result = winsys::http::download_from(url, have, &mut writer, &mut report, cancelled);
    match result {
        Err(error) if error == winsys::http::CANT_RESUME => {
            writer.file.set_len(0).map_err(|error| error.to_string())?;
            *writer.hasher = Sha256::new();
            winsys::http::download_from(url, 0, &mut writer, &mut report, cancelled)?;
        }
        other => {
            other?;
        }
    }
    drop(out);
    let digest: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    if digest != file.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err("it didn't download correctly, its checksum differs".into());
    }
    std::fs::rename(&partial, target).map_err(|error| error.to_string())
}

fn hash_into(path: &Path, hasher: &mut Sha256, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        if cancelled() {
            return Err("stopping".into());
        }
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

struct HashingWriter<'a> {
    file: &'a mut std::fs::File,
    hasher: &'a mut Sha256,
}

impl Write for HashingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(bytes);
        self.file.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn control() -> &'static Control {
        Box::leak(Box::default())
    }

    /// Reading starting over (a settings change, "Full speed") stops a lane's wait, not its download: the next session's
    /// lane takes up the same download, which isn't started a second time and doesn't go back to nothing.
    #[test]
    fn a_download_goes_on_through_a_restart() {
        let control = control();
        let started = Arc::new(AtomicUsize::new(0));
        let finish = Arc::new(AtomicBool::new(false));
        let work = |started: Arc<AtomicUsize>, finish: Arc<AtomicBool>| move |cancelled: &dyn Fn() -> bool, progress: &mut dyn FnMut(u64, u64)| {
            started.fetch_add(1, Ordering::SeqCst);
            let mut received = 0;
            while !finish.load(Ordering::SeqCst) {
                if cancelled() {
                    return Err("cancelled".to_string());
                }
                received += 1;
                progress(received, 1000);
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        };
        // The settings change while the first session waits.
        let waiter = std::thread::spawn({
            let (started, finish) = (started.clone(), finish.clone());
            move || in_background("test restart", control, &mut |_, _| {}, work(started, finish))
        });
        std::thread::sleep(Duration::from_millis(100));
        control.restart();
        assert_eq!(waiter.join().unwrap(), Err("stopping".to_string()));
        // The next session: the same download, further along than nothing, and not started again.
        control.restarted();
        let seen = Arc::new(Mutex::new(0u64));
        let finisher = {
            let finish = finish.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                finish.store(true, Ordering::SeqCst);
            })
        };
        let first_seen = seen.clone();
        let result = in_background("test restart", control, &mut |received, _| {
            let mut first = lock(&first_seen);
            if *first == 0 {
                *first = received;
            }
        }, work(started.clone(), finish.clone()));
        finisher.join().unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(started.load(Ordering::SeqCst), 1, "started once");
        assert!(*lock(&seen) > 1, "taken up where it was");
    }

    /// Nobody waiting for it any more (the choice changed): it stops, and the next ask starts it afresh.
    #[test]
    fn a_download_nobody_wants_stops() {
        let control = control();
        let cancelled_seen = Arc::new(AtomicBool::new(false));
        let waiter = std::thread::spawn({
            let cancelled_seen = cancelled_seen.clone();
            move || in_background_with("test unwanted", control, Duration::from_millis(200), &mut |_, _| {}, move |cancelled, _| {
                while !cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
                cancelled_seen.store(true, Ordering::SeqCst);
                Err("cancelled".to_string())
            })
        });
        std::thread::sleep(Duration::from_millis(50));
        control.restart();
        assert_eq!(waiter.join().unwrap(), Err("stopping".to_string()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cancelled_seen.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cancelled_seen.load(Ordering::SeqCst));
        std::thread::sleep(Duration::from_millis(50));
        control.restarted();
        assert_eq!(in_background_with("test unwanted", control, Duration::from_secs(5), &mut |_, _| {}, |_, _| Ok(())), Ok(()));
    }

    /// A package with these files (folders inside it ignored by the unpacking), in `folder`.
    fn package(folder: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let path = folder.join(name);
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, text) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("excavate-unpack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// llama.cpp's package: only the server and its libraries, its folders flattened; one without the server isn't
    /// unpacked, and leaves nothing behind.
    #[test]
    fn unpacks_only_the_server() {
        let home = scratch("server");
        let zip = package(&home, "llama.zip", &[("bin/llama-server.exe", "server"), ("bin/ggml-vulkan.dll", "ggml"), ("bin/llama-cli.exe", "cli")]);
        let folder = home.join("llama").join("build");
        unpack_server(&zip, &folder).unwrap();
        assert_eq!(std::fs::read_to_string(folder.join("llama-server.exe")).unwrap(), "server");
        assert!(folder.join("ggml-vulkan.dll").is_file());
        assert!(!folder.join("llama-cli.exe").exists());
        assert!(!folder.with_extension("unpacking").exists());
        let without = package(&home, "without.zip", &[("bin/llama-cli.exe", "cli")]);
        let other = home.join("other");
        assert_eq!(unpack_server(&without, &other), Err("llama.cpp's package doesn't have its server".into()));
        assert!(!other.exists());
        let _ = std::fs::remove_dir_all(home);
    }

    /// The listener's packages: every file, a later package's winning, and what the folder had before kept.
    #[test]
    fn unpacks_every_package_keeping_what_was_there() {
        let home = scratch("listen");
        let folder = home.join("listen");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("ggml-cuda.dll"), "earlier").unwrap();
        std::fs::write(folder.join("excavate-listen.exe"), "earlier").unwrap();
        let first = package(&home, "first.zip", &[("excavate-listen.exe", "new"), ("whisper.dll", "first")]);
        let second = package(&home, "second.zip", &[("whisper.dll", "second")]);
        unpack_all(&[first, second], &folder).unwrap();
        let read = |name: &str| std::fs::read_to_string(folder.join(name)).unwrap();
        assert_eq!((read("excavate-listen.exe"), read("whisper.dll"), read("ggml-cuda.dll")), ("new".into(), "second".into(), "earlier".into()));
        let empty = package(&home, "empty.zip", &[("readme.txt", "")]);
        assert_eq!(unpack_all(&[empty], &folder), Err("the listener's package doesn't have it".into()));
        assert_eq!(read("excavate-listen.exe"), "new");
        let _ = std::fs::remove_dir_all(home);
    }
}
