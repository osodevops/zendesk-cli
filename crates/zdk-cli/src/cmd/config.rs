//! `zdk config …` — init / show / get / set / edit / validate / path / profiles.
//!
//! Every mutation goes through `toml_edit` on the raw file text so the user's comments and
//! layout survive, and every write is atomic with owner-only permissions.

use std::path::Path;

use clap::{Args, Subcommand, ValueEnum};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};
use zdk_core::auth::GrantKind;
use zdk_core::config::{
    ConfigFile, DEFAULT_PROFILE, ProfileConfig, Starter, profiles, starter_toml,
};
use zdk_core::output::{self, OutputFormat, write_stdout, write_stdout_line};
use zdk_core::util::fs::atomic_write_0600;
use zdk_core::{Result, ZdkError};

use crate::context::{AppContext, read_line_from_stdin};

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Write a commented starter config file
    Init(InitArgs),

    /// Print the config file (TOML on a terminal, JSON when piped)
    Show {
        /// Print the fully resolved settings (flags, env, profile and defaults merged)
        #[arg(long)]
        effective: bool,
        /// With --effective: show secret environment values instead of ***
        #[arg(long)]
        reveal_secrets: bool,
    },

    /// Read one key (dotted path, e.g. default.page_size or profiles.prod.subdomain)
    Get {
        /// Dotted key
        key: String,
    },

    /// Set one key. Values are typed: true/false, integers, a,b lists; anything else is a string
    Set {
        /// Dotted key
        key: String,
        /// New value
        value: String,
    },

    /// Open the config file in $VISUAL / $EDITOR, then validate it
    Edit,

    /// Check the config file: hard errors exit 10, warnings print to stderr and exit 0
    Validate,

    /// Print the config file path
    Path,

    /// Manage profiles (one per Zendesk instance)
    Profiles {
        #[command(subcommand)]
        command: ProfilesCommand,
    },
}

/// OAuth grant / auth method for a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GrantArg {
    /// Browser login with PKCE (interactive default)
    #[value(name = "authorization_code", alias = "authorization-code")]
    AuthorizationCode,
    /// Confidential client for CI / automation
    #[value(name = "client_credentials", alias = "client-credentials")]
    ClientCredentials,
    /// Legacy API token (Zendesk stops issuing them on 27 Oct 2026)
    #[value(name = "api_token", alias = "api-token")]
    ApiToken,
}

impl GrantArg {
    const fn into_kind(self) -> GrantKind {
        match self {
            Self::AuthorizationCode => GrantKind::AuthorizationCode,
            Self::ClientCredentials => GrantKind::ClientCredentials,
            Self::ApiToken => GrantKind::ApiToken,
        }
    }
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Zendesk subdomain (the `acme` in acme.zendesk.com)
    #[arg(long, value_name = "SUB")]
    pub subdomain: Option<String>,
    /// OAuth client identifier (Admin Center → Apps and integrations → APIs → OAuth clients)
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
    /// Auth method for the profile
    #[arg(long, value_enum, value_name = "GRANT")]
    pub grant_type: Option<GrantArg>,
    /// Agent email (api_token only)
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,
    /// Scopes to request at login, comma-separated
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    pub scopes: Vec<String>,
    /// Never prompt; every required value must be a flag
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Debug, Subcommand)]
pub enum ProfilesCommand {
    /// List profiles
    List,
    /// Add (or replace) a profile
    Add(AddProfileArgs),
    /// Remove a profile from the config file (stored credentials are not touched)
    Remove {
        /// Profile name
        name: String,
    },
    /// Rename a profile
    Rename {
        /// Current name
        old: String,
        /// New name
        new: String,
    },
    /// Make a profile the active one (default.active_profile)
    Switch {
        /// Profile name
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct AddProfileArgs {
    /// Profile name
    pub name: String,
    /// Zendesk subdomain
    #[arg(long, value_name = "SUB")]
    pub subdomain: String,
    /// OAuth client identifier
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
    /// Auth method
    #[arg(long, value_enum, value_name = "GRANT")]
    pub grant_type: Option<GrantArg>,
    /// Scopes, comma-separated
    #[arg(long, value_name = "A,B", value_delimiter = ',')]
    pub scopes: Vec<String>,
    /// Agent email (api_token only)
    #[arg(long, value_name = "EMAIL")]
    pub email: Option<String>,
    /// Zendesk plan (informational)
    #[arg(long, value_name = "PLAN")]
    pub plan: Option<String>,
    /// Fixed loopback port for the OAuth callback
    #[arg(long, value_name = "PORT")]
    pub callback_port: Option<u16>,
    /// Credential store for this profile
    #[arg(long, value_enum, value_name = "STORE")]
    pub credential_store: Option<crate::cli::StoreArg>,
    /// Also make it the active profile
    #[arg(long)]
    pub switch: bool,
}

pub fn run(args: ConfigArgs, ctx: &AppContext) -> Result<()> {
    match args.command {
        ConfigCommand::Init(init) => run_init(&init, ctx),
        ConfigCommand::Show {
            effective,
            reveal_secrets,
        } => run_show(ctx, effective, reveal_secrets),
        ConfigCommand::Get { key } => run_get(ctx, &key),
        ConfigCommand::Set { key, value } => run_set(ctx, &key, &value),
        ConfigCommand::Edit => run_edit(ctx),
        ConfigCommand::Validate => run_validate(ctx),
        ConfigCommand::Path => run_path(ctx),
        ConfigCommand::Profiles { command } => match command {
            ProfilesCommand::List => run_profiles_list(ctx),
            ProfilesCommand::Add(add) => run_profiles_add(&add, ctx),
            ProfilesCommand::Remove { name } => run_profiles_remove(ctx, &name),
            ProfilesCommand::Rename { old, new } => run_profiles_rename(ctx, &old, &new),
            ProfilesCommand::Switch { name } => run_profiles_switch(ctx, &name),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn config_path(ctx: &AppContext) -> &Path {
    &ctx.settings.paths.config_file
}

/// Raw file text; a missing file is an empty document.
fn read_text(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(ZdkError::Config(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    let text = read_text(path)?;
    profiles::parse_document(&text).map_err(|e| match e {
        ZdkError::Config(m) => ZdkError::Config(format!("{}: {m}", path.display())),
        other => other,
    })
}

fn write_document(path: &Path, doc: &DocumentMut) -> Result<()> {
    atomic_write_0600(path, doc.to_string().as_bytes())
        .map_err(|e| ZdkError::Config(format!("cannot write {}: {e}", path.display())))
}

fn is_human(ctx: &AppContext) -> bool {
    ctx.output == OutputFormat::Table && ctx.settings.output.jq.is_none()
}

fn prompt(question: &str, default: Option<&str>) -> Result<String> {
    let suffix = default.map(|d| format!(" [{d}]")).unwrap_or_default();
    let answer = read_line_from_stdin(&format!("{question}{suffix}: "))?;
    let answer = answer.trim();
    if answer.is_empty() {
        default
            .map(str::to_string)
            .ok_or_else(|| ZdkError::Usage(format!("{question} is required")))
    } else {
        Ok(answer.to_string())
    }
}

/// A `toml_edit` item as JSON (values, tables and arrays of tables alike).
fn item_to_json(item: &Item) -> Result<Value> {
    let text = match item {
        Item::None => return Ok(Value::Null),
        Item::Value(v) => format!("v = {v}\n"),
        Item::Table(t) => {
            let mut doc = DocumentMut::new();
            doc.as_table_mut().insert("v", Item::Table(t.clone()));
            doc.to_string()
        }
        Item::ArrayOfTables(a) => {
            let mut doc = DocumentMut::new();
            doc.as_table_mut()
                .insert("v", Item::ArrayOfTables(a.clone()));
            doc.to_string()
        }
    };
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| ZdkError::Config(format!("cannot read value: {e}")))?;
    let value = table
        .get("v")
        .cloned()
        .unwrap_or(toml::Value::String(String::new()));
    serde_json::to_value(value).map_err(ZdkError::from)
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        output::warn(&format!("warning: {w}"));
    }
}

// ---------------------------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------------------------

fn run_init(args: &InitArgs, ctx: &AppContext) -> Result<()> {
    let path = config_path(ctx);
    if path.exists() && !ctx.settings.yes {
        return Err(ZdkError::Usage(format!(
            "{} already exists; pass --yes to overwrite it, or use `zdk config profiles add` to add a profile",
            path.display()
        )));
    }
    let interactive = !args.non_interactive && ctx.stdin_is_tty && ctx.stdout_is_tty;
    let required = |flag: &str, value: Option<&String>, question: &str| -> Result<String> {
        match value {
            Some(v) => Ok(v.clone()),
            None if interactive => prompt(question, None),
            None => Err(ZdkError::Usage(format!(
                "--{flag} is required with --non-interactive (or when not on a terminal)"
            ))),
        }
    };

    let profile = ctx
        .args
        .profile
        .clone()
        .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
    let subdomain = required(
        "subdomain",
        args.subdomain.as_ref(),
        "Zendesk subdomain (the 'acme' in acme.zendesk.com)",
    )?;
    let grant_type = match args.grant_type {
        Some(g) => g.into_kind(),
        None if interactive => {
            let answer = prompt(
                "Auth method (authorization_code, client_credentials, api_token)",
                Some("authorization_code"),
            )?;
            GrantArg::from_str(&answer, true)
                .map(GrantArg::into_kind)
                .map_err(ZdkError::Usage)?
        }
        None => GrantKind::AuthorizationCode,
    };
    let client_id = if grant_type == GrantKind::ApiToken {
        args.client_id.clone().unwrap_or_default()
    } else {
        required(
            "client-id",
            args.client_id.as_ref(),
            "OAuth client identifier",
        )?
    };
    let email = match (&args.email, grant_type) {
        (Some(e), _) => Some(e.clone()),
        (None, GrantKind::ApiToken) => {
            Some(required("email", None, "Agent email (for API-token auth)")?)
        }
        (None, _) => None,
    };

    let starter = Starter {
        profile: profile.clone(),
        subdomain: subdomain.clone(),
        client_id: client_id.clone(),
        grant_type,
        email,
        scopes: args.scopes.clone(),
    };
    let text = starter_toml(&starter);
    ConfigFile::from_toml(&text)?;
    atomic_write_0600(path, text.as_bytes())
        .map_err(|e| ZdkError::Config(format!("cannot write {}: {e}", path.display())))?;

    let value = json!({
        "path": path,
        "profile": profile,
        "subdomain": subdomain,
        "client_id": client_id,
        "grant_type": grant_type.as_str(),
    });
    let line = format!(
        "Wrote {} (profile '{profile}', subdomain '{subdomain}'). Next: `zdk auth login`",
        path.display()
    );
    ctx.emit_or_line(value, &line)
}

// ---------------------------------------------------------------------------------------------
// show / get / set / edit / validate / path
// ---------------------------------------------------------------------------------------------

fn run_show(ctx: &AppContext, effective: bool, reveal_secrets: bool) -> Result<()> {
    let path = config_path(ctx);
    if effective {
        let env_secrets: serde_json::Map<String, Value> = ctx
            .env
            .secrets_present()
            .into_iter()
            .map(|name| {
                let shown = if reveal_secrets {
                    ctx.env
                        .secret(name)
                        .map_or_else(String::new, |s| s.expose_secret().to_string())
                } else {
                    "***".to_string()
                };
                (name.to_string(), Value::String(shown))
            })
            .collect();
        let file = ConfigFile::load(path)?;
        let value = json!({
            "config_file": path,
            "config_file_exists": path.exists(),
            "settings": serde_json::to_value(&ctx.settings)?,
            "env_secrets": env_secrets,
            "warnings": profiles::validate(&file),
        });
        if is_human(ctx) {
            let toml_text = toml::to_string_pretty(&value)
                .map_err(|e| ZdkError::Other(format!("cannot render TOML: {e}")))?;
            write_stdout(toml_text.as_bytes());
            return Ok(());
        }
        return ctx.emit(value, None);
    }

    if is_human(ctx) {
        let text = read_text(path)?;
        if text.is_empty() {
            write_stdout_line(&format!(
                "# no config file at {} — built-in defaults apply. Run `zdk config init`.",
                path.display()
            ));
            write_stdout(ConfigFile::default().to_toml()?.as_bytes());
        } else {
            write_stdout(text.as_bytes());
            if !text.ends_with('\n') {
                write_stdout(b"\n");
            }
        }
        return Ok(());
    }
    let file = ConfigFile::load(path)?;
    ctx.emit(serde_json::to_value(&file)?, None)
}

fn run_get(ctx: &AppContext, key: &str) -> Result<()> {
    let doc = read_document(config_path(ctx))?;
    let item = profiles::get_value(&doc, key).ok_or_else(|| ZdkError::NotFound {
        resource: "config key".into(),
        id: key.into(),
        request_id: None,
    })?;
    let value = item_to_json(item)?;
    if is_human(ctx) {
        match &value {
            Value::String(s) => write_stdout_line(s),
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                write_stdout_line(&value.to_string());
            }
            _ => write_stdout_line(&serde_json::to_string_pretty(&value)?),
        }
        return Ok(());
    }
    ctx.emit(value, None)
}

fn run_set(ctx: &AppContext, key: &str, value: &str) -> Result<()> {
    let path = config_path(ctx);
    let mut doc = read_document(path)?;
    let stored = profiles::set_value_typed(&mut doc, key, value)?;
    write_document(path, &doc)?;
    let json_value = item_to_json(&Item::Value(stored.clone()))?;
    if is_human(ctx) {
        output::warn(&format!("set {key} = {stored} in {}", path.display()));
        return Ok(());
    }
    ctx.emit(
        json!({ "key": key, "value": json_value, "path": path }),
        None,
    )
}

fn run_edit(ctx: &AppContext) -> Result<()> {
    let path = config_path(ctx);
    let editor = ctx.env.editor.clone().ok_or_else(|| {
        ZdkError::Usage(
            "no editor configured: set $VISUAL or $EDITOR (e.g. `export EDITOR=vim`)".into(),
        )
    })?;
    if !path.exists() {
        let text = starter_toml(&Starter {
            profile: ctx
                .args
                .profile
                .clone()
                .unwrap_or_else(|| DEFAULT_PROFILE.into()),
            ..Starter::default()
        });
        atomic_write_0600(path, text.as_bytes())
            .map_err(|e| ZdkError::Config(format!("cannot write {}: {e}", path.display())))?;
    }

    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| ZdkError::Usage("$EDITOR is empty".into()))?;
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()
        .map_err(|e| ZdkError::Other(format!("cannot run editor '{editor}': {e}")))?;
    if !status.success() {
        return Err(ZdkError::Other(format!(
            "editor '{editor}' exited with {status}"
        )));
    }

    let file = ConfigFile::load(path)?;
    let warnings = profiles::validate(&file);
    print_warnings(&warnings);
    if is_human(ctx) {
        return Ok(());
    }
    ctx.emit(
        json!({ "path": path, "ok": true, "warnings": warnings }),
        None,
    )
}

fn run_validate(ctx: &AppContext) -> Result<()> {
    let path = config_path(ctx);
    let mut warnings = Vec::new();
    if !path.exists() {
        warnings.push(format!(
            "no config file at {} — built-in defaults apply (run `zdk config init`)",
            path.display()
        ));
    }
    let file = ConfigFile::load(path)?;
    warnings.extend(profiles::validate(&file));
    print_warnings(&warnings);
    let value = json!({ "path": path, "ok": true, "profiles": file.profiles.keys().collect::<Vec<_>>(), "warnings": warnings });
    let line = if warnings.is_empty() {
        format!("{}: OK", path.display())
    } else {
        format!("{}: OK with {} warning(s)", path.display(), warnings.len())
    };
    ctx.emit_or_line(value, &line)
}

fn run_path(ctx: &AppContext) -> Result<()> {
    let paths = &ctx.settings.paths;
    let value = json!({
        "config_file": paths.config_file,
        "config_dir": paths.config_dir,
        "state_dir": paths.state_dir,
        "cache_dir": paths.cache_dir,
        "exists": paths.config_file.exists(),
    });
    ctx.emit_or_line(value, &paths.config_file.display().to_string())
}

// ---------------------------------------------------------------------------------------------
// profiles
// ---------------------------------------------------------------------------------------------

fn profile_row(name: &str, p: &ProfileConfig, file: &ConfigFile) -> Value {
    json!({
        "profile": name,
        "subdomain": p.subdomain,
        "auth": p.grant_type.unwrap_or(file.auth.method).as_str(),
        "plan": p.plan,
        "client_id": p.client_id,
        "scopes": p.scopes,
        "email": p.email,
        "credential_store": p.credential_store.unwrap_or(file.auth.credential_store).as_str(),
        "active": file.active_profile_name() == name,
    })
}

fn run_profiles_list(ctx: &AppContext) -> Result<()> {
    let file = ConfigFile::load(config_path(ctx))?;
    let rows: Vec<Value> = file
        .profiles
        .iter()
        .map(|(name, p)| profile_row(name, p, &file))
        .collect();
    if rows.is_empty() && is_human(ctx) {
        output::warn(&format!(
            "no profiles in {} — run `zdk config init` or `zdk config profiles add`",
            config_path(ctx).display()
        ));
    }
    ctx.emit(Value::Array(rows), Some("profiles"))
}

fn run_profiles_add(args: &AddProfileArgs, ctx: &AppContext) -> Result<()> {
    let path = config_path(ctx);
    let mut doc = read_document(path)?;
    let profile = ProfileConfig {
        subdomain: Some(args.subdomain.clone()),
        client_id: args.client_id.clone(),
        grant_type: args.grant_type.map(GrantArg::into_kind),
        scopes: args.scopes.clone(),
        plan: args.plan.clone(),
        email: args.email.clone(),
        callback_port: args.callback_port,
        credential_store: args
            .credential_store
            .map(crate::cli::StoreArg::into_selector),
        extra: std::collections::BTreeMap::default(),
    };
    let existed = profiles::profile_names(&doc)
        .iter()
        .any(|n| n == &args.name);
    profiles::add_profile(&mut doc, &args.name, &profile)?;
    let no_active = profiles::get_value(&doc, "default.active_profile").is_none();
    if args.switch || no_active {
        profiles::switch_profile(&mut doc, &args.name)?;
    }
    write_document(path, &doc)?;

    let file = ConfigFile::from_toml(&doc.to_string())?;
    print_warnings(
        &profiles::validate(&file)
            .into_iter()
            .filter(|w| w.contains(&format!("profiles.{}", args.name)))
            .collect::<Vec<_>>(),
    );
    let row = profile_row(&args.name, &profile, &file);
    let verb = if existed { "Updated" } else { "Added" };
    let line = format!(
        "{verb} profile '{}' ({}.zendesk.com) in {}",
        args.name,
        args.subdomain,
        path.display()
    );
    if is_human(ctx) {
        output::warn(&line);
        return Ok(());
    }
    ctx.emit(row, None)
}

fn run_profiles_remove(ctx: &AppContext, name: &str) -> Result<()> {
    let path = config_path(ctx);
    let mut doc = read_document(path)?;
    if !profiles::remove_profile(&mut doc, name)? {
        return Err(ZdkError::NotFound {
            resource: "profile".into(),
            id: name.into(),
            request_id: None,
        });
    }
    write_document(path, &doc)?;
    let line = format!(
        "Removed profile '{name}' from {} (stored credentials are untouched; use `zdk auth logout`)",
        path.display()
    );
    if is_human(ctx) {
        output::warn(&line);
        return Ok(());
    }
    ctx.emit(
        json!({ "profile": name, "removed": true, "path": path }),
        None,
    )
}

fn run_profiles_rename(ctx: &AppContext, old: &str, new: &str) -> Result<()> {
    let path = config_path(ctx);
    let mut doc = read_document(path)?;
    profiles::rename_profile(&mut doc, old, new)?;
    write_document(path, &doc)?;
    if is_human(ctx) {
        output::warn(&format!(
            "Renamed profile '{old}' to '{new}' in {}",
            path.display()
        ));
        return Ok(());
    }
    ctx.emit(json!({ "old": old, "new": new, "path": path }), None)
}

fn run_profiles_switch(ctx: &AppContext, name: &str) -> Result<()> {
    let path = config_path(ctx);
    let mut doc = read_document(path)?;
    profiles::switch_profile(&mut doc, name)?;
    write_document(path, &doc)?;
    let subdomain = profiles::get_value(&doc, &format!("profiles.{name}.subdomain"))
        .and_then(Item::as_str)
        .unwrap_or("?")
        .to_string();
    if is_human(ctx) {
        output::warn(&format!(
            "Active profile is now '{name}' ({subdomain}.zendesk.com)"
        ));
        return Ok(());
    }
    ctx.emit(
        json!({ "active_profile": name, "subdomain": subdomain, "path": path }),
        None,
    )
}
