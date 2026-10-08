//! Finding a referenced-assembly symbol's documentation: the `.xml` beside the
//! DLL the env read, a per-file cache of parsed indexes, and the lookup itself.
//!
//! Every way a lookup can come up empty is its own [`DocLookup`] variant.
//! Hover renders them all as "no documentation" today, but they mean different
//! things — "this file documents nothing under that key" is a fact about the
//! symbol, "there is no file" or "the file is unreadable" are facts about the
//! installation — and collapsing them into an `Option` is how a "we did not
//! look" comes to be read as "provably none".

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::SystemTime;

use borzoi_sema::AssemblyEnv;

use super::file::{DocEntry, DocFile, DocFileError, EntryError};
use super::key::{DocTarget, KeyCensus, KeyError, doc_key};
use super::tree::DocElement;

/// The outcome of looking up one symbol's documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocLookup {
    /// The symbol's `<member>` element.
    Found(DocElement),
    /// The env does not record which DLL the symbol came from (an env built
    /// from entities alone), so there is no `.xml` to look beside.
    NoAssemblyPath,
    /// The symbol has no documentation-comment ID to look up.
    Key(KeyError),
    /// No `.xml` beside the DLL.
    NoXmlFile(PathBuf),
    /// The `.xml` exists but could not be read.
    Io { path: PathBuf, error: String },
    /// The `.xml` was read but is not a usable doc file.
    Unreadable { path: PathBuf, error: DocFileError },
    /// The `.xml` documents a different assembly — a stale or misplaced file.
    OtherAssembly { declared: String, expected: String },
    /// The file is a doc file for this assembly and has no entry for the key.
    NoEntry { key: String },
    /// The file carries the key more than once, with differing content.
    Ambiguous { key: String },
    /// The file has a unique entry for the key, but it could not be read.
    EntryUnreadable { key: String, error: EntryError },
}

/// Where the documentation of `dll` lives: beside it, with the same stem.
///
/// Reference packs and NuGet packages also carry *localised* copies in
/// per-culture subdirectories (`cs/`, `de/`, …, beside the DLL); the neutral
/// one is this sibling, and no other location is guessed at.
pub fn xml_path_for(dll: &Path) -> PathBuf {
    dll.with_extension("xml")
}

/// What identifies one version of a file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

/// Why [`DocFileCache::load`] has no index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    NotFound,
    Io(String),
    Doc(DocFileError),
}

/// Parsed doc files, keyed by path and validated by `(size, mtime)` on every
/// use.
///
/// Validating rather than trusting invalidation events is deliberate: a
/// project's own build can rewrite its `.xml` while leaving the DLL
/// byte-identical (a doc-comment edit changes no IL), and the watched-file
/// globs do not cover `.xml`. The stamp costs one `stat` per lookup. The
/// residual hole is a rewrite that keeps both size and mtime, the same one the
/// on-disk assembly cache accepts. A failure to parse is cached too (against
/// the same stamp), so a malformed file is not re-parsed — or re-logged — on
/// every hover.
#[derive(Debug, Default)]
pub struct DocFileCache {
    files: HashMap<PathBuf, (FileStamp, Result<Arc<DocFile>, DocFileError>)>,
}

impl DocFileCache {
    /// The index of the doc file at `path`, parsing it if this version of it
    /// has not been parsed yet.
    pub fn load(&mut self, path: &Path) -> Result<Arc<DocFile>, LoadError> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => {
                self.files.remove(path);
                return Err(LoadError::NotFound);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.files.remove(path);
                return Err(LoadError::NotFound);
            }
            Err(e) => return Err(LoadError::Io(e.to_string())),
        };
        let stamp = FileStamp {
            len: meta.len(),
            modified: meta.modified().ok(),
        };
        if let Some((cached, parsed)) = self.files.get(path)
            && *cached == stamp
        {
            return parsed.clone().map_err(LoadError::Doc);
        }
        let bytes = std::fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;
        let parsed = {
            let _span =
                tracing::info_span!("parse_xml_doc", path = %path.display(), bytes = bytes.len())
                    .entered();
            DocFile::decode(&bytes)
                .and_then(DocFile::parse)
                .map(Arc::new)
        };
        if let Err(e) = &parsed {
            tracing::warn!(path = %path.display(), error = ?e, "unusable XML documentation file");
        }
        self.files
            .insert(path.to_path_buf(), (stamp, parsed.clone()));
        parsed.map_err(LoadError::Doc)
    }

    /// Drop every cached index (memory only: correctness rests on the stamps).
    pub fn clear(&mut self) {
        self.files.clear();
    }
}

/// The [`KeyCensus`] of each assembly an env has been asked about, so the
/// one-pass census runs once per assembly per env rather than per hover.
///
/// Keyed by DLL path and validated by env *identity*: an env is rebuilt
/// whenever its inputs change (and an env whose sidecar transport failed is
/// built afresh per request), so a census describes the env it was taken
/// from and no other. The [`Weak`] both proves identity and keeps the env's
/// allocation from being reused while the entry lives, so a rebuilt env can
/// never pass for the old one.
#[derive(Debug, Default)]
pub struct KeyCensusCache {
    censuses: HashMap<PathBuf, (Weak<AssemblyEnv>, Arc<KeyCensus>)>,
}

impl KeyCensusCache {
    /// The census of the assembly at `dll` in `env`.
    pub fn census(&mut self, env: &Arc<AssemblyEnv>, dll: &Path) -> Arc<KeyCensus> {
        if let Some((taken_from, census)) = self.censuses.get(dll)
            && Weak::ptr_eq(taken_from, &Arc::downgrade(env))
        {
            return census.clone();
        }
        let census = {
            let _span = tracing::info_span!("xml_doc_key_census", dll = %dll.display()).entered();
            Arc::new(KeyCensus::of_assembly(env, dll))
        };
        self.censuses
            .insert(dll.to_path_buf(), (Arc::downgrade(env), census.clone()));
        census
    }

    /// Drop every census (memory only: correctness rests on env identity).
    pub fn clear(&mut self) {
        self.censuses.clear();
    }
}

/// Look up `target`'s documentation in the doc file beside its DLL.
pub fn lookup(
    files: &mut DocFileCache,
    censuses: &mut KeyCensusCache,
    env: &Arc<AssemblyEnv>,
    target: DocTarget,
) -> DocLookup {
    let owner = target.owner();
    let Some(dll) = env.assembly_path(owner) else {
        return DocLookup::NoAssemblyPath;
    };
    let census = censuses.census(env, dll);
    let key = match doc_key(env, &census, target) {
        Ok(key) => key,
        Err(e) => return DocLookup::Key(e),
    };
    let path = xml_path_for(dll);
    let file = match files.load(&path) {
        Ok(file) => file,
        Err(LoadError::NotFound) => return DocLookup::NoXmlFile(path),
        Err(LoadError::Io(error)) => return DocLookup::Io { path, error },
        Err(LoadError::Doc(error)) => return DocLookup::Unreadable { path, error },
    };
    let expected = &env.entity(owner).assembly.name;
    if let Some(declared) = file.assembly()
        && !declared.eq_ignore_ascii_case(expected)
    {
        return DocLookup::OtherAssembly {
            declared: declared.to_string(),
            expected: expected.clone(),
        };
    }
    match file.entry(&key) {
        None => DocLookup::NoEntry { key },
        Some(DocEntry::Ambiguous) => DocLookup::Ambiguous { key },
        Some(DocEntry::Unique(range)) => match file.member_element(range.clone()) {
            Ok(member) => DocLookup::Found(member),
            Err(error) => DocLookup::EntryUnreadable { key, error },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doc_file_is_the_neutral_sibling() {
        assert_eq!(
            xml_path_for(Path::new("/packs/ref/net10.0/System.Runtime.dll")),
            Path::new("/packs/ref/net10.0/System.Runtime.xml")
        );
    }

    /// A localised copy (`de/X.xml`) is not the documentation of `X.dll`: with
    /// no neutral sibling, there is no file.
    #[test]
    fn a_localised_copy_alone_is_no_doc_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("de")).unwrap();
        std::fs::write(tmp.path().join("de/X.xml"), "<doc/>").unwrap();
        let mut cache = DocFileCache::default();
        assert_eq!(
            cache.load(&xml_path_for(&tmp.path().join("X.dll"))).err(),
            Some(LoadError::NotFound)
        );
    }

    /// A parse failure is cached against the file's stamp — and dropped the
    /// moment the file changes.
    #[test]
    fn a_failure_is_cached_until_the_file_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("X.xml");
        std::fs::write(&path, "<doc>").unwrap();
        let mut cache = DocFileCache::default();
        assert!(matches!(cache.load(&path), Err(LoadError::Doc(_))));
        assert!(matches!(cache.load(&path), Err(LoadError::Doc(_))));
        std::fs::write(&path, "<doc><members/></doc>").unwrap();
        assert!(cache.load(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(cache.load(&path).err(), Some(LoadError::NotFound));
    }

    /// A path that stops being a file — deleted, or replaced by a directory —
    /// drops its cached index, so nothing of the old file can be served later.
    #[test]
    fn a_path_that_is_no_longer_a_file_is_evicted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("X.xml");
        let mut cache = DocFileCache::default();
        std::fs::write(&path, "<doc><members/></doc>").unwrap();
        assert!(cache.load(&path).is_ok());
        assert!(cache.files.contains_key(&path));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(cache.load(&path).err(), Some(LoadError::NotFound));
        assert!(
            !cache.files.contains_key(&path),
            "a directory evicts the entry"
        );
    }
}
