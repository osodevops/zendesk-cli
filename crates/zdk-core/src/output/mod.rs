//! Rendering: table / json / ndjson / csv / tsv / yaml / raw, projection, jq, progress.
//!
//! Invariants (PRD §12):
//! - stdout carries data only; JSON mode emits plain arrays/objects with **no envelope**.
//! - every diagnostic goes to stderr through [`warn`] (or `tracing`), never stdout.
//! - a closed stdout pipe (`| head`) is normal termination: [`write_stdout`] exits 0.

pub mod csv;
pub mod jq;
pub mod json;
pub mod ndjson;
pub mod progress;
pub mod project;
pub mod raw;
pub mod sideload;
pub mod table;
pub mod yaml;

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, ZdkError};

/// Output formats (PRD §7.1 `-o`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Table,
    Json,
    Ndjson,
    Csv,
    Tsv,
    Yaml,
    Raw,
}

impl OutputFormat {
    /// Every format, in help order.
    pub const ALL: [Self; 7] = [
        Self::Table,
        Self::Json,
        Self::Ndjson,
        Self::Csv,
        Self::Tsv,
        Self::Yaml,
        Self::Raw,
    ];

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "table" | "human" => Some(Self::Table),
            "json" => Some(Self::Json),
            "ndjson" | "jsonl" => Some(Self::Ndjson),
            "csv" => Some(Self::Csv),
            "tsv" => Some(Self::Tsv),
            "yaml" | "yml" => Some(Self::Yaml),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Json => "json",
            Self::Ndjson => "ndjson",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Yaml => "yaml",
            Self::Raw => "raw",
        }
    }

    /// Machine formats get the one-line JSON error on stderr; `table` gets the miette report.
    #[must_use]
    pub const fn is_machine(self) -> bool {
        !matches!(self, Self::Table)
    }

    /// Precedence: flag → `ZENDESK_OUTPUT` → `[default].output` → table on a TTY, json otherwise.
    pub fn detect(
        flag: Option<Self>,
        env: Option<&str>,
        config: Option<Self>,
        stdout_is_tty: bool,
    ) -> Result<Self> {
        if let Some(f) = flag {
            return Ok(f);
        }
        if let Some(raw) = env {
            return Self::parse(raw).ok_or_else(|| {
                ZdkError::Config(format!(
                    "ZENDESK_OUTPUT: '{raw}' is not one of {}",
                    Self::ALL
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            });
        }
        if let Some(c) = config {
            return Ok(c);
        }
        Ok(if stdout_is_tty {
            Self::Table
        } else {
            Self::Json
        })
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OutputFormat {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| {
            format!(
                "'{s}' is not one of {}",
                Self::ALL
                    .iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

/// Everything a renderer needs besides the data.
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// `--fields` dot paths (applied before `exclude`).
    pub fields: Vec<String>,
    /// `--exclude` dot paths.
    pub exclude: Vec<String>,
    /// `--compact`: single-line JSON.
    pub compact: bool,
    /// ANSI colour in tables.
    pub color: bool,
    /// Table preset name (`tickets`, `users`, …); unknown/None → columns from the data.
    pub resource: Option<String>,
    /// stdout is a terminal (tables probe its width).
    pub stdout_is_tty: bool,
    /// Fixed table width (tests); `None` = terminal width or 120 when not a TTY.
    pub width: Option<u16>,
}

/// A streaming destination: `begin`, then any number of `item`s, then `end`.
///
/// `json`/`ndjson` write each item as it arrives; `table`/`csv`/`tsv`/`yaml` buffer until `end`.
pub trait OutputSink {
    fn begin(&mut self) -> Result<()>;
    fn item(&mut self, value: Value) -> Result<()>;
    fn end(&mut self) -> Result<()>;
}

/// Build the sink for a format.
#[must_use]
pub fn sink(format: OutputFormat, opts: &RenderOptions) -> Box<dyn OutputSink> {
    match format {
        OutputFormat::Table => Box::new(table::TableSink::new(opts)),
        OutputFormat::Json => Box::new(json::JsonSink::new(opts.compact)),
        OutputFormat::Ndjson => Box::new(ndjson::NdjsonSink),
        OutputFormat::Csv => Box::new(csv::CsvSink::new(b',', opts)),
        OutputFormat::Tsv => Box::new(csv::CsvSink::new(b'\t', opts)),
        OutputFormat::Yaml => Box::new(yaml::YamlSink::default()),
        OutputFormat::Raw => Box::new(raw::RawSink),
    }
}

/// Apply `--fields` / `--exclude` (each element of an array is projected).
pub fn project(value: Value, opts: &RenderOptions) -> Result<Value> {
    let mut v = value;
    if !opts.fields.is_empty() {
        let paths = project::parse_paths(&opts.fields)?;
        v = project::select(&v, &paths);
    }
    if !opts.exclude.is_empty() {
        let paths = project::parse_paths(&opts.exclude)?;
        v = project::exclude(&v, &paths);
    }
    Ok(v)
}

/// Render a whole value: arrays stream through the sink; anything else is a single record.
pub fn render(format: OutputFormat, value: Value, opts: &RenderOptions) -> Result<()> {
    match value {
        Value::Array(items) => {
            let mut s = sink(format, opts);
            s.begin()?;
            for item in items {
                s.item(item)?;
            }
            s.end()
        }
        other => render_single(format, &other, opts),
    }
}

/// Render one record (an object in JSON mode; a two-column table in table mode).
pub fn render_single(format: OutputFormat, value: &Value, opts: &RenderOptions) -> Result<()> {
    match format {
        OutputFormat::Table => table::render_single(value, opts),
        OutputFormat::Json => {
            json::write_value(value, opts.compact);
            Ok(())
        }
        OutputFormat::Ndjson | OutputFormat::Raw => {
            json::write_value(value, true);
            Ok(())
        }
        OutputFormat::Csv => csv::render_single(value, b',', opts),
        OutputFormat::Tsv => csv::render_single(value, b'\t', opts),
        OutputFormat::Yaml => yaml::render_single(value),
    }
}

/// Render the outputs of a `--jq` filter the way `jq` would: one result after another.
/// Scalars in table/raw mode print bare (so `--jq '.[].id'` gives one id per line).
pub fn render_jq_results(
    format: OutputFormat,
    results: Vec<Value>,
    opts: &RenderOptions,
) -> Result<()> {
    for value in results {
        match (&value, format) {
            (Value::String(s), OutputFormat::Table | OutputFormat::Raw) => write_stdout_line(s),
            (
                Value::Null | Value::Bool(_) | Value::Number(_),
                OutputFormat::Table | OutputFormat::Raw,
            ) => {
                write_stdout_line(&value.to_string());
            }
            (Value::Array(_), _) if format != OutputFormat::Json => render(format, value, opts)?,
            _ => render_single(format, &value, opts)?,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// stdout / stderr plumbing
// ---------------------------------------------------------------------------------------------

static QUIET: AtomicBool = AtomicBool::new(false);

/// Set once at startup from `--quiet`; silences [`warn`] and progress.
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

#[must_use]
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Write bytes to stdout. A closed pipe is normal early termination (exit 0).
pub fn write_stdout(bytes: &[u8]) {
    handle_stdout_result(io::stdout().lock().write_all(bytes));
}

/// Write a line to stdout (see [`write_stdout`]).
pub fn write_stdout_line(line: &str) {
    let result = {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        out.write_all(line.as_bytes())
            .and_then(|()| out.write_all(b"\n"))
    };
    handle_stdout_result(result);
}

/// Flush stdout; a closed pipe exits 0 like the writers do.
pub fn flush_stdout() {
    handle_stdout_result(io::stdout().lock().flush());
}

fn handle_stdout_result(result: io::Result<()>) {
    if let Err(err) = result {
        if err.kind() == io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        error_line(&format!("error: failed to write to stdout: {err}"));
        std::process::exit(1);
    }
}

/// A warning/notice on stderr, suppressed by `--quiet`. The only sanctioned non-tracing stderr path.
pub fn warn(message: &str) {
    if !is_quiet() {
        error_line(message);
    }
}

/// An error line on stderr — never suppressed (errors must always be visible).
pub fn error_line(message: &str) {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    let _ = err
        .write_all(message.as_bytes())
        .and_then(|()| err.write_all(b"\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_follows_the_documented_precedence() {
        assert_eq!(
            OutputFormat::detect(None, None, None, true).unwrap(),
            OutputFormat::Table
        );
        assert_eq!(
            OutputFormat::detect(None, None, None, false).unwrap(),
            OutputFormat::Json
        );
        assert_eq!(
            OutputFormat::detect(None, None, Some(OutputFormat::Yaml), true).unwrap(),
            OutputFormat::Yaml
        );
        assert_eq!(
            OutputFormat::detect(None, Some("csv"), Some(OutputFormat::Yaml), true).unwrap(),
            OutputFormat::Csv
        );
        assert_eq!(
            OutputFormat::detect(
                Some(OutputFormat::Ndjson),
                Some("csv"),
                Some(OutputFormat::Yaml),
                true
            )
            .unwrap(),
            OutputFormat::Ndjson
        );
        let err = OutputFormat::detect(None, Some("xml"), None, true).unwrap_err();
        assert_eq!(err.exit_code(), 10);
        assert!(err.to_string().contains("ZENDESK_OUTPUT"));
    }

    #[test]
    fn parse_accepts_aliases_and_round_trips() {
        for f in OutputFormat::ALL {
            assert_eq!(OutputFormat::parse(f.as_str()), Some(f));
            assert_eq!(f.as_str().parse::<OutputFormat>().unwrap(), f);
        }
        assert_eq!(OutputFormat::parse("JSONL"), Some(OutputFormat::Ndjson));
        assert_eq!(OutputFormat::parse("yml"), Some(OutputFormat::Yaml));
        assert!(OutputFormat::parse("xml").is_none());
        assert!(!OutputFormat::Table.is_machine());
        assert!(OutputFormat::Raw.is_machine());
    }

    #[test]
    fn project_applies_fields_then_exclude() {
        let v =
            serde_json::json!([{"id": 1, "a": {"b": 2, "c": 3}}, {"id": 2, "a": {"b": 4, "c": 5}}]);
        let opts = RenderOptions {
            fields: vec!["id".into(), "a".into()],
            exclude: vec!["a.c".into()],
            ..Default::default()
        };
        let out = project(v, &opts).unwrap();
        assert_eq!(
            out,
            serde_json::json!([{"id": 1, "a": {"b": 2}}, {"id": 2, "a": {"b": 4}}])
        );
    }
}
