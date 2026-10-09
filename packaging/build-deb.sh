#!/usr/bin/env bash
# Build a .deb for yt-transcribe from the release binary.
#
# The ONNX model is intentionally NOT bundled: it is ~670 MB and separately
# licensed (CC-BY-4.0). scripts/fetch-model.sh downloads it.
#
# Usage: bash packaging/build-deb.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=amd64
PKG="yt-transcribe_${VERSION}_${ARCH}"
BIN=target/release/yt-transcribe

if [ ! -x "$BIN" ]; then
    echo "error: $BIN missing -- run: bash scripts/build.sh build --release" >&2
    exit 1
fi

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/yt-transcribe-deb.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT

install -Dm755 "$BIN" "$STAGE/usr/bin/yt-transcribe"
install -Dm644 README.md "$STAGE/usr/share/doc/yt-transcribe/README.md"
install -Dm644 scripts/fetch-model.sh "$STAGE/usr/share/doc/yt-transcribe/fetch-model.sh"

mkdir -p "$STAGE/DEBIAN"
SIZE=$(du -sk "$STAGE/usr" | cut -f1)
cat > "$STAGE/DEBIAN/control" <<EOF
Package: yt-transcribe
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${SIZE}
Maintainer: Djair Guilherme da Silva Junior <djair.jr@gmail.com>
Description: Speech-to-subtitles from a YouTube URL or a local media file
 Turns a YouTube URL or a local media file into <id>.txt and <id>.srt.
 The media decoder (FFmpeg) is linked statically and the speech model runs
 in-process on ONNX Runtime; no external media toolchain is required.
 .
 The ONNX model is not bundled; fetch it with fetch-model.sh (see docs).
EOF

dpkg-deb --root-owner-group --build "$STAGE" "$PKG.deb"
echo "== built =="
ls -la "$PKG.deb"
dpkg-deb --info "$PKG.deb"
echo "== contents =="
dpkg-deb --contents "$PKG.deb"
