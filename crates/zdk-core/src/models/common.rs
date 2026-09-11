//! Envelope pieces every list response shares, and the parser for Zendesk's error bodies.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ValidationDetail;

/// `meta` of a cursor-paginated response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListMeta {
    #[serde(default)]
    pub has_more: bool,
    #[serde(default)]
    pub after_cursor: Option<String>,
    #[serde(default)]
    pub before_cursor: Option<String>,
}

/// `links` of a cursor-paginated response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Links {
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub prev: Option<String>,
}

/// `count` object of the `/count` endpoints (`{"count": {"value": 1, "refreshed_at": "…"}}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Count {
    #[serde(default)]
    pub value: u64,
    #[serde(default)]
    pub refreshed_at: Option<String>,
}

/// A Zendesk error body, whichever of the documented shapes it came in:
///
/// 1. `{"error": "RecordNotFound", "description": "Not found"}`
/// 2. `{"error": {"title": "Forbidden", "message": "You do not have access…"}}`
/// 3. `{"error": "RecordInvalid", "description": "Record validation errors",
///    "details": {"base": [{"description": "…", "error": "…"}]}}`
///
/// plus the OAuth style (`{"error": "invalid_scope", "error_description": "…"}`) and the newer
/// `{"errors": [{"code": "…", "title": "…", "detail": "…"}]}` list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZendeskErrorBody {
    /// Machine code, e.g. `RecordNotFound`, `invalid_scope`, `TooManyJobs`.
    pub code: Option<String>,
    /// Short title (shape 2 / `errors[].title`).
    pub title: Option<String>,
    /// Human description (`description`, `message`, `error_description` or `errors[].detail`).
    pub description: Option<String>,
    /// Field-level details, flattened from `details.<field>[]` or `errors[]`.
    pub details: Vec<ValidationDetail>,
}

impl ZendeskErrorBody {
    /// Parse a response body; `None` when it is not a JSON object with any error-ish key.
    #[must_use]
    pub fn parse(body: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(body).ok()?;
        Self::from_value(&value)
    }

    /// Parse an already-decoded JSON value.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let mut out = Self::default();
        let mut recognised = false;

        match obj.get("error") {
            Some(Value::String(code)) => {
                out.code = Some(code.clone());
                recognised = true;
            }
            Some(Value::Object(inner)) => {
                out.title = string_field(inner, "title");
                out.description =
                    string_field(inner, "message").or_else(|| string_field(inner, "description"));
                out.code = string_field(inner, "code");
                recognised = true;
            }
            _ => {}
        }
        if let Some(d) = string_field(obj, "description")
            .or_else(|| string_field(obj, "error_description"))
            .or_else(|| string_field(obj, "message"))
        {
            out.description = Some(d);
            recognised = true;
        }
        if let Some(Value::Object(details)) = obj.get("details") {
            recognised = true;
            for (field, entries) in details {
                if let Value::Array(items) = entries {
                    for item in items {
                        let message = item
                            .get("description")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("error").and_then(Value::as_str))
                            .or_else(|| item.as_str())
                            .unwrap_or_default()
                            .to_string();
                        out.details.push(ValidationDetail {
                            field: field.clone(),
                            message,
                        });
                    }
                }
            }
        }
        if let Some(Value::Array(errors)) = obj.get("errors") {
            recognised = true;
            for e in errors {
                let title = e.get("title").and_then(Value::as_str).unwrap_or_default();
                let detail = e.get("detail").and_then(Value::as_str).unwrap_or_default();
                let message = match (title.is_empty(), detail.is_empty()) {
                    (false, false) => format!("{title}: {detail}"),
                    (false, true) => title.to_string(),
                    _ => detail.to_string(),
                };
                let field = e
                    .get("source")
                    .and_then(|s| s.get("pointer"))
                    .and_then(Value::as_str)
                    .map(|p| p.trim_start_matches('/').replace('/', "."))
                    .or_else(|| e.get("code").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_else(|| "error".into());
                if out.code.is_none() {
                    out.code = e.get("code").and_then(Value::as_str).map(str::to_string);
                }
                out.details.push(ValidationDetail { field, message });
            }
            if out.description.is_none() {
                out.description = out.details.first().map(|d| d.message.clone());
            }
        }
        recognised.then_some(out)
    }

    /// The best one-line message for humans.
    #[must_use]
    pub fn message(&self) -> String {
        match (&self.title, &self.description, &self.code) {
            (Some(t), Some(d), _) if t != d => format!("{t}: {d}"),
            (_, Some(d), Some(c)) if !c.eq_ignore_ascii_case(d) => format!("{c}: {d}"),
            (_, Some(d), _) => d.clone(),
            (Some(t), None, _) => t.clone(),
            (None, None, Some(c)) => c.clone(),
            (None, None, None) => "request failed".into(),
        }
    }

    /// `400 invalid_scope` from the OAuth layer or a scope-restricted endpoint.
    #[must_use]
    pub fn is_invalid_scope(&self) -> bool {
        self.code
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("invalid_scope"))
            || self
                .description
                .as_deref()
                .is_some_and(|d| d.to_ascii_lowercase().contains("invalid scope"))
    }

    /// The `400` Zendesk returns past the offset-pagination limit (100 pages / 10,000 records).
    /// The exact wording is not documented, so this is deliberately lenient.
    #[must_use]
    pub fn is_offset_limit(&self) -> bool {
        let text = format!(
            "{} {} {}",
            self.code.as_deref().unwrap_or_default(),
            self.title.as_deref().unwrap_or_default(),
            self.description.as_deref().unwrap_or_default()
        )
        .to_ascii_lowercase();
        let mentions_page = text.contains("page") || text.contains("offset");
        let mentions_limit = [
            "limit",
            "10000",
            "10,000",
            "10 000",
            "too large",
            "exceed",
            "100",
        ]
        .iter()
        .any(|w| text.contains(w));
        mentions_page && mentions_limit
    }
}

fn string_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shape_one_string_error_with_description() {
        let b = ZendeskErrorBody::parse(br#"{"error":"RecordNotFound","description":"Not found"}"#)
            .unwrap();
        assert_eq!(b.code.as_deref(), Some("RecordNotFound"));
        assert_eq!(b.description.as_deref(), Some("Not found"));
        assert_eq!(b.message(), "RecordNotFound: Not found");
        assert!(!b.is_invalid_scope());
        assert!(!b.is_offset_limit());
    }

    #[test]
    fn shape_two_object_error_with_title_and_message() {
        let b = ZendeskErrorBody::parse(
            br#"{"error":{"title":"Forbidden","message":"You do not have access to this page."}}"#,
        )
        .unwrap();
        assert_eq!(b.title.as_deref(), Some("Forbidden"));
        assert_eq!(
            b.message(),
            "Forbidden: You do not have access to this page."
        );
    }

    #[test]
    fn shape_three_record_invalid_with_details() {
        let v = json!({
            "error": "RecordInvalid",
            "description": "Record validation errors",
            "details": {
                "base": [{"description": "Requester: Name: is too short", "error": "ValueTooShort"}],
                "subject": [{"description": "Subject: cannot be blank"}]
            }
        });
        let b = ZendeskErrorBody::from_value(&v).unwrap();
        assert_eq!(b.details.len(), 2);
        assert_eq!(b.details[0].field, "base");
        assert!(b.details[0].message.contains("too short"));
        assert_eq!(b.details[1].field, "subject");
        assert_eq!(b.message(), "RecordInvalid: Record validation errors");
    }

    #[test]
    fn oauth_and_errors_array_shapes() {
        let b = ZendeskErrorBody::parse(
            br#"{"error":"invalid_scope","error_description":"The requested scope is invalid"}"#,
        )
        .unwrap();
        assert!(b.is_invalid_scope());
        let b = ZendeskErrorBody::parse(
            br#"{"errors":[{"code":"TooManyValues","title":"Invalid attribute","detail":"Too many values","source":{"pointer":"/data/attributes/tags"}}]}"#,
        )
        .unwrap();
        assert_eq!(b.code.as_deref(), Some("TooManyValues"));
        assert_eq!(b.details[0].field, "data.attributes.tags");
        assert_eq!(b.details[0].message, "Invalid attribute: Too many values");
    }

    #[test]
    fn offset_limit_detection_is_lenient() {
        for body in [
            r#"{"error":"InvalidPaginationParameter","description":"page must be less than or equal to 100"}"#,
            r#"{"error":"BadRequest","description":"Offset pagination is limited to 10,000 records"}"#,
            r#"{"error":{"title":"Invalid page","message":"Page number too large"}}"#,
        ] {
            assert!(
                ZendeskErrorBody::parse(body.as_bytes())
                    .unwrap()
                    .is_offset_limit(),
                "{body}"
            );
        }
        assert!(
            !ZendeskErrorBody::parse(br#"{"error":"RecordInvalid","description":"bad tag"}"#)
                .unwrap()
                .is_offset_limit()
        );
    }

    #[test]
    fn non_error_bodies_are_none() {
        assert!(ZendeskErrorBody::parse(b"not json").is_none());
        assert!(ZendeskErrorBody::parse(br#"{"ticket":{"id":1}}"#).is_none());
        assert!(ZendeskErrorBody::parse(b"[]").is_none());
    }

    #[test]
    fn list_meta_and_links_deserialise_with_defaults() {
        let m: ListMeta =
            serde_json::from_str(r#"{"has_more":true,"after_cursor":"abc"}"#).unwrap();
        assert!(m.has_more);
        assert_eq!(m.after_cursor.as_deref(), Some("abc"));
        assert!(m.before_cursor.is_none());
        let l: Links = serde_json::from_str("{}").unwrap();
        assert!(l.next.is_none());
        let c: Count = serde_json::from_str(r#"{"value":42}"#).unwrap();
        assert_eq!(c.value, 42);
    }
}
