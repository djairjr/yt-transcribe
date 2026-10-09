//! Watch page: GET the HTML and pull out the embedded `ytInitialPlayerResponse`
//! JSON plus the innertube bootstrap values (API key / visitor data).

use crate::ingest::cookies::Jar;
use serde_json::Value;
use std::error::Error;

/// A desktop Chrome UA -- YouTube serves a different page (and different
/// player response) to unknown clients.
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// GET the watch page and return its HTML.
pub fn fetch_watch(url: &str, cookies: Option<&Jar>) -> Result<String, Box<dyn Error>> {
    let client = crate::ingest::http::client_for(url, Some(UA), cookies)?;
    let html = client.get(url).send()?.error_for_status()?.text()?;
    Ok(html)
}

/// Values needed to talk to innertube, read straight from the watch page.
pub struct WatchPage {
    pub html: String,
    pub api_key: String,
    pub visitor_data: Option<String>,
}

pub fn watch_page(url: &str, cookies: Option<&Jar>) -> Result<WatchPage, Box<dyn Error>> {
    let html = fetch_watch(url, cookies)?;
    let api_key = quoted_after(&html, "INNERTUBE_API_KEY")
        .ok_or("INNERTUBE_API_KEY not found in watch page")?;
    let visitor_data = quoted_after(&html, "VISITOR_DATA");
    Ok(WatchPage {
        html,
        api_key,
        visitor_data,
    })
}

/// First `"<key>":"<value>"` occurrence; returns `<value>`.
fn quoted_after(html: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let start = html.find(&marker)? + marker.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Find `marker`, then the `{` after the following `=`, and return the balanced
/// JSON object that starts there.
///
/// Brace matching is string- and escape-aware: the player response contains
/// braces inside quoted strings (JS code, titles), so a naive depth count on
/// raw bytes would cut the object in the wrong place. YouTube ships several
/// occurrences of the marker, so every one is tried until one parses.
fn extract_object(html: &str, marker: &str) -> Option<String> {
    let mut from = 0usize;
    while let Some(pos) = html[from..].find(marker) {
        let abs = from + pos;
        from = abs + marker.len();

        let rest = &html[abs..];
        let eq = match rest.find('=') {
            Some(i) => i,
            None => continue,
        };
        let brace = match rest[eq..].find('{') {
            Some(i) => i,
            None => continue,
        };
        let start = abs + eq + brace;
        if let Some(obj) = balanced_object(&html[start..]) {
            return Some(obj);
        }
    }
    None
}

/// Return the balanced `{...}` starting at `s[0]`, or None if unbalanced.
fn balanced_object(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Fetch the watch page and parse `ytInitialPlayerResponse` into JSON.
pub fn player_response(url: &str, cookies: Option<&Jar>) -> Result<Value, Box<dyn Error>> {
    let html = fetch_watch(url, cookies)?;
    player_response_from_html(&html)
}

pub fn player_response_from_html(html: &str) -> Result<Value, Box<dyn Error>> {
    let raw = extract_object(html, "ytInitialPlayerResponse")
        .ok_or("ytInitialPlayerResponse not found in watch page")?;
    Ok(serde_json::from_str(&raw)?)
}
