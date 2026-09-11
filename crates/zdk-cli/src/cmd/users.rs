//! `zdk users` — agents, admins and end users (PRD §8.4, plan A10).

use std::path::PathBuf;

use clap::{ArgAction, Args, Subcommand};
use serde_json::{Map, Value, json};
use zdk_core::api::PageDialect;
use zdk_core::api::curated::users::{self as api, ROLES};
use zdk_core::api::curated::{me, unwrap_key};
use zdk_core::output::{self, OutputFormat};
use zdk_core::{Result, ZdkError};

use crate::args::body::resource_body;
use crate::args::field::parse_named_custom_field;
use crate::args::ids::{UserRef, resolve_user};
use crate::args::list::{ListRun, fetch_single, send, stream_list};
use crate::context::AppContext;

const PRESET: Option<&str> = Some("users");

#[derive(Debug, Args)]
pub struct UsersArgs {
    #[command(subcommand)]
    pub command: UsersCommand,
}

#[derive(Debug, Subcommand)]
pub enum UsersCommand {
    /// List users (all, by role, or the members of an organization or group)
    List(ListArgs),
    /// Search users by name/email query, or by external id
    Search(SearchArgs),
    /// Autocomplete users by the start of a name
    Autocomplete(NameArg),
    /// Show one user: an id, an email, or me
    Get(RefArg),
    /// The authenticated user
    Me,
    /// Ticket counts and organization membership for a user
    Related(IdArg),
    /// Create a user from flags, --file or stdin
    Create(CreateArgs),
    /// Create, or update the user with the same email / external id
    CreateOrUpdate(CreateArgs),
    /// Update a user
    Update(UpdateArgs),
    /// Soft-delete a user; asks unless --yes
    Delete(IdArg),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Role (comma-separated or repeated): end-user, agent, admin
    #[arg(long, value_name = "ROLE", value_delimiter = ',', action = ArgAction::Append)]
    pub role: Vec<String>,

    /// Custom role (permission set) id
    #[arg(long, value_name = "ID")]
    pub permission_set: Option<u64>,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// Members of this organization
    #[arg(long, value_name = "ID", conflicts_with = "group_id")]
    pub organization_id: Option<u64>,

    /// Members of this group
    #[arg(long, value_name = "ID")]
    pub group_id: Option<u64>,

    /// Sort: updated_at, created_at, id (prefix - for descending)
    #[arg(long, value_name = "FIELD")]
    pub sort: Option<String>,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Name / email query (Zendesk user search syntax)
    #[arg(value_name = "QUERY", required_unless_present = "external_id")]
    pub query: Option<String>,

    /// Look up by external id instead
    #[arg(long, value_name = "ID", conflicts_with = "query")]
    pub external_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct NameArg {
    /// Name prefix (at least two characters)
    pub name: String,
}

#[derive(Debug, Args)]
pub struct RefArg {
    /// User id, email, or me
    #[arg(value_name = "ID|EMAIL|ME")]
    pub user: String,
}

#[derive(Debug, Args)]
pub struct IdArg {
    /// User id
    pub id: u64,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Display name
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Email (becomes the primary identity)
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    /// Role: end-user (default), agent, admin
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,

    /// Organization id
    #[arg(long, value_name = "ID")]
    pub organization_id: Option<u64>,

    /// Mark the email verified (no verification mail is sent)
    #[arg(long)]
    pub verified: bool,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// User field key=value or key:=json (repeatable; goes under user_fields)
    #[arg(long, value_name = "KEY=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// Any user attribute k=v or k:=json (dots nest; applied last)
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,

    /// JSON document: a user object or {"user": {...}} (- for stdin); flags merge on top
    #[arg(long, value_name = "PATH")]
    pub file: Option<PathBuf>,

    /// Read the JSON document from stdin
    #[arg(long)]
    pub from_stdin: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// User id
    pub id: u64,

    /// Display name
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Email
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    /// Role: end-user, agent, admin
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,

    /// Organization id
    #[arg(long, value_name = "ID")]
    pub organization_id: Option<u64>,

    /// Suspend the user
    #[arg(long, conflicts_with = "no_suspended")]
    pub suspended: bool,

    /// Lift a suspension
    #[arg(long)]
    pub no_suspended: bool,

    /// External id
    #[arg(long, value_name = "ID")]
    pub external_id: Option<String>,

    /// User field key=value or key:=json (repeatable; goes under user_fields)
    #[arg(long, value_name = "KEY=VALUE", action = ArgAction::Append)]
    pub custom_field: Vec<String>,

    /// Any user attribute k=v or k:=json (dots nest; applied last)
    #[arg(long, value_name = "K=V|K:=JSON", action = ArgAction::Append)]
    pub field: Vec<String>,
}

pub async fn run(args: UsersArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        UsersCommand::List(a) => list(a, ctx).await?,
        UsersCommand::Search(a) => search(a, ctx).await?,
        UsersCommand::Autocomplete(a) => {
            stream_list(ctx, ListRun::new(api::autocomplete(&a.name), PRESET)).await?;
        }
        UsersCommand::Get(a) => get(a, ctx).await?,
        UsersCommand::Me => fetch_single(ctx, me::show(), "user", PRESET).await?,
        UsersCommand::Related(a) => {
            fetch_single(ctx, api::related(a.id), "user_related", None).await?;
        }
        UsersCommand::Create(a) => create(a, ctx, false).await?,
        UsersCommand::CreateOrUpdate(a) => create(a, ctx, true).await?,
        UsersCommand::Update(a) => update(a, ctx).await?,
        UsersCommand::Delete(a) => delete(a.id, ctx).await?,
    }
    ctx.finish()
}

async fn list(a: ListArgs, ctx: &AppContext) -> Result<()> {
    let roles = a
        .role
        .iter()
        .map(|r| valid_role(r))
        .collect::<Result<Vec<_>>>()?;
    let spec = api::list(&api::ListOptions {
        roles,
        permission_set: a.permission_set,
        external_id: a.external_id,
        organization_id: a.organization_id,
        group_id: a.group_id,
        sideload: ctx.settings.sideload.clone(),
        sort: a.sort,
    });
    let mut run = ListRun::new(spec, PRESET);
    if a.group_id.is_some() {
        // The spec lists no page parameters for ListGroupUsers, but the endpoint does paginate.
        run = run.dialect(PageDialect::Cursor);
    }
    stream_list(ctx, run).await
}

async fn search(a: SearchArgs, ctx: &AppContext) -> Result<()> {
    let spec = match (a.query, a.external_id) {
        (_, Some(ext)) => api::search_external_id(&ext),
        (Some(q), None) => api::search(&q),
        (None, None) => {
            return Err(ZdkError::Usage("pass a query or --external-id".into()));
        }
    };
    stream_list(ctx, ListRun::new(spec, PRESET)).await
}

async fn get(a: RefArg, ctx: &AppContext) -> Result<()> {
    match UserRef::parse(&a.user)? {
        UserRef::Me => fetch_single(ctx, me::show(), "user", PRESET).await,
        UserRef::Id(id) => {
            fetch_single(ctx, api::show(id, &ctx.settings.sideload), "user", PRESET).await
        }
        r @ UserRef::Email(_) => {
            let client = ctx.client().await?;
            let Some(id) = resolve_user(&client, &r).await?.as_u64() else {
                // Dry run: the lookup was skipped, so there is no id to show.
                return Ok(());
            };
            fetch_single(ctx, api::show(id, &ctx.settings.sideload), "user", PRESET).await
        }
    }
}

async fn create(a: CreateArgs, ctx: &AppContext, upsert: bool) -> Result<()> {
    let client = ctx.client().await?;
    let mut flags = Map::new();
    put_str(&mut flags, "name", a.name);
    put_str(&mut flags, "email", a.email);
    if let Some(role) = a.role {
        flags.insert("role".into(), Value::String(valid_role(&role)?));
    }
    put_u64(&mut flags, "organization_id", a.organization_id);
    put_str(&mut flags, "external_id", a.external_id);
    if a.verified {
        flags.insert("verified".into(), Value::Bool(true));
    }
    if !a.custom_field.is_empty() {
        flags.insert("user_fields".into(), named_fields(&a.custom_field)?);
    }
    let user = resource_body(
        "user",
        a.file.as_deref(),
        a.from_stdin,
        Value::Object(flags),
        &a.field,
    )?;
    if user.get("name").is_none() && user.get("email").is_none() {
        return Err(ZdkError::Usage(
            "a user needs at least --name (Zendesk requires a name; --email is strongly recommended)".into(),
        ));
    }
    let spec = if upsert {
        api::create_or_update(user)
    } else {
        api::create(user)
    };
    let Some(body) = send(&client, spec).await? else {
        return Ok(());
    };
    let created = unwrap_key(body, "user");
    if ctx.output == OutputFormat::Table
        && let Some(id) = created.get("id").and_then(Value::as_u64)
    {
        output::warn(&format!(
            "{} user #{id}",
            if upsert {
                "Created or updated"
            } else {
                "Created"
            }
        ));
    }
    ctx.emit(created, PRESET)
}

async fn update(a: UpdateArgs, ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let mut flags = Map::new();
    put_str(&mut flags, "name", a.name);
    put_str(&mut flags, "email", a.email);
    if let Some(role) = a.role {
        flags.insert("role".into(), Value::String(valid_role(&role)?));
    }
    put_u64(&mut flags, "organization_id", a.organization_id);
    put_str(&mut flags, "external_id", a.external_id);
    if a.suspended {
        flags.insert("suspended".into(), Value::Bool(true));
    } else if a.no_suspended {
        flags.insert("suspended".into(), Value::Bool(false));
    }
    if !a.custom_field.is_empty() {
        flags.insert("user_fields".into(), named_fields(&a.custom_field)?);
    }
    let user = resource_body("user", None, false, Value::Object(flags), &a.field)?;
    if user.as_object().is_none_or(Map::is_empty) {
        return Err(ZdkError::Usage(
            "nothing to update: pass --name, --email, --role, --suspended, --field, … (see --help)"
                .into(),
        ));
    }
    let Some(body) = send(&client, api::update(a.id, user)).await? else {
        return Ok(());
    };
    ctx.emit(unwrap_key(body, "user"), PRESET)
}

async fn delete(id: u64, ctx: &AppContext) -> Result<()> {
    if !ctx.settings.dry_run && !ctx.confirm(&format!("delete user #{id}"))? {
        return Err(ZdkError::Other("aborted".into()));
    }
    let client = ctx.client().await?;
    let Some(body) = send(&client, api::delete(id)).await? else {
        return Ok(());
    };
    let value = if body.is_null() {
        json!({ "id": id, "deleted": true })
    } else {
        unwrap_key(body, "user")
    };
    ctx.emit_or_line(value, &format!("Deleted user #{id}"))
}

fn valid_role(role: &str) -> Result<String> {
    let r = role.trim().to_ascii_lowercase().replace('_', "-");
    if ROLES.contains(&r.as_str()) {
        Ok(r)
    } else {
        Err(ZdkError::Usage(format!(
            "role '{role}' is not one of {}",
            ROLES.join(", ")
        )))
    }
}

/// `--custom-field k=v` pairs → the `user_fields` / `organization_fields` object.
pub(crate) fn named_fields(raw: &[String]) -> Result<Value> {
    let mut map = Map::new();
    for f in raw {
        let (k, v) = parse_named_custom_field(f)?;
        map.insert(k, v);
    }
    Ok(Value::Object(map))
}

pub(crate) fn put_str(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(v) = value {
        map.insert(key.to_string(), Value::String(v));
    }
}

pub(crate) fn put_u64(map: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if let Some(v) = value {
        map.insert(key.to_string(), Value::from(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_named_fields() {
        assert_eq!(valid_role("End_User").unwrap(), "end-user");
        assert_eq!(valid_role("agent").unwrap(), "agent");
        assert_eq!(valid_role("owner").unwrap_err().exit_code(), 2);
        assert_eq!(
            named_fields(&["tier=gold".into(), "seats:=3".into()]).unwrap(),
            json!({"tier": "gold", "seats": 3})
        );
        assert_eq!(named_fields(&["bad".into()]).unwrap_err().exit_code(), 2);
    }
}
