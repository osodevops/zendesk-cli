//! The static per-endpoint rate-limit table (PRD §4.2) and the matcher that maps a concrete
//! request onto the rules it must honour.

use std::time::Duration;

use serde_json::Value;

use crate::api::{Method, template};

/// What a rule's bucket is keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKey {
    /// One bucket for the whole account.
    Account,
    /// One bucket per value of this path parameter (`ticket_id`, `view_id`, …).
    PathParam(&'static str),
    /// One bucket per value of this dotted body field (`user.email`).
    BodyField(&'static str),
}

/// One documented per-endpoint limit.
#[derive(Debug, Clone)]
pub struct RateRule {
    /// Stable identifier (`ticket_update`, `incremental`, …).
    pub id: &'static str,
    /// Human name of the budget for error messages.
    pub name: &'static str,
    pub methods: &'static [Method],
    /// Path templates (`{x}` = one segment, `{rest..}` = remainder).
    pub templates: &'static [&'static str],
    pub limit: u32,
    pub per: Duration,
    /// Limit with the High Volume API add-on, when Zendesk raises it.
    pub high_volume_limit: Option<u32>,
    pub key: RuleKey,
}

impl RateRule {
    /// The limit that applies to this account.
    #[must_use]
    pub fn effective_limit(&self, high_volume: bool) -> u32 {
        if high_volume {
            self.high_volume_limit.unwrap_or(self.limit)
        } else {
            self.limit
        }
    }
}

const MINUTE: Duration = Duration::from_secs(60);
const TEN_MINUTES: Duration = Duration::from_secs(600);

const GET: &[Method] = &[Method::Get];
const PUT: &[Method] = &[Method::Put];
const POST: &[Method] = &[Method::Post];

/// Rules matched by method + path on every request.
pub static RULES: &[RateRule] = &[
    RateRule {
        id: "ticket_update",
        name: "ticket updates (account)",
        methods: PUT,
        templates: &["/api/v2/tickets/{ticket_id}"],
        limit: 100,
        per: MINUTE,
        high_volume_limit: Some(300),
        key: RuleKey::Account,
    },
    RateRule {
        id: "ticket_update_per_ticket",
        name: "ticket updates (per ticket)",
        methods: PUT,
        templates: &["/api/v2/tickets/{ticket_id}"],
        limit: 30,
        per: TEN_MINUTES,
        high_volume_limit: None,
        key: RuleKey::PathParam("ticket_id"),
    },
    RateRule {
        id: "incremental",
        name: "incremental exports",
        methods: GET,
        templates: &["/api/v2/incremental/{rest..}"],
        limit: 10,
        per: MINUTE,
        high_volume_limit: Some(30),
        key: RuleKey::Account,
    },
    RateRule {
        id: "view_execute",
        name: "view execute (per view)",
        methods: GET,
        templates: &["/api/v2/views/{view_id}/execute"],
        limit: 5,
        per: MINUTE,
        high_volume_limit: None,
        key: RuleKey::PathParam("view_id"),
    },
    RateRule {
        id: "user_update",
        name: "user updates (per user)",
        methods: PUT,
        templates: &["/api/v2/users/{user_id}"],
        limit: 5,
        per: MINUTE,
        high_volume_limit: None,
        key: RuleKey::PathParam("user_id"),
    },
    RateRule {
        id: "user_create_or_update",
        name: "user create_or_update (per email)",
        methods: POST,
        templates: &["/api/v2/users/create_or_update"],
        limit: 5,
        per: MINUTE,
        high_volume_limit: None,
        key: RuleKey::BodyField("user.email"),
    },
    RateRule {
        id: "org_update",
        name: "organization updates (per organization)",
        methods: PUT,
        templates: &["/api/v2/organizations/{organization_id}"],
        limit: 5,
        per: MINUTE,
        high_volume_limit: None,
        key: RuleKey::PathParam("organization_id"),
    },
    RateRule {
        id: "search_export",
        name: "search export",
        methods: GET,
        templates: &["/api/v2/search/export"],
        limit: 100,
        per: MINUTE,
        high_volume_limit: None,
        key: RuleKey::Account,
    },
    RateRule {
        id: "side_conversations",
        name: "side conversations",
        methods: POST,
        templates: &[
            "/api/v2/tickets/{ticket_id}/side_conversations",
            "/api/v2/tickets/{ticket_id}/side_conversations/{side_conversation_id}/reply",
        ],
        limit: 300,
        per: TEN_MINUTES,
        high_volume_limit: None,
        key: RuleKey::Account,
    },
    RateRule {
        id: "side_conversation_events",
        name: "side conversation events",
        methods: GET,
        templates: &["/api/v2/tickets/side_conversations/events"],
        limit: 600,
        per: TEN_MINUTES,
        high_volume_limit: None,
        key: RuleKey::Account,
    },
    RateRule {
        id: "agent_availabilities",
        name: "agent availabilities",
        methods: GET,
        templates: &["/api/v2/agent_availabilities"],
        limit: 300,
        per: MINUTE,
        high_volume_limit: None,
        key: RuleKey::Account,
    },
];

/// `GET /api/v2/tickets?page={n}` with `n > 500` is throttled to 50/min. The path alone cannot
/// tell, so the offset paginator asks for this rule explicitly (`RequestSpec::rate_rules`).
pub static TICKETS_INDEX_DEEP: RateRule = RateRule {
    id: "tickets_index_deep",
    name: "ticket index beyond page 500",
    methods: GET,
    templates: &["/api/v2/tickets"],
    limit: 50,
    per: MINUTE,
    high_volume_limit: None,
    key: RuleKey::Account,
};

/// Every rule, including the ones only applied on request.
pub fn all_rules() -> impl Iterator<Item = &'static RateRule> {
    RULES.iter().chain(std::iter::once(&TICKETS_INDEX_DEEP))
}

/// Look a rule up by id.
#[must_use]
pub fn rule(id: &str) -> Option<&'static RateRule> {
    all_rules().find(|r| r.id == id)
}

/// The rules that apply to `method path`, with the bucket key when it comes from the path.
/// Body-keyed rules are returned with `None`; use [`match_rules_with_body`] to resolve them.
#[must_use]
pub fn match_rules(method: Method, path: &str) -> Vec<(&'static RateRule, Option<String>)> {
    match_rules_with_body(method, path, None)
}

/// [`match_rules`] that can also resolve [`RuleKey::BodyField`] keys from the JSON body.
#[must_use]
pub fn match_rules_with_body(
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Vec<(&'static RateRule, Option<String>)> {
    let normalized = template::normalize(path);
    let mut out = Vec::new();
    for rule in RULES {
        if !rule.methods.contains(&method) {
            continue;
        }
        let Some(params) = rule
            .templates
            .iter()
            .find_map(|t| template::match_template(t, &normalized))
        else {
            continue;
        };
        let key = match rule.key {
            RuleKey::Account => None,
            RuleKey::PathParam(name) => params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone()),
            RuleKey::BodyField(field) => body.and_then(|b| body_field(b, field)),
        };
        out.push((rule, key));
    }
    out
}

/// Read a dotted field (`user.email`) from a JSON body as a string.
#[must_use]
pub fn body_field(body: &Value, dotted: &str) -> Option<String> {
    let mut cur = body;
    for part in dotted.split('.') {
        cur = cur.get(part)?;
    }
    match cur {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(rules: &[(&RateRule, Option<String>)]) -> Vec<(&'static str, Option<String>)> {
        rules.iter().map(|(r, k)| (r.id, k.clone())).collect()
    }

    #[test]
    fn rule_ids_are_unique_and_the_table_is_complete() {
        let mut seen = std::collections::HashSet::new();
        for r in all_rules() {
            assert!(seen.insert(r.id), "duplicate rule {}", r.id);
            assert!(r.limit > 0 && !r.templates.is_empty(), "{}", r.id);
        }
        for id in [
            "ticket_update",
            "ticket_update_per_ticket",
            "incremental",
            "view_execute",
            "user_update",
            "user_create_or_update",
            "org_update",
            "search_export",
            "side_conversations",
            "side_conversation_events",
            "agent_availabilities",
            "tickets_index_deep",
        ] {
            assert!(rule(id).is_some(), "{id}");
        }
        assert_eq!(rule("incremental").unwrap().effective_limit(true), 30);
        assert_eq!(rule("incremental").unwrap().effective_limit(false), 10);
    }

    #[test]
    fn ticket_update_matches_both_account_and_keyed_rules() {
        let m = match_rules(Method::Put, "/api/v2/tickets/42.json");
        assert_eq!(
            ids(&m),
            vec![
                ("ticket_update", None),
                ("ticket_update_per_ticket", Some("42".into()))
            ]
        );
        assert!(match_rules(Method::Get, "/api/v2/tickets/42").is_empty());
        assert!(match_rules(Method::Put, "/api/v2/tickets/42/tags").is_empty());
    }

    #[test]
    fn remainder_and_keyed_rules_match() {
        assert_eq!(
            ids(&match_rules(
                Method::Get,
                "/api/v2/incremental/tickets/cursor?cursor=x"
            )),
            vec![("incremental", None)]
        );
        assert_eq!(
            ids(&match_rules(Method::Get, "/api/v2/views/7/execute.json")),
            vec![("view_execute", Some("7".into()))]
        );
        assert_eq!(
            ids(&match_rules(Method::Put, "/api/v2/users/9")),
            vec![("user_update", Some("9".into()))]
        );
        assert_eq!(
            ids(&match_rules(Method::Put, "/api/v2/organizations/3")),
            vec![("org_update", Some("3".into()))]
        );
        assert_eq!(
            ids(&match_rules(Method::Get, "/api/v2/search/export?query=x")),
            vec![("search_export", None)]
        );
        assert_eq!(
            ids(&match_rules(
                Method::Post,
                "/api/v2/tickets/1/side_conversations/2/reply"
            )),
            vec![("side_conversations", None)]
        );
        assert_eq!(
            ids(&match_rules(
                Method::Get,
                "/api/v2/tickets/side_conversations/events"
            )),
            vec![("side_conversation_events", None)]
        );
        assert_eq!(
            ids(&match_rules(Method::Get, "/api/v2/agent_availabilities")),
            vec![("agent_availabilities", None)]
        );
        assert!(
            match_rules(Method::Get, "/api/v2/tickets").is_empty(),
            "deep rule is explicit"
        );
    }

    #[test]
    fn body_keyed_rule_uses_the_email() {
        let body = json!({"user": {"email": "a@b.c", "name": "A"}});
        let m = match_rules_with_body(Method::Post, "/api/v2/users/create_or_update", Some(&body));
        assert_eq!(
            ids(&m),
            vec![("user_create_or_update", Some("a@b.c".into()))]
        );
        let m = match_rules(Method::Post, "/api/v2/users/create_or_update");
        assert_eq!(ids(&m), vec![("user_create_or_update", None)]);
    }
}
