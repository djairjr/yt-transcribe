//! Watch page: GET the HTML and read the innertube bootstrap values out of it
//! (the API key and the visitor data).
//!
//! The HTML is not kept. The player response is fetched through the innertube API
//! ([`super::innertube::player`]), never parsed out of the page: this module used to
//! extract `ytInitialPlayerResponse` from the HTML as well, and that path was
//! removed when it turned out the binary never walked it (measured 2026-10-10: the
//! whole route goes through the API call).

use crate::ingest::cookies::Jar;
use std::error::Error;

/// A desktop Chrome UA -- YouTube serves a different page (and different
/// bootstrap values) to unknown clients.
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
    pub api_key: String,
    pub visitor_data: Option<String>,
}

pub fn watch_page(url: &str, cookies: Option<&Jar>) -> Result<WatchPage, Box<dyn Error>> {
    let html = fetch_watch(url, cookies)?;
    let api_key = quoted_after(&html, "INNERTUBE_API_KEY")
        .ok_or("INNERTUBE_API_KEY not found in watch page")?;
    let visitor_data = quoted_after(&html, "VISITOR_DATA");
    Ok(WatchPage {
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
