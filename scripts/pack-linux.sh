#!/usr/bin/env bash
# Builds kiku-engine for Linux x64 in release mode and packs target/dist/kiku-engine-<version>-linux-x64.tar.gz,
# which unpacks to kiku-engine/: the binary, the ONNX Runtime and sherpa-onnx libraries it finds through $ORIGIN,
# HOSTING.md and the licences. Run on Ubuntu 22.04 (scripts/linux-build.Dockerfile) after fetch-runtime.sh and
# build-moonshine.sh. Paths of this machine are kept out of the binary, as scripts/build-release.ps1 does.
set -euo pipefail

if [ "$(uname -s)-$(uname -m)" != "Linux-x86_64" ]; then
    echo "pack-linux.sh is for Linux x64" >&2
    exit 1
fi
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' Cargo.toml | tr -d '\r')"
[ -n "$version" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
lib="$root/third_party/sherpa-onnx/lib"
dist="$root/target/dist"
package="$dist/kiku-engine"

# Panic locations and C's __FILE__ name no folder of this machine. Encoded flags, since paths may hold spaces.
printf -v CARGO_ENCODED_RUSTFLAGS '%s\x1f%s\x1f%s' \
    "--remap-path-prefix=$root=kiku" "--remap-path-prefix=$cargo_home=cargo" "--remap-path-prefix=$rustup_home=rustup"
export CARGO_ENCODED_RUSTFLAGS
export CFLAGS="-ffile-prefix-map=$root/= -ffile-prefix-map=$cargo_home/=" CXXFLAGS="-ffile-prefix-map=$root/= -ffile-prefix-map=$cargo_home/="
export CARGO_TARGET_DIR="$root/target/release-build"
cargo build --release -j 6 -p kiku-core --bin kiku-engine

rm -rf "$package"
mkdir -p "$package"
cp "$CARGO_TARGET_DIR/release/kiku-engine" "$package/"
# Every library but TensorRT's provider, which nothing here loads (and which needs TensorRT installed).
for file in "$lib"/*.so*; do
    case "$(basename "$file")" in *tensorrt*) ;; *) cp "$file" "$package/" ;; esac
done
cp HOSTING.md "$package/HOSTING.md"
cp LICENSE "$package/LICENSE.txt"
python3 scripts/notices.py "$package/THIRD-PARTY-NOTICES.txt" --engine

# Everything the binary links is in the package or on any system; the providers load CUDA only when asked.
missing="$(ldd "$package/kiku-engine" | grep 'not found' || true)"
if [ -n "$missing" ]; then
    echo "kiku-engine needs libraries the package lacks:" >&2
    echo "$missing" >&2
    exit 1
fi
"$package/kiku-engine" --models > /dev/null

chmod -R u=rwX,go=rX "$package"
chmod 755 "$package/kiku-engine"
tarball="$dist/kiku-engine-$version-linux-x64.tar.gz"
rm -f "$dist"/kiku-engine-*-linux-x64.tar.gz
tar -C "$dist" --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -czf "$tarball" kiku-engine
rm -rf "$package"
echo "$tarball"
