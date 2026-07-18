//! Faithful port of FCS's `QualifiedNameOfFile` derivation — the key that
//! pairs an `.fsi` signature file with the implementation it constrains
//! (`docs/fsi-signature-restriction-plan.md`, §pairing).
//!
//! FCS derives the name in `ParseAndCheckInputs.fs`:
//!
//! - **module-headed file** (`QualFileNameOfImpls` / `QualFileNameOfSpecs`):
//!   a file whose parse is exactly one `module M.N` fragment is named by the
//!   dotted module path (`"M.N"`);
//! - **anything else** (anonymous, namespace-headed, multi-fragment):
//!   `CanonicalizeFilename` — the filename's last component, extension
//!   chopped with .NET `Path` semantics, first character upper-cased with the
//!   invariant *simple* (1:1) mapping;
//! - then every input, in Compile order, is threaded through
//!   `DeduplicateParsedInputModuleName`: per raw name, the first file from
//!   each *directory* keeps (or, from the second directory on, suffixes
//!   `___<count>` onto) the name, and later files from an already-seen
//!   directory reuse that directory's deduped name. This is what pairs
//!   `d1/Pair.fsi` with `d1/Pair.fs` while keeping a same-named
//!   `d2/Extra.fs` a separate `M___2` fragment (probe X3).
//!
//! A mismatch here is not cosmetic: over-pairing suppresses an unrelated
//! implementation's exports (under-resolution — sound but lossy), while
//! under-pairing leaks signature-hidden exports (a wrong commit). The
//! FCS-differential fixtures in `crates/sema/tests/all/resolve_signatures.rs`
//! pin the port against observable pairing behaviour.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::resolve::SourceFile;

/// A file's deduplicated FCS `QualifiedNameOfFile`. Opaque; equality is the
/// pairing relation (FCS's `qnameOrder` compares the text ordinally).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualifiedNameOfFile(String);

impl QualifiedNameOfFile {
    /// The deduplicated name text.
    pub fn text(&self) -> &str {
        &self.0
    }

    /// A placeholder for an impl-only fold, where no signature ever opens a
    /// pairing and the name is never consulted.
    pub(crate) fn placeholder() -> Self {
        QualifiedNameOfFile(String::new())
    }
}

/// Derive each file's [`QualifiedNameOfFile`], in Compile order. `files` and
/// `paths` are parallel; the paths feed both the filename-derived case and
/// the per-directory deduplication, so they should be the real (absolute)
/// Compile-item paths.
///
/// # Panics
///
/// Panics when `files` and `paths` have different lengths.
pub fn qualified_names(files: &[SourceFile], paths: &[PathBuf]) -> Vec<QualifiedNameOfFile> {
    assert_eq!(
        files.len(),
        paths.len(),
        "qualified_names: files and paths must be parallel"
    );
    // FCS's `ModuleNamesDict`: raw name → the directories seen so far, each
    // with its deduped name, in first-seen order (`paths.Count` drives the
    // `___<count>` suffix).
    let mut dict: HashMap<String, Vec<(String, String)>> = HashMap::new();
    files
        .iter()
        .zip(paths)
        .map(|(file, path)| {
            let raw = raw_name(file, path);
            let dir = directory_key(path);
            let seen = dict.entry(raw.clone()).or_default();
            if let Some((_, deduped)) = seen.iter().find(|(d, _)| *d == dir) {
                return QualifiedNameOfFile(deduped.clone());
            }
            let count = seen.len() + 1;
            let deduped = if count == 1 {
                raw.clone()
            } else {
                format!("{raw}___{count}")
            };
            seen.push((dir, deduped.clone()));
            QualifiedNameOfFile(deduped)
        })
        .collect()
}

/// The pre-deduplication name: the dotted module path for a module-headed
/// file, else [`canonicalize_filename`]. (The `$fsx` script suffix is not
/// modelled — Compile items are `.fs`/`.fsi`.)
fn raw_name(file: &SourceFile, path: &Path) -> String {
    match file.module_header_path() {
        Some(segments) => segments.join("."),
        None => canonicalize_filename(path),
    }
}

/// FCS `CanonicalizeFilename`: the path's last component, extension chopped
/// (with .NET `Path` semantics — everything from the *last* dot; a lone
/// trailing dot also counts as an extension; no dot leaves the name whole),
/// first character upper-cased.
fn canonicalize_filename(path: &Path) -> String {
    let basic = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    capitalize_first_invariant(&chop_extension(&basic))
}

/// .NET-faithful extension chop of a bare filename (no directory part):
/// `FileSystemUtils.chopExtension` — `"."` maps to the empty string; a name
/// with no extension (per `hasExtensionWithValidate`: a trailing dot other
/// than `"."`/`".."` counts, otherwise a dot with at least one character
/// after it) is returned whole; otherwise everything from the last dot is
/// dropped (`".gitignore"` → `""`, `"a.b.fs"` → `"a.b"`, `"foo."` → `"foo"`).
fn chop_extension(basic: &str) -> String {
    if basic == "." {
        return String::new();
    }
    let trailing_dot = basic.ends_with('.') && basic != "..";
    let inner_dot = basic
        .rfind('.')
        .is_some_and(|i| i + '.'.len_utf8() < basic.len());
    if !(trailing_dot || inner_dot) {
        return basic.to_string();
    }
    let cut = basic.rfind('.').expect("an extension implies a dot");
    basic[..cut].to_string()
}

/// FCS `String.capitalize`: upper-case the first character with the
/// invariant **simple** (1:1) mapping — .NET's `ToUpperInvariant` maps
/// char-for-char, so a character whose only upper-case form expands (`ß`)
/// stays as it is; Rust's `char::to_uppercase` is the *full* mapping, so an
/// expanding result means "no simple mapping" and the character is kept.
fn capitalize_first_invariant(s: &str) -> String {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut upper = first.to_uppercase();
    let mapped = match (upper.next(), upper.next()) {
        (Some(one), None) => one,
        _ => first,
    };
    let mut out = String::with_capacity(s.len());
    out.push(mapped);
    out.push_str(chars.as_str());
    out
}

/// The deduplication key for a file: its directory, lexically normalised when
/// rooted (FCS calls `GetFullPathShim` on a rooted directory — `.`/`..`
/// collapse, no symlink resolution; a relative directory is used as-is).
fn directory_key(path: &Path) -> String {
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    if !dir.is_absolute() {
        return dir.to_string_lossy().into_owned();
    }
    let mut normalised = PathBuf::new();
    for component in dir.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalised.pop();
            }
            other => normalised.push(other),
        }
    }
    normalised.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_matches_dotnet_semantics() {
        assert_eq!(canonicalize_filename(Path::new("/a/foo.fs")), "Foo");
        assert_eq!(canonicalize_filename(Path::new("/a/a.b.fsi")), "A.b");
        assert_eq!(canonicalize_filename(Path::new("/a/foo")), "Foo");
        assert_eq!(canonicalize_filename(Path::new("/a/foo.")), "Foo");
        assert_eq!(canonicalize_filename(Path::new("/a/.gitignore")), "");
        assert_eq!(canonicalize_filename(Path::new("/a/ß.fs")), "ß");
    }

    #[test]
    fn directory_key_normalises_rooted_paths() {
        assert_eq!(directory_key(Path::new("/a/b/../c/f.fs")), "/a/c");
        assert_eq!(directory_key(Path::new("/a/./b/f.fs")), "/a/b");
        assert_eq!(directory_key(Path::new("rel/dir/f.fs")), "rel/dir");
    }
}
