//! `zdk search …` request builders and the ticket filter compiler (PRD §4.6, §8.8).
//!
//! `tickets list --status open --assignee me --older-than 24h` compiles to the Zendesk search
//! query `type:ticket status:open assignee:me created<2026-09-10T12:00:00Z` and runs through
//! `GET /api/v2/search` (offset, 1,000-result cap) or, with `--all`, `GET /api/v2/search/export`
//! (cursor, uncapped). `zdk search explain` prints the compiled string; [`TicketQuery::parse`]
//! reads it back, so the two directions are checked against each other.
//!
//! Grammar of a compiled query (space separated, values with spaces are double-quoted):
//!
//! ```text
//! type:ticket
//!   [status:<s>]*          --status a,b  (repeated = OR)      --awaiting-customer → status:pending
//!   [assignee:none | assignee:<me|id|email>]   --unassigned / --assignee
//!   [requester:<me|id|email>]                  --requester
//!   [group:<id>] [organization:<id>] [brand:<id>] [ticket_form:<id>]
//!   [tags:<t>]*            --tag (repeat)
//!   [priority:<p>] [ticket_type:<t>] [external_id:<x>]
//!   [created>TS] [created<TS] [updated>TS] [updated<TS]   TS = RFC 3339 UTC, seconds
//!   [custom_field_<id>:<v>]*                   --custom-field id=v
//!   [<raw term>]*                              extra terms appended verbatim
//! ```

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

use super::spec;
use crate::api::Method;
use crate::http::RequestSpec;

/// `GET /api/v2/search` path.
pub const PATH: &str = "/api/v2/search";
/// Values `--filter-type` / `filter[type]` accept.
pub const FILTER_TYPES: &[&str] = &["ticket", "user", "organization", "group"];
/// Fields `sort_by` accepts on `/api/v2/search`.
pub const SORT_FIELDS: &[&str] = &[
    "updated_at",
    "created_at",
    "priority",
    "status",
    "ticket_type",
];

/// `GET /api/v2/search?query=` (ListSearchResults, offset).
#[must_use]
pub fn results(query: &str, sort_by: Option<&str>, sort_order: Option<&str>) -> RequestSpec {
    let mut s = spec(Method::Get, PATH, "ListSearchResults").query("query", query);
    if let Some(by) = sort_by {
        s = s.query("sort_by", by);
    }
    if let Some(order) = sort_order {
        s = s.query("sort_order", order);
    }
    s
}

/// `GET /api/v2/search/count?query=` (CountSearchResults).
#[must_use]
pub fn count(query: &str) -> RequestSpec {
    spec(Method::Get, format!("{PATH}/count"), "CountSearchResults").query("query", query)
}

/// `GET /api/v2/search/export?query=&filter[type]=` (ExportSearchResults, cursor).
#[must_use]
pub fn export(query: &str, filter_type: &str) -> RequestSpec {
    spec(Method::Get, format!("{PATH}/export"), "ExportSearchResults")
        .query("query", query)
        .query("filter[type]", filter_type)
}

/// The `type:` term of a query, if it has exactly one (`type:ticket` → `ticket`).
#[must_use]
pub fn infer_filter_type(query: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for term in tokenize(query) {
        if let Some(t) = term.strip_prefix("type:") {
            let t = t.trim_matches('"').to_ascii_lowercase();
            if !FILTER_TYPES.contains(&t.as_str()) || found.is_some() {
                return None;
            }
            found = Some(t);
        }
    }
    found
}

/// The count of a `/search/count` body (`{"count": 57}`) or a `/count` body
/// (`{"count": {"value": 57}}`).
#[must_use]
pub fn count_value(body: &Value) -> Option<u64> {
    match body.get("count")? {
        Value::Number(n) => n.as_u64(),
        Value::Object(o) => o.get("value").and_then(Value::as_u64),
        _ => None,
    }
}

/// The filter flags of `tickets list` / `tickets count` / `search explain`, resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketQuery {
    pub status: Vec<String>,
    /// `me`, a user id, or an email — Zendesk resolves all three server side.
    pub assignee: Option<String>,
    pub requester: Option<String>,
    pub group_id: Option<u64>,
    pub organization_id: Option<u64>,
    pub brand_id: Option<u64>,
    pub form_id: Option<u64>,
    pub tags: Vec<String>,
    pub priority: Option<String>,
    pub ticket_type: Option<String>,
    pub external_id: Option<String>,
    pub created_after: Option<DateTime<Utc>>,
    /// `--created-before` and `--older-than` both land here (the earlier one wins).
    pub created_before: Option<DateTime<Utc>>,
    pub updated_after: Option<DateTime<Utc>>,
    pub updated_before: Option<DateTime<Utc>>,
    /// `(field id, value)` → `custom_field_<id>:<value>`.
    pub custom_fields: Vec<(u64, String)>,
    /// `assignee:none`.
    pub unassigned: bool,
    /// Raw terms appended verbatim (a user-supplied query string).
    pub extra_terms: Vec<String>,
}

impl TicketQuery {
    /// No filter at all (an unfiltered list uses `/api/v2/tickets` instead of search).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Set `created_before` to the earlier of the current value and `t`.
    pub fn older_than(&mut self, t: DateTime<Utc>) {
        self.created_before = Some(match self.created_before {
            Some(cur) if cur < t => cur,
            _ => t,
        });
    }

    /// `--awaiting-customer`: pending status (idempotent).
    pub fn awaiting_customer(&mut self) {
        if !self.status.iter().any(|s| s == "pending") {
            self.status.push("pending".into());
        }
    }

    /// The Zendesk search string (see the module docs for the grammar).
    #[must_use]
    pub fn compile(&self) -> String {
        let mut terms: Vec<String> = Vec::new();
        let has_type = self
            .extra_terms
            .iter()
            .any(|t| t.to_ascii_lowercase().starts_with("type:"));
        if !has_type {
            terms.push("type:ticket".into());
        }
        for s in &self.status {
            terms.push(format!("status:{}", quote(s)));
        }
        if self.unassigned {
            terms.push("assignee:none".into());
        } else if let Some(a) = &self.assignee {
            terms.push(format!("assignee:{}", quote(a)));
        }
        if let Some(r) = &self.requester {
            terms.push(format!("requester:{}", quote(r)));
        }
        if let Some(g) = self.group_id {
            terms.push(format!("group:{g}"));
        }
        if let Some(o) = self.organization_id {
            terms.push(format!("organization:{o}"));
        }
        if let Some(b) = self.brand_id {
            terms.push(format!("brand:{b}"));
        }
        if let Some(f) = self.form_id {
            terms.push(format!("ticket_form:{f}"));
        }
        for t in &self.tags {
            terms.push(format!("tags:{}", quote(t)));
        }
        if let Some(p) = &self.priority {
            terms.push(format!("priority:{}", quote(p)));
        }
        if let Some(t) = &self.ticket_type {
            terms.push(format!("ticket_type:{}", quote(t)));
        }
        if let Some(e) = &self.external_id {
            terms.push(format!("external_id:{}", quote(e)));
        }
        if let Some(t) = self.created_after {
            terms.push(format!("created>{}", ts(t)));
        }
        if let Some(t) = self.created_before {
            terms.push(format!("created<{}", ts(t)));
        }
        if let Some(t) = self.updated_after {
            terms.push(format!("updated>{}", ts(t)));
        }
        if let Some(t) = self.updated_before {
            terms.push(format!("updated<{}", ts(t)));
        }
        for (id, v) in &self.custom_fields {
            terms.push(format!("custom_field_{id}:{}", quote(v)));
        }
        terms.extend(self.extra_terms.iter().cloned());
        terms.join(" ")
    }

    /// Read a compiled query back. Terms outside the grammar land in `extra_terms`; the
    /// leading `type:ticket` is consumed. `None` when a value is malformed (a non-numeric id,
    /// an unparsable timestamp).
    #[must_use]
    pub fn parse(query: &str) -> Option<Self> {
        let mut q = Self::default();
        let mut saw_type = false;
        for term in tokenize(query) {
            if !saw_type && term == "type:ticket" {
                saw_type = true;
                continue;
            }
            if let Some(rest) = term.strip_prefix("created>") {
                q.created_after = Some(parse_ts(rest)?);
            } else if let Some(rest) = term.strip_prefix("created<") {
                q.created_before = Some(parse_ts(rest)?);
            } else if let Some(rest) = term.strip_prefix("updated>") {
                q.updated_after = Some(parse_ts(rest)?);
            } else if let Some(rest) = term.strip_prefix("updated<") {
                q.updated_before = Some(parse_ts(rest)?);
            } else if let Some((key, raw)) = term.split_once(':') {
                let value = unquote(raw);
                match key {
                    "status" => q.status.push(value),
                    "assignee" if value == "none" => q.unassigned = true,
                    "assignee" => q.assignee = Some(value),
                    "requester" => q.requester = Some(value),
                    "group" => q.group_id = Some(value.parse().ok()?),
                    "organization" => q.organization_id = Some(value.parse().ok()?),
                    "brand" => q.brand_id = Some(value.parse().ok()?),
                    "ticket_form" => q.form_id = Some(value.parse().ok()?),
                    "tags" => q.tags.push(value),
                    "priority" => q.priority = Some(value),
                    "ticket_type" => q.ticket_type = Some(value),
                    "external_id" => q.external_id = Some(value),
                    k if k.starts_with("custom_field_") => {
                        let id = k["custom_field_".len()..].parse().ok()?;
                        q.custom_fields.push((id, value));
                    }
                    _ => q.extra_terms.push(term),
                }
            } else {
                q.extra_terms.push(term);
            }
        }
        Some(q)
    }
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Double-quote a value that would otherwise split into several terms.
fn quote(v: &str) -> String {
    if v.is_empty() || v.chars().any(char::is_whitespace) {
        format!("\"{}\"", v.replace('"', ""))
    } else {
        v.to_string()
    }
}

fn unquote(v: &str) -> String {
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .map_or_else(|| v.to_string(), str::to_string)
}

/// Split on whitespace, keeping double-quoted spans (including their quotes) together.
#[must_use]
pub fn tokenize(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in query.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 10, h, 0, 0).unwrap()
    }

    #[test]
    fn every_flag_maps_to_its_term() {
        let mut q = TicketQuery {
            status: vec!["open".into(), "pending".into()],
            assignee: Some("42".into()),
            requester: Some("grace@example.com".into()),
            group_id: Some(501),
            organization_id: Some(1001),
            brand_id: Some(9001),
            form_id: Some(7001),
            tags: vec!["urgent".into(), "vip customer".into()],
            priority: Some("high".into()),
            ticket_type: Some("incident".into()),
            external_id: Some("ORDER-1".into()),
            created_after: Some(t(1)),
            created_before: Some(t(2)),
            updated_after: Some(t(3)),
            updated_before: Some(t(4)),
            custom_fields: vec![(360_000_001, "production".into())],
            unassigned: false,
            extra_terms: vec!["kafka".into()],
        };
        assert_eq!(
            q.compile(),
            "type:ticket status:open status:pending assignee:42 requester:grace@example.com \
             group:501 organization:1001 brand:9001 ticket_form:7001 tags:urgent \
             tags:\"vip customer\" priority:high ticket_type:incident external_id:ORDER-1 \
             created>2026-09-10T01:00:00Z created<2026-09-10T02:00:00Z \
             updated>2026-09-10T03:00:00Z updated<2026-09-10T04:00:00Z \
             custom_field_360000001:production kafka"
        );
        q.unassigned = true;
        assert!(q.compile().contains("assignee:none"));
        assert!(!q.compile().contains("assignee:42"));
        assert!(TicketQuery::default().is_empty());
        assert!(!q.is_empty());
        assert_eq!(TicketQuery::default().compile(), "type:ticket");
    }

    #[test]
    fn composite_flags_and_me() {
        let mut q = TicketQuery {
            assignee: Some("me".into()),
            ..Default::default()
        };
        q.awaiting_customer();
        q.awaiting_customer();
        q.older_than(t(5));
        q.older_than(t(3));
        q.older_than(t(9));
        assert_eq!(
            q.compile(),
            "type:ticket status:pending assignee:me created<2026-09-10T03:00:00Z"
        );
        let raw = TicketQuery {
            extra_terms: vec!["type:user".into(), "role:agent".into()],
            ..Default::default()
        };
        assert_eq!(
            raw.compile(),
            "type:user role:agent",
            "a raw type: term wins"
        );
    }

    #[test]
    fn parse_reads_a_compiled_query_back() {
        let q = TicketQuery {
            status: vec!["open".into()],
            assignee: Some("me".into()),
            tags: vec!["a b".into()],
            created_before: Some(t(2)),
            custom_fields: vec![(7, "x".into())],
            group_id: Some(3),
            extra_terms: vec!["kafka".into(), "subject:\"api latency\"".into()],
            ..Default::default()
        };
        let back = TicketQuery::parse(&q.compile()).unwrap();
        assert_eq!(back, q);
        assert!(
            TicketQuery::parse("type:ticket assignee:none")
                .unwrap()
                .unassigned
        );
        assert!(TicketQuery::parse("group:abc").is_none());
        assert!(TicketQuery::parse("created>yesterday").is_none());
        assert_eq!(
            TicketQuery::parse("hello world").unwrap().extra_terms,
            ["hello", "world"]
        );
    }

    #[test]
    fn tokenizer_and_quoting() {
        assert_eq!(
            tokenize("a \"b c\" d:\"e f\"  g"),
            ["a", "\"b c\"", "d:\"e f\"", "g"]
        );
        assert!(tokenize("   ").is_empty());
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(unquote("\"x y\""), "x y");
        assert_eq!(unquote("x"), "x");
    }

    #[test]
    fn filter_type_inference_and_counts() {
        assert_eq!(
            infer_filter_type("type:ticket status:open").as_deref(),
            Some("ticket")
        );
        assert_eq!(
            infer_filter_type("TYPE:User").as_deref(),
            None,
            "case-sensitive key"
        );
        assert_eq!(infer_filter_type("type:User").as_deref(), Some("user"));
        assert!(infer_filter_type("status:open").is_none());
        assert!(infer_filter_type("type:ticket type:user").is_none());
        assert!(infer_filter_type("type:article").is_none());
        assert_eq!(count_value(&serde_json::json!({"count": 57})), Some(57));
        assert_eq!(
            count_value(&serde_json::json!({"count": {"value": 3}})),
            Some(3)
        );
        assert_eq!(count_value(&serde_json::json!({"count": "x"})), None);
        assert_eq!(count_value(&serde_json::json!({})), None);
    }

    #[test]
    fn request_specs() {
        let s = results("type:ticket", Some("updated_at"), Some("desc"));
        assert_eq!(s.path, PATH);
        assert_eq!(s.query_value("query"), Some("type:ticket"));
        assert_eq!(s.query_value("sort_by"), Some("updated_at"));
        assert_eq!(s.query_value("sort_order"), Some("desc"));
        assert_eq!(s.op.map(|o| o.id), Some("ListSearchResults"));
        assert_eq!(count("x").path, "/api/v2/search/count");
        let e = export("status:open", "ticket");
        assert_eq!(e.path, "/api/v2/search/export");
        assert_eq!(e.query_value("filter[type]"), Some("ticket"));
        assert_eq!(e.op.map(|o| o.id), Some("ExportSearchResults"));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    fn word() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_-]{0,11}"
    }

    fn phrase() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9]{0,5}( [a-z0-9]{1,5}){0,2}"
    }

    fn stamp() -> impl Strategy<Value = DateTime<Utc>> {
        (0i64..1_000_000_000).prop_map(|s| Utc.timestamp_opt(1_600_000_000 + s, 0).unwrap())
    }

    prop_compose! {
        fn query()(
            status in prop::collection::vec(word(), 0..3),
            assignee in prop::option::of(word()),
            unassigned in any::<bool>(),
            requester in prop::option::of(word()),
            group_id in prop::option::of(1u64..10_000),
            organization_id in prop::option::of(1u64..10_000),
            brand_id in prop::option::of(1u64..10_000),
            form_id in prop::option::of(1u64..10_000),
            tags in prop::collection::vec(phrase(), 0..3),
            priority in prop::option::of(word()),
            ticket_type in prop::option::of(word()),
            external_id in prop::option::of(word()),
            created_after in prop::option::of(stamp()),
            created_before in prop::option::of(stamp()),
            updated_after in prop::option::of(stamp()),
            updated_before in prop::option::of(stamp()),
            custom_fields in prop::collection::vec((1u64..1_000_000, phrase()), 0..3),
            extra_terms in prop::collection::vec(word(), 0..3),
        ) -> TicketQuery {
            TicketQuery {
                status,
                // `assignee:none` is what `unassigned` compiles to; an explicit assignee is dropped then.
                assignee: if unassigned { None } else { assignee },
                requester, group_id, organization_id, brand_id, form_id, tags, priority,
                ticket_type, external_id, created_after, created_before, updated_after,
                updated_before, custom_fields, unassigned, extra_terms,
            }
        }
    }

    proptest! {
        #[test]
        fn compile_then_parse_is_identity(q in query()) {
            let compiled = q.compile();
            let back = TicketQuery::parse(&compiled).expect("compiled query parses");
            prop_assert_eq!(&back, &q);
            prop_assert_eq!(back.compile(), compiled);
        }

        #[test]
        fn compiled_queries_start_with_the_type_term(q in query()) {
            prop_assert!(q.compile().starts_with("type:ticket"));
        }
    }
}
