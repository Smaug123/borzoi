//! The hash maps and sets sema builds.
//!
//! Resolution and inference hash short keys at a high rate (names, name
//! paths, text ranges, ids), and std's default SipHash-1-3 spends a tenth of
//! the analysis's CPU doing it. `foldhash`'s fast hasher takes resolution
//! over the compiler's own sources (`examples/phase_timings.rs`) about 10%
//! faster, as fast as the unseeded FxHash.
//!
//! It keeps the two properties of std's `RandomState` that matter here. Each
//! process draws a random seed, so a workspace cannot pick identifiers that
//! all collide and make every lookup linear; and iteration order still varies
//! from run to run, so no output can come to depend on it unnoticed.
//!
//! Every map and set in the crate uses these aliases; mixing them with std's
//! default-hashed types is a type error wherever the two meet. Build one with
//! `default()` (or `collect`, `from_iter`), not `new()`, which std defines only
//! for `RandomState`.

/// The hasher factory behind [`HashMap`] and [`HashSet`].
pub type BuildHasher = foldhash::fast::RandomState;

/// A [`std::collections::HashMap`] hashed with [`BuildHasher`].
pub type HashMap<K, V> = std::collections::HashMap<K, V, BuildHasher>;

/// A [`std::collections::HashSet`] hashed with [`BuildHasher`].
pub type HashSet<T> = std::collections::HashSet<T, BuildHasher>;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// Source files that name std's default-hashed types, each with why.
    const EXEMPT: &[(&str, &str)] = &[(
        "recovery.rs",
        "takes borzoi_cst's preprocessor symbol set, which is std-hashed, and only passes it on",
    )];

    /// A map or set of std's default-hashed type compiles anywhere it meets
    /// nothing of ours, and silently hashes with SipHash; so every source file
    /// is scanned for the std spelling.
    #[test]
    fn sema_hashes_only_through_the_aliases() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for path in rust_files(&src) {
            let rel = path.strip_prefix(&src).expect("under src");
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel == "hash.rs" || EXEMPT.iter().any(|(f, _)| *f == rel) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read a source file");
            for offset in std_hash_collection_sites(&text) {
                let line = text[..offset].lines().count().max(1);
                offenders.push(format!("{rel}:{line}"));
            }
        }
        assert!(
            offenders.is_empty(),
            "use crate::hash::{{HashMap, HashSet}}, not std's default-hashed types, at:\n{}",
            offenders.join("\n")
        );
    }

    /// Each exemption still names std's types, so none outlives its reason.
    #[test]
    fn every_exemption_is_live() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for (file, why) in EXEMPT {
            let text = std::fs::read_to_string(src.join(file)).expect("read an exempt file");
            assert!(
                !std_hash_collection_sites(&text).is_empty(),
                "{file} is exempt ({why}) but no longer names std's hash collections"
            );
        }
    }

    #[test]
    fn the_scan_sees_each_spelling() {
        for text in [
            "use std::collections::HashMap;",
            "use std::collections::{BTreeSet, HashSet};",
            "let m: std::collections::HashMap<u32, u32> = Default::default();",
            "use std::collections::hash_map::Entry;\nuse std::collections::HashSet;",
            "use std::collections::{\n    BTreeMap,\n    HashSet,\n};",
            "use std::collections::{hash_map::{Entry, OccupiedEntry}, HashMap};",
        ] {
            assert_eq!(std_hash_collection_sites(text).len(), 1, "{text}");
        }
        for text in [
            "use std::collections::BTreeSet;",
            "use std::collections::hash_map::Entry;",
            "use std::collections::{hash_map::Entry, BTreeMap};",
            "use crate::hash::{HashMap, HashSet};",
        ] {
            assert!(std_hash_collection_sites(text).is_empty(), "{text}");
        }
    }

    /// The byte offset of each `std::collections::` path in `text` that names
    /// `HashMap` or `HashSet`, directly or as a top-level item of a brace
    /// group (which may span lines and nest).
    fn std_hash_collection_sites(text: &str) -> Vec<usize> {
        const PREFIX: &str = "std::collections::";
        let names = |item: &str| {
            let item = item.trim();
            item.starts_with("HashMap") || item.starts_with("HashSet")
        };
        text.match_indices(PREFIX)
            .filter(|(i, _)| {
                let rest = &text[i + PREFIX.len()..];
                let Some(group) = rest.strip_prefix('{') else {
                    return names(rest);
                };
                let mut depth = 0usize;
                let mut item_start = 0;
                for (j, c) in group.char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' if depth == 0 => return names(&group[item_start..j]),
                        '}' => depth -= 1,
                        ',' if depth == 0 => {
                            if names(&group[item_start..j]) {
                                return true;
                            }
                            item_start = j + 1;
                        }
                        _ => {}
                    }
                }
                false
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).expect("read a source directory") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    out.push(path);
                }
            }
        }
        out
    }
}
