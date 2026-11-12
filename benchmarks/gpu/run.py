"""Repeated trials of Excavate's graphics-card models, in the environment setup.sh makes (WSL2, CUDA), with the same
model files, server settings and prompts Excavate uses. Each trial starts the engine afresh, runs one item to warm
up, then times every item; the environment, the configuration and every timing go into one JSON file.

    /opt/excavate-bench/venv/bin/python run.py <workload> --data <corpus folder> [--trials 5] [--items N]

Workloads:
  ocr-thorough        PaddleOCR-VL-1.5 reads TextVQA photos (Thorough's text in images)
  images-gemma        EmbeddingGemma 2 embeds COCO photos; captions find them (Balanced's images by what they show)
  images-qwen         Qwen3-VL-Embedding-8B, the same (Thorough's most accurate)
  speech-turbo        Whisper large-v3-turbo hears LibriSpeech test-clean (Balanced's speech)
  speech-large        Whisper large-v3, the same (Thorough's speech)

Models download into /opt/excavate-bench/models the first time, checked against the SHA-256 Excavate checks them by.
"""
import argparse
import base64
import hashlib
import json
import os
import platform
import re
import secrets
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time
import urllib.request

import numpy as np
from PIL import Image, ImageOps

ROOT = "/opt/excavate-bench"
LLAMA = f"{ROOT}/llama.cpp/build/bin/llama-server"
WHISPER = f"{ROOT}/whisper.cpp/build/bin/whisper-cli"
MODELS = f"{ROOT}/models"
HF = "https://huggingface.co"

FILES = {
    "PaddleOCR-VL-1.5.gguf": ("299051d54faa065abc505cc39b8383ea338fd3020c775ea3e0ba514a7022328c",
                              f"{HF}/PaddlePaddle/PaddleOCR-VL-1.5-GGUF/resolve/main/PaddleOCR-VL-1.5.gguf"),
    "PaddleOCR-VL-1.5-mmproj.gguf": ("e7f1a72400fba517046f90d964e2fa0f4dac7781ee3b1bc5d2022f5f8cecbd87",
                                     f"{HF}/PaddlePaddle/PaddleOCR-VL-1.5-GGUF/resolve/main/PaddleOCR-VL-1.5-mmproj.gguf"),
    "embeddinggemma-2-Q8_0.gguf": ("2188ac1deca4b77dffefd603c2776a9d76d9d74ec01841392982ebb840b09135",
                                   f"{HF}/ggml-org/embeddinggemma-2-GGUF/resolve/main/embeddinggemma-2-Q8_0.gguf"),
    "mmproj-embeddinggemma-2-Q8_0.gguf": ("0531309472a6bc6f4b146c3736eee7795ec9db1903c25490d3c937490cef6dd8",
                                          f"{HF}/ggml-org/embeddinggemma-2-GGUF/resolve/main/mmproj-embeddinggemma-2-Q8_0.gguf"),
    "Qwen3-VL-Embedding-8B.Q4_K_M.gguf": ("a81ec5d9211f03a51369ab1e858f750eabc84a41bec19e129f3d4a0659faf1c7",
                                          f"{HF}/mradermacher/Qwen3-VL-Embedding-8B-GGUF/resolve/main/Qwen3-VL-Embedding-8B.Q4_K_M.gguf"),
    "Qwen3-VL-Embedding-8B.mmproj-Q8_0.gguf": ("36c44edfbe487f743128e369e53966f2e51e1641e604064f68a443c864b5bb9c",
                                               f"{HF}/mradermacher/Qwen3-VL-Embedding-8B-GGUF/resolve/main/Qwen3-VL-Embedding-8B.mmproj-Q8_0.gguf"),
    "ggml-large-v3-turbo-q8_0.bin": ("317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1",
                                     f"{HF}/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin"),
    "ggml-large-v3-q5_0.bin": ("d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1",
                               f"{HF}/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin"),
    "ggml-silero-v6.2.0.bin": ("2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
                               f"{HF}/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin"),
}

# Excavate's own settings for each (indexer/src/deep.rs, looking.rs, listening.rs).
WORKLOADS = {
    "ocr-thorough": {"files": ["PaddleOCR-VL-1.5.gguf", "PaddleOCR-VL-1.5-mmproj.gguf"], "items": 50,
                     "args": ["-c", "8192", "-np", "1", "-ngl", "99", "--temp", "0"], "prompt": "OCR:", "max_side": 2048,
                     "max_tokens": 3000},
    "images-gemma": {"files": ["embeddinggemma-2-Q8_0.gguf", "mmproj-embeddinggemma-2-Q8_0.gguf"], "items": 200,
                     "args": ["--embedding", "-c", "8192", "-b", "4096", "-ub", "4096", "-np", "1", "-ngl", "99",
                              "--pooling", "mean", "--image-min-tokens", "140", "--image-max-tokens", "140"],
                     "query": "task: search result | query: {}", "max_side": 1024},
    "images-qwen": {"files": ["Qwen3-VL-Embedding-8B.Q4_K_M.gguf", "Qwen3-VL-Embedding-8B.mmproj-Q8_0.gguf"], "items": 200,
                    "args": ["--embedding", "-c", "8192", "-b", "4096", "-ub", "4096", "-np", "1", "-ngl", "99",
                             "--pooling", "last", "--image-max-tokens", "256"],
                    "image_instruction": "Represent the user's input.",
                    "query_instruction": "Retrieve images relevant to the user's query.", "max_side": 1024},
    "speech-turbo": {"files": ["ggml-large-v3-turbo-q8_0.bin", "ggml-silero-v6.2.0.bin"], "items": 100},
    "speech-large": {"files": ["ggml-large-v3-q5_0.bin", "ggml-silero-v6.2.0.bin"], "items": 100},
}


def sh(command):
    try:
        done = subprocess.run(command, capture_output=True, text=True, timeout=60)
        return (done.stdout + done.stderr).strip()
    except Exception as error:  # a tool that isn't there is recorded as such
        return f"unavailable: {error}"


def environment():
    gpu = sh(["nvidia-smi", "--query-gpu=name,driver_version,memory.total,pci.bus_id,power.limit,clocks.max.sm",
              "--format=csv,noheader"])
    smi = sh(["nvidia-smi"])
    cuda_driver = re.search(r"CUDA (?:UMD )?Version: ([\d.]+)", smi)
    windows_driver = re.search(r"KMD Version: ([\d.]+)", smi)
    os_release = dict(line.split("=", 1) for line in open("/etc/os-release").read().splitlines() if "=" in line)
    cpu = next((line.split(":", 1)[1].strip() for line in open("/proc/cpuinfo") if line.startswith("model name")), "")
    wsl = sh(["/mnt/c/Windows/System32/wsl.exe", "--version"]).replace("\x00", "")
    return {
        "gpu": gpu,
        "cuda_driver_api": cuda_driver.group(1) if cuda_driver else None,
        "windows_driver": windows_driver.group(1) if windows_driver else None,
        "cuda_toolkit": sh(["/usr/local/cuda/bin/nvcc", "--version"]).splitlines()[-1:],
        "cuda_runtime_libraries": sorted(os.path.basename(p) for p in os.listdir("/usr/local/cuda/lib64") if p.startswith("libcudart.so.")),
        "llama_cpp_tag": sh(["git", "-C", f"{ROOT}/llama.cpp", "describe", "--tags", "--exact-match"]),
        "llama_cpp": sh([LLAMA, "--version"]).splitlines()[-2:] if os.path.exists(LLAMA) else None,
        "whisper_cpp": sh(["git", "-C", f"{ROOT}/whisper.cpp", "rev-parse", "--short", "HEAD"]),
        "kernel": platform.release(),
        "os": os_release.get("PRETTY_NAME", "").strip('"'),
        "wsl": [line.strip() for line in wsl.splitlines() if line.strip()][:3],
        "cpu": cpu,
        "cores": os.cpu_count(),
        "memory_gb": round(os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") / 2**30, 1),
        "python": platform.python_version(),
    }


def download(url, path, parts=8):
    """In parts at once (one stream can be slow), each resuming where it stopped, then joined into `path`."""
    with urllib.request.urlopen(urllib.request.Request(url, headers={"Range": "bytes=0-0"}), timeout=60) as r:
        size = int(r.headers["Content-Range"].rsplit("/", 1)[1])
    bounds = [(size * i // parts, size * (i + 1) // parts) for i in range(parts)]
    pieces = [f"{path}.part{i}" for i in range(parts)]
    had = lambda i: os.path.getsize(pieces[i]) if os.path.exists(pieces[i]) else 0

    def get(i):
        start, end = bounds[i]
        for attempt in range(10):
            if start + had(i) >= end:
                return
            try:
                request = urllib.request.Request(url, headers={"Range": f"bytes={start + had(i)}-{end - 1}"})
                with urllib.request.urlopen(request, timeout=60) as r, open(pieces[i], "ab") as f:
                    shutil.copyfileobj(r, f, 1 << 20)
            except OSError as error:
                print(f"  part {i}: {error}; again", flush=True)
                time.sleep(5)
        raise SystemExit(f"{os.path.basename(path)}: part {i} didn't finish")

    threads = [threading.Thread(target=get, args=(i,), daemon=True) for i in range(parts)]
    for thread in threads:
        thread.start()
    began = time.time()
    while any(thread.is_alive() for thread in threads):
        time.sleep(30)
        done = sum(had(i) for i in range(parts))
        print(f"  {os.path.basename(path)}: {done / 1e9:.2f} of {size / 1e9:.2f} GB, "
              f"{done / max(1, time.time() - began) / 1e6:.1f} MB/s", flush=True)
    if sum(had(i) for i in range(parts)) != size:
        raise SystemExit(f"{os.path.basename(path)}: incomplete")
    # Joined onto the first piece, each removed once it's in: never twice the file on disk.
    with open(pieces[0], "ab") as joined:
        for piece in pieces[1:]:
            with open(piece, "rb") as f:
                shutil.copyfileobj(f, joined, 1 << 24)
            os.remove(piece)
    os.rename(pieces[0], path)


def fetch(name):
    os.makedirs(MODELS, exist_ok=True)
    path = f"{MODELS}/{name}"
    sha, url = FILES[name]
    if not os.path.exists(path):
        print(f"downloading {name}", flush=True)
        download(url, path)
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 24), b""):
            digest.update(block)
    if digest.hexdigest() != sha:
        raise SystemExit(f"{name}: SHA-256 doesn't match")
    return path


class GpuWatch:
    """The card's utilization and memory every half second while a trial runs."""

    def __init__(self):
        self.samples = []
        self.process = subprocess.Popen(["nvidia-smi", "--query-gpu=utilization.gpu,memory.used", "--format=csv,noheader,nounits",
                                         "-lms", "500"], stdout=subprocess.PIPE, text=True)
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.process.stdout:
            try:
                use, memory = (float(x) for x in line.split(","))
                self.samples.append((use, memory))
            except ValueError:
                pass

    def stop(self):
        self.process.terminate()
        uses = [s[0] for s in self.samples] or [0]
        return {"utilization_median_pct": statistics.median(uses), "memory_peak_mib": max((s[1] for s in self.samples), default=0)}


class Server:
    def __init__(self, model, mmproj, args):
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            self.port = s.getsockname()[1]
        self.key = secrets.token_hex(16)
        started = time.perf_counter()
        self.process = subprocess.Popen([LLAMA, "-m", model, "--mmproj", mmproj, *args, "--host", "127.0.0.1",
                                         "--port", str(self.port), "--no-webui", "--log-disable", "--api-key", self.key],
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        while True:
            try:
                if json.loads(self.get("health")).get("status") == "ok":
                    break
            except Exception:
                pass
            if self.process.poll() is not None:
                raise SystemExit("llama-server ended while loading")
            time.sleep(0.2)
        self.load_s = time.perf_counter() - started
        self.marker = json.loads(self.get("props")).get("media_marker", "<__media__>")

    def get(self, path):
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}/{path}", headers={"Authorization": f"Bearer {self.key}"})
        return urllib.request.urlopen(request, timeout=10).read()

    def post(self, path, body):
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}/{path}", data=json.dumps(body).encode(),
                                         headers={"Authorization": f"Bearer {self.key}", "Content-Type": "application/json"})
        return json.loads(urllib.request.urlopen(request, timeout=600).read())

    def stop(self):
        self.process.terminate()
        self.process.wait()


def ppm(path, max_side):
    """The image as Excavate sends it: the right way up, at most `max_side`, uncompressed."""
    image = ImageOps.exif_transpose(Image.open(path)).convert("RGB")
    image.thumbnail((max_side, max_side))
    head = f"P6\n{image.width} {image.height}\n255\n".encode()
    return base64.b64encode(head + image.tobytes()).decode()


def jsonl(path):
    return [json.loads(line) for line in open(path, encoding="utf-8")]


def vector(answer):
    item = answer[0] if isinstance(answer, list) else answer
    v = np.array(item["embedding"][0] if isinstance(item["embedding"][0], list) else item["embedding"], dtype=np.float32)
    return v / (np.linalg.norm(v) or 1)


def run_ocr(config, items, data):
    model, mmproj = (fetch(name) for name in config["files"])
    server = Server(model, mmproj, config["args"])
    def read(entry):
        body = {"messages": [{"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": "data:image/x-portable-pixmap;base64," + ppm(f"{data}/{entry['file']}", config["max_side"])}},
            {"type": "text", "text": config["prompt"]}]}], "temperature": 0, "max_tokens": config["max_tokens"]}
        started = time.perf_counter()
        answer = server.post("v1/chat/completions", body)
        return time.perf_counter() - started, answer["choices"][0]["message"]["content"], answer.get("usage", {}).get("completion_tokens")
    read(items[0])
    watch = GpuWatch()
    rows = []
    for entry in items:
        seconds, text, tokens = read(entry)
        truth = {t.lower() for t in entry["ocr_tokens"] if len(t) > 1}
        found = sum(1 for t in truth if t in text.lower())
        rows.append({"file": entry["file"], "seconds": seconds, "tokens": tokens, "truth_tokens": len(truth), "found": found})
    gpu = watch.stop()
    server.stop()
    found = sum(r["found"] for r in rows) / max(1, sum(r["truth_tokens"] for r in rows))
    return {"load_s": server.load_s, "gpu": gpu, "items": rows, "score": {"ocr_tokens_found": found}}


def run_images(config, items, data, name):
    model, mmproj = (fetch(file) for file in config["files"])
    server = Server(model, mmproj, config["args"])
    qwen = name == "images-qwen"
    def template(instruction, text):
        return f"<|im_start|>system\n{instruction}<|im_end|>\n<|im_start|>user\n{text}<|im_end|>\n<|im_start|>assistant\n"
    def embed_image(entry):
        prompt = template(config["image_instruction"], server.marker) if qwen else server.marker
        body = {"content": [{"prompt_string": prompt, "multimodal_data": [ppm(f"{data}/{entry['file']}", config["max_side"])]}]}
        started = time.perf_counter()
        v = vector(server.post("embeddings", body))
        return time.perf_counter() - started, v
    def embed_text(text):
        prompt = template(config["query_instruction"], text) if qwen else config["query"].format(text)
        return vector(server.post("embeddings", {"content": prompt}))
    embed_image(items[0])
    watch = GpuWatch()
    rows, images = [], []
    for entry in items:
        seconds, v = embed_image(entry)
        rows.append({"file": entry["file"], "seconds": seconds})
        images.append(v)
    gpu = watch.stop()
    queries = np.stack([embed_text(entry["captions"][0]) for entry in items])
    server.stop()
    ranks = []
    scores = queries @ np.stack(images).T
    for i, row in enumerate(scores):
        ranks.append(int((row > row[i]).sum()) + 1)
    return {"load_s": server.load_s, "gpu": gpu, "items": rows,
            "score": {"recall_at_1": sum(r == 1 for r in ranks) / len(ranks), "recall_at_10": sum(r <= 10 for r in ranks) / len(ranks),
                      "among": len(items), "query": "each photo's first COCO caption"}}


def normalize(text):
    return " ".join(re.sub(r"[^\w' ]+", " ", text.lower()).split())


def run_speech(config, items, data, name):
    import jiwer
    model, vad = (fetch(file) for file in config["files"])
    work = f"{ROOT}/work"
    os.makedirs(work, exist_ok=True)
    def hear(entry):
        wav = f"{work}/{os.path.basename(entry['file'])}.wav"
        if not os.path.exists(wav):
            subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", f"{data}/{entry['file']}", "-ar", "16000", "-ac", "1", wav], check=True)
        seconds_audio = os.path.getsize(wav) / 32000
        started = time.perf_counter()
        out = subprocess.run([WHISPER, "-m", model, "-f", wav, "-l", "en", "-nt", "--vad", "-vm", vad, "-np"],
                             capture_output=True, text=True, check=True).stdout
        return time.perf_counter() - started, seconds_audio, out
    hear(items[0])
    watch = GpuWatch()
    rows, hypotheses, references = [], [], []
    for entry in items:
        seconds, audio, text = hear(entry)
        rows.append({"file": entry["file"], "seconds": seconds, "audio_seconds": audio})
        hypotheses.append(normalize(text))
        references.append(normalize(entry["transcript"]))
    gpu = watch.stop()
    return {"load_s": None, "gpu": gpu, "items": rows,
            "score": {"word_error_rate": jiwer.wer(references, hypotheses),
                      "real_time_factor": sum(r["seconds"] for r in rows) / sum(r["audio_seconds"] for r in rows)},
            "note": "each file is a separate whisper-cli run, so every item includes loading the model"}


def items_for(name, data, count):
    truth = f"{data}/truth"
    if name == "ocr-thorough":
        rows = [r for r in jsonl(f"{truth}/textvqa.jsonl") if len(r["ocr_tokens"]) >= 3]
    elif name.startswith("images"):
        rows = jsonl(f"{truth}/coco.jsonl")
    else:
        lines = open(f"{truth}/librispeech.tsv", encoding="utf-8").read().splitlines()[1:]
        rows = [dict(zip(["file", "language", "transcript"], line.split("\t"))) for line in lines if "test-clean" in line]
    rows.sort(key=lambda r: r["file"])
    return rows[:count]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("workload", choices=WORKLOADS)
    parser.add_argument("--data", required=True, help="the corpus folder (with truth/)")
    parser.add_argument("--trials", type=int, default=5)
    parser.add_argument("--items", type=int)
    parser.add_argument("--out", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "results", "gpu"))
    parser.add_argument("--fetch-only", action="store_true", help="download and check the workload's models, then stop")
    args = parser.parse_args()
    config = WORKLOADS[args.workload]
    if args.fetch_only:
        for name in config["files"]:
            fetch(name)
            print(f"{name}: ready", flush=True)
        return
    items = items_for(args.workload, args.data, args.items or config["items"])
    report = {"workload": args.workload, "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "environment": environment(),
              "configuration": {**config, "items": len(items), "trials": args.trials,
                                "model_sha256": {f: FILES[f][0] for f in config["files"]}},
              "trials": []}
    for trial in range(args.trials):
        print(f"{args.workload}: trial {trial + 1} of {args.trials}", flush=True)
        if args.workload == "ocr-thorough":
            result = run_ocr(config, items, args.data)
        elif args.workload.startswith("images"):
            result = run_images(config, items, args.data, args.workload)
        else:
            result = run_speech(config, items, args.data, args.workload)
        times = [r["seconds"] for r in result["items"]]
        result["summary"] = {"median_s": statistics.median(times), "p90_s": float(np.percentile(times, 90)), "total_s": sum(times)}
        print(f"  median {result['summary']['median_s']:.3f} s an item; {result['score']}", flush=True)
        report["trials"].append(result)
    medians = [t["summary"]["median_s"] for t in report["trials"]]
    report["summary"] = {"median_of_trial_medians_s": statistics.median(medians),
                         "spread_s": [min(medians), max(medians)], "score": report["trials"][-1]["score"]}
    os.makedirs(args.out, exist_ok=True)
    path = os.path.join(args.out, f"{args.workload}-{time.strftime('%Y%m%d-%H%M%S')}.json")
    json.dump(report, open(path, "w", encoding="utf-8"), indent=1)
    print(f"written: {path}")


if __name__ == "__main__":
    sys.exit(main())
