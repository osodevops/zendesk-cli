//! `zdk comments` — ticket comments (PRD §8.2, plan A10): `list`, `get`, `count`,
//! `make-private`, `redact`.

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use zdk_core::api::curated::comments as api;
use zdk_core::api::curated::search::count_value;
use zdk_core::{Result, ZdkError};

use crate::args::list::{ListRun, collect_all, emit_count, send, stream_list};
use crate::context::AppContext;

const PRESET: Option<&str> = Some("comments");

#[derive(Debug, Args)]
pub struct CommentsArgs {
    #[command(subcommand)]
    pub command: CommentsCommand,
}

#[derive(Debug, Subcommand)]
pub enum CommentsCommand {
    /// List a ticket's comments (authors joined from the users sideload)
    List(ListArgs),
    /// Show one comment of a ticket
    Get(CommentRef),
    /// Count a ticket's comments
    Count(TicketArg),
    /// Make a public comment private (cannot be undone through the API)
    MakePrivate(CommentRef),
    /// Permanently remove a string from a comment; asks unless --yes
    Redact(RedactArgs),
}

#[derive(Debug, Args)]
pub struct TicketArg {
    /// Ticket id
    pub ticket: u64,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Ticket id
    pub ticket: u64,

    /// Only comments visible to the requester
    #[arg(long)]
    pub public_only: bool,

    /// Include inline images as attachments
    #[arg(long)]
    pub include_inline_images: bool,

    /// asc (oldest first, default) or desc
    #[arg(long, value_name = "ORDER")]
    pub sort_order: Option<String>,
}

#[derive(Debug, Args)]
pub struct CommentRef {
    /// Ticket id
    pub ticket: u64,

    /// Comment id
    #[arg(long, value_name = "ID")]
    pub comment: u64,
}

#[derive(Debug, Args)]
pub struct RedactArgs {
    /// Ticket id
    pub ticket: u64,

    /// Comment id
    #[arg(long, value_name = "ID")]
    pub comment: u64,

    /// The exact text to replace with ▇▇▇ (irreversible)
    #[arg(long, value_name = "TEXT")]
    pub text: String,
}

pub async fn run(args: CommentsArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        CommentsCommand::List(a) => list(a, ctx).await?,
        CommentsCommand::Get(a) => get(a, ctx).await?,
        CommentsCommand::Count(a) => count(a.ticket, ctx).await?,
        CommentsCommand::MakePrivate(a) => make_private(a, ctx).await?,
        CommentsCommand::Redact(a) => redact(a, ctx).await?,
    }
    ctx.finish()
}

fn list_spec(
    ticket: u64,
    include_inline_images: bool,
    sort_order: Option<String>,
) -> Result<zdk_core::http::RequestSpec> {
    if let Some(o) = &sort_order
        && !(o.eq_ignore_ascii_case("asc") || o.eq_ignore_ascii_case("desc"))
    {
        return Err(ZdkError::Usage(format!(
            "--sort-order '{o}' must be asc or desc"
        )));
    }
    Ok(api::list(
        ticket,
        &api::ListOptions {
            include_users: true,
            include_inline_images,
            sort_order: sort_order.map(|s| s.to_ascii_lowercase()),
        },
    ))
}

async fn list(a: ListArgs, ctx: &AppContext) -> Result<()> {
    let spec = list_spec(a.ticket, a.include_inline_images, a.sort_order)?;
    let mut run = ListRun::new(spec, PRESET);
    if a.public_only {
        run = run.keep(is_public);
    }
    stream_list(ctx, run).await
}

fn is_public(c: &Value) -> bool {
    c.get("public").and_then(Value::as_bool).unwrap_or(false)
}

async fn get(a: CommentRef, ctx: &AppContext) -> Result<()> {
    let spec = list_spec(a.ticket, false, None)?;
    let Some(comments) = collect_all(ctx, spec, true).await? else {
        return Ok(());
    };
    let found = comments
        .into_iter()
        .find(|c| c.get("id").and_then(Value::as_u64) == Some(a.comment))
        .ok_or_else(|| ZdkError::NotFound {
            resource: format!("comment on ticket {}", a.ticket),
            id: a.comment.to_string(),
            request_id: None,
        })?;
    ctx.emit(found, PRESET)
}

async fn count(ticket: u64, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::count(ticket)).await? else {
        return Ok(());
    };
    let n = count_value(&body)
        .ok_or_else(|| ZdkError::Other("comments/count returned no numeric count".into()))?;
    let refreshed = body
        .pointer("/count/refreshed_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    emit_count(ctx, n, refreshed.as_deref())
}

async fn make_private(a: CommentRef, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    if send(&client, api::make_private(a.ticket, a.comment))
        .await?
        .is_none()
    {
        return Ok(());
    }
    ctx.emit_or_line(
        json!({ "ticket_id": a.ticket, "comment_id": a.comment, "public": false }),
        &format!(
            "Comment {} on ticket #{} is now private",
            a.comment, a.ticket
        ),
    )
}

async fn redact(a: RedactArgs, ctx: &AppContext) -> Result<()> {
    if a.text.trim().is_empty() {
        return Err(ZdkError::Usage("--text must not be empty".into()));
    }
    if !ctx.settings.dry_run
        && !ctx.confirm(&format!(
            "redact {:?} from comment {} on ticket #{} (irreversible)",
            a.text, a.comment, a.ticket
        ))?
    {
        return Err(ZdkError::Other("aborted".into()));
    }
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::redact(a.ticket, a.comment, &a.text)).await? else {
        return Ok(());
    };
    let value = if body.is_null() {
        json!({ "ticket_id": a.ticket, "comment_id": a.comment, "redacted": true })
    } else {
        body
    };
    ctx.emit_or_line(
        value,
        &format!(
            "Redacted text from comment {} on ticket #{}",
            a.comment, a.ticket
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_filter_and_sort_validation() {
        assert!(is_public(&json!({"public": true})));
        assert!(!is_public(&json!({"public": false})));
        assert!(!is_public(&json!({})));
        assert_eq!(
            list_spec(1, false, Some("sideways".into()))
                .unwrap_err()
                .exit_code(),
            2
        );
        let s = list_spec(1, true, Some("DESC".into())).unwrap();
        assert_eq!(s.query_value("sort"), Some("-created_at"));
        assert_eq!(s.query_value("include"), Some("users"));
        assert_eq!(s.query_value("include_inline_images"), Some("true"));
    }
}
