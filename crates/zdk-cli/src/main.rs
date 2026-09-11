//! `zdk` — the Zendesk CLI. Argument parsing, process plumbing and dispatch live here;
//! everything else is `zdk_core`.
//!
//! Process contract: stdout is data; errors go to stderr (a miette report in table mode, one
//! JSON line in machine modes); the exit code is `ZdkError::exit_code()` (PRD §14.3);
//! ctrl-c exits 130 with nothing on stdout; a closed stdout pipe exits 0.

mod cli;
mod cmd;
mod context;
mod help_json;

use std::fmt;
use std::io::IsTerminal;

use clap::{CommandFactory, Parser};
use miette::{Diagnostic, GraphicalReportHandler, GraphicalTheme};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use zdk_core::ZdkError;
use zdk_core::config::{ConfigFile, EnvOverrides, GlobalArgs, Paths, Settings};
use zdk_core::output::{self, OutputFormat};

use crate::cli::{Cli, Commands};
use crate::context::AppContext;

#[tokio::main]
async fn main() {
    // Handled before clap parses: the root requires a subcommand and `zdk --help-json` has none.
    if std::env::args_os().any(|arg| arg == "--help-json") {
        let dump = help_json::build(Cli::command());
        match serde_json::to_string_pretty(&dump) {
            Ok(json) => output::write_stdout_line(&json),
            Err(e) => {
                output::error_line(&format!("error: failed to render help JSON: {e}"));
                std::process::exit(1);
            }
        }
        return;
    }

    // One TLS code path on every target (aws-lc-rs needs cmake/NASM when cross-compiling).
    let _ = rustls::crypto::ring::default_provider().install_default();
    install_panic_hook();

    let cli = Cli::parse();
    let env = EnvOverrides::from_env();
    let stdout_is_tty = std::io::stdout().is_terminal();
    let stdin_is_tty = std::io::stdin().is_terminal();
    let stderr_is_tty = std::io::stderr().is_terminal();
    let globals = cli.global.to_global_args(stdout_is_tty);

    output::set_quiet(globals.quiet);
    let stderr_color = stderr_is_tty && !globals.no_color && !env.no_color;
    init_tracing(globals.verbosity, env.log.as_deref(), stderr_color);

    // Used to report errors that happen before settings are resolved.
    let fallback_format =
        OutputFormat::detect(globals.output, env.output.as_deref(), None, stdout_is_tty).unwrap_or(
            if stdout_is_tty {
                OutputFormat::Table
            } else {
                OutputFormat::Json
            },
        );

    let cancel = CancellationToken::new();
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        }
    });

    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => Err((ZdkError::Interrupted, None)),
        result = run(cli.command, globals, env, cancel.clone(), stdin_is_tty) => result,
    };

    match outcome {
        Ok(_format) => output::flush_stdout(),
        Err((err, format)) => {
            output::flush_stdout();
            if !matches!(err, ZdkError::Interrupted) {
                report_error(&err, format.unwrap_or(fallback_format), stderr_color);
            }
            std::process::exit(err.exit_code());
        }
    }
}

/// Load config, resolve settings, dispatch. The `Err` carries the format that was in force
/// (if known) so the error is rendered the way the user asked for.
async fn run(
    command: Commands,
    globals: GlobalArgs,
    env: EnvOverrides,
    cancel: CancellationToken,
    stdin_is_tty: bool,
) -> Result<OutputFormat, (ZdkError, Option<OutputFormat>)> {
    let paths = Paths::resolve(&env, globals.config.as_deref()).map_err(|e| (e, None))?;
    let config = match ConfigFile::load(&paths.config_file) {
        Ok(c) => c,
        // `config init/edit/validate/path` must still run so a broken file can be fixed.
        Err(e) if command.tolerates_broken_config() => {
            tracing::debug!("ignoring unreadable config: {e}");
            ConfigFile::default()
        }
        Err(e) => return Err((e, None)),
    };
    for key in config.unknown_keys() {
        output::warn(&format!(
            "warning: unknown config key `{key}` in {} is ignored",
            paths.config_file.display()
        ));
    }
    let settings = Settings::resolve(&globals, &env, &config, &paths).map_err(|e| (e, None))?;
    let format = settings.output.format;
    let ctx = AppContext::new(settings, globals, env, cancel, stdin_is_tty);
    cmd::dispatch(command, &ctx)
        .await
        .map(|()| format)
        .map_err(|e| (e, Some(format)))
}

/// `warn` by default; `-v` info, `-vv` debug, `-vvv` trace; `ZENDESK_LOG` wins when no `-v`.
fn init_tracing(verbosity: u8, log: Option<&str>, color: bool) {
    let filter = match verbosity {
        0 => log
            .and_then(|l| EnvFilter::try_new(l).ok())
            .unwrap_or_else(|| EnvFilter::new("warn")),
        1 => EnvFilter::new("info"),
        2 => EnvFilter::new("debug"),
        _ => EnvFilter::new("trace"),
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(color)
        .with_target(verbosity >= 2)
        .try_init();
}

/// One line on stderr, exit 1 — never a stack dump on stdout, never SIGABRT.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        let location = info
            .location()
            .map(|l| format!(" ({}:{})", l.file(), l.line()))
            .unwrap_or_default();
        output::error_line(&format!(
            "zdk hit an internal error{location}: {message} — please report it at https://github.com/osodevops/zendesk-cli/issues"
        ));
        std::process::exit(1);
    }));
}

/// Wraps `ZdkError` so miette renders `help_text()` under the report.
#[derive(Debug)]
struct Report<'a>(&'a ZdkError);

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.0, f)
    }
}

impl std::error::Error for Report<'_> {}

impl Diagnostic for Report<'_> {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.0.code()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.0
            .help_text()
            .map(|h| Box::new(h) as Box<dyn fmt::Display + 'a>)
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Error)
    }
}

/// Table mode: a miette report. Machine modes: one JSON line. Both on stderr, never stdout.
fn report_error(err: &ZdkError, format: OutputFormat, color: bool) {
    if format.is_machine() {
        let line = serde_json::json!({
            "error": {
                "code": err.error_code(),
                "message": err.to_string(),
                "help": err.help_text(),
                "exit_code": err.exit_code(),
                "request_id": err.request_id(),
            }
        });
        output::error_line(&line.to_string());
        return;
    }
    let theme = if color {
        GraphicalTheme::unicode()
    } else {
        GraphicalTheme::unicode_nocolor()
    };
    let handler = GraphicalReportHandler::new_themed(theme).with_width(100);
    let mut rendered = String::new();
    if handler.render_report(&mut rendered, &Report(err)).is_ok() {
        output::error_line(rendered.trim_end());
    } else {
        output::error_line(&format!("error: {err}"));
    }
}
