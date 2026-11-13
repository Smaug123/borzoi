//! Every file-system observation the server makes
//! (`docs/incremental-lsp-plan.md`, D3).
//!
//! - **Source reads** go through [`crate::vfs::Vfs`], whose disk is a
//!   [`HostDisk`]: the `stat`s and reads here, which the `Vfs` counts per
//!   **generation**, the unit of work the dispatch loop does for one message
//!   (or one step of a `workspace/diagnostic` sweep). The `Vfs` emits each
//!   generation's [`ReadCounts`] as the [`GENERATION_EVENT`] `tracing` event,
//!   so "a comment keystroke opens no file" can be asserted by a test on any
//!   platform, and `edit_counts` reports the same counts.
//! - **Every other observation** is a method of [`Unobserved`], whose variant
//!   names the reader it belongs to and the plan stage that will route it.
//!   Those are not counted.
//!
//! The crate's `clippy.toml` bans the `std::fs` and `std::path::Path` calls
//! that observe the file system, so a new reader that skips this module is a
//! lint error. The calls here are the only ones allowed outside test code,
//! which reaches them through `host_fs` (and its integration-test twin).

use std::io::Read as _;
use std::path::Path;

use crate::vfs::{Disk, Nanos, Stamp};

/// The name of the event [`crate::vfs::Vfs::end_generation`] emits, whose
/// fields are the generation's number (`generation`) and its [`ReadCounts`]
/// (`opens`, `stats`).
pub const GENERATION_EVENT: &str = "observed.generation";

/// What one generation observed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadCounts {
    /// Files opened to read them, whether or not the open succeeded. Each
    /// successful open is followed by one `fstat` of the open file.
    pub opens: u64,
    /// Paths `stat`ed to validate what an earlier generation read.
    pub stats: u64,
}

/// The host's file system, as the [`crate::vfs::Vfs`] reads it.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostDisk;

/// The stamp of a file whose metadata is `meta`.
#[cfg(unix)]
fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    use std::os::unix::fs::MetadataExt as _;
    let nanos = |secs: i64, nsec: i64| Nanos(i128::from(secs) * 1_000_000_000 + i128::from(nsec));
    Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime: nanos(meta.mtime(), meta.mtime_nsec()),
        ctime: nanos(meta.ctime(), meta.ctime_nsec()),
    }
}

/// The stamp of a file whose metadata is `meta`. `std` exposes no inode or
/// change time here, so the stamp is the size and the mtime, which is weaker
/// than the unix stamp the racy rule is argued for.
#[cfg(not(unix))]
fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    let mtime = meta.modified().map_or(Nanos(0), Nanos::of);
    Stamp {
        dev: 0,
        ino: 0,
        size: meta.len(),
        mtime,
        ctime: mtime,
    }
}

// The `Vfs`'s disk is the observer: its calls are the ones counted.
#[allow(clippy::disallowed_methods)]
impl Disk for HostDisk {
    fn stat(&self, path: &Path) -> std::io::Result<Stamp> {
        std::fs::metadata(path).map(|meta| stamp_of(&meta))
    }

    fn read(&self, path: &Path) -> std::io::Result<(Stamp, Vec<u8>)> {
        let mut file = std::fs::File::open(path)?;
        // Stamp first: a write after this moves the stamp, so the next
        // generation re-reads whatever the read below saw.
        let stamp = stamp_of(&file.metadata()?);
        let mut bytes = Vec::with_capacity(stamp.size as usize);
        file.read_to_end(&mut bytes)?;
        Ok((stamp, bytes))
    }
}

/// A reason a file-system observation is **not** counted: the reader it
/// belongs to, and so the plan stage that will route it through an observing
/// module, or the fact that no stage does.
///
/// Every observation the crate makes outside [`crate::vfs::Vfs`] is one of these
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
    fn a_host_read_stamps_what_it_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("A.fs");
        std::fs::write(&path, "let x = 1\n").unwrap();
        let (stamp, bytes) = HostDisk.read(&path).unwrap();
        assert_eq!(bytes, b"let x = 1\n");
        assert_eq!(stamp.size, 10);
        assert_eq!(HostDisk.stat(&path).unwrap(), stamp);
        assert_eq!(
            HostDisk.stat(&tmp.path().join("B.fs")).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
