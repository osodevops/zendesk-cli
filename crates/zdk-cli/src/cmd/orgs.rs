//! `zdk orgs` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct OrgsArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: OrgsArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other("`zdk orgs` is not implemented yet".into()))
}
