//! The Zendesk API surface: the generated operation registry (`generated/`) plus the
//! hand-written curated wrappers (`curated/`).

pub mod generated;

use std::fmt;

/// Which upstream OpenAPI document an operation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Spec {
    Support,
    HelpCenter,
    Voice,
}

impl Spec {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Support => "support",
            Self::HelpCenter => "help_center",
            Self::Voice => "voice",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "support" => Some(Self::Support),
            "help_center" | "hc" => Some(Self::HelpCenter),
            "voice" | "talk" => Some(Self::Voice),
            _ => None,
        }
    }
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// HTTP method of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "PATCH" => Some(Self::Patch),
            "DELETE" => Some(Self::Delete),
            _ => None,
        }
    }

    /// Whether a request with this method may be retried after it was sent.
    #[must_use]
    pub const fn is_idempotent(self) -> bool {
        matches!(self, Self::Get | Self::Put | Self::Delete)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<Method> for reqwest::Method {
    fn from(m: Method) -> Self {
        match m {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Patch => reqwest::Method::PATCH,
            Method::Delete => reqwest::Method::DELETE,
        }
    }
}

/// Where a parameter is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamIn {
    Path,
    Query,
    Header,
}

/// Coarse parameter type (enough for validation and help text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    String,
    Integer,
    Number,
    Boolean,
    Array,
    Object,
}

/// Whether an operation takes a request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    None,
    Optional,
    Required,
}

/// Pagination dialect inferred for an operation (PRD §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageDialect {
    None,
    Cursor,
    Offset,
    /// Endpoint supports both; cursor is preferred.
    Dual,
    Incremental,
    Audits,
}

impl PageDialect {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Cursor => "cursor",
            Self::Offset => "offset",
            Self::Dual => "cursor|offset",
            Self::Incremental => "incremental",
            Self::Audits => "audits",
        }
    }
}

/// One parameter of a generated operation.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub location: ParamIn,
    pub ty: ParamType,
    pub required: bool,
    /// `style: deepObject` (e.g. `page[size]`).
    pub deep_object: bool,
}

/// One documented Zendesk operation. Emitted by `cargo xtask codegen` into `generated/registry.rs`.
#[derive(Debug, Clone, Copy)]
pub struct Operation {
    pub spec: Spec,
    /// The upstream `operationId`, unique within `spec`.
    pub id: &'static str,
    pub method: Method,
    /// Path template, e.g. `/api/v2/tickets/{ticket_id}`.
    pub path: &'static str,
    /// First tag = resource group, e.g. `Tickets`.
    pub tag: &'static str,
    pub summary: &'static str,
    pub params: &'static [Param],
    pub body: BodyKind,
    /// Array property of the 200 response that holds the records, e.g. `tickets`.
    pub items_key: Option<&'static str>,
    pub pagination: PageDialect,
    /// Granular OAuth scope required, e.g. `tickets:read`; `None` when unknown.
    pub scope: Option<&'static str>,
    pub deprecated: bool,
}

impl Operation {
    /// `support.ListTickets` — the fully qualified display form.
    #[must_use]
    pub fn qualified_id(&self) -> String {
        format!("{}.{}", self.spec, self.id)
    }
}

/// A committed spec snapshot, mirrored from `specs/SPEC_VERSIONS.toml`.
#[derive(Debug, Clone, Copy)]
pub struct SpecVersion {
    pub spec: Spec,
    pub url: &'static str,
    pub openapi: &'static str,
    pub info_version: &'static str,
    pub sha256: &'static str,
    pub fetched_at: &'static str,
    pub paths: usize,
    pub operations: usize,
}

/// Every generated operation, sorted by `(spec, path, method)`.
#[must_use]
pub fn operations() -> &'static [Operation] {
    generated::registry::OPERATIONS
}

/// The committed spec snapshots (`specs/SPEC_VERSIONS.toml`), in `support`, `help_center`,
/// `voice` order.
#[must_use]
pub fn spec_versions() -> &'static [SpecVersion] {
    generated::registry::SPEC_VERSIONS
}

/// The snapshot metadata for one spec.
#[must_use]
pub fn spec_version(spec: Spec) -> Option<&'static SpecVersion> {
    spec_versions().iter().find(|v| v.spec == spec)
}

/// Look an operation up by bare or qualified id. Returns all candidates when the bare id is ambiguous.
#[must_use]
pub fn find_operations(id: &str) -> Vec<&'static Operation> {
    let (spec, bare) = match id.split_once('.') {
        Some((s, rest)) => (Spec::parse(s), rest),
        None => (None, id),
    };
    operations()
        .iter()
        .filter(|op| op.id.eq_ignore_ascii_case(bare) && spec.is_none_or(|s| op.spec == s))
        .collect()
}

/// Descriptions and flattened request/response schemas for every operation, inflated on first
/// use from the embedded `generated/detail.json.gz` (`zdk api describe --schema`).
pub mod detail {
    use std::io::Read as _;
    use std::sync::OnceLock;

    use serde_json::{Map, Value};

    use super::{Operation, Spec};

    static TABLE: OnceLock<Option<Map<String, Value>>> = OnceLock::new();

    fn inflate() -> Option<Map<String, Value>> {
        let mut json = String::new();
        if let Err(e) =
            flate2::read::GzDecoder::new(super::generated::DETAIL_GZ).read_to_string(&mut json)
        {
            tracing::error!(error = %e, "embedded detail.json.gz is not a valid gzip stream");
            return None;
        }
        match serde_json::from_str::<Value>(&json) {
            Ok(Value::Object(map)) => Some(map),
            Ok(_) => {
                tracing::error!("embedded detail payload is not a JSON object");
                None
            }
            Err(e) => {
                tracing::error!(error = %e, "embedded detail payload is not valid JSON");
                None
            }
        }
    }

    fn table() -> Option<&'static Map<String, Value>> {
        TABLE.get_or_init(inflate).as_ref()
    }

    /// The detail entry for `<spec>.<id>`: an object with `description`, `parameters`
    /// (`name`, `in`, `required`, `description`, `schema`), `request_body` (schema or null) and
    /// `responses` (status code → schema or null). `None` when the operation is unknown.
    #[must_use]
    pub fn describe(spec: Spec, id: &str) -> Option<Value> {
        table()?.get(&format!("{spec}.{id}")).cloned()
    }

    /// [`describe`] for a registry entry.
    #[must_use]
    pub fn describe_op(op: &Operation) -> Option<Value> {
        describe(op.spec, op.id)
    }

    /// Number of operations with a detail entry (equals `operations().len()` when in sync).
    #[must_use]
    pub fn len() -> usize {
        table().map_or(0, Map::len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(id: &str) -> &'static Operation {
        let found = find_operations(id);
        assert_eq!(
            found.len(),
            1,
            "{id}: expected exactly one operation, got {found:?}"
        );
        found[0]
    }

    #[test]
    fn registry_has_every_documented_operation() {
        assert_eq!(operations().len(), 887);
        let per_spec = |s: Spec| operations().iter().filter(|o| o.spec == s).count();
        assert_eq!(per_spec(Spec::Support), 645);
        assert_eq!(per_spec(Spec::HelpCenter), 182);
        assert_eq!(per_spec(Spec::Voice), 60);
        for v in spec_versions() {
            assert_eq!(
                per_spec(v.spec),
                v.operations,
                "{}: SPEC_VERSIONS operation count",
                v.spec
            );
            assert_eq!(v.sha256.len(), 64);
        }
        assert_eq!(spec_versions().len(), 3);
        assert!(spec_version(Spec::Voice).is_some_and(|v| v.url.ends_with("/voice/oas.yaml")));
    }

    #[test]
    fn registry_is_sorted_and_ids_are_unique_per_spec() {
        let ops = operations();
        assert!(
            ops.windows(2)
                .all(|w| (w[0].spec, w[0].path, w[0].method) < (w[1].spec, w[1].path, w[1].method)),
            "OPERATIONS must be strictly sorted by (spec, path, method)"
        );
        let mut seen = std::collections::HashSet::new();
        for o in ops {
            assert!(
                seen.insert((o.spec, o.id)),
                "duplicate {}",
                o.qualified_id()
            );
            assert!(!o.tag.is_empty(), "{} has no tag", o.qualified_id());
            assert!(
                o.path.starts_with('/'),
                "{} path {}",
                o.qualified_id(),
                o.path
            );
            for p in o.params {
                assert!(
                    p.location != ParamIn::Path || p.required,
                    "{}: path param {} must be required",
                    o.qualified_id(),
                    p.name
                );
            }
        }
    }

    #[test]
    fn find_operations_handles_bare_and_qualified_ids() {
        assert_eq!(find_operations("ListTickets").len(), 1);
        assert_eq!(
            find_operations("listtickets").len(),
            1,
            "lookup is case-insensitive"
        );
        assert_eq!(
            find_operations("ListLocales").len(),
            2,
            "ListLocales exists in support and help_center"
        );
        assert_eq!(find_operations("ShowComment").len(), 2);
        assert_eq!(find_operations("support.ListLocales").len(), 1);
        assert_eq!(find_operations("hc.ListLocales").len(), 1);
        assert_eq!(
            find_operations("help_center.ListLocales")[0].qualified_id(),
            "help_center.ListLocales"
        );
        assert!(find_operations("voice.ListLocales").is_empty());
        assert!(find_operations("NoSuchOperation").is_empty());
    }

    #[test]
    fn curated_v0_1_operations_exist_with_scopes() {
        const CURATED: &[&str] = &[
            "ListTickets",
            "ShowTicket",
            "CreateTicket",
            "UpdateTicket",
            "DeleteTicket",
            "CountTickets",
            "ListRecentTickets",
            "RestoreDeletedTicket",
            "DeleteTicketPermanently",
            "ListTicketComments",
            "CountTicketComments",
            "MakeTicketCommentPrivate",
            "RedactStringInComment",
            "ListUsers",
            "SearchUsers",
            "AutocompleteUsers",
            "ShowUser",
            "ShowCurrentUser",
            "ShowUserRelated",
            "CreateUser",
            "CreateOrUpdateUser",
            "UpdateUser",
            "DeleteUser",
            "ListOrganizations",
            "ShowOrganization",
            "SearchOrganizations",
            "AutocompleteOrganizations",
            "CountOrganizations",
            "OrganizationRelated",
            "ListOrganizationTickets",
            "ListOrganizationUsers",
            "CreateOrganization",
            "UpdateOrganization",
            "DeleteOrganization",
            "ListSearchResults",
            "CountSearchResults",
            "ExportSearchResults",
        ];
        for id in CURATED {
            let o = op(id);
            assert_eq!(o.spec, Spec::Support, "{id}");
            assert!(o.scope.is_some(), "{id} has no scope");
        }
        assert_eq!(op("ListTickets").scope, Some("tickets:read"));
        assert_eq!(op("UpdateTicket").scope, Some("tickets:write"));
        assert_eq!(op("ListUsers").scope, Some("users:read"));
        assert_eq!(op("DeleteUser").scope, Some("users:write"));
        assert_eq!(op("ListOrganizations").scope, Some("organizations:read"));
        assert_eq!(op("ListSearchResults").scope, Some("read"));
        assert_eq!(
            op("PutTagsTicket").scope,
            Some("tickets:write"),
            "Tags ops inherit the ticket family from the path"
        );
        assert_eq!(op("ListAuditLogs").scope, Some("auditlogs:read"));
        assert_eq!(
            op("CreateCustomObject").scope,
            Some("write"),
            "custom_objects has no granular write scope"
        );
        assert!(op("ListArticles").scope.is_some_and(|s| s == "hc:read"));
    }

    #[test]
    fn pagination_and_items_key_inference() {
        assert!(matches!(
            op("ListTickets").pagination,
            PageDialect::Dual | PageDialect::Cursor
        ));
        assert_eq!(op("ListTickets").items_key, Some("tickets"));
        assert!(matches!(
            op("ListUsers").pagination,
            PageDialect::Dual | PageDialect::Cursor
        ));
        assert_eq!(op("ListUsers").items_key, Some("users"));
        assert_eq!(op("ListSearchResults").pagination, PageDialect::Offset);
        assert_eq!(op("ListSearchResults").items_key, Some("results"));
        assert_eq!(op("ExportSearchResults").pagination, PageDialect::Cursor);
        assert_eq!(op("ExportSearchResults").items_key, Some("results"));
        assert_eq!(
            op("IncrementalTicketExportTime").pagination,
            PageDialect::Incremental
        );
        assert_eq!(
            op("IncrementalTicketExportCursor").pagination,
            PageDialect::Incremental
        );
        assert_eq!(op("IncrementalTicketExportTime").items_key, Some("tickets"));
        assert_eq!(op("ListTicketAudits").pagination, PageDialect::Audits);
        assert_eq!(op("ListTicketAudits").items_key, Some("audits"));
        assert_eq!(op("ListTicketComments").pagination, PageDialect::Dual);
        assert_eq!(op("ListTicketComments").items_key, Some("comments"));
        assert_eq!(op("ShowTicket").pagination, PageDialect::None);
        assert_eq!(op("ShowTicket").items_key, None);
        assert_eq!(op("CreateTicket").body, BodyKind::Required);
        assert_eq!(op("CreateTicket").pagination, PageDialect::None);
        assert_eq!(op("DeleteTicket").body, BodyKind::None);
        assert!(
            operations()
                .iter()
                .filter(|o| o.method != Method::Get)
                .all(|o| o.pagination == PageDialect::None)
        );
        assert_eq!(operations().iter().filter(|o| o.deprecated).count(), 4);
        assert!(op("ListApiTokens").deprecated);
        let page = op("ListTickets")
            .params
            .iter()
            .find(|p| p.name == "page")
            .expect("page param");
        assert!(
            page.deep_object && page.location == ParamIn::Query && page.ty == ParamType::Object
        );
    }

    #[test]
    fn detail_payload_describes_every_operation() {
        assert_eq!(detail::len(), operations().len());
        let d = detail::describe(Spec::Support, "ListTickets").expect("ListTickets detail");
        assert!(
            d["description"].is_string(),
            "ListTickets only has a summary upstream"
        );
        let comments = detail::describe(Spec::Support, "ListTicketComments")
            .expect("ListTicketComments detail");
        assert!(
            comments["description"]
                .as_str()
                .is_some_and(|s| s.contains("comments"))
        );
        let params = d["parameters"].as_array().expect("parameters");
        assert!(
            params
                .iter()
                .any(|p| p["name"] == "page" && p["in"] == "query")
        );
        assert_eq!(d["request_body"], serde_json::Value::Null);
        assert_eq!(
            d["responses"]["200"]["properties"]["tickets"]["type"],
            "array"
        );
        assert!(
            detail::describe_op(op("CreateTicket")).is_some_and(|d| d["request_body"].is_object())
        );
        assert!(detail::describe(Spec::Voice, "ListTickets").is_none());
        assert_eq!(
            detail::describe(Spec::HelpCenter, "ListLocales")
                .map(|d| d["responses"]["200"].is_object()),
            Some(true)
        );
    }
}
