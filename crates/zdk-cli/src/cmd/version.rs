//! `zdk version` — `zdk <ver>` for humans; version, target and the committed spec snapshots
//! for machines.

use zdk_core::Result;
use zdk_core::output::write_stdout_line;

use crate::context::AppContext;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run(ctx: &AppContext) -> Result<()> {
    if ctx.output == zdk_core::output::OutputFormat::Table && ctx.settings.output.jq.is_none() {
        write_stdout_line(&format!("zdk {VERSION}"));
        return Ok(());
    }
    let specs: Vec<serde_json::Value> = zdk_core::api::generated::SPEC_VERSIONS
        .iter()
        .map(|s| {
            serde_json::json!({
                "spec": s.spec.as_str(),
                "url": s.url,
                "openapi": s.openapi,
                "info_version": s.info_version,
                "sha256": s.sha256,
                "fetched_at": s.fetched_at,
                "paths": s.paths,
                "operations": s.operations,
            })
        })
        .collect();
    ctx.emit(
        serde_json::json!({
            "version": VERSION,
            "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            "spec_versions": specs,
        }),
        None,
    )
}
