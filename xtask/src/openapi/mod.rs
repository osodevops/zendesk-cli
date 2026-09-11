//! OpenAPI → operation-registry pipeline: walk the committed specs, infer the bits the
//! CLI needs (pagination, items key, scope), and emit `crates/zdk-core/src/api/generated/`.

pub mod emit;
pub mod infer;
pub mod walk;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::specs::{self, SpecVersions};

/// Which upstream document an operation comes from. Mirrors `zdk_core::api::Spec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpecName {
    Support,
    HelpCenter,
    Voice,
}

impl SpecName {
    pub const ALL: [Self; 3] = [Self::Support, Self::HelpCenter, Self::Voice];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Support => "support",
            Self::HelpCenter => "help_center",
            Self::Voice => "voice",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|n| n.as_str() == s)
    }

    /// Rust path of the matching `zdk_core::api::Spec` variant.
    pub const fn rust(self) -> &'static str {
        match self {
            Self::Support => "Spec::Support",
            Self::HelpCenter => "Spec::HelpCenter",
            Self::Voice => "Spec::Voice",
        }
    }
}

impl fmt::Display for SpecName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// HTTP method. Ordering mirrors `zdk_core::api::Method` so the emitted sort is identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    pub const fn rust(self) -> &'static str {
        match self {
            Self::Get => "Method::Get",
            Self::Post => "Method::Post",
            Self::Put => "Method::Put",
            Self::Patch => "Method::Patch",
            Self::Delete => "Method::Delete",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
}

impl ParamLocation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Header => "header",
        }
    }

    pub const fn rust(self) -> &'static str {
        match self {
            Self::Path => "ParamIn::Path",
            Self::Query => "ParamIn::Query",
            Self::Header => "ParamIn::Header",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParamType {
    String,
    Integer,
    Number,
    Boolean,
    Array,
    Object,
}

impl ParamType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Array => "array",
            Self::Object => "object",
        }
    }

    pub const fn rust(self) -> &'static str {
        match self {
            Self::String => "ParamType::String",
            Self::Integer => "ParamType::Integer",
            Self::Number => "ParamType::Number",
            Self::Boolean => "ParamType::Boolean",
            Self::Array => "ParamType::Array",
            Self::Object => "ParamType::Object",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BodyKind {
    None,
    Optional,
    Required,
}

impl BodyKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }

    pub const fn rust(self) -> &'static str {
        match self {
            Self::None => "BodyKind::None",
            Self::Optional => "BodyKind::Optional",
            Self::Required => "BodyKind::Required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PageDialect {
    None,
    Cursor,
    Offset,
    Dual,
    Incremental,
    Audits,
}

impl PageDialect {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Cursor,
        Self::Offset,
        Self::Dual,
        Self::Incremental,
        Self::Audits,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Cursor => "cursor",
            Self::Offset => "offset",
            Self::Dual => "dual",
            Self::Incremental => "incremental",
            Self::Audits => "audits",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }

    pub const fn rust(self) -> &'static str {
        match self {
            Self::None => "PageDialect::None",
            Self::Cursor => "PageDialect::Cursor",
            Self::Offset => "PageDialect::Offset",
            Self::Dual => "PageDialect::Dual",
            Self::Incremental => "PageDialect::Incremental",
            Self::Audits => "PageDialect::Audits",
        }
    }
}

/// A parameter as it will be emitted into the registry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ParamDef {
    pub name: String,
    pub location: ParamLocation,
    pub ty: ParamType,
    pub required: bool,
    pub deep_object: bool,
}

/// An operation as it will be emitted into the registry.
#[derive(Debug, Clone)]
pub struct Op {
    pub spec: SpecName,
    pub id: String,
    pub method: Method,
    pub path: String,
    pub tag: String,
    pub summary: String,
    pub params: Vec<ParamDef>,
    pub body: BodyKind,
    pub items_key: Option<String>,
    pub pagination: PageDialect,
    pub scope: Option<String>,
    pub deprecated: bool,
}

impl Op {
    /// `support.ListTickets`
    pub fn qualified_id(&self) -> String {
        format!("{}.{}", self.spec, self.id)
    }
}

/// Aggregate numbers printed after a run (and reported by tests).
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub per_spec: BTreeMap<String, usize>,
    pub with_items_key: usize,
    pub with_scope: usize,
    pub deprecated: usize,
    pub pagination: BTreeMap<&'static str, usize>,
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total: usize = self.per_spec.values().sum();
        writeln!(f, "operations: {total}")?;
        for (spec, n) in &self.per_spec {
            writeln!(f, "  {spec}: {n}")?;
        }
        writeln!(f, "with items_key: {}", self.with_items_key)?;
        writeln!(f, "with scope:     {}", self.with_scope)?;
        writeln!(f, "deprecated:     {}", self.deprecated)?;
        writeln!(f, "pagination:")?;
        for d in PageDialect::ALL {
            writeln!(
                f,
                "  {:<12}{}",
                d.as_str(),
                self.pagination.get(d.as_str()).copied().unwrap_or(0)
            )?;
        }
        Ok(())
    }
}

/// Everything one codegen run produces.
#[derive(Debug)]
pub struct Generated {
    pub registry: String,
    pub detail: Vec<u8>,
    pub stats: Stats,
    pub warnings: Vec<String>,
}

/// Walk every committed spec and build the registry + detail payload in memory.
pub fn generate(root: &Path) -> Result<Generated> {
    let versions = specs::load(root)?;
    let scope_map = infer::ScopeMap::load(&root.join("xtask/scope_map.toml"))?;
    let overrides = infer::Overrides::load(&root.join("xtask/overrides.toml"))?;
    generate_with(root, &versions, &scope_map, &overrides)
}

/// [`generate`] with explicit inputs (used by tests to exercise overrides).
pub fn generate_with(
    root: &Path,
    versions: &SpecVersions,
    scope_map: &infer::ScopeMap,
    overrides: &infer::Overrides,
) -> Result<Generated> {
    let mut ops: Vec<Op> = Vec::new();
    let mut detail: BTreeMap<String, Value> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut unmapped_tags: BTreeSet<String> = BTreeSet::new();

    for (spec, entry) in &versions.entries {
        let file = root.join("specs").join(&entry.file);
        let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        let text = String::from_utf8(bytes.clone())
            .with_context(|| format!("{} is not valid UTF-8", file.display()))?;
        let doc = walk::Document::parse(*spec, &text)?;

        let sha = specs::sha256_hex(&bytes);
        if sha != entry.sha256 {
            warnings.push(format!(
                "{spec}: sha256 of {} ({sha}) differs from SPEC_VERSIONS.toml ({}); run `cargo xtask spec-refresh`",
                entry.file, entry.sha256
            ));
        }
        if doc.operations.len() != entry.operations || doc.path_count != entry.paths {
            warnings.push(format!(
                "{spec}: walked {} paths / {} operations but SPEC_VERSIONS.toml says {} / {}",
                doc.path_count,
                doc.operations.len(),
                entry.paths,
                entry.operations
            ));
        }

        for raw in &doc.operations {
            let key = format!("{}.{}", doc.spec, raw.id);
            let (mut op, tag_missing) = infer::build_op(doc.spec, raw, &doc, scope_map);
            if tag_missing {
                unmapped_tags.insert(op.tag.clone());
            }
            overrides.apply(&key, &mut op)?;
            detail.insert(key, detail_entry(raw, &doc));
            ops.push(op);
        }
    }

    for tag in unmapped_tags {
        warnings.push(format!(
            "tag {tag:?} is not mapped in xtask/scope_map.toml; scope emitted as None"
        ));
    }

    let known: BTreeSet<String> = ops.iter().map(Op::qualified_id).collect();
    let unknown: Vec<&String> = overrides.keys().filter(|k| !known.contains(*k)).collect();
    if !unknown.is_empty() {
        bail!("xtask/overrides.toml references unknown operations: {unknown:?}");
    }

    let mut seen: BTreeSet<(SpecName, String)> = BTreeSet::new();
    for op in &ops {
        if !seen.insert((op.spec, op.id.clone())) {
            bail!("duplicate operationId {} in {}", op.id, op.spec);
        }
    }

    ops.sort_by(|a, b| (a.spec, &a.path, a.method).cmp(&(b.spec, &b.path, b.method)));

    let stats = stats(&ops);
    let registry = emit::registry(&ops, versions);
    let detail = emit::detail(&detail)?;
    Ok(Generated {
        registry,
        detail,
        stats,
        warnings,
    })
}

fn stats(ops: &[Op]) -> Stats {
    let mut s = Stats::default();
    for op in ops {
        *s.per_spec.entry(op.spec.to_string()).or_default() += 1;
        s.with_items_key += usize::from(op.items_key.is_some());
        s.with_scope += usize::from(op.scope.is_some());
        s.deprecated += usize::from(op.deprecated);
        *s.pagination.entry(op.pagination.as_str()).or_default() += 1;
    }
    s
}

/// The `detail.json.gz` entry for one operation: description, parameters with schemas,
/// request body schema and per-status response schemas (refs inlined, see
/// [`walk::SCHEMA_INLINE_DEPTH`]).
fn detail_entry(raw: &walk::RawOperation, doc: &walk::Document) -> Value {
    let parameters: Vec<Value> = raw
        .params
        .iter()
        .map(|p| {
            let mut m = Map::new();
            m.insert("name".into(), Value::String(p.name.clone()));
            m.insert("in".into(), Value::String(p.location.as_str().into()));
            m.insert("required".into(), Value::Bool(p.required));
            m.insert("description".into(), Value::String(p.description.clone()));
            m.insert("schema".into(), doc.inline_schema(&p.schema));
            Value::Object(m)
        })
        .collect();
    let request_body = raw
        .body
        .as_ref()
        .and_then(|b| b.schema.as_ref())
        .map_or(Value::Null, |s| doc.inline_schema(s));
    let mut responses = Map::new();
    for (code, schema) in &raw.responses {
        responses.insert(
            code.clone(),
            schema
                .as_ref()
                .map_or(Value::Null, |s| doc.inline_schema(s)),
        );
    }
    let mut m = Map::new();
    m.insert("description".into(), Value::String(raw.description.clone()));
    m.insert("parameters".into(), Value::Array(parameters));
    m.insert("request_body".into(), request_body);
    m.insert("responses".into(), Value::Object(responses));
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::specs::workspace_root;

    fn load_docs() -> (SpecVersions, Vec<walk::Document>) {
        let root = workspace_root();
        let versions = specs::load(&root).expect("SPEC_VERSIONS.toml");
        let docs = versions
            .entries
            .iter()
            .map(|(spec, entry)| {
                walk::Document::parse_file(*spec, &root.join("specs").join(&entry.file))
                    .expect("spec parses")
            })
            .collect();
        (versions, docs)
    }

    #[test]
    fn walker_parses_all_committed_specs_and_counts_match_spec_versions() {
        let (versions, docs) = load_docs();
        assert_eq!(docs.len(), 3);
        for ((spec, entry), doc) in versions.entries.iter().zip(&docs) {
            assert_eq!(doc.spec, *spec);
            assert_eq!(
                doc.operations.len(),
                entry.operations,
                "{spec}: operation count"
            );
            assert_eq!(doc.path_count, entry.paths, "{spec}: path count");
            assert_eq!(doc.openapi, entry.openapi, "{spec}: openapi version");
            assert_eq!(doc.info_version, entry.info_version, "{spec}: info.version");
            let mut ids = BTreeSet::new();
            for op in &doc.operations {
                assert!(
                    ids.insert(op.id.as_str()),
                    "{spec}: duplicate operationId {}",
                    op.id
                );
                assert!(!op.tags.is_empty(), "{spec}.{} has no tag", op.id);
                assert!(
                    op.path.starts_with('/'),
                    "{spec}.{} path {:?}",
                    op.id,
                    op.path
                );
            }
        }
        let total: usize = docs.iter().map(|d| d.operations.len()).sum();
        assert_eq!(total, 887);
    }

    #[test]
    fn every_tag_in_the_committed_specs_is_mapped_to_a_scope_family() {
        let (_, docs) = load_docs();
        let map = infer::ScopeMap::load(&workspace_root().join("xtask/scope_map.toml"))
            .expect("scope map");
        let mut missing = BTreeSet::new();
        for doc in &docs {
            for op in &doc.operations {
                let tag = op.tags.first().map(String::as_str).unwrap_or_default();
                if map.family_for_tag(tag).is_none() {
                    missing.insert(tag.to_owned());
                }
            }
        }
        assert!(missing.is_empty(), "unmapped tags: {missing:?}");
    }

    #[test]
    fn generate_is_deterministic_and_warning_free_on_committed_inputs() {
        let root = workspace_root();
        let a = generate(&root).expect("generate");
        let b = generate(&root).expect("generate");
        assert_eq!(a.registry, b.registry, "registry.rs differs between runs");
        assert_eq!(a.detail, b.detail, "detail.json.gz differs between runs");
        assert!(a.warnings.is_empty(), "warnings: {:?}", a.warnings);
        let total: usize = a.stats.per_spec.values().sum();
        assert_eq!(total, 887);
        assert_eq!(a.stats.per_spec["support"], 645);
        assert_eq!(a.stats.per_spec["help_center"], 182);
        assert_eq!(a.stats.per_spec["voice"], 60);
        assert_eq!(a.stats.deprecated, 4);
        assert!(
            a.registry
                .starts_with("// @generated by `cargo xtask codegen`")
        );
        assert!(
            a.registry
                .contains("pub static OPERATIONS: &[Operation] = &[")
        );
        assert!(
            a.registry
                .contains("pub static SPEC_VERSIONS: &[SpecVersion] = &[")
        );
        assert!(
            a.registry.lines().count() < 15_000,
            "registry is {} lines",
            a.registry.lines().count()
        );
    }

    #[test]
    fn detail_payload_round_trips_and_has_one_entry_per_operation() {
        use std::io::Read as _;
        let root = workspace_root();
        let g = generate(&root).expect("generate");
        // Fixed gzip header: mtime 0, OS byte 255.
        assert_eq!(&g.detail[..4], &[0x1f, 0x8b, 0x08, 0x00]);
        assert_eq!(&g.detail[4..8], &[0, 0, 0, 0], "gzip mtime must be zero");
        let mut json = String::new();
        flate2::read::GzDecoder::new(g.detail.as_slice())
            .read_to_string(&mut json)
            .expect("inflate");
        let v: Value = serde_json::from_str(&json).expect("json");
        let obj = v.as_object().expect("object");
        assert_eq!(obj.len(), 887);
        let keys: Vec<&String> = obj.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "detail keys must be sorted");
        let lt = &obj["support.ListTickets"];
        assert!(
            lt["description"].is_string(),
            "ListTickets only has a summary upstream"
        );
        assert!(
            obj["support.ListTicketComments"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("comments"))
        );
        assert!(
            lt["parameters"]
                .as_array()
                .is_some_and(|p| p.iter().any(|p| p["name"] == "page"))
        );
        assert!(lt["responses"]["200"]["properties"]["tickets"]["type"] == "array");
        // One level below the response object the Ticket schema is inlined too.
        assert!(lt["responses"]["200"]["properties"]["tickets"]["items"]["properties"].is_object());
        assert!(obj["support.CreateTicket"]["request_body"].is_object());
        assert!(obj["support.DeleteTicket"]["responses"]["204"].is_null());
    }

    #[test]
    fn pagination_inference_on_known_operations() {
        let root = workspace_root();
        let g = generate(&root).expect("generate");
        let find = |id: &str| -> &str {
            let needle = format!("id: {id:?},");
            g.registry
                .lines()
                .find(|l| l.contains(&needle))
                .unwrap_or_else(|| panic!("{id} not emitted"))
        };
        let dialect = |id: &str| -> String {
            // The pagination field sits on the operation's closing line; find the block.
            let lines: Vec<&str> = g.registry.lines().collect();
            let needle = format!("id: {id:?},");
            let start = lines
                .iter()
                .position(|l| l.contains(&needle))
                .expect("op start");
            for l in &lines[start..] {
                if let Some(i) = l.find("pagination: PageDialect::") {
                    let rest = &l[i + "pagination: PageDialect::".len()..];
                    return rest.split(',').next().unwrap_or_default().to_owned();
                }
            }
            panic!("no pagination for {id}")
        };
        assert!(find("ListTickets").contains("Spec::Support"));
        assert!(matches!(dialect("ListTickets").as_str(), "Dual" | "Cursor"));
        assert!(matches!(dialect("ListUsers").as_str(), "Dual" | "Cursor"));
        assert_eq!(dialect("ListSearchResults"), "Offset");
        assert_eq!(dialect("ExportSearchResults"), "Cursor");
        assert_eq!(dialect("IncrementalTicketExportTime"), "Incremental");
        assert_eq!(dialect("IncrementalTicketExportCursor"), "Incremental");
        assert_eq!(dialect("ListTicketAudits"), "Audits");
        assert_eq!(dialect("ListTicketComments"), "Dual");
        assert_eq!(dialect("ListJobStatuses"), "Cursor");
        assert_eq!(dialect("ShowTicket"), "None");
        assert_eq!(dialect("CreateTicket"), "None");
    }

    #[test]
    fn overrides_win_over_inference_and_unknown_keys_are_rejected() {
        let root = workspace_root();
        let versions = specs::load(&root).expect("versions");
        let scope_map =
            infer::ScopeMap::load(&root.join("xtask/scope_map.toml")).expect("scope map");
        let overrides = infer::Overrides::parse(
            r#"
["support.ListTickets"]
items_key = "records"
pagination = "offset"
scope = "read"
"#,
        )
        .expect("overrides parse");
        let g = generate_with(&root, &versions, &scope_map, &overrides).expect("generate");
        let line_block: String = {
            let lines: Vec<&str> = g.registry.lines().collect();
            let start = lines
                .iter()
                .position(|l| l.contains("id: \"ListTickets\","))
                .expect("op");
            lines[start..start + 40].join("\n")
        };
        assert!(line_block.contains("items_key: Some(\"records\")"));
        assert!(line_block.contains("pagination: PageDialect::Offset"));
        assert!(line_block.contains("scope: Some(\"read\")"));

        let bad =
            infer::Overrides::parse("[\"support.NoSuchOp\"]\nscope = \"read\"\n").expect("parse");
        let err =
            generate_with(&root, &versions, &scope_map, &bad).expect_err("unknown op must fail");
        assert!(err.to_string().contains("NoSuchOp"));
    }
}
