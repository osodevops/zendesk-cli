//! Sideload joins (PRD §12, plan A8): a list response that carried `include=users,groups,…`
//! has the related objects in top-level arrays next to the records. [`embed`] joins them onto
//! each record under the singular key (`assignee_id → assignee`, `group_id → group`, …) so
//! `--fields assignee.name` and the table presets work without a second request.
//!
//! Joins never overwrite a key the record already has, and an id with no match is left as is.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// One join rule: which record key holds the id, which sideloaded collection to look in, and
/// the key the joined object is stored under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Join {
    pub id_key: &'static str,
    pub collection: &'static str,
    pub target: &'static str,
}

const fn join(id_key: &'static str, collection: &'static str, target: &'static str) -> Join {
    Join {
        id_key,
        collection,
        target,
    }
}

/// The joins applied to every record (tickets, comments, users and organizations share them).
pub const JOINS: &[Join] = &[
    join("assignee_id", "users", "assignee"),
    join("requester_id", "users", "requester"),
    join("submitter_id", "users", "submitter"),
    join("author_id", "users", "author"),
    join("group_id", "groups", "group"),
    join("organization_id", "organizations", "organization"),
    join("brand_id", "brands", "brand"),
    join("ticket_form_id", "ticket_forms", "ticket_form"),
];

/// The collection names a `--sideload` value may request and that [`embed`] knows how to join.
pub const COLLECTIONS: &[&str] = &["users", "groups", "organizations", "brands", "ticket_forms"];

/// The sideloaded collections of one response body, indexed by id.
#[derive(Debug, Clone, Default)]
pub struct Sideloads {
    tables: HashMap<&'static str, HashMap<u64, Value>>,
}

impl Sideloads {
    /// Collect every known collection (`users`, `groups`, …) present at the top level of `body`.
    #[must_use]
    pub fn from_body(body: &Value) -> Self {
        let mut tables: HashMap<&'static str, HashMap<u64, Value>> = HashMap::new();
        let Some(map) = body.as_object() else {
            return Self::default();
        };
        for name in COLLECTIONS {
            let Some(Value::Array(items)) = map.get(*name) else {
                continue;
            };
            let table: HashMap<u64, Value> = items
                .iter()
                .filter_map(|item| Some((item.get("id")?.as_u64()?, item.clone())))
                .collect();
            if !table.is_empty() {
                tables.insert(name, table);
            }
        }
        Self { tables }
    }

    /// Nothing to join.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Look one object up.
    #[must_use]
    pub fn get(&self, collection: &str, id: u64) -> Option<&Value> {
        self.tables.get(collection)?.get(&id)
    }

    /// How many objects a collection holds (0 when absent).
    #[must_use]
    pub fn len(&self, collection: &str) -> usize {
        self.tables.get(collection).map_or(0, HashMap::len)
    }
}

/// Join the sideloaded objects onto one record (in place). Nested records that carry their own
/// ids (`via`, `satisfaction_rating`) are left alone; only the top level is joined.
pub fn embed(record: &mut Value, sideloads: &Sideloads) {
    if sideloads.is_empty() {
        return;
    }
    let Some(map) = record.as_object_mut() else {
        return;
    };
    for j in JOINS {
        if map.contains_key(j.target) {
            continue;
        }
        let Some(id) = map.get(j.id_key).and_then(Value::as_u64) else {
            continue;
        };
        if let Some(obj) = sideloads.get(j.collection, id) {
            map.insert(j.target.to_string(), obj.clone());
        }
    }
}

/// [`embed`] for every record of a page, using the collections found in the page body.
pub fn embed_page(records: &mut [Value], body: &Value) {
    let sideloads = Sideloads::from_body(body);
    if sideloads.is_empty() {
        return;
    }
    for r in records {
        embed(r, &sideloads);
    }
}

/// A single-object response (`{"ticket": {...}, "users": [...]}`): join onto the object under
/// `key` and return it, dropping the sideload arrays.
#[must_use]
pub fn embed_single(body: Value, key: &str) -> Value {
    let sideloads = Sideloads::from_body(&body);
    let mut inner = match body {
        Value::Object(mut map) => map.remove(key).unwrap_or(Value::Object(Map::new())),
        other => other,
    };
    embed(&mut inner, &sideloads);
    inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../../../tests/fixtures/tickets/list_sideloaded.json"
        ))
        .expect("fixture parses")
    }

    #[test]
    fn joins_assignee_requester_group_and_organization_from_the_fixture() {
        let body = fixture();
        let mut records = body["tickets"].as_array().cloned().unwrap();
        embed_page(&mut records, &body);
        let t = &records[0];
        assert_eq!(t["assignee"]["name"], "Ada Lovelace");
        assert_eq!(t["requester"]["email"], "grace@example.com");
        assert_eq!(t["submitter"]["id"], 42);
        assert_eq!(t["group"]["name"], "Platform Support");
        assert_eq!(t["organization"]["name"], "Acme Corp");
        assert_eq!(t["brand"]["name"], "OSO Support");
        // The ids stay in place next to the joined objects.
        assert_eq!(t["assignee_id"], 42);
        // Unassigned ticket: no assignee key is invented.
        let u = &records[1];
        assert!(u["assignee_id"].is_null());
        assert!(u.get("assignee").is_none());
        assert_eq!(u["requester"]["name"], "Grace Hopper");
    }

    #[test]
    fn missing_ids_and_unknown_collections_are_left_alone() {
        let body = json!({"tickets": [{"id": 1, "assignee_id": 999, "group_id": 5}], "users": [{"id": 1, "name": "x"}]});
        let sideloads = Sideloads::from_body(&body);
        assert_eq!(sideloads.len("users"), 1);
        assert_eq!(sideloads.len("groups"), 0);
        let mut rec = body["tickets"][0].clone();
        embed(&mut rec, &sideloads);
        assert!(rec.get("assignee").is_none(), "no user 999");
        assert!(rec.get("group").is_none(), "groups were not sideloaded");
        let mut scalar = json!(3);
        embed(&mut scalar, &sideloads);
        assert_eq!(scalar, json!(3));
        assert!(Sideloads::from_body(&json!([1, 2])).is_empty());
    }

    #[test]
    fn existing_keys_are_never_overwritten() {
        let body = json!({"users": [{"id": 7, "name": "Sideloaded"}]});
        let mut rec = json!({"assignee_id": 7, "assignee": {"name": "Already here"}});
        embed(&mut rec, &Sideloads::from_body(&body));
        assert_eq!(rec["assignee"]["name"], "Already here");
    }

    #[test]
    fn embed_single_unwraps_and_joins() {
        let body = json!({
            "ticket": {"id": 1, "assignee_id": 7, "requester_id": 8},
            "users": [{"id": 7, "name": "Ada"}, {"id": 8, "name": "Grace"}]
        });
        let t = embed_single(body, "ticket");
        assert_eq!(t["id"], 1);
        assert_eq!(t["assignee"]["name"], "Ada");
        assert_eq!(t["requester"]["name"], "Grace");
        assert!(t.get("users").is_none());
        assert_eq!(embed_single(json!({"nope": 1}), "ticket"), json!({}));
    }

    #[test]
    fn comments_join_their_author() {
        let body = json!({
            "comments": [{"id": 1, "author_id": 7, "body": "hi"}],
            "users": [{"id": 7, "name": "Ada"}]
        });
        let mut records = body["comments"].as_array().cloned().unwrap();
        embed_page(&mut records, &body);
        assert_eq!(records[0]["author"]["name"], "Ada");
    }
}
