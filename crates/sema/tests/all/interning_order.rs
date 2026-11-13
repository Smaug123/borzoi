//! Interning-order invariance (`docs/sema-interning-plan.md`, D7): what
//! resolution answers must not depend on the order the env's name table met
//! its names in.
//!
//! Ids are handed out in interning order, which differs between runs (test
//! threads share an env; an incremental fold re-resolves some files and not
//! others). `Name` and `PathId` have no `Ord`, which keeps the obvious misuse
//! from compiling, but an answer can still depend on an id's value through a
//! hash order or an index. So each case resolves its input twice, against two
//! copies of one env: once with an empty table, and once with a table already
//! holding every path the first run interned, plus decoys, interned first in a
//! shuffled order. The results must be equal.

use std::path::{Path, PathBuf};

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::test_support::names::{interned_paths, rekeyed, with_fresh_names};
use borzoi_sema::{AssemblyEnv, ResolvedProject, resolve_project, resolve_project_files};
use proptest::prelude::*;

use crate::common::scope_gen;

/// A deterministic shuffle (xorshift-driven Fisher–Yates).
fn shuffle<T>(items: &mut [T], mut seed: u64) {
    seed |= 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

fn resolve(sources: &[String], env: &AssemblyEnv) -> ResolvedProject {
    let files: Vec<ImplFile> = sources
        .iter()
        .map(|s| ImplFile::cast(parse(s).root).expect("impl file"))
        .collect();
    resolve_project(&files, env)
}

/// Resolve `sources` as one project against an empty table and against one
/// filled first in an order `seed` picks, and compare. Returns how many paths
/// the first run interned, so a caller can tell the comparison was not empty.
fn assert_order_invariant(sources: &[String], base: &AssemblyEnv, seed: u64) -> usize {
    assert_fold_order_invariant(|env| resolve(sources, env), base, seed)
}

/// [`assert_order_invariant`] for any fold of one input: `fold` resolves it
/// against the env it is given.
fn assert_fold_order_invariant(
    fold: impl Fn(&AssemblyEnv) -> ResolvedProject,
    base: &AssemblyEnv,
    seed: u64,
) -> usize {
    let first_env = with_fresh_names(base, &[]);
    let first = fold(&first_env);
    let mut paths = interned_paths(&first_env);
    let walked = paths.len();
    // Decoys: each path reversed, which the walk never entered, so a map
    // keyed by ids from the second table sees ids for paths it holds nothing
    // under.
    let decoys: Vec<Vec<String>> = paths
        .iter()
        .map(|p| p.iter().rev().cloned().collect())
        .collect();
    paths.extend(decoys);
    shuffle(&mut paths, seed);
    let second_env = with_fresh_names(base, &paths);
    let second = fold(&second_env);
    // A result holds ids of the table it was computed against, so the first
    // is compared in the second's ids, translated by spelling.
    let first = rekeyed(&first, &first_env, &second_env);
    assert!(
        first == second,
        "resolution moved with interning order (seed {seed}):\nfirst: {first:#?}\nsecond: {second:#?}"
    );
    walked
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Generated programs: modules nested two deep, opens of sibling, nested
    /// and `[<AutoOpen>]` modules, and types, as a two-file project.
    #[test]
    fn generated_programs_resolve_the_same_in_any_interning_order(
        a in any::<u32>(),
        b in any::<u32>(),
        seed in any::<u64>(),
    ) {
        let sources = vec![
            scope_gen::generate_seed(a).src,
            scope_gen::generate_seed(b).src,
        ];
        let walked = assert_order_invariant(&sources, &AssemblyEnv::default(), seed);
        prop_assert!(walked > 1, "the walk interned no container path");
    }
}

fn collect_fs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_fs(&path, out);
        } else if path.extension().is_some_and(|e| e == "fs") {
            out.push(path);
        }
    }
}

/// A sample of the corpus's `src/` tree, as one project in path order,
/// against the BCL.
#[test]
fn corpus_sample_resolves_the_same_in_any_interning_order() {
    let Some(root) = std::env::var_os("BORZOI_CORPUS") else {
        eprintln!("BORZOI_CORPUS unset; skipping the interning-order corpus sample");
        return;
    };
    let mut files = Vec::new();
    collect_fs(&Path::new(&root).join("src"), &mut files);
    files.sort();
    // Every 23rd file under 40 kB: a few hundred kilobytes, spread across
    // the tree.
    let sources: Vec<String> = files
        .iter()
        .step_by(23)
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter(|s| s.len() < 40_000)
        .collect();
    assert!(
        sources.len() >= 5,
        "corpus sample too small: {}",
        sources.len()
    );
    for seed in [1, 0x9e37_79b9_7f4a_7c15] {
        let walked = assert_order_invariant(&sources, crate::common::full_bcl_env(), seed);
        assert!(walked > 10, "the walk interned {walked} paths");
    }
}

/// A project whose signature files materialise exports at their
/// implementations' slots: what a signature stashes is re-keyed with the
/// rest of the result.
#[test]
fn signature_exports_resolve_the_same_in_any_interning_order() {
    let files = crate::resolve_signatures::project(crate::exported_names::FIXTURE);
    for seed in [1, 7, 0x9e37_79b9_7f4a_7c15] {
        let walked = assert_fold_order_invariant(
            |env| resolve_project_files(&files, env),
            &AssemblyEnv::default(),
            seed,
        );
        assert!(walked > 10, "the walk interned {walked} paths");
    }
}
