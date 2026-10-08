#!/usr/bin/env bash
# Build the yt-transcribe-model .deb: the Parakeet TDT 0.6B v3 ONNX weights.
#
# Kept apart from the program package on purpose -- the weights are ~670 MB and
# separately licensed (CC-BY-4.0), so a user who only wants the binary should
# not have to pull them.
#
# Installs into /usr/share/yt-transcribe/models/tdt, which is where the binary
# looks by default (after --model and $YT_TRANSCRIBE_MODEL).
#
# Usage: bash packaging/build-deb-model.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

SRC=models/tdt
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
# The ONNX weights are architecture-independent.
ARCH=all
PKG="yt-transcribe-model_${VERSION}_${ARCH}"
DEST=usr/share/yt-transcribe/models/tdt

[ -d "$SRC" ] || { echo "error: $SRC missing -- run: bash scripts/fetch-model.sh" >&2; exit 1; }

STAGE=$(mktemp -d "${TMPDIR:-/tmp}/yt-transcribe-model-deb.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT

install -d "$STAGE/$DEST"
for f in encoder-model.int8.onnx decoder_joint-model.int8.onnx nemo128.onnx vocab.txt; do
    [ -s "$SRC/$f" ] || { echo "error: $SRC/$f missing -- run: bash scripts/fetch-model.sh" >&2; exit 1; }
    install -Dm644 "$SRC/$f" "$STAGE/$DEST/$f"
done

install -d "$STAGE/usr/share/doc/yt-transcribe-model"
cat > "$STAGE/usr/share/doc/yt-transcribe-model/README" <<'EOF'
Parakeet TDT 0.6B v3 (ONNX, int8) for yt-transcribe.

Source: https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx
Base model: nvidia/parakeet-tdt-0.6b-v3
License: CC-BY-4.0 (the model weights; not the yt-transcribe code)

Installed to /usr/share/yt-transcribe/models/tdt/
EOF

mkdir -p "$STAGE/DEBIAN"
SIZE=$(du -sk "$STAGE/usr" | cut -f1)
cat > "$STAGE/DEBIAN/control" <<EOF
Package: yt-transcribe-model
Version: ${VERSION}
Section: sound
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${SIZE}
Maintainer: Lesords <lesords168@gmail.com>
Recommends: yt-transcribe
Description: Parakeet TDT 0.6B v3 ONNX weights for yt-transcribe
 The NVIDIA Parakeet TDT 0.6B v3 speech model in ONNX (int8) form, installed
 to /usr/share/yt-transcribe/models/tdt/ where yt-transcribe looks for it.
 .
 25 European languages with automatic language detection. Weights are
 CC-BY-4.0; see /usr/share/doc/yt-transcribe-model/README.
EOF

# -Zxz: the .onnx weights compress well and the package is large.
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$PKG.deb"
echo "== built =="
ls -la "$PKG.deb"
dpkg-deb --info "$PKG.deb" | head -14
