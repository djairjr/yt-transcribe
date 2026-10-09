//! `.srt` writer: one subtitle cue per line.

use crate::output::Cue;
use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Write `cues` as an SRT file. Returns the number of cues written.
pub fn write_srt(cues: &[Cue], path: &Path) -> Result<usize, Box<dyn Error>> {
    let mut out = BufWriter::new(File::create(path)?);
    let mut n = 0;
    for c in cues {
        let text = c.text.trim();
        if text.is_empty() {
            continue;
        }
        n += 1;
        writeln!(out, "{}", n)?;
        writeln!(
            out,
            "{} --> {}",
            format_timestamp(c.start as f64),
            format_timestamp(c.end as f64)
        )?;
        writeln!(out, "{text}")?;
        writeln!(out)?;
    }
    out.flush()?;
    Ok(n)
}

/// SRT timestamp: `HH:MM:SS,mmm`.
pub fn format_timestamp(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = total_s / 3600;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// A short, human time: `M:SS`, or `H:MM:SS` past an hour.
///
/// This is the form the MCP tools put next to a cue, because it is the one a person repeats out
/// loud when pointing at a video. `format_timestamp` stays as it is: an `.srt` has a format to obey.
pub fn format_short(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (s, m, h) = (total % 60, (total / 60) % 60, total / 3600);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
