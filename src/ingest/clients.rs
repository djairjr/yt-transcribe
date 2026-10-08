//! Innertube client table. Values come from yt-dlp master's INNERTUBE_CLIENTS
//! (measured 2026-10-08); they rotate over time, so this is the one place to
//! update when YouTube moves.
//!
//! Note (2026): `android_vr` no longer escapes the PO Token -- yt-dlp now marks
//! its GVS token as required, and since 2026-08-17 reports every format 403'd
//! with version 1.65.10. `visionos` carries no PO Token policy and is kept as
//! the fallback; the E3 probe picks empirically.

use serde_json::{json, Value};

pub struct Client {
    pub client_name: &'static str,
    pub client_version: &'static str,
    /// Value for the `X-Youtube-Client-Name` header.
    pub client_id: i32,
    pub user_agent: &'static str,
    pub device_make: Option<&'static str>,
    pub device_model: Option<&'static str>,
    pub android_sdk_version: Option<i32>,
    pub os_name: Option<&'static str>,
    pub os_version: Option<&'static str>,
}

impl Client {
    /// The `context.client` object for the innertube request body.
    pub fn context_client(&self) -> Value {
        let mut c = json!({
            "clientName": self.client_name,
            "clientVersion": self.client_version,
            "userAgent": self.user_agent,
        });
        let obj = c.as_object_mut().unwrap();
        if let Some(v) = self.device_make {
            obj.insert("deviceMake".into(), json!(v));
        }
        if let Some(v) = self.device_model {
            obj.insert("deviceModel".into(), json!(v));
        }
        if let Some(v) = self.android_sdk_version {
            obj.insert("androidSdkVersion".into(), json!(v));
        }
        if let Some(v) = self.os_name {
            obj.insert("osName".into(), json!(v));
        }
        if let Some(v) = self.os_version {
            obj.insert("osVersion".into(), json!(v));
        }
        c
    }
}

pub fn android_vr() -> Client {
    Client {
        client_name: "ANDROID_VR",
        client_version: "1.65.10",
        client_id: 28,
        user_agent: "com.google.android.apps.youtube.vr.oculus/1.65.10 \
                     (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip",
        device_make: Some("Oculus"),
        device_model: Some("Quest 3"),
        android_sdk_version: Some(32),
        os_name: Some("Android"),
        os_version: Some("12L"),
    }
}

pub fn visionos() -> Client {
    Client {
        client_name: "VISIONOS",
        client_version: "1.02",
        client_id: 101,
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 \
                     (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
        device_make: Some("Apple"),
        device_model: Some("RealityDevice17,1"),
        android_sdk_version: None,
        os_name: Some("visionOS"),
        os_version: Some("26.5.23O471"),
    }
}

/// iOS client. Measured 2026-10-08: it is the one that returns usable
/// `captionTracks` (visionos and the WEB page either omit them or hand out a
/// timedtext URL that answers 200 with an empty body).
pub fn ios() -> Client {
    Client {
        client_name: "IOS",
        client_version: "21.26.4",
        client_id: 5,
        user_agent: "com.google.ios.youtube/21.26.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
        device_make: Some("Apple"),
        device_model: Some("iPhone16,2"),
        android_sdk_version: None,
        os_name: Some("iPhone"),
        os_version: Some("18.3.2.22D82"),
    }
}
