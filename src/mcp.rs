//! The yt-transcribe MCP server.
//!
//! ## The tools, and why each one exists
//!
//! * `echo` — proves the server answered just now, rather than a cached value;
//! * `submit` — starts a video, in the background, and returns at once;
//! * `status` — how a video is going, read from the record on disk;
//! * `result` — the transcript, as **text anchored in time**, in one of three modes;
//! * `cancel` — stops a running video, keeping every block already transcribed.
//!
//! ## Why a task, and not a long call
//!
//! A 35-minute video is minutes of decoding and minutes of recognition. An MCP call that took
//! minutes would be a hung conversation: the client would time out, the agent would have nothing
//! to say, and a dropped connection would throw the work away. So `submit` returns as soon as the
//! job starts, and the state lives **on disk** — the cue registry and the job record — where any
//! later call, in any session, can read it. Restarting the service therefore costs nothing.
//!
//! ## Why the result is not a path
//!
//! The agent needs the words to answer with, and a path would force a second trip to the disk that
//! only the agent can make. So `result` returns the content. But a 35-minute video is on the order
//! of thirty thousand characters, and returning all of it would burn the context: hence three
//! modes, all of them timed — a summary to know what is there, a window to read one stretch, and a
//! search to find where something is said.
//!
//! ## The golden rule of this module
//!
//! Over stdio, **stdout is the protocol**. Any `println!` that is not the transport corrupts the
//! conversation with the client. Everything here — diagnostics, warnings, errors — goes to
//! **stderr**.

use crate::ingest::cookies::Jar;
use crate::output::{Cue, cues as registry, srt};
use crate::run::{self, Options};
use crate::task;
use rmcp::{
    ErrorData as McpError, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
    transport::stdio,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// The parameters of each tool
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct EchoParams {
    /// The text that must come back, unchanged.
    pub text: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SubmitParams {
    /// The video: a YouTube URL, any other http(s) media URL (direct file or HLS/DASH playlist),
    /// or a path to a local media file.
    pub source: String,
    /// Where the transcript is written. If omitted, the configured directory.
    #[serde(default)]
    pub dir: Option<String>,
    /// Transcribe from the first block again, ignoring what is already recorded.
    #[serde(default)]
    pub force: Option<bool>,
    /// Preferred caption language. If omitted, the video's own default track.
    #[serde(default)]
    pub lang: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatusParams {
    /// The job's name: the video id for YouTube, the file stem otherwise.
    pub document: String,
    /// Directory holding the record. If omitted, the configured directory.
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ResultParams {
    /// The job's name: the video id for YouTube, the file stem otherwise.
    pub document: String,
    /// Directory holding the record. If omitted, the configured directory.
    #[serde(default)]
    pub dir: Option<String>,
    /// Return the whole text instead of the summary. Only when the whole text is really wanted:
    /// a 35-minute video is tens of thousands of characters.
    #[serde(default)]
    pub full: Option<bool>,
    /// Window start, in seconds. With `to`, returns only the cues inside the window.
    #[serde(default)]
    pub from: Option<f32>,
    /// Window end, in seconds.
    #[serde(default)]
    pub to: Option<f32>,
    /// A term to search for. Returns the cues that contain it, each with its time.
    #[serde(default)]
    pub term: Option<String>,
    /// How many neighbouring cues to bring alongside each match, for context. Default 1.
    #[serde(default)]
    pub context: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CancelParams {
    /// The job's name: the video id for YouTube, the file stem otherwise.
    pub document: String,
}

// ---------------------------------------------------------------------------
// The jobs that are running
// ---------------------------------------------------------------------------

/// One flag per job, so `cancel` has something to set.
///
/// A `Mutex` and not a channel: written on submit, read on cancel, and never contended — one agent
/// submits a handful of videos in an hour.
#[derive(Default)]
struct Jobs {
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl Jobs {
    fn start(&self, key: &str) -> Option<Arc<AtomicBool>> {
        let mut running = self.running.lock().unwrap();
        if running.contains_key(key) {
            return None;
        }
        let flag = Arc::new(AtomicBool::new(false));
        running.insert(key.to_string(), flag.clone());
        Some(flag)
    }

    fn cancel(&self, key: &str) -> bool {
        match self.running.lock().unwrap().get(key) {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    fn is_running(&self, key: &str) -> bool {
        self.running.lock().unwrap().contains_key(key)
    }

    fn finish(&self, key: &str) {
        self.running.lock().unwrap().remove(key);
    }
}

/// The MCP server. The job table is shared by every session, so a `submit` from one connection can
/// be cancelled from another.
#[derive(Clone)]
pub struct Server {
    jobs: Arc<Jobs>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Jobs::default()),
        }
    }
}

// ---------------------------------------------------------------------------
// Where things live, resolved the same way everywhere
// ---------------------------------------------------------------------------

/// Where transcripts are written when the caller does not say.
///
/// The default is a per-user data directory, so the service can run under systemd (where there is
/// no convenient working directory) and still leave the transcripts somewhere a person can find.
fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("YT_TRANSCRIBE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/share/yt-transcribe");
    }
    PathBuf::from(".")
}

/// The directory for this call: the caller's `dir` wins over the default.
fn output_dir(dir: Option<String>) -> PathBuf {
    match dir.map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => default_dir(),
    }
}

/// The cookie file, when one is configured for the service.
///
/// A service has no command line, so the export is picked up from `YT_COOKIES`. The cookies are
/// what lets a long queue of videos finish without hitting the request ceiling; they are optional,
/// and an unreadable file is a warning, not a failure.
fn cookies() -> Option<Jar> {
    let path = std::env::var("YT_COOKIES").ok()?;
    match Jar::load(Path::new(&path)) {
        Ok(jar) => {
            eprintln!("yt-transcribe mcp: cookies: {} entry(ies)", jar.len());
            Some(jar)
        }
        Err(e) => {
            eprintln!("yt-transcribe mcp: ignoring cookies: {e}");
            None
        }
    }
}

fn model_dir() -> PathBuf {
    crate::resolve_model(Path::new("models/tdt"))
}

fn error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

fn json(value: serde_json::Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("{{}}\n{e}"));
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// A cue as the agent reads it: the time twice — once to compute with, once to say out loud.
fn cue_json(index: usize, cue: &Cue) -> serde_json::Value {
    serde_json::json!({
        "i": index + 1,
        "start": (cue.start * 100.0).round() / 100.0,
        "end": (cue.end * 100.0).round() / 100.0,
        "at": srt::format_short(cue.start as f64),
        "text": cue.text.trim(),
    })
}

fn cues_json(indices: &[usize], cues: &[Cue]) -> Vec<serde_json::Value> {
    indices
        .iter()
        .filter_map(|&i| cues.get(i).map(|c| cue_json(i, c)))
        .collect()
}

/// The current instant in seconds since the epoch, with no date/time crate.
fn now_secs() -> u64 {
    task::now()
}

#[tool_router(server_handler)]
impl Server {
    /// Returns the text it received, together with the instant of the call.
    ///
    /// The instant is not decoration: it is what tells "the server answered just now" apart from
    /// "the client returned a cached value".
    #[tool(
        description = "Returns the received text together with the instant of the call. Living proof that the server answered just now."
    )]
    fn echo(&self, Parameters(EchoParams { text }): Parameters<EchoParams>) -> String {
        format!("{text} | echo at {} | pid {}", now_secs(), std::process::id())
    }

    /// Starts a video, in the background, and returns at once.
    ///
    /// It returns immediately on purpose: decoding plus recognition takes minutes, and a call that
    /// ran for minutes would be a hung conversation. Ask `status` for progress, `result` for the
    /// text, and `cancel` to stop. What was already transcribed is never redone.
    #[tool(
        description = "Starts transcribing a video in the background and returns immediately with the job's name. Accepts a YouTube URL, any http(s) media URL, or a local file path. Ask `status` for progress and `result` for the text. Already-transcribed blocks are skipped, so stopping and starting again costs nothing."
    )]
    async fn submit(
        &self,
        Parameters(SubmitParams {
            source,
            dir,
            force,
            lang,
        }): Parameters<SubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        let (stem, kind) = match run::classify(&source) {
            Ok(pair) => pair,
            Err(e) => return Ok(error(e.to_string())),
        };
        let target_dir = output_dir(dir);
        if let Err(e) = std::fs::create_dir_all(&target_dir) {
            return Ok(error(format!(
                "could not create {}: {e}",
                target_dir.display()
            )));
        }

        let Some(flag) = self.jobs.start(&stem) else {
            return Ok(error(format!(
                "{stem} is already being transcribed. Ask `status` for progress, or `cancel` first."
            )));
        };

        let recorded = registry::read(&registry::path(&target_dir, &stem)).unwrap_or_default();
        let resumed_from = registry::last_chunk(&recorded).map(|c| c + 1).unwrap_or(0);

        let jobs = self.jobs.clone();
        let key = stem.clone();
        let model = model_dir();
        let cookies = cookies();
        let force = force.unwrap_or(false);
        let lang = lang.clone();

        // The job takes copies: the response below still needs the originals.
        let job_source = source.clone();
        let job_dir = target_dir.clone();

        tokio::spawn(async move {
            // The work is blocking (ONNX, HTTP reads): it goes to a blocking thread so it cannot
            // starve the runtime that serves the other sessions. The error comes back as a String
            // because `Box<dyn Error>` is not `Send` and so cannot cross the thread boundary.
            let outcome = tokio::task::spawn_blocking(move || {
                let options = Options {
                    model: &model,
                    dir: &job_dir,
                    lang: lang.as_deref(),
                    cookies: cookies.as_ref(),
                    force_transcribe: false,
                    force,
                    verbose: true,
                    // Never: over stdio, stdout is the protocol.
                    dump_url: false,
                };
                run::transcribe(&job_source, &options, &flag).map_err(|e| e.to_string())
            })
            .await;

            match outcome {
                Ok(Ok(finished)) => eprintln!(
                    "yt-transcribe mcp: {key} finished -- {} cues, {:.1} s ({})",
                    finished.task.cues, finished.task.seconds, finished.task.state
                ),
                Ok(Err(message)) => eprintln!("yt-transcribe mcp: {key} failed: {message}"),
                Err(e) => eprintln!("yt-transcribe mcp: {key} panicked: {e}"),
            }
            jobs.finish(&key);
        });

        Ok(json(serde_json::json!({
            "job": stem,
            "source": source,
            "kind": kind,
            "state": "running",
            "directory": target_dir.display().to_string(),
            "resumed_from_chunk": resumed_from,
        })))
    }

    /// How a video's transcription is going, read from the record on disk.
    ///
    /// There is no in-memory state to consult: the record and the cue registry are the truth,
    /// which is why this answers the same way from any session and why restarting the service
    /// loses nothing.
    #[tool(
        description = "Progress of a transcription: state, blocks done of total, cues recorded, length in seconds. Reads the record and the cue registry on disk; it does not depend on anything running in memory."
    )]
    fn status(
        &self,
        Parameters(StatusParams { document, dir }): Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir_path = output_dir(dir);
        let cues = match registry::read(&registry::path(&dir_path, &document)) {
            Ok(cues) => cues,
            Err(e) => return Ok(error(format!("could not read the cue registry: {e}"))),
        };
        let record = task::read(&dir_path, &document).ok().flatten();

        Ok(json(serde_json::json!({
            "document": document,
            "directory": dir_path.display().to_string(),
            "running": self.jobs.is_running(&document),
            "cues_recorded": cues.len(),
            "record": record,
        })))
    }

    /// The transcript, as text anchored in time — in one of three modes.
    ///
    /// This is where the arrangement pays off, and it returns **content, not a path**: the agent
    /// needs the words to answer with. But a 35-minute video is tens of thousands of characters, so
    /// the default is a summary, and there are two ways to reach into the text — by **time window**
    /// and by **term**. Every cue comes back with its `at`, which is what lets the agent point at
    /// the video instead of talking about the text in the abstract.
    #[tool(
        description = "The transcript as timed text. No arguments beyond `document`: a summary (cues, length, first and last cue, under 1 KB). `term`: the cues containing it, each with its time, plus `context` neighbours each side. `from`/`to` in seconds: only the cues in that window, each with its time. `full: true`: the whole text."
    )]
    fn result(
        &self,
        Parameters(ResultParams {
            document,
            dir,
            full,
            from,
            to,
            term,
            context,
        }): Parameters<ResultParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir_path = output_dir(dir);
        let cues = match registry::read(&registry::path(&dir_path, &document)) {
            Ok(cues) => cues,
            Err(e) => return Ok(error(format!("could not read the cue registry: {e}"))),
        };
        let record = task::read(&dir_path, &document).ok().flatten();

        if cues.is_empty() {
            return Ok(json(serde_json::json!({
                "document": document,
                "directory": dir_path.display().to_string(),
                "cues": 0,
                "note": "nothing transcribed yet -- submit it, or ask `status`",
                "record": record,
            })));
        }

        // Search: where is it said, and what surrounds it.
        if let Some(term) = term.filter(|t| !t.trim().is_empty()) {
            let needle = term.to_lowercase();
            let around = context.unwrap_or(1);

            let mut hits: Vec<usize> = cues
                .iter()
                .enumerate()
                .filter(|(_, c)| c.text.to_lowercase().contains(&needle))
                .map(|(i, _)| i)
                .collect();

            let matches = hits.len();
            // The neighbours are what make a hit quotable: a line pulled out of its sentence says
            // less than the sentence did.
            let mut wanted: Vec<usize> = Vec::new();
            for &h in &hits {
                let lo = h.saturating_sub(around);
                let hi = (h + around).min(cues.len().saturating_sub(1));
                for i in lo..=hi {
                    if !wanted.contains(&i) {
                        wanted.push(i);
                    }
                }
            }
            wanted.sort_unstable();
            hits.retain(|&i| wanted.contains(&i));

            let text = wanted
                .iter()
                .filter_map(|&i| cues.get(i))
                .map(|c| c.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");

            return Ok(json(serde_json::json!({
                "document": document,
                "mode": "search",
                "term": term,
                "matches": matches,
                "cues": cues_json(&wanted, &cues),
                "text": text,
            })));
        }

        // Window: read one stretch, by the clock.
        if from.is_some() || to.is_some() {
            let a = from.unwrap_or(0.0).max(0.0);
            let b = to.unwrap_or(f32::MAX);
            let window: Vec<usize> = cues
                .iter()
                .enumerate()
                .filter(|(_, c)| c.end >= a && c.start <= b)
                .map(|(i, _)| i)
                .collect();
            let seconds = window
                .iter()
                .filter_map(|&i| cues.get(i))
                .map(|c| (c.end - c.start).max(0.0))
                .sum::<f32>();
            let text = window
                .iter()
                .filter_map(|&i| cues.get(i))
                .map(|c| c.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");

            return Ok(json(serde_json::json!({
                "document": document,
                "mode": "window",
                "from": a,
                "to": if b == f32::MAX { serde_json::Value::Null } else { serde_json::json!(b) },
                "cues": cues_json(&window, &cues),
                "seconds": (seconds * 10.0).round() / 10.0,
                "text": text,
            })));
        }

        if full.unwrap_or(false) {
            return Ok(json(serde_json::json!({
                "document": document,
                "mode": "full",
                "cues": cues.len(),
                "characters": registry::join_text(&cues).chars().count(),
                "text": registry::join_text(&cues),
            })));
        }

        // Summary: enough to know what is there before reading any of it.
        let last = cues.len() - 1;
        let total = cues.last().map(|c| c.end as f64).unwrap_or(0.0);

        Ok(json(serde_json::json!({
            "document": document,
            "mode": "summary",
            "cues": cues.len(),
            "seconds": (total * 10.0).round() / 10.0,
            "kind": record.as_ref().map(|t| t.output_kind.clone()).unwrap_or_default(),
            "language": record.as_ref().and_then(|t| t.language.clone()),
            "state": record.as_ref().map(|t| t.state.clone()).unwrap_or_default(),
            "first": cue_json(0, &cues[0]),
            "last": cue_json(last, &cues[last]),
        })))
    }

    /// Stops a running transcription, keeping everything finished.
    ///
    /// The flag is read between ASR blocks and never inside one, so nothing half-written is left
    /// behind: the cue registry keeps every block that was done, and the next run continues from
    /// there.
    #[tool(
        description = "Stops a transcription that is running. Everything already recorded is kept: the cue registry is not rolled back, and the next run continues from where this one stopped."
    )]
    fn cancel(
        &self,
        Parameters(CancelParams { document }): Parameters<CancelParams>,
    ) -> CallToolResult {
        if self.jobs.cancel(&document) {
            json(serde_json::json!({
                "document": document,
                "state": "cancelling",
                "note": "the current block finishes first; everything recorded is kept",
            }))
        } else {
            json(serde_json::json!({
                "document": document,
                "state": "not running",
            }))
        }
    }
}

/// Starts the MCP server over stdio and waits until the client closes.
pub async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "yt-transcribe mcp: serving over stdio (pid {})",
        std::process::id()
    );
    let service = Server::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Starts the MCP server over HTTP (Streamable HTTP) — the mode **systemd** can keep alive.
///
/// Why this exists: a stdio server only lives while a client spawned it and holds the stdin/stdout
/// pipes. systemd keeps the process alive but hands pipes to nobody — so "systemd + stdio" is
/// impossible. Serving over HTTP is the only way for the service to live on its own and answer any
/// harness that points at the address.
pub async fn serve_http(address: &str) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };

    // One job table for the whole process: a session may submit and another may cancel, and both
    // must see the same jobs.
    let jobs = Arc::new(Jobs::default());

    let mcp_service: StreamableHttpService<Server, LocalSessionManager> = StreamableHttpService::new(
        move || {
            Ok(Server {
                jobs: jobs.clone(),
            })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let app = axum::Router::new().nest_service("/mcp", mcp_service);
    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!(
        "yt-transcribe mcp: serving on http://{address}/mcp (pid {})",
        std::process::id()
    );
    axum::serve(listener, app).await?;
    Ok(())
}
