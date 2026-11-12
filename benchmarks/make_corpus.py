"""The benchmark corpus, made from the public datasets' downloads: unpacked into plain files, the way they'd sit on a
PC, with what each holds (the ground truth searches are scored against) in truth/.

    python make_corpus.py <downloads folder> <out folder>

Run again, it only adds what's missing. Needs pyarrow (for the Hugging Face parquet files). The downloads folder holds:

  000.zip, 002.zip … 020.zip, 200.zip … 210.zip   GovDocs1 threads (digitalcorpora.org)
  val2017.zip, annotations_trainval2017.zip        COCO 2017 (cocodataset.org)
  textvqa/validation-*.parquet                     TextVQA validation, as parquet (textvqa.org; Hugging Face)
  test-clean.tar.gz, dev-clean.tar.gz              LibriSpeech (openslr.org/12)
  fleurs_<language>/test-00000-of-00001.parquet    FLEURS test splits (Hugging Face: google/fleurs)
  test_videos.zip, test_videodatainfo.json.zip     MSR-VTT test

It makes:

  documents/govdocs/<thread>/   GovDocs1: real US-government files of every type (PDF, Word, Excel, PowerPoint, HTML…)
  images/coco-val2017/          COCO 2017 validation photos            truth/coco.jsonl: captions, objects
  images/textvqa-val/           TextVQA validation photos with text    truth/textvqa.jsonl: OCR tokens, questions
  audio/librispeech-<split>/    LibriSpeech read English (FLAC)        truth/librispeech.tsv: transcripts
  audio/fleurs-<language>/      FLEURS read speech (WAV), test split   truth/fleurs.tsv: transcripts
  videos/msrvtt-test/           MSR-VTT test clips (MP4)               truth/msrvtt.jsonl: category, captions
  manifest.json                 counts and bytes of each part
"""
import io
import json
import os
import sys
import tarfile
import time
import zipfile

import pyarrow.parquet as pq

source, out = sys.argv[1], sys.argv[2]
truth_folder = os.path.join(out, "truth")
os.makedirs(truth_folder, exist_ok=True)
manifest = {}


def log(text):
    print(time.strftime("%H:%M:%S"), text, flush=True)


def write(path, data, when=None):
    """Writes a file unless it's there already at that size; `when` (seconds since 1970) as its date modified."""
    if os.path.exists(path) and os.path.getsize(path) == len(data):
        return
    os.makedirs(os.path.dirname(path), exist_ok=True)
    temporary = path + ".part"
    with open(temporary, "wb") as file:
        file.write(data)
    os.replace(temporary, path)
    if when:
        os.utime(path, (when, when))


def zip_time(info):
    try:
        return time.mktime(info.date_time + (0, 0, -1))
    except (OverflowError, ValueError):
        return None


def count(part, folder):
    files, size = 0, 0
    for root, _, names in os.walk(folder):
        for name in names:
            files += 1
            size += os.path.getsize(os.path.join(root, name))
    manifest[part] = {"folder": os.path.relpath(folder, out).replace("\\", "/"), "files": files, "bytes": size}
    log(f"{part}: {files} files, {size / 1e9:.2f} GB")


def unzip(archive, folder, keep=lambda name: True, rename=lambda name: os.path.basename(name), sub=lambda name: ""):
    with zipfile.ZipFile(archive) as z:
        for info in z.infolist():
            if info.is_dir() or not keep(info.filename):
                continue
            write(os.path.join(folder, sub(info.filename), rename(info.filename)), z.read(info), zip_time(info))


def jsonl(name, rows):
    with open(os.path.join(truth_folder, name), "w", encoding="utf-8", newline="\n") as file:
        for row in rows:
            file.write(json.dumps(row, ensure_ascii=False) + "\n")


# ---- Documents: GovDocs1's threads, each in its own folder (as the archive has them) ----
govdocs = os.path.join(out, "documents", "govdocs")
threads = sorted(name for name in os.listdir(source) if name[:3].isdigit() and name.endswith(".zip"))
for thread in threads:
    log(f"govdocs {thread}")
    unzip(os.path.join(source, thread), govdocs, sub=lambda name: os.path.dirname(name))
rows = []
for root, _, names in os.walk(govdocs):
    for name in sorted(names):
        path = os.path.join(root, name)
        rows.append({"file": os.path.relpath(path, out).replace("\\", "/"), "extension": os.path.splitext(name)[1].lstrip(".").lower(),
                     "bytes": os.path.getsize(path)})
jsonl("govdocs.jsonl", rows)
count("govdocs", govdocs)

# ---- Images: COCO val2017, with its captions and the objects in each ----
coco = os.path.join(out, "images", "coco-val2017")
log("coco")
unzip(os.path.join(source, "val2017.zip"), coco, keep=lambda name: name.endswith(".jpg"))
with zipfile.ZipFile(os.path.join(source, "annotations_trainval2017.zip")) as z:
    captions = json.loads(z.read("annotations/captions_val2017.json"))
    instances = json.loads(z.read("annotations/instances_val2017.json"))
names = {image["id"]: image["file_name"] for image in captions["images"]}
categories = {category["id"]: category["name"] for category in instances["categories"]}
by_image = {}
for caption in captions["annotations"]:
    by_image.setdefault(caption["image_id"], {"captions": [], "objects": set()})["captions"].append(caption["caption"].strip())
for instance in instances["annotations"]:
    by_image.setdefault(instance["image_id"], {"captions": [], "objects": set()})["objects"].add(categories[instance["category_id"]])
jsonl("coco.jsonl", ({"file": f"images/coco-val2017/{names[i]}", "captions": v["captions"], "objects": sorted(v["objects"])}
                     for i, v in sorted(by_image.items()) if i in names))
count("coco", coco)

# ---- Images with text: TextVQA's validation photos (a row per question: each photo once) ----
textvqa = os.path.join(out, "images", "textvqa-val")
log("textvqa")
seen = {}
for shard in sorted(os.listdir(os.path.join(source, "textvqa"))):
    table = pq.ParquetFile(os.path.join(source, "textvqa", shard))
    for group in range(table.num_row_groups):
        for row in table.read_row_group(group).to_pylist():
            name = f"{row['image_id']}.jpg"
            if name not in seen:
                write(os.path.join(textvqa, name), row["image"]["bytes"])
                seen[name] = {"file": f"images/textvqa-val/{name}", "ocr_tokens": row["ocr_tokens"], "classes": row["image_classes"],
                              "questions": []}
            seen[name]["questions"].append({"question": row["question"], "answers": sorted(set(row["answers"]))})
jsonl("textvqa.jsonl", (seen[name] for name in sorted(seen)))
count("textvqa", textvqa)

# ---- Speech: LibriSpeech test-clean and dev-clean (FLAC, a folder per split), with each recording's words ----
lines = []
for split in ["test-clean", "dev-clean"]:
    folder = os.path.join(out, "audio", f"librispeech-{split}")
    log(f"librispeech {split}")
    with tarfile.open(os.path.join(source, f"{split}.tar.gz")) as tar:
        for member in tar:
            if not member.isfile():
                continue
            data = tar.extractfile(member).read()
            if member.name.endswith(".flac"):
                write(os.path.join(folder, os.path.basename(member.name)), data, member.mtime)
            elif member.name.endswith(".trans.txt"):
                for line in data.decode("utf-8").splitlines():
                    key, _, words = line.partition(" ")
                    lines.append(f"audio/librispeech-{split}/{key}.flac\ten\t{words.strip().lower()}")
    count(f"librispeech-{split}", folder)
with open(os.path.join(truth_folder, "librispeech.tsv"), "w", encoding="utf-8", newline="\n") as file:
    file.write("file\tlanguage\ttranscript\n" + "\n".join(lines) + "\n")

# ---- Speech in other languages: FLEURS' test splits (WAV), each recording's words as said ----
lines = []
for language in sorted(name[len("fleurs_"):] for name in os.listdir(source) if name.startswith("fleurs_")):
    folder = os.path.join(out, "audio", f"fleurs-{language.replace('_', '-')}")
    log(f"fleurs {language}")
    table = pq.ParquetFile(os.path.join(source, f"fleurs_{language}", "test-00000-of-00001.parquet"))
    for group in range(table.num_row_groups):
        for row in table.read_row_group(group).to_pylist():
            name = os.path.basename(row["audio"]["path"] or row["path"])
            write(os.path.join(folder, name), row["audio"]["bytes"])
            lines.append(f"{os.path.relpath(os.path.join(folder, name), out).replace(os.sep, '/')}\t{language}\t{row['raw_transcription'].strip()}")
    count(f"fleurs-{language}", folder)
with open(os.path.join(truth_folder, "fleurs.tsv"), "w", encoding="utf-8", newline="\n") as file:
    file.write("file\tlanguage\ttranscript\n" + "\n".join(lines) + "\n")

# ---- Videos: MSR-VTT's 2,990 test clips, with their category and twenty captions each ----
msrvtt = os.path.join(out, "videos", "msrvtt-test")
log("msrvtt")
unzip(os.path.join(source, "test_videos.zip"), msrvtt, keep=lambda name: name.endswith(".mp4"))
with zipfile.ZipFile(os.path.join(source, "test_videodatainfo.json.zip")) as z:
    info = json.loads(z.read("test_videodatainfo.json"))
category_names = ["music", "people", "gaming", "sports/actions", "news/events/politics", "education", "tv shows", "movie/comedy",
                  "animation", "vehicles/autos", "howto", "travel", "science/technology", "animals/pets", "kids/family", "documentary",
                  "food/drink", "cooking", "beauty/fashion", "advertisement"]
clips = {video["video_id"]: {"file": f"videos/msrvtt-test/{video['video_id']}.mp4", "category": category_names[video["category"]],
                             "seconds": round(video["end time"] - video["start time"], 2), "captions": []} for video in info["videos"]}
for sentence in info["sentences"]:
    if sentence["video_id"] in clips:
        clips[sentence["video_id"]]["captions"].append(sentence["caption"].strip())
jsonl("msrvtt.jsonl", (clips[key] for key in sorted(clips, key=lambda key: int(key[5:]))))
count("msrvtt", msrvtt)

with open(os.path.join(out, "manifest.json"), "w", encoding="utf-8", newline="\n") as file:
    json.dump({"made": time.strftime("%Y-%m-%d %H:%M"), "parts": manifest,
               "files": sum(part["files"] for part in manifest.values()), "bytes": sum(part["bytes"] for part in manifest.values())},
              file, indent=2)
log("done")
