//! The job record on disk: `<stem>.task.json`.
//!
//! `status` reads this file, never memory. That is why the answer is the same from any session and
//! why restarting the service costs nothing -- and it is also why a run that was killed leaves a
//! record behind saying what it had reached.

use std::error::Error;
use std::path::{Path, PathBuf};

/// One transcription, as the outside world sees it.
///
/// Every field is `#[serde(default)]` so a record written by an older build still reads: a missing
/// field is a fact about that run, not a reason to lose the whole record.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Task {
    pub stem: String,
    pub source: String,
    /// Where the audio came from: `local`, `youtube` or `network`.
    pub kind: String,
    /// What the text came from: `captions` or `asr`.
    pub output_kind: String,
    /// `running`, `finished`, `cancelled` or `failed`.
    pub state: String,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub chunks_total: usize,
    pub chunks_done: usize,
    pub cues: usize,
    /// Length of the audio, in seconds.
    pub seconds: f64,
    /// The caption track's language code, when the text came from captions. It stays `None` for
    /// ASR: the model detects the language but does not report it, and a tool that invented one
    /// would be lying.
    pub language: Option<String>,
    /// The model that produced the text, when it came from ASR.
    pub model: Option<String>,
    pub error: Option<String>,
}

pub fn path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.task.json"))
}

pub fn write(dir: &Path, task: &Task) -> Result<(), Box<dyn Error>> {
    std::fs::write(path(dir, &task.stem), serde_json::to_string_pretty(task)?)?;
    Ok(())
}

pub fn read(dir: &Path, stem: &str) -> Result<Option<Task>, Box<dyn Error>> {
    let p = path(dir, stem);
    if !p.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p)?;
    Ok(Some(serde_json::from_str(&text)?))
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
