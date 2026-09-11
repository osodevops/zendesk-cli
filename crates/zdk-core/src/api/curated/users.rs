//! `zdk users …` request builders (Users endpoints).

use serde_json::Value;

use super::{envelope, include_param, spec};
use crate::api::Method;
use crate::http::RequestSpec;

/// The collection path.
pub const PATH: &str = "/api/v2/users";
/// Zendesk user roles.
pub const ROLES: &[&str] = &["end-user", "agent", "admin"];

/// Flags of `users list`.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// `role=` (one) or `role[]=` (several).
    pub roles: Vec<String>,
    /// `permission_set=`.
    pub permission_set: Option<u64>,
    /// `external_id=`.
    pub external_id: Option<String>,
    /// Scope the list to `/organizations/{id}/users`.
    pub organization_id: Option<u64>,
    /// Scope the list to `/groups/{id}/users`.
    pub group_id: Option<u64>,
    /// `include=`.
    pub sideload: Vec<String>,
    /// `sort=` (cursor form, e.g. `-updated_at`).
    pub sort: Option<String>,
}

fn user_path(id: u64) -> String {
    format!("{PATH}/{id}")
}

/// `GET /api/v2/users` — or the organization / group scoped variant when asked for.
#[must_use]
pub fn list(o: &ListOptions) -> RequestSpec {
    let mut s = match (o.organization_id, o.group_id) {
        (Some(org), _) => spec(
            Method::Get,
            format!("/api/v2/organizations/{org}/users"),
            "ListOrganizationUsers",
        ),
        (None, Some(group)) => spec(
            Method::Get,
            format!("/api/v2/groups/{group}/users"),
            "ListGroupUsers",
        ),
        (None, None) => spec(Method::Get, PATH, "ListUsers"),
    };
    match o.roles.as_slice() {
        [] => {}
        [one] => s = s.query("role", one),
        many => {
            for r in many {
                s = s.query("role[]", r);
            }
        }
    }
    if let Some(p) = o.permission_set {
        s = s.query("permission_set", p);
    }
    if let Some(e) = &o.external_id {
        s = s.query("external_id", e);
    }
    if let Some(inc) = include_param(&o.sideload) {
        s = s.query("include", inc);
    }
    if let Some(sort) = &o.sort {
        s = s.query("sort", sort);
    }
    s
}

/// `GET /api/v2/users/search?query=` (SearchUsers, offset).
#[must_use]
pub fn search(query: &str) -> RequestSpec {
    spec(Method::Get, format!("{PATH}/search"), "SearchUsers").query("query", query)
}

/// `GET /api/v2/users/search?external_id=` (SearchUsers).
#[must_use]
pub fn search_external_id(external_id: &str) -> RequestSpec {
    spec(Method::Get, format!("{PATH}/search"), "SearchUsers").query("external_id", external_id)
}

/// The exact-email search used to resolve `--assignee a@b.c` style references.
#[must_use]
pub fn search_email(email: &str) -> RequestSpec {
    search(&format!("email:{email}"))
}

/// `GET /api/v2/users/autocomplete?name=` (AutocompleteUsers).
#[must_use]
pub fn autocomplete(name: &str) -> RequestSpec {
    spec(
        Method::Get,
        format!("{PATH}/autocomplete"),
        "AutocompleteUsers",
    )
    .query("name", name)
}

/// `GET /api/v2/users/{id}` (ShowUser).
#[must_use]
pub fn show(id: u64, sideload: &[String]) -> RequestSpec {
    let mut s = spec(Method::Get, user_path(id), "ShowUser");
    if let Some(inc) = include_param(sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `GET /api/v2/users/{id}/related` (ShowUserRelated).
#[must_use]
pub fn related(id: u64) -> RequestSpec {
    spec(
        Method::Get,
        format!("{}/related", user_path(id)),
        "ShowUserRelated",
    )
}

/// `POST /api/v2/users` (CreateUser).
#[must_use]
pub fn create(user: Value) -> RequestSpec {
    spec(Method::Post, PATH, "CreateUser").json(envelope("user", user))
}

/// `POST /api/v2/users/create_or_update` (CreateOrUpdateUser) — matched on email/external id.
#[must_use]
pub fn create_or_update(user: Value) -> RequestSpec {
    spec(
        Method::Post,
        format!("{PATH}/create_or_update"),
        "CreateOrUpdateUser",
    )
    .json(envelope("user", user))
}

/// `PUT /api/v2/users/{id}` (UpdateUser).
#[must_use]
pub fn update(id: u64, user: Value) -> RequestSpec {
    spec(Method::Put, user_path(id), "UpdateUser").json(envelope("user", user))
}

/// `DELETE /api/v2/users/{id}` (DeleteUser) — soft delete.
#[must_use]
pub fn delete(id: u64) -> RequestSpec {
    spec(Method::Delete, user_path(id), "DeleteUser")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn list_routes_to_the_scoped_endpoints_and_repeats_roles() {
        let s = list(&ListOptions {
            roles: vec!["agent".into(), "admin".into()],
            permission_set: Some(3),
            external_id: Some("CRM-1".into()),
            sideload: vec!["organizations".into()],
            sort: Some("-updated_at".into()),
            ..Default::default()
        });
        assert_eq!(s.path, PATH);
        let roles: Vec<&str> = s
            .query
            .iter()
            .filter(|(k, _)| k == "role[]")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(roles, ["agent", "admin"]);
        assert_eq!(s.query_value("permission_set"), Some("3"));
        assert_eq!(s.query_value("external_id"), Some("CRM-1"));
        assert_eq!(s.query_value("include"), Some("organizations"));
        assert_eq!(s.query_value("sort"), Some("-updated_at"));
        let one = list(&ListOptions {
            roles: vec!["agent".into()],
            ..Default::default()
        });
        assert_eq!(one.query_value("role"), Some("agent"));
        assert!(one.query_value("role[]").is_none());
        let org = list(&ListOptions {
            organization_id: Some(9),
            ..Default::default()
        });
        assert_eq!(org.path, "/api/v2/organizations/9/users");
        assert_eq!(org.op.map(|o| o.id), Some("ListOrganizationUsers"));
        let group = list(&ListOptions {
            group_id: Some(4),
            ..Default::default()
        });
        assert_eq!(group.path, "/api/v2/groups/4/users");
        assert_eq!(group.op.map(|o| o.id), Some("ListGroupUsers"));
    }

    #[test]
    fn search_show_and_write_specs() {
        assert_eq!(search("foo").query_value("query"), Some("foo"));
        assert_eq!(
            search_external_id("X").query_value("external_id"),
            Some("X")
        );
        assert_eq!(
            search_email("a@b.c").query_value("query"),
            Some("email:a@b.c")
        );
        assert_eq!(autocomplete("jan").query_value("name"), Some("jan"));
        assert_eq!(show(1, &[]).op.map(|o| o.id), Some("ShowUser"));
        assert_eq!(related(1).path, "/api/v2/users/1/related");
        let c = create(json!({"name": "A"}));
        assert_eq!(c.json_body().unwrap(), &json!({"user": {"name": "A"}}));
        let cu = create_or_update(json!({"email": "a@b.c"}));
        assert_eq!(cu.path, "/api/v2/users/create_or_update");
        assert_eq!(cu.op.map(|o| o.id), Some("CreateOrUpdateUser"));
        let u = update(1, json!({"suspended": true}));
        assert_eq!(u.method, Method::Put);
        assert_eq!(u.json_body().unwrap()["user"]["suspended"], true);
        assert_eq!(delete(1).op.map(|o| o.scope), Some(Some("users:write")));
    }
}
