//! Host-independent keys for items in a pinned corpus: the spelling a manifest
//! entry (see [`crate::manifest`]) gives a corpus file and a position in it.
//!
//! A manifest is only exact if every host computes the same one. The corpus is
//! a Nix store path, and the store spells some names differently by host: on a
//! case-insensitive file system (macOS) a name that collides case-insensitively
//! with a sibling's gets a `~nix~case~hack~<n>` suffix. [`corpus_relative`]
//! decodes it, and [`sort_by_corpus_key`] gives the file order every host
//! agrees on, so a strided sample picks the same files everywhere.
//!
//! Lines are 1-based; columns are 1-based and count characters
//! ([`Positions`]).

use std::path::{Path, PathBuf};

/// The suffix Nix appends to a store entry whose name collides
/// case-insensitively with a sibling's, on a case-insensitive file system
/// (macOS): the corpus has both `CompilerOptions/Fsc` and `CompilerOptions/fsc`,
/// and on macOS the second is stored as `fsc~nix~case~hack~1`.
const NIX_CASE_HACK: &str = "~nix~case~hack~";

/// One path component as the source tree spells it: [`NIX_CASE_HACK`] and its
/// counter stripped. Distinct entries of the original tree have distinct names,
/// so decoding cannot merge two files; [`sort_by_corpus_key`] asserts it anyway.
fn decode_component(name: &str) -> &str {
    match name.rfind(NIX_CASE_HACK) {
        Some(i)
            if {
                let n = &name[i + NIX_CASE_HACK.len()..];
                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
            } =>
        {
            &name[..i]
        }
        _ => name,
    }
}

/// `path` relative to the corpus `root`, `/`-separated and with Nix's case-hack
/// suffixes decoded, so an entry names the same file wherever and on whatever
/// file system the corpus is checked out.
pub fn corpus_relative(root: &Path, path: &Path) -> String {
    corpus_components(root, path).join("/")
}

/// The decoded components of `path` below `root`.
fn corpus_components(root: &Path, path: &Path) -> Vec<String> {
    let rel = path.strip_prefix(root).unwrap_or_else(|_| {
        panic!(
            "{} is not under the corpus root {}",
            path.display(),
            root.display()
        )
    });
    rel.components()
        .map(|c| decode_component(&c.as_os_str().to_string_lossy()).to_string())
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
