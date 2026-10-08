//! The `///` documentation hover shows for a project-local symbol, diffed
//! against what FCS attaches to the same symbol (`fcs-dump xmldoc-batch`).
//!
//! - [`harness`] — both sides and the certain-implies-exact comparison.
//! - [`generated`] — the generated case space (a table and a property).

mod corpus;
mod examples;
mod generated;
mod harness;
mod signature;
