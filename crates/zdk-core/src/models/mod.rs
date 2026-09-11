//! Serde models shared by the HTTP core, the paginator and (from P5) the curated commands.
//!
//! Resource models are deliberately `Option`-heavy and keep unknown keys, so a Zendesk field
//! added tomorrow never breaks a command shipped today.

pub mod comment;
pub mod common;
pub mod organization;
pub mod search;
pub mod ticket;
pub mod user;

pub use comment::Comment;
pub use common::{Count, Links, ListMeta, ZendeskErrorBody};
pub use organization::{Organization, OrganizationEnvelope};
pub use search::{SearchPage, SearchResult};
pub use ticket::{CustomField, SatisfactionRating, Ticket, TicketEnvelope, Via};
pub use user::{User, UserEnvelope};
