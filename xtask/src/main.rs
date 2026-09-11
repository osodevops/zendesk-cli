//! Build tooling for zendesk-cli. Run with `cargo xtask <command>`.
//!
//! - `codegen [--check]` — regenerate `crates/zdk-core/src/api/generated/` from `specs/`.
//! - `spec-refresh [--spec name]` — download the upstream documents and update `SPEC_VERSIONS.toml`.
//! - `spec-diff [--json] [--summary]` — compare upstream with the committed snapshots.

#![allow(clippy::print_stdout, clippy::print_stderr)]

mod diff;
mod openapi;
mod specs;

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use similar::TextDiff;

const REGISTRY_REL: &str = "crates/zdk-core/src/api/generated/registry.rs";
const DETAIL_REL: &str = "crates/zdk-core/src/api/generated/detail.json.gz";

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "zendesk-cli build tooling")]
struct Xtask {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Regenerate crates/zdk-core/src/api/generated from specs/
    Codegen {
        /// Fail (exit 1) if the committed output differs instead of writing it
        #[arg(long)]
        check: bool,
    },
    /// Download the upstream OpenAPI documents listed in specs/SPEC_VERSIONS.toml
    SpecRefresh {
        /// Only refresh one spec (support | help_center | voice)
        #[arg(long)]
        spec: Option<String>,
    },
    /// Compare upstream OpenAPI documents with the committed snapshots; exit 1 on drift
    SpecDiff {
        /// Print a machine-readable JSON report
        #[arg(long)]
        json: bool,
        /// Print a Markdown summary suitable as a pull-request body
        #[arg(long)]
        summary: bool,
    },
}

fn main() -> Result<ExitCode> {
    let xtask = Xtask::parse();
    let root = specs::workspace_root();
    match xtask.command {
        Command::Codegen { check } => codegen(&root, check),
        Command::SpecRefresh { spec } => {
            specs::refresh(&root, spec.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::SpecDiff { json, summary } => spec_diff(&root, json, summary),
    }
}

fn codegen(root: &Path, check: bool) -> Result<ExitCode> {
    let generated = openapi::generate(root)?;
    for w in &generated.warnings {
        eprintln!("warning: {w}");
    }
    let registry_path = root.join(REGISTRY_REL);
    let detail_path = root.join(DETAIL_REL);

    if check {
        let out_dir = root.join("target/xtask/codegen-check");
        std::fs::create_dir_all(&out_dir)?;
        std::fs::write(out_dir.join("registry.rs"), &generated.registry)?;
        std::fs::write(out_dir.join("detail.json.gz"), &generated.detail)?;

        let committed_registry = std::fs::read_to_string(&registry_path).unwrap_or_default();
        let committed_detail = std::fs::read(&detail_path).unwrap_or_default();
        let mut stale = false;
        if committed_registry != generated.registry {
            stale = true;
            println!("{REGISTRY_REL} is stale:");
            let diff =
                TextDiff::from_lines(committed_registry.as_str(), generated.registry.as_str());
            print!(
                "{}",
                diff.unified_diff()
                    .context_radius(2)
                    .header(REGISTRY_REL, "generated")
            );
        }
        if committed_detail != generated.detail {
            stale = true;
            println!(
                "{DETAIL_REL} is stale ({} bytes committed, {} bytes generated):",
                committed_detail.len(),
                generated.detail.len()
            );
            let old = inflate_pretty(&committed_detail);
            let new = inflate_pretty(&generated.detail);
            let diff = TextDiff::from_lines(old.as_str(), new.as_str());
            let text = diff
                .unified_diff()
                .context_radius(2)
                .header(DETAIL_REL, "generated")
                .to_string();
            let mut lines = text.lines();
            for line in lines.by_ref().take(200) {
                println!("{line}");
            }
            let rest = lines.count();
            if rest > 0 {
                println!("... ({rest} more diff lines; regenerate with `cargo xtask codegen`)");
            }
        }
        if stale {
            eprintln!(
                "generated files are out of date — run `cargo xtask codegen` and commit the result"
            );
            return Ok(ExitCode::from(1));
        }
        println!(
            "generated files are up to date ({} operations)",
            generated.stats.per_spec.values().sum::<usize>()
        );
        return Ok(ExitCode::SUCCESS);
    }

    std::fs::create_dir_all(
        registry_path
            .parent()
            .context("registry path has no parent")?,
    )?;
    std::fs::write(&registry_path, &generated.registry)
        .with_context(|| format!("writing {}", registry_path.display()))?;
    std::fs::write(&detail_path, &generated.detail)
        .with_context(|| format!("writing {}", detail_path.display()))?;
    println!(
        "wrote {REGISTRY_REL} ({} lines) and {DETAIL_REL} ({} bytes)",
        generated.registry.lines().count(),
        generated.detail.len()
    );
    print!("{}", generated.stats);
    Ok(ExitCode::SUCCESS)
}

fn inflate_pretty(gz: &[u8]) -> String {
    use std::io::Read as _;
    if gz.is_empty() {
        return String::new();
    }
    let mut json = String::new();
    if flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .is_err()
    {
        return String::from("<not a gzip stream>\n");
    }
    match serde_json::from_str::<serde_json::Value>(&json) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(json),
        Err(_) => json,
    }
}

fn spec_diff(root: &Path, json: bool, summary: bool) -> Result<ExitCode> {
    let report = diff::run(root)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    if summary {
        print!("{}", diff::render_summary(&report));
    }
    if !json && !summary {
        print!("{}", diff::render_text(&report));
    }
    Ok(if report.drift {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
