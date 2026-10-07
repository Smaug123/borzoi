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

/// `path` relative to the corpus `root`, `/`-separated and with Nix's case-hack
/// suffixes decoded (`manifest::relative_key`), so an entry names the same file
/// wherever and on whatever file system the corpus is checked out.
pub fn corpus_relative(root: &Path, path: &Path) -> String {
    borzoi_oracle_harness::manifest::relative_key(root, path)
}

/// The decoded components of `path` below `root`.
fn corpus_components(root: &Path, path: &Path) -> Vec<String> {
    borzoi_oracle_harness::manifest::relative_components(root, path)
}

/// Sort corpus files component-wise by their decoded [`corpus_relative`]
/// components — the order a `PathBuf` sort gives on a case-sensitive host, and
/// the one every host agrees on, so a strided sample picks the same files
/// everywhere (sorting the raw paths would not: `fsc/` sorts before `fsc-x/`
/// but `fsc~nix~case~hack~1/` after it). Component-wise, not by the joined
/// string, which would put `Dir.fs` before `Dir/a.fs`. Panics if two files
/// share a key, which would let one hide the other in a manifest.
pub fn sort_by_corpus_key(root: &Path, files: &mut [PathBuf]) {
    files.sort_by_cached_key(|p| corpus_components(root, p));
    for w in files.windows(2) {
        assert_ne!(
            corpus_relative(root, &w[0]),
            corpus_relative(root, &w[1]),
            "two corpus files share a manifest key: {} and {}",
            w[0].display(),
            w[1].display()
        );
    }
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
fn corpus_keys_decode_the_nix_case_hack() {
    let root = Path::new("/store/src");
    let key = |p: &str| corpus_relative(root, &root.join(p));
    assert_eq!(key("tests/fsc~nix~case~hack~1/a.fs"), "tests/fsc/a.fs");
    assert_eq!(key("tests/Fsc/a.fs"), "tests/Fsc/a.fs");
    // Only a well-formed suffix is decoded.
    assert_eq!(key("x~nix~case~hack~/a.fs"), "x~nix~case~hack~/a.fs");
    assert_eq!(key("x~nix~case~hack~1b/a.fs"), "x~nix~case~hack~1b/a.fs");

    // The macOS spelling sorts like the Linux one.
    // The macOS spelling sorts like the Linux one, which is `PathBuf` order.
    let linux = ["Fsc/a.fs", "fsc/a.fs", "fsc-x/a.fs", "fsc.fs"];
    let mut want: Vec<PathBuf> = linux.iter().map(|p| root.join(p)).collect();
    want.sort();
    let mut mac: Vec<PathBuf> = [
        "fsc~nix~case~hack~1/a.fs",
        "fsc-x/a.fs",
        "Fsc/a.fs",
        "fsc.fs",
    ]
    .iter()
    .map(|p| root.join(p))
    .collect();
    sort_by_corpus_key(root, &mut mac);
    let keys: Vec<String> = mac.iter().map(|p| corpus_relative(root, p)).collect();
    let want: Vec<String> = want.iter().map(|p| corpus_relative(root, p)).collect();
    assert_eq!(keys, want);
}

#[test]
#[should_panic(expected = "share a manifest key")]
fn two_files_on_one_key_are_refused() {
    let root = Path::new("/store/src");
    let mut files = vec![root.join("fsc~nix~case~hack~1/a.fs"), root.join("fsc/a.fs")];
    sort_by_corpus_key(root, &mut files);
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
