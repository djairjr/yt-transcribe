#!/usr/bin/env bash
# Fetch the Parakeet TDT 0.6B v3 ONNX files into a model directory.
#
# The model is not shipped in the .deb: the int8 set is ~670 MB and is
# separately licensed (CC-BY-4.0). This downloads it once.
#
# Usage: bash scripts/fetch-model.sh [target-dir]   (default models/tdt)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

DIR="${1:-models/tdt}"
BASE="https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main"
mkdir -p "$DIR"

# int8 encoder/decoder: ~670 MB total instead of the ~2.5 GB fp32 set.
for f in encoder-model.int8.onnx decoder_joint-model.int8.onnx nemo128.onnx vocab.txt; do
    if [ -s "$DIR/$f" ]; then
        echo "have $f"
        continue
    fi
    echo "fetching $f"
    curl -sL --fail --retry 3 -o "$DIR/$f" "$BASE/$f"
done

ls -la "$DIR"
