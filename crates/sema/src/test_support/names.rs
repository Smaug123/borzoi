//! Hooks into an env's name and type tables (`docs/sema-interning-plan.md`,
//! D7), for the interning-order metamorphic test: a result must not depend on
//! the order the tables met their names and types in, so a test resolves or
//! infers one input against empty tables and against ones it filled first, in
//! another order, and compares.

use crate::{AssemblyEnv, InferredProject, ResolvedProject, Ty};

/// `env` with a name table of its own, which interns `first`, in order,
/// before the env's own namespaces and type names.
pub fn with_fresh_names(env: &AssemblyEnv, first: &[Vec<String>]) -> AssemblyEnv {
    env.with_fresh_names(first)
}

/// [`with_fresh_names`], with a type table of its own too, which interns
/// `first_types` (types of `types_from`) in order before anything else; and
/// every inference session against the result interns `session_seed` (also
/// `types_from`'s) first, so the ids a session hands out move as well.
pub fn with_fresh_names_and_types(
    env: &AssemblyEnv,
    first: &[Vec<String>],
    types_from: &AssemblyEnv,
    first_types: &[Ty],
    session_seed: &[Ty],
) -> AssemblyEnv {
    env.with_fresh_names_and_types(first, Some(types_from), first_types, session_seed)
}

/// `project`, resolved against `from`, with every id it holds translated to
/// `to`'s table, so it compares with a result resolved against `to`.
pub fn rekeyed(project: &ResolvedProject, from: &AssemblyEnv, to: &AssemblyEnv) -> ResolvedProject {
    project.rekey_names(from.names(), to.names())
}

/// `project`, inferred against `from`, with every type translated to `to`'s
/// tables, so it compares with an inference made against `to`.
pub fn rekeyed_inference(
    project: &InferredProject,
    from: &AssemblyEnv,
    to: &AssemblyEnv,
) -> InferredProject {
    project.rekey_names(from, to)
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

/// Every type `env`'s type table holds, in the order they were interned.
pub fn interned_types(env: &AssemblyEnv) -> Vec<Ty> {
    env.types().all().collect()
}
