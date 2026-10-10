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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Parser)]
#[command(
    name = "yt-transcribe",
    version,
    about,
    after_help = "\
Exit codes, one meaning for the whole family:
  0  the work is done and nothing needs a person
  1  something needs a person -- a cancelled run, or a record carrying an error
  2  the work could not be done

Ctrl-C asks the run to stop between blocks: the block in flight finishes and is recorded, the
record is left as `cancelled`, and the exit code is 1. A second Ctrl-C refuses to wait and exits 2.
Nothing already recorded is redone, so running the same command again resumes from there."
)]
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

/// The exit codes, and there is one meaning for the whole family — a script that calls this tool or
/// its sibling should not have to know which one it called:
///
/// * `0` the work is done and nothing needs a person;
/// * `1` something needs a person: a cancelled run (the registry keeps every finished block), or a
///   record that carries an error;
/// * `2` the work could not be done — nothing was attempted, or nothing came out of it.
const NOTHING_TO_READ: i32 = 0;
const READ_ME: i32 = 1;
const COULD_NOT_WORK: i32 = 2;

fn main() {
    let cli = Cli::parse();

    if let Some(Command::Mcp { transport, address }) = cli.command {
        std::process::exit(serve_mcp(&transport, &address));
    }

    std::process::exit(transcribe_cli(&cli));
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

/// How many interrupts this process has taken. A `static` because a signal handler can reach nothing
/// else; it is only ever touched through [`handle_interrupt`].
static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

/// The stop a signal asks for: set by the handler, read by the run between ASR blocks.
static STOP: AtomicBool = AtomicBool::new(false);

/// What an interrupt asks for.
///
/// Kept apart from the signal itself so the instrument can be tested without one: a handler whose
/// only test is "send yourself a SIGINT and hope" would take the whole test suite down with it when
/// it fails.
#[derive(Debug, PartialEq, Eq)]
enum Interrupt {
    /// The first `Ctrl-C`: finish the block in flight, record it, and leave code 1.
    StopBetweenBlocks,
    /// The second: refuse to wait any longer, leave code 2 now.
    LeaveNow,
}

/// The body of the handler, taking its state by reference so that a test can own it.
///
/// Measured 2026-10-10: the record is written **per block**, so two blocks are on disk while the
/// third runs. That is what makes the second interrupt safe to honour at once -- the finished work
/// is already recorded, and only `state` would stay `running`.
fn handle_interrupt(seen: &AtomicUsize, stop: &AtomicBool) -> Interrupt {
    if seen.fetch_add(1, Ordering::SeqCst) == 0 {
        stop.store(true, Ordering::SeqCst);
        Interrupt::StopBetweenBlocks
    } else {
        Interrupt::LeaveNow
    }
}

/// Async-signal-safe by construction: it sets a flag and, on the second interrupt, calls `_exit`.
///
/// It writes nothing. Writing a file is not async-signal-safe, and nothing here needs it: the
/// record follows the work, so the block in flight finishes on the normal path and is on disk
/// before this process leaves.
extern "C" fn on_sigint(_signal: i32) {
    if handle_interrupt(&INTERRUPTS, &STOP) == Interrupt::LeaveNow {
        unsafe { libc::_exit(COULD_NOT_WORK) }
    }
}

/// Make `Ctrl-C` ask the run to stop between blocks instead of killing it mid-block.
///
/// Only the command line installs this. The MCP server must not: its stop is the `cancel` tool, and
/// a server that caught SIGINT would ignore what systemd uses to take it down.
fn install_signal_handler() {
    // SAFETY: `on_sigint` is an `extern "C"` function with the single `int` parameter `signal(2)`
    // requires, and `signal` only stores the pointer -- it never calls into Rust here.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}

/// Transcribe from the command line, report where everything landed, and say how it went in the
/// exit code.
fn transcribe_cli(cli: &Cli) -> i32 {
    let model = resolve_model(&cli.model);
    if let Err(e) = std::fs::create_dir_all(&cli.output) {
        eprintln!("error: could not use {} as the output directory: {e}", cli.output.display());
        return COULD_NOT_WORK;
    }

    // Loaded once, before any network work: an unreadable cookie file has to fail
    // here, where the message can name the file, and not midway through a queue.
    // Only the count is ever reported -- never a value.
    let cookies = match cli.cookies.as_deref() {
        Some(path) => match ingest::cookies::Jar::load(path) {
            Ok(jar) => {
                eprintln!("cookies: {} entry(ies)", jar.len());
                Some(jar)
            }
            Err(e) => {
                eprintln!("error: {e}");
                return COULD_NOT_WORK;
            }
        },
        None => None,
    };

    let Some(source) = cli.input.clone() else {
        eprintln!("error: no input given: pass a YouTube URL, an http(s) media URL, or a file");
        return COULD_NOT_WORK;
    };
    let (stem, kind) = match run::classify(&source) {
        Ok(classified) => classified,
        Err(e) => {
            eprintln!("error: {e}");
            return COULD_NOT_WORK;
        }
    };
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
    // Ctrl-C now asks for a stop instead of killing the process mid-block: the block in flight
    // finishes, the record is written, and the run leaves code 1 with every finished block kept.
    install_signal_handler();
    let outcome = match run::transcribe(&source, &options, &STOP) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("error: {e}");
            return failed_exit(&cli.output, &stem);
        }
    };

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
    exit_for(&outcome.task)
}

/// The code for a run that finished: only a run that came out clean has nothing to look at.
fn exit_for(task: &task::Task) -> i32 {
    if task.state == "finished" && task.error.is_none() {
        NOTHING_TO_READ
    } else {
        READ_ME
    }
}

/// The code for a run that ended in an error.
///
/// The record is read from disk because that is where the run left it: blocks already recorded are
/// something a person may want to look at (and the next run continues from there), while a run that
/// recorded nothing never did the work. Reading it from disk rather than from memory is the same
/// rule `status` follows.
fn failed_exit(dir: &Path, stem: &str) -> i32 {
    match task::read(dir, stem).ok().flatten() {
        Some(t) if t.chunks_done > 0 || t.cues > 0 => READ_ME,
        _ => COULD_NOT_WORK,
    }
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
            return COULD_NOT_WORK;
        }
    };

    let result = match transport {
        "stdio" => rt.block_on(mcp::serve()),
        "http" => rt.block_on(mcp::serve_http(address)),
        other => {
            eprintln!("error: unknown transport: {other:?} -- use \"stdio\" or \"http\"");
            return COULD_NOT_WORK;
        }
    };

    match result {
        Ok(()) => NOTHING_TO_READ,
        Err(e) => {
            // A server that died did not do its work; there is no residue for a person here.
            eprintln!("error in the MCP server: {e}");
            COULD_NOT_WORK
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(stem: &str, state: &str, error: Option<&str>, cues: usize, chunks_done: usize) -> task::Task {
        task::Task {
            stem: stem.to_string(),
            state: state.to_string(),
            error: error.map(str::to_string),
            cues,
            chunks_done,
            ..Default::default()
        }
    }

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yt-transcribe-exit-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_clean_finished_run_has_nothing_to_read() {
        assert_eq!(exit_for(&record("doc", "finished", None, 12, 3)), NOTHING_TO_READ);
    }

    #[test]
    fn a_cancelled_run_keeps_its_blocks_and_asks_for_a_look() {
        // The registry is not rolled back, so the partial transcript is exactly what a person
        // (or the next run) wants to see.
        assert_eq!(exit_for(&record("doc", "cancelled", None, 4, 2)), READ_ME);
    }

    #[test]
    fn a_record_carrying_an_error_asks_for_a_look() {
        assert_eq!(exit_for(&record("doc", "finished", Some("boom"), 3, 1)), READ_ME);
    }

    #[test]
    fn a_run_that_recorded_nothing_could_not_work() {
        let dir = fresh_dir("nothing");
        // No record on disk at all: the run died before it wrote one.
        assert_eq!(failed_exit(&dir, "nothing"), COULD_NOT_WORK);
        // A record that never got a single block down.
        task::write(&dir, &record("nothing", "failed", Some("decode"), 0, 0)).unwrap();
        assert_eq!(failed_exit(&dir, "nothing"), COULD_NOT_WORK);
    }

    #[test]
    fn a_run_that_recorded_blocks_has_something_to_look_at() {
        let dir = fresh_dir("partial");
        task::write(&dir, &record("partial", "failed", Some("network"), 0, 2)).unwrap();
        assert_eq!(failed_exit(&dir, "partial"), READ_ME);
    }

    #[test]
    fn the_first_interrupt_arms_the_flag_the_run_reads_between_blocks() {
        // The instrument for the stop request, without a signal in sight: this test process would
        // not survive its own failure.
        let seen = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        assert_eq!(handle_interrupt(&seen, &stop), Interrupt::StopBetweenBlocks);
        assert!(stop.load(Ordering::SeqCst));
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_second_interrupt_refuses_to_wait() {
        let seen = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        assert_eq!(handle_interrupt(&seen, &stop), Interrupt::StopBetweenBlocks);
        assert_eq!(handle_interrupt(&seen, &stop), Interrupt::LeaveNow);
    }

    #[test]
    fn the_stop_the_handler_arms_is_what_makes_the_run_ask_for_a_look() {
        // Writer and reader of the same contract in one test: the signal arms the flag, the run
        // turns that flag into `cancelled`, and a `cancelled` record is code 1 -- not code 0.
        let seen = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        handle_interrupt(&seen, &stop);
        let mut stopped = record("doc", "running", None, 4, 2);
        if stop.load(Ordering::SeqCst) {
            stopped.state = "cancelled".to_string();
        }
        assert_eq!(exit_for(&stopped), READ_ME);
    }

    #[test]
    fn the_handler_installs_without_signal_rejecting_it() {
        // A `sighandler_t` of the wrong shape is what a hand-declared `signal` gets wrong, and the
        // mistake is silent until a signal arrives. Installing it here, in the suite, catches it.
        let previous = unsafe { libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t) };
        assert_ne!(previous, libc::SIG_ERR, "signal() rejected the handler");
        unsafe { libc::signal(libc::SIGINT, previous) };
    }
}
