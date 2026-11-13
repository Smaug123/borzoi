//! Every file-system observation the server makes
//! (`docs/incremental-lsp-plan.md`, D3).
//!
//! - **Source reads** go through [`Sources`]: the client's open buffers, and
//!   the text of every other source, read from disk and **counted** per
//!   **generation**, the unit of work the dispatch loop does for one message
//!   (or one step of a `workspace/diagnostic` sweep). The loop begins each
//!   generation ([`Sources::begin_generation`]) and ends it
//!   ([`Sources::end_generation`]), which emits the generation's counts as
//!   the [`GENERATION_EVENT`] `tracing` event. So "a comment keystroke opens
//!   no file" can be asserted by a test on any platform, and `edit_counts`
//!   reports the same counts.
//! - **Every other observation** is a method of [`Unobserved`], whose variant
//!   names the reader it belongs to and the plan stage that will route it.
//!   Those are not counted.
//!
//! The crate's `clippy.toml` bans the `std::fs` and `std::path::Path` calls
//! that observe the file system, so a new reader that skips this module is a
//! lint error. The calls here are the only ones allowed outside test code,
//! which reaches them through `host_fs` (and its integration-test twin).
//!
//! The counts are kept in a [`Cell`] so that a read through `&Sources`, which
//! is how the fold borrows the buffers beside the rest of the server state,
//! can count. A `Sources` is owned by one thread, the dispatch loop's.

use std::cell::Cell;
use std::collections::HashMap;
use std::collections::hash_map;
use std::path::Path;

use lsp_types::Uri;

use crate::paths::{lexically_normalize, paths_equal};
use crate::uri::UriExt as _;

/// The name of the event [`Sources::end_generation`] emits, whose fields are
/// the generation's number (`generation`) and its [`ReadCounts`] (`opens`).
pub const GENERATION_EVENT: &str = "observed.generation";

/// What one generation observed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadCounts {
    /// Files opened to read them, whether or not the open succeeded.
    pub opens: u64,
}

/// The client's open buffers, and the counted reads of everything else.
#[derive(Debug, Default)]
pub struct Sources {
    /// The client's text for each open document, keyed by the URI it was
    /// opened under. Whatever it holds is what the client's positions count,
    /// so it is never touched.
    buffers: HashMap<Uri, String>,
    /// The current generation's number. Generation 0 is the work done before
    /// the dispatch loop begins one.
    generation: u64,
    /// What the current generation has observed so far.
    counts: Cell<ReadCounts>,
}

impl FromIterator<(Uri, String)> for Sources {
    /// Sources whose open buffers are `buffers`.
    fn from_iter<I: IntoIterator<Item = (Uri, String)>>(buffers: I) -> Self {
        Sources {
            buffers: buffers.into_iter().collect(),
            ..Sources::default()
        }
    }
}

impl Sources {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the client's text for `uri`, returning the text it replaces.
    pub fn insert(&mut self, uri: Uri, text: String) -> Option<String> {
        self.buffers.insert(uri, text)
    }

    /// Forget the client's text for `uri`, returning it.
    pub fn remove(&mut self, uri: &Uri) -> Option<String> {
        self.buffers.remove(uri)
    }

    /// The client's text for `uri`, if it has the document open under exactly
    /// that URI.
    pub fn get(&self, uri: &Uri) -> Option<&String> {
        self.buffers.get(uri)
    }

    pub fn contains_key(&self, uri: &Uri) -> bool {
        self.buffers.contains_key(uri)
    }

    /// The open documents and their text.
    pub fn iter(&self) -> hash_map::Iter<'_, Uri, String> {
        self.buffers.iter()
    }

    /// The URIs of the open documents.
    pub fn keys(&self) -> hash_map::Keys<'_, Uri, String> {
        self.buffers.keys()
    }

    /// The text of the file at `path`: the open buffer whose path is
    /// path-equal to it, else the file on disk, read and counted.
    ///
    /// Buffer lookup is by **platform path equality**, not by exact `Uri`
    /// match: on case-insensitive filesystems (Windows, macOS) the client may
    /// open `lib.fs` while the project lists `Lib.fs`, and the buffer must
    /// still win over disk. It is the rule
    /// [`crate::workspace::Workspace::owning_project`] uses, so an open buffer
    /// that decides project ownership also supplies the text that is folded.
    /// O(N) over the open buffers.
    pub fn text_of(&self, path: &Path) -> Option<String> {
        let target = lexically_normalize(path);
        for (uri, text) in &self.buffers {
            if let Ok(doc_path) = uri.to_file_path()
                && paths_equal(&lexically_normalize(&doc_path), &target)
            {
                return Some(text.clone());
            }
        }
        self.read_disk(path).ok()
    }

    /// A document's current text: the buffer the client has open under
    /// exactly `uri`, else the on-disk contents of its path, read and counted.
    /// `None` for a non-`file:` URI that is not open, or an unreadable path.
    pub fn document_text(&self, uri: &Uri) -> Option<String> {
        if let Some(text) = self.buffers.get(uri) {
            return Some(text.clone());
        }
        let path = uri.to_file_path().ok()?;
        self.read_disk(&path).ok()
    }

    /// `path`'s text on disk without a leading byte-order mark, read and
    /// counted: the document a client would hold for it
    /// ([`crate::disk_text`]).
    pub fn read_disk(&self, path: &Path) -> std::io::Result<String> {
        let mut counts = self.counts.get();
        counts.opens += 1;
        self.counts.set(counts);
        // This module is the observer: its reads are the ones counted.
        #[allow(clippy::disallowed_methods)]
        let text = std::fs::read_to_string(path)?;
        Ok(crate::disk_text::without_bom(text))
    }

    /// Start a new generation, with nothing observed yet.
    pub fn begin_generation(&mut self) {
        self.generation += 1;
        self.counts.set(ReadCounts::default());
    }

    /// End the current generation: emit its counts as [`GENERATION_EVENT`]
    /// and return them.
    pub fn end_generation(&self) -> ReadCounts {
        let counts = self.counts.get();
        tracing::debug!(
            name: GENERATION_EVENT,
            generation = self.generation,
            opens = counts.opens,
        );
        counts
    }

    /// The current generation's number.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// What the current generation has observed so far.
    pub fn generation_counts(&self) -> ReadCounts {
        self.counts.get()
    }
}

/// A reason a file-system observation is **not** counted: the reader it
/// belongs to, and so the plan stage that will route it through an observing
/// module, or the fact that no stage does.
///
/// Every observation the crate makes outside [`Sources`] is one of these
/// methods, called on the variant that names its reader, so the plan's
/// worklist is this enum: routing a reader deletes its variant, and the
/// compiler lists every call site left to route. The methods do exactly what
/// the `std` call they wrap does, and count nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unobserved {
    /// The LSP's own reads of project-evaluation inputs: the `.fsproj` text,
    /// SDK discovery's `global.json` and `dotnet` probes, and the `.fsproj`
    /// diagnostics' existence probes. Stage 3b validates evaluations against
    /// their read set.
    Evaluation,
    /// The ownership walk's directory listings. Stage 4a.
    Ownership,
    /// The assembly env's reference resolution: the assets file, the project
    /// graph, the output-DLL search, the packs, the DLL reads, the on-disk
    /// assembly cache and the C# sidecar's binaries. Stage 6a.
    EnvResolution,
    /// The `workspace/diagnostic` sweep's walk of the workspace roots for
    /// `.fsproj` files. No stage routes it.
    WorkspaceSweep,
    /// Go-to-definition's reads of a referenced DLL, its PDB, and the
    /// SourceLink cache. No stage routes them: the DLLs and PDBs sit under
    /// the immutable roots of D3, and the cache is immutable by URL.
    GoToDefinition,
    /// Hover's `.xml` documentation files, which carry their own
    /// `(size, mtime)` witness (`xml_doc::lookup::DocFileCache`). No stage
    /// routes them.
    XmlDoc,
    /// On-demand restore: its inputs probe, and the files a `dotnet restore`
    /// child wrote to its scratch directory. No stage routes it.
    Restore,
}

// `Unobserved`'s methods are the crate's uncounted observations, so they are
// the calls the crate's `clippy.toml` bans everywhere else.
#[allow(clippy::disallowed_methods)]
impl Unobserved {
    /// [`std::fs::read`].
    pub fn read(self, path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    /// [`std::fs::read_to_string`].
    pub fn read_to_string(self, path: impl AsRef<Path>) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    /// [`std::fs::read_dir`].
    pub fn read_dir(self, path: impl AsRef<Path>) -> std::io::Result<std::fs::ReadDir> {
        std::fs::read_dir(path)
    }

    /// [`std::fs::metadata`].
    pub fn metadata(self, path: impl AsRef<Path>) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }

    /// [`std::fs::canonicalize`].
    pub fn canonicalize(self, path: impl AsRef<Path>) -> std::io::Result<std::path::PathBuf> {
        std::fs::canonicalize(path)
    }

    /// [`std::fs::File::open`].
    pub fn open(self, path: impl AsRef<Path>) -> std::io::Result<std::fs::File> {
        std::fs::File::open(path)
    }

    /// [`Path::exists`].
    pub fn exists(self, path: impl AsRef<Path>) -> bool {
        path.as_ref().exists()
    }

    /// [`Path::is_file`].
    pub fn is_file(self, path: impl AsRef<Path>) -> bool {
        path.as_ref().is_file()
    }

    /// [`Path::is_dir`].
    pub fn is_dir(self, path: impl AsRef<Path>) -> bool {
        path.as_ref().is_dir()
    }
}

/// The file-system reads of the crate's inline test modules, which set up and
/// inspect fixtures rather than observe a client's workspace. The crate's
/// `clippy.toml` bans the `std` calls, and allowing that lint wholesale in a
/// test module would also allow the child-process spawns it bans, so tests
/// reach the `std` calls here instead. `tests/all/common/host_fs.rs` is the
/// integration tests' twin.
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
pub(crate) mod host_fs {
    use std::path::{Path, PathBuf};

    pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    pub fn read_to_string(path: impl AsRef<Path>) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }

    pub fn read_dir(path: impl AsRef<Path>) -> std::io::Result<std::fs::ReadDir> {
        std::fs::read_dir(path)
    }

    pub fn canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    pub fn open_with(
        options: &std::fs::OpenOptions,
        path: impl AsRef<Path>,
    ) -> std::io::Result<std::fs::File> {
        options.open(path)
    }

    /// The `Path` probes, under names that do not collide with the inherent
    /// methods (which would win method resolution).
    pub trait HostPath {
        fn host_exists(&self) -> bool;
        fn host_is_file(&self) -> bool;
        fn host_is_dir(&self) -> bool;
    }

    impl HostPath for Path {
        fn host_exists(&self) -> bool {
            self.exists()
        }
        fn host_is_file(&self) -> bool {
            self.is_file()
        }
        fn host_is_dir(&self) -> bool {
            self.is_dir()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clippy reads only the nearest `clippy.toml`, so the crate's replaces
    /// the workspace's: it must carry every one of the workspace's settings,
    /// or this crate silently loses a ban every other crate keeps.
    #[test]
    fn clippy_config_repeats_the_workspace_bans() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = host_fs::read_to_string(manifest.join("../../clippy.toml")).unwrap();
        let crate_config = host_fs::read_to_string(manifest.join("clippy.toml")).unwrap();
        let crate_lines: std::collections::HashSet<&str> = crate_config.lines().collect();
        let settings: Vec<&str> = workspace
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .collect();
        assert!(
            settings
                .iter()
                .any(|line| line.contains("std::process::Command::spawn")),
            "the workspace's clippy.toml no longer holds the spawn ban: {workspace}"
        );
        for line in settings {
            assert!(
                crate_lines.contains(line),
                "crates/lsp/clippy.toml is missing the workspace's {line:?}"
            );
        }
    }

    #[test]
    fn a_disk_read_drops_a_leading_mark() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("A.fs");
        let sources = Sources::new();
        std::fs::write(&path, "\u{feff}let x = \"\u{feff}\"\n").unwrap();
        assert_eq!(sources.read_disk(&path).unwrap(), "let x = \"\u{feff}\"\n");
        std::fs::write(&path, "let x = 1\n").unwrap();
        assert_eq!(sources.read_disk(&path).unwrap(), "let x = 1\n");
    }
}
