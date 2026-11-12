"""The Results section of benchmarks/README.md, made from the JSON files in results/laptop (the everyday PC) and
results/vm (the virtual machine): python results_tables.py (prints Markdown: everything below "## Results").
Medians, with the median's 95% interval where it says something."""
import json
import pathlib
import sys

results = pathlib.Path(__file__).parent / "results"


def load(here, name):
    path = here / f"{name}.json"
    return json.loads(path.read_text(encoding="utf-8-sig"))["result"] if path.exists() else None


def num(value, digits=1):
    if value is None:
        return "–"
    if abs(value) >= 100:
        return f"{value:,.0f}"
    return f"{value:.{digits}f}"


def med(summary, digits=1, scale=1.0):
    if not summary or summary.get("median") is None:
        return "–"
    return num(summary["median"] * scale, digits)


def share(s):
    return f"{s['share'] * 100:.0f}% ({s['share_95'][0] * 100:.0f}–{s['share_95'][1] * 100:.0f}%)"


def on_disk(b, e):
    """Both sizes; within 2% of each other they're the same, to a rounding, and shown so."""
    ours, theirs = b["size_on_disk_mb"], e.get("size_on_disk_mb")
    if theirs and abs(ours - theirs) <= 0.02 * max(ours, theirs):
        same = f"~{round((ours + theirs) / 2, -1):,.0f} MB"
        return same, same
    return f"{num(ours, 1)} MB", f"{num(theirs, 1)} MB"


def names_table(names, drives):
    b, e = names["bedrock"], names["everything"]
    ours_disk, theirs_disk = on_disk(b, e)
    return ["| | Bedrock | Everything |", "|---|---|---|",
            f"| Building from nothing, from the {drives} own tables (administrator rights) | {med(b.get('build_from_the_drives_tables_seconds'), 2)} s | {med(e.get('build_seconds'), 2)} s |",
            f"| Building from nothing, folder by folder (without them) | {med(b['build_seconds'], 2)} s | – |",
            f"| Opening at start | {med(b['open_seconds'], 3)} s | {med(e.get('open_seconds'), 3)} s |",
            f"| Answering after start | {med(b['answering_after_ms'], 1)} ms | – |",
            f"| Memory | {med(b['memory_mb'], 1)} MB | {num(e['memory_mb']['both'], 1)} MB (its two processes) |",
            f"| Size on disk | {ours_disk} | {theirs_disk} |",
            f"| A new file found | {med(names['new_file_found_ms']['bedrock'], 1)} ms | {med(names['new_file_found_ms']['everything'], 0)} ms |",
            f"| Processor time per change ({names['changes']['changes_each_run']:,} changes, 5 times) | {med(names['changes']['bedrock_cpu_us'], 1)} µs | {med(names['changes']['everything_cpu_us'], 1)} µs |",
            f"| Processor time when idle, over {names['idle']['seconds']} s | {num(names['idle']['bedrock_cpu_seconds'], 2)} s | {num(names['idle']['everything_cpu_seconds'], 2)} s |",
            ""]


def searches_table(q):
    rows = ["| Search | Bedrock | Everything | Bedrock, ms | Everything, ms |", "|---|---|---|---|---|"]
    for row in q["each"]:
        theirs = row.get("everything_count")
        rows.append(f"| `{row['query'].replace('|', chr(92) + '|')}` | {row['bedrock_count']:,} | "
                    f"{'–' if theirs is None else format(theirs, ',')} | "
                    f"{med(row['bedrock_ms'], 2)} | {med(row.get('everything_ms'), 2)} |")
    return rows + [""]


kinds = [("documents", "Documents"), ("image-text", "Text in photos"), ("image-shown", "What photos show"),
         ("video-shown", "What videos show"), ("speech", "Speech, English"), ("speech-fleurs", "Speech, other languages")]


def reading_table(here):
    rows = []
    for kind, label in kinds:
        for choice in ("quick", "balanced"):
            found = sorted(here.glob(f"reading-{kind}-{choice}-*.json"))
            if not found:
                continue
            r = json.loads(found[0].read_text(encoding="utf-8-sig"))["result"]
            item = {"speech": "recording", "speech-fleurs": "recording", "video-shown": "video"}.get(kind, "file")
            rows.append(f"| {label} | {choice.capitalize()} | {num(r['files_read']['median'], 0)} {item}s × {r['finished_runs']} | "
                        f"{med(r['ms_per_file'], 0)} ms ({num(r['ms_per_file']['median_95'][0], 0)}–{num(r['ms_per_file']['median_95'][1], 0)}) | "
                        f"{med(r['cpu_seconds'], 0)} s | {med(r['peak_memory_mb'], 0)} MB | {num(r['timer_late_ms']['p99'], 1)} ms |")
    if not rows:
        return []
    return ["| Kind | Choice | Read | Time each | Processor | Peak memory | Timer late, p99 |",
            "|---|---|---|---|---|---|---|"] + rows + [""]


def finding_table(here):
    rows = []
    for kind, label in kinds:
        found = [p for p in sorted(here.glob(f"finding-{kind}-balanced-*.json")) if not p.name.endswith("-read.json")]
        if not found:
            continue
        r = json.loads(found[0].read_text(encoding="utf-8-sig"))["result"]
        extra = []
        if r.get("reference_words_read"):
            extra.append(f"{r['reference_words_read']['share'] * 100:.0f}% of the photos' words read")
        if r.get("word_error_rate") is not None:
            by = r.get("word_error_rate_by_language") or {}
            extra.append(f"word error rate {r['word_error_rate'] * 100:.1f}%" +
                         (" (" + ", ".join(f"{k} {v * 100:.1f}%" for k, v in sorted(by.items())) + ")" if len(by) > 1 else ""))
        rows.append(f"| {label} | {r['files_read']:,} | {r['queries']:,} | {share(r['first'])} | {share(r['top_ten'])} | "
                    f"{med(r['search_ms'], 2)} ms | {'; '.join(extra)} |")
    if not rows:
        return []
    return ["| Kind | Files | Searches | First | Top ten | Search time | Also |", "|---|---|---|---|---|---|---|"] + rows + [""]


def gpu_table(here):
    """The graphics-card workloads (gpu/run.py): the newest result of each."""
    models = {"ocr-thorough": ("Text in images, Thorough", "PaddleOCR-VL-1.5"), "images-gemma": ("What photos show", "EmbeddingGemma 2"),
              "images-qwen": ("What photos show, Thorough", "Qwen3-VL-Embedding-8B"),
              "speech-turbo": ("Speech, Balanced", "Whisper large-v3-turbo"), "speech-large": ("Speech, Thorough", "Whisper large-v3")}
    rows = []
    for workload, (label, model) in models.items():
        found = sorted(here.glob(f"{workload}-*.json"))
        if not found:
            continue
        r = json.loads(found[-1].read_text(encoding="utf-8"))
        s, score = r["summary"], r["summary"]["score"]
        if "ocr_tokens_found" in score:
            scored = f"{score['ocr_tokens_found'] * 100:.0f}% of the photos' words read"
        elif "recall_at_1" in score:
            scored = f"first {score['recall_at_1'] * 100:.0f}%, top ten {score['recall_at_10'] * 100:.0f}% (among {score['among']})"
        else:
            scored = f"word error rate {score['word_error_rate'] * 100:.1f}%, {score['real_time_factor']:.2f}× real time"
        card = [t["gpu"] for t in r["trials"] if t.get("gpu")]
        use = sorted(c["utilization_median_pct"] for c in card)
        peak = max((c["memory_peak_mib"] for c in card), default=0)
        rows.append(f"| {label} | {model} | {r['configuration']['items']} × {len(r['trials'])} | "
                    f"{s['median_of_trial_medians_s'] * 1000:,.0f} ms ({s['spread_s'][0] * 1000:,.0f}–{s['spread_s'][1] * 1000:,.0f}) | "
                    f"{scored} | {use[len(use) // 2]:.0f}% | {peak / 1024:.1f} GB |" if use else
                    f"| {label} | {model} | {r['configuration']['items']} × {len(r['trials'])} | "
                    f"{s['median_of_trial_medians_s'] * 1000:,.0f} ms | {scored} | – | – |")
    if not rows:
        return []
    return ["| Kind | Model | Items × trials | Time each | Scored | Card use | Card memory, peak |",
            "|---|---|---|---|---|---|---|"] + rows + [""]


laptop, vm = results / "laptop", results / "vm"
pc, box = load(laptop, "names"), load(vm, "names")
out = ["Excavate 0.1.0, 10-11 November 2026. Raw files in [results/laptop](results/laptop) (the everyday PC) and "
       "[results/vm](results/vm) (the virtual machine); the tables below are made from them by "
       "[results_tables.py](results_tables.py).", ""]

# The everyday PC: the figures Excavate shows.
out += ["### On the everyday PC", ""]
if pc:
    q = pc["queries"]
    ours = sorted(row["bedrock_ms"]["median"] for row in q["each"])
    theirs = sorted(row["everything_ms"]["median"] for row in q["each"] if row.get("everything_ms"))
    faster = sum(1 for row in q["each"] if row.get("everything_ms") and row["bedrock_ms"]["median"] < row["everything_ms"]["median"])
    out += ["#### Names: Bedrock and Everything", "",
            f"{pc['bedrock']['facts']['entries']:,} files and folders on its two drives.", ""] + names_table(pc, "drives'")
    out += [f"The {q['queries']} searches, {q['repeat']} times each, until the first 100 results sorted by name are in "
            f"hand: the median of their medians is {ours[len(ours) // 2]:.1f} ms for Bedrock and "
            f"{theirs[len(theirs) // 2]:.1f} ms for Everything. Bedrock is faster on {faster} of {q['queries']}; Everything "
            "by a few milliseconds on searches by date or size and on those with hundreds of thousands of results. The "
            "counts differ by a few files changing while the run went on, except where Bedrock finds more (`ext:mp4`, "
            "`*.jp*g`, `size:>1gb`): files in folders this PC's Everything is set to leave out.", ""] + searches_table(q)
table = reading_table(laptop)
if table:
    out += ["#### Reading", "",
            "From nothing, in a fresh profile each run, with all 32 threads and the graphics card where Excavate puts its "
            "models. Time is per file Excavate reads (recording heard, video looked at), whole runs included; the 95% "
            "interval of the median in brackets. Processor time and peak memory are for a whole run, with the model "
            "servers the indexer starts. The last column: how late a 10 ms timer fired meanwhile (99th percentile), a "
            "measure of how the PC stays responsive.", ""] + table
table = finding_table(laptop)
if table:
    out += ["#### Finding", "", "Balanced. The right file first, and in the top ten, with the share's 95% interval.", ""] + table
table = gpu_table(results / "gpu")
if table:
    out += ["#### On the graphics card", "",
            "The models on the card alone, as described under [The graphics card](#the-graphics-card): the median of "
            "five trials' medians per item, with the trials' spread; a speech item includes loading the model.", ""] + table

# The virtual machine: the same runs, confirming them.
out += ["### Confirmed on the virtual machine", "",
        "The same stages and samples on the clean, small machine: 4 cores, no graphics card, nothing else running.", ""]
if box:
    q = box["queries"]
    out += ["#### Names: Bedrock and Everything", "",
            f"{box['bedrock']['facts']['entries']:,} files and folders on the machine's drive.", ""] + names_table(box, "drive's")
    if pc:
        out += ["**Why two rows differ here.** On this machine Everything used less processor time per change, and its "
                "index was smaller. Both come from the machine's size, "
                f"{box['bedrock']['facts']['entries']:,} files, a hundred times fewer than the everyday PC's. "
                "Everything's time per change grows with the number of files it holds, from "
                f"{med(box['changes']['everything_cpu_us'], 1)} µs here to {med(pc['changes']['everything_cpu_us'], 0)} µs "
                f"there; Bedrock's grows far less, from {med(box['changes']['bedrock_cpu_us'], 1)} µs to "
                f"{med(pc['changes']['bedrock_cpu_us'], 0)} µs. And Bedrock's index has a few megabytes that don't grow "
                "with the files: they show on a small drive and vanish on a big one. Excavate is built for PCs with "
                "millions of files, where Bedrock comes out ahead on both.", ""]
    out += [f"The {q['queries']} searches, {q['repeat']} times each (medians). The counts agree for {q['same_count']} of "
            f"{q['queries']}; where they differ, by a few files that changed while the run went on (Everything's database "
            "and the run's own files).", ""] + searches_table(q)
table = reading_table(vm)
if table:
    out += ["#### Reading", "", "As on the everyday PC, on 4 cores without a graphics card (Windows' own timer steps by "
            "15.6 ms here, so up to that much late in the last column is the timer, not the load).", ""] + table
table = finding_table(vm)
if table:
    out += ["#### Finding", "", "Balanced, as on the everyday PC.", ""] + table

sys.stdout.reconfigure(encoding="utf-8")
print("\n".join(out))
