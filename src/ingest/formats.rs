//! Format selection: pick audio-only entries out of `streamingData`.

use serde_json::Value;

#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub itag: i64,
    pub mime: String,
    pub bitrate: i64,
    pub content_length: Option<i64>,
    /// Present when the URL is served directly (no JS challenge).
    pub url: Option<String>,
    /// Present when the URL must be rebuilt from `signatureCipher`/`cipher` (E4).
    pub cipher: Option<String>,
}

/// Audio-only formats, best bitrate first.
pub fn audio_formats(player_response: &Value) -> Vec<AudioFormat> {
    let mut out = Vec::new();
    let lists = [
        player_response.pointer("/streamingData/adaptiveFormats"),
        player_response.pointer("/streamingData/formats"),
    ];
    for list in lists.into_iter().flatten() {
        let Some(arr) = list.as_array() else { continue };
        for f in arr {
            let mime = f.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
            if !mime.starts_with("audio/") {
                continue;
            }
            out.push(AudioFormat {
                itag: f.get("itag").and_then(|v| v.as_i64()).unwrap_or(0),
                mime: mime.to_string(),
                bitrate: f.get("bitrate").and_then(|v| v.as_i64()).unwrap_or(0),
                content_length: f
                    .get("contentLength")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok()),
                url: f.get("url").and_then(|v| v.as_str()).map(str::to_string),
                cipher: f
                    .get("signatureCipher")
                    .or_else(|| f.get("cipher"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            });
        }
    }
    // Prefer m4a/AAC over webm/Opus: measured 2026-10-08, the AAC path decodes
    // correctly over long videos (974 s OK) while the Opus/WebM path produced a
    // garbled, truncated transcript past ~4.5 min. Then rank by bitrate.
    out.sort_by(|a, b| {
        let a_m4a = a.mime.contains("audio/mp4");
        let b_m4a = b.mime.contains("audio/mp4");
        b_m4a.cmp(&a_m4a).then(b.bitrate.cmp(&a.bitrate))
    });
    out
}

/// Why no audio could be taken from a player response -- in the response's own
/// words.
///
/// The player says *why* it refused, and swallowing that is how a day gets lost:
/// an age gate reported as "no audio-only format with a direct URL was returned"
/// sends the reader after the wrong cause entirely. Measured 2026-10-09 on an
/// age-gated video -- the real answer was `LOGIN_REQUIRED -- Sign in to confirm
/// your age`, and the tool said nothing of the sort.
pub fn why_no_audio(player_response: &Value, formats: &[AudioFormat]) -> String {
    let status = player_response
        .pointer("/playabilityStatus/status")
        .and_then(|v| v.as_str());
    let reason = player_response
        .pointer("/playabilityStatus/reason")
        .and_then(|v| v.as_str());

    match status {
        // Refused before offering anything: the reason is right there.
        Some(status) if status != "OK" => match reason {
            Some(reason) => format!(
                "YouTube refused the player request: {status} -- {reason}. \
                 Content that needs an account (age-restricted, members-only, a private \
                 playlist) needs an account cookie export: --cookies <file>"
            ),
            None => format!("YouTube refused the player request: {status}"),
        },
        // Answered, but every audio URL would have to be rebuilt first.
        _ if !formats.is_empty() && formats.iter().all(|f| f.url.is_none()) => {
            let ciphered = formats.iter().filter(|f| f.cipher.is_some()).count();
            format!(
                "no audio format carries a direct URL: {ciphered} of {} need their URL rebuilt \
                 from a signature/`n` challenge, which needs a JavaScript runtime this build \
                 does not embed",
                formats.len()
            )
        }
        _ if formats.is_empty() => "the player response carried no audio format at all".to_string(),
        _ => "no audio-only format with a direct URL was returned".to_string(),
    }
}
