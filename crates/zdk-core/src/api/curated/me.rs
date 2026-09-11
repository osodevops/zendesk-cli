//! The current user (`GET /api/v2/users/me`, ShowCurrentUser): `zdk users me`, `auth whoami`,
//! and the `me` half of `--assignee me`-style references on write paths.

use serde_json::Value;

use super::spec;
use crate::api::Method;
use crate::http::RequestSpec;

/// The path.
pub const PATH: &str = "/api/v2/users/me";

/// `GET /api/v2/users/me`.
#[must_use]
pub fn show() -> RequestSpec {
    spec(Method::Get, PATH, "ShowCurrentUser")
}

/// The user id out of a `{"user": {"id": …}}` body (or a bare user object).
#[must_use]
pub fn user_id(body: &Value) -> Option<u64> {
    body.get("user")
        .unwrap_or(body)
        .get("id")
        .and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn spec_and_id_extraction() {
        let s = show();
        assert_eq!(s.path, PATH);
        assert_eq!(s.op.map(|o| o.id), Some("ShowCurrentUser"));
        let me: Value =
            serde_json::from_str(include_str!("../../../../../tests/fixtures/users/me.json"))
                .unwrap();
        assert_eq!(user_id(&me), Some(42));
        assert_eq!(user_id(&json!({"id": 7})), Some(7));
        assert_eq!(user_id(&json!({"user": {}})), None);
        assert_eq!(user_id(&Value::Null), None);
    }
}
