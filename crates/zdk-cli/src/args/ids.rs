//! `me | <id> | <email>` user references on write paths (`--assignee`, `--requester`,
//! `assign --to`, `users get`): `me` → `GET /api/v2/users/me`; an email → exact match in
//! `GET /api/v2/users/search?query=email:<e>` (none → not found, several → usage).
//!
//! Search *filters* never resolve: the Zendesk search language accepts `assignee:me` and
//! emails natively, so `tickets list --assignee a@b.c` costs no extra request.

use serde_json::Value;
use zdk_core::api::curated::{me, users};
use zdk_core::http::ZendeskClient;
use zdk_core::{Result, ZdkError};

/// A parsed user reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UserRef {
    Me,
    Id(u64),
    Email(String),
}

impl UserRef {
    /// `me` (any case), a non-negative integer, or anything containing `@`.
    pub(crate) fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("me") {
            return Ok(Self::Me);
        }
        if let Ok(id) = s.parse::<u64>() {
            return Ok(Self::Id(id));
        }
        if s.contains('@') && !s.contains(char::is_whitespace) {
            return Ok(Self::Email(s.to_string()));
        }
        Err(ZdkError::Usage(format!(
            "'{s}' is not a user reference: use `me`, a numeric user id, or an email address (names need `zdk users search`)"
        )))
    }

    /// The email, when this is one (ticket creation can pass `requester: {email}` directly).
    pub(crate) fn email(&self) -> Option<&str> {
        match self {
            Self::Email(e) => Some(e),
            _ => None,
        }
    }
}

/// The user id for a reference. Under `--dry-run` no lookup is made: `me` and emails come
/// back as the reference text (a warning says so), ids as numbers.
pub(crate) async fn resolve_user(client: &ZendeskClient, r: &UserRef) -> Result<Value> {
    match r {
        UserRef::Id(id) => Ok(Value::from(*id)),
        UserRef::Me if client.is_dry_run() => {
            dry_run_note("me");
            Ok(Value::String("me".into()))
        }
        UserRef::Email(e) if client.is_dry_run() => {
            dry_run_note(e);
            Ok(Value::String(e.clone()))
        }
        UserRef::Me => {
            let body = client.execute(me::show()).await?.value()?;
            me::user_id(&body)
                .map(Value::from)
                .ok_or_else(|| ZdkError::Other("GET /api/v2/users/me returned no user id".into()))
        }
        UserRef::Email(e) => {
            let body = client.execute(users::search_email(e)).await?.value()?;
            let found = body
                .get("users")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            pick_exact_email(&found, e).map(Value::from)
        }
    }
}

fn dry_run_note(what: &str) {
    zdk_core::output::warn(&format!(
        "dry run: '{what}' is not resolved to a user id (that lookup is skipped); the real request carries the id"
    ));
}

/// Exactly one user whose `email` equals `email` (case-insensitively): not found → exit 5,
/// several → exit 2 naming the candidate ids.
pub(crate) fn pick_exact_email(users: &[Value], email: &str) -> Result<u64> {
    let matches: Vec<&Value> = users
        .iter()
        .filter(|u| {
            u.get("email")
                .and_then(Value::as_str)
                .is_some_and(|e| e.eq_ignore_ascii_case(email))
        })
        .collect();
    match matches.as_slice() {
        [] => Err(ZdkError::NotFound {
            resource: "user".into(),
            id: email.to_string(),
            request_id: None,
        }),
        [one] => one
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| ZdkError::Other(format!("user '{email}' has no numeric id"))),
        many => Err(ZdkError::Usage(format!(
            "'{email}' matches {} users ({}); pass the id instead",
            many.len(),
            many.iter()
                .filter_map(|u| u.get("id"))
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_table() {
        let cases: Vec<(&str, std::result::Result<UserRef, i32>)> = vec![
            ("me", Ok(UserRef::Me)),
            ("ME", Ok(UserRef::Me)),
            (" 42 ", Ok(UserRef::Id(42))),
            ("0", Ok(UserRef::Id(0))),
            (
                "ada@example.com",
                Ok(UserRef::Email("ada@example.com".into())),
            ),
            ("Ada Lovelace", Err(2)),
            ("a @b", Err(2)),
            ("-1", Err(2)),
            ("", Err(2)),
        ];
        for (input, expected) in cases {
            let got = UserRef::parse(input).map_err(|e| e.exit_code());
            assert_eq!(got, expected, "{input}");
        }
        assert_eq!(UserRef::parse("a@b.c").unwrap().email(), Some("a@b.c"));
        assert_eq!(UserRef::Me.email(), None);
    }

    #[test]
    fn exact_email_match_is_case_insensitive_and_unique() {
        let users = vec![
            json!({"id": 1, "email": "Ada@Example.com"}),
            json!({"id": 2, "email": "ada@example.com.au"}),
            json!({"id": 3, "email": "other@example.com"}),
            json!({"id": 4}),
        ];
        assert_eq!(pick_exact_email(&users, "ada@example.com").unwrap(), 1);
        let err = pick_exact_email(&users, "nobody@example.com").unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("nobody@example.com"));
        let dupes = vec![
            json!({"id": 1, "email": "x@y.z"}),
            json!({"id": 2, "email": "X@Y.Z"}),
        ];
        let err = pick_exact_email(&dupes, "x@y.z").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("1, 2"), "{err}");
        assert_eq!(
            pick_exact_email(&[json!({"email": "a@b.c"})], "a@b.c")
                .unwrap_err()
                .exit_code(),
            1
        );
    }
}
