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
mod config;
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
Nothing already recorded is redone, so running the same command again resumes from there.

Configuration, when there is one, is read from --config, then $YT_TRANSCRIBE_CONFIG, then
$XDG_CONFIG_HOME/yt-transcribe/config.json (~/.config/yt-transcribe/config.json), then
/etc/yt-transcribe/config.json. Every key is optional, none of them is required, and a run with no
file behaves exactly as it did before the file existed. `yt-transcribe config show` says what was
read and which step decided each value."
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

    /// Output directory. Default: `.`, or the `output` key of the configuration file.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Model directory (encoder/decoder_joint ONNX + vocab.txt). Default: the first of
    /// `$YT_TRANSCRIBE_MODEL`, the `model` key of the configuration file,
    /// `/usr/share/yt-transcribe/models/tdt`, `~/.local/share/yt-transcribe/models/tdt`
    /// and `models/tdt` that exists.
    #[arg(short, long)]
    model: Option<PathBuf>,

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

    /// Configuration file. Default: the first of `$YT_TRANSCRIBE_CONFIG`,
    /// `$XDG_CONFIG_HOME/yt-transcribe/config.json`, `~/.config/yt-transcribe/config.json`
    /// and `/etc/yt-transcribe/config.json` that exists. Naming one that is not there is an error.
    ///
    /// `global`, so it is accepted after a subcommand too: `config show --config <path>` is how the
    /// family contract writes it.
    #[arg(long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

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
    /// What the program really read, and which step decided each value.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show the configuration file in force and the value in force for every key.
    Show,
}

/// Everything the process reads from outside itself, read once and passed down.
///
/// The rule the family follows: no function of the logic reads an environment variable on its own.
/// The binary reads them here, and everything below takes them as parameters -- which is exactly
/// what makes the chain testable without mutating the process environment (a trap: it breaks under
/// parallel tests).
pub(crate) struct World {
    pub env: config::Environment,
    /// `$YT_TRANSCRIBE_MODEL`.
    pub model: Option<PathBuf>,
    /// `$YT_TRANSCRIBE_DIR` -- the server's counterpart of `--output`.
    pub dir: Option<PathBuf>,
    /// `$YT_COOKIES` -- the server's counterpart of `--cookies`.
    pub cookies: Option<PathBuf>,
}

impl World {
    /// Reads the process's REAL environment. Only the binary calls this.
    pub(crate) fn real() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            env: config::Environment::real(),
            model: var("YT_TRANSCRIBE_MODEL"),
            dir: var("YT_TRANSCRIBE_DIR"),
            cookies: var("YT_COOKIES"),
        }
    }
}

/// Where a value came from. The chain is flag > environment > file > default, and `config show`
/// reports which step decided each value, so a person can see WHY they got what they got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Flag,
    Environment,
    File,
    Default,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Flag => "flag",
            Origin::Environment => "environment",
            Origin::File => "config file",
            Origin::Default => "default",
        }
    }
}

/// The output directory a command-line run uses when nothing else says so.
const DEFAULT_OUTPUT: &str = ".";
/// Where the crate looks for the model when nothing else says so.
const DEFAULT_MODEL: &str = "models/tdt";

/// One key's walk down the chain: the flag, then the environment, then the file, then the built-in
/// default. One function, so the run and `config show` can never disagree about the order.
fn pick<T: Clone>(flag: Option<T>, env: Option<T>, file: Option<T>, default: T) -> (T, Origin) {
    if let Some(v) = flag {
        return (v, Origin::Flag);
    }
    if let Some(v) = env {
        return (v, Origin::Environment);
    }
    if let Some(v) = file {
        return (v, Origin::File);
    }
    (default, Origin::Default)
}

/// Like [`pick`], for a key whose absence is a value: nothing declared means `None`, and the
/// origin says so.
fn pick_opt<T: Clone>(flag: Option<T>, env: Option<T>, file: Option<T>) -> (Option<T>, Origin) {
    if let Some(v) = flag {
        return (Some(v), Origin::Flag);
    }
    if let Some(v) = env {
        return (Some(v), Origin::Environment);
    }
    if let Some(v) = file {
        return (Some(v), Origin::File);
    }
    (None, Origin::Default)
}

/// A switch has no "not declared" state on the command line (there is no `--no-force-transcribe`),
/// so the chain is a decision rather than a lookup: the flag asks for it, the file may ask for it,
/// and nothing asks for it.
fn pick_switch(flag: bool, file: Option<bool>) -> (bool, Origin) {
    if flag {
        return (true, Origin::Flag);
    }
    match file {
        Some(v) => (v, Origin::File),
        None => (false, Origin::Default),
    }
}

/// The output directory for the server, when the caller and the environment are both silent.
pub(crate) fn data_dir(env: &config::Environment) -> PathBuf {
    env.home
        .as_ref()
        .map(|home| home.join(".local/share/yt-transcribe"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where to look for the ONNX model, in order: `--model` when it is a directory, then
/// `$YT_TRANSCRIBE_MODEL`, then the file's `model`, then the system and per-user data dirs (where
/// the .deb installs it), then the crate's own default.
///
/// A value that is not a directory falls through rather than failing here: that is what this tool
/// has always done with a model path that is not there, and the run reports the missing directory
/// where it matters, naming what it looked for.
pub(crate) fn resolve_model(
    world: &World,
    flag: Option<&Path>,
    file: Option<&Path>,
) -> (PathBuf, Origin) {
    if let Some(p) = flag.filter(|p| p.is_dir()) {
        return (p.to_path_buf(), Origin::Flag);
    }
    if let Some(p) = world.model.clone().filter(|p| p.is_dir()) {
        return (p, Origin::Environment);
    }
    if let Some(p) = file.filter(|p| p.is_dir()) {
        return (p.to_path_buf(), Origin::File);
    }
    let mut candidates = vec![PathBuf::from("/usr/share/yt-transcribe/models/tdt")];
    if let Some(home) = world.env.home.as_ref() {
        candidates.push(home.join(".local/share/yt-transcribe/models/tdt"));
    }
    if let Some(c) = candidates.into_iter().find(|c| c.is_dir()) {
        return (c, Origin::Default);
    }
    (
        flag.or(file).unwrap_or(Path::new(DEFAULT_MODEL)).to_path_buf(),
        Origin::Default,
    )
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

    match &cli.command {
        Some(Command::Mcp { transport, address }) => {
            std::process::exit(serve_mcp(transport, address));
        }
        Some(Command::Config { action }) => {
            std::process::exit(config_cli(&cli, action));
        }
        None => {}
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

/// The values a run really uses, each with the step that decided it.
#[derive(Debug, PartialEq)]
pub(crate) struct Effective {
    pub lang: Option<String>,
    pub lang_origin: Origin,
    pub output: PathBuf,
    pub output_origin: Origin,
    pub model: PathBuf,
    pub model_origin: Origin,
    pub cookies: Option<PathBuf>,
    pub cookies_origin: Origin,
    pub force_transcribe: bool,
    pub force_transcribe_origin: Origin,
    pub force: bool,
    pub force_origin: Origin,
}

/// Walk the whole chain for a command-line run.
fn effective_cli(cli: &Cli, world: &World, file: &config::Config) -> Effective {
    let (lang, lang_origin) = pick_opt(cli.lang.clone(), None, file.lang.clone());
    let (output, output_origin) = pick(
        cli.output.clone(),
        None,
        file.output.clone(),
        PathBuf::from(DEFAULT_OUTPUT),
    );
    let (model, model_origin) = resolve_model(world, cli.model.as_deref(), file.model.as_deref());
    let (cookies, cookies_origin) = pick_opt(cli.cookies.clone(), None, file.cookies.clone());
    let (force_transcribe, force_transcribe_origin) =
        pick_switch(cli.force_transcribe, file.force_transcribe);
    let (force, force_origin) = pick_switch(cli.force, file.force);

    Effective {
        lang,
        lang_origin,
        output,
        output_origin,
        model,
        model_origin,
        cookies,
        cookies_origin,
        force_transcribe,
        force_transcribe_origin,
        force,
        force_origin,
    }
}

/// Walk the same chain for the server, where the caller's arguments take the place of the flags.
///
/// One chain, two frontes: the server must not invent a second precedence, or `status` and the
/// command line would disagree about where a transcript lands on the same machine.
pub(crate) fn effective_mcp(
    world: &World,
    file: &config::Config,
    dir: Option<&str>,
    lang: Option<&str>,
    force: Option<bool>,
) -> Effective {
    let caller_dir = dir
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    let (lang, lang_origin) = pick_opt(lang.map(str::to_string), None, file.lang.clone());
    let (output, output_origin) = pick(
        caller_dir,
        world.dir.clone(),
        file.output.clone(),
        data_dir(&world.env),
    );
    let (model, model_origin) = resolve_model(world, None, file.model.as_deref());
    let (cookies, cookies_origin) = pick_opt(world.cookies.clone(), None, file.cookies.clone());
    let (force, force_origin) = pick_switch(force.unwrap_or(false), file.force);

    Effective {
        lang,
        lang_origin,
        output,
        output_origin,
        model,
        model_origin,
        cookies,
        cookies_origin,
        // The server has no switch for this: `submit` never asks to ignore captions.
        force_transcribe: false,
        force_transcribe_origin: Origin::Default,
        force,
        force_origin,
    }
}

/// The configuration file in force, or an empty one when no file exists anywhere.
///
/// An empty configuration is not a degraded state: every key is optional, so the defaults below it
/// in the chain are exactly what a machine without a file has always used.
pub(crate) fn load_config(
    explicit: Option<&Path>,
    world: &World,
) -> Result<config::Config, Box<dyn Error>> {
    match config::resolve(explicit, &world.env)? {
        Some((_, cfg)) => Ok(cfg),
        None => Ok(config::Config::default()),
    }
}

/// `config show`: what the program really read, and which step decided each value.
fn config_cli(cli: &Cli, action: &ConfigAction) -> i32 {
    match action {
        ConfigAction::Show => {
            let world = World::real();
            let found = match config::resolve(cli.config.as_deref(), &world.env) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("error: {e}");
                    return COULD_NOT_WORK;
                }
            };
            match &found {
                Some((path, _)) => println!("file       {}", path.display()),
                None => println!("file       none -- the built-in defaults are in force"),
            }
            println!("looked in:");
            for p in config::candidates(cli.config.as_deref(), &world.env) {
                println!("             {}", p.display());
            }

            let file = found.map(|(_, cfg)| cfg).unwrap_or_default();
            let plan = effective_cli(cli, &world, &file);
            let show = |name: &str, value: String, origin: Origin| {
                println!("{name:<17}{value}  ({})", origin.label())
            };
            show("lang", plan.lang.unwrap_or_else(|| "none".to_string()), plan.lang_origin);
            show("output", plan.output.display().to_string(), plan.output_origin);
            show("model", plan.model.display().to_string(), plan.model_origin);
            show(
                "cookies",
                plan.cookies
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".to_string()),
                plan.cookies_origin,
            );
            show(
                "force_transcribe",
                plan.force_transcribe.to_string(),
                plan.force_transcribe_origin,
            );
            show("force", plan.force.to_string(), plan.force_origin);
            NOTHING_TO_READ
        }
    }
}

/// Transcribe from the command line, report where everything landed, and say how it went in the
/// exit code.
fn transcribe_cli(cli: &Cli) -> i32 {
    let world = World::real();
    let file = match load_config(cli.config.as_deref(), &world) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("error: {e}");
            return COULD_NOT_WORK;
        }
    };
    let plan = effective_cli(cli, &world, &file);

    if let Err(e) = std::fs::create_dir_all(&plan.output) {
        eprintln!(
            "error: could not use {} as the output directory: {e}",
            plan.output.display()
        );
        return COULD_NOT_WORK;
    }

    // Loaded once, before any network work: an unreadable cookie file has to fail
    // here, where the message can name the file, and not midway through a queue.
    // Only the count is ever reported -- never a value.
    let cookies = match plan.cookies.as_deref() {
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
        model: &plan.model,
        dir: &plan.output,
        lang: plan.lang.as_deref(),
        cookies: cookies.as_ref(),
        force_transcribe: plan.force_transcribe,
        force: plan.force,
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
            return failed_exit(&plan.output, &stem);
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

    // -----------------------------------------------------------------------
    // The configuration chain. Every test here owns its `World`: nothing reads
    // the process environment, which is what makes them safe in parallel.
    // -----------------------------------------------------------------------

    fn fake_world() -> World {
        World {
            env: config::Environment {
                home: Some(PathBuf::from("/home/fake")),
                xdg_config_home: None,
                env_config: None,
            },
            model: None,
            dir: None,
            cookies: None,
        }
    }

    /// A parsed command line, with an input these tests never touch.
    fn cli_with(args: &[&str]) -> Cli {
        let mut full = vec!["yt-transcribe", "https://www.youtube.com/watch?v=x"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    fn a_directory(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yt-transcribe-chain-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_chain_is_flag_then_file_then_default() {
        let world = fake_world();
        let file = config::Config {
            output: Some(PathBuf::from("/from/file")),
            ..Default::default()
        };

        let by_flag = effective_cli(&cli_with(&["-o", "/from/flag"]), &world, &file);
        assert_eq!(by_flag.output, PathBuf::from("/from/flag"));
        assert_eq!(by_flag.output_origin, Origin::Flag);

        let by_file = effective_cli(&cli_with(&[]), &world, &file);
        assert_eq!(by_file.output, PathBuf::from("/from/file"));
        assert_eq!(by_file.output_origin, Origin::File);

        let by_default = effective_cli(&cli_with(&[]), &world, &config::Config::default());
        assert_eq!(by_default.output, PathBuf::from(DEFAULT_OUTPUT));
        assert_eq!(by_default.output_origin, Origin::Default);
    }

    #[test]
    fn the_environment_sits_between_the_flag_and_the_file_for_the_model() {
        let env_model = a_directory("env-model");
        let file_model = a_directory("file-model");
        let flag_model = a_directory("flag-model");
        let world = World {
            model: Some(env_model.clone()),
            ..fake_world()
        };
        let file = config::Config {
            model: Some(file_model.clone()),
            ..Default::default()
        };

        let by_flag = effective_cli(&cli_with(&["-m", flag_model.to_str().unwrap()]), &world, &file);
        assert_eq!(by_flag.model, flag_model);
        assert_eq!(by_flag.model_origin, Origin::Flag);

        let by_env = effective_cli(&cli_with(&[]), &world, &file);
        assert_eq!(by_env.model, env_model);
        assert_eq!(by_env.model_origin, Origin::Environment);

        let by_file = effective_cli(&cli_with(&[]), &fake_world(), &file);
        assert_eq!(by_file.model, file_model);
        assert_eq!(by_file.model_origin, Origin::File);
    }

    #[test]
    fn a_model_path_that_is_not_a_directory_does_not_stop_the_chain() {
        // The tool has always read a non-existent --model as "not this one": the run fails later,
        // where the model is needed, naming what it looked for.
        let file = config::Config {
            model: Some(a_directory("fallthrough-file-model")),
            ..Default::default()
        };
        let plan = effective_cli(&cli_with(&["-m", "/no/such/model"]), &fake_world(), &file);
        assert_ne!(plan.model, PathBuf::from("/no/such/model"));
        assert_eq!(plan.model_origin, Origin::File);
    }

    #[test]
    fn a_switch_is_the_flag_or_the_file_and_nothing_else() {
        let world = fake_world();
        let with_flag = effective_cli(&cli_with(&["--force"]), &world, &config::Config::default());
        assert!(with_flag.force && with_flag.force_origin == Origin::Flag);

        let from_file = effective_cli(
            &cli_with(&[]),
            &world,
            &config::Config {
                force: Some(true),
                ..Default::default()
            },
        );
        assert!(from_file.force && from_file.force_origin == Origin::File);

        let from_nothing = effective_cli(&cli_with(&[]), &world, &config::Config::default());
        assert!(!from_nothing.force && from_nothing.force_origin == Origin::Default);
    }

    #[test]
    fn the_server_walks_the_same_chain() {
        // The caller's own arguments come first (they are this front's flags), then the
        // environment, then the file, then the data directory: one chain, two frontes.
        let from_file = config::Config {
            output: Some(PathBuf::from("/from/file")),
            ..Default::default()
        };

        let caller = effective_mcp(
            &fake_world(),
            &config::Config::default(),
            Some("/from/caller"),
            None,
            None,
        );
        assert_eq!(caller.output, PathBuf::from("/from/caller"));
        assert_eq!(caller.output_origin, Origin::Flag);

        let by_env = World {
            dir: Some(PathBuf::from("/from/env")),
            ..fake_world()
        };
        let by_env = effective_mcp(&by_env, &from_file, None, None, None);
        assert_eq!(by_env.output, PathBuf::from("/from/env"));
        assert_eq!(by_env.output_origin, Origin::Environment);

        let by_file = effective_mcp(&fake_world(), &from_file, None, None, None);
        assert_eq!(by_file.output, PathBuf::from("/from/file"));
        assert_eq!(by_file.output_origin, Origin::File);

        let by_default = effective_mcp(&fake_world(), &config::Config::default(), None, None, None);
        assert_eq!(
            by_default.output,
            PathBuf::from("/home/fake/.local/share/yt-transcribe")
        );
        assert_eq!(by_default.output_origin, Origin::Default);
    }

    #[test]
    fn a_run_with_no_file_reads_the_same_values_as_before_the_file_existed() {
        // CA-U2.3, as a unit: no file anywhere -> the output is `.`, both switches are off, and
        // nothing about this run depends on the configuration existing.
        let plan = effective_cli(&cli_with(&[]), &fake_world(), &config::Config::default());
        assert_eq!(plan.output, PathBuf::from("."));
        assert!(!plan.force && !plan.force_transcribe);
        assert!(plan.lang.is_none() && plan.cookies.is_none());
    }
}
