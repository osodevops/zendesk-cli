//! Per-invocation state shared by every command handler.

use std::io::{self, BufRead, Write};

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use zdk_core::config::{EnvOverrides, GlobalArgs, Settings};
use zdk_core::output::{self, OutputFormat, RenderOptions, jq};
use zdk_core::{Result, ZdkError};

/// Everything a command needs: resolved settings, the chosen output format, cancellation,
/// and the terminal facts computed once at startup. (P2 adds the lazy HTTP client; P3 the auth
/// provider and credential store.)
#[derive(Debug)]
pub struct AppContext {
    pub settings: Settings,
    /// The raw global flags, for commands that need to know what was passed explicitly.
    pub args: GlobalArgs,
    /// Environment as read once at startup (secrets stay wrapped).
    pub env: EnvOverrides,
    pub output: OutputFormat,
    /// Cancelled on ctrl-c; the HTTP client and paginator (P2) poll it between requests.
    #[allow(dead_code)]
    pub cancel: CancellationToken,
    pub stdout_is_tty: bool,
    pub stdin_is_tty: bool,
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
        Self {
            settings,
            args,
            env,
            output,
            cancel,
            stdout_is_tty,
            stdin_is_tty,
        }
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
