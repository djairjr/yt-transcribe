#!/usr/bin/env bash
#
# Build a static FFmpeg for yt-transcribe that reads everything.
#
# Produces static libraries (.a) built with -fPIC, so the Rust binary links
# them into a single ELF and needs no libav*.so at runtime.
#
# Design: the tool's job is "point at a video and read it". A hand-picked list
# of codecs makes it refuse real files -- no ac3 for that .mkv, no avi demuxer
# for that .avi -- which defeats the purpose. So every decoder, demuxer,
# parser, protocol and bitstream filter is KEPT; only the categories with no
# role in transcription are turned off. Components whose external dependency is
# absent are dropped by configure with a WARNING, not an error.
#
# Licensing: FFmpeg stays LGPL-2.1-or-later. No --enable-gpl. The only external
# libraries are OpenSSL (static TLS for https) and libxml2 (required by the dash
# demuxer); neither of them is a codec.
#
# Usage:  bash scripts/build-ffmpeg.sh
# Re-run safe: configure regenerates config.mak each time.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/third_party/ffmpeg"
# The in-use prefix. Override with FFMPEG_PREFIX to build an alternative
# install (e.g. the network-enabled variant) without touching this one.
PREFIX="${FFMPEG_PREFIX:-$ROOT/third_party/ffmpeg-install}"
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

# Note: no --disable-everything. The catalogs stay on by default; the flags
# below remove whole categories instead of picking names out of them.
./configure \
    --prefix="$PREFIX" \
    --enable-static \
    --disable-shared \
    --enable-pic \
    --disable-programs \
    --disable-doc \
    --disable-autodetect \
    --disable-encoders \
    --disable-muxers \
    --disable-filters \
    --disable-devices \
    --disable-indevs \
    --disable-outdevs \
    --disable-hwaccels \
    --enable-network \
    --enable-openssl \
    --enable-libxml2

# Fail fast. A configure that silently drops the catalogs is worse than one that
# fails outright: it exits 0 and produces a smaller library, and the tool then
# refuses real files. Measured 09/10/2026 -- a malformed enable list did exactly
# that, with no decoders and no demuxers at all. The components below are the
# minimum this tool needs, so their absence aborts the build here.
echo "== verifying the build kept what matters =="
missing=0
for c in CONFIG_HLS_DEMUXER CONFIG_DASH_DEMUXER CONFIG_MPEGTS_DEMUXER \
         CONFIG_MOV_DEMUXER CONFIG_MATROSKA_DEMUXER CONFIG_AVI_DEMUXER \
         CONFIG_HTTP_PROTOCOL CONFIG_HTTPS_PROTOCOL \
         CONFIG_AAC_DECODER CONFIG_AC3_DECODER CONFIG_OPUS_DECODER \
         CONFIG_MP3_DECODER CONFIG_VORBIS_DECODER; do
    grep -q "^#define $c 1" config_components.h || { echo "  MISSING: $c"; missing=1; }
done
if [ "$missing" -ne 0 ]; then
    echo "error: FFmpeg configured without components this tool needs" >&2
    exit 1
fi
echo "  ok"

make -j"$JOBS"
make install

echo
echo "== static libs installed =="
ls -la "$PREFIX/lib"
