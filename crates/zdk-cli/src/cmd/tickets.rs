//! `zdk tickets` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct TicketsArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: TicketsArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other(
        "`zdk tickets` is not implemented yet".into(),
    ))
}
