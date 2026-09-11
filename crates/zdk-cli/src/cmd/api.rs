//! `zdk api` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct ApiArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: ApiArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other("`zdk api` is not implemented yet".into()))
}
