//! `zdk orgs` — organizations (PRD §8.5, plan A10).

use std::path::PathBuf;

use clap::{ArgAction, Args, Subcommand};
use serde_json::{Map, Value, json};
use zdk_core::api::PageDialect;
use zdk_core::api::curated::organizations as api;
use zdk_core::api::curated::search::count_value;
use zdk_core::api::curated::unwrap_key;
use zdk_core::output::{self, OutputFormat};
use zdk_core::{Result, ZdkError};

use crate::args::body::resource_body;
use crate::args::list::{ListRun, emit_count, fetch_single, send, stream_list};
use crate::cmd::users::{named_fields, put_str};
use crate::context::AppContext;

const PRESET: Option<&str> = Some("organizations");

#[derive(Debug, Args)]
pub struct OrgsArgs {
    #[command(subcommand)]
    pub command: OrgsCommand,
}

#[derive(Debug, Subcommand)]
pub enum OrgsCommand {
    /// List organizations
    List,
    /// Show one organization
    Get(IdArg),
    /// Find organizations by external id or exact name
    Search(SearchArgs),
    /// Autocomplete organizations by the start of a name
    Autocomplete(NameArg),
    /// Count organizations
    Count,
    /// Ticket and user counts for an organization
    Related(IdArg),
    /// Tickets of an organization
    Tickets(IdArg),
    /// Users of an organization
    Users(IdArg),
    /// Create an organization from flags, --file or stdin
    Create(CreateArgs),
    /// Update an organization (tags are added/removed without touching the rest)
    Update(UpdateArgs),
    /// Delete an organization; asks unless --yes
    Delete(IdArg),
}

#[derive(Debug, Args)]
pub struct IdArg {
    /// Organization id
    pub id: u64,
}

#[derive(Debug, Args)]
pub struct NameArg {
    /// Name prefix
    pub name: String,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// External id
    #[arg(long, value_name = "ID", required_unless_present = "name")]
    pub external_id: Option<String>,

    /// Exact organization name
    #[arg(long, value_name = "NAME", conflicts_with = "external_id")]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Name (required unless a document provides it)
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Email domain whose users join automatically (repeatable)
    #[arg(long, value_name = "DOMAIN", action = ArgAction::Append)]
    pub domain: Vec<String>,

    /// Tag (repeatable)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub tag: Vec<String>,

    /// Members can see each other's tickets
    #[arg(long)]
    pub shared_tickets: bool,

    /// Members can comment on each other's tickets (implies shared tickets)
    #[arg(long)]
    pub shared_comments: bool,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// Organization field key=value or key:=json (repeatable; under organization_fields)
    #[arg(long, value_name = "KEY=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// Any organization attribute k=v or k:=json (dots nest; applied last)
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,

    /// JSON document: an organization object or {"organization": {...}} (- for stdin)
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Read the JSON document from stdin
    #[arg(long)]
    pub from_stdin: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Organization id
    pub id: u64,

    /// Name
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Add a tag without touching the others (repeatable)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub add_tag: Vec<String>,

    /// Remove a tag (repeatable)
    #[arg(long, value_name = "TAG", action = ArgAction::Append)]
    pub remove_tag: Vec<String>,

    /// Replace the domain list (repeatable)
    #[arg(long, value_name = "DOMAIN", action = ArgAction::Append)]
    pub domain: Vec<String>,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// Organization field key=value or key:=json (repeatable; under organization_fields)
    #[arg(long, value_name = "KEY=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// Any organization attribute k=v or k:=json (dots nest; applied last)
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,
}

pub async fn run(args: OrgsArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        OrgsCommand::List => {
            let spec = api::list(&ctx.settings.sideload);
            stream_list(ctx, ListRun::new(spec, PRESET)).await?;
        }
        OrgsCommand::Get(a) => {
            fetch_single(
                ctx,
                api::show(a.id, &ctx.settings.sideload),
                "organization",
                PRESET,
            )
            .await?;
        }
        OrgsCommand::Search(a) => {
            let spec = match (a.external_id, a.name) {
                (Some(ext), _) => api::search_external_id(&ext),
                (None, Some(name)) => api::search_name(&name),
                (None, None) => {
                    return Err(ZdkError::Usage("pass --external-id or --name".into()));
                }
            };
            stream_list(ctx, ListRun::new(spec, PRESET)).await?;
        }
        OrgsCommand::Autocomplete(a) => {
            stream_list(ctx, ListRun::new(api::autocomplete(&a.name), PRESET)).await?;
        }
        OrgsCommand::Count => count(ctx).await?,
        OrgsCommand::Related(a) => {
            fetch_single(ctx, api::related(a.id), "organization_related", None).await?;
        }
        OrgsCommand::Tickets(a) => {
            let spec = api::tickets(a.id, &ctx.settings.sideload);
            let run = ListRun::new(spec, Some("tickets")).dialect(PageDialect::Cursor);
            stream_list(ctx, run).await?;
        }
        OrgsCommand::Users(a) => {
            let spec = api::users(a.id, &ctx.settings.sideload);
            stream_list(ctx, ListRun::new(spec, Some("users"))).await?;
        }
        OrgsCommand::Create(a) => create(a, ctx).await?,
        OrgsCommand::Update(a) => update(a, ctx).await?,
        OrgsCommand::Delete(a) => delete(a.id, ctx).await?,
    }
    ctx.finish()
}

async fn count(ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::count()).await? else {
        return Ok(());
    };
    let n = count_value(&body)
        .ok_or_else(|| ZdkError::Other("organizations/count returned no numeric count".into()))?;
    let refreshed = body
        .pointer("/count/refreshed_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    emit_count(ctx, n, refreshed.as_deref())
}

async fn create(a: CreateArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let mut flags = Map::new();
    put_str(&mut flags, "name", a.name);
    put_str(&mut flags, "external_id", a.external_id);
    if !a.domain.is_empty() {
        flags.insert("domain_names".into(), json!(a.domain));
    }
    if !a.tag.is_empty() {
        flags.insert("tags".into(), json!(a.tag));
    }
    if a.shared_tickets || a.shared_comments {
        flags.insert("shared_tickets".into(), Value::Bool(true));
    }
    if a.shared_comments {
        flags.insert("shared_comments".into(), Value::Bool(true));
    }
    if !a.custom_field.is_empty() {
        flags.insert("organization_fields".into(), named_fields(&a.custom_field)?);
    }
    let org = resource_body(
        "organization",
        a.file.as_deref(),
        a.from_stdin,
        Value::Object(flags),
        &a.field,
    )?;
    if org.get("name").is_none() {
        return Err(ZdkError::Usage(
            "an organization needs --name (or a document with one)".into(),
        ));
    }
    let Some(body) = send(&client, api::create(org)).await? else {
        return Ok(());
    };
    let created = unwrap_key(body, "organization");
    if ctx.output == OutputFormat::Table
        && let Some(id) = created.get("id").and_then(Value::as_u64)
    {
        output::warn(&format!("Created organization #{id}"));
    }
    ctx.emit(created, PRESET)
}

async fn update(a: UpdateArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let mut flags = Map::new();
    put_str(&mut flags, "name", a.name);
    put_str(&mut flags, "external_id", a.external_id);
    if !a.domain.is_empty() {
        flags.insert("domain_names".into(), json!(a.domain));
    }
    if !a.custom_field.is_empty() {
        flags.insert("organization_fields".into(), named_fields(&a.custom_field)?);
    }
    let org = resource_body("organization", None, false, Value::Object(flags), &a.field)?;
    let has_changes = org.as_object().is_some_and(|m| !m.is_empty());
    if !has_changes && a.add_tag.is_empty() && a.remove_tag.is_empty() {
        return Err(ZdkError::Usage(
            "nothing to update: pass --name, --domain, --add-tag, --field, … (see --help)".into(),
        ));
    }
    let mut result: Option<Value> = None;
    if has_changes {
        result = send(&client, api::update(a.id, org))
            .await?
            .map(|b| unwrap_key(b, "organization"));
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
        (Some(mut o), tags) => {
            if let (Some(tags), Some(map)) =
                (tags.as_ref().and_then(|t| t.get("tags")), o.as_object_mut())
            {
                map.insert("tags".into(), tags.clone());
            }
            ctx.emit(o, PRESET)
        }
        (None, Some(tags)) => {
            let tags = tags.get("tags").cloned().unwrap_or(Value::Null);
            ctx.emit_or_line(
                json!({ "id": a.id, "tags": tags }),
                &format!("Updated tags on organization #{}", a.id),
            )
        }
        (None, None) => Ok(()),
    }
}

async fn delete(id: u64, ctx: &AppContext) -> Result<()> {
    if !ctx.settings.dry_run && !ctx.confirm(&format!("delete organization #{id}"))? {
        return Err(ZdkError::Other("aborted".into()));
    }
    let client = ctx.client().await?;
    if send(&client, api::delete(id)).await?.is_none() {
        return Ok(());
    }
    ctx.emit_or_line(
        json!({ "id": id, "deleted": true }),
        &format!("Deleted organization #{id}"),
    )
}
