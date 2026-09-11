//! `zdk doctor` — placeholder; implemented in a later phase.

use clap::Args;
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

#[derive(Debug, Args)]
pub struct DoctorArgs {}

#[allow(clippy::unused_async)]
pub async fn run(_args: DoctorArgs, _ctx: &AppContext) -> Result<()> {
    Err(ZdkError::Other(
        "`zdk doctor` is not implemented yet".into(),
    ))
}
