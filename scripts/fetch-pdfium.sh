#!/bin/bash
# Downloads PDFium (Chrome's PDF engine) for Windows x64 from bblanchon/pdfium-binaries, the build pdfium-render 0.9
# targets (chromium/7881), checks it, and unpacks pdfium.dll into vendor/pdfium/bin. The indexer's build copies it
# beside the executables.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p vendor/pdfium
cd vendor/pdfium
url="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881/pdfium-win-x64.tgz"
sum="73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08"
[[ -s pdfium-win-x64.tgz ]] || curl -sSfL -o pdfium-win-x64.tgz "$url"
echo "$sum  pdfium-win-x64.tgz" | sha256sum -c -
tar -xzf pdfium-win-x64.tgz bin/pdfium.dll LICENSE VERSION
ls -la bin/pdfium.dll
