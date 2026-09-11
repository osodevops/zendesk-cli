//! `zdk man --to DIR` — `zdk.1` plus one page per subcommand, generated from the clap tree so
//! the manual can never drift from `--help`. Runs on a 16 MiB thread like completions.

use std::path::Path;

use clap::CommandFactory;
use zdk_core::{Result, ZdkError};

use super::completions::GENERATOR_STACK_BYTES;
use crate::context::AppContext;

pub fn run(dir: &Path, ctx: &AppContext) -> Result<()> {
    zdk_core::util::fs::ensure_dir(dir)
        .map_err(|e| ZdkError::Other(format!("cannot create {}: {e}", dir.display())))?;

    let out_dir = dir.to_path_buf();
    std::thread::Builder::new()
        .name("zdk-man".into())
        .stack_size(GENERATOR_STACK_BYTES)
        .spawn(move || clap_mangen::generate_to(crate::cli::Cli::command(), &out_dir))
        .map_err(|e| ZdkError::Other(format!("cannot start the man-page generator: {e}")))?
        .join()
        .map_err(|_| ZdkError::Other("man-page generator panicked".into()))?
        .map_err(|e| {
            ZdkError::Other(format!("cannot write man pages to {}: {e}", dir.display()))
        })?;

    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| ZdkError::Other(format!("cannot list {}: {e}", dir.display())))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("zdk") && name.ends_with(".1"))
        .collect();
    files.sort();

    let value = serde_json::json!({ "directory": dir, "files": files });
    let line = format!("Wrote {} man page(s) to {}", files.len(), dir.display());
    ctx.emit_or_line(value, &line)
}
