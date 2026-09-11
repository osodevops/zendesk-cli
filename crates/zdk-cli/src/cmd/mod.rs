//! Command handlers. Each module owns one top-level command; `dispatch` is the only match.

pub mod api;
pub mod auth;
pub mod comments;
pub mod completions;
pub mod config;
pub mod doctor;
pub mod man;
pub mod orgs;
pub mod search;
pub mod tickets;
pub mod users;
pub mod version;

use zdk_core::Result;

use crate::cli::Commands;
use crate::context::AppContext;

// Handlers from P2 on await the HTTP client; keeping the signature async now keeps `main` stable.
#[allow(clippy::unused_async)]
pub async fn dispatch(command: Commands, ctx: &AppContext) -> Result<()> {
    match command {
        Commands::Config(args) => config::run(args, ctx),
        Commands::Completions { shell } => completions::run(shell),
        Commands::Man { to } => man::run(&to, ctx),
        Commands::Version => version::run(ctx),
        Commands::Api(args) => api::run(args, ctx).await,
        Commands::Auth(args) => auth::run(args, ctx).await,
        Commands::Doctor(args) => doctor::run(args, ctx).await,
        Commands::Tickets(args) => tickets::run(args, ctx).await,
        Commands::Comments(args) => comments::run(args, ctx).await,
        Commands::Users(args) => users::run(args, ctx).await,
        Commands::Orgs(args) => orgs::run(args, ctx).await,
        Commands::Search(args) => search::run(args, ctx).await,
    }
}
