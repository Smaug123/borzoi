//! `parse_type` tests, split by grammar form. Extracted verbatim from the
//! former flat `types.rs`; each submodule owns one family of type productions
//! and keeps that form's happy-path shape tests next to its swallowed-`)`
//! recovery guards.
//!
//! The submodules reach the shared tree-rendering helpers (`debug_tree`,
//! `assert_lossless`, …) in the `tests` module via `use super::super::*`,
//! and the parser internals under test via `use super::super::super::*`.

mod anon_record;
mod applications;
mod array;
mod constrained;
mod context;
mod function_tuple;
mod hash_constraint;
mod intersection;
mod long_ident_app;
mod measure;
mod static_const;
mod typar;
mod with_null;
