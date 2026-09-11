//! Hand-written request builders for the curated v0.1.0 commands (plan A10): tickets,
//! comments, users, organizations, search and the current user.
//!
//! Each builder returns a [`RequestSpec`] tagged with its registry [`Operation`], so the HTTP
//! core can run the scope pre-flight, infer the pagination dialect and name the resource in a
//! 404. Bodies are plain [`serde_json::Value`]s: the CLI assembles them from flags and files
//! and unknown keys pass through untouched.

pub mod comments;
pub mod me;
pub mod organizations;
pub mod search;
pub mod tickets;
pub mod users;

use serde_json::{Map, Value};

use crate::api::{self, Method, Operation};
use crate::http::RequestSpec;

/// The registry entry for a curated operation id (`ListTickets`). Every id used by this
/// module is asserted to exist by `api::tests::curated_v0_1_operations_exist_with_scopes`;
/// `None` only if the generated registry regresses, in which case the request still goes out
/// (the path matcher fills `op` in) — it just cannot pre-flight the scope.
#[must_use]
pub fn op(id: &str) -> Option<&'static Operation> {
    api::find_operations(id).first().copied()
}

/// A request spec for `method path`, tagged with the operation `op_id`.
#[must_use]
pub fn spec(method: Method, path: impl Into<String>, op_id: &str) -> RequestSpec {
    let s = RequestSpec::new(method, path);
    match op(op_id) {
        Some(o) => s.op(o),
        None => s,
    }
}

/// `{"ticket": {...}}` → the inner object. A body that is not an object with `key` passes
/// through unchanged (so a `204` `Null` stays `Null`).
#[must_use]
pub fn unwrap_key(body: Value, key: &str) -> Value {
    match body {
        Value::Object(mut map) if map.contains_key(key) => map.remove(key).unwrap_or(Value::Null),
        other => other,
    }
}

/// Wrap `value` as `{key: value}` unless it already is (`{"ticket": {...}}` stays as is).
#[must_use]
pub fn envelope(key: &str, value: Value) -> Value {
    if let Value::Object(map) = &value
        && map.len() == 1
        && map.contains_key(key)
    {
        return value;
    }
    let mut map = Map::new();
    map.insert(key.to_string(), value);
    Value::Object(map)
}

/// The `include=` value for a `--sideload` list (`None` when nothing was asked for).
#[must_use]
pub fn include_param(sideload: &[String]) -> Option<String> {
    let parts: Vec<&str> = sideload
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(","))
    }
}

/// `sort=` value for cursor-paginated lists: `-updated_at` for descending.
#[must_use]
pub fn cursor_sort(sort_by: &str, descending: bool) -> String {
    if descending {
        format!("-{sort_by}")
    } else {
        sort_by.to_string()
    }
}

/// `{"tags": [...]}` for the tag endpoints.
#[must_use]
pub fn tags_body(tags: &[String]) -> Value {
    serde_json::json!({ "tags": tags })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn op_lookup_and_spec_tagging() {
        assert_eq!(op("ListTickets").map(|o| o.id), Some("ListTickets"));
        assert!(op("NoSuchOperationAtAll").is_none());
        let s = spec(Method::Get, "/api/v2/tickets", "ListTickets");
        assert_eq!(s.op.map(|o| o.id), Some("ListTickets"));
        let s = spec(Method::Get, "/api/v2/tickets", "NoSuchOperationAtAll");
        assert!(s.op.is_none());
    }

    #[test]
    fn unwrap_and_envelope_are_inverse_and_idempotent() {
        let t = json!({"id": 1});
        assert_eq!(unwrap_key(json!({"ticket": {"id": 1}}), "ticket"), t);
        assert_eq!(
            unwrap_key(json!({"other": 1}), "ticket"),
            json!({"other": 1})
        );
        assert_eq!(unwrap_key(Value::Null, "ticket"), Value::Null);
        assert_eq!(envelope("ticket", t.clone()), json!({"ticket": {"id": 1}}));
        assert_eq!(
            envelope("ticket", json!({"ticket": {"id": 1}})),
            json!({"ticket": {"id": 1}})
        );
        assert_eq!(
            envelope("ticket", json!({"ticket": 1, "x": 2})),
            json!({"ticket": {"ticket": 1, "x": 2}})
        );
    }

    #[test]
    fn include_and_sort_params() {
        assert_eq!(
            include_param(&["users,groups".into(), " organizations ".into()]).as_deref(),
            Some("users,groups,organizations")
        );
        assert!(include_param(&[]).is_none());
        assert!(include_param(&[" , ".into()]).is_none());
        assert_eq!(cursor_sort("updated_at", true), "-updated_at");
        assert_eq!(cursor_sort("created_at", false), "created_at");
        assert_eq!(tags_body(&["a".into()]), json!({"tags": ["a"]}));
    }
}
