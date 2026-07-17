//! `cargo run -p borzoi-astgen --example generate` — regenerate the checked-in typed-AST
//! facade modules under `crates/cst/src/syntax/generated/`. See the crate docs in
//! `lib.rs`.
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
}
