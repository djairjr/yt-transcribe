//! Media streaming: HTTP Range GET against a resolved format URL.
//!
//! Only the caller-provided-client form is here. The media URL belongs to the client that
//! issued it, and the AVIO read callback issues many requests with one client -- so this module
//! never builds a client of its own. It used to carry a second entry point with the
//! `android_vr` user agent baked in (and a diagnostic that read a stream to prove the bytes
//! flowed in chunks); both were removed 2026-10-10, unexercised, when the media client became
//! `visionos` and the UA stopped being part of this path.

use reqwest::StatusCode;
use reqwest::blocking::Client;
use std::error::Error;

pub struct RangeResponse {
    pub status: StatusCode,
    pub bytes: Vec<u8>,
}

/// GET `url`, optionally with a `Range` header, and collect the body.
///
/// For a bounded range this returns exactly that many bytes, which is what
/// makes it safe to call before deciding whether to stream.
///
/// The client is passed in by the caller: the AVIO read callback issues many requests and must not
/// rebuild the client each time.
pub fn range_get_with(
    client: &Client,
    url: &str,
    range: Option<&str>,
) -> Result<RangeResponse, Box<dyn Error>> {
    let mut req = client.get(url).header("Accept", "*/*");
    if let Some(r) = range {
        req = req.header("Range", r);
    }
    let resp = req.send()?;
    let status = resp.status();
    let bytes = resp.bytes()?.to_vec();
    Ok(RangeResponse { status, bytes })
}

/// Total length of the resource, via a HEAD request (needed for media seeking).
pub fn content_length(client: &Client, url: &str) -> Option<u64> {
    client
        .head(url)
        .send()
        .ok()
        .and_then(|r| r.content_length())
}
