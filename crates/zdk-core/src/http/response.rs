//! What the client hands back for a successful (2xx) request.

use std::time::Duration;

use bytes::Bytes;
use http::HeaderMap;
use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use super::rate_limit::RateHeaders;
use crate::{Result, ZdkError};

/// Zendesk's correlation id header.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// A 2xx response: raw bytes plus everything a caller might want to know about the exchange.
#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Bytes,
    /// `X-Request-Id`, for support escalations.
    pub request_id: Option<String>,
    pub rate: RateHeaders,
    /// The URL actually requested (after query assembly).
    pub url: Url,
    /// Wall time across every attempt.
    pub elapsed: Duration,
    /// How many attempts it took (1 = no retry).
    pub attempts: u32,
}

impl ApiResponse {
    /// Deserialise the body as `T`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| {
            ZdkError::Other(format!(
                "Zendesk returned a body that is not the expected JSON ({e}); status {}{}",
                self.status,
                self.request_id
                    .as_deref()
                    .map(|id| format!(", request id {id}"))
                    .unwrap_or_default()
            ))
        })
    }

    /// The body as a JSON value (`Value::Null` for an empty body such as `204 No Content`).
    pub fn value(&self) -> Result<Value> {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Null);
        }
        self.json()
    }

    /// The body as (lossy) text.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// `Content-Type` says JSON.
    #[must_use]
    pub fn is_json(&self) -> bool {
        self.headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.to_ascii_lowercase().contains("json"))
    }

    /// Read `X-Request-Id` from a header map.
    #[must_use]
    pub fn request_id_from(headers: &HeaderMap) -> Option<String> {
        headers
            .get(REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(body: &str) -> ApiResponse {
        let mut headers = HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            "application/json; charset=utf-8".parse().unwrap(),
        );
        headers.insert("x-request-id", "abc-123".parse().unwrap());
        ApiResponse {
            status: 200,
            request_id: ApiResponse::request_id_from(&headers),
            headers,
            body: Bytes::from(body.to_string()),
            rate: RateHeaders::default(),
            url: Url::parse("https://acme.zendesk.com/api/v2/users/me").unwrap(),
            elapsed: Duration::from_millis(5),
            attempts: 1,
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct Envelope {
        user: Value,
    }

    #[test]
    fn json_value_and_text_accessors() {
        let r = resp(r#"{"user":{"id":1}}"#);
        assert!(r.is_json());
        assert_eq!(r.request_id.as_deref(), Some("abc-123"));
        assert_eq!(r.value().unwrap()["user"]["id"], 1);
        assert_eq!(r.json::<Envelope>().unwrap().user["id"], 1);
        assert_eq!(r.text(), r#"{"user":{"id":1}}"#);
        assert_eq!(resp("  ").value().unwrap(), Value::Null);
        let err = resp("nope").json::<Envelope>().unwrap_err();
        assert!(err.to_string().contains("abc-123"), "{err}");
    }
}
