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
fn format_timestamp(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_s = total_ms / 1000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = total_s / 3600;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}
