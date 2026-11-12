---
license: other
license_name: mit-with-redistributables
license_link: https://github.com/ggml-org/whisper.cpp/blob/d1be6fd/LICENSE
library_name: whisper.cpp
pipeline_tag: automatic-speech-recognition
tags:
  - whisper
  - parakeet
  - vulkan
  - cuda
  - windows
---

# excavate-listen

The speech-to-text runner for Excavate, a local search app for Windows. Excavate downloads it, then uses it to make
recordings searchable by what's said in them.

## Why it exists

- whisper.cpp publishes no Windows build for Vulkan, and its Windows CUDA build leaves out cuBLAS, so it runs on the
  processor.
- Excavate needs one small program that runs Whisper and Parakeet on any graphics card or on the processor, keeps a
  model loaded, and takes sound over a pipe.

These files are that program, built from whisper.cpp's source. They contain no model weights. The models come from
ggml-org's own repositories: `ggerganov/whisper.cpp`, `ggml-org/parakeet-GGUF` and `ggml-org/whisper-vad`.

## Files

| File | What's in it |
|---|---|
| `excavate-listen-b5454-3-vulkan.zip` | Build 3: `excavate-listen.exe` (Silero's voice detector first, for Whisper and Parakeet), whisper.cpp's libraries, every processor variant, Vulkan, Microsoft's C++ runtime (18 MB). Every program and library carries version details (company, product, description, version 1.5454.3.0, license) || `excavate-listen-b5454-cuda12.zip` | Optional, for NVIDIA cards: whisper.cpp's CUDA library, NVIDIA's CUDA 12.9 runtime and cuBLAS (660 MB) |

Excavate checks each file's SHA-256 before using it.

## Built from

- whisper.cpp tag `b5454` (commit `d1be6fd`), with its backends as loadable libraries (`GGML_BACKEND_DL`).
- The listener's source and its build script are `tools/listen/listen.cpp` and `tools/listen/build.bat` in Excavate's
  repository.
- CUDA's library is built for Turing to Blackwell (GTX 16 and RTX 20 onwards).

## Licenses

- whisper.cpp and ggml: MIT (`LICENSE-whisper.cpp.txt`).
- The listener: MIT (`LICENSE-excavate-listen.txt`).
- NVIDIA's runtime and cuBLAS are redistributed under NVIDIA's CUDA Toolkit EULA (`EULA-NVIDIA-CUDA.txt`).
- Microsoft's C++ runtime is redistributed under Visual Studio's terms for app-local use.
