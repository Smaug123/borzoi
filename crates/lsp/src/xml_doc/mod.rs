//! XML documentation: doc files indexed by documentation-comment ID, and
//! documentation trees rendered to Markdown.
//!
//! - [`tree`] — the owned XML element tree an entry is rendered from.
//! - [`depth`] — the nesting bound every XML parse here goes through.
//! - [`file`](mod@file) — one `.xml` file, indexed by documentation-comment ID.
//! - [`markdown`] — the Markdown model and its proven-faithful printer.
//! - [`render`] — documentation trees to that model.

pub mod depth;
pub mod file;
pub mod markdown;
pub mod render;
pub mod tree;
