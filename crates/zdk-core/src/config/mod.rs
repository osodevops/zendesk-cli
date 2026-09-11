//! Configuration: the on-disk file (PRD §13.1), environment overrides (§13.2) and the
//! resolved [`Settings`] every command receives.
//!
//! Precedence, implemented once in [`settings::Settings::resolve`]:
//! CLI flag → environment variable → active profile → `[default]` → built-in default.
//!
//! No secret is ever a config-file field: tokens, client secrets and API tokens live in the
//! credential store or the environment only.

pub mod env;
pub mod profiles;
pub mod settings;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::auth::GrantKind;
use crate::output::OutputFormat;
use crate::store::StoreSelector;
use crate::{Result, ZdkError};

pub use env::EnvOverrides;
pub use settings::{
    AuthSettings, GlobalArgs, OutputSettings, RateLimitSettings, RetrySettings, Settings,
};

/// Application directory name under the platform config/state/cache roots.
pub const APP_DIR: &str = "zendesk-cli";
/// Config file name inside the config directory.
pub const CONFIG_FILE: &str = "config.toml";
/// Profile name used when nothing selects one.
pub const DEFAULT_PROFILE: &str = "default";

/// Unknown keys found while loading, as dotted paths (`rate_limit.foo`).
pub type ExtraKeys = BTreeMap<String, toml::Value>;

/// How to behave when a rate-limit budget is exhausted (PRD §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitStrategy {
    /// Sleep until the budget refills, then continue (default).
    #[default]
    Wait,
    /// Exit 7 immediately.
    Fail,
    /// Ignore the local governor and rely on 429 handling.
    Burst,
}

impl RateLimitStrategy {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "wait" => Some(Self::Wait),
            "fail" => Some(Self::Fail),
            "burst" => Some(Self::Burst),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Fail => "fail",
            Self::Burst => "burst",
        }
    }
}

impl fmt::Display for RateLimitStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Backoff jitter mode for retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Jitter {
    /// `rand(0, backoff)` (default).
    #[default]
    Full,
    /// `backoff/2 + rand(0, backoff/2)`.
    Equal,
    /// No jitter.
    None,
}

/// `[default]` — everything here is optional so the file can stay tiny.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DefaultSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolve_names: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_destructive: Option<bool>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

/// `[auth]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthSection {
    #[serde(default = "AuthSection::default_method")]
    pub method: GrantKind,
    /// Fixed loopback port for clients registered with an explicit redirect URI; `None` = ephemeral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    #[serde(default = "yes")]
    pub auto_refresh: bool,
    #[serde(default = "AuthSection::default_refresh_at_percent")]
    pub refresh_at_percent: u8,
    #[serde(default)]
    pub credential_store: StoreSelector,
    #[serde(default)]
    pub suppress_deprecation: bool,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl AuthSection {
    const fn default_method() -> GrantKind {
        GrantKind::AuthorizationCode
    }
    const fn default_refresh_at_percent() -> u8 {
        80
    }
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            method: Self::default_method(),
            callback_port: None,
            auto_refresh: true,
            refresh_at_percent: Self::default_refresh_at_percent(),
            credential_store: StoreSelector::Auto,
            suppress_deprecation: false,
            extra: ExtraKeys::new(),
        }
    }
}

/// `[rate_limit]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimitSection {
    #[serde(default)]
    pub strategy: RateLimitStrategy,
    #[serde(default = "RateLimitSection::default_max_concurrency")]
    pub max_concurrency: u32,
    #[serde(default = "RateLimitSection::default_reserve_percent")]
    pub reserve_percent: u8,
    #[serde(default = "RateLimitSection::default_warn_threshold")]
    pub warn_threshold: u8,
    #[serde(default = "yes")]
    pub respect_retry_after: bool,
    #[serde(default)]
    pub high_volume_addon: bool,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl RateLimitSection {
    const fn default_max_concurrency() -> u32 {
        4
    }
    const fn default_reserve_percent() -> u8 {
        10
    }
    const fn default_warn_threshold() -> u8 {
        50
    }
}

impl Default for RateLimitSection {
    fn default() -> Self {
        Self {
            strategy: RateLimitStrategy::Wait,
            max_concurrency: Self::default_max_concurrency(),
            reserve_percent: Self::default_reserve_percent(),
            warn_threshold: Self::default_warn_threshold(),
            respect_retry_after: true,
            high_volume_addon: false,
            extra: ExtraKeys::new(),
        }
    }
}

/// `[retry]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrySection {
    #[serde(default = "RetrySection::default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "RetrySection::default_base_ms")]
    pub base_ms: u64,
    #[serde(default = "RetrySection::default_max_ms")]
    pub max_ms: u64,
    #[serde(default)]
    pub jitter: Jitter,
    #[serde(default = "RetrySection::default_retry_on")]
    pub retry_on: Vec<u16>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl RetrySection {
    const fn default_max_attempts() -> u32 {
        6
    }
    const fn default_base_ms() -> u64 {
        1000
    }
    const fn default_max_ms() -> u64 {
        60_000
    }
    fn default_retry_on() -> Vec<u16> {
        vec![429, 500, 502, 503, 504]
    }
}

impl Default for RetrySection {
    fn default() -> Self {
        Self {
            max_attempts: Self::default_max_attempts(),
            base_ms: Self::default_base_ms(),
            max_ms: Self::default_max_ms(),
            jitter: Jitter::Full,
            retry_on: Self::default_retry_on(),
            extra: ExtraKeys::new(),
        }
    }
}

/// `[cache]` — typed now, used from v0.3 (name resolution / schema cache).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheSection {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "CacheSection::default_schema_ttl")]
    pub schema_ttl_seconds: u64,
    #[serde(default = "CacheSection::default_list_ttl")]
    pub list_ttl_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default = "CacheSection::default_max_size_mb")]
    pub max_size_mb: u64,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl CacheSection {
    const fn default_schema_ttl() -> u64 {
        3600
    }
    const fn default_list_ttl() -> u64 {
        60
    }
    const fn default_max_size_mb() -> u64 {
        500
    }
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            enabled: true,
            schema_ttl_seconds: Self::default_schema_ttl(),
            list_ttl_seconds: Self::default_list_ttl(),
            directory: None,
            max_size_mb: Self::default_max_size_mb(),
            extra: ExtraKeys::new(),
        }
    }
}

/// `[sync]` — typed now, used from v0.4 (incremental exports).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
    #[serde(default = "SyncSection::default_dedupe_window")]
    pub dedupe_window: u64,
    #[serde(default = "SyncSection::default_output_format")]
    pub output_format: String,
    #[serde(default = "SyncSection::default_min_interval")]
    pub min_interval_seconds: u64,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl SyncSection {
    const fn default_dedupe_window() -> u64 {
        10_000
    }
    fn default_output_format() -> String {
        "ndjson".into()
    }
    const fn default_min_interval() -> u64 {
        300
    }
}

impl Default for SyncSection {
    fn default() -> Self {
        Self {
            state_dir: None,
            dedupe_window: Self::default_dedupe_window(),
            output_format: Self::default_output_format(),
            min_interval_seconds: Self::default_min_interval(),
            extra: ExtraKeys::new(),
        }
    }
}

/// `[jobs]` — typed now, used from v0.3 (bulk operations).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobsSection {
    #[serde(default = "JobsSection::default_poll_base_ms")]
    pub poll_base_ms: u64,
    #[serde(default = "JobsSection::default_poll_max_ms")]
    pub poll_max_ms: u64,
    #[serde(default = "JobsSection::default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default = "yes")]
    pub persist_results: bool,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl JobsSection {
    const fn default_poll_base_ms() -> u64 {
        2000
    }
    const fn default_poll_max_ms() -> u64 {
        30_000
    }
    const fn default_max_concurrent() -> u32 {
        30
    }
}

impl Default for JobsSection {
    fn default() -> Self {
        Self {
            poll_base_ms: Self::default_poll_base_ms(),
            poll_max_ms: Self::default_poll_max_ms(),
            max_concurrent: Self::default_max_concurrent(),
            persist_results: true,
            extra: ExtraKeys::new(),
        }
    }
}

/// `[rules]` — typed now, used from v0.5 (configuration as code).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RulesSection {
    #[serde(default = "RulesSection::default_manifest_dir")]
    pub manifest_dir: String,
    #[serde(default = "yes")]
    pub symbolic_refs: bool,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl RulesSection {
    fn default_manifest_dir() -> String {
        "./zendesk-config".into()
    }
}

impl Default for RulesSection {
    fn default() -> Self {
        Self {
            manifest_dir: Self::default_manifest_dir(),
            symbolic_refs: true,
            extra: ExtraKeys::new(),
        }
    }
}

/// `[audit]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuditSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

/// `[profiles.<name>]` — one Zendesk instance + OAuth client. Never holds a secret.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdomain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// `None` means "inherit `[auth].method`".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_type: Option<GrantKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Legacy API-token auth needs the agent email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_store: Option<StoreSelector>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

/// The whole config file. Every section has defaults so a missing file is valid.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub default: DefaultSection,
    #[serde(default)]
    pub auth: AuthSection,
    #[serde(default)]
    pub rate_limit: RateLimitSection,
    #[serde(default)]
    pub retry: RetrySection,
    #[serde(default)]
    pub cache: CacheSection,
    #[serde(default)]
    pub sync: SyncSection,
    #[serde(default)]
    pub jobs: JobsSection,
    #[serde(default)]
    pub rules: RulesSection,
    #[serde(default)]
    pub audit: AuditSection,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default, flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: ExtraKeys,
}

impl ConfigFile {
    /// Parse TOML text. Unknown keys are kept (see [`ConfigFile::unknown_keys`]), not rejected.
    pub fn from_toml(text: &str) -> Result<Self> {
        toml::from_str(text)
            .map_err(|e| ZdkError::Config(format!("invalid config TOML: {}", e.message())))
    }

    /// Load from disk. A missing file yields the defaults; unreadable or unparsable is an error.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text).map_err(|e| match e {
                ZdkError::Config(msg) => ZdkError::Config(format!("{}: {msg}", path.display())),
                other => other,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ZdkError::Config(format!(
                "cannot read {}: {e}",
                path.display()
            ))),
        }
    }

    /// Serialise to TOML (comments are not preserved — use `profiles::*` on the raw text for edits).
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|e| ZdkError::Config(format!("cannot serialise config: {e}")))
    }

    /// Write atomically with owner-only permissions.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()?;
        crate::util::fs::atomic_write_0600(path, text.as_bytes())
            .map_err(|e| ZdkError::Config(format!("cannot write {}: {e}", path.display())))
    }

    /// Dotted paths of every key the schema does not know about.
    #[must_use]
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |section: &str, extra: &ExtraKeys| {
            for key in extra.keys() {
                out.push(if section.is_empty() {
                    key.clone()
                } else {
                    format!("{section}.{key}")
                });
            }
        };
        push("", &self.extra);
        push("default", &self.default.extra);
        push("auth", &self.auth.extra);
        push("rate_limit", &self.rate_limit.extra);
        push("retry", &self.retry.extra);
        push("cache", &self.cache.extra);
        push("sync", &self.sync.extra);
        push("jobs", &self.jobs.extra);
        push("rules", &self.rules.extra);
        push("audit", &self.audit.extra);
        for (name, profile) in &self.profiles {
            push(&format!("profiles.{name}"), &profile.extra);
        }
        out
    }

    /// The profile name that applies when nothing overrides it.
    #[must_use]
    pub fn active_profile_name(&self) -> &str {
        self.default
            .active_profile
            .as_deref()
            .unwrap_or(DEFAULT_PROFILE)
    }
}

/// Where the CLI keeps things. Resolved once from the environment and `--config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Paths {
    /// `config.toml`
    pub config_file: PathBuf,
    /// Directory holding the config file (and the encrypted credential file).
    pub config_dir: PathBuf,
    /// Rate-limit state, store decision, audit log default.
    pub state_dir: PathBuf,
    /// Schema/name caches (v0.3+).
    pub cache_dir: PathBuf,
}

impl Paths {
    /// Resolve directories: `--config`/`ZENDESK_CONFIG` for the file, `XDG_*` when set (on any OS),
    /// else the platform defaults from `dirs`.
    pub fn resolve(env: &EnvOverrides, cli_config: Option<&Path>) -> Result<Self> {
        let default_config_dir = env
            .xdg_config_home
            .clone()
            .or_else(dirs::config_dir)
            .ok_or_else(|| {
                ZdkError::Config(
                    "cannot determine the config directory (set XDG_CONFIG_HOME or --config)"
                        .into(),
                )
            })?
            .join(APP_DIR);

        let explicit_file = cli_config
            .map(Path::to_path_buf)
            .or_else(|| env.config.clone());
        let (config_file, config_dir) = match explicit_file {
            Some(file) => {
                let dir = file
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map_or(default_config_dir.clone(), Path::to_path_buf);
                (file, dir)
            }
            None => (default_config_dir.join(CONFIG_FILE), default_config_dir),
        };

        let state_dir = env
            .xdg_state_home
            .clone()
            .or_else(dirs::state_dir)
            .or_else(dirs::data_local_dir)
            .ok_or_else(|| {
                ZdkError::Config("cannot determine the state directory (set XDG_STATE_HOME)".into())
            })?
            .join(APP_DIR);

        let cache_dir = env
            .xdg_cache_home
            .clone()
            .or_else(dirs::cache_dir)
            .ok_or_else(|| {
                ZdkError::Config("cannot determine the cache directory (set XDG_CACHE_HOME)".into())
            })?
            .join(APP_DIR);

        Ok(Self {
            config_file,
            config_dir,
            state_dir,
            cache_dir,
        })
    }
}

/// Inputs for the commented starter file written by `zdk config init`.
#[derive(Debug, Clone)]
pub struct Starter {
    pub profile: String,
    pub subdomain: String,
    pub client_id: String,
    pub grant_type: GrantKind,
    pub email: Option<String>,
    pub scopes: Vec<String>,
}

impl Default for Starter {
    fn default() -> Self {
        Self {
            profile: DEFAULT_PROFILE.into(),
            subdomain: String::new(),
            client_id: String::new(),
            grant_type: GrantKind::AuthorizationCode,
            email: None,
            scopes: Vec::new(),
        }
    }
}

/// Render the starter `config.toml`, with comments explaining every section.
#[must_use]
pub fn starter_toml(s: &Starter) -> String {
    let profile = if s.profile.is_empty() {
        DEFAULT_PROFILE
    } else {
        &s.profile
    };
    let scopes = if s.scopes.is_empty() {
        vec![
            "tickets:read".to_string(),
            "tickets:write".into(),
            "users:read".into(),
            "organizations:read".into(),
        ]
    } else {
        s.scopes.clone()
    };
    let scopes_toml = scopes
        .iter()
        .map(|sc| format!("{sc:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let email_line = match &s.email {
        Some(e) => format!("email = {e:?}\n"),
        None => {
            "# email = \"agent@example.com\"   # only for grant_type = \"api_token\"\n".to_string()
        }
    };
    format!(
        r#"# zendesk-cli (zdk) configuration
# Docs: https://github.com/osodevops/zendesk-cli
# Precedence: CLI flag > environment variable > [profiles.<active>] > [default] > built-in.
# Secrets never live in this file: tokens are kept in the credential store or the environment.

[default]
active_profile = "{profile}"
# output = "table"            # table | json | ndjson | csv | tsv | yaml | raw (auto: table on a TTY, json when piped)
# page_size = 100
# color = true
# confirm_destructive = true

[auth]
method = "{method}"          # authorization_code | client_credentials | api_token
# callback_port = 8080       # fixed loopback port for clients registered with an explicit redirect URI
auto_refresh = true
refresh_at_percent = 80      # refresh once 80% of the access-token TTL has elapsed
credential_store = "auto"    # auto | keyring | file | env | none
suppress_deprecation = false # silence the API-token end-of-life warning

[rate_limit]
strategy = "wait"            # wait | fail | burst
max_concurrency = 4
reserve_percent = 10
warn_threshold = 50
respect_retry_after = true
high_volume_addon = false

[retry]
max_attempts = 6
base_ms = 1000
max_ms = 60000
jitter = "full"
retry_on = [429, 500, 502, 503, 504]

[profiles.{profile}]
subdomain = {subdomain:?}
client_id = {client_id:?}
grant_type = "{method}"
scopes = [{scopes_toml}]
{email_line}# plan = "enterprise"
"#,
        method = s.grant_type.as_str(),
        subdomain = s.subdomain,
        client_id = s.client_id,
    )
}

const fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PRD §13.1, verbatim.
    pub(crate) const PRD_EXAMPLE: &str = r#"
[default]
active_profile = "production"
output = "table"
page_size = 100
color = true
resolve_names = false
confirm_destructive = true

[auth]
method = "authorization_code"      # authorization_code | client_credentials | api_token
callback_port = 8080
auto_refresh = true
refresh_at_percent = 80            # refresh when 80% of TTL elapsed
credential_store = "auto"          # auto | keyring | file | env
suppress_deprecation = false

[rate_limit]
strategy = "wait"                  # wait | fail | burst
max_concurrency = 4
reserve_percent = 10
warn_threshold = 50
respect_retry_after = true
high_volume_addon = false

[retry]
max_attempts = 6
base_ms = 1000
max_ms = 60000
jitter = "full"
retry_on = [429, 500, 502, 503, 504]

[cache]
enabled = true
schema_ttl_seconds = 3600
list_ttl_seconds = 60
directory = "~/.cache/zendesk-cli"
max_size_mb = 500

[sync]
state_dir = "~/.local/state/zendesk-cli"
dedupe_window = 10000
output_format = "ndjson"
min_interval_seconds = 300

[jobs]
poll_base_ms = 2000
poll_max_ms = 30000
max_concurrent = 30
persist_results = true

[rules]
manifest_dir = "./zendesk-config"
symbolic_refs = true

[audit]
enabled = true
path = "~/.local/state/zendesk-cli/audit.ndjson"

[profiles.production]
subdomain = "oso"
client_id = "zdk_production"
grant_type = "authorization_code"
scopes = ["tickets:read", "tickets:write", "users:read", "organizations:read", "hc:read"]
plan = "enterprise"

[profiles.sandbox]
subdomain = "oso1234567890"
client_id = "zdk_sandbox"
grant_type = "authorization_code"
scopes = ["read", "write"]

[profiles.ci]
subdomain = "oso"
client_id = "zdk_ci"
grant_type = "client_credentials"
scopes = ["tickets:read", "users:read", "auditlogs:read"]
"#;

    #[test]
    fn the_full_prd_example_parses_with_no_unknown_keys() {
        let cfg = ConfigFile::from_toml(PRD_EXAMPLE).unwrap();
        assert_eq!(cfg.default.active_profile.as_deref(), Some("production"));
        assert_eq!(cfg.default.output, Some(OutputFormat::Table));
        assert_eq!(cfg.default.page_size, Some(100));
        assert_eq!(cfg.auth.method, GrantKind::AuthorizationCode);
        assert_eq!(cfg.auth.callback_port, Some(8080));
        assert_eq!(cfg.auth.refresh_at_percent, 80);
        assert_eq!(cfg.auth.credential_store, StoreSelector::Auto);
        assert_eq!(cfg.rate_limit.strategy, RateLimitStrategy::Wait);
        assert_eq!(cfg.rate_limit.max_concurrency, 4);
        assert_eq!(cfg.retry.retry_on, vec![429, 500, 502, 503, 504]);
        assert_eq!(cfg.retry.jitter, Jitter::Full);
        assert_eq!(cfg.cache.directory.as_deref(), Some("~/.cache/zendesk-cli"));
        assert_eq!(cfg.sync.dedupe_window, 10_000);
        assert_eq!(cfg.jobs.max_concurrent, 30);
        assert_eq!(cfg.rules.manifest_dir, "./zendesk-config");
        assert!(cfg.audit.enabled);
        assert_eq!(cfg.profiles.len(), 3);
        let ci = &cfg.profiles["ci"];
        assert_eq!(ci.grant_type, Some(GrantKind::ClientCredentials));
        assert_eq!(
            ci.scopes,
            vec!["tickets:read", "users:read", "auditlogs:read"]
        );
        assert_eq!(
            cfg.profiles["production"].plan.as_deref(),
            Some("enterprise")
        );
        assert!(cfg.unknown_keys().is_empty(), "{:?}", cfg.unknown_keys());
    }

    #[test]
    fn empty_text_and_missing_file_are_the_defaults() {
        let cfg = ConfigFile::from_toml("").unwrap();
        assert_eq!(cfg, ConfigFile::default());
        assert_eq!(cfg.active_profile_name(), "default");
        let dir = tempfile::tempdir().unwrap();
        let missing = ConfigFile::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(missing, ConfigFile::default());
    }

    #[test]
    fn unknown_keys_are_collected_not_rejected() {
        let cfg = ConfigFile::from_toml(
            "zz_top = 1\n[default]\nzz_unknown = 50\n[rate_limit]\nstrategy = \"fail\"\nzz_extra = 2\n[profiles.x]\nsubdomain = \"a\"\nzz_unknown = true\n",
        )
        .unwrap();
        assert_eq!(
            cfg.unknown_keys(),
            vec![
                "zz_top",
                "default.zz_unknown",
                "rate_limit.zz_extra",
                "profiles.x.zz_unknown"
            ]
        );
        assert_eq!(cfg.rate_limit.strategy, RateLimitStrategy::Fail);
    }

    #[test]
    fn invalid_toml_is_a_config_error() {
        let err = ConfigFile::from_toml("[default\n").unwrap_err();
        assert_eq!(err.exit_code(), 10);
        let err = ConfigFile::from_toml("[default]\npage_size = \"lots\"\n").unwrap_err();
        assert_eq!(err.exit_code(), 10);
    }

    #[test]
    fn save_then_load_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("config.toml");
        let mut cfg = ConfigFile::default();
        cfg.default.active_profile = Some("p".into());
        cfg.profiles.insert(
            "p".into(),
            ProfileConfig {
                subdomain: Some("acme".into()),
                ..Default::default()
            },
        );
        cfg.save(&path).unwrap();
        let back = ConfigFile::load(&path).unwrap();
        assert_eq!(back, cfg);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn starter_file_parses_and_names_the_profile() {
        let text = starter_toml(&Starter {
            profile: "acme".into(),
            subdomain: "acme".into(),
            client_id: "zdk_local".into(),
            grant_type: GrantKind::ClientCredentials,
            email: None,
            scopes: vec![],
        });
        let cfg = ConfigFile::from_toml(&text).unwrap();
        assert_eq!(cfg.default.active_profile.as_deref(), Some("acme"));
        assert_eq!(cfg.auth.method, GrantKind::ClientCredentials);
        let p = &cfg.profiles["acme"];
        assert_eq!(p.subdomain.as_deref(), Some("acme"));
        assert_eq!(p.client_id.as_deref(), Some("zdk_local"));
        assert_eq!(p.grant_type, Some(GrantKind::ClientCredentials));
        assert!(!p.scopes.is_empty());
        assert!(cfg.unknown_keys().is_empty());
    }

    #[test]
    fn paths_honour_xdg_and_explicit_config() {
        let env = EnvOverrides::from_pairs([
            ("XDG_CONFIG_HOME", "/x/config"),
            ("XDG_STATE_HOME", "/x/state"),
            ("XDG_CACHE_HOME", "/x/cache"),
        ]);
        let p = Paths::resolve(&env, None).unwrap();
        assert_eq!(
            p.config_file,
            PathBuf::from("/x/config/zendesk-cli/config.toml")
        );
        assert_eq!(p.config_dir, PathBuf::from("/x/config/zendesk-cli"));
        assert_eq!(p.state_dir, PathBuf::from("/x/state/zendesk-cli"));
        assert_eq!(p.cache_dir, PathBuf::from("/x/cache/zendesk-cli"));

        let p = Paths::resolve(&env, Some(Path::new("/elsewhere/z.toml"))).unwrap();
        assert_eq!(p.config_file, PathBuf::from("/elsewhere/z.toml"));
        assert_eq!(p.config_dir, PathBuf::from("/elsewhere"));

        let env = EnvOverrides::from_pairs([
            ("XDG_CONFIG_HOME", "/x/config"),
            ("ZENDESK_CONFIG", "/env/c.toml"),
        ]);
        let p = Paths::resolve(&env, None).unwrap();
        assert_eq!(p.config_file, PathBuf::from("/env/c.toml"));
        let p = Paths::resolve(&env, Some(Path::new("/cli/c.toml"))).unwrap();
        assert_eq!(
            p.config_file,
            PathBuf::from("/cli/c.toml"),
            "--config beats ZENDESK_CONFIG"
        );
    }
}
