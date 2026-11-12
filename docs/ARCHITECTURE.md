# Architecture

Native Windows programs written in Rust, with no runtime to install: no WebView, .NET or Electron.

| Program | What it does |
|---|---|
| `excavate.exe` | The popup and the settings window (Slint), the tray icon and the shortcut. Runs all the time, ranks every source as one list, shows previews. Holds Bedrock, the file-name index. Never loads a model. |
| `excavate-indexer.exe` | The reader: lists the chosen folders, reads what's new or changed, writes the content index, follows changes. Started by the popup in a job object (it ends with it), at idle priority. |
| `excavate-helper.exe` | Searches by what images show and what documents mean: holds the models' text halves and every image's vector, answers the popup on a local port, and ends a few minutes after the last search. |
| `pdfium.dll`, `DirectML.dll` | PDF reading; OCR on the graphics card. |
| Downloaded when chosen | The models, llama.cpp's server (Thorough and the image models on the graphics card), `excavate-listen.exe` (speech). |

Idle, every thread waits on a message, an event or the kernel: no timers, no polling.

## A keystroke

1. The text is parsed: plain words, or Everything's syntax.
2. **The places you use** (apps, recent files, history, your folders) are matched in the popup and drawn in the same
   frame.
3. **Names on every drive**, from Bedrock (or Everything), in milliseconds.
4. **The words inside** files, images, videos and recordings, from the content index, which the popup reads directly
   (memory-mapped), so a search never waits on the reader.
5. **What images show**, once typing rests, from the helper.
6. Each answer merges in and the list is ranked again; the selection stays where you put it.

## Bedrock (`crates/engine`)

The file-name index. Each distinct name is kept once in one byte arena, and each entry is a row of small columns: its
name's id and its folder (3 bytes each), size, date modified, flags, extension, its dates created and accessed (coded
against its date modified), and its attributes (an index into a table of the few dozen sets in use). A path is its chain
of folders. A name search scans the distinct names once, then the entries' name ids.

The columns are mapped from the saved file: Windows reads pages in as they're touched and writes changed ones back,
so the index costs almost no private memory and a save writes only what changed. A file not saved cleanly is never
trusted; the drives are listed again instead.

- **Building**: a drive's NTFS table read directly (with administrator rights, once), or its folders listed.
- **Following**: folder watches on one I/O completion port while running; the NTFS change journal at each start for
  what changed while Excavate was closed. An overflowed watch lists just its folder again.
- **Searching**: Everything's syntax, across cores; fuzzy search over the distinct names.

## The reader (`indexer/`)

A session starts from the settings: the chosen folders, what's left out, how closely to read each kind.

1. **List** the chosen folders (from Everything's list when it's the one chosen, else by walking them). Files kept online only (OneDrive's
   placeholders) are never opened, so nothing is downloaded.
2. **Compare** with the catalog (SQLite): new or changed files are read, gone ones removed from the index.
3. **Queue** newest first, in lanes: documents and code; images; then the slower passes (Thorough, what images show,
   videos, speech). What you just changed goes first. A large photo library is sampled first (`crates/sweep`): a few
   images stand for each numbered set or burst, the rest come after.
4. **Read**:
   - documents with `crates/extract`; PDFs by their text layer, scanned pages with OCR;
   - images decoded by Windows (any format it has a codec for, the right way up), then OCR, each line with its box;
   - images and video frames by what they show, as vectors (`crates/seeing`);
   - videos' text at the frames that can show new text, and their speech, each with its moment;
   - recordings through `excavate-listen.exe`.
5. **Protect**: secrets found are replaced by labels before anything is stored, and their place in an image is kept
   for the preview to blur (`crates/privacy`, [privacy](PRIVACY.md)).
6. **Write**: one writer commits every 2 seconds, then records the files in the catalog.
7. **Follow** the chosen folders' changes: a burst settles for 1.5 s, then what changed is read first.

**Out of the way**: idle CPU priority, low memory and I/O priority. Heavy models wait while other programs use the
processor or the graphics card, or a game, a full-screen app or a presentation runs, and stop on battery unless
allowed. Where each model runs fastest is measured on the PC the first time it runs. **Pause** stops everything for a
while; **Read everything now** skips the waiting.

## The content index (`crates/content`)

[tantivy](https://github.com/quickwit-oss/tantivy): BM25 with positions, memory-mapped, one writer (the reader) and any
number of readers (the popup).

- **Fields**: the path, the name, the words (up to 2 MB of text per file), the first 256 KB stored for snippets and
  previews, the kind, date and size, where the words came from (text, image, video, speech, with moments), and privacy
  marks.
- **Tokenizer**, the same for reading and typing: lowercased, accents folded (café = cafe), joined words split and kept
  whole (`getUserName` finds "user name"), Chinese, Japanese and Korean a character at a time.
- **A query**: every word must match, in the text or the name; the last word as a prefix while typing; longer words
  also one letter off (OCR's misreads).

## Updates

Off until turned on. See [releasing](RELEASING.md).
