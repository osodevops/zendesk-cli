//! `zdk auth …` — login (authorization code + PKCE, client credentials, legacy API token),
//! status, whoami, refresh, logout, test, token and the scope catalogue (PRD §6, plan A10).
//!
//! Prompts and the authorize URL go to stderr; stdout carries data only. `auth token` is the
//! one command that deliberately prints a secret.

use std::path::Path;
use std::sync::Arc;

use clap::{ArgAction, Args, Subcommand, ValueEnum};
use secrecy::SecretString;
use serde_json::{Value, json};
use url::Url;
use zdk_core::auth::token::Credential;
use zdk_core::auth::{
    self, ApiTokenProvider, AuthProvider, AuthStatus, GrantKind, LoginOptions, LogoutOptions,
    OAuthProvider, scopes,
};
use zdk_core::config::{ProfileConfig, profiles};
use zdk_core::error::AuthFailure;
use zdk_core::http::RequestSpec;
use zdk_core::output::{self, OutputFormat, write_stdout_line};
use zdk_core::store::{self, MemoryStore, SharedStore};
use zdk_core::util::fs::atomic_write_0600;
use zdk_core::{Result, ZdkError};

use crate::context::{AppContext, read_line_from_stdin};

const LOGIN_AFTER_HELP: &str = "\
The subdomain comes from the global --subdomain flag, ZENDESK_SUBDOMAIN or the profile.

Examples:
  zdk auth login                                  # browser + PKCE with the profile's client
  zdk auth login --subdomain acme --client-id zdk_local --preset agent
  zdk auth login --no-browser                     # SSH / containers: paste the code back
  zdk auth login --client-credentials --client-id zdk_ci --client-secret \"$SECRET\" \\
      --scopes tickets:read,users:read           # CI: confidential client, no refresh token
  zdk auth login --api-token --email me@acme.com --token \"$ZENDESK_API_TOKEN\"   # deprecated";

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Log in: browser + PKCE (default), --client-credentials (CI) or --api-token (legacy)
    Login(LoginArgs),

    /// Show the active credential: grant, scopes, expiry countdown, store backend
    Status,

    /// Show the signed-in user (GET /api/v2/users/me)
    Whoami,

    /// Refresh (authorization code) or re-mint (client credentials) the access token now
    Refresh {
        /// Refresh even when the token is not yet due
        #[arg(long)]
        force: bool,
    },

    /// Revoke the token server-side (best effort) and remove the stored credential
    Logout {
        /// Every profile in the credential store, not just the active one
        #[arg(long)]
        all: bool,
        /// Remove the local credential without revoking it at Zendesk
        #[arg(long)]
        no_revoke: bool,
    },

    /// One cheap call per product API the credential can reach
    Test,

    /// Print the current access token — the only command that prints a secret
    Token {
        /// raw (just the token), bearer (the Authorization header value) or json
        #[arg(long, value_enum, default_value = "raw", value_name = "FORMAT")]
        format: TokenFormat,
    },

    /// Scope catalogue, presets and pre-flight checks
    Scopes {
        #[command(subcommand)]
        command: ScopesCommand,
    },
}

#[derive(Debug, Default, Args)]
#[command(after_help = LOGIN_AFTER_HELP)]
pub struct LoginArgs {
    /// Client-credentials grant (confidential client for CI/automation; no refresh token)
    #[arg(long, conflicts_with_all = ["api_token", "no_browser", "port", "redirect_uri"])]
    pub client_credentials: bool,

    /// Legacy API token (deprecated: no new tokens after 27 Oct 2026, all stop on 30 Apr 2027)
    #[arg(long, conflicts_with_all = ["client_credentials", "no_browser", "port", "redirect_uri", "scopes", "preset", "expires_in", "store_secret"])]
    pub api_token: bool,

    /// Print the authorize URL and read the code (or the redirect URL) from stdin
    #[arg(long)]
    pub no_browser: bool,

    /// Loopback port for the browser redirect (default: ephemeral; 8484 with --no-browser)
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Registered redirect URI for a paste-from-address-bar flow, e.g. https://localhost
    #[arg(long, value_name = "URI")]
    pub redirect_uri: Option<String>,

    /// OAuth client identifier (default: ZENDESK_CLIENT_ID or the profile's client_id)
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,

    /// OAuth client secret (default: ZENDESK_CLIENT_SECRET); required for --client-credentials
    #[arg(long, value_name = "SECRET")]
    pub client_secret: Option<String>,

    /// Agent email for --api-token (default: ZENDESK_EMAIL or the profile's email)
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,

    /// The API token for --api-token (default: ZENDESK_API_TOKEN)
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,

    /// Scopes to request, comma-separated (default: the profile's scopes, else --preset agent)
    #[arg(long, value_name = "A,B", value_delimiter = ',', action = ArgAction::Append)]
    pub scopes: Vec<String>,

    /// Scope preset: agent, admin, readonly, exporter (repeatable; combined with --scopes)
    #[arg(long, value_name = "NAME", action = ArgAction::Append)]
    pub preset: Vec<String>,

    /// Requested access-token lifetime in seconds (300–172800)
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(300..=172_800))]
    pub expires_in: Option<u64>,

    /// Keep the client secret in the credential store so client-credentials tokens re-mint
    /// without ZENDESK_CLIENT_SECRET
    #[arg(long)]
    pub store_secret: bool,
}

/// `auth token --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TokenFormat {
    /// The bare token
    Raw,
    /// The full Authorization header value (`Bearer …`, or `Basic …` for API tokens)
    Bearer,
    /// A JSON object with the token and its metadata
    Json,
}

#[derive(Debug, Subcommand)]
pub enum ScopesCommand {
    /// The full scope catalogue (SCOPE FAMILY ACCESS DESCRIPTION)
    List,

    /// Granted scopes versus what a preset needs
    Show {
        /// Preset to compare against: agent, admin, readonly, exporter
        #[arg(long, value_name = "NAME", default_value = "agent")]
        preset: String,
    },

    /// Check a command against the granted scopes, e.g. `zdk auth scopes check tickets update`
    Check {
        /// Command words, e.g. `tickets update`
        #[arg(value_name = "COMMAND", required = true, num_args = 1..)]
        command: Vec<String>,
    },

    /// Print a preset's scopes (--login re-runs `auth login` with them)
    Preset {
        /// agent, admin, readonly or exporter
        #[arg(value_name = "NAME")]
        name: String,
        /// Log in again requesting exactly this preset
        #[arg(long)]
        login: bool,
        /// With --login: use the client-credentials grant
        #[arg(long, requires = "login")]
        client_credentials: bool,
        /// With --login: print the URL and read the code from stdin
        #[arg(long, requires = "login")]
        no_browser: bool,
    },
}

pub async fn run(args: AuthArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        AuthCommand::Login(login) => run_login(&login, ctx).await?,
        AuthCommand::Status => run_status(ctx)?,
        AuthCommand::Whoami => run_whoami(ctx).await?,
        AuthCommand::Refresh { force } => run_refresh(ctx, force).await?,
        AuthCommand::Logout { all, no_revoke } => run_logout(ctx, all, no_revoke).await?,
        AuthCommand::Test => run_test(ctx).await?,
        AuthCommand::Token { format } => run_token(ctx, format).await?,
        AuthCommand::Scopes { command } => match command {
            ScopesCommand::List => run_scopes_list(ctx)?,
            ScopesCommand::Show { preset } => run_scopes_show(ctx, &preset).await?,
            ScopesCommand::Check { command } => run_scopes_check(ctx, &command).await?,
            ScopesCommand::Preset {
                name,
                login,
                client_credentials,
                no_browser,
            } => run_scopes_preset(ctx, &name, login, client_credentials, no_browser).await?,
        },
    }
    ctx.finish()
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn is_human(ctx: &AppContext) -> bool {
    ctx.output == OutputFormat::Table && ctx.settings.output.jq.is_none()
}

/// The store the commands read from. A token in the environment bypasses the store, so the
/// keyring is never probed for it (mirrors `AppContext::provider`).
fn read_store(ctx: &AppContext) -> Result<SharedStore> {
    if ctx.env.access_token.is_some() || ctx.env.api_token.is_some() {
        return Ok(Arc::new(MemoryStore::new()));
    }
    store::open(ctx.settings.credential_store, &ctx.settings.paths, &ctx.env)
}

/// The store `login`/`logout`/`refresh` write to. `login` forces a fresh keyring probe so a
/// stale "keyring unavailable" decision does not stick forever.
fn write_store(ctx: &AppContext, force_probe: bool) -> Result<SharedStore> {
    store::open_with(
        ctx.settings.credential_store,
        &ctx.settings.paths,
        &ctx.env,
        force_probe,
    )
}

/// `acme.zendesk.com`, or the host of a custom base URL.
fn instance(ctx: &AppContext) -> String {
    match &ctx.settings.subdomain {
        Some(s)
            if ctx
                .settings
                .base_url
                .as_ref()
                .is_none_or(|u| u.host_str().is_some_and(|h| h.ends_with(".zendesk.com"))) =>
        {
            format!("{s}.zendesk.com")
        }
        _ => ctx.settings.instance_label(),
    }
}

/// `1d 2h`, `29m 59s`, `45s`.
fn human_secs(secs: i64) -> String {
    let secs = secs.unsigned_abs();
    let (d, h, m, s) = (
        secs / 86_400,
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60,
    );
    let parts: Vec<String> = [(d, "d"), (h, "h"), (m, "m"), (s, "s")]
        .into_iter()
        .filter(|(n, unit)| *n > 0 || (*unit == "s" && d == 0 && h == 0 && m == 0))
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect();
    parts.into_iter().take(2).collect::<Vec<_>>().join(" ")
}

/// `expires in 29m 10s (2026-09-11T11:00:00Z)`, `expired 3m ago`, `no expiry`.
fn expiry_phrase(
    expires_in_secs: Option<i64>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    match (expires_in_secs, expires_at) {
        (Some(s), _) if s <= 0 => format!("expired {} ago", human_secs(s)),
        (Some(s), Some(at)) => format!(
            "expires in {} ({})",
            human_secs(s),
            at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        (Some(s), None) => format!("expires in {}", human_secs(s)),
        (None, _) => "no expiry".into(),
    }
}

fn row(field: &str, value: impl Into<Value>) -> Value {
    json!({ "field": field, "value": value.into() })
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}

/// `{"user": {...}}` → the user; anything else is returned as-is.
fn unwrap_user(body: Value) -> Value {
    match body {
        Value::Object(mut map) if map.contains_key("user") => {
            map.remove("user").unwrap_or(Value::Null)
        }
        other => other,
    }
}

fn user_summary(user: &Value) -> Value {
    json!({
        "id": user.get("id").cloned().unwrap_or(Value::Null),
        "name": user.get("name").cloned().unwrap_or(Value::Null),
        "email": user.get("email").cloned().unwrap_or(Value::Null),
        "role": user.get("role").cloned().unwrap_or(Value::Null),
    })
}

fn user_line(user: &Value) -> String {
    let name = user["name"].as_str().unwrap_or("?");
    let role = user["role"].as_str().unwrap_or("?");
    match user["email"].as_str() {
        Some(email) => format!("{name} <{email}> ({role})"),
        None => format!("{name} ({role})"),
    }
}

/// `GET /api/v2/users/me` without the local scope pre-flight (the server is the authority on
/// whether a freshly minted token can see it).
async fn fetch_me(ctx: &AppContext, provider: Arc<dyn AuthProvider>) -> Result<Value> {
    let client = ctx.client_with_provider(provider)?;
    let body = client
        .execute(RequestSpec::get("/api/v2/users/me").no_preflight(true))
        .await?
        .value()?;
    Ok(unwrap_user(body))
}

// ---------------------------------------------------------------------------------------------
// login
// ---------------------------------------------------------------------------------------------

/// Never an empty request: explicit `--scopes`/`--preset`, else the profile's scopes, else the
/// `agent` preset.
fn login_scopes(args: &LoginArgs, ctx: &AppContext) -> Result<Vec<String>> {
    let explicit: Vec<String> = args
        .scopes
        .iter()
        .flat_map(|s| scopes::parse_list(s))
        .collect();
    let mut chosen = scopes::expand(&args.preset, &explicit)?;
    if chosen.is_empty() {
        chosen.clone_from(&ctx.settings.profile.scopes);
    }
    if chosen.is_empty() {
        chosen = scopes::preset("agent")
            .map(|p| p.scopes.iter().map(|s| (*s).to_string()).collect())
            .unwrap_or_default();
        output::warn(
            "no scopes configured: requesting the `agent` preset (tickets read/write; users, organizations and Help Center read). Pass --scopes or --preset to change this.",
        );
    }
    scopes::validate_requested(&chosen)?;
    Ok(chosen)
}

/// Print the authorize URL and instructions on stderr (never suppressed: the user needs it).
fn announce_url(url: &Url, manual: bool) {
    let text = if manual {
        format!(
            "Open this URL in a browser and sign in to Zendesk:\n\n  {url}\n\nAfter approving, paste the authorization code (or the full redirect URL) below.\nAuthorization codes expire after 120 seconds."
        )
    } else {
        format!(
            "Opening your browser to sign in to Zendesk… if it does not open, visit:\n\n  {url}\n\nWaiting for the redirect (up to 5 minutes; ctrl-c to abort)."
        )
    };
    output::error_line(&text);
}

async fn run_login(args: &LoginArgs, ctx: &AppContext) -> Result<()> {
    let store = write_store(ctx, true)?;
    if args.api_token {
        return login_api_token(args, ctx, &store).await;
    }

    let scopes = login_scopes(args, ctx)?;
    let client_secret = args
        .client_secret
        .clone()
        .map(SecretString::from)
        .or_else(|| ctx.env.client_secret.clone());
    let manual = args.no_browser || args.redirect_uri.is_some();
    let opts = LoginOptions {
        subdomain: ctx.args.subdomain.clone(),
        client_id: args.client_id.clone(),
        client_secret: client_secret.clone(),
        scopes: scopes.clone(),
        port: args.port,
        redirect_uri: args.redirect_uri.clone(),
        no_browser: args.no_browser,
        expires_in: args.expires_in,
        store_secret: args.store_secret,
        timeout: None,
        open_browser: None,
        stdin_reader: Some(Box::new(|| {
            read_line_from_stdin("Authorization code or redirect URL: ")
        })),
        on_authorize_url: Some(Box::new(move |url: &Url| announce_url(url, manual))),
    };

    let token = if args.client_credentials {
        auth::login_client_credentials(&ctx.settings, &ctx.env, &store, opts).await?
    } else {
        auth::login_authorization_code(&ctx.settings, &ctx.env, &store, opts).await?
    };

    // Verify with a client built from the token just minted (an env token must not take over).
    let base = ctx.settings.require_base_url()?.clone();
    let provider: Arc<dyn AuthProvider> = Arc::new(OAuthProvider::new(
        ctx.settings.profile_name.clone(),
        store.clone(),
        base,
        auth::http_client(ctx.settings.timeout)?,
        ctx.settings.auth.clone(),
        client_secret,
        Some(token.clone()),
    ));
    let user = verify_login(ctx, provider).await?;

    persist_profile(ctx, args, token.grant, &token.scopes);
    report_login(
        ctx,
        token.grant,
        &token.scopes,
        token.expires_at,
        &user,
        store.kind().as_str(),
    )
}

async fn login_api_token(args: &LoginArgs, ctx: &AppContext, store: &SharedStore) -> Result<()> {
    let email = args
        .email
        .clone()
        .or_else(|| ctx.settings.profile.email.clone())
        .ok_or_else(|| {
            ZdkError::Usage(
                "--email is required with --api-token (or set ZENDESK_EMAIL / the profile's `email`)"
                    .into(),
            )
        })?;
    let token = args
        .token
        .clone()
        .map(SecretString::from)
        .or_else(|| ctx.env.api_token.clone())
        .ok_or_else(|| {
            ZdkError::Usage(
                "--token is required with --api-token (or set ZENDESK_API_TOKEN)".into(),
            )
        })?;
    auth::login_api_token(
        &ctx.settings,
        store,
        &email,
        token.clone(),
        ctx.args.subdomain.as_deref(),
    )?;

    let provider: Arc<dyn AuthProvider> = Arc::new(ApiTokenProvider::new(
        ctx.settings.profile_name.clone(),
        email.clone(),
        token,
        ctx.settings.subdomain.clone().unwrap_or_default(),
        ctx.settings.auth.suppress_deprecation,
        Some(store.kind().to_string()),
    ));
    let user = verify_login(ctx, provider).await?;

    persist_profile(ctx, args, GrantKind::ApiToken, &[]);
    report_login(
        ctx,
        GrantKind::ApiToken,
        &[],
        None,
        &user,
        store.kind().as_str(),
    )
}

/// `GET /api/v2/users/me` with the new credential. A 403 is not fatal — the token is stored
/// and works for what it was scoped to — but an auth failure (401) is.
async fn verify_login(ctx: &AppContext, provider: Arc<dyn AuthProvider>) -> Result<Value> {
    match fetch_me(ctx, provider).await {
        Ok(user) => Ok(user),
        Err(ZdkError::DryRun) => Ok(Value::Null),
        Err(ZdkError::Forbidden { message, .. }) => {
            output::warn(&format!(
                "warning: credential stored, but GET /api/v2/users/me was refused ({message}); the token has no scope to read users"
            ));
            Ok(Value::Null)
        }
        Err(e) => Err(e),
    }
}

/// First login for a profile that is not in the config file yet: persist what was passed on
/// the command line so the next run needs no flags. Best effort — the credential is already saved.
fn persist_profile(ctx: &AppContext, args: &LoginArgs, grant: GrantKind, scopes: &[String]) {
    let explicit = ctx.args.subdomain.is_some() || args.client_id.is_some() || args.email.is_some();
    if !explicit {
        return;
    }
    let name = ctx.settings.profile_name.clone();
    let path = ctx.settings.paths.config_file.clone();
    let profile = ProfileConfig {
        subdomain: ctx.settings.subdomain.clone(),
        client_id: args
            .client_id
            .clone()
            .or_else(|| ctx.settings.profile.client_id.clone()),
        grant_type: Some(grant),
        scopes: scopes.to_vec(),
        email: args
            .email
            .clone()
            .or_else(|| ctx.settings.profile.email.clone()),
        ..ProfileConfig::default()
    };
    match write_profile(&path, &name, &profile) {
        Ok(true) => output::warn(&format!(
            "Saved profile '{name}' ({}) to {} — the next `zdk auth login` needs no flags",
            profile.subdomain.as_deref().unwrap_or("?"),
            path.display()
        )),
        Ok(false) => {}
        Err(e) => output::warn(&format!(
            "warning: could not save profile '{name}' to {}: {e}",
            path.display()
        )),
    }
}

/// Returns `Ok(false)` when the profile already exists (nothing written).
fn write_profile(path: &Path, name: &str, profile: &ProfileConfig) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(ZdkError::Config(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    let mut doc = profiles::parse_document(&text)?;
    if profiles::profile_names(&doc).iter().any(|n| n == name) {
        return Ok(false);
    }
    profiles::add_profile(&mut doc, name, profile)?;
    if profiles::get_value(&doc, "default.active_profile").is_none() {
        profiles::switch_profile(&mut doc, name)?;
    }
    atomic_write_0600(path, doc.to_string().as_bytes())
        .map_err(|e| ZdkError::Config(format!("cannot write {}: {e}", path.display())))?;
    Ok(true)
}

fn report_login(
    ctx: &AppContext,
    grant: GrantKind,
    scopes: &[String],
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    user: &Value,
    store_kind: &str,
) -> Result<()> {
    let profile = &ctx.settings.profile_name;
    let instance = instance(ctx);
    let who = if user.is_null() {
        "(identity not verified)".to_string()
    } else {
        user_line(user)
    };
    output::warn(&format!(
        "Logged in to {instance} as {who} — profile '{profile}', {} ({store_kind} store)",
        grant.as_str()
    ));

    let value = json!({
        "profile": profile,
        "subdomain": ctx.settings.subdomain,
        "instance": instance,
        "grant": grant.as_str(),
        "scopes": scopes,
        "expires_at": expires_at,
        "store": store_kind,
        "user": if user.is_null() { Value::Null } else { user_summary(user) },
    });
    let expiry = expires_at.map_or_else(
        || "no expiry".to_string(),
        |at| expiry_phrase(Some((at - chrono::Utc::now()).num_seconds()), Some(at)),
    );
    let scope_text = if scopes.is_empty() {
        "n/a".to_string()
    } else {
        scopes.join(", ")
    };
    let line = format!(
        "profile {profile}: {instance} · {} · scopes: {scope_text} · {expiry}",
        grant.as_str()
    );
    ctx.emit_or_line(value, &line)
}

// ---------------------------------------------------------------------------------------------
// status / whoami / refresh / logout
// ---------------------------------------------------------------------------------------------

fn status_rows(s: &AuthStatus, instance: &str) -> Vec<Value> {
    let d = &s.description;
    let mut rows = vec![
        row("Profile", d.profile.as_str()),
        row("Instance", instance),
        row("Grant", d.grant.as_str()),
        row("Store", d.store.as_deref().unwrap_or("-")),
    ];
    if let Some(c) = &d.client_id {
        rows.push(row("Client ID", c.as_str()));
    }
    if let Some(e) = &s.email {
        rows.push(row("Email", e.as_str()));
    }
    let scope_text = if d.scopes.is_empty() {
        "(unknown — not an OAuth token)".to_string()
    } else {
        d.scopes.join(", ")
    };
    rows.push(row("Scopes", scope_text));
    match d.grant {
        GrantKind::AuthorizationCode | GrantKind::ClientCredentials => {
            rows.push(row(
                "Access token",
                expiry_phrase(s.expires_in_secs, d.expires_at),
            ));
            if let Some(t) = &s.obtained_at {
                rows.push(row(
                    "Obtained",
                    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ));
            }
            rows.push(row("Refresh token", yes_no(d.has_refresh_token)));
            if let Some(r) = s.refresh_expires_at {
                rows.push(row(
                    "Refresh token expires",
                    r.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ));
            }
            rows.push(row("Refresh due", yes_no(s.refresh_due)));
            rows.push(row("Can renew", yes_no(s.can_renew)));
        }
        GrantKind::ApiToken => {
            if let Some(days) = d.api_token_days_remaining {
                rows.push(row(
                    "API token deadline",
                    format!("{days} days until 30 Apr 2027 (all API tokens stop working)"),
                ));
            }
            if let Some(days) = s.api_token_days_until_creation_cutoff {
                rows.push(row(
                    "Token creation cut-off",
                    format!("{days} days until 27 Oct 2026"),
                ));
            }
        }
        GrantKind::StaticToken => {
            rows.push(row(
                "Access token",
                "from ZENDESK_ACCESS_TOKEN (never refreshed)",
            ));
        }
    }
    rows
}

fn run_status(ctx: &AppContext) -> Result<()> {
    let store = read_store(ctx)?;
    let s = auth::status(&ctx.settings, &ctx.env, &store)?;
    let instance = instance(ctx);
    if is_human(ctx) {
        return ctx.emit(Value::Array(status_rows(&s, &instance)), None);
    }
    let mut value = serde_json::to_value(&s)?;
    if let Value::Object(map) = &mut value {
        map.insert("instance".into(), Value::String(instance));
        map.insert(
            "expires_in".into(),
            s.expires_in_secs
                .map_or(Value::Null, |secs| Value::String(human_secs(secs))),
        );
    }
    ctx.emit(value, None)
}

async fn run_whoami(ctx: &AppContext) -> Result<()> {
    let client = ctx.client().await?;
    let body = match client.get_value("/api/v2/users/me", &[]).await {
        Err(ZdkError::DryRun) => return Ok(()),
        other => other?,
    };
    let user = unwrap_user(body);
    if is_human(ctx) {
        ctx.emit(Value::Array(vec![user]), Some("users"))
    } else {
        ctx.emit(user, Some("users"))
    }
}

async fn run_refresh(ctx: &AppContext, force: bool) -> Result<()> {
    if ctx.env.access_token.is_some() {
        return Err(ZdkError::Usage(
            "ZENDESK_ACCESS_TOKEN is set: a static token cannot be refreshed".into(),
        ));
    }
    let store = write_store(ctx, false)?;
    let before = auth::status(&ctx.settings, &ctx.env, &store)?;
    let refreshed = force || before.refresh_due;
    let fresh = auth::refresh_now(&ctx.settings, &ctx.env, &store, force).await?;
    let expires_in = fresh
        .expires_at
        .map(|at| (at - chrono::Utc::now()).num_seconds());
    let expiry = expiry_phrase(expires_in, fresh.expires_at);
    let profile = &ctx.settings.profile_name;
    let value = json!({
        "profile": profile,
        "grant": fresh.grant.as_str(),
        "refreshed": refreshed,
        "expires_at": fresh.expires_at,
        "expires_in": expires_in.map(human_secs),
        "has_refresh_token": fresh.refresh_token.is_some(),
        "scopes": fresh.scopes,
    });
    let line = if refreshed {
        format!(
            "Renewed the {} token for profile '{profile}' — {expiry}",
            fresh.grant.as_str()
        )
    } else {
        format!(
            "Token for profile '{profile}' is not due for renewal ({expiry}); pass --force to renew anyway"
        )
    };
    ctx.emit_or_line(value, &line)
}

async fn run_logout(ctx: &AppContext, all: bool, no_revoke: bool) -> Result<()> {
    let store = write_store(ctx, false)?;
    let report = auth::logout(
        &ctx.settings,
        &store,
        LogoutOptions {
            revoke: !no_revoke,
            all,
        },
    )
    .await?;
    for (profile, why) in &report.revoke_errors {
        output::warn(&format!(
            "warning: could not revoke the token for profile '{profile}' at Zendesk ({why}); the local credential was removed"
        ));
    }
    if ctx.env.access_token.is_some() || ctx.env.api_token.is_some() {
        output::warn(
            "note: a token is set in the environment (ZENDESK_ACCESS_TOKEN / ZENDESK_API_TOKEN); unset it to stop using it",
        );
    }

    let value = json!({
        "removed": report.removed,
        "revoked": report.revoked,
        "not_found": report.not_found,
        "revoke_errors": report.revoke_errors.iter().map(|(p, e)| json!({"profile": p, "error": e})).collect::<Vec<_>>(),
        "store": store.kind().as_str(),
    });
    let line = if report.removed.is_empty() {
        format!(
            "Nothing to log out: no credential stored for profile '{}' ({} store)",
            ctx.settings.profile_name,
            store.kind()
        )
    } else {
        let revoked = if report.revoked.is_empty() {
            String::new()
        } else {
            format!(" (revoked at Zendesk: {})", report.revoked.join(", "))
        };
        format!(
            "Logged out: removed credential(s) for {}{revoked}",
            report.removed.join(", ")
        )
    };
    ctx.emit_or_line(value, &line)
}

// ---------------------------------------------------------------------------------------------
// test / token
// ---------------------------------------------------------------------------------------------

const ME: &str = "/api/v2/users/me";
const LOCALES: &str = "/api/v2/help_center/locales";

fn test_row(api: &str, endpoint: &str, status: &str, detail: &str, ms: Option<u128>) -> Value {
    json!({ "api": api, "endpoint": endpoint, "status": status, "detail": detail, "ms": ms })
}

async fn run_test(ctx: &AppContext) -> Result<()> {
    let provider = ctx.provider().await?;
    let client = ctx.client().await?;
    let mut rows = Vec::new();
    let mut auth_error: Option<ZdkError> = None;
    let mut failures = 0;

    match client
        .execute(RequestSpec::get(ME).no_preflight(true))
        .await
    {
        Ok(resp) => {
            let user = unwrap_user(resp.value()?);
            rows.push(test_row(
                "support",
                ME,
                "ok",
                &user_line(&user),
                Some(resp.elapsed.as_millis()),
            ));
        }
        Err(ZdkError::DryRun) => return Ok(()),
        Err(e) => {
            failures += 1;
            rows.push(test_row("support", ME, "fail", &e.to_string(), None));
            if matches!(e, ZdkError::Auth(_)) {
                auth_error = Some(e);
            }
        }
    }

    let granted = provider.granted_scopes();
    let can_read_hc = granted
        .as_ref()
        .is_none_or(|g| scopes::covers(g, "hc:read"));
    if auth_error.is_some() {
        rows.push(test_row(
            "help_center",
            LOCALES,
            "skip",
            "skipped after the authentication failure",
            None,
        ));
    } else if can_read_hc {
        match client
            .execute(RequestSpec::get(LOCALES).no_preflight(true))
            .await
        {
            Ok(resp) => {
                let body = resp.value()?;
                let locales = body
                    .get("locales")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                let default = body
                    .get("default_locale")
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                rows.push(test_row(
                    "help_center",
                    LOCALES,
                    "ok",
                    &format!("{locales} locale(s), default {default}"),
                    Some(resp.elapsed.as_millis()),
                ));
            }
            Err(e) => {
                failures += 1;
                rows.push(test_row(
                    "help_center",
                    LOCALES,
                    "fail",
                    &e.to_string(),
                    None,
                ));
            }
        }
    } else {
        rows.push(test_row(
            "help_center",
            LOCALES,
            "skip",
            &format!(
                "the token does not grant hc:read (granted: {})",
                granted.unwrap_or_default().join(", ")
            ),
            None,
        ));
    }

    let total = rows.len();
    ctx.emit(Value::Array(rows), None)?;
    if let Some(e) = auth_error {
        return Err(e);
    }
    if failures > 0 {
        return Err(ZdkError::Other(format!(
            "{failures} of {total} API check(s) failed"
        )));
    }
    Ok(())
}

async fn run_token(ctx: &AppContext, format: TokenFormat) -> Result<()> {
    let provider = ctx.provider().await?;
    let description = provider.description();
    // Refreshes / re-mints first when due, exactly like a real request would.
    let header = provider.authorization().await?;
    let header_text = header
        .to_str()
        .map_err(|e| ZdkError::Other(format!("authorization header is not text: {e}")))?
        .to_string();
    let after_scheme = header_text
        .split_once(' ')
        .map_or(header_text.as_str(), |(_, rest)| rest)
        .to_string();

    // API tokens: the "token" is the API token itself, not the Basic-encoded pair.
    let raw = if description.grant == GrantKind::ApiToken {
        api_token_secret(ctx)?.unwrap_or(after_scheme)
    } else {
        after_scheme
    };

    match format {
        TokenFormat::Raw => write_stdout_line(&raw),
        TokenFormat::Bearer => write_stdout_line(&header_text),
        TokenFormat::Json => {
            let (token_type, expires_in) = match description.grant {
                GrantKind::ApiToken => ("basic", None),
                _ => (
                    "bearer",
                    description
                        .expires_at
                        .map(|at| (at - chrono::Utc::now()).num_seconds()),
                ),
            };
            ctx.emit(
                json!({
                    "token_type": token_type,
                    "access_token": raw,
                    "authorization": header_text,
                    "grant": description.grant.as_str(),
                    "profile": description.profile,
                    "subdomain": description.subdomain,
                    "scopes": description.scopes,
                    "expires_at": description.expires_at,
                    "expires_in_secs": expires_in,
                }),
                None,
            )?;
        }
    }
    Ok(())
}

fn api_token_secret(ctx: &AppContext) -> Result<Option<String>> {
    use secrecy::ExposeSecret;
    if let Some(t) = &ctx.env.api_token {
        return Ok(Some(t.expose_secret().to_string()));
    }
    let store = read_store(ctx)?;
    Ok(match store.load(&ctx.settings.profile_name)? {
        Some(Credential::ApiToken { token, .. }) => Some(token.expose_secret().to_string()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------------------------
// scopes
// ---------------------------------------------------------------------------------------------

fn run_scopes_list(ctx: &AppContext) -> Result<()> {
    let rows: Vec<Value> = scopes::CATALOGUE
        .iter()
        .map(|s| {
            json!({
                "scope": s.name,
                "family": s.family,
                "access": s.access,
                "description": s.description,
            })
        })
        .collect();
    ctx.emit(Value::Array(rows), None)
}

/// Granted scopes for the active credential: `None` when unknown (static/API token) or when
/// not logged in (a warning is printed; the caller decides what that means).
async fn granted_scopes(ctx: &AppContext) -> Result<Option<Vec<String>>> {
    match ctx.provider().await {
        Ok(p) => Ok(p.granted_scopes()),
        Err(ZdkError::Auth(AuthFailure::NotLoggedIn { profile })) => {
            output::warn(&format!(
                "warning: not logged in for profile '{profile}' — granted scopes are unknown; run `zdk auth login`"
            ));
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

async fn run_scopes_show(ctx: &AppContext, preset_name: &str) -> Result<()> {
    let preset = scopes::preset(preset_name).ok_or_else(|| unknown_preset(preset_name))?;
    let granted = granted_scopes(ctx).await?;
    let rows: Vec<Value> = preset
        .scopes
        .iter()
        .map(|s| {
            let state = match &granted {
                Some(g) => yes_no(scopes::covers(g, s)),
                None => "unknown",
            };
            json!({
                "scope": s,
                "granted": state,
                "description": scopes::lookup(s).map_or("", |d| d.description),
            })
        })
        .collect();
    let missing: Vec<&str> = match &granted {
        Some(g) => preset
            .scopes
            .iter()
            .copied()
            .filter(|s| !scopes::covers(g, s))
            .collect(),
        None => vec![],
    };
    if is_human(ctx) {
        match &granted {
            Some(g) => output::warn(&format!("Granted: {}", g.join(", "))),
            None => output::warn(
                "Granted: unknown (static or API token — the server decides per request)",
            ),
        }
        return ctx.emit(Value::Array(rows), None);
    }
    ctx.emit(
        json!({
            "preset": preset.name,
            "description": preset.description,
            "granted": granted,
            "satisfied": granted.as_ref().map(|_| missing.is_empty()),
            "missing": missing,
            "scopes": rows,
        }),
        None,
    )
}

fn unknown_preset(name: &str) -> ZdkError {
    ZdkError::Usage(format!(
        "unknown preset '{name}': expected one of {}",
        scopes::PRESETS
            .iter()
            .map(|p| p.name)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Is `words` (or a prefix of it) a command the static scope map knows about?
fn known_command(words: &[&str]) -> bool {
    scopes::known_command_paths().iter().any(|path| {
        path.len() <= words.len()
            && path
                .iter()
                .zip(words)
                .all(|(want, have)| want == have || (*want == "orgs" && *have == "organizations"))
    })
}

async fn run_scopes_check(ctx: &AppContext, command: &[String]) -> Result<()> {
    let words: Vec<&str> = command
        .iter()
        .map(|w| w.trim_start_matches("zdk").trim())
        .filter(|w| !w.is_empty())
        .collect();
    if !known_command(&words) {
        return Err(ZdkError::Usage(format!(
            "unknown command `zdk {}`: the scope map covers tickets, comments, users, orgs, search, auth, api, config, completions, man, doctor and version",
            words.join(" ")
        )));
    }
    let required = scopes::required_for(&words);
    let display = format!("zdk {}", words.join(" "));
    let value = |granted: Option<&Vec<String>>, satisfied: Option<bool>, missing: Vec<&str>| {
        json!({
            "command": display,
            "required": required,
            "granted": granted,
            "satisfied": satisfied,
            "missing": missing,
        })
    };

    if required.is_empty() {
        return ctx.emit_or_line(
            value(None, Some(true), vec![]),
            &format!("`{display}` requires no OAuth scope"),
        );
    }
    let granted = granted_scopes(ctx).await?;
    let Some(granted) = granted else {
        return ctx.emit_or_line(
            value(None, None, vec![]),
            &format!(
                "`{display}` requires {} — granted scopes are unknown (static or API token), so Zendesk decides per request",
                required.join(", ")
            ),
        );
    };
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|r| !scopes::covers(&granted, r))
        .collect();
    let have = if granted.is_empty() {
        "nothing".to_string()
    } else {
        granted.join(", ")
    };
    if missing.is_empty() {
        return ctx.emit_or_line(
            value(Some(&granted), Some(true), vec![]),
            &format!(
                "`{display}` requires {} — you have {have}",
                required.join(", ")
            ),
        );
    }
    ctx.emit_or_line(
        value(Some(&granted), Some(false), missing.clone()),
        &format!(
            "`{display}` requires {} — you have {have}",
            missing.join(", ")
        ),
    )?;
    scopes::preflight(&granted, required)
}

async fn run_scopes_preset(
    ctx: &AppContext,
    name: &str,
    login: bool,
    client_credentials: bool,
    no_browser: bool,
) -> Result<()> {
    let preset = scopes::preset(name).ok_or_else(|| unknown_preset(name))?;
    if login {
        let args = LoginArgs {
            preset: vec![preset.name.to_string()],
            client_credentials,
            no_browser,
            ..LoginArgs::default()
        };
        return run_login(&args, ctx).await;
    }
    let rows: Vec<Value> = preset
        .scopes
        .iter()
        .map(|s| {
            json!({
                "scope": s,
                "description": scopes::lookup(s).map_or("", |d| d.description),
            })
        })
        .collect();
    if is_human(ctx) {
        return ctx.emit(Value::Array(rows), None);
    }
    ctx.emit(
        json!({
            "preset": preset.name,
            "description": preset.description,
            "scopes": preset.scopes,
            "details": rows,
        }),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_secs_keeps_the_two_largest_units() {
        assert_eq!(human_secs(45), "45s");
        assert_eq!(human_secs(1799), "29m 59s");
        assert_eq!(human_secs(3600 + 1800 + 5), "1h 30m");
        assert_eq!(human_secs(2 * 86_400 + 3 * 3600 + 7), "2d 3h");
        assert_eq!(human_secs(-90), "1m 30s", "sign is dropped");
        assert_eq!(human_secs(0), "0s");
    }

    #[test]
    fn expiry_phrases() {
        let at = chrono::Utc::now() + chrono::TimeDelta::minutes(30);
        assert!(expiry_phrase(Some(1800), Some(at)).starts_with("expires in 30m"));
        assert_eq!(expiry_phrase(Some(-180), Some(at)), "expired 3m ago");
        assert_eq!(expiry_phrase(None, None), "no expiry");
    }

    #[test]
    fn known_commands_follow_the_scope_map() {
        assert!(known_command(&["tickets", "update"]));
        assert!(known_command(&["organizations", "list"]));
        assert!(known_command(&["config", "init"]));
        assert!(known_command(&["search"]));
        assert!(!known_command(&["frob"]));
        assert!(!known_command(&[]));
    }

    #[test]
    fn user_helpers_tolerate_missing_fields() {
        let body = json!({"user": {"id": 1, "name": "Ada", "role": "admin"}});
        let user = unwrap_user(body);
        assert_eq!(user_line(&user), "Ada (admin)");
        assert_eq!(user_summary(&user)["email"], Value::Null);
        let with_email = json!({"name": "Ada", "email": "a@x", "role": "agent"});
        assert_eq!(user_line(&with_email), "Ada <a@x> (agent)");
        assert_eq!(unwrap_user(json!({"other": 1}))["other"], 1);
    }
}
