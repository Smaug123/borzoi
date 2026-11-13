//! Every file-system observation an evaluation makes
//! (`docs/incremental-lsp-plan.md`, D3).
//!
//! Each function here performs one `std` call, counts it in the [`Observer`]
//! it is handed, and records what it saw as a [`Read`]: the path and the
//! [`Observation`], which is the query together with its answer. When the
//! evaluation ends, the observer's reads become the [`ReadSet`] that
//! [`crate::ParsedProject::read_set`] carries, and its counts become
//! [`crate::ParsedProject::read_counts`].
//!
//! An evaluation is a deterministic function of its inputs and of the answers
//! to the queries it makes. So while every query in its read set would get
//! the same answer again, the evaluation would come out the same:
//! [`ReadSet::first_stale`] asks each query again and reports the first whose
//! answer moved. That holds only if every query is recorded, which is why the
//! crate's `clippy.toml` bans the `std` calls everywhere but here.
//!
//! Absence is an answer like any other. An evaluation that found no
//! `Directory.Build.props` in some ancestor depends on that file staying
//! absent, so a failed query is recorded with its [`Failure`], whose kind is
//! [`std::io::ErrorKind::NotFound`] for a path that is not there.
//!
//! Test code, which builds fixtures rather than observing a project, reaches
//! the banned calls through `host_fs` (which the integration tests reach
//! through the `test-support` feature), because allowing the lint wholesale in
//! a test module would also lift the child-process spawn bans it carries.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Where an evaluation's observations go: the reads it made, and how many
/// calls of each kind it made.
///
/// It is shared by reference through the walk, and handed to the caller's
/// [`crate::SdkResolver`] and (inside [`crate::GlobRequest`]) its
/// [`crate::GlobResolver`], so it records through interior mutability. One
/// evaluation is single-threaded and owns its observer, so nothing else sees
/// the cells change.
#[derive(Debug, Default)]
pub struct Observer {
    reads: RefCell<Vec<Read>>,
    counts: Cell<ReadCounts>,
}

impl Observer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The read set and the counts. A query asked twice with the same answer
    /// is one read. Asked twice with different answers (the file moved during
    /// the evaluation), it is two, and the read set can never validate again.
    ///
    /// The reads are sorted, so the read set is a set: the order an
    /// evaluation happened to ask its queries in (a walk over a `HashMap`,
    /// say) does not tell two equal read sets apart.
    pub fn finish(self) -> (ReadSet, ReadCounts) {
        let mut reads = self.reads.into_inner();
        reads.sort();
        reads.dedup();
        (ReadSet { reads }, self.counts.get())
    }

    fn record(&self, path: &Path, observation: Observation) {
        let mut counts = self.counts.get();
        match &observation {
            Observation::Text(_) => counts.opens += 1,
            Observation::Kind(_) | Observation::LinkKind(_) => counts.stats += 1,
            Observation::Listing(_) => counts.listings += 1,
            Observation::Canonical(_) => counts.canonicalizations += 1,
        }
        self.counts.set(counts);
        self.reads.borrow_mut().push(Read {
            path: path.to_path_buf(),
            observation,
        });
    }
}

/// How many file-system calls of each kind an evaluation made, counting
/// repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ReadCounts {
    /// Files opened to read them whole, whether or not the open succeeded.
    /// Each successful open is followed by one `fstat` of the open file
    /// (`std`'s whole-file read asks for the size).
    pub opens: u64,
    /// Paths `stat`ed or `lstat`ed.
    pub stats: u64,
    /// Directories listed (`opendir`).
    pub listings: u64,
    /// Paths canonicalised (`realpath`).
    pub canonicalizations: u64,
}

impl std::ops::Add for ReadCounts {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self {
            opens: self.opens + other.opens,
            stats: self.stats + other.stats,
            listings: self.listings + other.listings,
            canonicalizations: self.canonicalizations + other.canonicalizations,
        }
    }
}

/// The distinct queries an evaluation made, each with the answer it got,
/// sorted by path and then by query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadSet {
    reads: Vec<Read>,
}

impl ReadSet {
    pub fn reads(&self) -> &[Read] {
        &self.reads
    }

    pub fn len(&self) -> usize {
        self.reads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reads.is_empty()
    }

    /// The first read whose query, asked of the host's file system now, gets
    /// a different answer. `None` when every answer stands, and then the
    /// evaluation that recorded this read set would come out the same.
    pub fn first_stale(&self) -> Option<&Read> {
        self.reads.iter().find(|read| !read.still_holds())
    }

    /// Both read sets' reads, as one set: what a value computed from both
    /// evaluations (or from an evaluation and the reads that prepared it)
    /// depends on. A query the two answered differently keeps both answers,
    /// so the union can never validate, as for a file that moved during one
    /// evaluation.
    pub fn union(self, other: ReadSet) -> ReadSet {
        let mut reads = self.reads;
        reads.extend(other.reads);
        reads.sort();
        reads.dedup();
        ReadSet { reads }
    }
}

/// One query and its answer.
///
/// Reads compare by the path's raw spelling, not by `Path`'s equality, which
/// ignores a trailing separator: `p/child/` demands a directory and
/// `p/child` does not, so they are different queries.
#[derive(Clone, Debug)]
pub struct Read {
    pub path: PathBuf,
    pub observation: Observation,
}

impl Read {
    fn key(&self) -> (&std::ffi::OsStr, &Observation) {
        (self.path.as_os_str(), &self.observation)
    }

    /// Whether the query, asked of the host's file system now, gets the
    /// answer recorded.
    pub fn still_holds(&self) -> bool {
        answer_now(&self.path, &self.observation) == self.observation
    }
}

impl PartialEq for Read {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for Read {}

impl Hash for Read {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl PartialOrd for Read {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Read {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

/// A query, with everything its asker could learn from the answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Observation {
    /// The file read whole as UTF-8 (`std::fs::read_to_string`).
    Text(Result<TextDigest, Failure>),
    /// What the path names, following symlinks (`stat`).
    Kind(Result<EntryKind, Failure>),
    /// What the path itself is, not following a final symlink (`lstat`).
    LinkKind(Result<EntryKind, Failure>),
    /// The directory's entries, sorted by name (see [`read_dir`]), each with
    /// the type its directory entry reports (which does not follow symlinks).
    Listing(Result<Vec<Listed>, Failure>),
    /// The path with every symlink resolved (`realpath`).
    Canonical(Result<PathBuf, Failure>),
}

/// `query`'s query (its answer is ignored) asked of the host's file system
/// now, at `path`, with its answer: what the query function would record,
/// without recording it. A caller that validates a read set through its own
/// counters asks here, so that its answers are the query functions' own.
pub fn answer_now(path: &Path, query: &Observation) -> Observation {
    match query {
        Observation::Text(_) => Observation::Text(text_digest(&read_text(path))),
        Observation::Kind(_) => Observation::Kind(kind_of(path)),
        Observation::LinkKind(_) => Observation::LinkKind(link_kind_of(path)),
        Observation::Listing(_) => Observation::Listing(listed(&listing_of(path))),
        Observation::Canonical(_) => Observation::Canonical(canonical_of(path)),
    }
}

/// The identity of a text: its length and a 128-bit hash of its bytes.
///
/// A digest rather than the text, so that a read set does not keep every file
/// of the SDK chain alive. A collision between a file's old and new text would
/// let a stale read validate; over accidental edits, at 128 bits, that is not
/// worth the memory of the texts. The hash is `std`'s, which is stable within
/// a process, and a read set is never persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextDigest {
    pub len: u64,
    pub hash: u128,
}

impl TextDigest {
    pub fn of(text: &str) -> Self {
        let half = |salt: u8| {
            let mut hasher = std::hash::DefaultHasher::new();
            salt.hash(&mut hasher);
            text.hash(&mut hasher);
            hasher.finish()
        };
        Self {
            len: text.len() as u64,
            hash: (u128::from(half(0)) << 64) | u128::from(half(1)),
        }
    }
}

/// What a path names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    File,
    Dir,
    /// Only from a query that does not follow symlinks.
    Symlink,
    /// A socket, a FIFO, a device.
    Other,
}

impl EntryKind {
    fn of(file_type: std::fs::FileType) -> Self {
        if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        }
    }
}

/// A failed query: everything its asker can learn from the `std::io::Error`,
/// which is its kind and its message (an OS error's message carries its
/// code). A path that is not there fails with [`ErrorKind::NotFound`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Failure {
    kind: ErrorKind,
    message: String,
}

impl Failure {
    /// What its asker can learn from `error`.
    pub fn of(error: &std::io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Whether the path was not there.
    pub fn is_absent(&self) -> bool {
        self.kind == ErrorKind::NotFound
    }
}

impl std::fmt::Display for Failure {
    /// The `std::io::Error`'s own message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// One entry of a recorded directory listing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Listed {
    Entry {
        name: OsString,
        /// The directory entry's own type, which does not follow a symlink.
        kind: Result<EntryKind, Failure>,
    },
    /// The listing failed at this entry.
    Unreadable(Failure),
}

/// One entry of a directory that [`read_dir`] listed.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: OsString,
    /// The directory entry's own type, which does not follow a symlink.
    pub kind: Result<EntryKind, Failure>,
}

// The functions below make the observations the crate's `clippy.toml` bans
// everywhere else. They are public so that a caller's own `SdkResolver` or
// `GlobResolver` can query through the observer it is handed.

/// The answer a whole-file read gets from `bytes`, the result of reading the
/// file: its text, or why there is none. A file that is not UTF-8 fails as
/// [`ErrorKind::InvalidData`].
///
/// [`read_to_string`] answers through this function, so a caller that reads
/// a file in its own way (stamping it first, say) and hands the bytes here
/// gets the answer the query function would have recorded.
pub fn text_answer(bytes: std::io::Result<Vec<u8>>) -> Result<String, Failure> {
    let bytes = bytes.map_err(|e| Failure::of(&e))?;
    String::from_utf8(bytes).map_err(|_| {
        Failure::of(&std::io::Error::new(
            ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        ))
    })
}

/// What a [`Observation::Text`] records of a whole-file read's answer.
pub fn text_digest(answer: &Result<String, Failure>) -> Result<TextDigest, Failure> {
    answer.as_deref().map(TextDigest::of).map_err(Clone::clone)
}

#[allow(clippy::disallowed_methods)]
fn read_text(path: &Path) -> Result<String, Failure> {
    text_answer(std::fs::read(path))
}

#[allow(clippy::disallowed_methods)]
fn kind_of(path: &Path) -> Result<EntryKind, Failure> {
    std::fs::metadata(path)
        .map(|meta| EntryKind::of(meta.file_type()))
        .map_err(|e| Failure::of(&e))
}

#[allow(clippy::disallowed_methods)]
fn link_kind_of(path: &Path) -> Result<EntryKind, Failure> {
    std::fs::symlink_metadata(path)
        .map(|meta| EntryKind::of(meta.file_type()))
        .map_err(|e| Failure::of(&e))
}

#[allow(clippy::disallowed_methods)]
fn listing_of(path: &Path) -> Result<Vec<Result<DirEntry, Failure>>, Failure> {
    std::fs::read_dir(path)
        .map_err(|e| Failure::of(&e))
        .map(|entries| {
            entries
                .map(|entry| {
                    let entry = entry.map_err(|e| Failure::of(&e))?;
                    Ok(DirEntry {
                        path: entry.path(),
                        name: entry.file_name(),
                        kind: entry
                            .file_type()
                            .map(EntryKind::of)
                            .map_err(|e| Failure::of(&e)),
                    })
                })
                .collect::<Vec<_>>()
        })
        .map(|mut entries| {
            entries.sort_by(|a, b| match (a, b) {
                (Ok(a), Ok(b)) => a.name.cmp(&b.name),
                (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                (Err(_), Err(_)) => std::cmp::Ordering::Equal,
            });
            entries
        })
}

/// What a [`Observation::Listing`] records of a listing's answer.
fn listed(
    answer: &Result<Vec<Result<DirEntry, Failure>>, Failure>,
) -> Result<Vec<Listed>, Failure> {
    answer.as_ref().map_err(Clone::clone).map(|entries| {
        entries
            .iter()
            .map(|entry| match entry {
                Ok(entry) => Listed::Entry {
                    name: entry.name.clone(),
                    kind: entry.kind.clone(),
                },
                Err(failure) => Listed::Unreadable(failure.clone()),
            })
            .collect()
    })
}

#[allow(clippy::disallowed_methods)]
fn canonical_of(path: &Path) -> Result<PathBuf, Failure> {
    std::fs::canonicalize(path).map_err(|e| Failure::of(&e))
}

/// [`std::fs::read_to_string`].
pub fn read_to_string(observer: &Observer, path: &Path) -> Result<String, Failure> {
    let result = read_text(path);
    observer.record(path, Observation::Text(text_digest(&result)));
    result
}

/// What `path` names, following symlinks: [`std::fs::metadata`].
pub fn kind(observer: &Observer, path: &Path) -> Result<EntryKind, Failure> {
    let result = kind_of(path);
    observer.record(path, Observation::Kind(result.clone()));
    result
}

/// What `path` itself is: [`std::fs::symlink_metadata`].
pub fn link_kind(observer: &Observer, path: &Path) -> Result<EntryKind, Failure> {
    let result = link_kind_of(path);
    observer.record(path, Observation::LinkKind(result.clone()));
    result
}

/// [`Path::is_file`].
pub fn is_file(observer: &Observer, path: &Path) -> bool {
    kind(observer, path) == Ok(EntryKind::File)
}

/// [`Path::is_dir`].
pub fn is_dir(observer: &Observer, path: &Path) -> bool {
    kind(observer, path) == Ok(EntryKind::Dir)
}

/// [`Path::exists`].
pub fn exists(observer: &Observer, path: &Path) -> bool {
    kind(observer, path).is_ok()
}

/// [`std::fs::read_dir`], read to the end, with the entries sorted by name and
/// any the listing could not read after them, as `Err`s.
///
/// Sorted because the host's order is not a property of the directory's
/// contents: tmpfs lists in creation order, so deleting a file and writing it
/// back moves it. An evaluation that read the host's order could change with
/// no entry changing, and its read set would have to record that order.
pub fn read_dir(
    observer: &Observer,
    path: &Path,
) -> Result<Vec<Result<DirEntry, Failure>>, Failure> {
    let result = listing_of(path);
    observer.record(path, Observation::Listing(listed(&result)));
    result
}

/// [`std::fs::canonicalize`].
pub fn canonicalize(observer: &Observer, path: &Path) -> Result<PathBuf, Failure> {
    let result = canonical_of(path);
    observer.record(path, Observation::Canonical(result.clone()));
    result
}

#[cfg(any(test, feature = "test-support"))]
pub mod soundness;

/// The file-system reads of the crate's inline test modules, which set up and
/// inspect fixtures rather than observe a project. The crate's `clippy.toml`
/// bans the `std` calls, and allowing that lint wholesale in a test module
/// would also allow the child-process spawns it bans, so tests reach the `std`
/// calls here instead. The integration tests reach it through the
/// `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::disallowed_methods)]
pub mod host_fs {
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

    pub fn metadata(path: impl AsRef<Path>) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }

    pub fn symlink_metadata(path: impl AsRef<Path>) -> std::io::Result<std::fs::Metadata> {
        std::fs::symlink_metadata(path)
    }

    pub fn read_link(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
        std::fs::read_link(path)
    }

    /// The `Path` probes, under names that do not collide with the inherent
    /// methods (which would win method resolution).
    pub trait HostPath {
        fn host_exists(&self) -> bool;
        fn host_is_file(&self) -> bool;
        fn host_is_dir(&self) -> bool;
        fn host_canonicalize(&self) -> std::io::Result<PathBuf>;
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
        fn host_canonicalize(&self) -> std::io::Result<PathBuf> {
            self.canonicalize()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::host_fs::HostPath as _;
    use super::*;

    /// Clippy reads only the nearest `clippy.toml`, so the crate's replaces
    /// the workspace's: it must carry every one of the workspace's settings,
    /// or this crate silently loses a ban every other crate keeps.
    #[test]
    fn clippy_config_repeats_the_workspace_bans() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = host_fs::read_to_string(manifest.join("../../clippy.toml")).unwrap();
        let crate_config = host_fs::read_to_string(manifest.join("clippy.toml")).unwrap();
        let crate_lines: HashSet<&str> = crate_config.lines().collect();
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
                "crates/msbuild/clippy.toml is missing the workspace's {line:?}"
            );
        }
    }

    /// Each query records what its asker saw, a failure with the
    /// `std::io::Error`'s own kind and message, and asking it again of an
    /// unchanged file system gives the same answer.
    #[test]
    fn a_query_records_its_answer_and_asks_again_the_same() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        std::fs::write(dir.join("a.props"), "<Project />").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let observer = Observer::new();
        assert_eq!(
            read_to_string(&observer, &dir.join("a.props")).unwrap(),
            "<Project />"
        );
        let missing = read_to_string(&observer, &dir.join("b.props")).unwrap_err();
        let io = host_fs::read_to_string(dir.join("b.props")).unwrap_err();
        assert!(missing.is_absent());
        assert_eq!(missing.to_string(), io.to_string());
        assert!(is_dir(&observer, &dir.join("sub")));
        assert!(!is_file(&observer, &dir.join("sub")));
        assert_eq!(link_kind(&observer, &dir.join("sub")), Ok(EntryKind::Dir));
        let mut names: Vec<_> = read_dir(&observer, &dir)
            .unwrap()
            .into_iter()
            .map(|e| e.unwrap().name)
            .collect();
        names.sort();
        assert_eq!(names, ["a.props", "sub"]);
        assert_eq!(canonicalize(&observer, &dir.join("sub/..")).unwrap(), dir);
        let (read_set, counts) = observer.finish();
        assert_eq!(
            counts,
            ReadCounts {
                opens: 2,
                // `is_dir` and `is_file` of `sub` ask the same query, so the
                // read set holds it once.
                stats: 3,
                listings: 1,
                canonicalizations: 1,
            }
        );
        assert_eq!(read_set.len(), 6);
        assert_eq!(read_set.first_stale(), None);
        std::fs::write(dir.join("b.props"), "").unwrap();
        // The reads are sorted by path, so the first to go stale is the
        // listing of `dir`, which gained an entry.
        assert_eq!(
            read_set.first_stale().map(|read| read.path.clone()),
            Some(dir.clone())
        );
    }

    /// `p/child/` and `p/child` are different queries (the slash demands a
    /// directory), though `Path` compares them equal. With `p` a file both
    /// fail alike; once `p` is a directory holding a file `child`, only the
    /// slash-ended one still fails, so the read set must keep both.
    #[test]
    fn a_trailing_separator_is_a_different_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        std::fs::write(dir.join("p"), "").unwrap();
        let observer = Observer::new();
        let slashed = PathBuf::from(format!("{}/p/child/", dir.display()));
        let _ = kind(&observer, &slashed);
        let _ = kind(&observer, &dir.join("p/child"));
        let (read_set, _) = observer.finish();
        assert_eq!(read_set.len(), 2, "{read_set:#?}");
        std::fs::remove_file(dir.join("p")).unwrap();
        std::fs::create_dir(dir.join("p")).unwrap();
        std::fs::write(dir.join("p/child"), "").unwrap();
        assert!(read_set.first_stale().is_some());
    }

    /// A listing is sorted by name whatever order the entries were created
    /// in, which some file systems (tmpfs) list them in.
    #[test]
    fn a_listing_is_sorted_by_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        for name in ["m", "z", "a", "q", "b"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let names: Vec<_> = read_dir(&Observer::new(), &dir)
            .unwrap()
            .into_iter()
            .map(|e| e.unwrap().name)
            .collect();
        assert_eq!(names, ["a", "b", "m", "q", "z"]);
    }

    /// A whole-file read answers what `std::fs::read_to_string` answers,
    /// for every byte content: text with a byte-order mark kept, and a file
    /// that is not UTF-8 failing as invalid data. And [`answer_now`] gives
    /// every query's recorded answer.
    #[test]
    fn a_read_answers_as_std_does() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        let contents: [&[u8]; 6] = [
            b"",
            b"<Project />",
            "\u{feff}<Project />".as_bytes(),
            b"\xff\xfe",
            b"ok\xc3",
            "\u{e9}t\u{e9}".as_bytes(),
        ];
        for bytes in contents {
            let path = dir.join("f");
            std::fs::write(&path, bytes).unwrap();
            let std = host_fs::read_to_string(&path);
            let ours = read_to_string(&Observer::new(), &path);
            match (&std, &ours) {
                (Ok(a), Ok(b)) => assert_eq!(a, b),
                (Err(a), Err(b)) => assert_eq!(a.kind(), b.kind()),
                _ => panic!("{bytes:?}: std {std:?}, ours {ours:?}"),
            }
        }
        let observer = Observer::new();
        let _ = read_to_string(&observer, &dir.join("f"));
        let _ = read_to_string(&observer, &dir.join("absent"));
        let _ = kind(&observer, &dir);
        let _ = link_kind(&observer, &dir.join("f"));
        let _ = read_dir(&observer, &dir);
        let _ = canonicalize(&observer, &dir.join("f"));
        let (read_set, _) = observer.finish();
        for read in read_set.reads() {
            assert_eq!(answer_now(&read.path, &read.observation), read.observation);
        }
    }

    /// A union holds each read once, and a query answered two ways keeps
    /// both answers, so it never validates.
    #[test]
    fn a_union_keeps_both_answers_of_a_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        let path = dir.join("p.props");
        let ask = || {
            let observer = Observer::new();
            let _ = read_to_string(&observer, &path);
            let _ = kind(&observer, &dir);
            observer.finish().0
        };
        let before = ask();
        assert_eq!(before.clone().union(before.clone()), before);
        std::fs::write(&path, "<Project />").unwrap();
        let after = ask();
        let both = before.union(after.clone());
        assert_eq!(both.len(), 3, "{both:#?}");
        assert!(both.first_stale().is_some());
        assert_eq!(after.first_stale(), None);
    }

    /// Two evaluations that ask the same queries in different orders (a walk
    /// over a `HashMap`, say) have equal read sets.
    #[test]
    fn the_read_set_does_not_depend_on_the_order_of_its_queries() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        let paths: Vec<PathBuf> = ["a", "b", "c", "d"]
            .iter()
            .map(|name| dir.join(name))
            .collect();
        std::fs::write(&paths[1], "x").unwrap();
        std::fs::create_dir(&paths[2]).unwrap();
        let ask = |order: &[usize]| {
            let observer = Observer::new();
            for &i in order {
                let _ = kind(&observer, &paths[i]);
                let _ = read_to_string(&observer, &paths[i]);
            }
            observer.finish()
        };
        let forwards = ask(&[0, 1, 2, 3]);
        for order in [[3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
            assert_eq!(ask(&order), forwards, "asked in the order {order:?}");
        }
    }
}
