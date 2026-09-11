//! Shared argument parsing and command plumbing for the curated commands (plan A2):
//! `--field k=v | k:=json`, `--data @file | - | json` and `--file`/`--from-stdin` bodies,
//! `me | id | email` user references, the ticket filter flags, and the list/mutation runners
//! every resource command drives through [`crate::context::AppContext`].

pub mod body;
pub mod field;
pub mod filters;
pub mod ids;
pub mod list;
