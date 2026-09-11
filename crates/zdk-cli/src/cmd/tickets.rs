//! `zdk tickets` — the core resource (PRD §8.1, plan A10).
//!
//! Reads: `list` (cursor, or a compiled search query once any filter is given; `--all` with
//! filters routes through `search/export`), `get`, `show` (transcript), `count`, `recent`.
//! Writes: `create`, `update`, `reply`/`note`, `solve`/`close`/`reopen`, `assign`, `delete`,
//! `restore`, `permanently-delete`. Every write honours `--dry-run`; the destructive ones
//! confirm (naming the profile) unless `--yes`.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args, Subcommand};
use serde_json::{Map, Value, json};
use zdk_core::api::curated::search::{self as search_api, count_value};
use zdk_core::api::curated::{comments as comments_api, tickets as api, unwrap_key};
use zdk_core::output::{self, OutputFormat, sideload};
use zdk_core::util::time::{format_local, parse_timestamp};
use zdk_core::{Result, ZdkError};

use crate::args::body::{edit_text, read_text_source, resource_body};
use crate::args::field::{apply_fields, parse_ticket_custom_field};
use crate::args::filters::TicketFilters;
use crate::args::ids::{UserRef, resolve_user};
use crate::args::list::{ListRun, collect_all, emit_count, fetch_single, send, stream_list};
use crate::context::AppContext;

const PRESET: Option<&str> = Some("tickets");
const NOTE_TEMPLATE: &str =
    "\n# Write the comment above. Lines starting with '#' are dropped; an empty message aborts.\n";

#[derive(Debug, Args)]
pub struct TicketsArgs {
    #[command(subcommand)]
    pub command: TicketsCommand,
}

#[derive(Debug, Subcommand)]
pub enum TicketsCommand {
    /// List tickets (cursor pages); any filter compiles to a search query (offset, capped at
    /// 1,000 results — add --all to walk search/export instead)
    List(ListArgs),
    /// Show one ticket (--with-comments embeds the conversation)
    Get(GetArgs),
    /// Render one ticket as a conversation transcript (JSON: ticket with comments embedded)
    Show(ShowArgs),
    /// Count tickets (with filters: the search count)
    Count(CountArgs),
    /// Tickets recently viewed by the current agent
    Recent,
    /// Create a ticket from flags, --file or stdin
    Create(CreateArgs),
    /// Update fields, tags and custom fields
    Update(UpdateArgs),
    /// Add a public reply (--internal for a private note)
    Reply(CommentArgs),
    /// Add an internal note (--public to make it visible to the requester)
    Note(CommentArgs),
    /// Set status solved, optionally with a comment
    Solve(StatusArgs),
    /// Set status closed, optionally with a comment
    Close(StatusArgs),
    /// Set status open, optionally with a comment
    Reopen(StatusArgs),
    /// Assign to an agent (me, an id or an email) and optionally a group
    Assign(AssignArgs),
    /// Soft-delete a ticket (restorable for 30 days); asks unless --yes
    Delete(IdArg),
    /// Restore a soft-deleted ticket
    Restore(IdArg),
    /// Permanently delete a soft-deleted ticket (irreversible); asks unless --yes
    PermanentlyDelete(IdArg),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// External id (server-side filter; a search term when combined with other filters)
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// Sort field: updated_at, created_at, id, status (search: priority, ticket_type too)
    #[arg(long, value_name = "FIELD")]
    pub sort_by: Option<String>,

    /// Sort order: asc or desc
    #[arg(long, value_name = "ORDER")]
    pub sort_order: Option<String>,

    #[command(flatten)]
    pub filters: TicketFilters,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    /// Ticket id
    pub id: u64,
    /// Embed every comment (with authors) under `comments`
    #[arg(long)]
    pub with_comments: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Ticket id
    pub id: u64,
}

#[derive(Debug, Args)]
pub struct CountArgs {
    #[command(flatten)]
    pub filters: TicketFilters,
}

#[derive(Debug, Args)]
pub struct IdArg {
    /// Ticket id
    pub id: u64,
}

#[derive(Debug, Args)]
#[command(group = ArgGroup::new("comment_src").args(["comment", "comment_file"]))]
pub struct CreateArgs {
    /// Subject
    #[arg(long, value_name = "TEXT")]
    pub subject: Option<String>,

    /// First comment (the ticket description)
    #[arg(long, value_name = "TEXT")]
    pub comment: Option<String>,

    /// First comment read from a file (- for stdin)
    #[arg(long, value_name = "PATH")]
    pub comment_file: Option<PathBuf>,

    /// Make the first comment private (default: public)
    #[arg(long)]
    pub internal: bool,

    /// Requester: an email (created on the fly), a user id, or me
    #[arg(long, value_name = "EMAIL|ID|ME")]
    pub requester: Option<String>,

    /// Requester display name, for a new end user given by email
    #[arg(long, value_name = "NAME")]
    pub requester_name: Option<String>,

    /// Priority: low, normal, high, urgent
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<String>,

    /// Type: problem, incident, question, task
    #[arg(long = "type", value_name = "TYPE")]
    pub ticket_type: Option<String>,

    /// Initial status
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Tag (repeatable)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub tag: Vec<String>,

    /// Group id
    #[arg(long, value_name = "ID")]
    pub group_id: Option<u64>,

    /// Assignee: me, a user id, or an email
    #[arg(long, value_name = "ME|ID|EMAIL")]
    pub assignee: Option<String>,

    /// Ticket form id
    #[arg(long, value_name = "ID")]
    pub form_id: Option<u64>,

    /// Brand id
    #[arg(long, value_name = "ID")]
    pub brand_id: Option<u64>,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// Custom field <field id>=<value> or <id>:=<json> (repeatable)
    #[arg(long, value_name = "ID=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// Any ticket attribute k=v or k:=json (dots nest; applied last)
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,

    /// JSON document: a ticket object or {"ticket": {...}} (- for stdin); flags merge on top
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Read the JSON document from stdin
    #[arg(long)]
    pub from_stdin: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Ticket id
    pub id: u64,

    /// Status: new, open, pending, hold, solved, closed
    #[arg(long, value_name = "STATUS")]
    pub status: Option<String>,

    /// Priority: low, normal, high, urgent
    #[arg(long, value_name = "PRIORITY")]
    pub priority: Option<String>,

    /// Type: problem, incident, question, task
    #[arg(long = "type", value_name = "TYPE")]
    pub ticket_type: Option<String>,

    /// Subject
    #[arg(long, value_name = "TEXT")]
    pub subject: Option<String>,

    /// Assignee: me, a user id, or an email
    #[arg(long, value_name = "ME|ID|EMAIL")]
    pub assignee: Option<String>,

    /// Group id
    #[arg(long, value_name = "ID")]
    pub group_id: Option<u64>,

    /// Add a tag without touching the others (repeatable)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub add_tag: Vec<String>,

    /// Remove a tag (repeatable)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub remove_tag: Vec<String>,

    /// Custom field <field id>=<value> or <id>:=<json> (repeatable)
    #[arg(long, value_name = "ID=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// Any ticket attribute k=v or k:=json (dots nest; applied last)
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,

    /// Optimistic concurrency: fail with 409 if the ticket changed since --updated-stamp
    #[arg(long, requires = "updated_stamp")]
    pub safe_update: bool,

    /// The ticket's updated_at as last read (with --safe-update)
    #[arg(long, value_name = "TIMESTAMP")]
    pub updated_stamp: Option<String>,
}

#[derive(Debug, Args)]
#[command(group = ArgGroup::new("text").required(true).args(["body", "body_file", "editor"]))]
pub struct CommentArgs {
    /// Ticket id
    pub id: u64,

    /// Comment text
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,

    /// Comment text from a file (- for stdin)
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,

    /// Compose in $VISUAL / $EDITOR
    #[arg(long)]
    pub editor: bool,

    /// Visible to the requester
    #[arg(long, conflicts_with = "internal")]
    pub public: bool,

    /// Agents only
    #[arg(long)]
    pub internal: bool,

    /// Post as this agent (defaults to the authenticated user)
    #[arg(long, value_name = "ID")]
    pub author_id: Option<u64>,
}

#[derive(Debug, Args)]
#[command(group = ArgGroup::new("text").args(["body", "body_file"]))]
pub struct StatusArgs {
    /// Ticket id
    pub id: u64,

    /// Comment to add with the status change
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,

    /// Comment from a file (- for stdin)
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<PathBuf>,

    /// Make the comment an internal note (default: public reply)
    #[arg(long)]
    pub internal: bool,
}

#[derive(Debug, Args)]
pub struct AssignArgs {
    /// Ticket id
    pub id: u64,

    /// Assignee: me, a user id, or an email
    #[arg(long, value_name = "ME|ID|EMAIL")]
    pub to: String,

    /// Also move the ticket into this group
    #[arg(long, value_name = "ID")]
    pub group_id: Option<u64>,
}

pub async fn run(args: TicketsArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        TicketsCommand::List(a) => list(a, ctx).await?,
        TicketsCommand::Get(a) => get(a, ctx).await?,
        TicketsCommand::Show(a) => show(a, ctx).await?,
        TicketsCommand::Count(a) => count(a, ctx).await?,
        TicketsCommand::Recent => {
            stream_list(ctx, ListRun::new(api::recent(), PRESET)).await?;
        }
        TicketsCommand::Create(a) => create(a, ctx).await?,
        TicketsCommand::Update(a) => update(a, ctx).await?,
        TicketsCommand::Reply(a) => comment(a, ctx, true).await?,
        TicketsCommand::Note(a) => comment(a, ctx, false).await?,
        TicketsCommand::Solve(a) => status(a, ctx, "solved").await?,
        TicketsCommand::Close(a) => status(a, ctx, "closed").await?,
        TicketsCommand::Reopen(a) => status(a, ctx, "open").await?,
        TicketsCommand::Assign(a) => assign(a, ctx).await?,
        TicketsCommand::Delete(a) => delete(a.id, ctx).await?,
        TicketsCommand::Restore(a) => restore(a.id, ctx).await?,
        TicketsCommand::PermanentlyDelete(a) => permanently_delete(a.id, ctx).await?,
    }
    ctx.finish()
}

// ---------------------------------------------------------------------------------------------
// reads
// ---------------------------------------------------------------------------------------------

async fn list(a: ListArgs, ctx: &AppContext) -> Result<()> {
    if !a.filters.is_active() {
        let spec = api::list(&api::ListOptions {
            sort_by: a.sort_by,
            sort_order: a.sort_order,
            external_id: a.external_id,
            sideload: ctx.settings.sideload.clone(),
        });
        return stream_list(ctx, ListRun::new(spec, PRESET)).await;
    }

    let mut query = a.filters.to_query(chrono::Utc::now())?;
    query.external_id = a.external_id;
    let compiled = query.compile();
    if !ctx.settings.sideload.is_empty() {
        output::warn(
            "warning: --sideload is ignored with filters (the search API has no sideloads); use `zdk tickets get <id> --sideload …` per ticket",
        );
    }
    if ctx.settings.all {
        let spec = search_api::export(&compiled, "ticket");
        return stream_list(ctx, ListRun::new(spec, PRESET)).await;
    }
    if let Some(by) = &a.sort_by
        && !search_api::SORT_FIELDS.contains(&by.as_str())
    {
        return Err(ZdkError::Usage(format!(
            "--sort-by '{by}' is not one of {} for a filtered list",
            search_api::SORT_FIELDS.join(", ")
        )));
    }
    let spec = search_api::results(&compiled, a.sort_by.as_deref(), a.sort_order.as_deref());
    stream_list(ctx, ListRun::new(spec, PRESET)).await
}

async fn get(a: GetArgs, ctx: &AppContext) -> Result<()> {
    if !a.with_comments {
        return fetch_single(
            ctx,
            api::show(a.id, &ctx.settings.sideload),
            "ticket",
            PRESET,
        )
        .await;
    }
    let Some(mut ticket) = fetch_ticket(ctx, a.id, &ctx.settings.sideload).await? else {
        return Ok(());
    };
    let Some(comments) = fetch_comments(ctx, a.id).await? else {
        return Ok(());
    };
    if let Some(map) = ticket.as_object_mut() {
        map.insert("comments".into(), Value::Array(comments));
    }
    ctx.emit(ticket, PRESET)
}

async fn show(a: ShowArgs, ctx: &AppContext) -> Result<()> {
    let sideload = vec!["users".to_string(), "groups".into(), "organizations".into()];
    let Some(mut ticket) = fetch_ticket(ctx, a.id, &sideload).await? else {
        return Ok(());
    };
    let Some(comments) = fetch_comments(ctx, a.id).await? else {
        return Ok(());
    };
    if ctx.output == OutputFormat::Table && ctx.settings.output.jq.is_none() {
        output::write_stdout(transcript(&ticket, &comments).as_bytes());
        return Ok(());
    }
    if let Some(map) = ticket.as_object_mut() {
        map.insert("comments".into(), Value::Array(comments));
    }
    ctx.emit(ticket, PRESET)
}

async fn count(a: CountArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    if a.filters.is_active() {
        let query = a.filters.to_query(chrono::Utc::now())?.compile();
        let Some(body) = send(&client, search_api::count(&query)).await? else {
            return Ok(());
        };
        let n = count_value(&body)
            .ok_or_else(|| ZdkError::Other("search/count returned no numeric count".into()))?;
        return emit_count(ctx, n, None);
    }
    let Some(body) = send(&client, api::count()).await? else {
        return Ok(());
    };
    let n = count_value(&body)
        .ok_or_else(|| ZdkError::Other("tickets/count returned no numeric count".into()))?;
    let refreshed = body
        .pointer("/count/refreshed_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    emit_count(ctx, n, refreshed.as_deref())
}

/// The ticket object with sideloads joined; `None` under `--dry-run`.
async fn fetch_ticket(ctx: &AppContext, id: u64, sideload: &[String]) -> Result<Option<Value>> {
    let client = ctx.client().await?;
    Ok(send(&client, api::show(id, sideload))
        .await?
        .map(|body| sideload::embed_single(body, "ticket")))
}

/// Every comment of a ticket, authors joined; `None` under `--dry-run`.
async fn fetch_comments(ctx: &AppContext, id: u64) -> Result<Option<Vec<Value>>> {
    let spec = comments_api::list(
        id,
        &comments_api::ListOptions {
            include_users: true,
            include_inline_images: false,
            sort_order: None,
        },
    );
    collect_all(ctx, spec, true).await
}

/// The human transcript: a header block, then one block per comment.
pub(crate) fn transcript(ticket: &Value, comments: &[Value]) -> String {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("-").to_string();
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64);
    let local = |v: &Value, k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .map_or_else(|| s(v, k), |t| format_local(&t))
    };
    let person = |v: &Value, key: &str, id_key: &str| match v.get(key) {
        Some(p) if p.is_object() => {
            let name = s(p, "name");
            match p.get("email").and_then(Value::as_str) {
                Some(e) => format!("{name} <{e}>"),
                None => name,
            }
        }
        _ => n(v, id_key).map_or_else(|| "-".into(), |id| id.to_string()),
    };
    let named = |v: &Value, key: &str, id_key: &str| match v.get(key) {
        Some(p) if p.is_object() => s(p, "name"),
        _ => n(v, id_key).map_or_else(|| "-".into(), |id| id.to_string()),
    };
    let tags = ticket
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "-".into());

    let mut out = String::new();
    let _ = writeln!(
        out,
        "#{}  {}",
        n(ticket, "id").map_or_else(|| "?".into(), |id| id.to_string()),
        s(ticket, "subject")
    );
    let _ = writeln!(
        out,
        "status: {}   priority: {}   type: {}",
        s(ticket, "status"),
        s(ticket, "priority"),
        s(ticket, "type")
    );
    let _ = writeln!(
        out,
        "requester: {}   assignee: {}",
        person(ticket, "requester", "requester_id"),
        person(ticket, "assignee", "assignee_id")
    );
    let _ = writeln!(
        out,
        "group: {}   organization: {}",
        named(ticket, "group", "group_id"),
        named(ticket, "organization", "organization_id")
    );
    let _ = writeln!(out, "tags: {tags}");
    let _ = writeln!(
        out,
        "created: {}   updated: {}",
        local(ticket, "created_at"),
        local(ticket, "updated_at")
    );
    for (i, c) in comments.iter().enumerate() {
        let visibility = if c.get("public").and_then(Value::as_bool).unwrap_or(true) {
            "public"
        } else {
            "internal"
        };
        let body = c
            .get("plain_body")
            .or_else(|| c.get("body"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim_end();
        let _ = writeln!(
            out,
            "\n#{}  {}  {}  [{visibility}]\n{body}",
            i + 1,
            named(c, "author", "author_id"),
            local(c, "created_at")
        );
    }
    out
}

// ---------------------------------------------------------------------------------------------
// writes
// ---------------------------------------------------------------------------------------------

async fn create(a: CreateArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let mut flags = Map::new();
    let comment_text = read_text_source(a.comment.as_deref(), a.comment_file.as_deref())?;
    if comment_text.is_none() && a.file.is_none() && !a.from_stdin {
        return Err(ZdkError::Usage(
            "a ticket needs --comment or --comment-file (or a --file / --from-stdin document)"
                .into(),
        ));
    }
    if let Some(text) = comment_text {
        flags.insert("comment".into(), api::comment(&text, !a.internal, None));
    }
    put_str(&mut flags, "subject", a.subject);
    put_str(&mut flags, "priority", a.priority);
    put_str(&mut flags, "type", a.ticket_type);
    put_str(&mut flags, "status", a.status);
    put_str(&mut flags, "external_id", a.external_id);
    put_u64(&mut flags, "group_id", a.group_id);
    put_u64(&mut flags, "ticket_form_id", a.form_id);
    put_u64(&mut flags, "brand_id", a.brand_id);
    if !a.tag.is_empty() {
        flags.insert("tags".into(), json!(a.tag));
    }
    if !a.custom_field.is_empty() {
        flags.insert(
            "custom_fields".into(),
            api::custom_fields(&parse_custom_fields(&a.custom_field)?),
        );
    }
    if let Some(r) = &a.requester {
        let r = UserRef::parse(r)?;
        if let Some(email) = r.email() {
            let mut req = json!({ "email": email });
            if let Some(name) = &a.requester_name
                && let Some(m) = req.as_object_mut()
            {
                m.insert("name".into(), Value::String(name.clone()));
            }
            flags.insert("requester".into(), req);
        } else {
            if a.requester_name.is_some() {
                return Err(ZdkError::Usage(
                    "--requester-name only applies to a requester given by email".into(),
                ));
            }
            flags.insert("requester_id".into(), resolve_user(&client, &r).await?);
        }
    } else if a.requester_name.is_some() {
        return Err(ZdkError::Usage(
            "--requester-name needs --requester <email>".into(),
        ));
    }
    if let Some(assignee) = &a.assignee {
        let r = UserRef::parse(assignee)?;
        flags.insert("assignee_id".into(), resolve_user(&client, &r).await?);
    }

    let ticket = resource_body(
        "ticket",
        a.file.as_deref(),
        a.from_stdin,
        Value::Object(flags),
        &a.field,
    )?;
    let Some(body) = send(&client, api::create(ticket)).await? else {
        return Ok(());
    };
    let created = unwrap_key(body, "ticket");
    if ctx.output == OutputFormat::Table
        && let Some(id) = created.get("id").and_then(Value::as_u64)
    {
        output::warn(&format!("Created ticket #{id}"));
    }
    ctx.emit(created, PRESET)
}

async fn update(a: UpdateArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let mut ticket = Map::new();
    put_str(&mut ticket, "status", a.status);
    put_str(&mut ticket, "priority", a.priority);
    put_str(&mut ticket, "type", a.ticket_type);
    put_str(&mut ticket, "subject", a.subject);
    put_str(&mut ticket, "external_id", a.external_id);
    put_u64(&mut ticket, "group_id", a.group_id);
    if let Some(assignee) = &a.assignee {
        let r = UserRef::parse(assignee)?;
        ticket.insert("assignee_id".into(), resolve_user(&client, &r).await?);
    }
    if !a.custom_field.is_empty() {
        ticket.insert(
            "custom_fields".into(),
            api::custom_fields(&parse_custom_fields(&a.custom_field)?),
        );
    }
    let mut ticket = Value::Object(ticket);
    apply_fields(&mut ticket, &a.field)?;
    let has_changes = ticket.as_object().is_some_and(|m| !m.is_empty());
    if !has_changes && a.add_tag.is_empty() && a.remove_tag.is_empty() {
        return Err(ZdkError::Usage(
            "nothing to update: pass --status, --priority, --assignee, --add-tag, --field, … (see --help)".into(),
        ));
    }
    if a.safe_update
        && let (Some(stamp), Some(map)) = (&a.updated_stamp, ticket.as_object_mut())
    {
        map.insert("safe_update".into(), Value::Bool(true));
        map.insert("updated_stamp".into(), Value::String(stamp.clone()));
    }

    let mut result: Option<Value> = None;
    if has_changes {
        result = send(&client, api::update(a.id, ticket))
            .await?
            .map(|b| unwrap_key(b, "ticket"));
    }
    let mut tags_result: Option<Value> = None;
    if !a.add_tag.is_empty() {
        tags_result = send(&client, api::add_tags(a.id, &a.add_tag)).await?;
    }
    if !a.remove_tag.is_empty() {
        tags_result = send(&client, api::remove_tags(a.id, &a.remove_tag)).await?;
    }
    if client.is_dry_run() {
        return Ok(());
    }
    match (result, tags_result) {
        (Some(mut t), tags) => {
            if let (Some(tags), Some(map)) =
                (tags.as_ref().and_then(|t| t.get("tags")), t.as_object_mut())
            {
                map.insert("tags".into(), tags.clone());
            }
            ctx.emit(t, PRESET)
        }
        (None, Some(tags)) => {
            let tags = tags.get("tags").cloned().unwrap_or(Value::Null);
            ctx.emit_or_line(
                json!({ "id": a.id, "tags": tags }),
                &format!("Updated tags on ticket #{}", a.id),
            )
        }
        (None, None) => Ok(()),
    }
}

async fn comment(a: CommentArgs, ctx: &AppContext, reply: bool) -> Result<()> {
    let public = if reply { !a.internal } else { a.public };
    let text = match read_text_source(a.body.as_deref(), a.body_file.as_deref())? {
        Some(t) => t,
        None if a.editor => edit_text(ctx.env.editor.as_deref(), NOTE_TEMPLATE)?,
        None => {
            return Err(ZdkError::Usage(
                "pass --body, --body-file or --editor".into(),
            ));
        }
    };
    if text.trim().is_empty() {
        return Err(ZdkError::Usage("the comment body is empty".into()));
    }
    let body = json!({ "comment": api::comment(&text, public, a.author_id) });
    put_ticket(
        ctx,
        a.id,
        body,
        &format!(
            "Added {} to ticket #{}",
            if public { "reply" } else { "note" },
            a.id
        ),
    )
    .await
}

async fn status(a: StatusArgs, ctx: &AppContext, status: &str) -> Result<()> {
    let mut body = json!({ "status": status });
    if let Some(text) = read_text_source(a.body.as_deref(), a.body_file.as_deref())?
        && let Some(map) = body.as_object_mut()
    {
        map.insert("comment".into(), api::comment(&text, !a.internal, None));
    }
    put_ticket(
        ctx,
        a.id,
        body,
        &format!("Ticket #{} is now {status}", a.id),
    )
    .await
}

async fn assign(a: AssignArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let to = UserRef::parse(&a.to)?;
    let mut body = json!({ "assignee_id": resolve_user(&client, &to).await? });
    if let Some(g) = a.group_id
        && let Some(map) = body.as_object_mut()
    {
        map.insert("group_id".into(), Value::from(g));
    }
    put_ticket(
        ctx,
        a.id,
        body,
        &format!("Assigned ticket #{} to {}", a.id, a.to),
    )
    .await
}

/// `PUT /api/v2/tickets/{id}` with `body`, then print the updated ticket.
async fn put_ticket(ctx: &AppContext, id: u64, body: Value, line: &str) -> Result<()> {
    let client = ctx.client().await?;
    let Some(resp) = send(&client, api::update(id, body)).await? else {
        return Ok(());
    };
    if ctx.output == OutputFormat::Table {
        output::warn(line);
    }
    ctx.emit(unwrap_key(resp, "ticket"), PRESET)
}

async fn delete(id: u64, ctx: &AppContext) -> Result<()> {
    if !ctx.settings.dry_run && !ctx.confirm(&format!("delete ticket #{id}"))? {
        return Err(ZdkError::Other("aborted".into()));
    }
    let client = ctx.client().await?;
    if send(&client, api::delete(id)).await?.is_none() {
        return Ok(());
    }
    ctx.emit_or_line(
        json!({ "id": id, "deleted": true }),
        &format!("Deleted ticket #{id} (restorable for 30 days with `zdk tickets restore {id}`)"),
    )
}

async fn restore(id: u64, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::restore(id)).await? else {
        return Ok(());
    };
    let value = if body.is_null() {
        json!({ "id": id, "restored": true })
    } else {
        body
    };
    ctx.emit_or_line(value, &format!("Restored ticket #{id}"))
}

async fn permanently_delete(id: u64, ctx: &AppContext) -> Result<()> {
    if !ctx.settings.dry_run
        && !ctx.confirm(&format!(
            "permanently delete ticket #{id} (irreversible: it cannot be restored afterwards)"
        ))?
    {
        return Err(ZdkError::Other("aborted".into()));
    }
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::delete_permanently(id)).await? else {
        return Ok(());
    };
    let value = if body.is_null() {
        json!({ "id": id, "permanently_deleted": true })
    } else {
        body
    };
    ctx.emit_or_line(value, &format!("Permanently deleted ticket #{id}"))
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn put_str(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(v) = value {
        map.insert(key.to_string(), Value::String(v));
    }
}

fn put_u64(map: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if let Some(v) = value {
        map.insert(key.to_string(), Value::from(v));
    }
}

fn parse_custom_fields(raw: &[String]) -> Result<Vec<(u64, Value)>> {
    raw.iter().map(|s| parse_ticket_custom_field(s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_renders_header_and_comments() {
        let body: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/tickets/ticket_sideloaded.json"
        ))
        .unwrap();
        let ticket = sideload::embed_single(body, "ticket");
        let comments_body: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/tickets/comments.json"
        ))
        .unwrap();
        let mut comments = comments_body["comments"].as_array().cloned().unwrap();
        sideload::embed_page(&mut comments, &comments_body);
        let text = transcript(&ticket, &comments);
        assert!(text.starts_with("#1  API latency on eu-west-1\n"), "{text}");
        assert!(text.contains("status: open   priority: high   type: incident"));
        assert!(text.contains(
            "requester: Grace Hopper <grace@example.com>   assignee: Ada Lovelace <ada@example.com>"
        ));
        assert!(text.contains("group: Platform Support   organization: Acme Corp"));
        assert!(text.contains("tags: latency, eu-west-1"));
        assert!(text.contains("#1  Grace Hopper  "));
        assert!(text.contains("  [public]\nRequests to the ticketing API"));
        assert!(text.contains("#2  Ada Lovelace  "));
        assert!(text.contains("  [internal]\nInvestigating"));
        assert!(text.contains("#3  Ada Lovelace"));
        // Without sideloads the ids are shown.
        let bare = json!({"id": 9, "subject": "s", "requester_id": 5});
        let text = transcript(
            &bare,
            &[json!({"author_id": 7, "body": "b", "public": false})],
        );
        assert!(text.contains("requester: 5   assignee: -"), "{text}");
        assert!(text.contains("#1  7  -  [internal]\nb\n"), "{text}");
        assert!(text.contains("tags: -"));
    }

    #[test]
    fn custom_field_lists_and_scalars() {
        let cf = parse_custom_fields(&["1=a".into(), "2:=false".into()]).unwrap();
        assert_eq!(
            api::custom_fields(&cf),
            json!([{"id": 1, "value": "a"}, {"id": 2, "value": false}])
        );
        assert_eq!(
            parse_custom_fields(&["x=1".into()])
                .unwrap_err()
                .exit_code(),
            2
        );
        let mut m = Map::new();
        put_str(&mut m, "a", None);
        put_u64(&mut m, "b", Some(1));
        assert_eq!(Value::Object(m), json!({"b": 1}));
    }
}
