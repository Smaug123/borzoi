//! The cst corpus gates' manifest vocabulary: where each sweep's checked-in
//! [`Manifest`] lives and the one command that regenerates it.
//!
//! Every per-file entry starts with the corpus-relative, `/`-separated path
//! ([`relative_key`]), then the file's bucket, so a [`Manifest`] diff — which is
//! sorted — puts a file's old and new lines next to each other. A sweep's
//! dominant bucket is one summary entry carrying its count, spelled with a
//! leading `(` so it sorts ahead of every path.

use std::path::{Path, PathBuf};

pub use borzoi_oracle_harness::manifest::relative_key;
use borzoi_oracle_harness::manifest::{Manifest, UPDATE_ENV, check};

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
        "{UPDATE_ENV}=1 nix develop -c cargo test -p borzoi-cst --test all {group}:: -- --ignored"
    )
}

/// Compare `actual` against the checked-in manifest of sweep `name`.
#[track_caller]
pub fn check_manifest(name: &str, actual: &Manifest, regenerate: &str) {
    check(&manifest_path(name), actual, regenerate);
}

/// The summary entry for a sweep's dominant bucket: `(<bucket>) <count> files`.
pub fn summary_entry(bucket: &str, count: usize) -> String {
    format!("({bucket}) {count} files")
}
