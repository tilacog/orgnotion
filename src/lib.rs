//! `orgnotion`: publish an Org-roam vault as a fresh, read-only snapshot
//! in Notion.
//!
//! Library layout follows the Dependency Inversion Principle: business
//! logic ([`vault`], [`org_parser`], [`converter`], [`validate`], [`run`])
//! depends only on the traits in [`ports`]; the real-world implementations
//! live in [`adapters`] and are injected in `main`.

pub mod adapters;
pub mod cli;
pub mod converter;
pub mod notion;
pub mod org_parser;
pub mod ports;
pub mod run;
pub mod transformers;
pub mod validate;
pub mod vault;
