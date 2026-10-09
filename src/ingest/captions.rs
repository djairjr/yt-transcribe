//! Captions: if the video already has a subtitle track, take it and stop --
//! never transcribe what YouTube already transcribed.

use crate::ingest::clients;
use crate::ingest::cookies::Jar;
use crate::ingest::innertube;
use crate::output::Cue;
use serde_json::Value;
use std::error::Error;

/// A caption track offered by the player response.
struct Track {
    language: String,
    is_asr: bool,
    url: String,
}

/// Try to fetch captions for `video_id`. `prefer` is a language *preference*:
/// when given, a track in that language wins; when absent, the video's own
/// default track is used (so a Portuguese video yields Portuguese captions
/// instead of a translated English track).
///
/// Returns `Ok(None)` when the video has no caption tracks at all.
pub fn fetch(
    video_id: &str,
    api_key: &str,
    visitor_data: Option<&str>,
    prefer: Option<&str>,
    cookies: Option<&Jar>,
) -> Result<Option<(String, Vec<Cue>, Option<String>)>, Box<dyn Error>> {
    // The iOS client is the one that actually hands out a working caption URL.
    let client = clients::ios();
    let pr = innertube::player(&client, api_key, video_id, visitor_data, cookies)?;
    let Some(tracks) = tracks(&pr) else {
        return Ok(None);
    };
    if tracks.is_empty() {
        return Ok(None);
    }

    let chosen = pick(&tracks, prefer, default_language(&pr).as_deref());
    let xml = crate::ingest::http::client_for(&chosen.url, Some(client.user_agent), cookies)?
        .get(&chosen.url)
        .send()?
        .error_for_status()?
        .text()?;

    if xml.trim().is_empty() {
        return Ok(None);
    }

    let cues = parse_transcript_xml(&xml)?;
    if cues.is_empty() {
        return Ok(None);
    }
    let text = cues
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some((text, cues, Some(chosen.language.clone()))))
}

fn tracks(pr: &Value) -> Option<Vec<Track>> {
    let list = pr.pointer("/captions/playerCaptionsTracklistRenderer/captionTracks")?;
    let arr = list.as_array()?;
    Some(
        arr.iter()
            .filter_map(|t| {
                let url = t.get("baseUrl").and_then(|v| v.as_str())?;
                Some(Track {
                    language: t
                        .get("languageCode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    is_asr: t.get("kind").and_then(|v| v.as_str()) == Some("asr"),
                    url: url.to_string(),
                })
            })
            .collect(),
    )
}

/// The video's own language, from `defaultAudioTrackIndex` -> `audioTracks[i]`
/// -> `audioTrackId` (e.g. "pt-BR.4" -> "pt"). Caption tracks are not
/// individually flagged as default, so this is how the original language is
/// recovered -- otherwise an English translated ASR track wins by being first.
fn default_language(pr: &Value) -> Option<String> {
    let r = pr.pointer("/captions/playerCaptionsTracklistRenderer")?;
    let idx = r.get("defaultAudioTrackIndex")?.as_i64()? as usize;
    let id = r
        .get("audioTracks")?
        .as_array()?
        .get(idx)?
        .get("audioTrackId")?
        .as_str()?;
    Some(id.split('-').next().unwrap_or(id).to_string())
}

/// Preference order: an explicit language (manual track first, then ASR), else
/// the video's own default language, then any manual track, then whatever exists.
fn pick<'a>(tracks: &'a [Track], prefer: Option<&str>, default_lang: Option<&str>) -> &'a Track {
    if let Some(lang) = prefer.or(default_lang) {
        if let Some(t) = tracks
            .iter()
            .find(|t| t.language == lang && !t.is_asr)
            .or_else(|| tracks.iter().find(|t| t.language == lang))
        {
            return t;
        }
    }
    tracks
        .iter()
        .find(|t| !t.is_asr)
        .unwrap_or(&tracks[0])
}

/// Parse YouTube's timedtext transcript XML:
/// `<transcript><text start="0" dur="3.48">The internet...</text>...</transcript>`
fn parse_transcript_xml(xml: &str) -> Result<Vec<Cue>, Box<dyn Error>> {
    let mut cues = Vec::new();
    let mut rest = xml;
    while let Some(open) = rest.find("<text ") {
        let after = &rest[open + "<text ".len()..];
        let tag_end = match after.find('>') {
            Some(i) => i,
            None => break,
        };
        let attrs = &after[..tag_end];
        let body = &after[tag_end + 1..];
        let close = match body.find("</text>") {
            Some(i) => i,
            None => break,
        };
        let raw = &body[..close];
        rest = &body[close + "</text>".len()..];

        let start = attr_f32(attrs, "start").unwrap_or(0.0);
        let dur = attr_f32(attrs, "dur").unwrap_or(0.0);
        let text = decode_entities(raw).replace('\n', " ");
        let text = text.trim().to_string();
        if !text.is_empty() {
            cues.push(Cue {
                start,
                end: start + dur,
                text,
                // A caption track is not an ASR block: the whole track is one unit.
                chunk: 0,
            });
        }
    }
    Ok(cues)
}

fn attr_f32(attrs: &str, name: &str) -> Option<f32> {
    let key = format!("{name}=\"");
    let start = attrs.find(&key)? + key.len();
    let end = attrs[start..].find('"')?;
    attrs[start..start + end].parse().ok()
}

/// YouTube double-encodes entities: the raw body carries `&amp;#39;`, so
/// decoding `&amp;` first yields `&#39;`, which then decodes to `'`.
///
/// Bytes that are not part of an entity are copied verbatim as bytes (never as
/// `char`), so multi-byte UTF-8 -- accented Portuguese, Korean, the music-note
/// symbol -- survives intact.
fn decode_entities(s: &str) -> String {
    let s = s.replace("&amp;", "&");
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&'
            && let Some((ch, consumed)) = parse_entity(&s[i..])
        {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            i += consumed;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse one entity at the start of `s` (`&...;`). Returns the character and how
/// many bytes it consumed.
fn parse_entity(s: &str) -> Option<(char, usize)> {
    let (body, end) = s.strip_prefix('&')?.split_once(';')?;
    let consumed = 1 + body.len() + 1;
    let ch = match body {
        "quot" => '"',
        "lt" => '<',
        "gt" => '>',
        "nbsp" => ' ',
        "apos" => '\'',
        "amp" => '&',
        _ => {
            let digits = body.strip_prefix('#')?;
            let code = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                digits.parse::<u32>().ok()?
            };
            char::from_u32(code)?
        }
    };
    let _ = end;
    Some((ch, consumed))
}
