#!/usr/bin/env bash
# Builds the Moonshine speech-to-text runtime from source into third_party/ (Linux and macOS):
# static libraries linked against the ONNX Runtime sherpa-onnx ships, so one runtime serves every model.
# Needs git, git-lfs, CMake and a C++20 compiler, and sherpa-onnx's libraries in
# third_party/sherpa-onnx/lib. Re-running is safe.
set -euo pipefail

tag=v0.1.5
commit=234f60faa0eb388b01cdf7e60aca232af37aefda
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../third_party"
src="$root/moonshine-src"
build="$root/moonshine-build"
case "$(uname -s)" in
    Darwin) runtime="$root/sherpa-onnx/lib/libonnxruntime.dylib" ;;
    *) runtime="$root/sherpa-onnx/lib/libonnxruntime.so" ;;
esac

if [ ! -d "$src" ]; then
    GIT_LFS_SKIP_SMUDGE=1 git clone --depth 1 --branch "$tag" https://github.com/moonshine-ai/moonshine.git "$src"
fi
head="$(git -C "$src" rev-parse HEAD)"
if [ "$head" != "$commit" ]; then
    echo "moonshine-src is at $head, expected $commit ($tag)" >&2
    exit 1
fi
# Of the files in Git LFS only these two sources get compiled; the rest are runtimes for other platforms.
git -C "$src" lfs pull --include "core/cpp-annote/src/community1_cpp_annote_embedded.cpp,core/moonshine-tts/src/zipvoice-voices-data.cpp"

# Our fixes to Moonshine, each applied once.
for patch in "$here"/../patches/moonshine-*.patch; do
    git -C "$src" apply --reverse --check "$patch" 2>/dev/null || git -C "$src" apply "$patch"
done

if [ ! -f "$runtime" ]; then
    echo "the runtime is not fetched yet: $runtime" >&2
    exit 1
fi
cmake -S "$src/core" -B "$build" -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DMOONSHINE_BUILD_STATIC=ON -DMOONSHINE_ORT_LIBRARY="$runtime"
cmake --build "$build" --target moonshine --parallel
