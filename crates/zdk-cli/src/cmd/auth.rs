//! `zdk auth` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct AuthArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: AuthArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other("`zdk auth` is not implemented yet".into()))
}
