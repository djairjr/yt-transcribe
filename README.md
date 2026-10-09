# yt-transcribe

Speech to subtitles, in one self-contained binary.

`yt-transcribe` takes **a YouTube URL, a local media file, or any http(s) media URL**
(a direct file, or an HLS/DASH playlist) and writes the transcript as timed text:
`<name>.cues.jsonl`, the source of truth; `<name>.srt`, the same text with
timestamps; and `<name>.txt`, plain. A fourth file, `<name>.task.json`, records
the job, which is what lets an interrupted run resume instead of restarting.

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

$ yt-transcribe "https://example.com/stream.m3u8" --output ./out
out/stream.srt
out/stream.txt
```

## Download

Prebuilt binaries are on the
[releases page](https://github.com/djairjr/yt-transcribe/releases):

| Asset | What it is |
|-------|------------|
| `yt-transcribe_<ver>_amd64.deb` | The program and the user systemd unit (Linux x86-64). Installs `/usr/bin/yt-transcribe`. ~19 MB |
| `yt-transcribe-model_<ver>_all.deb` | Parakeet TDT 0.6B v3 ONNX weights (int8). Installs to `/usr/share/yt-transcribe/models/tdt/`. ~432 MB |
| `<package>.sha256` | The checksum of each of the two |

The Windows build is not published as an asset; the recipe is under *Building from
source*.

```bash
sudo dpkg -i yt-transcribe_0.2.0_amd64.deb
sudo dpkg -i yt-transcribe-model_0.2.0_all.deb   # only if you need ASR

sha256sum -c yt-transcribe_0.2.0_amd64.deb.sha256   # optional, against the published checksum
```

The program package also installs the systemd **user** unit, so the MCP server is one command away:

```bash
systemctl --user enable --now yt-transcribe     # serves http://127.0.0.1:9730/mcp
```

The model package lands exactly where the binary looks (see *Model directory*
below), so no `--model` flag or environment variable is needed. A video that
already has subtitles needs only the program package — the captions path uses
no model.

To build from source instead, see *Building from source*.

## Purpose

A model can read text; it cannot watch a video. This tool exists to close that gap — to hand a model
the **textual content of a video**, with the times, so it can answer about what was said and point at
*when*.

That is why the MCP server returns text rather than a path (a path would only send the caller on a
second trip it may not be able to make), why every cue carries its time, and why the transcript lives
in a registry that survives an interrupted run instead of being written once at the end.

Getting there is normally a stack of moving parts: a downloader, a separate media decoder, a separate
ASR runtime, glue code between them. This project collapses that into one binary with one decode core,
so the result is reproducible and portable: one executable, no external media toolchain to install,
no inter-process piping of audio.

The video itself is never downloaded for the YouTube path — only the audio track is streamed and
decoded on the fly.

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
yt-transcribe mcp [--transport stdio|http] [--address 127.0.0.1:9730]

Arguments:
  <INPUT>   A YouTube URL, an http(s) media URL (direct file or HLS/DASH
            playlist), or a path to a local media file.

Options:
  -l, --lang <LANG>       Preferred caption language; when omitted the video's
                          own default track is used.
  -o, --output <DIR>      Output directory.                  [default: .]
  -m, --model <DIR>       Model directory.               [default: models/tdt]
      --force-transcribe  Ignore existing captions and always transcribe audio.
      --cookies <FILE>    A Netscape `cookies.txt` with an account session.
                          See *Account cookies* below.
      --force             Transcribe from the first block again, ignoring what is
                          already recorded.
  -h, --help              Print help.
  -V, --version           Print version.
```

### Output files

Named after the video id (YouTube) or the file stem (local file or URL).

| File | Content |
|------|---------|
| `<name>.cues.jsonl` | **The source of truth**: one cue per line, with its time and the block that produced it. |
| `<name>.srt` | SubRip subtitles, derived from the registry. |
| `<name>.txt` | Plain transcript, derived from the registry. |
| `<name>.task.json` | The job record: state, blocks done, cues, length, language. |

`.srt` and `.txt` are **derived**: the registry is where a transcription lives, and it is also what
makes a run resumable. It is appended to once per 240-second ASR block, and a block lands there only
after being transcribed whole — so re-running a transcript continues from `max(block) + 1` instead of
redoing the file, and a cancelled run keeps everything it finished.

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
`/usr/share/yt-transcribe/models/tdt` (the `yt-transcribe-model` package),
`~/.local/share/yt-transcribe/models/tdt`, then `models/tdt`.

### Source layout

```
src/
├── main.rs                 CLI, the `mcp` subcommand, route dispatch
├── run.rs                  one transcription, end to end (CLI and MCP share it)
├── task.rs                 the job record on disk (<stem>.task.json)
├── mcp.rs                  the MCP server (5 tools, stdio and HTTP)
├── ingest/                 YouTube ingestion (replaces yt-dlp)
│   ├── watch.rs            watch page -> ytInitialPlayerResponse
│   ├── innertube.rs        POST /youtubei/v1/player
│   ├── clients.rs          innertube client table (visionos, ios, android_vr)
│   ├── formats.rs          audio-only format selection, and why there is none
│   ├── captions.rs         caption track fetch + timedtext XML parser
│   ├── cookies.rs          a Netscape cookie file, scoped by host
│   ├── http.rs             the one place the HTTP client is built (UA + cookies)
│   └── stream.rs           HTTP Range GET
├── audio/pcm.rs            FFmpeg + AVIOContext -> PCM 16 kHz mono
├── asr/mod.rs              Parakeet TDT 0.6B v3 (ONNX), chunked, cancellable
└── output/                 the cue registry, and the .srt/.txt derived from it
```

## MCP server

The same binary is the MCP server: `yt-transcribe mcp` speaks the protocol over stdio (the client
spawns it), and `--transport http` serves on `http://127.0.0.1:9730/mcp` — the only mode **systemd**
can keep alive, because a stdio server dies with the client that spawned it.

| Tool | What it does |
|------|--------------|
| `echo` | Returns the text with the instant of the call: proof the server answered now, not a cached value. |
| `submit` | Starts a video in the background and returns at once. A job already running is refused, and what was already transcribed is never redone. |
| `status` | Progress, read from the record **on disk**: state, blocks done of total, cues recorded, length. Restarting the service loses nothing. |
| `result` | The transcript as **timed text**, in one of the modes below. |
| `cancel` | Stops a running transcription between ASR blocks, keeping every finished block. |

`result` never returns a path: the agent needs the words to answer with, and a path would force a
second trip to the disk that only the agent can make. It has three modes, all anchored in time —
a 35-minute video is tens of thousands of characters, and returning all of it would burn the context:

| `result` arguments | Returns |
|---|---|
| *(none)* | A summary: cues, length, source (`captions`/`asr`), language when known, first and last cue with their times. Under 1 KB. |
| `term`, `context` | The cues containing the term, each with its time, plus `context` neighbours each side — the neighbours are what make a hit quotable. |
| `from`, `to` | Only the cues in that window, each with `start`, `end` and `at`. |
| `full: true` | The whole text. |

Every cue carries `at` (`12:34`), because the time is exactly what lets an answer point at the video
instead of talking about the text in the abstract.

To register it with Hermes, add it as a hybrid entry — HTTP first, stdio as the fallback when the
service is down:

```yaml
mcp_servers:
  yt-transcribe:
    url: http://127.0.0.1:9730/mcp
    command: /usr/bin/yt-transcribe
    args: [mcp]
    enabled: true
```

The URL is what answers; `command`/`args` are the stdio fallback for when the service is down. To
bring the service up:

```bash
systemctl --user enable --now yt-transcribe
```

A **user** service stops when your last session ends, so an agent that should reach it after you log
out needs lingering:

```bash
loginctl enable-linger "$USER"
```

The service runs the *installed* binary — which is worth checking by the process rather than by the
unit file, since the unit states the intent and the process states the fact:

```bash
readlink -f "/proc/$(systemctl --user show yt-transcribe -p MainPID --value)/exe"
```

### Account cookies

An anonymous YouTube session has a low request ceiling, and a long queue of videos starts failing
partway through it; the account session raises that ceiling. Cookies are also how content that
requires a login is reached — **except** age-restricted video, which needs more than cookies (see
*Notes and limitations*).

```bash
yt-transcribe <url> --cookies ~/.config/yt-transcribe/cookies.txt
```

The file is the **Netscape** format that yt-dlp and the browser extensions write. Export it from an
**incognito window** (log in, open `robots.txt` in that same tab, export, close the window), because
YouTube rotates account cookies on open tabs and a normal-session export stops working. Only the
cookies whose domain matches the host being talked to are ever sent, so a file exported from YouTube
cannot leak to an unrelated media server, and no cookie value is ever logged.

The export ages: when a request stops authenticating, export again.

## Building from source

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

### Tests

```bash
# The HTTP server the network route is tested against: it honours Range, and
# `python3 -m http.server` does not.
python3 scripts/test/http_range_server.py 8090 /path/to/media

# The MCP surface, under a real client (needs the `mcp` package, >= 1.27).
python3 tests/tier2_test.py

# Cancel, resume, and the state surviving the process: needs media with several
# 240-second blocks.
python3 tests/cancel_resume_test.py /path/to/long-media.avi
```

`cargo test` covers the cookie file: the host scoping, the expiry rule, the look-alike host, and
both prefix conventions.

### Debian packages

Two packages, split on purpose — the weights are ~670 MB and separately
licensed, so someone who only wants the binary should not have to pull them:

```bash
bash packaging/build-deb.sh          # yt-transcribe_<ver>_amd64.deb        (~19 MB)
bash packaging/build-deb-model.sh    # yt-transcribe-model_<ver>_all.deb    (~432 MB)

sudo dpkg -i yt-transcribe_0.2.0_amd64.deb
sudo dpkg -i yt-transcribe-model_0.2.0_all.deb   # optional, needs the model

systemctl --user enable --now yt-transcribe      # the MCP server, on 127.0.0.1:9730
```

| Package | Contents | Installs to |
|---------|----------|-------------|
| `yt-transcribe` | the binary, the user systemd unit, and the docs below | `/usr/bin/yt-transcribe`, `/usr/lib/systemd/user/yt-transcribe.service`, `/usr/share/doc/yt-transcribe/` |
| `yt-transcribe-model` | Parakeet TDT 0.6B v3 ONNX (int8), `vocab.txt`, and a README recording its source and CC-BY-4.0 license | `/usr/share/yt-transcribe/models/tdt/`, `/usr/share/doc/yt-transcribe-model/` |

What the program package puts in `/usr/share/doc/yt-transcribe/`: this README, `LICENSE` (MIT, for the
code), and `copyright` — the Debian machine-readable file naming the terms of everything the binary
carries, including the statically linked LGPL FFmpeg.

Because the model package lands exactly where the binary looks (see *Model
directory* below), no `--model` or environment variable is needed: install both
and run `yt-transcribe <url>`. Only the program package is required for the
captions path (a video whose subtitles already exist needs no model).

### Windows (cross-compiled from Linux)

The build produces a 64-bit Windows executable from this Linux host.

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64

bash scripts/build-ffmpeg-win.sh      # static FFmpeg + libs for mingw
bash scripts/build-win.sh             # -> target/x86_64-pc-windows-gnu/release/yt-transcribe.exe
```

Two things differ from the Linux build:

- **ONNX Runtime is dynamic.** `ort` ships a prebuilt runtime only for the MSVC
  target, not for `x86_64-pc-windows-gnu`, so the Windows build uses ort's
  `load-dynamic` mode: the runtime is loaded at run time and `onnxruntime.dll`
  must sit next to the executable.
- **FFmpeg is still static.** The mingw build of FFmpeg is linked into the
  `.exe`.

Drop next to `yt-transcribe.exe`: `onnxruntime.dll` and `models/tdt/`.

## Dependencies

| Crate | Role |
|-------|------|
| `rsmpeg` 0.18 | FFmpeg bindings, linking our own static build. |
| `reqwest` | HTTP (rustls, so no system OpenSSL). |
| `serde_json` | Parsing player/innertube JSON. |
| `parakeet-rs` | Parakeet models on ONNX Runtime. |
| `clap` | CLI. |
| `rmcp` 3.5 | The MCP server, over stdio and Streamable HTTP. |
| `tokio`, `axum` | The async runtime, and the HTTP host for the MCP service. |

FFmpeg 8.1.3 is built from source by `scripts/build-ffmpeg.sh`: static, PIC, the **full** decoder and
demuxer catalogue with the network protocols (`http`, `https`, `tls`, `udp`) and OpenSSL, with
encoders, muxers, filters and devices disabled, and no `--enable-gpl` — it stays LGPL-2.1-or-later.
The full catalogue is what makes "point at a video and read it" hold for any container; the linker
discards what is unused.

## Credits

Standing on these:

| What | Whose | Terms |
|------|-------|-------|
| FFmpeg | the FFmpeg developers | LGPL-2.1-or-later — built from source with no `--enable-gpl`, linked statically. The terms and the relinking note are in `/usr/share/doc/yt-transcribe/copyright` |
| Parakeet TDT 0.6B v3 | NVIDIA | CC-BY-4.0 (the weights; not this code) |
| `parakeet-rs` | its authors | MIT or Apache-2.0 — the ONNX Runtime path |
| `rsmpeg` / `rusty_ffmpeg` | their authors | MIT — the FFmpeg bindings |
| `rmcp` | the modelcontextprotocol project | Apache-2.0 — the MCP server SDK |
| `reqwest`, `tokio`, `axum`, `clap`, `serde` | their authors | MIT or Apache-2.0 |
| The cookie-export method | the yt-dlp wiki, *Extractors* | the incognito-window procedure, and the file format this tool reads |

The code here is MIT — see `LICENSE`.

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
- **Age-restricted video needs more than cookies.** Measured against a signed-in account: the
  browser plays it and the API does not, even with valid cookies and across every innertube client
  tried. `yt-dlp` with the same cookies fails the same way, and its log names what is missing — a
  JavaScript runtime for the `n`/signature challenges, and a GVS PO token. Both are outside this
  build's design (no JS solver, self-contained), so age-restricted content is **out of scope** and
  reported as such rather than silently attempted.
- **`.avi` served over HTTP does not decode.** The same file decodes whole from disk (measured: 95
  minutes, 5690 s, no error). What fails is FFmpeg 8.1's HTTP layer: the demuxer asks for
  `Range: bytes=<size>-`, the server answers `416` (correct per RFC 7233), and the request is
  dropped; the system FFmpeg 9.0.1 reads the same URL from the same server without a word.
