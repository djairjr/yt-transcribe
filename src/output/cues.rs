//! The cue registry: `<stem>.cues.jsonl`, one cue per line, each with its time.
//!
//! This file is the **source of truth**. The `.srt` and the `.txt` are derived from it, and so is
//! everything the MCP tools return -- a transcription is timed text, not text, and throwing the
//! time away would throw away the ability to point at the video ("at 12:34 he says this").
//!
//! It is also what makes a run **resumable**. The registry is appended to once per ASR block, and
//! a block only lands here after being transcribed whole; so the next run asks for `max(chunk) + 1`
//! and nothing already paid for is paid again. A `cancel` therefore preserves exactly what was
//! finished, and restarting costs nothing.

use crate::output::{srt, txt, Cue};
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Where the registry for `stem` lives.
pub fn path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.cues.jsonl"))
}

/// Append cues, and flush.
///
/// Flushing on every block is the point: a reader -- a `status` call, or the `cancel` that stops
/// the run -- must see this work **before** the next block starts, otherwise "everything
/// finished is kept" would be a promise the file does not hold.
pub fn append(path: &Path, cues: &[Cue]) -> Result<(), Box<dyn Error>> {
    let mut out = OpenOptions::new().create(true).append(true).open(path)?;
    for cue in cues {
        serde_json::to_writer(&mut out, cue)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

/// Read the whole registry, in document order.
///
/// A line that does not parse is skipped: a run killed mid-write can leave a partial last line,
/// and losing the entire registry over one torn line would defeat the purpose of keeping it.
pub fn read(path: &Path) -> Result<Vec<Cue>, Box<dyn Error>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut cues = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(cue) = serde_json::from_str::<Cue>(&line) {
            cues.push(cue);
        }
    }
    Ok(cues)
}

/// The highest ASR block already recorded, if any.
pub fn last_chunk(cues: &[Cue]) -> Option<usize> {
    cues.iter().map(|c| c.chunk).max()
}

/// Write `<stem>.srt` and `<stem>.txt` from `cues`. Returns the number of cues written.
pub fn derive(cues: &[Cue], dir: &Path, stem: &str) -> Result<usize, Box<dyn Error>> {
    let n = srt::write_srt(cues, &dir.join(format!("{stem}.srt")))?;
    let text = join_text(cues);
    txt::write_txt(&text, &dir.join(format!("{stem}.txt")))?;
    Ok(n)
}

/// The plain text of the cues, joined the way a transcript reads.
pub fn join_text(cues: &[Cue]) -> String {
    cues.iter()
        .map(|c| c.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
