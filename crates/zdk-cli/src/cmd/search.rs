//! `zdk search` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct SearchArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: SearchArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other(
        "`zdk search` is not implemented yet".into(),
    ))
}
