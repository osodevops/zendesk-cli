//! Per-invocation state shared by every command handler.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use zdk_core::auth::{AuthProvider, StaticBearer};
use zdk_core::config::{EnvOverrides, GlobalArgs, Settings};
use zdk_core::error::AuthFailure;
use zdk_core::http::{AuditLogObserver, RateGovernor, RetryPolicy, ZendeskClient, redact};
use zdk_core::output::{self, OutputFormat, RenderOptions, jq};
use zdk_core::store::{MemoryStore, SharedStore};
use zdk_core::{Result, ZdkError};

/// Everything a command needs: resolved settings, the chosen output format, cancellation,
/// the terminal facts computed once at startup, and — built lazily on first use — the auth
/// provider and the rate-limited HTTP client.
#[derive(Debug)]
pub struct AppContext {
    pub settings: Settings,
    /// The raw global flags, for commands that need to know what was passed explicitly.
    pub args: GlobalArgs,
    /// Environment as read once at startup (secrets stay wrapped).
    pub env: EnvOverrides,
    pub output: OutputFormat,
    /// Cancelled on ctrl-c; the HTTP client and paginator poll it between requests.
    pub cancel: CancellationToken,
    pub stdout_is_tty: bool,
    pub stdin_is_tty: bool,
    /// The invoking command line with secret flag values redacted (for the audit log).
    pub command_line: String,
    provider: OnceCell<Arc<dyn AuthProvider>>,
    client: OnceCell<Arc<ZendeskClient>>,
}

impl AppContext {
    #[must_use]
    pub fn new(
        settings: Settings,
        args: GlobalArgs,
        env: EnvOverrides,
        cancel: CancellationToken,
        stdin_is_tty: bool,
    ) -> Self {
        let output = settings.output.format;
        let stdout_is_tty = args.stdout_is_tty;
        let invocation: Vec<String> = std::env::args().skip(1).collect();
        Self {
            settings,
            args,
            env,
            output,
            cancel,
            stdout_is_tty,
            stdin_is_tty,
            command_line: redact::redact_argv(&invocation),
            provider: OnceCell::new(),
            client: OnceCell::new(),
        }
    }

    /// The auth provider for the active profile (resolved once). With `--dry-run` a missing
    /// credential is not an error: nothing is sent, so a placeholder token is used.
    pub async fn provider(&self) -> Result<Arc<dyn AuthProvider>> {
        self.provider
            .get_or_try_init(|| async { self.resolve_provider() })
            .await
            .cloned()
    }

    fn resolve_provider(&self) -> Result<Arc<dyn AuthProvider>> {
        // A token in the environment never needs the store; skip the keyring probe entirely.
        let store: SharedStore = if self.env.access_token.is_some() || self.env.api_token.is_some()
        {
            Arc::new(MemoryStore::new())
        } else {
            zdk_core::store::open(
                self.settings.credential_store,
                &self.settings.paths,
                &self.env,
            )?
        };
        match zdk_core::auth::resolve_provider(&self.settings, &self.env, store) {
            Ok(p) => Ok(p),
            Err(ZdkError::Auth(AuthFailure::NotLoggedIn { .. })) if self.settings.dry_run => {
                Ok(Arc::new(StaticBearer::new(
                    "dry-run",
                    self.settings.profile_name.clone(),
                )))
            }
            Err(e) => Err(e),
        }
    }

    /// The HTTP client (built once): base URL from the profile, the resolved auth provider,
    /// the rate governor with per-profile learned limits, retries, `--dry-run`, `--audit-log`
    /// and ctrl-c cancellation.
    pub async fn client(&self) -> Result<Arc<ZendeskClient>> {
        self.client
            .get_or_try_init(|| async {
                let provider = self.provider().await?;
                self.client_with_provider(provider)
            })
            .await
            .cloned()
    }

    /// A client for an explicit provider (e.g. `auth login` verifying a token it just minted).
    pub fn client_with_provider(
        &self,
        provider: Arc<dyn AuthProvider>,
    ) -> Result<Arc<ZendeskClient>> {
        let base = self.settings.require_base_url()?.clone();
        let governor = RateGovernor::new(&self.settings.rate_limit)
            .with_state(&self.settings.paths.state_dir, &self.settings.profile_name);
        let mut builder = ZendeskClient::builder(base)
            .auth(provider)
            .governor(Arc::new(governor))
            .retry(RetryPolicy::from_settings(
                &self.settings.retry,
                &self.settings.rate_limit,
            ))
            .timeout(self.settings.timeout)
            .dry_run(self.settings.dry_run, self.output)
            .cancel(self.cancel.clone())
            .profile(self.settings.profile_name.clone())
            .command(self.command_line.clone());
        if let Some(path) = &self.settings.audit_log {
            builder = builder.observer(Arc::new(AuditLogObserver::new(path)));
        }
        Ok(Arc::new(builder.build()?))
    }

    /// Call after a command succeeds: a refresh-token rotation that could not be persisted
    /// during the run becomes exit 10 so the user re-authenticates instead of silently
    /// losing the session.
    pub fn finish(&self) -> Result<()> {
        if let Some(provider) = self.provider.get()
            && let Some(message) = provider.rotation_error()
        {
            return Err(ZdkError::CredentialStore(message));
        }
        Ok(())
    }

    /// Render options for the active format, with an optional table preset name.
    #[must_use]
    pub fn render_options(&self, resource: Option<&str>) -> RenderOptions {
        RenderOptions {
            fields: self.settings.output.fields.clone(),
            exclude: self.settings.output.exclude.clone(),
            compact: self.settings.output.compact,
            color: self.settings.color,
            resource: resource.map(str::to_string),
            stdout_is_tty: self.stdout_is_tty,
            width: None,
        }
    }

    /// Write a result to stdout: `--fields`/`--exclude` first, then `--jq`, then the format.
    pub fn emit(&self, value: Value, resource: Option<&str>) -> Result<()> {
        let opts = self.render_options(resource);
        let value = output::project(value, &opts)?;
        if let Some(filter) = &self.settings.output.jq {
            let results = jq::apply(filter, value)?;
            return output::render_jq_results(self.output, results, &opts);
        }
        output::render(self.output, value, &opts)
    }

    /// For commands whose natural human output is a sentence rather than a table:
    /// the line in table mode, the structured value in every machine format.
    pub fn emit_or_line(&self, value: Value, line: &str) -> Result<()> {
        if self.output == OutputFormat::Table && self.settings.output.jq.is_none() {
            output::write_stdout_line(line);
            Ok(())
        } else {
            self.emit(value, None)
        }
    }

    /// Ask before a destructive action. Auto-passes with `--yes` (or `confirm_destructive = false`);
    /// refuses with a usage error when stdin is not a terminal; otherwise reads `y/N`.
    /// The prompt always names the profile and instance so a sandbox/production mix-up is visible.
    /// (First caller is `tickets delete` in P5.)
    #[allow(dead_code)]
    pub fn confirm(&self, action: &str) -> Result<bool> {
        if self.settings.yes || !self.settings.confirm_destructive {
            return Ok(true);
        }
        let target = format!(
            "profile '{}' ({})",
            self.settings.profile_name,
            self.settings.instance_label()
        );
        if !self.stdin_is_tty {
            return Err(ZdkError::Usage(format!(
                "refusing to {action} on {target} without --yes on a non-interactive terminal"
            )));
        }
        let question = format!("{action} on {target}? [y/N] ");
        let answer = read_line_from_stdin(&question)?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

/// Print a prompt on stderr (stdout is data) and read one line from stdin.
pub fn read_line_from_stdin(prompt: &str) -> Result<String> {
    {
        let stderr = io::stderr();
        let mut err = stderr.lock();
        err.write_all(prompt.as_bytes())?;
        err.flush()?;
    }
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}
