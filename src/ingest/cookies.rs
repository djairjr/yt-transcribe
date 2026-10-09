//! Account cookies, read from a Netscape `cookies.txt`.
//!
//! Why it exists: an anonymous YouTube session has a low request ceiling, and a
//! long queue of videos starts failing partway through it. The account session
//! raises that ceiling, and it is also what opens content that requires a login
//! (age-restricted, members-only, private playlists).
//!
//! Two rules hold this module together, and both are load-bearing:
//!
//! * **Host-scoped.** Only cookies whose domain matches the host being talked to
//!   are ever sent, so a file exported from YouTube cannot leak to an unrelated
//!   media server.
//! * **Never printed.** A cookie value is never logged, never put into an error
//!   message, and never echoed under `YT_DEBUG`; the only thing that travels back
//!   to the caller is a count.
//!
//! Source of the method: `github.com/yt-dlp/yt-dlp/wiki/Extractors`. The export
//! has to come from an incognito window (log in, open `robots.txt` in the same
//! tab, export, close the window) because YouTube rotates the cookies of open
//! tabs and a normal-session export stops working.

use std::error::Error;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

struct Entry {
    domain: String,
    /// Whether the cookie also applies to sub-domains of `domain`.
    subdomains: bool,
    name: String,
    value: String,
}

/// The cookies of one export file, filtered by host at use time.
pub struct Jar {
    entries: Vec<Entry>,
}

impl Jar {
    /// Read a **Netscape** cookie file -- the format yt-dlp and the browser
    /// extensions write. Lines that are not seven-tab-separated fields are
    /// skipped, which is also how comments get ignored.
    ///
    /// Already-expired entries are dropped, so a stale export fails as "no
    /// cookies" rather than as a puzzling login wall.
    pub fn load(path: &Path) -> Result<Jar, Box<dyn Error>> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read the cookie file {}: {e}", path.display()))?;
        Ok(Jar::parse(&text, now_unix()))
    }

    /// The parser behind [`Jar::load`]. `now` is a parameter so that the expiry
    /// rule can be tested without waiting for a cookie to go stale.
    fn parse(text: &str, now: i64) -> Jar {
        let mut entries = Vec::new();

        for line in text.lines() {
            // `#HttpOnly_<domain>` is an entry, not a comment; any other `#`
            // line really is a comment.
            let line = match line.strip_prefix("#HttpOnly_") {
                Some(rest) => rest,
                None if line.starts_with('#') || line.trim().is_empty() => continue,
                None => line,
            };
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 7 {
                continue;
            }
            let expires = f[4].parse::<i64>().unwrap_or(0);
            if expires > 0 && expires <= now {
                continue;
            }
            entries.push(Entry {
                domain: f[0].trim_start_matches('.').to_ascii_lowercase(),
                subdomains: f[0].starts_with('.') || f[1].eq_ignore_ascii_case("TRUE"),
                name: f[5].to_string(),
                value: f[6].to_string(),
            });
        }

        Jar { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `name=value; name2=value2` for `host`, or `None` when nothing matches.
    ///
    /// A host-only cookie matches exactly; one that carries sub-domains matches
    /// the domain itself and anything under it -- and the leading dot in the
    /// comparison is what keeps `notyoutube.com` from matching `youtube.com`.
    pub fn cookie_header(&self, host: &str) -> Option<String> {
        let host = host.trim_start_matches('.').to_ascii_lowercase();
        let pairs: Vec<String> = self
            .entries
            .iter()
            .filter(|e| host == e.domain || (e.subdomains && host.ends_with(&format!(".{}", e.domain))))
            .map(|e| format!("{}={}", e.name, e.value))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }

    /// The `headers` AVOption value FFmpeg expects (`\r\n`-separated) for `url`.
    pub fn ffmpeg_headers(&self, url: &str) -> Option<String> {
        let host = crate::url_host(url)?;
        self.cookie_header(&host)
            .map(|header| format!("Cookie: {header}\r\n"))
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::Jar;

    /// One Netscape record: domain, sub-domains, path, secure, expiry, name, value.
    fn record(domain: &str, subdomains: &str, expires: i64, name: &str, value: &str) -> String {
        format!("{domain}\t{subdomains}\t/\tFALSE\t{expires}\t{name}\t{value}")
    }

    /// A jar whose "now" is 1000, so expiry is decided by the test and not by the
    /// clock.
    fn jar(text: &str) -> Jar {
        Jar::parse(text, 1000)
    }

    #[test]
    fn only_the_matching_host_is_sent() {
        let text = [
            "# a comment; the blank line below it is not an entry either".to_string(),
            String::new(),
            record(".youtube.com", "TRUE", 0, "SID", "yt-secret"),
            record(".example.com", "TRUE", 0, "token", "other-secret"),
            "not\tenough\tfields".to_string(),
        ]
        .join("\n");
        let jar = jar(&text);

        let header = jar.cookie_header("www.youtube.com").unwrap();
        assert!(header.contains("SID=yt-secret"), "{header}");
        assert!(!header.contains("other-secret"), "{header}");

        let other = jar.cookie_header("www.example.com").unwrap();
        assert!(other.contains("token=other-secret"), "{other}");
        assert!(!other.contains("yt-secret"), "{other}");

        assert!(jar.cookie_header("unrelated.test").is_none());
    }

    #[test]
    fn a_domain_without_subdomains_stays_exact() {
        let jar = jar(&record("www.youtube.com", "FALSE", 0, "SID", "v"));
        assert!(jar.cookie_header("www.youtube.com").is_some());
        assert!(jar.cookie_header("m.youtube.com").is_none());
    }

    #[test]
    fn a_lookalike_host_does_not_match() {
        let jar = jar(&record(".youtube.com", "TRUE", 0, "SID", "v"));
        assert!(jar.cookie_header("notyoutube.com").is_none());
        assert!(jar.cookie_header("youtube.com.evil.test").is_none());
        assert!(jar.cookie_header("youtube.com").is_some());
    }

    #[test]
    fn an_expired_cookie_is_dropped() {
        let text = [
            record(".youtube.com", "TRUE", 999, "STALE", "old"),
            record(".youtube.com", "TRUE", 0, "LIVE", "new"),
        ]
        .join("\n");
        assert_eq!(jar(&text).cookie_header("www.youtube.com").unwrap(), "LIVE=new");
    }

    #[test]
    fn an_httponly_prefix_is_still_an_entry() {
        let line = format!("#HttpOnly_{}", record(".youtube.com", "TRUE", 0, "SID", "v"));
        assert_eq!(jar(&line).cookie_header("www.youtube.com").unwrap(), "SID=v");
    }
}
