//! The one place that builds the `reqwest` client.
//!
//! Each caller used to build its own, and each one set a different subset of the
//! same two things: the user agent and -- since account cookies exist -- the
//! `Cookie` header. Keeping it here means a request can never forget one of them
//! silently.

use crate::ingest::cookies::Jar;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, COOKIE};
use std::error::Error;

/// A client that talks to `url` as `ua`, carrying a `Cookie` header when the jar
/// holds cookies for that URL's host.
///
/// The cookie goes in as a *default* header, so it also reaches the per-range
/// requests that the AVIO read callback issues through the same client.
pub fn client_for(
    url: &str,
    ua: Option<&str>,
    jar: Option<&Jar>,
) -> Result<Client, Box<dyn Error>> {
    let mut builder = Client::builder();
    if let Some(ua) = ua {
        builder = builder.user_agent(ua);
    }
    if let Some(header) = jar.and_then(|jar| {
        crate::url_host(url).and_then(|host| jar.cookie_header(&host))
    }) {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_str(&header)?);
        builder = builder.default_headers(headers);
    }
    Ok(builder.build()?)
}
