#!/usr/bin/env bash
# Cross-build yt-transcribe for Windows (x86_64-pc-windows-gnu).
#
# Needs, once:
#   - rustup target add x86_64-pc-windows-gnu
#   - gcc-mingw-w64-x86-64
#   - bash scripts/build-ffmpeg-win.sh      (the Windows FFmpeg .a set)
#
# Usage: bash scripts/build-win.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET=x86_64-pc-windows-gnu
CROSS=x86_64-w64-mingw32-

export FFMPEG_INCLUDE_DIR="$ROOT/third_party/ffmpeg-win-install/include"
export FFMPEG_LIBS_DIR="$ROOT/third_party/ffmpeg-win-install/lib"
export FFMPEG_BINDING_PATH="$ROOT/third_party/ffmpeg-binding/binding.rs"

# cc / bindgen must use the mingw toolchain for the target.
export "CC_${TARGET//-/_}=${CROSS}gcc"
export "CXX_${TARGET//-/_}=${CROSS}g++"
export "AR_${TARGET//-/_}=${CROSS}ar"
export "CARGO_TARGET_${TARGET^^}_LINKER=${CROSS}gcc"

if [ ! -d "$FFMPEG_INCLUDE_DIR" ]; then
    echo "error: Windows FFmpeg not built -- run: bash scripts/build-ffmpeg-win.sh" >&2
    exit 1
fi

echo "target:   $TARGET"
echo "ffmpeg:   $FFMPEG_INCLUDE_DIR"
echo "linker:   ${CROSS}gcc"
exec cargo build --release --target "$TARGET"
