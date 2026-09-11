//! `--dry-run`: render the request that would be sent (PRD §10.2 "consumes zero quota").
//!
//! Machine formats get one JSON object `{method, url, headers, body}` with the authorization
//! header redacted; table mode gets an equivalent `curl` line.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use url::Url;

use super::idempotency;
use super::redact;
use super::request::{Body, RequestSpec};
use crate::Result;
use crate::output::{self, OutputFormat};

/// The request, ready to print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRunRequest {
    pub method: String,
    pub url: String,
    /// Lower-case header names; secrets redacted.
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

/// Describe `spec` as it would be sent to `url`. `authenticated` adds the (redacted)
/// `Authorization` header; `idempotency_key` is the key the client would attach.
#[must_use]
pub fn describe(
    spec: &RequestSpec,
    url: &Url,
    authenticated: bool,
    idempotency_key: Option<&str>,
) -> DryRunRequest {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "application/json".to_string());
    headers.insert("user-agent".to_string(), super::USER_AGENT.to_string());
    if authenticated {
        headers.insert("authorization".to_string(), "Bearer [redacted]".to_string());
    }
    if let Some(key) = idempotency_key {
        headers.insert(idempotency::HEADER.to_ascii_lowercase(), key.to_string());
    }
    if let Some(ct) = spec.body.as_ref().and_then(Body::content_type) {
        headers.insert("content-type".to_string(), ct.to_string());
    }
    for (k, v) in &spec.headers {
        headers.insert(k.to_ascii_lowercase(), redact::redact_header(k, v));
    }
    let body = spec.body.as_ref().map(|b| match b {
        Body::Json(v) => v.clone(),
        Body::Bytes { data, content_type } => match std::str::from_utf8(data) {
            Ok(text) if data.len() <= redact::MAX_LOG_BODY => {
                Value::String(redact::redact_text(text))
            }
            _ => Value::String(format!("<{} bytes, {content_type}>", data.len())),
        },
        Body::Multipart(_) => Value::String("<multipart form>".to_string()),
    });
    DryRunRequest {
        method: spec.method.as_str().to_string(),
        url: redact::redact_url(url),
        headers,
        body,
    }
}

/// A copy-pasteable `curl` command (with the redacted authorization header, so it needs
/// the real token substituted before it runs).
#[must_use]
pub fn curl_line(req: &DryRunRequest) -> String {
    let mut parts = vec!["curl".to_string()];
    if req.method != "GET" {
        parts.push("-X".into());
        parts.push(req.method.clone());
    }
    parts.push(quote(&req.url));
    for (k, v) in &req.headers {
        if k == "user-agent" || k == "accept" {
            continue;
        }
        parts.push("-H".into());
        parts.push(quote(&format!("{}: {v}", title_case(k))));
    }
    if let Some(body) = &req.body {
        parts.push("--data".into());
        let text = match body {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        parts.push(quote(&text));
    }
    parts.join(" ")
}

/// Print the request in the caller's output format. Never touches the network or the governor.
pub fn render(
    spec: &RequestSpec,
    url: &Url,
    format: OutputFormat,
    authenticated: bool,
    idempotency_key: Option<&str>,
) -> Result<()> {
    let req = describe(spec, url, authenticated, idempotency_key);
    match format {
        OutputFormat::Table => output::write_stdout_line(&curl_line(&req)),
        OutputFormat::Json => output::json::write_value(&serde_json::to_value(&req)?, false),
        _ => output::json::write_value(&serde_json::to_value(&req)?, true),
    }
    Ok(())
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn title_case(header: &str) -> String {
    header
        .split('-')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn url() -> Url {
        Url::parse("https://acme.zendesk.com/api/v2/tickets?page%5Bsize%5D=2").unwrap()
    }

    #[test]
    fn describe_redacts_auth_and_user_headers_and_keeps_the_body() {
        let spec = RequestSpec::post("/api/v2/tickets")
            .json(json!({"ticket": {"subject": "x", "comment": {"body": "token: abc"}}}))
            .header("X-Api-Key", "sekret")
            .header("X-On-Behalf-Of", "agent@example.com");
        let d = describe(&spec, &url(), true, Some("k-1"));
        assert_eq!(d.method, "POST");
        assert_eq!(d.headers["authorization"], "Bearer [redacted]");
        assert_eq!(d.headers["x-api-key"], "[redacted]");
        assert_eq!(d.headers["x-on-behalf-of"], "agent@example.com");
        assert_eq!(d.headers["idempotency-key"], "k-1");
        assert_eq!(d.headers["content-type"], "application/json");
        assert_eq!(d.body.as_ref().unwrap()["ticket"]["subject"], "x");
        let text = serde_json::to_string(&d).unwrap();
        assert!(!text.contains("sekret"), "{text}");
        assert!(
            text.contains("\"url\":\"https://acme.zendesk.com/api/v2/tickets?page%5Bsize%5D=2\"")
        );
    }

    #[test]
    fn curl_line_is_shell_safe() {
        let spec =
            RequestSpec::put("/api/v2/tickets/1").json(json!({"ticket": {"subject": "it's"}}));
        let line = curl_line(&describe(&spec, &url(), true, None));
        assert!(
            line.starts_with("curl -X PUT 'https://acme.zendesk.com/"),
            "{line}"
        );
        assert!(
            line.contains("-H 'Authorization: Bearer [redacted]'"),
            "{line}"
        );
        assert!(
            line.contains("-H 'Content-Type: application/json'"),
            "{line}"
        );
        assert!(
            line.contains(r#"--data '{"ticket":{"subject":"it'\''s"}}'"#),
            "{line}"
        );
        let get = curl_line(&describe(&RequestSpec::get("/x"), &url(), false, None));
        assert!(
            !get.contains("-X") && !get.contains("Authorization"),
            "{get}"
        );
    }

    #[test]
    fn bytes_and_multipart_bodies_are_summarised() {
        let spec = RequestSpec::post("/api/v2/uploads")
            .bytes(vec![0u8, 159, 146, 150], "application/binary");
        let d = describe(&spec, &url(), true, None);
        assert_eq!(
            d.body,
            Some(Value::String("<4 bytes, application/binary>".into()))
        );
        let spec = RequestSpec::post("/api/v2/uploads").bytes("hello", "text/plain");
        assert_eq!(
            describe(&spec, &url(), true, None).body,
            Some(Value::String("hello".into()))
        );
        let spec = RequestSpec::post("/x").multipart(reqwest::multipart::Form::new());
        assert_eq!(
            describe(&spec, &url(), true, None).body,
            Some(Value::String("<multipart form>".into()))
        );
    }
}
