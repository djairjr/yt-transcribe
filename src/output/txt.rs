//! `.txt` writer: the plain transcript.

use std::error::Error;
use std::fs;
use std::path::Path;

pub fn write_txt(text: &str, path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(path, format!("{}\n", text.trim()))?;
    Ok(())
}
