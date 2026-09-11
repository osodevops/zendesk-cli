//! `zdk orgs …` request builders (Organizations endpoints).

use serde_json::Value;

use super::{envelope, include_param, spec, tags_body};
use crate::api::Method;
use crate::http::RequestSpec;

/// The collection path.
pub const PATH: &str = "/api/v2/organizations";

fn org_path(id: u64) -> String {
    format!("{PATH}/{id}")
}

/// `GET /api/v2/organizations` (ListOrganizations, cursor|offset).
#[must_use]
pub fn list(sideload: &[String]) -> RequestSpec {
    let mut s = spec(Method::Get, PATH, "ListOrganizations");
    if let Some(inc) = include_param(sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `GET /api/v2/organizations/{id}` (ShowOrganization).
#[must_use]
pub fn show(id: u64, sideload: &[String]) -> RequestSpec {
    let mut s = spec(Method::Get, org_path(id), "ShowOrganization");
    if let Some(inc) = include_param(sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `GET /api/v2/organizations/search?external_id=` (SearchOrganizations).
#[must_use]
pub fn search_external_id(external_id: &str) -> RequestSpec {
    spec(Method::Get, format!("{PATH}/search"), "SearchOrganizations")
        .query("external_id", external_id)
}

/// `GET /api/v2/organizations/search?name=` (SearchOrganizations).
#[must_use]
pub fn search_name(name: &str) -> RequestSpec {
    spec(Method::Get, format!("{PATH}/search"), "SearchOrganizations").query("name", name)
}

/// `GET /api/v2/organizations/autocomplete?name=` (AutocompleteOrganizations).
#[must_use]
pub fn autocomplete(name: &str) -> RequestSpec {
    spec(
        Method::Get,
        format!("{PATH}/autocomplete"),
        "AutocompleteOrganizations",
    )
    .query("name", name)
}

/// `GET /api/v2/organizations/count` (CountOrganizations).
#[must_use]
pub fn count() -> RequestSpec {
    spec(Method::Get, format!("{PATH}/count"), "CountOrganizations")
}

/// `GET /api/v2/organizations/{id}/related` (OrganizationRelated).
#[must_use]
pub fn related(id: u64) -> RequestSpec {
    spec(
        Method::Get,
        format!("{}/related", org_path(id)),
        "OrganizationRelated",
    )
}

/// `GET /api/v2/organizations/{id}/tickets` (ListOrganizationTickets, cursor|offset).
#[must_use]
pub fn tickets(id: u64, sideload: &[String]) -> RequestSpec {
    let mut s = spec(
        Method::Get,
        format!("{}/tickets", org_path(id)),
        "ListOrganizationTickets",
    );
    if let Some(inc) = include_param(sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `GET /api/v2/organizations/{id}/users` (ListOrganizationUsers, cursor|offset).
#[must_use]
pub fn users(id: u64, sideload: &[String]) -> RequestSpec {
    let mut s = spec(
        Method::Get,
        format!("{}/users", org_path(id)),
        "ListOrganizationUsers",
    );
    if let Some(inc) = include_param(sideload) {
        s = s.query("include", inc);
    }
    s
}

/// `POST /api/v2/organizations` (CreateOrganization).
#[must_use]
pub fn create(organization: Value) -> RequestSpec {
    spec(Method::Post, PATH, "CreateOrganization").json(envelope("organization", organization))
}

/// `PUT /api/v2/organizations/{id}` (UpdateOrganization).
#[must_use]
pub fn update(id: u64, organization: Value) -> RequestSpec {
    spec(Method::Put, org_path(id), "UpdateOrganization")
        .json(envelope("organization", organization))
}

/// `PUT /api/v2/organizations/{id}/tags` (AddOrganizationTags).
#[must_use]
pub fn add_tags(id: u64, tags: &[String]) -> RequestSpec {
    spec(
        Method::Put,
        format!("{}/tags", org_path(id)),
        "AddOrganizationTags",
    )
    .json(tags_body(tags))
}

/// `DELETE /api/v2/organizations/{id}/tags` (RemoveOrganizationTags).
#[must_use]
pub fn remove_tags(id: u64, tags: &[String]) -> RequestSpec {
    spec(
        Method::Delete,
        format!("{}/tags", org_path(id)),
        "RemoveOrganizationTags",
    )
    .json(tags_body(tags))
}

/// `DELETE /api/v2/organizations/{id}` (DeleteOrganization).
#[must_use]
pub fn delete(id: u64) -> RequestSpec {
    spec(Method::Delete, org_path(id), "DeleteOrganization")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_specs() {
        assert_eq!(list(&[]).op.map(|o| o.id), Some("ListOrganizations"));
        assert_eq!(
            list(&["users".into()]).query_value("include"),
            Some("users")
        );
        assert_eq!(show(1, &[]).path, "/api/v2/organizations/1");
        assert_eq!(
            search_external_id("ACME").query_value("external_id"),
            Some("ACME")
        );
        assert_eq!(search_name("Acme").query_value("name"), Some("Acme"));
        assert_eq!(
            autocomplete("ac").op.map(|o| o.id),
            Some("AutocompleteOrganizations")
        );
        assert_eq!(count().path, "/api/v2/organizations/count");
        assert_eq!(related(1).op.map(|o| o.id), Some("OrganizationRelated"));
        assert_eq!(tickets(1, &[]).path, "/api/v2/organizations/1/tickets");
        assert_eq!(
            tickets(1, &[]).op.map(|o| o.scope),
            Some(Some("tickets:read"))
        );
        assert_eq!(
            users(1, &[]).op.map(|o| o.id),
            Some("ListOrganizationUsers")
        );
    }

    #[test]
    fn write_specs() {
        let c = create(json!({"name": "Acme"}));
        assert_eq!(
            c.json_body().unwrap(),
            &json!({"organization": {"name": "Acme"}})
        );
        let u = update(1, json!({"name": "B"}));
        assert_eq!(u.method, Method::Put);
        assert_eq!(u.op.map(|o| o.id), Some("UpdateOrganization"));
        let a = add_tags(1, &["t".into()]);
        assert_eq!(a.path, "/api/v2/organizations/1/tags");
        assert_eq!(a.op.map(|o| o.id), Some("AddOrganizationTags"));
        let r = remove_tags(1, &["t".into()]);
        assert_eq!(r.method, Method::Delete);
        assert_eq!(r.json_body().unwrap(), &json!({"tags": ["t"]}));
        assert_eq!(
            delete(1).op.map(|o| o.scope),
            Some(Some("organizations:write"))
        );
    }
}
