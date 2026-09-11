//! `cargo xtask spec-diff`: compare the upstream OpenAPI documents with the committed snapshots.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::openapi::walk::{Document, RawOperation};
use crate::openapi::{BodyKind, Method};
use crate::specs::{self, SpecEntry};

#[derive(Debug, Serialize)]
pub struct Report {
    /// `true` when any spec drifted (operations or bytes).
    pub drift: bool,
    pub specs: Vec<SpecReport>,
}

#[derive(Debug, Serialize)]
pub struct SpecReport {
    pub spec: String,
    pub url: String,
    pub committed: Snapshot,
    pub upstream: Snapshot,
    pub sha256_changed: bool,
    pub added: Vec<OpRef>,
    pub removed: Vec<OpRef>,
    pub changed: Vec<ChangedOp>,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub sha256: String,
    pub openapi: String,
    pub info_version: String,
    pub paths: usize,
    pub operations: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpRef {
    pub id: String,
    pub method: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ChangedOp {
    #[serde(flatten)]
    pub op: OpRef,
    pub reasons: Vec<String>,
}

impl SpecReport {
    pub fn has_op_changes(&self) -> bool {
        !(self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty())
    }
}

fn op_ref(op: &RawOperation) -> OpRef {
    OpRef {
        id: op.id.clone(),
        method: op.method.as_str().to_owned(),
        path: op.path.clone(),
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct ParamSig {
    location: &'static str,
    name: String,
    required: bool,
    deep_object: bool,
    ty: &'static str,
}

fn param_sigs(op: &RawOperation) -> Vec<ParamSig> {
    let mut v: Vec<ParamSig> = op
        .params
        .iter()
        .map(|p| ParamSig {
            location: p.location.as_str(),
            name: p.name.clone(),
            required: p.required,
            deep_object: p.deep_object,
            ty: p.ty.as_str(),
        })
        .collect();
    v.sort();
    v
}

fn body_kind(op: &RawOperation) -> BodyKind {
    match &op.body {
        None => BodyKind::None,
        Some(b) if b.required => BodyKind::Required,
        Some(_) => BodyKind::Optional,
    }
}

/// Human-readable reasons why `new` differs from `old` (empty when the signature is the same).
fn reasons(old: &RawOperation, new: &RawOperation) -> Vec<String> {
    let mut out = Vec::new();
    if old.id != new.id {
        out.push(format!("operationId renamed `{}` → `{}`", old.id, new.id));
    }
    let (a, b) = (param_sigs(old), param_sigs(new));
    for p in b.iter().filter(|p| {
        !a.iter()
            .any(|q| q.name == p.name && q.location == p.location)
    }) {
        out.push(format!("parameter added `{}` ({})", p.name, p.location));
    }
    for p in a.iter().filter(|p| {
        !b.iter()
            .any(|q| q.name == p.name && q.location == p.location)
    }) {
        out.push(format!("parameter removed `{}` ({})", p.name, p.location));
    }
    for p in &b {
        if let Some(q) = a
            .iter()
            .find(|q| q.name == p.name && q.location == p.location)
            && q != p
        {
            out.push(format!(
                "parameter changed `{}` ({}): {} {}{} → {} {}{}",
                p.name,
                p.location,
                q.ty,
                if q.required { "required" } else { "optional" },
                if q.deep_object { " deepObject" } else { "" },
                p.ty,
                if p.required { "required" } else { "optional" },
                if p.deep_object { " deepObject" } else { "" },
            ));
        }
    }
    let (ob, nb) = (body_kind(old), body_kind(new));
    if ob != nb {
        out.push(format!("request body {} → {}", ob.as_str(), nb.as_str()));
    }
    let content_type = |op: &RawOperation| {
        op.body
            .as_ref()
            .map(|b| b.content_type.clone())
            .unwrap_or_default()
    };
    let (oc, nc) = (content_type(old), content_type(new));
    if oc != nc && !oc.is_empty() && !nc.is_empty() {
        out.push(format!("request body content type `{oc}` → `{nc}`"));
    }
    if old.deprecated != new.deprecated {
        out.push(if new.deprecated {
            "now deprecated".to_owned()
        } else {
            "no longer deprecated".to_owned()
        });
    }
    out
}

/// Compare two walked documents (committed vs upstream) operation by operation, keyed by
/// `(method, path)`.
pub fn compare(
    committed: &Document,
    upstream: &Document,
) -> (Vec<OpRef>, Vec<OpRef>, Vec<ChangedOp>) {
    let key = |op: &RawOperation| (op.path.clone(), op.method);
    let old: BTreeMap<(String, Method), &RawOperation> =
        committed.operations.iter().map(|o| (key(o), o)).collect();
    let new: BTreeMap<(String, Method), &RawOperation> =
        upstream.operations.iter().map(|o| (key(o), o)).collect();

    let mut added = Vec::new();
    let mut changed = Vec::new();
    for (k, op) in &new {
        match old.get(k) {
            None => added.push(op_ref(op)),
            Some(prev) => {
                let r = reasons(prev, op);
                if !r.is_empty() {
                    changed.push(ChangedOp {
                        op: op_ref(op),
                        reasons: r,
                    });
                }
            }
        }
    }
    let removed: Vec<OpRef> = old
        .iter()
        .filter(|(k, _)| !new.contains_key(*k))
        .map(|(_, op)| op_ref(op))
        .collect();
    (added, removed, changed)
}

fn snapshot(entry: &SpecEntry) -> Snapshot {
    Snapshot {
        sha256: entry.sha256.clone(),
        openapi: entry.openapi.clone(),
        info_version: entry.info_version.clone(),
        paths: entry.paths,
        operations: entry.operations,
    }
}

/// Download every spec and build the [`Report`].
pub fn run(root: &Path) -> Result<Report> {
    let versions = specs::load(root)?;
    let mut reports = Vec::new();
    for (spec, entry) in &versions.entries {
        eprintln!("{spec}: fetching {}", entry.url);
        let committed = Document::parse_file(*spec, &root.join("specs").join(&entry.file))?;
        let fetched = specs::fetch(*spec, entry)?;
        let (added, removed, changed) = compare(&committed, &fetched.doc);
        reports.push(SpecReport {
            spec: spec.to_string(),
            url: entry.url.clone(),
            committed: snapshot(entry),
            upstream: Snapshot {
                sha256: fetched.sha256.clone(),
                openapi: fetched.doc.openapi.clone(),
                info_version: fetched.doc.info_version.clone(),
                paths: fetched.doc.path_count,
                operations: fetched.doc.operations.len(),
            },
            sha256_changed: fetched.sha256 != entry.sha256,
            added,
            removed,
            changed,
        });
    }
    let drift = reports
        .iter()
        .any(|r| r.sha256_changed || r.has_op_changes());
    Ok(Report {
        drift,
        specs: reports,
    })
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Plain-text rendering for terminals.
pub fn render_text(report: &Report) -> String {
    let mut out = String::new();
    for s in &report.specs {
        let _ = writeln!(
            out,
            "{}: committed {} ops (sha {}) vs upstream {} ops (sha {}) — +{} -{} ~{}{}",
            s.spec,
            s.committed.operations,
            short(&s.committed.sha256),
            s.upstream.operations,
            short(&s.upstream.sha256),
            s.added.len(),
            s.removed.len(),
            s.changed.len(),
            if s.sha256_changed && !s.has_op_changes() {
                " (bytes changed, operations identical)"
            } else {
                ""
            }
        );
        for a in &s.added {
            let _ = writeln!(out, "  + {} {} ({})", a.method, a.path, a.id);
        }
        for r in &s.removed {
            let _ = writeln!(out, "  - {} {} ({})", r.method, r.path, r.id);
        }
        for c in &s.changed {
            let _ = writeln!(out, "  ~ {} {} ({})", c.op.method, c.op.path, c.op.id);
            for reason in &c.reasons {
                let _ = writeln!(out, "      {reason}");
            }
        }
    }
    let _ = writeln!(
        out,
        "{}",
        if report.drift {
            "drift detected"
        } else {
            "no drift: committed snapshots match upstream"
        }
    );
    out
}

/// Markdown rendering suitable as a pull-request body.
pub fn render_summary(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Zendesk OpenAPI drift report");
    let _ = writeln!(out);
    if !report.drift {
        let _ = writeln!(
            out,
            "No drift: the committed snapshots in `specs/` match upstream."
        );
        return out;
    }
    let _ = writeln!(
        out,
        "| Spec | Committed | Upstream | Added | Removed | Changed |"
    );
    let _ = writeln!(out, "|---|---|---|---:|---:|---:|");
    for s in &report.specs {
        let _ = writeln!(
            out,
            "| `{}` | {} ops, v{} (`{}`) | {} ops, v{} (`{}`) | {} | {} | {} |",
            s.spec,
            s.committed.operations,
            s.committed.info_version,
            short(&s.committed.sha256),
            s.upstream.operations,
            s.upstream.info_version,
            short(&s.upstream.sha256),
            s.added.len(),
            s.removed.len(),
            s.changed.len()
        );
    }
    for s in &report.specs {
        if !s.sha256_changed {
            continue;
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "### `{}` — [{}]({})", s.spec, s.url, s.url);
        if !s.has_op_changes() {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "Document bytes changed (descriptions, examples or schemas) but the operation set is identical."
            );
            continue;
        }
        let mut section = |title: &str, items: &[OpRef]| {
            if items.is_empty() {
                return;
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "**{title}**");
            let _ = writeln!(out);
            for i in items {
                let _ = writeln!(out, "- `{} {}` (`{}`)", i.method, i.path, i.id);
            }
        };
        section("Added", &s.added);
        section("Removed", &s.removed);
        if !s.changed.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "**Changed**");
            let _ = writeln!(out);
            for c in &s.changed {
                let _ = writeln!(
                    out,
                    "- `{} {}` (`{}`): {}",
                    c.op.method,
                    c.op.path,
                    c.op.id,
                    c.reasons.join("; ")
                );
            }
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Regenerate with `cargo xtask spec-refresh && cargo xtask codegen`."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::SpecName;

    const OLD: &str = r#"
openapi: 3.0.0
info: { version: "1" }
paths:
  /a:
    get:
      operationId: GetA
      parameters:
        - { name: q, in: query, schema: { type: string } }
      responses: { "200": { description: ok } }
  /b:
    delete:
      operationId: DeleteB
      responses: { "204": { description: ok } }
  /c:
    post:
      operationId: CreateC
      requestBody: { content: { application/json: { schema: { type: object } } } }
      responses: { "201": { description: ok } }
"#;
    const NEW: &str = r#"
openapi: 3.0.0
info: { version: "2" }
paths:
  /a:
    get:
      operationId: GetA
      deprecated: true
      parameters:
        - { name: q, in: query, required: true, schema: { type: string } }
        - { name: r, in: query, schema: { type: integer } }
      responses: { "200": { description: ok } }
  /c:
    post:
      operationId: CreateC
      requestBody: { required: true, content: { application/json: { schema: { type: object } } } }
      responses: { "201": { description: ok } }
  /d:
    get:
      operationId: GetD
      responses: { "200": { description: ok } }
"#;

    #[test]
    fn compares_operation_sets() {
        let old = Document::parse(SpecName::Support, OLD).expect("old");
        let new = Document::parse(SpecName::Support, NEW).expect("new");
        let (added, removed, changed) = compare(&old, &new);
        assert_eq!(
            added.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["GetD"]
        );
        assert_eq!(
            removed.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            vec!["DeleteB"]
        );
        assert_eq!(changed.len(), 2);
        let a = changed.iter().find(|c| c.op.id == "GetA").expect("GetA");
        assert!(a.reasons.iter().any(|r| r.contains("parameter added `r`")));
        assert!(
            a.reasons
                .iter()
                .any(|r| r.contains("parameter changed `q`"))
        );
        assert!(a.reasons.iter().any(|r| r == "now deprecated"));
        let c = changed
            .iter()
            .find(|c| c.op.id == "CreateC")
            .expect("CreateC");
        assert_eq!(c.reasons, vec!["request body optional → required"]);

        let (added, removed, changed) = compare(&old, &old);
        assert!(added.is_empty() && removed.is_empty() && changed.is_empty());
    }

    #[test]
    fn renders_summary_and_json() {
        let old = Document::parse(SpecName::Support, OLD).expect("old");
        let new = Document::parse(SpecName::Support, NEW).expect("new");
        let (added, removed, changed) = compare(&old, &new);
        let report = Report {
            drift: true,
            specs: vec![SpecReport {
                spec: "support".into(),
                url: "https://example.invalid/oas.yaml".into(),
                committed: Snapshot {
                    sha256: "a".repeat(64),
                    openapi: "3.0.0".into(),
                    info_version: "1".into(),
                    paths: 3,
                    operations: 3,
                },
                upstream: Snapshot {
                    sha256: "b".repeat(64),
                    openapi: "3.0.0".into(),
                    info_version: "2".into(),
                    paths: 3,
                    operations: 3,
                },
                sha256_changed: true,
                added,
                removed,
                changed,
            }],
        };
        let md = render_summary(&report);
        assert!(md.contains("| `support` | 3 ops, v1"));
        assert!(md.contains("- `GET /d` (`GetD`)"));
        assert!(md.contains("- `DELETE /b` (`DeleteB`)"));
        assert!(md.contains("request body optional → required"));
        let text = render_text(&report);
        assert!(text.contains("+1 -1 ~2"));
        let json = serde_json::to_value(&report).expect("json");
        assert_eq!(json["drift"], true);
        assert_eq!(json["specs"][0]["changed"][0]["id"], "GetA");
    }
}
