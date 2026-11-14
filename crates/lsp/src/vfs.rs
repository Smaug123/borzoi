//! The one owner of source text (`docs/incremental-lsp-plan.md`, D1): the
//! client's open buffers, and what every other source held on disk when the
//! server last looked.
//!
//! **Generations.** The dispatch loop does its work for one message (or one
//! step of a `workspace/diagnostic` sweep) in a **generation**, which it opens
//! with [`Vfs::begin_generation`] and closes with [`Vfs::end_generation`]. The
//! first read of a disk file in a generation `stat`s it; it re-reads the file
//! only if the stamp moved, or if the stamp is racy (below). Every later read
//! in that generation returns the same `Arc<str>` without touching the disk.
//! So within a generation every read of a path returns the same text, and an
//! unchanged file costs one `stat` per generation and keeps its allocation,
//! which is what lets the parse cache and the cached folds compare texts by
//! pointer.
//!
//! Outside a generation (the server's work before its loop starts, and code
//! that drives the semantic layer directly, as unit tests do) nothing is
//! remembered: every read reads the file.
//!
//! **Stamp, then read.** A read opens the file, stamps the open file
//! (`fstat`), and only then reads it. A write that lands after the stamp moves
//! the file's stamp, so the next generation re-reads it. Stamping after the
//! read could record a new stamp beside old text, which nothing would ever
//! re-read.
//!
//! **Racy stamps.** A filesystem's timestamps tick at its own granularity, so
//! two same-size writes within one tick can leave the stamp unchanged. Git's
//! racy-timestamp rule closes that hole: each stamp records the generation's
//! time, and a stamp whose mtime or ctime lies within [`RACY_WINDOW`] of it is
//! **racy**, and is re-read in every generation until one more than the window
//! past it. Any write after a non-racy stamp was taken then carries a
//! timestamp after the stamp's, provided the filesystem's granularity is at
//! most the window and its timestamps come from the server's clock. The time
//! is a value the shell passes to [`Vfs::begin_generation`], read before any
//! stamp of the generation, so the rule can be tested with a generated clock.
//!
//! **Guarantee.** The first read of a path in a generation returns the buffer
//! if the client has the file open, and otherwise what the disk held when the
//! read stamped it. It holds on local filesystems whose timestamp granularity
//! is at most [`RACY_WINDOW`] and which move `ctime` on every write (Linux's
//! ext4, xfs, btrfs and tmpfs; APFS and HFS+; NTFS). It does not hold where
//! timestamps come from another machine's clock (NFS or SMB with skew past the
//! window), or where `ctime` is not maintained; there a watch event, which
//! drops the entry ([`Vfs::hint`]), still covers a write.
//!
//! Every disk call is counted per generation ([`ReadCounts`]), and
//! [`Vfs::end_generation`] sends the ended [`Generation`] to the log a caller
//! attached ([`Vfs::log_generations`]) and emits it as the
//! [`GENERATION_EVENT`] `tracing` event. The disk itself is a [`Disk`]: [`HostDisk`] in the server,
//! and a model with a generated clock in the tests.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::collections::hash_map;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use lsp_types::Uri;

use borzoi_msbuild::observed::{Failure, TextDigest};

use crate::observed::{
    EnvCounts, EvaluationCounts, GENERATION_EVENT, Generation, HostDisk, ReadCounts,
};
use crate::parses::ParseCounts;
use crate::paths::{lexically_normalize, path_dedup_key};
use crate::uri::UriExt as _;

/// A moment, in nanoseconds since the Unix epoch: a file's mtime or ctime, or
/// the time a generation began.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Nanos(pub i128);

impl Nanos {
    /// `time` as nanoseconds since the Unix epoch (negative before it).
    pub fn of(time: SystemTime) -> Nanos {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(after) => Nanos(after.as_nanos() as i128),
            Err(before) => Nanos(-(before.duration().as_nanos() as i128)),
        }
    }
}

/// How close to the time it was taken a stamp's mtime or ctime may be before
/// the stamp is racy: 2 s, the coarsest local timestamp granularity in common
/// use (FAT's).
pub const RACY_WINDOW: Duration = Duration::from_secs(2);

/// What identifies one version of a file without reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: Nanos,
    /// The inode change time, which user space cannot set, so a rewrite that
    /// keeps the size and resets the mtime still moves it. `None` where the
    /// host does not expose one: such a stamp can never rule a rewrite out,
    /// so it is always racy, and the file is re-read in every generation.
    pub ctime: Option<Nanos>,
}

impl Stamp {
    /// Whether this stamp, taken at `taken`, could still be shared by a later
    /// write: its mtime or ctime is within [`RACY_WINDOW`] of `taken`, or
    /// after it, or it has no ctime.
    pub fn is_racy(&self, taken: Nanos) -> bool {
        let window = RACY_WINDOW.as_nanos() as i128;
        let recent = |time: Nanos| time.0 + window >= taken.0;
        recent(self.mtime) || self.ctime.is_none_or(recent)
    }
}

/// The file-system calls a [`Vfs`] makes.
pub trait Disk {
    /// The stamp of the file at `path` (a path `stat`).
    fn stat(&self, path: &Path) -> io::Result<Stamp>;
    /// Open `path`, stamp the open file, then read all of it: the stamp
    /// describes a version no newer than the bytes.
    fn read(&self, path: &Path) -> io::Result<(Stamp, Vec<u8>)>;
}

/// Where the shell takes each generation's time from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Clock {
    /// The host's wall clock: the clock its local filesystems stamp files by.
    #[default]
    Host,
    /// The host's wall clock, read this far ahead. Only for a test whose disk
    /// does not change while the server runs: it makes the files the test
    /// wrote before starting the server read as older than [`RACY_WINDOW`],
    /// and so it would hide a racy stamp taken during the run.
    HostAhead(Duration),
    /// The host's wall clock, read this far behind. Only for a test: with the
    /// clock behind by more than [`RACY_WINDOW`], every stamp of a file
    /// written during the test is racy, so every generation re-reads it.
    HostBehind(Duration),
}

impl Clock {
    pub fn now(self) -> Nanos {
        let host = Nanos::of(SystemTime::now());
        match self {
            Clock::Host => host,
            Clock::HostAhead(ahead) => Nanos(host.0 + ahead.as_nanos() as i128),
            Clock::HostBehind(behind) => Nanos(host.0 - behind.as_nanos() as i128),
        }
    }
}

/// What the disk held for one path, as a [`DiskCache`] last saw it.
#[derive(Debug)]
enum DiskEntry {
    /// The file exists.
    Present {
        stamp: Stamp,
        /// The time of the generation that took `stamp`.
        taken: Nanos,
        /// The text without a leading byte-order mark, or why the bytes are
        /// no text (they are not UTF-8).
        text: Result<Arc<str>, Failure>,
        /// Whether the bytes began with a byte-order mark, which `text` has
        /// dropped.
        bom: bool,
    },
    /// A `stat` or an open found nothing at the path, failing so.
    Absent(Failure),
    /// The file could not be stamped or read for another reason (a
    /// permission, a directory), failing so. Re-read in every generation.
    Failed(Failure),
}

impl DiskEntry {
    fn text(&self) -> Option<Arc<str>> {
        match self {
            DiskEntry::Present { text, .. } => text.as_ref().ok().cloned(),
            DiskEntry::Absent(_) | DiskEntry::Failed(_) => None,
        }
    }

    /// What a whole-file read of the bytes this entry saw answers, as
    /// `borzoi_msbuild::observed::read_to_string` would record it.
    fn text_answer(&self) -> Result<TextDigest, Failure> {
        match self {
            DiskEntry::Present {
                text: Ok(text),
                bom: false,
                ..
            } => Ok(TextDigest::of(text)),
            DiskEntry::Present {
                text: Ok(text),
                bom: true,
                ..
            } => Ok(TextDigest::of(&format!("\u{feff}{text}"))),
            DiskEntry::Present {
                text: Err(failure), ..
            }
            | DiskEntry::Absent(failure)
            | DiskEntry::Failed(failure) => Err(failure.clone()),
        }
    }
}

/// What files held on disk when a generation last looked, each stamped, so a
/// later generation confirms an unchanged file with one `stat` (see the
/// module docs). The [`Vfs`] reads unopened sources through one; the
/// [`crate::workspace::Workspace`] confirms what its evaluations read through
/// another.
#[derive(Debug)]
pub struct DiskCache<D: Disk = HostDisk> {
    disk: D,
    /// What each path read held, keyed by the spelling it was read under,
    /// with the generation that last confirmed it.
    entries: RefCell<HashMap<PathBuf, (DiskEntry, u64)>>,
    /// The latest generation's number. Generation 0 is the work done before
    /// the first one begins.
    generation: u64,
    /// The open generation's time, `None` between generations.
    open: Option<Nanos>,
    /// What the current generation has observed so far.
    counts: Cell<ReadCounts>,
}

impl<D: Disk + Default> Default for DiskCache<D> {
    fn default() -> Self {
        Self::with_disk(D::default())
    }
}

impl<D: Disk> DiskCache<D> {
    /// A cache over `disk`, with nothing read.
    pub fn with_disk(disk: D) -> Self {
        DiskCache {
            disk,
            entries: RefCell::new(HashMap::new()),
            generation: 0,
            open: None,
            counts: Cell::new(ReadCounts::default()),
        }
    }

    /// `path`'s text on disk without a leading byte-order mark; `None` if it
    /// is absent, unreadable or not UTF-8. Within a generation, the same
    /// answer every time.
    pub fn disk_text(&self, path: &Path) -> Option<Arc<str>> {
        self.with_entry(path, DiskEntry::text)
    }

    /// What a whole-file read of `path` answers now, as
    /// `borzoi_msbuild::observed::read_to_string` records it: within a
    /// generation the same answer every time, and across generations
    /// confirmed by the file's stamp as [`Self::disk_text`] is.
    pub fn text_answer(&self, path: &Path) -> Result<TextDigest, Failure> {
        self.with_entry(path, DiskEntry::text_answer)
    }

    /// `answer` of the entry for `path`: outside a generation a fresh read,
    /// inside one the entry this generation confirmed or read.
    fn with_entry<R>(&self, path: &Path, answer: impl FnOnce(&DiskEntry) -> R) -> R {
        let Some(now) = self.open else {
            return answer(&self.read(path, Nanos(0), None));
        };
        let mut entries = self.entries.borrow_mut();
        match entries.get_mut(path) {
            Some((entry, checked)) => {
                if *checked != self.generation {
                    if let Some(fresh) = self.revalidate(path, now, entry) {
                        *entry = fresh;
                    }
                    *checked = self.generation;
                }
                answer(entry)
            }
            None => {
                let entry = self.read(path, now, None);
                let result = answer(&entry);
                entries.insert(path.to_path_buf(), (entry, self.generation));
                result
            }
        }
    }

    /// What `path` holds now, given what an earlier generation saw there:
    /// `None` when a `stat` confirms `previous`, otherwise a fresh entry.
    fn revalidate(&self, path: &Path, now: Nanos, previous: &DiskEntry) -> Option<DiskEntry> {
        match previous {
            DiskEntry::Present {
                stamp, taken, text, ..
            } if !stamp.is_racy(*taken) => match self.stat(path) {
                Ok(current) if current == *stamp => None,
                Ok(_) => Some(self.read(path, now, text.as_ref().ok())),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    Some(DiskEntry::Absent(Failure::of(&err)))
                }
                Err(_) => Some(self.read(path, now, None)),
            },
            DiskEntry::Present { text, .. } => Some(self.read(path, now, text.as_ref().ok())),
            DiskEntry::Absent(_) => match self.stat(path) {
                Ok(_) => Some(self.read(path, now, None)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(_) => Some(self.read(path, now, None)),
            },
            DiskEntry::Failed(_) => Some(self.read(path, now, None)),
        }
    }

    /// `stat` `path`, counted.
    fn stat(&self, path: &Path) -> io::Result<Stamp> {
        let mut counts = self.counts.get();
        counts.stats += 1;
        self.counts.set(counts);
        self.disk.stat(path)
    }

    /// Read `path` (stamp, then read), counted, as an entry stamped at `now`.
    /// When the text is `old`'s, `old`'s allocation is kept, so an unchanged
    /// file keeps its pointer identity across a racy re-read.
    fn read(&self, path: &Path, now: Nanos, old: Option<&Arc<str>>) -> DiskEntry {
        let mut counts = self.counts.get();
        counts.opens += 1;
        self.counts.set(counts);
        match self.disk.read(path) {
            Ok((stamp, bytes)) => {
                let text = borzoi_msbuild::observed::text_answer(Ok(bytes));
                let bom = text
                    .as_deref()
                    .is_ok_and(|text| text.starts_with('\u{feff}'));
                let text = text.map(|text| {
                    let text = crate::disk_text::without_bom(text);
                    match old {
                        Some(old) if **old == *text => Arc::clone(old),
                        _ => Arc::from(text),
                    }
                });
                DiskEntry::Present {
                    stamp,
                    taken: now,
                    text,
                    bom,
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                DiskEntry::Absent(Failure::of(&err))
            }
            Err(err) => DiskEntry::Failed(Failure::of(&err)),
        }
    }

    /// Forget what the disk held at every remembered spelling path-equal to
    /// one of `paths`, so the next read re-reads it whatever its stamp says.
    pub fn hint<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let keys: std::collections::HashSet<String> = paths
            .into_iter()
            .map(|path| path_dedup_key(&lexically_normalize(path)))
            .collect();
        if keys.is_empty() {
            return;
        }
        self.entries
            .borrow_mut()
            .retain(|spelling, _| !keys.contains(&path_dedup_key(&lexically_normalize(spelling))));
    }

    /// Start a new generation at time `now`, with nothing observed yet. `now`
    /// must be read before any stamp the generation takes, as the racy rule
    /// assumes.
    pub fn begin_generation(&mut self, now: Nanos) {
        self.generation += 1;
        self.open = Some(now);
        self.counts.set(ReadCounts::default());
    }

    /// End the current generation, returning its number and what it
    /// observed.
    pub fn end_generation(&mut self) -> (u64, ReadCounts) {
        self.open = None;
        (self.generation, self.counts.get())
    }

    /// The open generation's number, or `None` between generations.
    pub fn generation(&self) -> Option<u64> {
        self.open.map(|_| self.generation)
    }

    /// The open generation's time, or `None` between generations.
    pub fn now(&self) -> Option<Nanos> {
        self.open
    }

    /// What the current generation has observed so far.
    pub fn generation_counts(&self) -> ReadCounts {
        self.counts.get()
    }

    /// The disk the cache reads.
    pub fn disk(&self) -> &D {
        &self.disk
    }
}

/// The client's open buffers, and what every other source held on disk.
#[derive(Debug, Default)]
pub struct Vfs<D: Disk = HostDisk> {
    /// The client's text for each open document, keyed by the URI it was
    /// opened under. Whatever it holds is what the client's positions count,
    /// so it is never touched.
    buffers: HashMap<Uri, Arc<str>>,
    /// The open `file:` documents by [`path_dedup_key`] of their lexically
    /// normalised path, in the order they were opened: the buffer a path
    /// reads is the first of them.
    buffer_paths: HashMap<String, Vec<Uri>>,
    /// What every unopened source read held on disk.
    disk: DiskCache<D>,
    /// Where each ended generation is sent, if a caller asked for them
    /// ([`Self::log_generations`]).
    log: Option<Sender<Generation>>,
}

impl FromIterator<(Uri, String)> for Vfs {
    /// A `Vfs` whose open buffers are `buffers`.
    fn from_iter<I: IntoIterator<Item = (Uri, String)>>(buffers: I) -> Self {
        let mut vfs = Vfs::new();
        for (uri, text) in buffers {
            vfs.insert(uri, text);
        }
        vfs
    }
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<D: Disk> Vfs<D> {
    /// A `Vfs` over `disk`, with nothing open and nothing read.
    pub fn with_disk(disk: D) -> Self {
        Vfs {
            buffers: HashMap::new(),
            buffer_paths: HashMap::new(),
            disk: DiskCache::with_disk(disk),
            log: None,
        }
    }

    /// The [`Self::buffer_paths`] key of a `file:` URI.
    fn buffer_key(uri: &Uri) -> Option<String> {
        let path = uri.to_file_path().ok()?;
        Some(path_dedup_key(&lexically_normalize(&path)))
    }

    /// Record the client's text for `uri`.
    pub fn insert(&mut self, uri: Uri, text: String) {
        if !self.buffers.contains_key(&uri)
            && let Some(key) = Self::buffer_key(&uri)
        {
            self.buffer_paths.entry(key).or_default().push(uri.clone());
        }
        self.buffers.insert(uri, Arc::from(text));
    }

    /// Forget the client's text for `uri`, returning it.
    pub fn remove(&mut self, uri: &Uri) -> Option<Arc<str>> {
        let text = self.buffers.remove(uri)?;
        if let Some(key) = Self::buffer_key(uri)
            && let hash_map::Entry::Occupied(mut uris) = self.buffer_paths.entry(key)
        {
            uris.get_mut().retain(|open| open != uri);
            if uris.get().is_empty() {
                uris.remove();
            }
        }
        Some(text)
    }

    /// The client's text for `uri`, if it has the document open under exactly
    /// that URI.
    pub fn get(&self, uri: &Uri) -> Option<&Arc<str>> {
        self.buffers.get(uri)
    }

    pub fn contains_key(&self, uri: &Uri) -> bool {
        self.buffers.contains_key(uri)
    }

    /// The open documents and their text.
    pub fn iter(&self) -> hash_map::Iter<'_, Uri, Arc<str>> {
        self.buffers.iter()
    }

    /// The URIs of the open documents.
    pub fn keys(&self) -> hash_map::Keys<'_, Uri, Arc<str>> {
        self.buffers.keys()
    }

    /// The text of the file at `path`: the open buffer whose path is
    /// path-equal to it, else the file on disk ([`Self::disk_text`]).
    ///
    /// Buffer lookup is by **platform path equality**, not by exact `Uri`
    /// match: on case-insensitive filesystems (Windows, macOS) the client may
    /// open `lib.fs` while the project lists `Lib.fs`, and the buffer must
    /// still win over disk. It is the rule
    /// [`crate::workspace::Workspace::owning_project`] uses, so an open buffer
    /// that decides project ownership also supplies the text that is folded.
    pub fn text_of(&self, path: &Path) -> Option<Arc<str>> {
        if let Some(text) = self.buffer_uri(path).and_then(|uri| self.buffers.get(uri)) {
            return Some(Arc::clone(text));
        }
        self.disk_text(path)
    }

    /// The open document whose buffer [`Self::text_of`] reads for `path`: of
    /// those whose path is path-equal to it, the first opened. A location
    /// computed from that text must carry this URI.
    pub fn buffer_uri(&self, path: &Path) -> Option<&Uri> {
        let key = path_dedup_key(&lexically_normalize(path));
        self.buffer_paths.get(&key).and_then(|uris| uris.first())
    }

    /// A document's current text: the buffer the client has open under
    /// exactly `uri`, else its path's text on disk ([`Self::disk_text`]).
    /// `None` for a non-`file:` URI that is not open, or an unreadable path.
    pub fn document_text(&self, uri: &Uri) -> Option<Arc<str>> {
        if let Some(text) = self.buffers.get(uri) {
            return Some(Arc::clone(text));
        }
        let path = uri.to_file_path().ok()?;
        self.disk_text(&path)
    }

    /// `path`'s text on disk without a leading byte-order mark, the document
    /// a client would hold for it ([`crate::disk_text`]); `None` if it is
    /// absent, unreadable or not UTF-8. Within a generation, the same answer
    /// every time (see the module docs).
    pub fn disk_text(&self, path: &Path) -> Option<Arc<str>> {
        self.disk.disk_text(path)
    }

    /// Watch events named `paths`: forget what the disk held at each, so the
    /// next read re-reads it whatever its stamp says. That covers a write the
    /// stamp cannot see (see the module docs). Every remembered spelling that
    /// is path-equal to one of `paths` is forgotten.
    pub fn hint<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        self.disk.hint(paths);
    }

    /// Start a new generation at time `now`, with nothing observed yet. `now`
    /// must be read before any stamp the generation takes, as the racy rule
    /// assumes.
    pub fn begin_generation(&mut self, now: Nanos) {
        self.disk.begin_generation(now);
    }

    /// Send every generation that ends from now on to `log`, replacing any
    /// log attached before.
    ///
    /// The channel is unbounded, and the dispatch loop never waits on it: the
    /// caller drains it, at one entry per message handled (and per sweep
    /// step). A receiver that hangs up stops the logging.
    pub fn log_generations(&mut self, log: Sender<Generation>) {
        self.log = Some(log);
    }

    /// End the current generation, in which no project evaluation was read,
    /// no file's owner was looked for, nothing was parsed and no env was
    /// read.
    pub fn end_generation(&mut self) -> Generation {
        self.end_generation_with(
            EvaluationCounts::default(),
            borzoi_msbuild::observed::ReadCounts::default(),
            ParseCounts::default(),
            EnvCounts::default(),
        )
    }

    /// End the current generation, in which reading project evaluations
    /// cost `evaluations`, finding the projects that own files cost
    /// `ownership`, the parse memo did `parses` and reading assembly envs
    /// cost `envs`: send it to the log, if one is attached, emit its counts
    /// as [`GENERATION_EVENT`], and return it.
    pub fn end_generation_with(
        &mut self,
        evaluations: EvaluationCounts,
        ownership: borzoi_msbuild::observed::ReadCounts,
        parses: ParseCounts,
        envs: EnvCounts,
    ) -> Generation {
        let (number, counts) = self.disk.end_generation();
        let generation = Generation {
            number,
            counts,
            evaluations,
            ownership,
            parses,
            envs,
        };
        if let Some(log) = &self.log
            && log.send(generation).is_err()
        {
            // The receiver hung up: nobody reads the log any more.
            self.log = None;
        }
        let checks = evaluations.validation;
        tracing::debug!(
            name: GENERATION_EVENT,
            generation = generation.number,
            opens = generation.counts.opens,
            stats = generation.counts.stats,
            evaluated = evaluations.evaluated,
            v_opens = checks.opens,
            v_stats = checks.stats,
            v_listings = checks.listings,
            v_canonicalizations = checks.canonicalizations,
            o_listings = ownership.listings,
            o_stats = ownership.stats,
            parsed = parses.parsed,
            parses_reused = parses.reused,
            envs_built = envs.built,
            av_opens = envs.validation.opens,
            av_stats = envs.validation.stats,
            av_listings = envs.validation.listings,
            av_canonicalizations = envs.validation.canonicalizations,
        );
        generation
    }

    /// The open generation's number, or `None` between generations.
    pub fn generation(&self) -> Option<u64> {
        self.disk.generation()
    }

    /// What the current generation has observed so far.
    pub fn generation_counts(&self) -> ReadCounts {
        self.disk.generation_counts()
    }
}

#[cfg(test)]
mod tests;
