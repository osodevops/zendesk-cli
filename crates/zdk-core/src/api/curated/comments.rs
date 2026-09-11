//! `zdk comments …` request builders (Ticket Comments endpoints).

use super::spec;
use crate::api::Method;
use crate::http::RequestSpec;

/// Flags of `comments list`.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// `include=users` so authors can be joined (on by default in the CLI).
    pub include_users: bool,
    /// `include_inline_images=true`.
    pub include_inline_images: bool,
    /// `desc` walks newest first (`sort=-created_at`, the cursor form).
    pub sort_order: Option<String>,
}

fn base(ticket_id: u64) -> String {
    format!("/api/v2/tickets/{ticket_id}/comments")
}

/// `GET /api/v2/tickets/{id}/comments` (ListTicketComments, cursor|offset).
#[must_use]
pub fn list(ticket_id: u64, o: &ListOptions) -> RequestSpec {
    let mut s = spec(Method::Get, base(ticket_id), "ListTicketComments");
    if o.include_users {
        s = s.query("include", "users");
    }
    if o.include_inline_images {
        s = s.query("include_inline_images", "true");
    }
    if let Some(order) = &o.sort_order {
        let desc = order.eq_ignore_ascii_case("desc");
        s = s.query("sort", super::cursor_sort("created_at", desc));
    }
    s
}

/// `GET /api/v2/tickets/{id}/comments/count` (CountTicketComments).
#[must_use]
pub fn count(ticket_id: u64) -> RequestSpec {
    spec(
        Method::Get,
        format!("{}/count", base(ticket_id)),
        "CountTicketComments",
    )
}

/// `PUT /api/v2/tickets/{id}/comments/{comment}/make_private` (MakeTicketCommentPrivate).
#[must_use]
pub fn make_private(ticket_id: u64, comment_id: u64) -> RequestSpec {
    spec(
        Method::Put,
        format!("{}/{comment_id}/make_private", base(ticket_id)),
        "MakeTicketCommentPrivate",
    )
}

/// `PUT /api/v2/tickets/{id}/comments/{comment}/redact` (RedactStringInComment) — irreversible.
#[must_use]
pub fn redact(ticket_id: u64, comment_id: u64, text: &str) -> RequestSpec {
    spec(
        Method::Put,
        format!("{}/{comment_id}/redact", base(ticket_id)),
        "RedactStringInComment",
    )
    .json(serde_json::json!({ "text": text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_flags_become_query_params() {
        let s = list(
            5,
            &ListOptions {
                include_users: true,
                include_inline_images: true,
                sort_order: Some("desc".into()),
            },
        );
        assert_eq!(s.path, "/api/v2/tickets/5/comments");
        assert_eq!(s.query_value("include"), Some("users"));
        assert_eq!(s.query_value("include_inline_images"), Some("true"));
        assert_eq!(s.query_value("sort"), Some("-created_at"));
        assert_eq!(s.op.map(|o| o.id), Some("ListTicketComments"));
        assert!(list(5, &ListOptions::default()).query.is_empty());
    }

    #[test]
    fn write_specs_carry_paths_ops_and_bodies() {
        assert_eq!(count(5).path, "/api/v2/tickets/5/comments/count");
        let m = make_private(5, 9);
        assert_eq!(m.path, "/api/v2/tickets/5/comments/9/make_private");
        assert_eq!(m.method, Method::Put);
        assert!(m.json_body().is_none());
        assert_eq!(m.op.map(|o| o.scope), Some(Some("tickets:write")));
        let r = redact(5, 9, "4111");
        assert_eq!(r.path, "/api/v2/tickets/5/comments/9/redact");
        assert_eq!(r.json_body().unwrap()["text"], "4111");
        assert_eq!(r.op.map(|o| o.id), Some("RedactStringInComment"));
    }
}
