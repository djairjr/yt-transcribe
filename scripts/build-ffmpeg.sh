#!/usr/bin/env bash
#
# Build a minimal, static, audio-only FFmpeg for yt-transcribe.
#
# Produces static libraries (.a) built with -fPIC, so the Rust binary links
# them into a single ELF and needs no libav*.so at runtime.
#
# Licensing: FFmpeg stays LGPL-2.1-or-later. No --enable-gpl, no external
# codec libraries -- every decoder below is FFmpeg's own implementation.
#
# Usage:  bash scripts/build-ffmpeg.sh
# Re-run safe: configure regenerates config.mak each time.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/third_party/ffmpeg"
PREFIX="$ROOT/third_party/ffmpeg-install"
JOBS="$(nproc)"

if [ ! -x "$SRC/configure" ]; then
    echo "error: $SRC/configure not found (is the FFmpeg clone in place?)" >&2
    exit 1
fi

echo "== FFmpeg source ==  $SRC"
echo "== install prefix == $PREFIX"
echo "== jobs ==           $JOBS"
echo "== source version ==  $(git -C "$SRC" describe --tags 2>/dev/null || echo '?')"

cd "$SRC"

# --disable-everything turns off all components; we re-enable only what an
# audio-only transcription path needs. That keeps the static libs small and
# makes the AVIO-custom design (audio arrives in memory, never over a network
# protocol) explicit instead of accidental.
./configure \
    --prefix="$PREFIX" \
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

echo
echo "== static libs installed =="
ls -la "$PREFIX/lib"
