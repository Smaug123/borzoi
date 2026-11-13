//! Every file-system observation the server makes
//! (`docs/incremental-lsp-plan.md`, D3).
//!
//! - **Source reads** go through [`crate::vfs::Vfs`], whose disk is a
//!   [`HostDisk`]: the `stat`s and reads here, which the `Vfs` counts per
//!   **generation**, the unit of work the dispatch loop does for one message
//!   (or one step of a `workspace/diagnostic` sweep). The `Vfs` sends each
//!   ended [`Generation`] to the log a caller attached
//!   ([`crate::vfs::Vfs::log_generations`]) and emits it as the
//!   [`GENERATION_EVENT`] `tracing` event. So "a comment keystroke opens no
//!   file" can be asserted by a test on any platform, reading the log as
//!   data, and `edit_counts` reports the same counts from the event.
//! - **MSBuild evaluations** observe through `borzoi_msbuild::observed`, which
//!   records each one's read set and counts in its `ParsedProject`; the
//!   server emits the counts as the [`EVALUATION_EVENT`] `tracing` event. The
//!   reads that prepare an evaluation (the `.fsproj` text, SDK discovery's
//!   `global.json` walk and `dotnet` probes) record into an observer of their
//!   own, and the [`crate::workspace::Workspace`] keeps all of them beside
//!   the evaluation and confirms them once per generation that reads it,
//!   counted in the generation's [`EvaluationCounts`].
//! - **The ownership walk** ([`crate::workspace::Workspace::owning_project`])
//!   lists the directories above a file through `borzoi_msbuild::observed`
//!   too, and the [`crate::workspace::Workspace`] counts its listings in the
//!   generation's [`Generation::ownership`].
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

/// The name of the event [`crate::vfs::Vfs::end_generation_with`] emits, whose
/// fields are the generation's number (`generation`), its source reads'
/// [`ReadCounts`] (`opens`, `stats`), its [`EvaluationCounts`]
/// (`evaluated`, and the validation's `v_opens`, `v_stats`, `v_listings` and
/// `v_canonicalizations`), and its [`Generation::ownership`] (`o_listings`,
/// `o_stats`).
///
/// A test reads [`crate::vfs::Vfs::log_generations`] instead. `tracing`
/// caches each callsite's interest for the whole process, and while one
/// dispatcher is registered it computes that interest from the default of
/// whichever thread reaches the callsite first. So a subscriber scoped to one
/// test's server thread misses every event if a loop in another test of the
/// same binary reaches this callsite first.
pub const GENERATION_EVENT: &str = "observed.generation";

/// The name of the event [`report_evaluation`] emits for every MSBuild
/// evaluation the server runs, whose fields are the evaluation's
/// [`borzoi_msbuild::observed::ReadCounts`] (`opens`, `stats`, `listings`,
/// `canonicalizations`) and the size of its read set (`reads`). Like
/// [`GENERATION_EVENT`], it is for `edit_counts`, not for tests.
pub const EVALUATION_EVENT: &str = "observed.evaluation";

/// Emit `parsed`'s file-system counts as [`EVALUATION_EVENT`].
pub fn report_evaluation(parsed: &borzoi_msbuild::ParsedProject) {
    let counts = parsed.read_counts;
    tracing::debug!(
        name: EVALUATION_EVENT,
        opens = counts.opens,
        stats = counts.stats,
        listings = counts.listings,
        canonicalizations = counts.canonicalizations,
        reads = parsed.read_set.len() as u64,
    );
}

/// What one generation observed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadCounts {
    /// Files opened to read them, whether or not the open succeeded. Each
    /// successful open is followed by one `fstat` of the open file.
    pub opens: u64,
    /// Paths `stat`ed to validate what an earlier generation read.
    pub stats: u64,
}

/// What reading project evaluations cost one generation: the evaluations it
/// ran, and the queries it asked to confirm the evaluations it reused
/// ([`crate::workspace::Workspace`] validates each cached evaluation's read
/// set once per generation that reads it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EvaluationCounts {
    /// MSBuild evaluations run (each [`EVALUATION_EVENT`] reports one's own
    /// reads). A project evaluated under a second, seeded target framework
    /// counts once.
    pub evaluated: u64,
    /// The queries asked to validate cached evaluations: files opened (a
    /// file read again because its stamp moved), paths `stat`ed (a file's
    /// stamp, or what a path names), directories listed and paths
    /// canonicalised.
    pub validation: borzoi_msbuild::observed::ReadCounts,
}

/// One ended generation: its number and what it observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation {
    pub number: u64,
    /// The source reads ([`crate::vfs::Vfs`]).
    pub counts: ReadCounts,
    /// The project evaluations read.
    pub evaluations: EvaluationCounts,
    /// What finding the projects that own files observed: the directories
    /// the ownership walk listed.
    pub ownership: borzoi_msbuild::observed::ReadCounts,
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
        ctime: Some(nanos(meta.ctime(), meta.ctime_nsec())),
    }
}

/// The stamp of a file whose metadata is `meta`. `std` exposes no inode or
/// change time here, so the stamp is the size and the mtime, and having no
/// ctime it is always racy: a timestamp-preserving rewrite of the same size
/// would leave it unchanged, so every generation re-reads the file.
#[cfg(not(unix))]
fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    let mtime = meta.modified().map_or(Nanos(0), Nanos::of);
    Stamp {
        dev: 0,
        ino: 0,
        size: meta.len(),
        mtime,
        ctime: None,
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

// The project evaluations' reads are confirmed through the same host.
#[allow(clippy::disallowed_methods)]
impl crate::read_checks::Probe for HostDisk {
    fn entry(&self, path: &Path) -> std::io::Result<crate::read_checks::EntryAt> {
        use crate::read_checks::EntryAt;
        let kind = std::fs::symlink_metadata(path)?.file_type();
        Ok(if kind.is_symlink() {
            EntryAt::Link(std::fs::read_link(path)?)
        } else if kind.is_dir() {
            EntryAt::Dir
        } else {
            EntryAt::Other
        })
    }

    fn answer(
        &self,
        path: &Path,
        query: &borzoi_msbuild::observed::Observation,
    ) -> borzoi_msbuild::observed::Observation {
        borzoi_msbuild::observed::answer_now(path, query)
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
    /// The `.fsproj` buffer's diagnostics: their existence probes, and the
    /// SDK discovery they run for the buffer (into an observer nobody
    /// reads). No stage routes them: the diagnostics are computed afresh for
    /// every request, so nothing they read is cached.
    FsprojDiagnostics,
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

    /// An observer for a `borzoi-msbuild` function called outside an
    /// evaluation, such as SDK discovery's `global.json` walk: the function
    /// records into it, and nothing reads what it recorded.
    pub fn msbuild_observer(self) -> borzoi_msbuild::observed::Observer {
        borzoi_msbuild::observed::Observer::new()
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
