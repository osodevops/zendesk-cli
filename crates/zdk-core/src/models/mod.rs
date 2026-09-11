//! Serde models shared by the HTTP core, the paginator and (from P5) the curated commands.
//!
//! Resource models are deliberately `Option`-heavy and keep unknown keys, so a Zendesk field
//! added tomorrow never breaks a command shipped today.

pub mod common;

pub use common::{Count, Links, ListMeta, ZendeskErrorBody};
