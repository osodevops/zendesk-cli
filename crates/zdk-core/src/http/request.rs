//! What a caller asks the client to send.

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;
use url::Url;

use crate::api::{self, Method, Operation};
use crate::{Result, ZdkError};

/// A request body.
pub enum Body {
    Json(Value),
    Bytes {
        data: Bytes,
        content_type: String,
    },
    /// Uploads. Never retried (the form is consumed when sent).
    Multipart(reqwest::multipart::Form),
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(v) => f.debug_tuple("Json").field(v).finish(),
            Self::Bytes { data, content_type } => f
                .debug_struct("Bytes")
                .field("len", &data.len())
                .field("content_type", content_type)
                .finish(),
            Self::Multipart(_) => f.write_str("Multipart(..)"),
        }
    }
}

impl Body {
    #[must_use]
    pub const fn is_multipart(&self) -> bool {
        matches!(self, Self::Multipart(_))
    }

    #[must_use]
    pub const fn json(&self) -> Option<&Value> {
        match self {
            Self::Json(v) => Some(v),
            _ => None,
        }
    }

    /// The `Content-Type` this body is sent with (`None` for multipart, which sets its own).
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        match self {
            Self::Json(_) => Some("application/json"),
            Self::Bytes { content_type, .. } => Some(content_type),
            Self::Multipart(_) => None,
        }
    }
}

/// One HTTP request, before auth, rate limiting and retries are applied.
#[derive(Debug)]
pub struct RequestSpec {
    pub method: Method,
    /// `/api/v2/…` (relative to the base URL) or an absolute URL.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
    /// Explicit `Idempotency-Key`; generated for POST/PUT/PATCH when `None`.
    pub idempotency_key: Option<String>,
    /// Generate a key for POST/PUT/PATCH when none is given (default). Off → a keyless write is
    /// never retried after it was sent.
    pub auto_idempotency_key: bool,
    /// The registry operation, filled in by [`resolve_op`](Self::resolve_op) when unset.
    pub op: Option<&'static Operation>,
    pub timeout: Option<Duration>,
    /// Skip the scope pre-flight for this request (`zdk api --no-preflight`).
    pub no_preflight: bool,
    /// Extra rate-rule ids to honour (e.g. `tickets_index_deep`).
    pub rate_rules: Vec<&'static str>,
}

impl RequestSpec {
    #[must_use]
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            idempotency_key: None,
            auto_idempotency_key: true,
            op: None,
            timeout: None,
            no_preflight: false,
            rate_rules: Vec::new(),
        }
    }

    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::Get, path)
    }

    #[must_use]
    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::Post, path)
    }

    #[must_use]
    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::Put, path)
    }

    #[must_use]
    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::Patch, path)
    }

    #[must_use]
    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::Delete, path)
    }

    /// A spec for a URL Zendesk handed back (`links.next`, `next_page`, a `Link` header):
    /// the path and query are taken from it; the host is checked by the client.
    #[must_use]
    pub fn from_url(method: Method, url: &Url) -> Self {
        let mut spec = Self::new(method, url.path());
        spec.query = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        spec
    }

    /// Append a query parameter (`value` may be a number, bool or string).
    #[must_use]
    #[allow(clippy::needless_pass_by_value)] // builder ergonomics: `.query("page[size]", 100)`
    pub fn query(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.query.push((key.into(), value.to_string()));
        self
    }

    /// Set (replacing any existing) a query parameter.
    #[allow(clippy::needless_pass_by_value)] // same ergonomics as `query`
    pub fn set_query(&mut self, key: &str, value: impl ToString) {
        self.query.retain(|(k, _)| k != key);
        self.query.push((key.to_string(), value.to_string()));
    }

    pub fn remove_query(&mut self, key: &str) {
        self.query.retain(|(k, _)| k != key);
    }

    #[must_use]
    pub fn query_value(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    #[must_use]
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn json(mut self, value: Value) -> Self {
        self.body = Some(Body::Json(value));
        self
    }

    #[must_use]
    pub fn bytes(mut self, data: impl Into<Bytes>, content_type: impl Into<String>) -> Self {
        self.body = Some(Body::Bytes {
            data: data.into(),
            content_type: content_type.into(),
        });
        self
    }

    #[must_use]
    pub fn multipart(mut self, form: reqwest::multipart::Form) -> Self {
        self.body = Some(Body::Multipart(form));
        self
    }

    #[must_use]
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Send a write without any `Idempotency-Key` (it will then never be retried once sent).
    #[must_use]
    pub fn without_idempotency_key(mut self) -> Self {
        self.idempotency_key = None;
        self.auto_idempotency_key = false;
        self
    }

    #[must_use]
    pub fn op(mut self, op: &'static Operation) -> Self {
        self.op = Some(op);
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn no_preflight(mut self, yes: bool) -> Self {
        self.no_preflight = yes;
        self
    }

    #[must_use]
    pub fn rate_rule(mut self, id: &'static str) -> Self {
        if !self.rate_rules.contains(&id) {
            self.rate_rules.push(id);
        }
        self
    }

    /// A copy of this spec. `None` when the body is multipart (a form cannot be cloned).
    #[must_use]
    pub fn try_clone(&self) -> Option<Self> {
        let body = match &self.body {
            None => None,
            Some(Body::Json(v)) => Some(Body::Json(v.clone())),
            Some(Body::Bytes { data, content_type }) => Some(Body::Bytes {
                data: data.clone(),
                content_type: content_type.clone(),
            }),
            Some(Body::Multipart(_)) => return None,
        };
        Some(Self {
            method: self.method,
            path: self.path.clone(),
            query: self.query.clone(),
            headers: self.headers.clone(),
            body,
            idempotency_key: self.idempotency_key.clone(),
            auto_idempotency_key: self.auto_idempotency_key,
            op: self.op,
            timeout: self.timeout,
            no_preflight: self.no_preflight,
            rate_rules: self.rate_rules.clone(),
        })
    }

    /// The JSON body, if any.
    #[must_use]
    pub fn json_body(&self) -> Option<&Value> {
        self.body.as_ref().and_then(Body::json)
    }

    /// Whether `path` is an absolute URL rather than a path under the base URL.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        let p = self.path.to_ascii_lowercase();
        p.starts_with("http://") || p.starts_with("https://")
    }

    /// Fill `op` from the registry when unset (only meaningful for relative paths).
    pub fn resolve_op(&mut self) -> Option<&'static Operation> {
        if self.op.is_none() && !self.is_absolute() {
            self.op = api::match_path(self.method, &self.path);
        }
        self.op
    }

    /// The absolute URL for this request under `base`, with the query appended.
    pub fn url(&self, base: &Url) -> Result<Url> {
        let mut url = if self.is_absolute() {
            Url::parse(&self.path)?
        } else {
            let rel = self.path.trim_start_matches('/');
            base.join(rel).map_err(|e| {
                ZdkError::Usage(format!("cannot build URL for '{}': {e}", self.path))
            })?
        };
        if !self.query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in &self.query {
                pairs.append_pair(k, v);
            }
        }
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://acme.zendesk.com/").unwrap()
    }

    #[test]
    fn urls_are_joined_and_query_is_appended() {
        let spec = RequestSpec::get("/api/v2/tickets")
            .query("page[size]", 100)
            .query("sort", "-updated_at");
        assert_eq!(
            spec.url(&base()).unwrap().as_str(),
            "https://acme.zendesk.com/api/v2/tickets?page%5Bsize%5D=100&sort=-updated_at"
        );
        let spec = RequestSpec::get("api/v2/users/me");
        assert_eq!(
            spec.url(&base()).unwrap().as_str(),
            "https://acme.zendesk.com/api/v2/users/me"
        );
        let spec = RequestSpec::get("https://status.zendesk.com/api/v2/incidents.json");
        assert!(spec.is_absolute());
        assert_eq!(
            spec.url(&base()).unwrap().host_str(),
            Some("status.zendesk.com")
        );
        // A base with a path prefix (proxy) is preserved.
        let proxied = Url::parse("http://127.0.0.1:9/zd/").unwrap();
        assert_eq!(
            RequestSpec::get("/api/v2/tickets")
                .url(&proxied)
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9/zd/api/v2/tickets"
        );
    }

    #[test]
    fn from_url_keeps_path_and_query_and_set_query_replaces() {
        let u = Url::parse(
            "https://acme.zendesk.com/api/v2/tickets?page%5Bsize%5D=2&page%5Bafter%5D=c1",
        )
        .unwrap();
        let mut spec = RequestSpec::from_url(Method::Get, &u);
        assert_eq!(spec.path, "/api/v2/tickets");
        assert_eq!(spec.query_value("page[after]"), Some("c1"));
        spec.set_query("page[after]", "c2");
        assert_eq!(spec.query_value("page[after]"), Some("c2"));
        assert_eq!(spec.query.len(), 2);
        spec.remove_query("page[after]");
        assert!(spec.query_value("page[after]").is_none());
    }

    #[test]
    fn resolve_op_uses_the_registry_for_relative_paths_only() {
        let mut spec = RequestSpec::get("/api/v2/tickets/1.json");
        assert_eq!(spec.resolve_op().map(|o| o.id), Some("ShowTicket"));
        let mut abs = RequestSpec::get("https://status.zendesk.com/api/v2/incidents");
        assert!(abs.resolve_op().is_none());
        let spec = RequestSpec::put("/api/v2/tickets/1")
            .json(serde_json::json!({"ticket": {"status": "open"}}))
            .rate_rule("tickets_index_deep")
            .rate_rule("tickets_index_deep");
        assert_eq!(spec.rate_rules, ["tickets_index_deep"]);
        assert_eq!(
            spec.body.as_ref().and_then(Body::content_type),
            Some("application/json")
        );
        assert!(format!("{spec:?}").contains("Json"));
    }
}
