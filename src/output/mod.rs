//! Output writers, and the registry they are derived from.

pub mod cues;
pub mod srt;
pub mod txt;

/// One timed line, produced either by the ASR or by a YouTube caption track.
///
/// `chunk` is the 240-second ASR block that produced the cue (0 for a caption track). It is what
/// makes a run resumable: a block only appears in the registry once it was transcribed whole, so
/// the next run starts at `max(chunk) + 1` instead of redoing the file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Cue {
    pub start: f32,
    pub end: f32,
    pub text: String,
    #[serde(default)]
    pub chunk: usize,
}

impl Cue {
    /// A cue built from an ASR token, tagged with the block it came from.
    ///
    /// The token's time is already on the absolute timeline: the ASR loop shifts each block back
    /// onto the whole file before handing the tokens over.
    pub fn from_token(t: &parakeet_rs::TimedToken, chunk: usize) -> Self {
        Cue {
            start: t.start,
            end: t.end,
            text: t.text.clone(),
            chunk,
        }
    }
}
