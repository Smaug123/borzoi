//! Hooks into an env's name table (`docs/sema-interning-plan.md`, D7), for
//! the interning-order metamorphic test: a result must not depend on the order
//! the table met its names in, so a test resolves one input against an empty
//! table and against one it filled first, in another order, and compares.

use crate::{AssemblyEnv, InferredProject, ResolvedProject};

/// `env` with a name table of its own, which interns `first`, in order,
/// before the env's own namespaces and type names.
pub fn with_fresh_names(env: &AssemblyEnv, first: &[Vec<String>]) -> AssemblyEnv {
    env.with_fresh_names(first)
}

/// `project`, resolved against `from`, with every id it holds translated to
/// `to`'s table, so it compares with a result resolved against `to`.
pub fn rekeyed(project: &ResolvedProject, from: &AssemblyEnv, to: &AssemblyEnv) -> ResolvedProject {
    project.rekey_names(from.names(), to.names())
}

/// `project`, inferred against `from`, with every type's paths translated to
/// `to`'s table, so it compares with an inference made against `to`.
pub fn rekeyed_inference(
    project: &InferredProject,
    from: &AssemblyEnv,
    to: &AssemblyEnv,
) -> InferredProject {
    project.rekey_names(from.names(), to.names())
}

/// Every path `env`'s table holds (the empty one included), as segments, in
/// the order they were interned.
pub fn interned_paths(env: &AssemblyEnv) -> Vec<Vec<String>> {
    env.names()
        .paths()
        .map(|p| {
            env.names()
                .segments(p)
                .into_iter()
                .map(str::to_string)
                .collect()
        })
        .collect()
}
