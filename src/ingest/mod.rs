//! Ingestion: everything that turns a YouTube URL into a byte stream.
//!
//! Replaces yt-dlp / rusty_ytdl as a dependency -- this crate implements the
//! ingestion itself.

pub mod captions;
pub mod clients;
pub mod formats;
pub mod innertube;
pub mod stream;
pub mod watch;
