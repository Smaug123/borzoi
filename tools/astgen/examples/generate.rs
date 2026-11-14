//! `cargo run -p borzoi-astgen --example generate` — regenerate the checked-in typed-AST
//! modules under `crates/cst/src/syntax/generated/`, the unpublished facades' locks
//! under `crates/cst/facades/`, and the facade property table, and write a published
//! facade's lock and consumer fixture when it is first published (or re-baselined).
//! See the crate docs in `lib.rs`.
//!
//! This is an *example*, not a `[[bin]]`, deliberately: a second workspace binary
//! would make root `cargo run` ambiguous (the LSP is meant to be the only one).
//! Examples are excluded from default `cargo run` target inference, while borzoi-astgen
//! stays a full workspace member so `cargo test`/`clippy`/`doc` still cover it.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve repo root from borzoi-astgen manifest dir")
}

fn main() {
    let root = repo_root();
    for (rel_path, contents) in borzoi_astgen::all_outputs() {
        let path = root.join(rel_path);
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("wrote {}", path.display());
    }
    let rows = borzoi_astgen::features::feature_table();
    let union = borzoi_astgen::union_lock();
    let generation = borzoi_astgen::facade::checked_in_generation(&rows, &union)
        .unwrap_or_else(|e| panic!("{e}"));
    // A re-baseline that turns a distinct published facade into an alias, or
    // withdraws it, leaves its consumer fixture behind.
    let present = borzoi_astgen::facade::checked_in_fixtures();
    for rel_path in borzoi_astgen::facade::obsolete_fixtures(&generation, &present) {
        let path = root.join(rel_path);
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
        println!("removed {}", path.display());
    }
    // A frozen facade does not offer what the union has grown since it was
    // published until its lock takes the line; say what is on offer.
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for (module, _) in generation.opt_in {
        *counts.entry(module).or_default() += 1;
    }
    for (module, n) in counts {
        println!(
            "note: the frozen `{module}` facade may opt into {n} union line(s) (accessors, \
             derives) by appending them to its lock; `facade::opt_in` lists them"
        );
    }
}
