#!/usr/bin/env bash
#
# Build helper: exports the FFmpeg paths (computed from this repo) and runs
# cargo. Keeps absolute machine paths out of any versioned config file.
#
# Usage:  bash scripts/build.sh build --release
#         bash scripts/build.sh run -- test-data/sample.m4a
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export FFMPEG_INCLUDE_DIR="$ROOT/third_party/ffmpeg-install-net/include"
export FFMPEG_LIBS_DIR="$ROOT/third_party/ffmpeg-install-net/lib"
# Use a prebuilt binding instead of bindgen-in-build.rs: the generated one came
# out with AVFormatContext opaque (missing its fields), while the crate's own
# shipped binding (same bindgen 0.71.1) has them. See notes in the plan.
export FFMPEG_BINDING_PATH="$ROOT/third_party/ffmpeg-binding/binding.rs"

echo "FFMPEG_INCLUDE_DIR=$FFMPEG_INCLUDE_DIR"
echo "FFMPEG_LIBS_DIR=$FFMPEG_LIBS_DIR"
echo "FFMPEG_BINDING_PATH=$FFMPEG_BINDING_PATH"
exec cargo "$@"
