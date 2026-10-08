//! Output writers: the two files the CLI produces.

pub mod srt;
pub mod txt;

/// One timed line, produced either by the ASR or by a YouTube caption track.
pub struct Cue {
    pub start: f32,
    pub end: f32,
    pub text: String,
}

impl From<&parakeet_rs::TimedToken> for Cue {
    fn from(t: &parakeet_rs::TimedToken) -> Self {
        Cue {
            start: t.start,
            end: t.end,
            text: t.text.clone(),
        }
    }
}
