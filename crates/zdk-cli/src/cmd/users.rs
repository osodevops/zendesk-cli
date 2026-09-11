//! `zdk users` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct UsersArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: UsersArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other("`zdk users` is not implemented yet".into()))
}
