#!/usr/bin/env bash
# Test the real binary against videos of different lengths and report facts.
set -u
cd "$(dirname "${BASH_SOURCE[0]}")/.."

BIN=./target/release/yt-transcribe
OUT=test-data/matrix
mkdir -p "$OUT"

# id:seconds
VIDEOS=(
  "jNQXAC9IVRw:19"
  "dQw4w9WgXcQ:213"
  "9bZkp7q19f0:252"
  "6MuAOFfJk2w:313"
  "rJt-x-0Jci4:527"
  "L6c876hyXtk:974"
  "wGCRAH29so4:2121"
)

printf '%-14s %8s %8s %10s %8s  %s\n' ID DUR EXIT PCM_BYTES RESULT NOTE
for entry in "${VIDEOS[@]}"; do
  id="${entry%%:*}"; dur="${entry##*:}"
  rm -f "$OUT/$id.txt" "$OUT/$id.srt"
  log=$("$BIN" "https://www.youtube.com/watch?v=$id" --output "$OUT" 2>&1)
  code=$?
  pcm=$(echo "$log" | grep -oE 'audio decoded: [0-9.]+ s' | grep -oE '[0-9.]+')
  if [ "$code" -ne 0 ]; then
    res="FAIL"; note=$(echo "$log" | grep -iE 'error|403|corrupt|moov' | head -1 | cut -c1-60)
  else
    bytes=$(wc -c < "$OUT/$id.txt")
    words=$(wc -w < "$OUT/$id.txt")
    # Density check: a transcript thinner than ~1 word per 4 s of audio is not a
    # usable result even though the process exited 0. This is the case that the
    # earlier version of this script reported as "OK".
    wps=$(awk "BEGIN{printf \"%.3f\", $words/$dur}")
    if [ "$words" -lt $((dur / 4)) ]; then
      res="SPARSE"; note="${bytes}B ${words}w (${wps} w/s) — suspeito"
    else
      res="OK"; note="${bytes}B ${words}w (${wps} w/s)"
    fi
  fi
  printf '%-14s %8s %8s %10s %8s  %s\n' "$id" "$dur" "$code" "${pcm:-0}" "$res" "$note"
done
