# Models

Every model runs on your PC, in the background, at idle priority, and only while there's something for it: heavy ones
wait while you're using the processor or the graphics card, or a game or a full-screen app runs. Each downloads the
first time it's chosen and is checked against its published SHA-256. How fast and how accurate each one is on a test
machine: [benchmarks](../benchmarks/README.md).

Everything that uses a graphics card uses one: the one with the most memory of its own (on a laptop, the NVIDIA or AMD
card rather than the processor's graphics), or the one chosen in Settings → General → Graphics card, where "None"
keeps it all on the processor. Videos are decoded on it too.

## Text in images

| | Quick | Balanced | Thorough |
|---|---|---|---|
| Model | PP-OCRv6 tiny | PP-OCRv6 small | PP-OCRv6 small, then PaddleOCR-VL-1.5 |
| Size | 6 MB | 31 MB | 1.7 GB (with llama.cpp) |
| Runs on | the processor | the processor, or the graphics card if you choose it (faster, but a few GB of memory) | the processor, then the graphics card (4 GB of its memory) |
| Reads | Latin-script languages and Chinese | the same, better with small text and photos of documents | 111 languages, including Arabic, Russian and Hindi; tables and layouts |

PP-OCRv6 ([PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR), Apache 2.0) runs in ONNX Runtime. With Thorough, an
image is read by PP-OCRv6 first, so it's searchable at once and its secrets can be placed and blurred; PaddleOCR-VL-1.5
then reads the images that have text again, and its text takes the first read's place. It runs through
[llama.cpp](https://github.com/ggml-org/llama.cpp) (MIT), downloaded into `%LOCALAPPDATA%\Excavate` without
administrator rights, started only while there are images for it.

## Images and videos by what they show

These let you find an image by describing it ("a dog on a beach"). Videos are looked at a frame every few seconds, so
a video opens at the moment that fits. With Balanced or Thorough, documents are also found by what they mean
("kitchen renovation costs" finds a quote that never uses those words).

| | Quick | Balanced | Thorough |
|---|---|---|---|
| Model | SigLIP 2 base | EmbeddingGemma 2 | EmbeddingGemma 2, plus SigLIP 2 or Qwen3-VL-Embedding-8B |
| Made by | Google | Google | Google; Alibaba's Qwen team |
| Size | 725 MB | 825 MB | 1.5 GB or 5.9 GB |
| Runs on | the processor | the graphics card | the graphics card (Qwen's needs 8 GB of its memory) |
| Languages | English and European languages | nearly every language | nearly every language |

Qwen3-VL-Embedding-8B is tried on your first 10 images; if it doesn't fit your graphics card or is too slow, Thorough
uses SigLIP 2 instead and says so.

Where and when a photo was taken comes from its own metadata, not a model: its GPS position is named after the nearest
town of 15,000 people or more, from [GeoNames](https://www.geonames.org) (CC BY 4.0), built in. So "Prague",
"last winter" or "July 2023" work, alone or with a description.

## What's said in recordings and videos

All three run through [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT), in a small program of Excavate's
own ([tools/listen](../tools/listen)):

| | Quick | Balanced | Thorough |
|---|---|---|---|
| Model | [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) (NVIDIA, CC BY 4.0) | [Whisper large-v3-turbo](https://huggingface.co/openai/whisper-large-v3-turbo) (OpenAI, MIT) | [Whisper large-v3](https://huggingface.co/openai/whisper-large-v3) (OpenAI, MIT) |
| Runs on | the processor | the graphics card, or the processor | the graphics card |
| Languages | English and 24 European languages | nearly every language | nearly every language, a little more accurate |

Whisper is told your languages (from Windows' settings) and hears only speech: [Silero's voice
detector](https://github.com/snakers4/silero-vad) (MIT) skips silence and music first. What's said is kept with when it
was said, so a result opens at that moment. The graphics card is used through Vulkan, which every graphics driver
brings; CUDA can be chosen on NVIDIA cards.
