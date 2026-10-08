#!/usr/bin/env bash
# Download a previously tested YouTube video as a LOCAL mp4, to exercise the
# file path of yt-transcribe. Uses ffmpeg's own HTTP client (it reads in ranges
# and respects the server better than a single curl GET, which gets throttled).
#
# Usage: bash scripts/fetch-local.sh <video-id> <seconds>
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

VID="${1:?video id}"
DUR="${2:?duration seconds}"
FF="$HOME/.hermes/tools/ffmpeg-9.0.1-linux-x64/bin/ffmpeg"
mkdir -p test-data/local

# Resolve an audio-only URL through our own tool (visionos client).
URL=$(YT_DUMP_URL=1 ./target/release/yt-transcribe \
        "https://www.youtube.com/watch?v=$VID" --force-transcribe \
        -o test-data/local 2>/dev/null | head -1)
if [ -z "$URL" ]; then
  echo "could not resolve an audio URL for $VID" >&2
  exit 1
fi
echo "resolved audio URL (${#URL} chars)"

# The media URL was issued to the visionos client, so the request must carry the
# same User-Agent or googlevideo answers 403.
UA="Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 \
(KHTML, like Gecko) Version/26.0 Safari/605.1.15"

# Build a real mp4: synthetic video track + the streamed audio, written locally.
# The ".local." infix keeps this apart from a file yt-dlp may write for the
# same video id, so the two downloaders never overwrite each other.
"$FF" -hide_banner -loglevel warning -y \
  -f lavfi -i "testsrc=size=320x240:rate=4:duration=$DUR" \
  -user_agent "$UA" -i "$URL" \
  -c:v libx264 -preset ultrafast -crf 35 -c:a aac -b:a 96k -shortest \
  "test-data/local/$VID.local.mp4"

echo "== result =="
ls -la "test-data/local/$VID.local.mp4"
"$FF" -hide_banner -i "test-data/local/$VID.local.mp4" 2>&1 | grep -E "Duration|Stream"
