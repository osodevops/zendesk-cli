//! Inference of the registry fields the spec does not state directly: pagination dialect,
//! `items_key`, OAuth scope — plus the `scope_map.toml` / `overrides.toml` inputs.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::walk::{Document, RawOperation};
use super::{BodyKind, Method, Op, PageDialect, ParamDef, ParamLocation, ParamType, SpecName};

/// Build the emitted [`Op`] for one raw operation. The boolean is `true` when the operation's
/// tag has no entry in the scope map (scope emitted as `None`).
pub fn build_op(
    spec: SpecName,
    raw: &RawOperation,
    doc: &Document,
    scope_map: &ScopeMap,
) -> (Op, bool) {
    let tag = raw.tags.first().cloned().unwrap_or_default();
    let params = raw
        .params
        .iter()
        .map(|p| ParamDef {
            name: p.name.clone(),
            location: p.location,
            ty: p.ty,
            required: p.required,
            deep_object: p.deep_object,
        })
        .collect();
    let body = match &raw.body {
        None => BodyKind::None,
        Some(b) if b.required => BodyKind::Required,
        Some(_) => BodyKind::Optional,
    };
    let (scope, tag_missing) = scope_map.scope_for(&tag, raw.method, &raw.path);
    let op = Op {
        spec,
        id: raw.id.clone(),
        method: raw.method,
        path: raw.path.clone(),
        tag,
        summary: raw.summary.clone(),
        params,
        body,
        items_key: items_key(raw, doc),
        pagination: pagination(raw),
        scope,
        deprecated: raw.deprecated,
    };
    (op, tag_missing)
}

/// Pagination dialect from path and query-parameter shape. Non-GET operations never paginate.
pub fn pagination(op: &RawOperation) -> PageDialect {
    if op.method != Method::Get {
        return PageDialect::None;
    }
    let path = op.path.as_str();
    if path.starts_with("/api/v2/incremental/") {
        return PageDialect::Incremental;
    }
    if path.starts_with("/api/v2/ticket_audits") {
        return PageDialect::Audits;
    }
    match path {
        "/api/v2/search" => return PageDialect::Offset,
        "/api/v2/search/export" => return PageDialect::Cursor,
        _ => {}
    }

    let query = |name: &str| {
        op.params
            .iter()
            .find(|p| p.location == ParamLocation::Query && p.name == name)
    };
    let per_page = query("per_page").is_some();
    let bracketed = query("page[size]").is_some() || query("page[after]").is_some();

    if let Some(page) = query("page") {
        if page.one_of {
            // Zendesk's `DualPaginationPage`: `?page=2` or `?page[size]=..&page[after]=..`.
            return PageDialect::Dual;
        }
        if page.deep_object {
            // `CursorPaginationPage` (`page[size]`/`page[after]`/`page[before]` as one deepObject);
            // a sibling `per_page` means the offset form is documented too.
            return if per_page {
                PageDialect::Dual
            } else {
                PageDialect::Cursor
            };
        }
        if page.ty == ParamType::Integer {
            return PageDialect::Offset;
        }
    }
    if bracketed {
        return PageDialect::Cursor;
    }
    PageDialect::None
}

/// The array property of the 200/201 response that holds the records.
///
/// Preference among the array-typed properties: the last static path segment, the snake_cased
/// tag, the last word of the tag, `results`, then the first array property.
pub fn items_key(op: &RawOperation, doc: &Document) -> Option<String> {
    let schema = op
        .responses
        .get("200")
        .or_else(|| op.responses.get("201"))?
        .as_ref()?;
    let arrays = doc.array_properties(schema);
    if arrays.is_empty() {
        return None;
    }
    let last_static = op
        .path
        .split('/')
        .rev()
        .find(|seg| !seg.is_empty() && !seg.starts_with('{'))
        .unwrap_or_default();
    let tag_snake = snake_case(op.tags.first().map(String::as_str).unwrap_or_default());
    let tag_last = tag_snake.rsplit('_').next().unwrap_or_default().to_owned();
    for candidate in [
        last_static,
        tag_snake.as_str(),
        tag_last.as_str(),
        "results",
    ] {
        if !candidate.is_empty() && arrays.iter().any(|a| a == candidate) {
            return Some(candidate.to_owned());
        }
    }
    arrays.into_iter().next()
}

/// `Ticket Comments` → `ticket_comments`, `IVR Menus` → `ivr_menus`.
pub fn snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_sep = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('_');
            }
            pending_sep = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

/// `xtask/scope_map.toml`: tag → scope family, plus which families have granular scopes.
#[derive(Debug, Clone, Deserialize)]
pub struct ScopeMap {
    /// Families with granular OAuth scopes (`<family>:read` / `<family>:write`).
    #[serde(default)]
    families: BTreeMap<String, Family>,
    /// First tag of an operation → family name, or `global` for the legacy `read`/`write` scopes.
    tags: BTreeMap<String, String>,
    /// Path prefix → family, consulted only for operations whose tag maps to `global`
    /// (e.g. `Tags`, `Incremental Export` span several resources). Longest prefix wins.
    #[serde(default)]
    path_prefixes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct Family {
    #[serde(default = "yes")]
    read: bool,
    #[serde(default = "yes")]
    write: bool,
}

const fn yes() -> bool {
    true
}

impl ScopeMap {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let map: Self = toml::from_str(text)?;
        for (tag, family) in &map.tags {
            if family != "global" && !map.families.contains_key(family) {
                bail!("tag {tag:?} maps to unknown family {family:?} (add it to [families])");
            }
        }
        for (prefix, family) in &map.path_prefixes {
            if !map.families.contains_key(family) {
                bail!("path prefix {prefix:?} maps to unknown family {family:?}");
            }
        }
        Ok(map)
    }

    /// Family for a tag (`Some("global")` for legacy-scope tags), `None` if unmapped.
    pub fn family_for_tag(&self, tag: &str) -> Option<&str> {
        self.tags.get(tag).map(String::as_str)
    }

    /// `(scope, tag_missing)`.
    pub fn scope_for(&self, tag: &str, method: Method, path: &str) -> (Option<String>, bool) {
        let Some(mut family) = self.family_for_tag(tag) else {
            return (None, true);
        };
        if family == "global"
            && let Some((_, f)) = self
                .path_prefixes
                .iter()
                .filter(|(prefix, _)| path.starts_with(prefix.as_str()))
                .max_by_key(|(prefix, _)| prefix.len())
        {
            family = f;
        }
        let read = method == Method::Get;
        let scope = match self.families.get(family) {
            None => if read { "read" } else { "write" }.to_owned(),
            Some(fam) => match (read, fam.read, fam.write) {
                (true, true, _) => format!("{family}:read"),
                (true, false, _) => "read".to_owned(),
                (false, _, true) => format!("{family}:write"),
                (false, _, false) => "write".to_owned(),
            },
        };
        (Some(scope), false)
    }
}

/// `xtask/overrides.toml`: per-operation corrections applied after inference.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct Overrides {
    entries: BTreeMap<String, Override>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    /// Array property holding the records; `""` clears it.
    items_key: Option<String>,
    /// `none | cursor | offset | dual | incremental | audits`
    pagination: Option<String>,
    /// OAuth scope, e.g. `tickets:read`; `""` clears it.
    scope: Option<String>,
}

impl Overrides {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let o: Self = toml::from_str(text)?;
        for (key, entry) in &o.entries {
            let Some((spec, _)) = key.split_once('.') else {
                bail!("override key {key:?} must be `<spec>.<operationId>`");
            };
            if SpecName::parse(spec).is_none() {
                bail!("override key {key:?}: unknown spec {spec:?}");
            }
            if let Some(p) = &entry.pagination
                && PageDialect::parse(p).is_none()
            {
                bail!("override {key:?}: unknown pagination {p:?}");
            }
        }
        Ok(o)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub fn apply(&self, key: &str, op: &mut Op) -> Result<()> {
        let Some(o) = self.entries.get(key) else {
            return Ok(());
        };
        if let Some(k) = &o.items_key {
            op.items_key = if k.is_empty() { None } else { Some(k.clone()) };
        }
        if let Some(p) = &o.pagination {
            op.pagination = PageDialect::parse(p)
                .with_context(|| format!("override {key:?}: pagination {p:?}"))?;
        }
        if let Some(s) = &o.scope {
            op.scope = if s.is_empty() { None } else { Some(s.clone()) };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_tags() {
        assert_eq!(snake_case("Tickets"), "tickets");
        assert_eq!(snake_case("Ticket Comments"), "ticket_comments");
        assert_eq!(snake_case("IVR Menus"), "ivr_menus");
        assert_eq!(snake_case("Digital lines"), "digital_lines");
        assert_eq!(snake_case("  X  Channel "), "x_channel");
    }

    #[test]
    fn scope_map_rules() {
        let map = ScopeMap::parse(
            r#"
[families]
tickets = {}
users = {}
auditlogs = { write = false }
[tags]
"Tickets" = "tickets"
"Audit Logs" = "auditlogs"
"Tags" = "global"
"Countries" = "global"
[path_prefixes]
"/api/v2/tickets/" = "tickets"
"/api/v2/users/" = "users"
"#,
        )
        .expect("parse");
        let s = |tag, m, p| map.scope_for(tag, m, p).0;
        assert_eq!(
            s("Tickets", Method::Get, "/api/v2/tickets").as_deref(),
            Some("tickets:read")
        );
        assert_eq!(
            s("Tickets", Method::Put, "/api/v2/tickets/{id}").as_deref(),
            Some("tickets:write")
        );
        assert_eq!(
            s("Audit Logs", Method::Get, "/api/v2/audit_logs").as_deref(),
            Some("auditlogs:read")
        );
        assert_eq!(
            s("Audit Logs", Method::Post, "/api/v2/audit_logs").as_deref(),
            Some("write")
        );
        assert_eq!(
            s("Countries", Method::Get, "/api/v2/countries").as_deref(),
            Some("read")
        );
        assert_eq!(
            s("Tags", Method::Get, "/api/v2/tags").as_deref(),
            Some("read")
        );
        assert_eq!(
            s("Tags", Method::Put, "/api/v2/tickets/{id}/tags").as_deref(),
            Some("tickets:write")
        );
        assert_eq!(
            s("Tags", Method::Get, "/api/v2/users/{id}/tags").as_deref(),
            Some("users:read")
        );
        assert_eq!(map.scope_for("Nope", Method::Get, "/x"), (None, true));
        assert!(ScopeMap::parse("[tags]\n\"A\" = \"nope\"\n").is_err());
    }

    #[test]
    fn overrides_validate_keys() {
        assert!(Overrides::parse("[\"support.X\"]\npagination = \"cursor\"\n").is_ok());
        assert!(Overrides::parse("[\"nope.X\"]\npagination = \"cursor\"\n").is_err());
        assert!(Overrides::parse("[\"support.X\"]\npagination = \"sideways\"\n").is_err());
        assert!(Overrides::parse("[\"support.X\"]\nbogus = 1\n").is_err());
        assert!(Overrides::parse("[\"X\"]\nscope = \"read\"\n").is_err());
    }
}
