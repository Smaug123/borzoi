//! Hooks into an env's name table (`docs/sema-interning-plan.md`, D7), for
//! the interning-order metamorphic test: a result must not depend on the order
//! the table met its names in, so a test resolves one input against an empty
//! table and against one it filled first, in another order, and compares.

use crate::AssemblyEnv;

/// `env` with an empty name table of its own.
pub fn with_fresh_names(env: &AssemblyEnv) -> AssemblyEnv {
    env.with_fresh_names()
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

/// Intern each of `paths` into `env`'s table, in order.
pub fn intern_paths(env: &AssemblyEnv, paths: &[Vec<String>]) {
    let names = env.names();
    for path in paths {
        path.iter().fold(names.root(), |parent, segment| {
            names.intern_child(parent, names.logical(segment))
        });
    }
}
