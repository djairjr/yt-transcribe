#!/usr/bin/env bash
# Cross-compile a static, audio-only FFmpeg for Windows (x86_64-pc-windows-gnu).
#
# Same component set as the Linux build (scripts/build-ffmpeg.sh); only the
# toolchain and target change. Output goes to a separate directory so the Linux
# build stays intact.
#
# Usage: bash scripts/build-ffmpeg-win.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/third_party/ffmpeg"
PREFIX="$ROOT/third_party/ffmpeg-win-install"
JOBS="$(nproc)"

CROSS=x86_64-w64-mingw32-
command -v "${CROSS}gcc" >/dev/null || { echo "error: ${CROSS}gcc not found (install gcc-mingw-w64-x86-64)"; exit 1; }

# FFmpeg wants an out-of-tree build directory for a second configuration.
BUILD="$SRC/../ffmpeg-win-build"
mkdir -p "$BUILD"
cd "$BUILD"

echo "== configure (mingw32, static, audio-only) =="
"$SRC/configure" \
    --prefix="$PREFIX" \
    --cross-prefix="$CROSS" \
    --target-os=mingw32 \
    --arch=x86_64 \
    --enable-static \
    --disable-shared \
    --enable-pic \
    --disable-programs \
    --disable-doc \
    --disable-network \
    --disable-autodetect \
    --disable-everything \
    --enable-avcodec \
    --enable-avformat \
    --enable-avutil \
    --enable-swresample \
    --enable-decoder=aac,opus,vorbis,flac,mp3,pcm_s16le \
    --enable-demuxer=mov,matroska,ogg,wav,mp3 \
    --enable-parser=aac,opus,vorbis,flac,mpegaudio \
    --enable-protocol=file

make -j"$JOBS"
make install

echo "== windows static libs =="
ls -la "$PREFIX/lib"
