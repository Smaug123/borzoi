//! The sema corpus gates' shared manifest vocabulary: where each sweep's
//! checked-in [`Manifest`] lives and the one command that regenerates it.
//! How a corpus file and a position in it are spelled is
//! [`borzoi_oracle_harness::corpus_key`], shared with the other crates' gates.
//!
//! Every entry starts with its item's key — the corpus-relative path, then
//! `:line:col` for an item inside the file — so a [`Manifest`] diff, which is
//! sorted, puts an item's old and new lines next to each other.

use std::path::{Path, PathBuf};

pub use borzoi_oracle_harness::corpus_key::{Positions, corpus_relative, sort_by_corpus_key};
use borzoi_oracle_harness::manifest::{Manifest, check};

/// The checked-in manifest for the sweep `name`.
pub fn manifest_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/manifests")
        .join(format!("{name}.txt"))
}

/// The command that regenerates the manifest of the `#[ignore]`d sweep in case
/// group `group`.
pub fn regenerate_ignored(group: &str) -> String {
    format!(
        "{}=1 nix develop -c cargo test -p borzoi-sema --test all {group}:: -- --ignored",
        borzoi_oracle_harness::manifest::UPDATE_ENV
    )
}

/// The command that regenerates the manifest of the ordinary (not ignored)
/// test selected by `filter`.
pub fn regenerate(filter: &str) -> String {
    format!(
        "{}=1 nix develop -c cargo test -p borzoi-sema --test all {filter}",
        borzoi_oracle_harness::manifest::UPDATE_ENV
    )
}

/// Compare `actual` against the checked-in manifest of sweep `name`.
#[track_caller]
pub fn check_manifest(name: &str, actual: &Manifest, regenerate: &str) {
    check(&manifest_path(name), actual, regenerate);
}
