//! `zdk tickets …` request builders (Tickets, Tags and Deleted Tickets endpoints).

use serde_json::Value;

use super::{cursor_sort, envelope, include_param, spec, tags_body};
use crate::api::Method;
use crate::http::RequestSpec;

/// The collection path.
pub const PATH: &str = "/api/v2/tickets";
/// Zendesk ticket statuses.
pub const STATUSES: &[&str] = &["new", "open", "pending", "hold", "solved", "closed"];
/// Zendesk ticket priorities.
pub const PRIORITIES: &[&str] = &["low", "normal", "high", "urgent"];
/// Zendesk ticket types.
pub const TYPES: &[&str] = &["problem", "incident", "question", "task"];

/// Flags of the unfiltered `tickets list`.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// `sort_by` (cursor pagination turns this into `sort=[-]field`).
    pub sort_by: Option<String>,
    /// `desc` sorts descending; anything else ascending.
    pub sort_order: Option<String>,
    /// `external_id=` filter (server side).
    pub external_id: Option<String>,
    /// `include=` sideloads.
    pub sideload: Vec<String>,
}

fn ticket_path(id: u64) -> String {
    format!("{PATH}/{id}")
}

/// `GET /api/v2/tickets` (ListTickets, cursor).
#[must_use]
pub fn list(o: &ListOptions) -> RequestSpec {
    let mut s = spec(Method::Get, PATH, "ListTickets");
    if let Some(by) = &o.sort_by {
        let desc = o
            .sort_order
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case("desc"));
        s = s.query("sort", cursor_sort(by, desc));
    }
    if let Some(ext) = &o.external_id {
        s = s.query("external_id", ext);
    }
    if let Some(inc) = include_param(&o.sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `GET /api/v2/tickets/{id}` (ShowTicket).
#[must_use]
pub fn show(id: u64, sideload: &[String]) -> RequestSpec {
    let mut s = spec(Method::Get, ticket_path(id), "ShowTicket");
    if let Some(inc) = include_param(sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `GET /api/v2/tickets/count` (CountTickets).
#[must_use]
pub fn count() -> RequestSpec {
    spec(Method::Get, format!("{PATH}/count"), "CountTickets")
}

/// `GET /api/v2/tickets/recent` (ListRecentTickets).
#[must_use]
pub fn recent() -> RequestSpec {
    spec(Method::Get, format!("{PATH}/recent"), "ListRecentTickets")
}

/// `POST /api/v2/tickets` (CreateTicket). `ticket` may be the bare object or `{"ticket": …}`.
#[must_use]
pub fn create(ticket: Value) -> RequestSpec {
    spec(Method::Post, PATH, "CreateTicket").json(envelope("ticket", ticket))
}

/// `PUT /api/v2/tickets/{id}` (UpdateTicket).
#[must_use]
pub fn update(id: u64, ticket: Value) -> RequestSpec {
    spec(Method::Put, ticket_path(id), "UpdateTicket").json(envelope("ticket", ticket))
}

/// `PUT /api/v2/tickets/{id}/tags` (PutTagsTicket) — adds without replacing.
#[must_use]
pub fn add_tags(id: u64, tags: &[String]) -> RequestSpec {
    spec(
        Method::Put,
        format!("{}/tags", ticket_path(id)),
        "PutTagsTicket",
    )
    .json(tags_body(tags))
}

/// `DELETE /api/v2/tickets/{id}/tags` (DeleteTagsTicket) with a `{"tags": …}` body.
#[must_use]
pub fn remove_tags(id: u64, tags: &[String]) -> RequestSpec {
    spec(
        Method::Delete,
        format!("{}/tags", ticket_path(id)),
        "DeleteTagsTicket",
    )
    .json(tags_body(tags))
}

/// `DELETE /api/v2/tickets/{id}` (DeleteTicket) — soft delete, restorable for 30 days.
#[must_use]
pub fn delete(id: u64) -> RequestSpec {
    spec(Method::Delete, ticket_path(id), "DeleteTicket")
}

/// `PUT /api/v2/deleted_tickets/{id}/restore` (RestoreDeletedTicket).
#[must_use]
pub fn restore(id: u64) -> RequestSpec {
    spec(
        Method::Put,
        format!("/api/v2/deleted_tickets/{id}/restore"),
        "RestoreDeletedTicket",
    )
}

/// `DELETE /api/v2/deleted_tickets/{id}` (DeleteTicketPermanently) — irreversible.
#[must_use]
pub fn delete_permanently(id: u64) -> RequestSpec {
    spec(
        Method::Delete,
        format!("/api/v2/deleted_tickets/{id}"),
        "DeleteTicketPermanently",
    )
}

/// The `comment` object of a ticket create/update body.
#[must_use]
pub fn comment(body: &str, public: bool, author_id: Option<u64>) -> Value {
    let mut c = serde_json::json!({ "body": body, "public": public });
    if let Some(a) = author_id
        && let Some(map) = c.as_object_mut()
    {
        map.insert("author_id".into(), Value::from(a));
    }
    c
}

/// `custom_fields: [{id, value}]` from `(id, value)` pairs.
#[must_use]
pub fn custom_fields(pairs: &[(u64, Value)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(id, value)| serde_json::json!({ "id": id, "value": value }))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_translates_sort_and_filters_into_query_params() {
        let s = list(&ListOptions {
            sort_by: Some("updated_at".into()),
            sort_order: Some("desc".into()),
            external_id: Some("ORDER-1".into()),
            sideload: vec!["users,groups".into()],
        });
        assert_eq!(s.path, PATH);
        assert_eq!(s.query_value("sort"), Some("-updated_at"));
        assert_eq!(s.query_value("external_id"), Some("ORDER-1"));
        assert_eq!(s.query_value("include"), Some("users,groups"));
        assert_eq!(s.op.map(|o| o.id), Some("ListTickets"));
        let plain = list(&ListOptions::default());
        assert!(plain.query.is_empty());
        let asc = list(&ListOptions {
            sort_by: Some("created_at".into()),
            ..Default::default()
        });
        assert_eq!(asc.query_value("sort"), Some("created_at"));
    }

    #[test]
    fn single_ticket_specs_hit_the_documented_paths_and_ops() {
        assert_eq!(show(7, &[]).path, "/api/v2/tickets/7");
        assert_eq!(
            show(7, &["users".into()]).query_value("include"),
            Some("users")
        );
        assert_eq!(count().op.map(|o| o.id), Some("CountTickets"));
        assert_eq!(recent().path, "/api/v2/tickets/recent");
        let created = create(json!({"subject": "s"}));
        assert_eq!(created.method, Method::Post);
        assert_eq!(
            created.json_body().unwrap(),
            &json!({"ticket": {"subject": "s"}})
        );
        let updated = update(3, json!({"ticket": {"status": "solved"}}));
        assert_eq!(updated.method, Method::Put);
        assert_eq!(updated.path, "/api/v2/tickets/3");
        assert_eq!(updated.json_body().unwrap()["ticket"]["status"], "solved");
        assert_eq!(updated.op.map(|o| o.scope), Some(Some("tickets:write")));
        let added = add_tags(3, &["x".into()]);
        assert_eq!(
            (added.method, added.path.as_str()),
            (Method::Put, "/api/v2/tickets/3/tags")
        );
        assert_eq!(added.json_body().unwrap(), &json!({"tags": ["x"]}));
        let removed = remove_tags(3, &["x".into()]);
        assert_eq!(removed.method, Method::Delete);
        assert_eq!(removed.op.map(|o| o.id), Some("DeleteTagsTicket"));
        assert_eq!(delete(3).op.map(|o| o.id), Some("DeleteTicket"));
        assert_eq!(restore(3).path, "/api/v2/deleted_tickets/3/restore");
        assert_eq!(restore(3).method, Method::Put);
        let purged = delete_permanently(3);
        assert_eq!(
            (purged.method, purged.path.as_str()),
            (Method::Delete, "/api/v2/deleted_tickets/3")
        );
        assert_eq!(purged.op.map(|o| o.id), Some("DeleteTicketPermanently"));
    }

    #[test]
    fn comment_and_custom_field_helpers() {
        assert_eq!(
            comment("hi", false, Some(9)),
            json!({"body": "hi", "public": false, "author_id": 9})
        );
        assert_eq!(
            comment("hi", true, None),
            json!({"body": "hi", "public": true})
        );
        assert_eq!(
            custom_fields(&[(1, json!("a")), (2, json!(true))]),
            json!([{"id": 1, "value": "a"}, {"id": 2, "value": true}])
        );
    }
}
