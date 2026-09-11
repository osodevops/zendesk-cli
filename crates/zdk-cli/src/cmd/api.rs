//! `zdk api` — the escape hatch (PRD §5.2, plan A10): any method and path through the full
//! pipeline (auth, governor, retries, pagination), plus the generated operation registry
//! (`zdk api ops`, `zdk api describe`). The escape hatch never refuses an unknown path; the
//! registry only adds pagination dialect, items key and scope pre-flight when it recognises one.

use std::pin::pin;

use clap::{ArgAction, Args, Subcommand};
use futures_util::StreamExt;
use serde_json::{Value, json};
use zdk_core::api::{self, BodyKind, Method, Operation, PageDialect, ParamIn, ParamType, Spec};
use zdk_core::http::RequestSpec;
use zdk_core::output::{self, OutputFormat};
use zdk_core::pagination::{PageOptions, stream_pages};
use zdk_core::{Result, ZdkError};

use crate::args::body::build_body;
use crate::context::AppContext;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct ApiArgs {
    #[command(subcommand)]
    pub command: Option<ApiCommand>,

    /// HTTP method (GET, POST, PUT, PATCH, DELETE); with a single value, the path (GET)
    #[arg(value_name = "METHOD|PATH")]
    pub first: Option<String>,

    /// Path (/api/v2/...) or an absolute URL on this profile's host or https://status.zendesk.com
    #[arg(value_name = "PATH")]
    pub second: Option<String>,

    /// Request body: inline JSON, @file, or - for stdin
    #[arg(long, value_name = "JSON|@FILE|-")]
    pub data: Option<String>,

    /// Body field: k=v (string) or k:=json (raw); dots nest, e.g. ticket.subject=Hi
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,

    /// Query parameter k=v (repeatable)
    #[arg(long, value_name = "K=V", action = ArgAction::Append)]
    pub query: Vec<String>,

    /// Extra request header k:v (repeatable)
    #[arg(long, value_name = "K:V", action = ArgAction::Append)]
    pub header: Vec<String>,

    /// Print the untouched response body (same as -o raw)
    #[arg(long)]
    pub raw: bool,

    /// Skip the local OAuth-scope pre-flight check
    #[arg(long)]
    pub no_preflight: bool,

    /// Response key holding the records (default: inferred from the registry)
    #[arg(long, value_name = "KEY")]
    pub items_key: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ApiCommand {
    /// List operations from the generated registry (ID METHOD PATH PAGINATION SCOPE)
    Ops(OpsArgs),
    /// Show one operation: parameters, body, scope, pagination (--schema adds JSON schemas)
    Describe(DescribeArgs),
}

#[derive(Debug, Args)]
pub struct OpsArgs {
    /// Case-insensitive regex matched against id, path, tag and summary
    #[arg(long, value_name = "REGEX")]
    pub grep: Option<String>,
    /// Only this spec: support, help_center (hc), voice (talk)
    #[arg(long, value_name = "SPEC")]
    pub spec: Option<String>,
    /// Only this tag (resource group), e.g. Tickets
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    /// Only this HTTP method
    #[arg(long, value_name = "METHOD")]
    pub method: Option<String>,
    /// Only deprecated operations
    #[arg(long)]
    pub deprecated: bool,
}

#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// Operation id (ListTickets, support.ListTickets) or an HTTP method followed by a path
    #[arg(value_name = "ID|METHOD")]
    pub id: String,
    /// Path, when the first argument is an HTTP method
    #[arg(value_name = "PATH")]
    pub path: Option<String>,
    /// Include the request-body and response schemas
    #[arg(long)]
    pub schema: bool,
}

pub async fn run(args: ApiArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        Some(ApiCommand::Ops(ref ops)) => run_ops(ops, ctx)?,
        Some(ApiCommand::Describe(ref describe)) => run_describe(describe, ctx)?,
        None => run_call(&args, ctx).await?,
    }
    ctx.finish()
}

// ---------------------------------------------------------------------------------------------
// ops / describe
// ---------------------------------------------------------------------------------------------

fn run_ops(o: &OpsArgs, ctx: &AppContext) -> Result<()> {
    let re = o
        .grep
        .as_deref()
        .map(|g| {
            regex::RegexBuilder::new(g)
                .case_insensitive(true)
                .build()
                .map_err(|e| ZdkError::Usage(format!("--grep: invalid regex: {e}")))
        })
        .transpose()?;
    let spec = o
        .spec
        .as_deref()
        .map(|s| {
            Spec::parse(s).ok_or_else(|| {
                ZdkError::Usage(format!(
                    "--spec '{s}' is not one of support, help_center, voice"
                ))
            })
        })
        .transpose()?;
    let method = o
        .method
        .as_deref()
        .map(|m| {
            Method::parse(m).ok_or_else(|| {
                ZdkError::Usage(format!(
                    "--method '{m}' is not one of GET, POST, PUT, PATCH, DELETE"
                ))
            })
        })
        .transpose()?;
    let tag = o.tag.as_deref().map(str::to_ascii_lowercase);

    let rows: Vec<Value> = api::operations()
        .iter()
        .filter(|op| spec.is_none_or(|s| op.spec == s))
        .filter(|op| method.is_none_or(|m| op.method == m))
        .filter(|op| {
            tag.as_deref()
                .is_none_or(|t| op.tag.to_ascii_lowercase().contains(t))
        })
        .filter(|op| !o.deprecated || op.deprecated)
        .filter(|op| {
            re.as_ref().is_none_or(|re| {
                re.is_match(op.id)
                    || re.is_match(op.path)
                    || re.is_match(op.tag)
                    || re.is_match(op.summary)
            })
        })
        .map(op_row)
        .collect();
    ctx.emit(Value::Array(rows), Some("ops"))
}

fn run_describe(d: &DescribeArgs, ctx: &AppContext) -> Result<()> {
    let op = resolve_operation(&d.id, d.path.as_deref())?;
    let mut record = op_row(op);
    let detail = api::detail::describe_op(op);
    let detail_field = |name: &str| {
        detail
            .as_ref()
            .and_then(|d| d.get(name).cloned())
            .unwrap_or(Value::Null)
    };
    if let Value::Object(map) = &mut record {
        map.insert("body".into(), Value::String(body_kind(op.body).into()));
        map.insert(
            "parameters".into(),
            Value::Array(
                op.params
                    .iter()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "in": param_in(p.location),
                            "type": param_type(p.ty),
                            "required": p.required,
                            "deep_object": p.deep_object,
                        })
                    })
                    .collect(),
            ),
        );
        map.insert("description".into(), detail_field("description"));
        if d.schema {
            map.insert("request_body".into(), detail_field("request_body"));
            map.insert("responses".into(), detail_field("responses"));
        }
    }
    ctx.emit(record, None)
}

fn op_row(op: &Operation) -> Value {
    json!({
        "id": op.id,
        "qualified_id": op.qualified_id(),
        "spec": op.spec.as_str(),
        "method": op.method.as_str(),
        "path": op.path,
        "tag": op.tag,
        "summary": op.summary,
        "pagination": op.pagination.as_str(),
        "scope": op.scope,
        "items_key": op.items_key,
        "deprecated": op.deprecated,
    })
}

/// `ListTickets` / `support.ListTickets` / `GET /api/v2/tickets` → the registry entry.
/// Ambiguous bare ids are a usage error listing the candidates; unknown ones are not found.
pub(crate) fn resolve_operation(id: &str, path: Option<&str>) -> Result<&'static Operation> {
    if let Some(path) = path {
        let method = Method::parse(id).ok_or_else(|| {
            ZdkError::Usage(format!(
                "'{id}' is not an HTTP method; use `zdk api describe <OperationId>` or `zdk api describe <METHOD> <path>`"
            ))
        })?;
        return api::match_path(method, path).ok_or_else(|| ZdkError::NotFound {
            resource: "operation".into(),
            id: format!("{method} {path}"),
            request_id: None,
        });
    }
    let found = api::find_operations(id);
    match found.as_slice() {
        [] => Err(ZdkError::NotFound {
            resource: "operation".into(),
            id: id.to_string(),
            request_id: None,
        }),
        [one] => Ok(one),
        many => Err(ZdkError::Usage(format!(
            "'{id}' is ambiguous; use one of: {}",
            many.iter()
                .map(|o| o.qualified_id())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

const fn param_in(location: ParamIn) -> &'static str {
    match location {
        ParamIn::Path => "path",
        ParamIn::Query => "query",
        ParamIn::Header => "header",
    }
}

const fn param_type(ty: ParamType) -> &'static str {
    match ty {
        ParamType::String => "string",
        ParamType::Integer => "integer",
        ParamType::Number => "number",
        ParamType::Boolean => "boolean",
        ParamType::Array => "array",
        ParamType::Object => "object",
    }
}

const fn body_kind(kind: BodyKind) -> &'static str {
    match kind {
        BodyKind::None => "none",
        BodyKind::Optional => "optional",
        BodyKind::Required => "required",
    }
}

// ---------------------------------------------------------------------------------------------
// the escape hatch
// ---------------------------------------------------------------------------------------------

async fn run_call(args: &ApiArgs, ctx: &AppContext) -> Result<()> {
    let (method, path) = parse_target(args.first.as_deref(), args.second.as_deref())?;
    let mut spec = RequestSpec::new(method, path);
    for q in &args.query {
        let (k, v) = q
            .split_once('=')
            .ok_or_else(|| ZdkError::Usage(format!("--query '{q}' must be key=value")))?;
        spec = spec.query(k.trim(), v);
    }
    for h in &args.header {
        let (k, v) = h
            .split_once(':')
            .ok_or_else(|| ZdkError::Usage(format!("--header '{h}' must be Name: value")))?;
        spec = spec.header(k.trim(), v.trim());
    }
    if let Some(body) = build_body(args.data.as_deref(), &args.field)? {
        spec = spec.json(body);
    }
    if let Some(key) = &ctx.settings.idempotency_key {
        spec = spec.idempotency_key(key.clone());
    }
    spec = spec.no_preflight(args.no_preflight);
    spec.resolve_op();

    let raw = args.raw || ctx.output == OutputFormat::Raw;
    let items_key = args
        .items_key
        .clone()
        .or_else(|| spec.op.and_then(|o| o.items_key).map(str::to_string));
    let client = ctx.client().await?;

    if !ctx.settings.all {
        let resp = match client.execute(spec).await {
            Err(ZdkError::DryRun) => return Ok(()),
            other => other?,
        };
        if raw {
            output::raw::write(&resp.body);
            return Ok(());
        }
        let body = resp.value()?;
        let records = items_key
            .as_deref()
            .and_then(|k| body.get(k))
            .and_then(Value::as_array)
            .cloned();
        return match records {
            Some(items) => ctx.emit(Value::Array(items), None),
            None => ctx.emit(body, None),
        };
    }

    // --paginate / --all: the dialect comes from the registry; an unknown path gets the
    // cursor walker as a best effort (it stops after one page when nothing looks paginated).
    let dialect = spec.op.map_or(PageDialect::Cursor, |o| o.pagination);
    let opts = PageOptions::from_settings(&ctx.settings);
    let mut pages = pin!(stream_pages(
        client,
        spec,
        dialect,
        items_key,
        opts,
        ctx.cancel.clone()
    ));

    if ctx.settings.output.jq.is_some() {
        // A jq filter sees the whole result, so buffer.
        let mut items = Vec::new();
        while let Some(page) = pages.next().await {
            match page {
                Ok(p) => items.extend(p.items),
                Err(ZdkError::DryRun) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        return ctx.emit(Value::Array(items), None);
    }

    let render = ctx.render_options(None);
    let mut sink = output::sink(ctx.output, &render);
    let mut started = false;
    while let Some(page) = pages.next().await {
        let page = match page {
            Ok(p) => p,
            Err(ZdkError::DryRun) => return Ok(()),
            Err(e) => return Err(e),
        };
        if raw {
            output::json::write_value(&page.body, true);
            continue;
        }
        if !started {
            sink.begin()?;
            started = true;
        }
        for item in page.items {
            sink.item(output::project(item, &render)?)?;
        }
    }
    if started {
        sink.end()?;
    }
    Ok(())
}

/// `(GET, path)` from `zdk api <path>` or `(METHOD, path)` from `zdk api <METHOD> <path>`.
pub(crate) fn parse_target(first: Option<&str>, second: Option<&str>) -> Result<(Method, String)> {
    match (first, second) {
        (Some(m), Some(p)) => {
            let method = Method::parse(m).ok_or_else(|| {
                ZdkError::Usage(format!(
                    "'{m}' is not an HTTP method (GET, POST, PUT, PATCH, DELETE)"
                ))
            })?;
            Ok((method, normalize_path(p)))
        }
        (Some(only), None) => {
            if Method::parse(only).is_some() {
                return Err(ZdkError::Usage(format!(
                    "missing path after {}: zdk api {} /api/v2/...",
                    only.to_ascii_uppercase(),
                    only.to_ascii_uppercase()
                )));
            }
            Ok((Method::Get, normalize_path(only)))
        }
        (None, _) => Err(ZdkError::Usage(
            "usage: zdk api [METHOD] <path> | zdk api ops [--grep RE] | zdk api describe <id | METHOD path>"
                .into(),
        )),
    }
}

fn normalize_path(p: &str) -> String {
    let lower = p.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_default_to_get_and_normalise_paths() {
        assert_eq!(
            parse_target(Some("api/v2/users/me"), None).unwrap(),
            (Method::Get, "/api/v2/users/me".to_string())
        );
        assert_eq!(
            parse_target(Some("post"), Some("/api/v2/tickets")).unwrap(),
            (Method::Post, "/api/v2/tickets".to_string())
        );
        assert_eq!(
            parse_target(
                Some("GET"),
                Some("https://status.zendesk.com/api/v2/incidents.json")
            )
            .unwrap()
            .1,
            "https://status.zendesk.com/api/v2/incidents.json"
        );
        assert_eq!(
            parse_target(Some("FROB"), Some("/x"))
                .unwrap_err()
                .exit_code(),
            2
        );
        assert_eq!(parse_target(Some("POST"), None).unwrap_err().exit_code(), 2);
        assert_eq!(parse_target(None, None).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn operations_resolve_by_id_or_method_and_path() {
        assert_eq!(
            resolve_operation("ListTickets", None).unwrap().id,
            "ListTickets"
        );
        assert_eq!(
            resolve_operation("hc.ListLocales", None).unwrap().spec,
            Spec::HelpCenter
        );
        assert_eq!(
            resolve_operation("GET", Some("/api/v2/tickets/42.json"))
                .unwrap()
                .id,
            "ShowTicket"
        );
        let err = resolve_operation("ListLocales", None).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("support.ListLocales"), "{err}");
        assert_eq!(
            resolve_operation("NoSuchOp", None).unwrap_err().exit_code(),
            5
        );
        assert_eq!(
            resolve_operation("GET", Some("/api/v2/nope"))
                .unwrap_err()
                .exit_code(),
            5
        );
        assert_eq!(
            resolve_operation("FROB", Some("/api/v2/tickets"))
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
