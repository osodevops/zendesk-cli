//! `zdk-core` — everything behind the `zdk` command line except argv parsing.
//!
//! Layers (see `docs/zendesk-cli-prd.md` §5):
//! - [`config`]: config file, environment overrides and the resolved [`config::Settings`].
//! - [`auth`] + [`store`]: OAuth 2.0 (authorization code + PKCE, client credentials),
//!   legacy API tokens, and where credentials are kept.
//! - [`http`]: the rate-limited, retrying Zendesk client.
//! - [`pagination`]: the cursor / offset / link-header dialects.
//! - [`api`]: the generated operation registry and the curated typed wrappers.
//! - [`output`]: table / json / ndjson / csv / yaml / raw rendering and projection.

pub mod api;
pub mod auth;
pub mod config;
pub mod error;
pub mod http;
pub mod models;
pub mod output;
pub mod pagination;
pub mod store;
pub mod util;

pub use error::{Result, ZdkError};
