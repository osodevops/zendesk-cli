//! Secret scrubbing for `-vvv` request/response logs, `--dry-run` output and the audit log.

use std::sync::OnceLock;

use http::HeaderMap;
use regex::Regex;
use url::Url;

/// Bodies are cut here before logging (64 KiB).
pub const MAX_LOG_BODY: usize = 64 * 1024;

/// Header names whose values are never shown.
pub static SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-zendesk-api-token",
];

/// Query parameters and argv flags whose values are never shown.
pub static SENSITIVE_PARAMS: &[&str] = &[
    "access_token",
    "refresh_token",
    "client_secret",
    "api_token",
    "token",
    "password",
    "code",
];

fn body_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `"access_token": "abc"`, `refresh_token=abc`, `client_secret: abc`, `token = 'abc'`
        Regex::new(
            r#"(?i)("?(?:access_token|refresh_token|client_secret|api_token|password|token)"?\s*[:=]\s*)(["']?)([^"'&,\s}\]]+)"#,
        )
        .unwrap_or_else(|e| panic!("redact regex is valid: {e}"))
    })
}

fn credential_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9\-._~+/=]+")
            .unwrap_or_else(|e| panic!("credential regex is valid: {e}"))
    })
}

/// One header value, redacted when the name is sensitive.
#[must_use]
pub fn redact_header(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if SENSITIVE_HEADERS.contains(&lower.as_str()) {
        // Keep the scheme so `Bearer [redacted]` vs `Basic [redacted]` is still visible.
        let scheme = value.split_whitespace().next().unwrap_or_default();
        if !scheme.is_empty()
            && scheme.chars().all(|c| c.is_ascii_alphabetic())
            && value.contains(' ')
        {
            return format!("{scheme} [redacted]");
        }
        return "[redacted]".to_string();
    }
    redact_text(value)
}

/// Every header, redacted, as `(lower-case name, value)` in map order.
#[must_use]
pub fn redact_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let value = if v.is_sensitive() {
                "[redacted]".to_string()
            } else {
                redact_header(k.as_str(), &String::from_utf8_lossy(v.as_bytes()))
            };
            (k.as_str().to_string(), value)
        })
        .collect()
}

/// Scrub token-like `key: value` / `key=value` pairs and `Bearer …` / `Basic …` credentials.
#[must_use]
pub fn redact_text(text: &str) -> String {
    let pass1 = body_regex().replace_all(text, "${1}${2}[redacted]");
    credential_regex()
        .replace_all(&pass1, "${1} [redacted]")
        .into_owned()
}

/// A body for the log: lossy UTF-8, truncated to [`MAX_LOG_BODY`], then scrubbed.
#[must_use]
pub fn redact_body(bytes: &[u8]) -> String {
    let (slice, truncated) = if bytes.len() > MAX_LOG_BODY {
        (&bytes[..MAX_LOG_BODY], true)
    } else {
        (bytes, false)
    };
    let text = redact_text(&String::from_utf8_lossy(slice));
    if truncated {
        format!(
            "{text}… [truncated, {MAX_LOG_BODY} of {} bytes shown]",
            bytes.len()
        )
    } else {
        text
    }
}

/// A URL with sensitive query values replaced.
#[must_use]
pub fn redact_url(url: &Url) -> String {
    if url.query().is_none() {
        return url.to_string();
    }
    let mut out = url.clone();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let lower = k.to_ascii_lowercase();
            if SENSITIVE_PARAMS.contains(&lower.as_str()) {
                (k.into_owned(), "[redacted]".to_string())
            } else {
                (k.into_owned(), v.into_owned())
            }
        })
        .collect();
    out.query_pairs_mut().clear();
    for (k, v) in &pairs {
        out.query_pairs_mut().append_pair(k, v);
    }
    out.to_string()
}

/// The invoking command line for the audit log, with secret flag values hidden.
#[must_use]
pub fn redact_argv<S: AsRef<str>>(args: &[S]) -> String {
    let mut out = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for a in args {
        let a = a.as_ref();
        if hide_next {
            out.push("[redacted]".to_string());
            hide_next = false;
            continue;
        }
        let (flag, inline) = a.split_once('=').map_or((a, None), |(f, v)| (f, Some(v)));
        let name = flag.trim_start_matches('-').replace('-', "_");
        let sensitive = flag.starts_with('-') && SENSITIVE_PARAMS.iter().any(|p| name.ends_with(p));
        if sensitive {
            if inline.is_some() {
                out.push(format!("{flag}=[redacted]"));
            } else {
                out.push(a.to_string());
                hide_next = true;
            }
        } else {
            out.push(a.to_string());
        }
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn json_and_form_secrets_are_scrubbed() {
        let s = redact_text(
            r#"{"access_token":"abc123","refresh_token": "r-1","client_secret":"s3","password":"pw","token":"t","name":"keep"}"#,
        );
        assert!(
            !s.contains("abc123") && !s.contains("r-1") && !s.contains("s3"),
            "{s}"
        );
        assert!(!s.contains("\"pw\"") && !s.contains("\"t\""), "{s}");
        assert!(s.contains("\"name\":\"keep\""), "{s}");
        assert_eq!(
            redact_text("grant_type=refresh_token&refresh_token=zzz&client_id=me"),
            "grant_type=refresh_token&refresh_token=[redacted]&client_id=me"
        );
        assert_eq!(
            redact_text("Authorization: Bearer abc.def"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(redact_text("Basic dXNlcjpwYXNz"), "Basic [redacted]");
    }

    #[test]
    fn headers_are_redacted_by_name_and_sensitivity_flag() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer secret"));
        h.insert("cookie", HeaderValue::from_static("a=b"));
        let mut sensitive = HeaderValue::from_static("plain");
        sensitive.set_sensitive(true);
        h.insert("x-custom", sensitive);
        h.insert("content-type", HeaderValue::from_static("application/json"));
        let out = redact_headers(&h);
        assert!(
            out.contains(&("authorization".into(), "Bearer [redacted]".into())),
            "{out:?}"
        );
        assert!(out.contains(&("cookie".into(), "[redacted]".into())));
        assert!(out.contains(&("x-custom".into(), "[redacted]".into())));
        assert!(out.contains(&("content-type".into(), "application/json".into())));
        assert_eq!(redact_header("X-Api-Key", "k"), "[redacted]");
    }

    #[test]
    fn bodies_are_truncated_and_urls_scrubbed() {
        let big = vec![b'x'; MAX_LOG_BODY + 10];
        let s = redact_body(&big);
        assert!(s.contains("[truncated"), "{}", &s[s.len() - 60..]);
        assert!(s.len() < MAX_LOG_BODY + 100);
        assert_eq!(redact_body(b"{\"a\":1}"), "{\"a\":1}");
        let u = Url::parse("https://x.zendesk.com/oauth/tokens?client_id=a&client_secret=b&code=c")
            .unwrap();
        let r = redact_url(&u);
        assert!(
            r.contains("client_id=a")
                && r.contains("client_secret=%5Bredacted%5D")
                && !r.contains("code=c"),
            "{r}"
        );
        assert_eq!(
            redact_url(&Url::parse("https://x/y").unwrap()),
            "https://x/y"
        );
    }

    #[test]
    fn argv_hides_secret_flag_values() {
        let args = [
            "auth",
            "login",
            "--client-secret",
            "s3",
            "--token=t0",
            "--subdomain",
            "acme",
        ];
        assert_eq!(
            redact_argv(&args),
            "auth login --client-secret [redacted] --token=[redacted] --subdomain acme"
        );
    }
}
