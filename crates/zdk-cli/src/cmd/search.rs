//! `zdk search` — the unified search API (PRD §4.6, §8.8, plan A10).
//!
//! `search <query>` walks `GET /api/v2/search` (offset; Zendesk caps it at 1,000 results and
//! the paginator warns there); `search count`; `search export --filter-type` walks
//! `GET /api/v2/search/export` (cursor, no cap; `--to` writes NDJSON to a file);
//! `search explain` prints the query `tickets list` would compile from its filter flags.

use std::io::Write;
use std::path::PathBuf;
use std::pin::pin;

use clap::{Args, Subcommand};
use futures_util::StreamExt;
use serde_json::json;
use zdk_core::api::curated::search::{self as api, FILTER_TYPES, SORT_FIELDS, count_value};
use zdk_core::pagination::{PageOptions, stream_pages};
use zdk_core::{Result, ZdkError};

use crate::args::filters::TicketFilters;
use crate::args::list::{ListRun, emit_count, send, stream_list};
use crate::context::AppContext;

const PRESET: Option<&str> = Some("search");

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    after_help = "The search index is eventually consistent: a record written seconds ago may not appear yet.\n\
                  Results are capped at 1,000 even when count says more; use `zdk search export` for the full set."
)]
pub struct SearchArgs {
    #[command(subcommand)]
    pub command: Option<SearchCommand>,

    /// Zendesk search query, e.g. "type:ticket status:open priority:urgent"
    #[arg(value_name = "QUERY", required = true)]
    pub query: Option<String>,

    /// Sort field: updated_at, created_at, priority, status, ticket_type
    #[arg(long, value_name = "FIELD")]
    pub sort_by: Option<String>,

    /// Sort order: asc or desc
    #[arg(long, value_name = "ORDER")]
    pub sort_order: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SearchCommand {
    /// Number of results for a query (no records are fetched)
    Count(QueryArg),
    /// Export every result (cursor pagination, no 1,000 cap); --to writes NDJSON to a file
    Export(ExportArgs),
    /// Print the search query that `tickets list` compiles from these filters
    Explain(Box<ExplainArgs>),
}

#[derive(Debug, Args)]
pub struct QueryArg {
    /// Zendesk search query
    #[arg(value_name = "QUERY")]
    pub query: String,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Zendesk search query
    #[arg(value_name = "QUERY")]
    pub query: String,

    /// Result type: ticket, user, organization, group (inferred from a type: term when absent)
    #[arg(long, value_name = "TYPE")]
    pub filter_type: Option<String>,

    /// Write NDJSON to this file (walks every page) and print the record count
    #[arg(long, value_name = "PATH")]
    pub to: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ExplainArgs {
    /// Extra query terms appended verbatim
    #[arg(value_name = "QUERY")]
    pub query: Option<String>,

    #[command(flatten)]
    pub filters: TicketFilters,
}

pub async fn run(args: SearchArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        Some(SearchCommand::Count(a)) => count(&a.query, ctx).await?,
        Some(SearchCommand::Export(a)) => export(a, ctx).await?,
        Some(SearchCommand::Explain(a)) => explain(&a, ctx)?,
        None => {
            let query = args.query.ok_or_else(|| {
                ZdkError::Usage(
                    "usage: zdk search <QUERY> | zdk search count|export|explain …".into(),
                )
            })?;
            if let Some(by) = &args.sort_by
                && !SORT_FIELDS.contains(&by.as_str())
            {
                return Err(ZdkError::Usage(format!(
                    "--sort-by '{by}' is not one of {}",
                    SORT_FIELDS.join(", ")
                )));
            }
            if let Some(o) = &args.sort_order
                && !(o.eq_ignore_ascii_case("asc") || o.eq_ignore_ascii_case("desc"))
            {
                return Err(ZdkError::Usage(format!(
                    "--sort-order '{o}' must be asc or desc"
                )));
            }
            let spec = api::results(&query, args.sort_by.as_deref(), args.sort_order.as_deref());
            stream_list(ctx, ListRun::new(spec, PRESET)).await?;
        }
    }
    ctx.finish()
}

async fn count(query: &str, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::count(query)).await? else {
        return Ok(());
    };
    let n = count_value(&body)
        .ok_or_else(|| ZdkError::Other("search/count returned no numeric count".into()))?;
    emit_count(ctx, n, None)
}

/// `--filter-type`, or the query's own `type:` term; a usage error otherwise.
pub(crate) fn filter_type(flag: Option<&str>, query: &str) -> Result<String> {
    if let Some(t) = flag {
        let t = t.trim().to_ascii_lowercase();
        return if FILTER_TYPES.contains(&t.as_str()) {
            Ok(t)
        } else {
            Err(ZdkError::Usage(format!(
                "--filter-type '{t}' is not one of {}",
                FILTER_TYPES.join(", ")
            )))
        };
    }
    api::infer_filter_type(query).ok_or_else(|| {
        ZdkError::Usage(format!(
            "search export needs --filter-type ticket|user|organization|group when the query has no single type: term (query: {query:?})"
        ))
    })
}

async fn export(a: ExportArgs, ctx: &AppContext) -> Result<()> {
    let filter = filter_type(a.filter_type.as_deref(), &a.query)?;
    let spec = api::export(&a.query, &filter);
    let Some(path) = a.to else {
        return stream_list(ctx, ListRun::new(spec, PRESET)).await;
    };

    // --to: every page to the file as NDJSON, then a count line.
    let client = ctx.client().await?;
    let opts = PageOptions {
        all: true,
        ..PageOptions::from_settings(&ctx.settings)
    };
    let mut pages = pin!(stream_pages(
        client,
        spec,
        zdk_core::api::PageDialect::Cursor,
        Some("results".into()),
        opts,
        ctx.cancel.clone()
    ));
    let file = std::fs::File::create(&path)
        .map_err(|e| ZdkError::Usage(format!("--to: cannot create {}: {e}", path.display())))?;
    let mut out = std::io::BufWriter::new(file);
    let mut written: u64 = 0;
    while let Some(page) = pages.next().await {
        let page = match page {
            Ok(p) => p,
            Err(ZdkError::DryRun) => return Ok(()),
            Err(e) => return Err(e),
        };
        for item in page.items {
            serde_json::to_writer(&mut out, &item)?;
            out.write_all(b"\n")?;
            written += 1;
        }
    }
    out.flush()?;
    ctx.emit_or_line(
        json!({ "exported": written, "path": path, "filter_type": filter }),
        &format!(
            "Exported {written} {filter} record(s) to {}",
            path.display()
        ),
    )
}

fn explain(a: &ExplainArgs, ctx: &AppContext) -> Result<()> {
    let mut q = a.filters.to_query(chrono::Utc::now())?;
    if let Some(raw) = a.query.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        q.extra_terms = api::tokenize(raw);
    }
    if q.is_empty() {
        return Err(ZdkError::Usage(
            "nothing to explain: pass ticket filter flags (--status, --assignee, …) or a query"
                .into(),
        ));
    }
    let compiled = q.compile();
    let endpoint = if ctx.settings.all {
        "/api/v2/search/export"
    } else {
        "/api/v2/search"
    };
    ctx.emit_or_line(
        json!({ "query": compiled, "endpoint": endpoint }),
        &compiled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_type_flag_or_inference() {
        assert_eq!(filter_type(Some("Ticket"), "x").unwrap(), "ticket");
        assert_eq!(filter_type(None, "type:user role:agent").unwrap(), "user");
        assert_eq!(
            filter_type(Some("article"), "x").unwrap_err().exit_code(),
            2
        );
        let err = filter_type(None, "status:open").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("--filter-type"), "{err}");
    }
}
