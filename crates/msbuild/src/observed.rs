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
//! An evaluation asks each query at most once: the observer remembers every
//! answer it recorded, and a query asked again takes that answer
//! ([`Observer::new`]). The read set is what it would be had the query been
//! asked again and got the same answer, which is one read; a file that moves
//! during the evaluation is seen by the next validation, which asks again.
//! An imported file's parsed document is kept across evaluations in
//! [`ImportDocuments`], found by the file's path and reused only when the
//! file's bytes, read again, are the bytes it was parsed from.
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
use std::collections::HashMap;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::document::ParsedDocument;

/// Where an evaluation's observations go: the reads it made, and how many
/// calls of each kind it made.
///
/// It is shared by reference through the walk, and handed to the caller's
/// [`crate::SdkResolver`] and (inside [`crate::GlobRequest`]) its
/// [`crate::GlobResolver`], so it records through interior mutability. One
/// evaluation is single-threaded and owns its observer, so nothing else sees
/// the cells change.
#[derive(Debug)]
pub struct Observer {
    reads: RefCell<Vec<Read>>,
    counts: Cell<ReadCounts>,
    /// Every answer recorded so far, by query, which a query asked again
    /// takes instead of asking the file system. `None` in an observer that
    /// asks every time ([`Observer::asking_every_time`]).
    memo: Option<RefCell<Memo>>,
    /// The imported files' documents the evaluation may reuse, and those it
    /// parsed ([`read_document`]).
    documents: RefCell<ImportDocuments>,
}

impl Default for Observer {
    fn default() -> Self {
        Self::new()
    }
}

/// The answers an [`Observer`] has recorded, by query and then by the path's
/// raw spelling (`p/child/` and `p/child` are different queries).
#[derive(Debug, Default)]
struct Memo {
    texts: HashMap<OsString, Result<String, Failure>>,
    documents: HashMap<OsString, Result<Parsed, Failure>>,
    kinds: HashMap<OsString, Result<EntryKind, Failure>>,
    link_kinds: HashMap<OsString, Result<EntryKind, Failure>>,
    listings: HashMap<OsString, Result<Vec<Result<DirEntry, Failure>>, Failure>>,
    canonicals: HashMap<OsString, Result<PathBuf, Failure>>,
}

/// A whole-file read parsed as XML ([`read_document`]): the document, or why
/// the text does not parse.
pub type Parsed = Result<Arc<ParsedDocument>, roxmltree::Error>;

/// Parsed documents of imported files, kept from one evaluation to the next:
/// at most one per path, the last one parsed.
///
/// An entry is reused only when the file's bytes, read again, equal the text
/// it was parsed from ([`read_document`]): the file is still read, so the
/// read set records what it records without the entry, and parsing is a
/// function of the text. So an entry is never stale; it is only a parse
/// saved.
#[derive(Debug, Default)]
pub struct ImportDocuments {
    by_path: HashMap<OsString, Arc<ParsedDocument>>,
    /// Documents parsed into it, ever.
    parsed: u64,
    /// Reads that reused a kept document, ever.
    reused: u64,
}

impl ImportDocuments {
    /// How many documents are kept.
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// How many documents the evaluations it was handed to parsed.
    pub fn parsed(&self) -> u64 {
        self.parsed
    }

    /// How many reads by the evaluations it was handed to reused a kept
    /// document rather than parse.
    pub fn reused(&self) -> u64 {
        self.reused
    }

    /// The digest of the text the document kept for `path` was parsed from.
    #[cfg(test)]
    pub(crate) fn kept_digest(&self, path: &Path) -> Option<TextDigest> {
        self.by_path.get(path.as_os_str()).map(|kept| kept.digest())
    }
}

impl Observer {
    /// An observer that asks each query once per evaluation, with no
    /// documents to reuse.
    pub fn new() -> Self {
        Self::with_documents(ImportDocuments::default())
    }

    /// [`Observer::new`], reusing `documents`' parses;
    /// [`Self::finish_with_documents`] hands them back with the evaluation's
    /// own added.
    pub fn with_documents(documents: ImportDocuments) -> Self {
        Self {
            reads: RefCell::new(Vec::new()),
            counts: Cell::new(ReadCounts::default()),
            memo: Some(RefCell::new(Memo::default())),
            documents: RefCell::new(documents),
        }
    }

    /// An observer that asks the file system every query it is asked, however
    /// often, and parses every document it reads: the reference that
    /// [`Observer::new`]'s memo and [`ImportDocuments`] are held to.
    #[cfg(any(test, feature = "test-support"))]
    pub fn asking_every_time() -> Self {
        Self {
            memo: None,
            ..Self::new()
        }
    }

    /// The read set, the counts, and the documents: those the observer was
    /// made with, and those its evaluation parsed.
    pub fn finish_with_documents(self) -> (ReadSet, ReadCounts, ImportDocuments) {
        let documents = self.documents.take();
        let (reads, counts) = self.finish();
        (reads, counts, documents)
    }

    /// The memo's answer to a query, if it has one; `None` in an observer
    /// that asks every time.
    fn recall<T: Clone>(
        &self,
        table: impl FnOnce(&Memo) -> &HashMap<OsString, T>,
        path: &Path,
    ) -> Option<T> {
        let memo = self.memo.as_ref()?.borrow();
        table(&memo).get(path.as_os_str()).cloned()
    }

    /// Remember a query's answer, in an observer that remembers.
    fn remember<T: Clone>(
        &self,
        table: impl FnOnce(&mut Memo) -> &mut HashMap<OsString, T>,
        path: &Path,
        answer: &T,
    ) {
        if let Some(memo) = &self.memo {
            table(&mut memo.borrow_mut()).insert(path.as_os_str().to_owned(), answer.clone());
        }
    }

    /// The read set and the counts. A query asked twice with the same answer
    /// is one read. Asked twice with different answers (the file moved during
    /// the evaluation), it is two, and the read set can never validate again.
    ///
    /// The reads are sorted, so the read set is a set: the order an
    /// evaluation happened to ask its queries in (a walk over a `HashMap`,
    /// say) does not tell two equal read sets apart.
    pub fn finish(self) -> (ReadSet, ReadCounts) {
        let Self { reads, counts, .. } = self;
        let mut reads = reads.into_inner();
        reads.sort();
        reads.dedup();
        (ReadSet { reads }, counts.get())
    }

    /// Another observer's reads and counts, as if this one had made them:
    /// what a caller that observes on several threads, each through an
    /// observer of its own, joins them with.
    pub fn absorb(&self, (reads, counts): (ReadSet, ReadCounts)) {
        self.reads.borrow_mut().extend(reads.reads);
        self.counts.set(self.counts.get() + counts);
    }

    fn record(&self, path: &Path, observation: Observation) {
        let mut counts = self.counts.get();
        match &observation {
            Observation::Text(_) => counts.opens += 1,
            Observation::Kind(_) | Observation::LinkKind(_) | Observation::Stamp(_) => {
                counts.stats += 1;
            }
            Observation::Listing(_) => counts.listings += 1,
            Observation::Canonical(_) => counts.canonicalizations += 1,
        }
        self.counts.set(counts);
        self.push(path, observation);
    }

    /// Record a whole-file read whose observation is the file's stamp
    /// ([`read_stamped`]): an open, not a `stat`.
    fn record_open(&self, path: &Path, observation: Observation) {
        let mut counts = self.counts.get();
        counts.opens += 1;
        self.counts.set(counts);
        self.push(path, observation);
    }

    fn push(&self, path: &Path, observation: Observation) {
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

impl FromIterator<Read> for ReadSet {
    /// The reads as a set: sorted, each once.
    fn from_iter<I: IntoIterator<Item = Read>>(reads: I) -> Self {
        let mut reads: Vec<Read> = reads.into_iter().collect();
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
    ///
    /// A racy stamp ([`FileStamp::racy`]) holds only while the file's bytes
    /// are the ones the read that took it saw, which it carries a digest of
    /// when a whole-file read took it; otherwise it never holds: the file
    /// may have changed since within the same timestamp tick. The stamp of
    /// a file a whole-file read could not read ([`FileStamp::unreadable`])
    /// is asked again as that read ([`read_stamped_now`]): asked as a
    /// `stat`, it would succeed, and never hold.
    pub fn still_holds(&self) -> bool {
        match &self.observation {
            Observation::Stamp(Ok(stamp)) if stamp.unreadable => {
                same_unreadable(stamp, &read_stamped_now(&self.path))
            }
            Observation::Stamp(Ok(stamp)) if stamp.racy => stamp
                .digest
                .is_some_and(|digest| racy_content_now(&self.path, digest).0),
            _ => answer_now(&self.path, &self.observation) == self.observation,
        }
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
    /// The file's identity and when it last changed (`stat`), for a reader
    /// that keeps neither its bytes nor their digest: one that reads a large
    /// binary file ([`read_stamped`]), or reads it through a cache keyed by
    /// its stamp ([`stamp`]).
    Stamp(Result<FileStamp, Failure>),
}

impl Observation {
    /// Whether this is a stamp taken within [`STAMP_RACY_WINDOW`] of the
    /// file's last change, which no later observation can confirm.
    pub fn is_racy_stamp(&self) -> bool {
        matches!(self, Observation::Stamp(Ok(stamp)) if stamp.racy)
    }
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
        Observation::Stamp(_) => Observation::Stamp(stamp_of(path)),
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

/// How close to the time a stamp is taken a file's last change may lie for
/// the stamp to be racy: the coarsest local timestamp granularity in common
/// use (HFS+'s second, FAT's two), as the LSP's `Vfs` rules (git's
/// racy-timestamp rule).
pub const STAMP_RACY_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// A file's identity and its last changes, as `stat` reports them: the
/// device and inode, the size, and the modification and change times in
/// nanoseconds. The change time cannot be set from user space, so a rewrite
/// that keeps the size and resets the modification time still moves it;
/// it is `None` on a host whose `std` exposes none.
///
/// Two writes of the same size within one timestamp tick leave every field
/// the same, so a stamp taken within [`STAMP_RACY_WINDOW`] of the file's
/// last change, or on a host with no change time, is `racy`: it speaks for
/// the file only at the moment it was taken. A racy stamp a whole-file read
/// took ([`read_stamped`]) carries a digest of the bytes the read saw, and
/// holds again exactly while the file's bytes are those (git's rule for a
/// racy index entry: compare the contents); any other racy stamp never
/// holds again ([`Read::still_holds`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub len: u64,
    pub mtime_ns: i128,
    pub ctime_ns: Option<i128>,
    pub racy: bool,
    /// A 128-bit hash of the bytes the read that took this stamp saw
    /// ([`bytes_digest`]), recorded only for a racy stamp a whole-file read
    /// took.
    pub digest: Option<u128>,
    /// The whole-file read this stamp was taken for failed: the file could be
    /// `stat`ed and not opened or read ([`read_stamped`]). Such a read
    /// depends on the file staying unreadable, which only that read can say:
    /// a permission is no stamp field, and an I/O error can clear with none
    /// moving.
    pub unreadable: bool,
}

impl FileStamp {
    #[cfg(unix)]
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let nanos = |secs: i64, nsec: i64| i128::from(secs) * 1_000_000_000 + i128::from(nsec);
        let mtime_ns = nanos(meta.mtime(), meta.mtime_nsec());
        let ctime_ns = Some(nanos(meta.ctime(), meta.ctime_nsec()));
        FileStamp {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.size(),
            mtime_ns,
            ctime_ns,
            racy: racy(mtime_ns, ctime_ns),
            digest: None,
            unreadable: false,
        }
    }

    #[cfg(not(unix))]
    fn of(meta: &std::fs::Metadata) -> Self {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i128);
        FileStamp {
            dev: 0,
            ino: 0,
            len: meta.len(),
            mtime_ns,
            ctime_ns: None,
            racy: true,
            digest: None,
            unreadable: false,
        }
    }
}

/// A 128-bit hash of `bytes`, as [`TextDigest`] hashes a text: what a racy
/// stamp's read saw ([`FileStamp::digest`]).
pub fn bytes_digest(bytes: &[u8]) -> u128 {
    let half = |salt: u8| {
        let mut hasher = std::hash::DefaultHasher::new();
        salt.hash(&mut hasher);
        bytes.hash(&mut hasher);
        hasher.finish()
    };
    (u128::from(half(0)) << 64) | u128::from(half(1))
}

/// What [`read_stamped`] would record for `path` now, without recording it:
/// the stamp the open file had before it was read, the read's failure if it
/// failed after that, and for a racy stamp a digest of the bytes.
pub fn read_stamped_now(path: &Path) -> Observation {
    let (stamp, bytes) = stamped_bytes(path);
    Observation::Stamp(recorded_stamp(path, stamp, &bytes))
}

/// Whether `now`, a whole-file read's record ([`read_stamped_now`]), is the
/// file `recorded` was, still unreadable: the same stamp fields bar
/// raciness. The failing read is what an unreadable stamp rests on, as the
/// digest is for a readable racy one, so a stamp racy then or now holds
/// while the read keeps failing.
pub fn same_unreadable(recorded: &FileStamp, now: &Observation) -> bool {
    let Observation::Stamp(Ok(now)) = now else {
        return false;
    };
    now.unreadable
        && FileStamp {
            racy: recorded.racy,
            ..*now
        } == *recorded
}

/// The stamp a whole-file read of `path` records ([`read_stamped`]): the
/// open file's stamp, with a digest of the bytes read when it is racy; and
/// when the file could not be opened or read, its stamp marked
/// [`FileStamp::unreadable`] (taken by a `stat`, when it could not be
/// opened), or the `stat`'s failure, as [`stamp`] would record it.
fn recorded_stamp(
    path: &Path,
    stamp: Result<FileStamp, Failure>,
    bytes: &Result<Vec<u8>, Failure>,
) -> Result<FileStamp, Failure> {
    let unreadable = |stamp: FileStamp| FileStamp {
        unreadable: true,
        ..stamp
    };
    match (stamp, bytes) {
        (Ok(stamp), Ok(bytes)) if stamp.racy => Ok(FileStamp {
            digest: Some(bytes_digest(bytes)),
            ..stamp
        }),
        (Ok(stamp), Ok(_)) => Ok(stamp),
        (Ok(stamp), Err(_)) => Ok(unreadable(stamp)),
        (Err(_), _) => stamp_of(path).map(unreadable),
    }
}

/// Whether the file at `path`, read whole now, holds the bytes a racy stamp
/// recorded as `digest` ([`FileStamp::digest`]), and the stamp the open file
/// had before it was read: what a racy whole-file read is confirmed by.
pub fn racy_content_now(path: &Path, digest: u128) -> (bool, Result<FileStamp, Failure>) {
    let (stamp, bytes) = stamped_bytes(path);
    let same = bytes.is_ok_and(|bytes| bytes_digest(&bytes) == digest);
    (same, stamp)
}

/// Whether a file last changed at `mtime_ns` and `ctime_ns` is racy now:
/// either time within [`STAMP_RACY_WINDOW`] of the wall clock, or after it,
/// or no change time at all.
fn racy(mtime_ns: i128, ctime_ns: Option<i128>) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i128);
    let window = STAMP_RACY_WINDOW.as_nanos() as i128;
    match ctime_ns {
        Some(ctime) => now - mtime_ns < window || now - ctime < window,
        None => true,
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

#[allow(clippy::disallowed_methods)]
fn stamp_of(path: &Path) -> Result<FileStamp, Failure> {
    std::fs::metadata(path)
        .map(|meta| FileStamp::of(&meta))
        .map_err(|e| Failure::of(&e))
}

/// The file at `path`, opened and stamped through its descriptor, then read
/// whole: stamp first, so that a write landing after it moves the stamp.
#[allow(clippy::disallowed_methods)]
fn stamped_bytes(path: &Path) -> (Result<FileStamp, Failure>, Result<Vec<u8>, Failure>) {
    use std::io::Read as _;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return (Err(Failure::of(&e)), Err(Failure::of(&e))),
    };
    let stamp = match file.metadata() {
        Ok(meta) => FileStamp::of(&meta),
        Err(e) => return (Err(Failure::of(&e)), Err(Failure::of(&e))),
    };
    // Reserved fallibly, as `std::fs::read` does: a file too large to hold
    // fails the read with `OutOfMemory` rather than aborting the process.
    let mut bytes = Vec::new();
    if let Err(e) = bytes.try_reserve_exact(usize::try_from(stamp.len).unwrap_or(0)) {
        let e = std::io::Error::new(ErrorKind::OutOfMemory, e);
        return (Ok(stamp), Err(Failure::of(&e)));
    }
    match file.read_to_end(&mut bytes) {
        Ok(_) => (Ok(stamp), Ok(bytes)),
        Err(e) => (Ok(stamp), Err(Failure::of(&e))),
    }
}

/// [`std::fs::read_to_string`].
pub fn read_to_string(observer: &Observer, path: &Path) -> Result<String, Failure> {
    if let Some(answer) = observer.recall(|m| &m.texts, path) {
        return answer;
    }
    let result = read_text(path);
    observer.record(path, Observation::Text(text_digest(&result)));
    observer.remember(|m| &mut m.texts, path, &result);
    result
}

/// [`read_to_string`], parsed as XML: the read [`read_to_string`] records,
/// and the document `roxmltree` parses from its text.
///
/// The document is the observer's [`ImportDocuments`] entry for `path` when
/// that was parsed from exactly the bytes read, and is parsed (and kept
/// there, replacing any other) otherwise.
pub fn read_document(observer: &Observer, path: &Path) -> Result<Parsed, Failure> {
    if let Some(answer) = observer.recall(|m| &m.documents, path) {
        return answer;
    }
    let result = read_text(path).map(|text| observer.parse_document(path, text));
    let digest = match &result {
        Ok(Ok(parsed)) => Ok(parsed.digest()),
        Ok(Err((_, digest))) => Ok(*digest),
        Err(failure) => Err(failure.clone()),
    };
    observer.record(path, Observation::Text(digest));
    let result = result.map(|parsed| parsed.map_err(|(error, _)| error));
    observer.remember(|m| &mut m.documents, path, &result);
    result
}

impl Observer {
    /// The document parsed from `text`, the text of the file at `path`: the
    /// kept one when it was parsed from these bytes, else a new parse, kept
    /// in its place. A text that does not parse fails with its digest.
    fn parse_document(
        &self,
        path: &Path,
        text: String,
    ) -> Result<Arc<ParsedDocument>, (roxmltree::Error, TextDigest)> {
        let key = path.as_os_str();
        let reusing = self.memo.is_some();
        let mut documents = self.documents.borrow_mut();
        if reusing
            && let Some(kept) = documents.by_path.get(key)
            && kept.text() == text
        {
            let kept = Arc::clone(kept);
            documents.reused += 1;
            return Ok(kept);
        }
        let digest = TextDigest::of(&text);
        let parsed = Arc::new(ParsedDocument::parse(text, digest).map_err(|e| (e, digest))?);
        if reusing {
            documents.parsed += 1;
            documents
                .by_path
                .insert(key.to_owned(), Arc::clone(&parsed));
        }
        Ok(parsed)
    }
}

/// What `path` names, following symlinks: [`std::fs::metadata`].
pub fn kind(observer: &Observer, path: &Path) -> Result<EntryKind, Failure> {
    if let Some(answer) = observer.recall(|m| &m.kinds, path) {
        return answer;
    }
    let result = kind_of(path);
    observer.record(path, Observation::Kind(result.clone()));
    observer.remember(|m| &mut m.kinds, path, &result);
    result
}

/// What `path` itself is: [`std::fs::symlink_metadata`].
pub fn link_kind(observer: &Observer, path: &Path) -> Result<EntryKind, Failure> {
    if let Some(answer) = observer.recall(|m| &m.link_kinds, path) {
        return answer;
    }
    let result = link_kind_of(path);
    observer.record(path, Observation::LinkKind(result.clone()));
    observer.remember(|m| &mut m.link_kinds, path, &result);
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
    if let Some(answer) = observer.recall(|m| &m.listings, path) {
        return answer;
    }
    let result = listing_of(path);
    observer.record(path, Observation::Listing(listed(&result)));
    observer.remember(|m| &mut m.listings, path, &result);
    result
}

/// [`std::fs::canonicalize`].
pub fn canonicalize(observer: &Observer, path: &Path) -> Result<PathBuf, Failure> {
    if let Some(answer) = observer.recall(|m| &m.canonicals, path) {
        return answer;
    }
    let result = canonical_of(path);
    observer.record(path, Observation::Canonical(result.clone()));
    observer.remember(|m| &mut m.canonicals, path, &result);
    result
}

/// The file's [`FileStamp`]: [`std::fs::metadata`].
pub fn stamp(observer: &Observer, path: &Path) -> Result<FileStamp, Failure> {
    let result = stamp_of(path);
    observer.record(path, Observation::Stamp(result.clone()));
    result
}

/// [`std::fs::read`], recorded as the stamp the open file had before it was
/// read rather than as a digest of the bytes: for a large binary file that a
/// caller reads whole and need not hash. A write that lands after the stamp
/// moves it, so the stamp never speaks for bytes the read did not see. A
/// racy stamp, which cannot speak for them, carries a digest of the bytes
/// instead ([`FileStamp::digest`]). A read that fails after the stamp is
/// recorded with its failure.
pub fn read_stamped(observer: &Observer, path: &Path) -> Result<Vec<u8>, Failure> {
    let (stamp, bytes) = stamped_bytes(path);
    let recorded = recorded_stamp(path, stamp, &bytes);
    observer.record_open(path, Observation::Stamp(recorded));
    bytes
}

#[cfg(any(test, feature = "test-support"))]
pub mod soundness;

#[cfg(any(test, feature = "test-support"))]
pub mod reuse;

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

    /// A stamped read records the stamp the file had when it was read, which
    /// [`stamp`] reads alike, as one query. A file changed within
    /// [`STAMP_RACY_WINDOW`] of the stamp gives a racy one, which never
    /// holds, unchanged or not; past the window the stamp holds until a
    /// rewrite, at the same size, moves it. An absent file is recorded absent
    /// by both.
    #[test]
    fn a_stamped_read_records_a_stamp_a_rewrite_moves() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("A.dll");
        std::fs::write(&path, b"abcd").unwrap();
        let racy = Observer::new();
        assert!(stamp(&racy, &path).unwrap().racy);
        assert!(racy.finish().0.first_stale().is_some(), "a racy stamp held");

        std::thread::sleep(STAMP_RACY_WINDOW + std::time::Duration::from_millis(200));
        let observer = Observer::new();
        assert_eq!(read_stamped(&observer, &path).unwrap(), b"abcd");
        assert!(!stamp(&observer, &path).unwrap().racy);
        let missing = tmp.path().join("B.dll");
        assert!(read_stamped(&observer, &missing).unwrap_err().is_absent());
        assert!(stamp(&observer, &missing).unwrap_err().is_absent());
        let (reads, counts) = observer.finish();
        assert_eq!(reads.len(), 2, "{reads:?}");
        assert_eq!((counts.opens, counts.stats), (2, 2));
        assert_eq!(reads.first_stale(), None);
        std::fs::write(&path, b"wxyz").unwrap();
        assert!(reads.first_stale().is_some_and(|read| read.path == path));
    }

    /// A racy stamp a whole-file read took carries a digest of the bytes it
    /// saw, and holds exactly while the file holds them: through a rewrite of
    /// the same bytes, and not through a same-size rewrite within the window,
    /// which leaves every field a `stat` reports possibly the same.
    #[test]
    fn a_racy_whole_file_read_holds_while_its_bytes_do() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("A.dll");
        std::fs::write(&path, b"abcd").unwrap();
        let observer = Observer::new();
        assert_eq!(read_stamped(&observer, &path).unwrap(), b"abcd");
        let (reads, _) = observer.finish();
        let [read] = reads.reads() else {
            panic!("{reads:?}")
        };
        let Observation::Stamp(Ok(stamp)) = read.observation else {
            panic!("{read:?}")
        };
        assert!(stamp.racy, "precondition: the stamp is racy");
        assert_eq!(stamp.digest, Some(bytes_digest(b"abcd")));
        assert!(
            read.still_holds(),
            "a racy read of unchanged bytes was stale"
        );
        std::fs::write(&path, b"abcd").unwrap();
        assert!(
            read.still_holds(),
            "a rewrite of the same bytes made it stale"
        );
        std::fs::write(&path, b"wxyz").unwrap();
        assert!(!read.still_holds(), "a same-size rewrite left it standing");
        std::fs::remove_file(&path).unwrap();
        assert!(!read.still_holds(), "a deletion left it standing");
    }

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
                // `is_dir` and `is_file` of `sub` ask the same query, which
                // the observer asks once.
                stats: 2,
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

    /// An observer asks each query once: asked again, a query takes the first
    /// answer, while the reference observer asks the file system again and
    /// records both answers.
    #[test]
    fn a_query_asked_again_takes_its_first_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        let file = dir.join("a.props");
        std::fs::write(&file, "<A/>").unwrap();
        let ask = |observer: &Observer| {
            (
                read_to_string(observer, &file),
                kind(observer, &dir.join("b")),
                read_dir(observer, &dir).map(|entries| entries.len()),
            )
        };
        let once = Observer::new();
        let every_time = Observer::asking_every_time();
        let first = (ask(&once), ask(&every_time));
        std::fs::write(&file, "<B/>").unwrap();
        std::fs::create_dir(dir.join("b")).unwrap();
        assert_eq!(ask(&once), first.0, "the memo answers as it first did");
        assert_ne!(ask(&every_time), first.1, "the reference asks again");
        let (once, once_counts) = once.finish();
        let (every_time, every_time_counts) = every_time.finish();
        assert_eq!(
            once_counts,
            ReadCounts {
                opens: 1,
                stats: 1,
                listings: 1,
                canonicalizations: 0
            }
        );
        assert_eq!(
            every_time_counts,
            ReadCounts {
                opens: 2,
                stats: 2,
                listings: 2,
                canonicalizations: 0
            }
        );
        assert_eq!(once.len(), 3);
        assert_eq!(every_time.len(), 6, "each query recorded with both answers");
        assert!(
            once.first_stale().is_some(),
            "the next validation sees the move"
        );
    }

    /// A kept document is reused for the very bytes it was parsed from, and
    /// parsed again (and kept in its place) for any others; the read is
    /// recorded either way, as [`read_to_string`] records it.
    #[test]
    fn a_document_is_reused_only_for_its_own_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        let file = dir.join("a.props");
        std::fs::write(&file, "<A/>").unwrap();
        let evaluate = |documents: ImportDocuments| {
            let observer = Observer::with_documents(documents);
            let parsed = read_document(&observer, &file).unwrap().unwrap();
            let reference = Observer::new();
            read_to_string(&reference, &file).unwrap();
            let (reads, counts, documents) = observer.finish_with_documents();
            assert_eq!((reads, counts), reference.finish());
            (parsed, documents)
        };
        let (first, documents) = evaluate(ImportDocuments::default());
        assert_eq!(documents.len(), 1);
        let (again, documents) = evaluate(documents);
        assert!(
            Arc::ptr_eq(&first, &again),
            "the same bytes reuse the parse"
        );
        assert_eq!((documents.parsed(), documents.reused()), (1, 1));
        std::fs::write(&file, "<B/>").unwrap();
        let (changed, documents) = evaluate(documents);
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(changed.document().root_element().tag_name().name(), "B");
        assert_eq!(documents.len(), 1, "the new parse replaces the old");
        assert_eq!((documents.parsed(), documents.reused()), (2, 1));
        std::fs::write(&file, "<A/>").unwrap();
        let (back, _) = evaluate(documents);
        assert!(!Arc::ptr_eq(&first, &back), "only the last parse is kept");
        assert_eq!(back.document().root_element().tag_name().name(), "A");
    }

    /// A file that does not parse as XML records its read, fails as
    /// `roxmltree` fails on its text, and is not kept.
    #[test]
    fn a_malformed_document_records_its_read_and_is_not_kept() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().host_canonicalize().unwrap();
        let file = dir.join("a.props");
        std::fs::write(&file, "<A>").unwrap();
        let observer = Observer::new();
        let error = read_document(&observer, &file).unwrap().unwrap_err();
        assert_eq!(error, roxmltree::Document::parse("<A>").unwrap_err());
        let reference = Observer::new();
        read_to_string(&reference, &file).unwrap();
        let (reads, counts, documents) = observer.finish_with_documents();
        assert_eq!((reads, counts), reference.finish());
        assert!(documents.is_empty());
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
