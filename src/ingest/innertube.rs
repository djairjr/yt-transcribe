//! Innertube player call: POST /youtubei/v1/player with a chosen client and get
//! back the streamingData (adaptiveFormats included).

use crate::ingest::clients::Client;
use crate::ingest::cookies::Jar;
use serde_json::{json, Value};
use std::error::Error;

const PLAYER_ENDPOINT: &str = "https://www.youtube.com/youtubei/v1/player";

/// Call the innertube player endpoint for `video_id` as `client`.
///
/// `visitor_data` goes out as `X-Goog-Visitor-Id`: without it YouTube treats
/// the request as a fresh anonymous session and tends to answer LOGIN_REQUIRED.
/// `cookies`, when given, are what actually authenticate an account session --
/// the visitor id alone never does.
pub fn player(
    client: &Client,
    api_key: &str,
    video_id: &str,
    visitor_data: Option<&str>,
    cookies: Option<&Jar>,
) -> Result<Value, Box<dyn Error>> {
    let body = json!({
        "context": {
            "client": client.context_client(),
        },
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    });

    let http = crate::ingest::http::client_for(PLAYER_ENDPOINT, Some(client.user_agent), cookies)?;

    let mut req = http
        .post(PLAYER_ENDPOINT)
        .query(&[("key", api_key)])
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.youtube.com")
        .header("X-Youtube-Client-Name", client.client_id.to_string())
        .header("X-Youtube-Client-Version", client.client_version);
    if let Some(vd) = visitor_data {
        req = req.header("X-Goog-Visitor-Id", vd);
    }

    let resp = req
        .json(&body)
        .send()?
        .error_for_status()?
        .json::<Value>()?;

    Ok(resp)
}

/// Pull `videoId` out of a watch URL (`?v=ID`).
pub fn video_id_from_url(url: &str) -> Option<String> {
    let after = url.split("v=").nth(1)?;
    let id: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!id.is_empty()).then_some(id)
}
