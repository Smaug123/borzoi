//! XML documentation for hover: the sidecar `.xml` doc files of referenced
//! assemblies, located, indexed, looked up by documentation-comment ID, and
//! rendered to Markdown; and the `///` comments of project-local declarations,
//! rendered the same way.
//!
//! - [`tree`] — the owned XML element tree an entry is rendered from.
//! - [`depth`] — the nesting bound every XML parse here goes through.
//! - [`file`](mod@file) — one `.xml` file, indexed by documentation-comment ID.
//! - [`key`] — a symbol's documentation-comment ID, from the assembly env.
//! - [`lookup`] — the `.xml` beside a DLL, the per-file cache, the lookup.
//! - [`markdown`] — the Markdown model and its proven-faithful printer.
//! - [`render`] — documentation trees to that model.
//! - [`source`] — the `///` documentation FCS attaches to a declaration in F#
//!   source, for a project-local symbol.

pub mod depth;
pub mod file;
pub mod key;
pub mod lookup;
pub mod markdown;
mod pairing;
pub mod render;
pub mod source;
pub mod tree;
