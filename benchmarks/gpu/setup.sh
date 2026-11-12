#!/usr/bin/env bash
# The GPU benchmarks' environment: a clean Ubuntu 24.04 in WSL2, with the CUDA toolkit and the two engines Excavate
# runs its graphics-card models in, built at the versions Excavate uses. Run once, as root, in a new distribution:
#
#   wsl --install Ubuntu-24.04 --name excavate-gpu --no-launch
#   wsl -d excavate-gpu -u root -- bash /mnt/c/<this folder>/setup.sh
#
# The graphics driver is Windows' own (WSL2 passes the card through); only the CUDA toolkit is installed here.
set -euo pipefail

LLAMA_TAG=b12001             # llama.cpp, as Excavate downloads it
WHISPER_COMMIT=d1be6fd       # whisper.cpp, as tools/listen builds it
# Only what the two builds need (the whole toolkit adds 3 GB of profilers).
CUDA_PACKAGES="cuda-nvcc-12-9 cuda-cudart-dev-12-9 libcublas-dev-12-9"
ROOT=/opt/excavate-bench

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential cmake git ca-certificates wget pkg-config \
  python3 python3-venv ffmpeg

# CUDA for WSL, from NVIDIA's WSL-Ubuntu repository (its packages leave out the driver, which is Windows').
if ! command -v /usr/local/cuda-12.9/bin/nvcc >/dev/null 2>&1; then
  wget -q https://developer.download.nvidia.com/compute/cuda/repos/wsl-ubuntu/x86_64/cuda-keyring_1.1-1_all.deb -O /tmp/cuda-keyring.deb
  dpkg -i /tmp/cuda-keyring.deb
  apt-get update
  apt-get install -y --no-install-recommends $CUDA_PACKAGES
fi
export PATH=/usr/local/cuda/bin:$PATH

mkdir -p "$ROOT" && cd "$ROOT"
if [ ! -x llama.cpp/build/bin/llama-server ]; then
  rm -rf llama.cpp
  git clone --depth 1 --branch "$LLAMA_TAG" https://github.com/ggml-org/llama.cpp
  cmake -S llama.cpp -B llama.cpp/build -DGGML_CUDA=ON -DLLAMA_CURL=OFF -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_CUDA_ARCHITECTURES=native
  cmake --build llama.cpp/build -j"$(nproc)" --target llama-server
fi
if [ ! -x whisper.cpp/build/bin/whisper-cli ]; then
  rm -rf whisper.cpp
  git clone https://github.com/ggml-org/whisper.cpp
  git -C whisper.cpp checkout "$WHISPER_COMMIT"
  cmake -S whisper.cpp -B whisper.cpp/build -DGGML_CUDA=ON -DCMAKE_BUILD_TYPE=Release -DCMAKE_CUDA_ARCHITECTURES=native
  cmake --build whisper.cpp/build -j"$(nproc)" --target whisper-cli
fi

python3 -m venv "$ROOT/venv"
"$ROOT/venv/bin/pip" install --quiet numpy==2.1.3 pillow==11.0.0 jiwer==3.0.5

echo "ready: $ROOT"
"$ROOT/llama.cpp/build/bin/llama-server" --version 2>&1 | tail -2
nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv,noheader
nvcc --version | tail -2
