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
