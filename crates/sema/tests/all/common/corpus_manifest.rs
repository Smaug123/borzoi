//! The sema corpus gates' shared manifest vocabulary: where each sweep's
//! checked-in [`Manifest`] lives, how a corpus file and a position in it are
//! spelled in an entry, and the one command that regenerates it.
//!
//! Every entry starts with its item's key — the corpus-relative path, then
//! `:line:col` for an item inside the file — so a [`Manifest`] diff, which is
//! sorted, puts an item's old and new lines next to each other. Lines are
//! 1-based; columns are 1-based and count characters.

use std::path::{Path, PathBuf};

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

/// `path` relative to the corpus `root`, `/`-separated, so an entry names the
/// same file wherever the corpus is checked out.
pub fn corpus_relative(root: &Path, path: &Path) -> String {
    borzoi_oracle_harness::manifest::relative_key(root, path)
}

/// Byte offsets to `line:col` positions in one source text.
pub struct Positions<'a> {
    source: &'a str,
    /// Byte offset of the start of each line (`starts[0]` is line 1).
    starts: Vec<usize>,
}

impl<'a> Positions<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        Positions { source, starts }
    }

    /// `line:col` of byte `offset` (1-based; the column counts characters).
    pub fn at(&self, offset: usize) -> String {
        let line = self.starts.partition_point(|&s| s <= offset);
        let start = self.starts[line - 1];
        let col = self
            .source
            .get(start..offset)
            .map_or(offset - start, |s| s.chars().count());
        format!("{line}:{}", col + 1)
    }
}

#[test]
fn positions_are_one_based_and_count_characters() {
    let p = Positions::new("ab\n\u{e9}x\n");
    assert_eq!(p.at(0), "1:1");
    assert_eq!(p.at(1), "1:2");
    assert_eq!(p.at(3), "2:1");
    // `é` is two bytes and one character.
    assert_eq!(p.at(5), "2:2");
    assert_eq!(p.at(7), "3:1");
}
