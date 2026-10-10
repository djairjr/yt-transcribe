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
# Read from the system, never typed by hand: a package labelled amd64 holding an arm64 binary is
# worse than no package at all.
ARCH=$(dpkg --print-architecture)
PKG="yt-transcribe_${VERSION}_${ARCH}"
BIN=target/release/yt-transcribe

if [ ! -x "$BIN" ]; then
    echo "error: $BIN missing -- run: bash scripts/build.sh build --release" >&2
    exit 1
fi

# Guard against the class of error that already happened in this family: a document inside the
# artifact citing a version other than the one being built (the manual shipped 0.1.0 once, a README
# pointed at an older package). Cargo.toml is the source of truth; a document that disagrees stops
# the build. The pattern deliberately matches the PROGRAM package only -- the model package carries
# the weights' own version, which is not the program's.
grep -q "yt-transcribe_${VERSION}_" README.md || {
    echo "error: README.md does not cite the package yt-transcribe_${VERSION}_ -- update it" >&2
    exit 1
}
stale=$(grep -HoE "yt-transcribe_[0-9]+\.[0-9]+\.[0-9]+" README.md | grep -v "$VERSION" || true)
if [ -n "$stale" ]; then
    echo "error: README cites a version other than $VERSION:" >&2
    echo "$stale" >&2
    exit 1
fi

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/yt-transcribe-deb.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT

install -Dm755 "$BIN" "$STAGE/usr/bin/yt-transcribe"
# The user unit, not a system one: the service reads the user's data directory (and their cookie
# export, if any), which a system service running as root would not find.
install -Dm644 packaging/yt-transcribe.service "$STAGE/usr/lib/systemd/user/yt-transcribe.service"
install -Dm644 README.md "$STAGE/usr/share/doc/yt-transcribe/README.md"
install -Dm644 LICENSE "$STAGE/usr/share/doc/yt-transcribe/LICENSE"
# The Debian convention: the licenses of everything the package carries, including the
# statically linked LGPL FFmpeg.
install -Dm644 packaging/copyright "$STAGE/usr/share/doc/yt-transcribe/copyright"
install -Dm644 scripts/fetch-model.sh "$STAGE/usr/share/doc/yt-transcribe/fetch-model.sh"
# An EXAMPLE, never a configuration in force: the .deb installs nothing under ~/.config or /etc, so
# the file the user's runs read is always one the user wrote.
install -Dm644 packaging/config.example.json "$STAGE/usr/share/doc/yt-transcribe/config.example.json"

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
