#!/usr/bin/env bash
# Fetches the runtime every model shares into third_party/sherpa-onnx/lib (Linux x64):
# sherpa-onnx's C API and its GPU-capable ONNX Runtime 1.28.2 (Moonshine links it too), the Linux twin of
# fetch-runtime.ps1. No NVIDIA libraries: the GPU needs the system's CUDA 12 and cuDNN 9 (the CPU needs none).
# Re-running skips what is already there.
set -euo pipefail

package=sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-linux-x64-gpu
sha256=2aa541a65c1e7d921a053283419b70b30c1f6ae0a0f52f64b947ebe88c5f2a77
url="https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/$package.tar.bz2"

if [ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]; then
    echo "fetch-runtime.sh is for Linux x64; on Windows use fetch-runtime.ps1" >&2
    exit 1
fi
root="$(cd "$(dirname "$0")" && pwd)/../third_party"
lib="$root/sherpa-onnx/lib"
if [ -e "$lib/libonnxruntime.so" ]; then
    echo "Runtime ready in $root"
    exit 0
fi

mkdir -p "$lib"
archive="$root/$package.tar.bz2"
[ -f "$archive" ] || curl -fL --proto =https --proto-redir =https --retry 4 -o "$archive" "$url"
echo "$sha256  $archive" | sha256sum -c - >&2 || { rm -f "$archive"; exit 1; }
tar -xjf "$archive" -C "$root" "$package/lib"
mv "$root/$package/lib/"* "$lib/"
rm -rf "${root:?}/$package" "$archive"
echo "Runtime ready in $root"
