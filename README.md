# yt-transcribe

Speech to subtitles, in one self-contained binary.

`yt-transcribe` takes **a YouTube URL or a local media file** and writes two
files: `<id>.txt` (the plain transcript) and `<id>.srt` (the same text with
timestamps).

It does not shell out to `yt-dlp`, does not need a JavaScript runtime, and does
not need Python at run time. The media decoder (FFmpeg) is linked statically
into the executable; the speech model runs in-process on ONNX Runtime.

```
$ yt-transcribe "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
dQw4w9WgXcQ.srt
dQw4w9WgXcQ.txt

$ yt-transcribe ./talk.mp4 --output ./out
out/talk.srt
out/talk.txt
```

## Purpose

Getting a transcript out of a video is normally a stack of moving parts: a
downloader, a separate media decoder, a separate ASR runtime, glue code between
them. This project collapses that into one binary with one decode core, so the
result is reproducible and portable: one executable, no external media
toolchain to install, no inter-process piping of audio.

The video itself is never downloaded for the YouTube path — only the audio
track is streamed and decoded on the fly.

## How it works

### YouTube URL

1. **Captions first.** If the video already carries a subtitle track, it is
   fetched and written out — the tool never transcribes what YouTube already
   transcribed. The track is chosen by the video's own default language (so a
   Portuguese video yields Portuguese captions) unless `--lang` asks otherwise.
2. **Otherwise, transcribe.** The audio-only format is resolved through the
   innertube player API, streamed over HTTP `Range` requests, decoded to
   16 kHz mono PCM, and passed to the ASR model.

### Local media file

The file is decoded straight from disk through the same decode core. No
caption lookup (a raw file has none); it always transcribes the audio.

### Decode core

Audio arrives at FFmpeg through a **custom `AVIOContext`**, not through a
protocol handler: for the YouTube path the read callback pulls HTTP `Range`
slices on demand; for a local file it reads the file handle. Either way the
demuxer works from a byte provider the application controls, which is also why
an `.m4a` whose `moov` atom sits at the end of the file decodes correctly
(seeking is handled, including `AVSEEK_SIZE`/`SEEK_END`).

## Schema

### CLI

```
yt-transcribe <INPUT> [OPTIONS]

Arguments:
  <INPUT>   A YouTube URL, or a path to a local media file.

Options:
  -l, --lang <LANG>       Preferred caption language; when omitted the video's
                          own default track is used.
  -o, --output <DIR>      Output directory.                  [default: .]
  -m, --model <DIR>       Model directory.               [default: models/tdt]
      --force-transcribe  Ignore existing captions and always transcribe audio.
  -h, --help              Print help.
  -V, --version           Print version.
```

### Output files

Named after the video id (YouTube) or the file stem (local file).

| File | Content |
|------|---------|
| `<name>.txt` | Plain transcript, one paragraph. |
| `<name>.srt` | SubRip subtitles, one cue per sentence/line. |

An `.srt` cue:

```
1
00:00:01,040 --> 00:00:04,000
Alright, so here we are, one of the uh elephants.
```

### Model directory

The ONNX model lives in a directory holding:

| File | Role |
|------|------|
| `encoder-model.int8.onnx` | Parakeet TDT encoder (int8). |
| `decoder_joint-model.int8.onnx` | Decoder + joint network (int8). |
| `nemo128.onnx` | NeMo mel preprocessor. |
| `vocab.txt` | Token vocabulary. |

Lookup order: `--model` value, then `$YT_TRANSCRIBE_MODEL`,
`/usr/share/yt-transcribe/models/tdt`, `~/.local/share/yt-transcribe/models/tdt`,
then `models/tdt`.

### Source layout

```
src/
├── main.rs                 CLI and the two input paths
├── ingest/                 YouTube ingestion (replaces yt-dlp)
│   ├── watch.rs            watch page -> ytInitialPlayerResponse
│   ├── innertube.rs        POST /youtubei/v1/player
│   ├── clients.rs          innertube client table (visionos, ios, android_vr)
│   ├── formats.rs          audio-only format selection
│   ├── captions.rs         caption track fetch + timedtext XML parser
│   └── stream.rs           HTTP Range GET
├── audio/pcm.rs            FFmpeg + AVIOContext -> PCM 16 kHz mono
├── asr/mod.rs              Parakeet TDT 0.6B v3 (ONNX), chunked
└── output/                 .srt and .txt writers
```

## Installation

### Build from source

Prerequisites: a C toolchain, `nasm`/`yasm`, `clang` (bindgen), and Rust.

```bash
# 1. Fetch the model (~670 MB, int8). Not shipped with the binary.
bash scripts/fetch-model.sh

# 2. Build the statically linked FFmpeg (audio-only, LGPL). ~1 minute.
bash scripts/build-ffmpeg.sh

# 3. Build the binary.
bash scripts/build.sh build --release
```

The `scripts/build.sh` wrapper exports the FFmpeg paths
(`FFMPEG_INCLUDE_DIR`, `FFMPEG_LIBS_DIR`, `FFMPEG_BINDING_PATH`) computed from
the repository root, then runs `cargo`. Use it instead of plain `cargo build`
so no absolute path is baked into any tracked file.

Run:

```bash
./target/release/yt-transcribe "https://www.youtube.com/watch?v=..."
./target/release/yt-transcribe ./video.mp4 --output ./out
```

### Debian package

```bash
bash packaging/build-deb.sh          # produces yt-transcribe_<ver>_amd64.deb
sudo dpkg -i yt-transcribe_<ver>_amd64.deb
```

The package installs the binary as `/usr/bin/yt-transcribe`. The model is not
bundled (size and a separate model license); fetch it with
`scripts/fetch-model.sh`.

## Dependencies

| Crate | Role |
|-------|------|
| `rsmpeg` 0.18 | FFmpeg bindings, linking our own static build. |
| `reqwest` | HTTP (rustls, so no system OpenSSL). |
| `serde_json` | Parsing player/innertube JSON. |
| `parakeet-rs` | Parakeet models on ONNX Runtime. |
| `clap` | CLI. |

FFmpeg 8.1.3 is built from source by `scripts/build-ffmpeg.sh`: static, PIC,
audio-only decoders (AAC, Opus, Vorbis, FLAC, MP3), demuxers (mov, matroska,
ogg, wav, mp3), with `--disable-everything` and no `--enable-gpl` (stays
LGPL-2.1-or-later).

## Notes and limitations

- **Client-dependent ingestion.** The innertube client used for the media URL
  is chosen because it serves the whole file (`visionos`); `android_vr` was
  measured throttling to 403 past the first megabyte. YouTube rotates this
  over time.
- **Caption tracks come from the `ios` client** in this build; other clients
  either omit them or hand out a timedtext URL that returns an empty body.
- **ASR language coverage.** Parakeet TDT 0.6B v3 covers 25 European
  languages with automatic language detection; a language outside that set
  degrades. Music is not speech and transcribes poorly.
- **Model license.** The Parakeet weights are CC-BY-4.0 (NVIDIA). The code
  here is not the model.
