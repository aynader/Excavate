<p align="center"><img src="brand/mark-128.png" width="96" alt=""></p>

<h1 align="center">Excavate</h1>

<p align="center"><b>Search, the way it should be.</b><br>
Press <kbd>Alt</kbd>+<kbd>S</kbd> and search for apps and files on every drive by name, the words inside your documents,
the text in your images, what your photos and videos show, and what's said in your recordings. Within milliseconds.</p>



https://github.com/user-attachments/assets/4be85b45-9b3c-4896-8222-ebeabf878860



---

Excavate is a search popup for Windows 11 that opens over the taskbar from one shortcut. Each keystroke is answered in
a few milliseconds from the places you use, then filled in with names from every drive and with what's inside your
files, images, videos and recordings, all ranked as one list. Everything runs on *your* PC, and nothing of yours leaves
it.

- **Names on every drive**, from Bedrock, Excavate's own index (built from each drive's own list of files when you allow
  it once, or folder by folder, then kept up to date as files change), or from
  [Everything](https://www.voidtools.com) if you'd rather keep it.
- **Inside files**: documents, PDFs, spreadsheets, slides, email and code.
- **Text in images**: screenshots, scans and photos, read with OCR on the processor, or on the graphics card for nearly
  every language (Thorough).
- **Images by what they show**, and documents by what they mean: type what you remember (e.g. "a dog on a beach").
- **Videos**: the text they show (slides, screen recordings, captions), what they show, and what's said in them, each
  opened at its moment.
- **Recordings** by what's said in them.

A background reader does the work at the pace you choose, waits while you need the PC, and stops on battery unless you
allow it. Passwords, card numbers and IDs it finds are blurred in previews and never stored, and previews hide
themselves while you present ([privacy](docs/PRIVACY.md)).

## Install

Windows 11, 64-bit. Download the latest zip from [Releases](../../releases), unzip it anywhere, and run
`excavate.exe`. The first time, a few short steps set it up: the shortcut, how names are found (Bedrock, or Everything,
installed in one click if you want it), which folders to read, and how closely to read images, recordings and videos
(Quick, Balanced or Thorough, with how long each takes on your PC). The models download as they're chosen
([models](docs/MODELS.md)).

### System requirements

Excavate is built and tested for Windows 11. Recent Windows 10 builds will likely run it, but aren't tested, and some
parts (the popup's place by the taskbar, its look) may need adjusting.

| | Minimum | Recommended |
|---|---|---|
| Windows | Windows 11, 64-bit (x64) | Windows 11, 64-bit, up to date |
| Processor | 4 cores | 8 cores or more |
| Memory | 8 GB | 16 GB |
| Graphics card | none: names, documents, and Quick or Balanced reading run on the processor | 8 GB of its own memory and a current driver, such as an NVIDIA GeForce RTX 3060, AMD Radeon RX 6600 or Intel Arc A750, or newer |
| Disk | 1 GB, plus the index (about 40 bytes per file and folder) | 15 GB, for every model |

Thorough's reading and the Balanced and Thorough image search run on the graphics card; Thorough needs at least 4 GB of
its memory, and its most accurate image model 8 GB.

Settings live in `%APPDATA%\Excavate\settings.json`; the index, models and logs in `%LOCALAPPDATA%\Excavate`. Deleting
those two folders removes everything Excavate has kept.

## Use it

| Key | |
|---|---|
| <kbd>Alt</kbd>+<kbd>S</kbd> | Open or close Excavate (Settings → General to change it) |
| type | Names, parts of names, initials (`vsc` finds Visual Studio Code), words inside files, or Everything syntax (`ext:pdf dm:thisweek`) |
| <kbd>Ctrl</kbd>+<kbd>I</kbd>, <kbd>B</kbd>, <kbd>U</kbd>, <kbd>R</kbd> | Match case, whole words, the full path, a regular expression, as in Everything (also the toggles in the search field) |
| <kbd>↑</kbd> <kbd>↓</kbd> <kbd>PgUp</kbd> <kbd>PgDn</kbd> <kbd>Ctrl</kbd>+<kbd>Home</kbd>/<kbd>End</kbd> | Move through results |
| <kbd>Enter</kbd> | Open |
| <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Show in its folder |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Enter</kbd> | Run as administrator |
| <kbd>Alt</kbd>+<kbd>Enter</kbd> | Properties |
| <kbd>Ctrl</kbd>+<kbd>O</kbd> | Open with… |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Copy the file / copy its path |
| <kbd>Tab</kbd>, <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>9</kbd> | Filters: All, Apps, Documents, Images, Code, Audio, Video, Folders, Archives |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | Preview pane |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings |
| <kbd>Ctrl</kbd>+<kbd>Space</kbd> | Enlarge the selected image (<kbd>↑</kbd> <kbd>↓</kbd> next image, the wheel to zoom, <kbd>Esc</kbd> back) |
| <kbd>Esc</kbd> | Close |

A result found by its contents says where ("In text", "In image", "In video · 1:24", "Said · 3:05") and shows the
words that matched; the preview shows the part of the file around them, and a video or recording opens at that moment.
Right-click a result for more.

The menu at the field's right end orders the results: **Best match**, **Fuzzy match** (for a name half remembered:
`mikro11` finds `Micro11.iso`), or by name, date, size, path or when you last opened them.

Beside Everything's syntax, Excavate answers `content:` (the words inside), `width:`, `height:`, `dimensions:`,
`length:` (`length:>10:00`), `runcount:` and `daterun:` (what you opened from Excavate); with Bedrock, also `dc:`,
`da:` and `attrib:`. An experimental switch in Settings → General lets a search start with `>` for the words inside
files or `<` for what they show or mean (`>v` videos, `>a` recordings, `>pdf` PDFs).

**Update automatically** (Settings → About) is off until you turn it on. Then Excavate checks this page's releases
once a day, verifies a new version's signature, and uses it at the next start ([releasing](docs/RELEASING.md)).

## Build

Windows 11, Rust 1.95+ (`rustup update`), Visual Studio 2022's C++ tools. Everything is optional.

```bash
scripts/fetch-pdfium.sh          # once: PDFium for reading PDFs (the build copies it beside the exe)
scripts/fetch-onnxruntime.sh     # once: ONNX Runtime and DirectML, from Microsoft (likewise)
cargo build --release            # target/release/excavate.exe, excavate-indexer.exe and excavate-helper.exe
cargo test --workspace
target/release/excavate.exe --bench --bench-out bench.json   # measures the popup on this PC
```

`EXCAVATE_HOME=<folder>` runs a separate copy with all its settings and data under one folder (it never touches
Start with Windows); `excavate-indexer.exe --quit` stops the reader.

## Layout

```
app/          excavate.exe: the popup, the settings and first-run window (ui/*.slint)
indexer/      excavate-indexer.exe (the reader) and excavate-helper.exe (searches by what images show)
crates/
  config/     settings, paths, exclusions, the reader's status
  content/    the content index (tantivy): schema, tokenizer, writer, reader, snippets
  engine/     Bedrock: the file-name index, the NTFS tables, the change journal, Everything's syntax, fuzzy search
  everything/ Everything's SDKs, compiled in
  extract/    readers: text and code, Office and OpenDocument, PDF, EPUB, HTML, RTF, email
  mark/       the animated mark
  privacy/    finding secrets: cards, IBANs, IDs, passports, keys, passwords, recovery phrases
  search/     filters, query parsing, ranking, query plans, history
  seeing/     images and videos by what they show; when and where photos were taken
  sweep/      the first pass over a large photo library: which images to read first
  winsys/     Windows: shell, icons, imaging, HTTP, folder changes, processes, tray, hotkey, Windows Hello
tools/
  listen/     excavate-listen.exe: whisper.cpp's speech-to-text, built for Excavate
  places/     makes the table of places photos are named after, from GeoNames
vendor/       Slint's winit backend, patched (needed to build)
brand/        the icons
docs/         architecture, privacy, models, Everything, releasing
benchmarks/   how Excavate compares, with the data to check it
scripts/      fetching PDFium, signing and packing releases
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the pieces fit.

## Credits

- [Everything](https://www.voidtools.com) by voidtools: its SDKs (MIT) are compiled in.
- [Slint](https://slint.dev) (Royalty-free license): the UI toolkit.
- [tantivy](https://github.com/quickwit-oss/tantivy) (MIT): the content index.
- [PP-OCRv6](https://github.com/PaddlePaddle/PaddleOCR) (Apache 2.0) through [oar-ocr](https://github.com/GreatV/oar-ocr)
  (Apache 2.0) and ONNX Runtime (MIT): text in images.
- [PaddleOCR-VL-1.5](https://github.com/PaddlePaddle/PaddleOCR) (Apache 2.0) through
  [llama.cpp](https://github.com/ggml-org/llama.cpp) (MIT): Thorough, on the graphics card.
- [SigLIP 2](https://huggingface.co/google/siglip2-base-patch16-224) by Google (Apache 2.0), from Immich's export;
  EmbeddingGemma 2 by Google (Gemma Terms of Use); [Qwen3-VL-Embedding](https://github.com/QwenLM) by Alibaba's Qwen
  team (Apache 2.0): images by what they show, and documents by what they mean.
- [Whisper](https://github.com/openai/whisper) by OpenAI (MIT) and [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
  by NVIDIA (CC BY 4.0), through [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT), with
  [Silero VAD](https://github.com/snakers4/silero-vad) (MIT): what's said in recordings and videos.
- [GeoNames](https://www.geonames.org) (CC BY 4.0): the places photos are named after.
- [PDFium](https://pdfium.googlesource.com/pdfium/) (BSD/Apache 2.0), built by
  [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries), through pdfium-render: PDFs.
- SQLite (public domain) through rusqlite: the reader's catalog.
- Twemoji by Twitter, CC-BY 4.0
- The BIP-39 English word list (bitcoin/bips, MIT): recognizing recovery phrases.
- Ideas from [EverythingToolbar](https://github.com/srwi/EverythingToolbar) (taskbar placement, its key map) and
  [DigUp](https://github.com/ARahim3/DigUp) (search by what's inside files).

MIT License.
