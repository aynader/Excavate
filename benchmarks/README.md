# Benchmarks

How Excavate performs, and how Bedrock compares with Everything, on an everyday PC with millions of files: the
figures Excavate's own pages show come from here. Then the same runs on a clean virtual machine, where nothing else
competes, to confirm them under controlled conditions. Every figure comes from the setup below, with the raw results
beside it, so anyone can check them or run them again.

## The machines

**An everyday PC.** A laptop in daily use, with its own files, programs and background services, as Excavate is meant
to run. Its files are its owner's, so the results hold only counts and timings: no file names, folders or paths.

| | |
|---|---|
| Windows | Windows 11 Home 25H2 (build 26200), virtualization-based security on |
| Processor | Intel Core i9-13900HX (24 cores, 32 threads) |
| Memory | 32 GB |
| Graphics | NVIDIA GeForce RTX 4080 Laptop GPU, 12 GB (Excavate uses it through Vulkan) |
| Drives | two, NTFS: 9,453,012 files and folders |
| Power | plugged in, Windows' Balanced plan |
| Everything | 1.4.1.1026, the settings its owner uses |

**A clean virtual machine.** VirtualBox, installed unattended and reset to the same snapshot before each run:

| | |
|---|---|
| Windows | [Windows X-Lite "Micro 11"](https://windowsxlite.com/micro11/), build 22621.1928 (Windows 11 22H2 Pro, 64-bit) |
| Processor | 4 cores of the same Intel Core i9-13900HX, AVX2 (VirtualBox on the processor itself, not under Windows' hypervisor) |
| Memory | 8 GB |
| Disk | 64 GB, NTFS: 72,495 files and folders |
| Graphics | VirtualBox's virtual adapter (none to compute on) |
| User Account Control | on (Micro 11 has it off) |
| Network | NAT, only for downloading the models and Everything |
| Everything | 1.4.1.1032, as Excavate installs it, default settings |

**Why Micro 11.** A stock Windows install keeps dozens of things busy in the background that have nothing to do with
what's measured: Defender scanning every file as it's read, the Windows Search indexer reading the same files,
telemetry, error reporting, OneDrive, Edge and the inbox apps updating themselves. Each takes processor time, disk and
memory at moments no one controls, so the same run gives different numbers from one try to the next. Micro 11 is
Windows 11 with those removed or turned off, so what's left is mostly Windows itself and the program measured: smaller
differences between runs, and figures that belong to Excavate and Everything rather than to whatever else woke up.
It also makes the machine a hard case: 4 cores and no graphics card, and what Excavate needs that a full Windows
usually brings shows up here first.

## The data

Names are measured on each machine's own drives. Reading and finding use the same 35,213 files, 15.2 GB, from public
datasets, on both machines, unpacked as they'd sit on a PC, each with what it holds (the ground truth a search is
scored against):

| Kind | Dataset | Files | What's scored |
|---|---|---|---|
| Documents | [GovDocs1](https://digitalcorpora.org/corpora/file-corpora/files/), 17 of its 1,000 threads | 16,797 | Finding a file by words inside it |
| Photos | [COCO 2017](https://cocodataset.org) validation | 5,000 | Finding a photo by a description (its captions) |
| Photos with text | [TextVQA](https://textvqa.org) validation | 3,166 | Reading the text in a photo |
| Speech, English | [LibriSpeech](https://www.openslr.org/12) test-clean and dev-clean | 5,323 | Words heard (word error rate), on test-clean |
| Speech, other languages | [FLEURS](https://huggingface.co/datasets/google/fleurs) Arabic, German, English | 1,937 | Words heard in each language |
| Videos | [MSR-VTT](https://www.microsoft.com/en-us/research/publication/msr-vtt-a-large-video-description-dataset-for-bridging-video-and-language/) test | 2,990 | Finding a clip by a description |

## What's measured

All by `measure` (in [measure/](measure/)), which writes each result to a JSON file: every figure with how many times
it was measured, its median, the median's 95% interval, the 95th percentile and the extremes; every share with its
95% interval. Samples are drawn with a fixed seed, so a run again picks the same files. Both machines run the same
stages with the same sample sizes.

- **Names, Bedrock against Everything.** Bedrock runs as it does in Excavate (the same service, given administrator
  rights so it reads the drives' tables); Everything is asked through its SDK, as Excavate asks it.
  - Building from nothing and opening at start: 5 times each, for both.
  - Memory, and size on disk.
  - The [searches in Everything's syntax](names-queries.txt): 30 times each, alternating, timed until the first 100
    results, sorted by name, are in hand; both counts compared.
  - A new file: 50 made, each engine asked until it has it.
  - Changes: 10,000 files made, half renamed, half deleted, by another process; each one's processor time per change,
    5 times. Processor time when idle, over a minute.
- **Reading.** Excavate's own indexer from the release, reading one kind with one choice from nothing, each run in a
  fresh profile, after a warm-up that downloads the models. A seeded sample per kind: 1,000 documents, 300 images, 200
  recordings and 100 videos, 5 runs each; the slowest on a processor alone (Balanced for what images show, for speech
  and for videos) 200 images, 50 recordings (40 in the other languages) and 100 videos, 3 runs each. Time per file,
  processor time, peak memory (the indexer and the model servers it starts), and how late a 10 ms timer fires
  meanwhile. Quick and Balanced; Thorough's models are measured on the graphics card, below.
- **Finding.** A seeded sample of a kind read once (Balanced): 3,000 documents, 600 photos with text, 300 photos, 150
  videos; for speech, the recordings of the reading runs. Then up to 1,000 of them searched for, as the popup searches:
  - documents: four words in a row from the file's text;
  - photos with text: the longest word in the dataset's reading of the photo; also, how many of its words were read;
  - speech: four words in a row from the reference transcript; also the word error rate;
  - photos and videos: the first of the dataset's descriptions.

  How often the right file comes first, and in the top ten.

## The graphics card

What runs on the graphics card is also measured on its own, on the everyday PC's card: in a clean Ubuntu 24.04 under
WSL2, with the same model files, settings and prompts Excavate uses, in the same engines
([llama.cpp](https://github.com/ggml-org/llama.cpp) b12001, [whisper.cpp](https://github.com/ggml-org/whisper.cpp)
d1be6fd) built for CUDA. A distribution made only for this, with nothing else installed, keeps the measurements free
of whatever else a desktop runs, and the same commands make the same environment on any PC with an NVIDIA card.

| | |
|---|---|
| Graphics card | NVIDIA GeForce RTX 4080 Laptop GPU, 12 GB |
| Driver | 610.88 (Windows), CUDA 13.3 driver interface |
| CUDA toolkit | 12.9 (runtime 12.9.79) |
| System | Ubuntu 24.04.5 LTS under WSL 2.7.3 (kernel 6.6.114.1), 15.5 GB of memory given to it |
| Processor | Intel Core i9-13900HX (32 threads) |

| Workload | Model | Data | Scored by |
|---|---|---|---|
| `ocr-thorough` | PaddleOCR-VL-1.5 | 50 TextVQA photos | share of the photos' known words read |
| `images-gemma` | EmbeddingGemma 2 | 200 COCO photos | each photo found by its caption: first, or in the top ten |
| `images-qwen` | Qwen3-VL-Embedding-8B | 200 COCO photos | the same |
| `speech-turbo` | Whisper large-v3-turbo | 100 LibriSpeech recordings | word error rate, and time per second of speech |
| `speech-large` | Whisper large-v3 | 100 LibriSpeech recordings | the same |

Each workload runs five trials. A trial starts the engine afresh, sends one item to warm it up, then times every
item; the card's use and memory are sampled every half second meanwhile. Every result file records the card, its
driver, the CUDA driver interface and toolkit, the engines' versions, the operating system and WSL's version, the
processor and memory, the model files' SHA-256, and every setting, beside every timing.

On Windows, Excavate runs these models through Vulkan rather than CUDA. The figures here are the models' cost on the
card under controlled conditions; Excavate measures its own speed on each PC the first time each model runs, and the
first-run steps show those times.

## Running them again

- **The data**: download the datasets above, then `python make_corpus.py <downloads> <corpus>`.
- **The everyday PC**: Excavate quit, the corpus anywhere, Excavate's release unzipped.
- **The virtual machine**: VirtualBox 7.1 with the settings above, the corpus in the account's own folders (documents
  in Documents, photos in Pictures, speech in Music, clips in Videos), Excavate's release unzipped in Downloads.
- **Names, reading and finding**: `cargo build --release -p measure`, then on the machine (the first as
  administrator):

  ```bat
  measure names --everything "C:\Program Files\Everything\Everything.exe" --everything-db <its Everything.db>
  measure reading --excavate <release> --data <corpus> --kind image-shown --choice balanced --files 300
  measure reading --excavate <release> --data <corpus> --kind image-shown --choice balanced --files 0 --runs 1 --keep <home>
  measure finding --home <home> --excavate <release>
  ```

  `measure` alone lists the options and kinds.
- **The graphics card**:

  ```bash
  wsl --install Ubuntu-24.04 --name excavate-gpu --no-launch
  wsl -d excavate-gpu -u root -- bash gpu/setup.sh
  wsl -d excavate-gpu -u root -- /opt/excavate-bench/venv/bin/python gpu/run.py ocr-thorough --data <corpus>
  ```

## Results

Excavate 0.1.0, 10-11 November 2026. Raw files in [results/laptop](results/laptop) (the everyday PC) and [results/vm](results/vm) (the virtual machine); the tables below are made from them by [results_tables.py](results_tables.py).

### On the everyday PC

#### Names: Bedrock and Everything

9,453,012 files and folders on its two drives.

| | Bedrock | Everything |
|---|---|---|
| Building from nothing, from the drives' own tables (administrator rights) | 10.06 s | 27.88 s |
| Building from nothing, folder by folder (without them) | 237 s | – |
| Opening at start | 0.067 s | 2.103 s |
| Answering after start | 24.7 ms | – |
| Memory | 87.5 MB | 800 MB (its two processes) |
| Size on disk | ~340 MB | ~340 MB |
| A new file found | 6.2 ms | 926 ms |
| Processor time per change (20,000 changes, 5 times) | 102 µs | 426 µs |
| Processor time when idle, over 60 s | 0.69 s | 0.14 s |

The 44 searches, 30 times each, until the first 100 results sorted by name are in hand: the median of their medians is 7.0 ms for Bedrock and 27.5 ms for Everything. Bedrock is faster on 33 of 44; Everything by a few milliseconds on searches by date or size and on those with hundreds of thousands of results. The counts differ by a few files changing while the run went on, except where Bedrock finds more (`ext:mp4`, `*.jp*g`, `size:>1gb`): files in folders this PC's Everything is set to leave out.

| Search | Bedrock | Everything | Bedrock, ms | Everything, ms |
|---|---|---|---|---|
| `readme` | 24,209 | 24,208 | 6.25 | 26.60 |
| `config` | 185,547 | 185,546 | 6.66 | 30.55 |
| `setup` | 7,221 | 7,221 | 5.08 | 30.77 |
| `report final` | 10 | 10 | 5.29 | 31.93 |
| `zzqxj` | 0 | 0 | 4.82 | 29.58 |
| `e` | 6,435,315 | 6,433,448 | 28.84 | 27.53 |
| `readme \| license` | 63,806 | 63,805 | 6.22 | 56.31 |
| `readme !md` | 2,778 | 2,778 | 5.55 | 32.02 |
| `"read me"` | 7 | 7 | 5.14 | 30.82 |
| `<readme \| license> ext:md` | 23,345 | 23,345 | 8.70 | 58.55 |
| `config setup` | 90 | 90 | 6.41 | 37.38 |
| `*.dll` | 158,399 | 158,399 | 7.25 | 12.71 |
| `*.jp*g` | 58,465 | 57,160 | 7.22 | 66.32 |
| `IMG_????*` | 5,824 | 5,812 | 5.18 | 11.89 |
| `*report*.pdf` | 113 | 113 | 6.18 | 64.06 |
| `setup.exe` | 190 | 190 | 5.48 | 31.55 |
| `case:README` | 20,203 | 20,202 | 6.41 | 32.24 |
| `ww:config` | 150,558 | 150,557 | 7.96 | 51.48 |
| `path:system32 .dll` | 8,217 | 8,217 | 38.54 | 233 |
| `wfn:readme.md` | 20,949 | 20,949 | 6.18 | 12.90 |
| `file:readme` | 24,197 | 24,196 | 6.86 | 26.74 |
| `folder:config` | 50,724 | 50,724 | 6.75 | 10.20 |
| `nocase:README` | 24,209 | 24,208 | 6.34 | 34.19 |
| `startwith:read` | 28,783 | 28,785 | 6.42 | 12.61 |
| `endwith:.json` | 1,196,703 | 1,196,703 | 22.77 | 14.44 |
| `ext:jpg;png;gif` | 276,552 | 274,524 | 8.37 | 11.88 |
| `ext:flac;wav` | 26,519 | 26,519 | 6.81 | 11.90 |
| `ext:mp4` | 6,760 | 6,425 | 6.48 | 11.66 |
| `ext:pdf dm:thisyear` | 1,329 | 1,328 | 7.04 | 12.36 |
| `regex:^[a-f0-9]{32}$` | 93,724 | 93,724 | 21.44 | 299 |
| `regex:"\.(tmp\|log)$"` | 32,234 | 32,235 | 6.61 | 318 |
| `len:>60` | 311,098 | 310,708 | 28.53 | 17.89 |
| `parent:C:\Windows` | 121 | 121 | 9.60 | 139 |
| `infolder:C:\Windows` | 121 | 121 | 9.92 | 136 |
| `nosubfolders:C:\Windows` | 121 | 121 | 12.35 | 137 |
| `dm:today` | 61,481 | 61,515 | 13.75 | 12.15 |
| `dm:thisweek` | 309,757 | 309,719 | 15.23 | 12.69 |
| `dm:lastmonth` | 1,663,225 | 1,663,168 | 18.91 | 14.41 |
| `dm:>2025-01-01` | 7,326,626 | 7,324,269 | 28.67 | 23.30 |
| `size:>1gb` | 360 | 334 | 15.70 | 10.97 |
| `size:empty` | 154,196 | 154,044 | 16.67 | 11.40 |
| `size:tiny` | 5,392,990 | 5,392,315 | 25.46 | 20.55 |
| `size:1mb..10mb` | 123,800 | 123,671 | 16.89 | 11.59 |
| `size:>10mb ext:mp4` | 719 | 529 | 5.29 | 11.54 |

#### Reading

From nothing, in a fresh profile each run, with all 32 threads and the graphics card where Excavate puts its models. Time is per file Excavate reads (recording heard, video looked at), whole runs included; the 95% interval of the median in brackets. Processor time and peak memory are for a whole run, with the model servers the indexer starts. The last column: how late a 10 ms timer fired meanwhile (99th percentile), a measure of how the PC stays responsive.

| Kind | Choice | Read | Time each | Processor | Peak memory | Timer late, p99 |
|---|---|---|---|---|---|---|
| Documents | Balanced | 673 files × 5 | 82 ms (78–110) | 52 s | 137 MB | 0.7 ms |
| Text in photos | Quick | 300 files × 5 | 109 ms (105–125) | 155 s | 243 MB | 0.8 ms |
| Text in photos | Balanced | 300 files × 5 | 253 ms (236–263) | 404 s | 323 MB | 0.8 ms |
| What photos show | Quick | 300 files × 5 | 38 ms (38–54) | 5 s | 1,140 MB | 0.7 ms |
| What photos show | Balanced | 200 files × 3 | 137 ms (132–152) | 72 s | 2,505 MB | 0.7 ms |
| What videos show | Quick | 100 videos × 5 | 143 ms (133–153) | 11 s | 1,163 MB | 0.7 ms |
| What videos show | Balanced | 100 videos × 3 | 253 ms (253–276) | 26 s | 2,547 MB | 0.7 ms |
| Speech, English | Quick | 200 recordings × 5 | 435 ms (429–466) | 668 s | 1,694 MB | 0.7 ms |
| Speech, English | Balanced | 50 recordings × 3 | 306 ms (306–306) | 8 s | 1,819 MB | 0.8 ms |
| Speech, other languages | Balanced | 40 recordings × 3 | 357 ms (332–397) | 8 s | 1,822 MB | 0.7 ms |

#### Finding

Balanced. The right file first, and in the top ten, with the share's 95% interval.

| Kind | Files | Searches | First | Top ten | Search time | Also |
|---|---|---|---|---|---|---|
| Documents | 3,000 | 662 | 67% (63–70%) | 90% (87–92%) | 42.54 ms |  |
| Text in photos | 600 | 532 | 48% (44–52%) | 51% (47–55%) | 0.03 ms | 67% of the photos' words read |
| What photos show | 300 | 300 | 81% (76–85%) | 99% (97–99%) | 8.91 ms |  |
| What videos show | 150 | 150 | 68% (60–75%) | 91% (86–95%) | 9.42 ms |  |
| Speech, English | 50 | 41 | 93% (81–97%) | 93% (81–97%) | 0.12 ms | word error rate 2.0% |
| Speech, other languages | 40 | 40 | 98% (87–100%) | 98% (87–100%) | 0.19 ms | word error rate 5.8% (ar_eg 5.3%, de_de 5.5%, en_us 6.5%) |

#### On the graphics card

The models on the card alone, as described under [The graphics card](#the-graphics-card): the median of five trials' medians per item, with the trials' spread; a speech item includes loading the model.

| Kind | Model | Items × trials | Time each | Scored | Card use | Card memory, peak |
|---|---|---|---|---|---|---|
| Text in images, Thorough | PaddleOCR-VL-1.5 | 50 × 5 | 490 ms (481–494) | 52% of the photos' words read | 81% | 2.7 GB |
| What photos show | EmbeddingGemma 2 | 200 × 5 | 52 ms (48–55) | first 89%, top ten 100% (among 200) | 36% | 2.0 GB |
| What photos show, Thorough | Qwen3-VL-Embedding-8B | 200 × 5 | 157 ms (156–159) | first 88%, top ten 100% (among 200) | 66% | 8.3 GB |
| Speech, Balanced | Whisper large-v3-turbo | 100 × 5 | 724 ms (672–763) | word error rate 1.7%, 0.08× real time | 27% | 1.8 GB |
| Speech, Thorough | Whisper large-v3 | 100 × 5 | 1,100 ms (1,072–1,172) | word error rate 1.7%, 0.13× real time | 36% | 2.7 GB |

### Confirmed on the virtual machine

The same stages and samples on the clean, small machine: 4 cores, no graphics card, nothing else running.

#### Names: Bedrock and Everything

72,495 files and folders on the machine's drive.

| | Bedrock | Everything |
|---|---|---|
| Building from nothing, from the drive's own tables (administrator rights) | 0.14 s | 0.45 s |
| Building from nothing, folder by folder (without them) | 0.11 s | – |
| Opening at start | 0.012 s | 0.041 s |
| Answering after start | 1.2 ms | – |
| Memory | 4.6 MB | 15.1 MB (its two processes) |
| Size on disk | 6.3 MB | 2.9 MB |
| A new file found | 1.3 ms | 921 ms |
| Processor time per change (20,000 changes, 5 times) | 20.3 µs | 10.9 µs |
| Processor time when idle, over 60 s | 0.00 s | 0.00 s |

**Why two rows differ here.** On this machine Everything used less processor time per change, and its index was smaller. Both come from the machine's size, 72,495 files, a hundred times fewer than the everyday PC's. Everything's time per change grows with the number of files it holds, from 10.9 µs here to 426 µs there; Bedrock's grows far less, from 20.3 µs to 102 µs. And Bedrock's index has a few megabytes that don't grow with the files: they show on a small drive and vanish on a big one. Excavate is built for PCs with millions of files, where Bedrock comes out ahead on both.

The 44 searches, 30 times each (medians). The counts agree for 34 of 44; where they differ, by a few files that changed while the run went on (Everything's database and the run's own files).

| Search | Bedrock | Everything | Bedrock, ms | Everything, ms |
|---|---|---|---|---|
| `readme` | 4 | 4 | 0.32 | 2.95 |
| `config` | 518 | 518 | 0.48 | 3.06 |
| `setup` | 195 | 195 | 0.43 | 3.23 |
| `report final` | 0 | 0 | 0.36 | 2.99 |
| `zzqxj` | 0 | 0 | 0.37 | 3.31 |
| `e` | 32,707 | 32,722 | 0.81 | 2.75 |
| `readme \| license` | 109 | 109 | 0.62 | 4.09 |
| `readme !md` | 2 | 2 | 0.31 | 2.75 |
| `"read me"` | 0 | 0 | 0.31 | 2.79 |
| `<readme \| license> ext:md` | 2 | 2 | 0.15 | 3.67 |
| `config setup` | 0 | 0 | 0.33 | 2.84 |
| `*.dll` | 8,259 | 8,259 | 0.98 | 2.54 |
| `*.jp*g` | 9,579 | 9,584 | 0.87 | 4.57 |
| `IMG_????*` | 0 | 0 | 0.25 | 2.16 |
| `*report*.pdf` | 0 | 0 | 0.42 | 4.01 |
| `setup.exe` | 15 | 15 | 0.42 | 2.88 |
| `case:README` | 2 | 2 | 0.36 | 2.96 |
| `ww:config` | 202 | 202 | 0.54 | 3.85 |
| `path:system32 .dll` | 4,945 | 4,945 | 6.48 | 6.58 |
| `wfn:readme.md` | 2 | 2 | 0.37 | 2.08 |
| `file:readme` | 4 | 4 | 0.32 | 2.81 |
| `folder:config` | 44 | 44 | 0.42 | 2.06 |
| `nocase:README` | 4 | 4 | 0.34 | 2.91 |
| `startwith:read` | 69 | 70 | 0.30 | 2.11 |
| `endwith:.json` | 403 | 403 | 0.50 | 2.32 |
| `ext:jpg;png;gif` | 13,274 | 13,279 | 0.43 | 2.26 |
| `ext:flac;wav` | 7,343 | 7,343 | 0.28 | 2.28 |
| `ext:mp4` | 2,991 | 2,991 | 0.22 | 2.05 |
| `ext:pdf dm:thisyear` | 0 | 0 | 0.18 | 2.47 |
| `regex:^[a-f0-9]{32}$` | 25 | 25 | 0.89 | 4.59 |
| `regex:"\.(tmp\|log)$"` | 283 | 283 | 0.58 | 5.53 |
| `len:>60` | 3,421 | 3,421 | 1.62 | 2.64 |
| `parent:C:\Windows` | 93 | 93 | 0.25 | 2.35 |
| `infolder:C:\Windows` | 93 | 93 | 0.25 | 2.27 |
| `nosubfolders:C:\Windows` | 93 | 93 | 0.25 | 2.28 |
| `dm:today` | 811 | 824 | 0.41 | 2.19 |
| `dm:thisweek` | 6,889 | 6,906 | 0.43 | 2.27 |
| `dm:lastmonth` | 25 | 25 | 0.22 | 2.09 |
| `dm:>2025-01-01` | 7,038 | 7,055 | 0.42 | 2.27 |
| `size:>1gb` | 0 | 0 | 0.22 | 1.93 |
| `size:empty` | 217 | 200 | 0.37 | 2.06 |
| `size:tiny` | 17,225 | 17,236 | 0.64 | 1.94 |
| `size:1mb..10mb` | 3,473 | 3,479 | 0.50 | 1.96 |
| `size:>10mb ext:mp4` | 0 | 0 | 0.13 | 1.81 |

#### Reading

As on the everyday PC, on 4 cores without a graphics card (Windows' own timer steps by 15.6 ms here, so up to that much late in the last column is the timer, not the load).

| Kind | Choice | Read | Time each | Processor | Peak memory | Timer late, p99 |
|---|---|---|---|---|---|---|
| Documents | Balanced | 673 files × 5 | 92 ms (86–93) | 63 s | 126 MB | 10.7 ms |
| Text in photos | Quick | 300 files × 5 | 233 ms (223–333) | 114 s | 240 MB | 11.7 ms |
| Text in photos | Balanced | 300 files × 5 | 509 ms (498–512) | 256 s | 291 MB | 9.2 ms |
| What photos show | Quick | 300 files × 5 | 195 ms (179–206) | 89 s | 566 MB | 12.2 ms |
| What photos show | Balanced | 200 files × 3 | 2,510 ms (2,504–2,550) | 1,976 s | 1,763 MB | 5.6 ms |
| What videos show | Quick | 100 videos × 5 | 254 ms (254–264) | 37 s | 565 MB | 12.5 ms |
| What videos show | Balanced | 100 videos × 3 | 7,549 ms (7,349–7,569) | 3,004 s | 1,764 MB | 2.7 ms |
| Speech, English | Quick | 200 recordings × 5 | 1,114 ms (1,105–1,129) | 440 s | 1,658 MB | 6.6 ms |
| Speech, English | Balanced | 50 recordings × 3 | 36,582 ms (36,513–36,638) | 3,654 s | 1,681 MB | 2.9 ms |
| Speech, other languages | Balanced | 40 recordings × 3 | 36,532 ms (36,510–36,884) | 2,922 s | 1,684 MB | 2.9 ms |

#### Finding

Balanced, as on the everyday PC.

| Kind | Files | Searches | First | Top ten | Search time | Also |
|---|---|---|---|---|---|---|
| Documents | 3,000 | 662 | 66% (63–70%) | 90% (87–92%) | 86.52 ms |  |
| Text in photos | 600 | 532 | 48% (44–52%) | 51% (47–55%) | 0.04 ms | 67% of the photos' words read |
| What photos show | 300 | 300 | 81% (76–85%) | 99% (97–99%) | 34.29 ms |  |
| What videos show | 150 | 150 | 65% (57–72%) | 92% (87–95%) | 32.86 ms |  |
| Speech, English | 50 | 41 | 93% (81–97%) | 93% (81–97%) | 0.10 ms | word error rate 2.1% |
| Speech, other languages | 40 | 40 | 98% (87–100%) | 98% (87–100%) | 0.19 ms | word error rate 5.6% (ar_eg 5.0%, de_de 5.5%, en_us 6.5%) |
