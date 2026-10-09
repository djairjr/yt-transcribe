//! Ingestion: everything that turns a YouTube URL into a byte stream.
//!
//! Replaces yt-dlp / rusty_ytdl as a dependency -- this crate implements the
//! ingestion itself.

pub mod captions;
pub mod clients;
pub mod cookies;
pub mod formats;
pub mod http;
pub mod innertube;
pub mod stream;
pub mod watch;
