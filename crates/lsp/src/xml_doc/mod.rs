//! XML documentation for hover: the sidecar `.xml` doc files of referenced
//! assemblies, located, indexed, looked up by documentation-comment ID, and
//! rendered to Markdown.
//!
//! - [`tree`] — the owned XML element tree an entry is rendered from.
//! - [`depth`] — the nesting bound every XML parse here goes through.
//! - [`file`](mod@file) — one `.xml` file, indexed by documentation-comment ID.
//! - [`key`] — a symbol's documentation-comment ID, from the assembly env.
//! - [`lookup`] — the `.xml` beside a DLL, the per-file cache, the lookup.
//! - [`markdown`] — the Markdown model and its proven-faithful printer.
//! - [`render`] — documentation trees to that model.

pub mod depth;
pub mod file;
pub mod key;
pub mod lookup;
pub mod markdown;
pub mod render;
pub mod tree;
