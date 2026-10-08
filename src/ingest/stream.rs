//! Media streaming: HTTP Range GET against a resolved format URL.

use reqwest::StatusCode;
use reqwest::blocking::Client;
use std::error::Error;

/// The media URL was issued to the android_vr client, so the request must carry
/// the same UA or googlevideo may answer 403.
const UA: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 \
                  (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";

pub struct RangeResponse {
    pub status: StatusCode,
    pub content_range: Option<String>,
    pub content_length: Option<u64>,
    pub bytes: Vec<u8>,
}

/// GET `url`, optionally with a `Range` header, and collect the body.
///
/// For a bounded range this returns exactly that many bytes, which is what
/// makes it safe to call before deciding whether to stream.
pub fn range_get(url: &str, range: Option<&str>) -> Result<RangeResponse, Box<dyn Error>> {
    let client = Client::builder().user_agent(UA).build()?;
    range_get_with(&client, url, range)
}

/// Same as [`range_get`], with a caller-provided client (the AVIO read callback
/// issues many requests and must not rebuild the client each time).
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
    let content_range = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let content_length = resp.content_length();
    let bytes = resp.bytes()?.to_vec();
    Ok(RangeResponse {
        status,
        content_range,
        content_length,
        bytes,
    })
}

/// Total length of the resource, via a HEAD request (needed for media seeking).
pub fn content_length(client: &Client, url: &str) -> Option<u64> {
    client
        .head(url)
        .send()
        .ok()
        .and_then(|r| r.content_length())
}

/// Read the body as a stream and return the total byte count, aborting after
/// `max_bytes`. Proves bytes actually flow rather than arriving in one block.
pub fn read_stream(url: &str, max_bytes: usize) -> Result<usize, Box<dyn Error>> {
    use std::io::Read;
    let client = Client::builder().user_agent(UA).build()?;
    let mut resp = client
        .get(url)
        .header("Range", "bytes=0-")
        .header("Accept", "*/*")
        .send()?
        .error_for_status()?;
    let mut total = 0usize;
    let mut buf = [0u8; 64 * 1024];
    let mut chunks = 0usize;
    while total < max_bytes {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n;
        chunks += 1;
    }
    eprintln!("stream: {total} bytes in {chunks} chunk(s)");
    Ok(total)
}
