//! yt-transcribe: turn speech into text, anchored in time.
//!
//! Three kinds of input, one decode core:
//!
//! * a **YouTube URL** -- captions first (never transcribe what YouTube already
//!   transcribed); with no caption track, the audio is streamed over HTTP Range
//!   into a custom AVIOContext and decoded;
//! * a **network media URL** -- any `http(s)` address: a direct media file, or a
//!   playlist (HLS/DASH), which FFmpeg opens itself so the demuxer can fetch
//!   every segment;
//! * a **local media file** (.mp4, .m4a, .webm, .mkv, .avi, ...) -- decoded
//!   straight from disk.
//!
//! Either way: FFmpeg (statically linked) -> PCM 16 kHz mono -> Parakeet TDT
//! 0.6B v3 (ONNX) -> the cue registry, from which `.srt` and `.txt` are derived.
//!
//! The same binary **serves the MCP tools**: `yt-transcribe mcp` over stdio, or
//! `--transport http` for systemd. The transcription itself lives in [`run`], so the terminal and
//! the agent walk exactly the same path.

mod asr;
mod audio;
mod ingest;
mod mcp;
mod output;
mod run;
mod task;

use clap::{Parser, Subcommand};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
#[command(name = "yt-transcribe", version, about)]
struct Cli {
    /// A YouTube URL, an http(s) media URL (direct file or HLS/DASH playlist),
    /// or a path to a local media file.
    ///
    /// Not marked `required_unless_present`: measured 2026-10-09, neither the field name
    /// (`command`) nor the subcommand name (`mcp`) satisfies it -- clap checks that requirement
    /// apart from the subcommand match, so a client spawning `yt-transcribe mcp` would meet a
    /// usage error and the server would die at once. The missing input is reported by name
    /// instead, where the message can say what was expected.
    input: Option<String>,

    /// Preferred caption language; when omitted, the video's own default track
    /// is used (so a Portuguese video yields Portuguese captions).
    #[arg(short, long)]
    lang: Option<String>,

    /// Output directory.
    #[arg(short, long, default_value = ".")]
    output: PathBuf,

    /// Model directory (encoder/decoder_joint ONNX + vocab.txt).
    #[arg(short, long, default_value = "models/tdt")]
    model: PathBuf,

    /// Ignore existing captions and always transcribe the audio.
    #[arg(long)]
    force_transcribe: bool,

    /// A Netscape `cookies.txt` holding an account session. It reaches content
    /// that requires a login (age-restricted, members-only, private playlists)
    /// and raises the request ceiling that a long queue of videos runs into.
    /// Export it from an incognito window; see the yt-dlp wiki, "Extractors".
    #[arg(long, value_name = "FILE")]
    cookies: Option<PathBuf>,

    /// Transcribe from the first block again, ignoring what is already recorded.
    #[arg(long)]
    force: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the MCP tools: stdio (the client spawns it) or HTTP (systemd keeps it alive).
    Mcp {
        /// `stdio` or `http`.
        #[arg(long, default_value = "stdio")]
        transport: String,
        /// Address for `--transport http`.
        #[arg(long, default_value = "127.0.0.1:9730")]
        address: String,
    },
}

/// Where to look for the ONNX model, in order: the `--model` value, then the
/// system and per-user data dirs (used by the .deb install), then the crate's
/// own default.
pub(crate) fn resolve_model(cli_model: &Path) -> PathBuf {
    if cli_model.is_dir() {
        return cli_model.to_path_buf();
    }
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("YT_TRANSCRIBE_MODEL") {
        candidates.push(PathBuf::from(dir));
    }
    candidates.push(PathBuf::from("/usr/share/yt-transcribe/models/tdt"));
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/share/yt-transcribe/models/tdt"));
    }
    for c in candidates {
        if c.is_dir() {
            return c;
        }
    }
    cli_model.to_path_buf()
}

fn main() {
    let cli = Cli::parse();

    if let Some(Command::Mcp { transport, address }) = cli.command {
        std::process::exit(serve_mcp(&transport, &address));
    }

    if let Err(e) = transcribe_cli(&cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Host of an `http(s)` URL, lower-cased, without userinfo or port.
///
/// Deliberately minimal: its only job is telling YouTube apart from every other
/// network URL, so it has to recognise `youtube.com` correctly and stay
/// predictable for the rest. It is not a URL parser.
pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

pub(crate) fn is_youtube_url(url: &str) -> bool {
    matches!(
        url_host(url).as_deref(),
        Some(
            "youtube.com"
                | "www.youtube.com"
                | "m.youtube.com"
                | "music.youtube.com"
                | "youtu.be"
        )
    )
}

pub(crate) fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Output name for a network URL: the last path segment without its extension
/// (`.../index.m3u8` -> `index`, `.../video.mp4` -> `video`); the host when the
/// path carries no usable name. Deterministic, and it never collides with the
/// YouTube route, which names its files after the video id.
pub(crate) fn url_stem(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let stem = last.rsplit_once('.').map_or(last, |(stem, _)| stem);
    if !stem.is_empty() {
        return stem.to_string();
    }
    url_host(url).unwrap_or_else(|| "output".to_string())
}

/// Wrap a decode failure with what the tool expected from the input, so a wrong
/// URL names the expectation instead of leaking a raw FFmpeg error code.
pub(crate) fn media_error(input: &str, err: Box<dyn Error>) -> Box<dyn Error> {
    format!(
        "cannot read {input} as media: {err}\n\
         expected a media file (.mp4/.m4a/.mkv/.webm/.avi/...) or an HLS/DASH playlist"
    )
    .into()
}

/// Transcribe from the command line, and report where everything landed.
fn transcribe_cli(cli: &Cli) -> Result<(), Box<dyn Error>> {
    let model = resolve_model(&cli.model);
    std::fs::create_dir_all(&cli.output)?;

    // Loaded once, before any network work: an unreadable cookie file has to fail
    // here, where the message can name the file, and not midway through a queue.
    // Only the count is ever reported -- never a value.
    let cookies = match cli.cookies.as_deref() {
        Some(path) => {
            let jar = ingest::cookies::Jar::load(path)?;
            eprintln!("cookies: {} entry(ies)", jar.len());
            Some(jar)
        }
        None => None,
    };

    let source = cli
        .input
        .clone()
        .ok_or("no input given: pass a YouTube URL, an http(s) media URL, or a file")?;
    let (stem, kind) = run::classify(&source)?;
    match kind {
        "youtube" => eprintln!("video: {stem}"),
        "network" => eprintln!("url: {source} ({stem})"),
        _ => eprintln!("file: {source} ({stem})"),
    }

    let options = run::Options {
        model: &model,
        dir: &cli.output,
        lang: cli.lang.as_deref(),
        cookies: cookies.as_ref(),
        force_transcribe: cli.force_transcribe,
        force: cli.force,
        verbose: true,
        dump_url: std::env::var("YT_DUMP_URL").is_ok(),
    };
    // No signal handler on purpose: Ctrl-C kills the process, the registry already holds every
    // finished block, and the next run picks up from there.
    let cancel = AtomicBool::new(false);
    let outcome = run::transcribe(&source, &options, &cancel)?;

    eprintln!(
        "transcribed: wrote {} ({} cues) and {}",
        outcome.srt.display(),
        outcome.task.cues,
        outcome.txt.display()
    );
    println!("registry  {}", outcome.cues_path.display());
    println!(
        "state     {} -- {:.1} s of audio{}",
        outcome.task.state,
        outcome.task.seconds,
        match &outcome.task.language {
            Some(language) => format!(", language {language}"),
            None => String::new(),
        }
    );
    Ok(())
}

/// Starts the MCP server on the requested transport.
///
/// Over stdio, nothing here may write to stdout: stdout is the protocol channel, and a stray
/// `println!` corrupts the conversation with the client. Diagnostics go to stderr, in both modes.
fn serve_mcp(transport: &str, address: &str) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: could not start the async runtime: {e}");
            return 2;
        }
    };

    let result = match transport {
        "stdio" => rt.block_on(mcp::serve()),
        "http" => rt.block_on(mcp::serve_http(address)),
        other => {
            eprintln!("error: unknown transport: {other:?} -- use \"stdio\" or \"http\"");
            return 2;
        }
    };

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error in the MCP server: {e}");
            1
        }
    }
}
