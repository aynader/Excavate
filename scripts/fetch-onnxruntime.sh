#!/bin/bash
# Downloads Microsoft's ONNX Runtime with DirectML (Microsoft.ML.OnnxRuntime.DirectML 1.24.4) and DirectML itself
# (Microsoft.AI.DirectML 1.15.4) from nuget.org, checks them, and unpacks onnxruntime.dll, DirectML.dll and their
# licences into vendor/onnxruntime. The indexer's build copies the two DLLs beside the executables; the indexer loads
# ONNX Runtime from there (Microsoft's build picks the processor's instructions as it runs, so it works without AVX).
# DirectML's package holds every architecture and debug builds (about 200 MB); only the x64 DLL is kept.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p vendor/onnxruntime
cd vendor/onnxruntime
get() { # name version sha256
  local file="$1.$2.nupkg"
  [[ -s "$file" ]] || curl -sSfL -o "$file" "https://api.nuget.org/v3-flatcontainer/$1/$2/$1.$2.nupkg"
  echo "$3  $file" | sha256sum -c -
}
get microsoft.ml.onnxruntime.directml 1.24.4 57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681
get microsoft.ai.directml 1.15.4 4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9
unzip -ojq microsoft.ml.onnxruntime.directml.1.24.4.nupkg runtimes/win-x64/native/onnxruntime.dll
unzip -oq microsoft.ml.onnxruntime.directml.1.24.4.nupkg LICENSE ThirdPartyNotices.txt -d onnxruntime-licence
unzip -ojq microsoft.ai.directml.1.15.4.nupkg bin/x64-win/DirectML.dll
unzip -oq microsoft.ai.directml.1.15.4.nupkg LICENSE.txt ThirdPartyNotices.txt -d directml-licence
rm -f ./*.nupkg
ls -la onnxruntime.dll DirectML.dll
