//! `zdk comments` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct CommentsArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: CommentsArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other(
        "`zdk comments` is not implemented yet".into(),
    ))
}
