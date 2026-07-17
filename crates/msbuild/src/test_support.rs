//! Test-only window onto the private `Condition` evaluator.
//!
//! Gated behind the `test-support` feature (see `Cargo.toml`) so it never
//! widens the crate's runtime public surface. It exists so the differential
//! integration tests (`tests/condition_diff.rs`, `tests/condition_properties.rs`)
//! can fuzz raw condition strings against the *real* MSBuild evaluator via
//! `tools/msbuild-condition-oracle` and assert the "certain-implies-exact"
//! property: whenever our evaluator commits to [`Outcome::True`] or
//! [`Outcome::False`], MSBuild agrees with that exact boolean; when we return
//! [`Outcome::Unsupported`] we make no claim (our conservative fail-safe).
//!
//! Only the pieces the differential needs are re-exported. `Exists(..)` is
//! deliberately out of scope: [`evaluate`] treats a reached `Exists` as
//! [`Outcome::Unsupported`] (no filesystem oracle), so the differential never
//! commits on it — the filesystem predicate is covered by the in-module unit
//! tests instead.

pub use crate::condition::{Eval, Outcome, evaluate};
pub use crate::properties::escaping::{escape, unescape};
pub use crate::properties::path_fixup::worlds as path_fixup_worlds;
pub use crate::properties::{Issue, PropertyMap};

/// Expand a property body and take the result to its **point of use** — i.e.
/// the string MSBuild's `Project.GetPropertyValue` hands back, which is
/// `UnescapeAll` of the value it stored (`ProjectProperty.cs:89`). That is the
/// quantity the `expand` oracle returns, so it is the one the differential
/// compares.
///
/// The escaped domain itself stays crate-private: a differential has no reason
/// to hold a half-evaluated value, and keeping `Escaped` unexported means no
/// consumer outside the evaluator can pick the wrong domain.
pub fn substitute(input: &str, props: &PropertyMap) -> (String, Vec<Issue>) {
    let (value, issues) = crate::properties::substitute(input, props);
    (value.unescape(), issues)
}
