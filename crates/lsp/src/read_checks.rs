//! Confirming recorded reads against the disk, once per generation, by
//! stamps (`docs/incremental-lsp-plan.md`, D1 and D3).
//!
//! A project evaluation records every file-system query it made with the
//! answer it got ([`borzoi_msbuild::observed::ReadSet`]). Before a cached
//! evaluation is served in a new generation, each of its reads must still get
//! the answer recorded. Asking every query again costs a `realpath` or a
//! directory listing apiece, which on a real project is most of a keystroke;
//! so a [`Check`] remembers, the first time it is confirmed, a **witness**:
//! the stamps of the directories whose entries decide the answer, taken
//! before the query was asked. While every one of those directories' stamps
//! is unchanged, the answer is too, and confirming it costs the `stat`s,
//! which reads that share a directory share within a generation.
//!
//! Why a directory's stamp decides these answers: creating, removing or
//! renaming an entry moves its directory's mtime and ctime, and so does
//! replacing a symlink (its target cannot be changed in place). `stat`ing a
//! directory resolves its path, so the stamp also says which directory the
//! path leads to now: a symlink re-pointed anywhere above it changes the
//! inode the stamp names. So, while the stamp of `d` holds,
//!
//! - **a listing** of `d` holds (its entries and their types), when it read
//!   every entry: a listing that failed at some entry can recover with no
//!   entry changing;
//! - **what a path names**, following symlinks or not, whether it is there,
//!   and **its canonical spelling** hold while the stamp of every directory
//!   its resolution looks an entry up in holds: each step of the resolution
//!   is an entry of the directory it is looked up in, and none can change
//!   without moving that directory's stamp. `ReadChecks::resolution` walks
//!   it as the kernel does (following symlinks, a link's target component by
//!   component, and taking each `..` from the directory reached), and a
//!   listing that found its directory absent is witnessed the same way.
//!
//! A relative path has no witness, and neither has an `lstat` of a path not
//! spelt as its components: a trailing separator makes it follow a final
//! symlink.
//!
//! Only a fact has a witness ([`a_fact`]): an answer, or an absence. A
//! read that failed otherwise (a permission, an I/O error, a listing that
//! failed at an entry) can recover with nothing on disk changing, and is
//! asked in every generation.
//!
//! Whole-file reads are confirmed by the file's own stamp
//! ([`DiskCache::text_answer`]). A read that failed for any reason but
//! absence is asked again in every generation, as is one whose resolution
//! met anything but an answer the walk understands.
//!
//! **Within a generation** a directory is `stat`ed once, so a check holds as
//! of the moment its directories were stamped in that generation, as a
//! [`DiskCache`] read holds as of its file's stamp; a change after that
//! moment moves a stamp, and the next generation asks again.
//!
//! **Racy stamps.** A witness any of whose stamps lies within
//! [`crate::vfs::RACY_WINDOW`] of the generation that took it is not
//! trusted: the read is asked again (and the witness taken again) in every
//! generation until it is old, as [`crate::vfs`] does for files. The
//! generated-clock model in this module's tests holds the witnesses to that,
//! at timestamp granularities the host does not have.
//!
//! **A witness belongs to a read, not to the reader that recorded it.** It
//! says that while its directories keep their stamps, the query gets the
//! answer it got when the witness was taken, and the witness is kept only if
//! that answer was the recorded one. Nothing in that names the evaluation or
//! the env that recorded the read: any read with the same path, spelt the
//! same, and the same recorded answer is confirmed by it exactly as far as
//! the read it was taken for. So a checker hands out one [`Check`] per
//! distinct read ([`ReadChecks::check`]), shared by every holder of that
//! read while any holds it: a project evaluated again, whose reads are
//! mostly those of the evaluation it replaces, starts with that
//! evaluation's witnesses, and an env whose resolution read the project's
//! evaluation shares the evaluation's. A shared witness is the same value
//! a fresh check would take, carried with its stamps, the time of the
//! generation that took them and the watch events it had seen, so the racy
//! rule and the hints judge it as they would have judged it for the read
//! it was taken for; and it names its checker, so no other checker's
//! directories can confirm it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

#[cfg(test)]
use borzoi_msbuild::observed::EntryKind;
use borzoi_msbuild::observed::{Failure, FileStamp, Listed, Observation, Read, ReadCounts};

use crate::observed::HostDisk;
use crate::paths::lexically_normalize;
use crate::vfs::{Disk, DiskCache, Nanos, Stamp};

/// Whether a query's answer is a fact about what the disk holds: an answer
/// with nothing in it that failed, or the absence of the path. Any other
/// failure (a permission, an I/O error, a listing that failed at an entry)
/// can clear with nothing on disk changing, so neither a witness nor the
/// immutability of an installed version may speak for it.
pub fn a_fact(observation: &Observation) -> bool {
    fn answered_or_absent<T>(answer: &Result<T, Failure>) -> bool {
        answer.as_ref().err().is_none_or(Failure::is_absent)
    }
    match observation {
        Observation::Text(answer) => answered_or_absent(answer),
        Observation::Kind(answer) | Observation::LinkKind(answer) => answered_or_absent(answer),
        Observation::Listing(Ok(entries)) => entries
            .iter()
            .all(|entry| matches!(entry, Listed::Entry { kind: Ok(_), .. })),
        Observation::Listing(Err(failure)) => failure.is_absent(),
        Observation::Canonical(answer) => answered_or_absent(answer),
        Observation::Stamp(answer) => {
            answered_or_absent(answer)
                && !observation.is_racy_stamp()
                && !answer.as_ref().is_ok_and(|stamp| stamp.unreadable)
        }
    }
}

/// How many symlinks a walk follows before it gives up, as `ELOOP` does.
const MAX_LINKS: usize = 40;

/// What one directory entry is, not following it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryAt {
    Dir,
    /// A symlink, and its target as written.
    Link(PathBuf),
    /// Anything else (a file, a socket).
    Other,
}

/// The file-system calls a [`ReadChecks`] makes beyond a [`Disk`]'s.
pub trait Probe: Disk {
    /// What the entry at `path` is, not following it (`lstat`, and
    /// `readlink` for a symlink).
    fn entry(&self, path: &Path) -> io::Result<EntryAt>;
    /// `query`'s query asked at `path` now, with its answer, as
    /// `borzoi_msbuild::observed` would record it.
    fn answer(&self, path: &Path, query: &Observation) -> Observation;
    /// The file at `path`, read whole now: its stamp, taken before the read,
    /// if its bytes are the ones `digest` names ([`FileStamp::digest`]), and
    /// `None` otherwise. A disk that cannot say confirms nothing.
    fn racy_content(&self, _path: &Path, _digest: u128) -> Option<FileStamp> {
        None
    }
    /// What a whole-file read of `path` records now
    /// (`borzoi_msbuild::observed::read_stamped_now`): how the stamp of a
    /// file such a read could not read is asked again. A disk that cannot
    /// read answers as a `stat` would.
    fn read_stamped(&self, path: &Path, query: &Observation) -> Observation {
        self.answer(path, query)
    }
}

/// One recorded read, and the witness that confirmed it last: a handle to
/// what its checker holds for that read ([`ReadChecks::check`]), shared with
/// every other holder of an equal read.
#[derive(Clone, Debug)]
pub struct Check {
    shared: Rc<Shared>,
}

/// What a checker knows about one distinct read.
#[derive(Debug)]
struct Shared {
    read: Read,
    witness: RefCell<Option<Witness>>,
    /// For a racy stamp a whole-file read took: a stamp of the file, not
    /// racy, taken before a read that found the bytes the racy one saw.
    /// While the file keeps it, the read holds with no read of the file
    /// ([`ReadChecks::racy_stamp_holds`]).
    confirmed: Cell<Option<FileStamp>>,
}

impl Shared {
    fn new(read: Read) -> Shared {
        Shared {
            read,
            witness: RefCell::new(None),
            confirmed: Cell::new(None),
        }
    }
}

impl Check {
    /// A check of `read` that shares nothing: its first confirmation asks
    /// the query again.
    #[cfg(test)]
    pub fn new(read: Read) -> Check {
        Check {
            shared: Rc::new(Shared::new(read)),
        }
    }

    pub fn read(&self) -> &Read {
        &self.shared.read
    }

    /// Whether `self` and `other` are one shared check.
    #[cfg(test)]
    pub fn shares(&self, other: &Check) -> bool {
        Rc::ptr_eq(&self.shared, &other.shared)
    }
}

/// The stamps of the directories that decide a read's answer, and the time
/// of the generation that took them, before the query was asked.
#[derive(Clone, Debug)]
struct Witness {
    /// The checker that took it ([`ReadChecks::id`]), whose directories
    /// `dirs` indexes.
    checker: u64,
    /// Each directory as its index in [`ReadChecks::dirs`], and its stamp.
    dirs: Vec<(usize, Stamp)>,
    taken: Nanos,
    /// The checker's [`ReadChecks::hints`] when it was taken: a directory
    /// hinted since ([`Dir::hinted`]) speaks for nothing it witnessed.
    hints: u64,
}

/// One directory a witness names: its path, and its stamp (or `None` where
/// it has none: it is absent, or cannot be `stat`ed) as the generation that
/// last `stat`ed it saw it.
#[derive(Debug)]
struct Dir {
    path: PathBuf,
    stamp: Option<Stamp>,
    checked: u64,
    /// The [`ReadChecks::hints`] count of the last watch event that named
    /// this directory or an entry of it, 0 if none has.
    hinted: u64,
}

/// The identity the next [`ReadChecks`] gets.
static NEXT_READ_CHECKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Confirms [`Check`]s for the open generation.
#[derive(Debug)]
pub struct ReadChecks<D: Probe = HostDisk> {
    /// No two checkers share one: a [`Check`]'s witness names directories
    /// by their index in the checker that took it, so a holder of checks
    /// confirms them only through that checker ([`Self::id`]).
    id: u64,
    /// Whole-file reads, confirmed by the file's stamp.
    files: DiskCache<D>,
    /// Every directory a witness has named, `stat`ed at most once per
    /// generation; witnesses name them by index, so confirming one looks
    /// nothing up by path.
    dirs: RefCell<Vec<Dir>>,
    /// The index of each directory in `dirs`.
    dir_ids: RefCell<HashMap<PathBuf, usize>>,
    /// The queries asked this generation beyond `files`' reads.
    counts: Cell<ReadCounts>,
    /// How many watch events have hinted the checker ([`Self::hint`]).
    hints: Cell<u64>,
    /// The checks this checker has handed out ([`Self::check`]), by a hash
    /// of their reads, while something holds them.
    shared: RefCell<HashMap<u64, Vec<Weak<Shared>>>>,
    /// How many entries `shared` holds, released ones included, and how
    /// many were held when it was last swept: it is swept once it holds
    /// twice that, so the released entries it keeps stay bounded by the
    /// held ones.
    shared_entries: Cell<usize>,
    shared_held: Cell<usize>,
}

impl<D: Probe + Default> Default for ReadChecks<D> {
    fn default() -> Self {
        Self::with_disk(D::default())
    }
}

impl<D: Probe> ReadChecks<D> {
    pub fn with_disk(disk: D) -> Self {
        ReadChecks {
            id: NEXT_READ_CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            files: DiskCache::with_disk(disk),
            dirs: RefCell::new(Vec::new()),
            dir_ids: RefCell::new(HashMap::new()),
            counts: Cell::new(ReadCounts::default()),
            hints: Cell::new(0),
            shared: RefCell::new(HashMap::new()),
            shared_entries: Cell::new(0),
            shared_held: Cell::new(0),
        }
    }

    /// The check of `read`: the one this checker already handed out for an
    /// equal read (the same path, spelt the same, and the same answer), if
    /// something still holds it, and otherwise a new one. A held check
    /// carries the witness its last confirmation took, which confirms
    /// `read` exactly as far as it confirms the read it was taken for (see
    /// the module docs), so a read set recorded again starts with the
    /// witnesses of the reads it shares with any read set still held.
    pub fn check(&self, read: &Read) -> Check {
        let mut hasher = std::hash::DefaultHasher::new();
        read.hash(&mut hasher);
        let hash = hasher.finish();
        let mut shared = self.shared.borrow_mut();
        let bucket = shared.entry(hash).or_default();
        let before = bucket.len();
        bucket.retain(|entry| entry.strong_count() > 0);
        let mut entries = self.shared_entries.get() - (before - bucket.len());
        if let Some(held) = bucket
            .iter()
            .filter_map(Weak::upgrade)
            .find(|held| held.read == *read)
        {
            self.shared_entries.set(entries);
            return Check { shared: held };
        }
        let check = Check {
            shared: Rc::new(Shared::new(read.clone())),
        };
        bucket.push(Rc::downgrade(&check.shared));
        entries += 1;
        if entries > 2 * self.shared_held.get().max(64) {
            shared.retain(|_, bucket| {
                bucket.retain(|entry| entry.strong_count() > 0);
                !bucket.is_empty()
            });
            entries = shared.values().map(Vec::len).sum();
            self.shared_held.set(entries);
        }
        self.shared_entries.set(entries);
        check
    }

    /// Start a generation at `now` ([`DiskCache::begin_generation`]).
    pub fn begin_generation(&mut self, now: Nanos) {
        self.files.begin_generation(now);
        self.counts.set(ReadCounts::default());
    }

    /// End the generation, returning what confirming reads cost it.
    pub fn end_generation(&mut self) -> ReadCounts {
        let (_, files) = self.files.end_generation();
        let counts = self.counts.get();
        ReadCounts {
            opens: files.opens + counts.opens,
            stats: files.stats + counts.stats,
            ..counts
        }
    }

    /// The open generation's number, or `None` between generations.
    pub fn generation(&self) -> Option<u64> {
        self.files.generation()
    }

    /// Which checker this is: the witnesses of the checks it confirms name
    /// its directories, and only its.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// What confirming reads has cost the open generation so far, as
    /// [`Self::end_generation`] will report it.
    pub fn counts_so_far(&self) -> ReadCounts {
        let files = self.files.generation_counts();
        let counts = self.counts.get();
        ReadCounts {
            opens: files.opens + counts.opens,
            stats: files.stats + counts.stats,
            ..counts
        }
    }

    /// The open generation's time, or `None` between generations.
    pub fn now(&self) -> Option<Nanos> {
        self.files.now()
    }

    /// The stamp of the file at `path` now (a counted `stat`), or `None`
    /// when it has none.
    pub fn stamp(&self, path: &Path) -> Option<Stamp> {
        let mut counts = self.counts.get();
        counts.stats += 1;
        self.counts.set(counts);
        self.files.disk().stat(path).ok()
    }

    /// Watch events named `paths`: forget what the files there held, so the
    /// next check reads them again whatever their stamps say, and stop
    /// trusting every witness taken before now that names one of the paths
    /// or the directory holding it, so the next check of each such read asks
    /// its query again.
    ///
    /// That is what covers a file system whose stamps do not move (D1's "not
    /// covered") for the paths an event names: a file there is read again
    /// and compared, and what a path names, whether it is there, its
    /// canonical spelling and its directory's listing are asked again. A
    /// witness names a directory by the spelling its read used, which may
    /// lead through a symlink, so a named directory is matched by spelling
    /// ([`crate::paths::path_dedup_key`]) and by identity: the device and
    /// inode a `stat` of it finds now, against those its last stamp held. A
    /// directory that can no longer be `stat`ed under a witness's spelling
    /// fails that witness anyway.
    pub fn hint<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let paths: Vec<&Path> = paths.into_iter().collect();
        self.files.hint(paths.iter().copied());
        let named: Vec<PathBuf> = paths
            .iter()
            .flat_map(|path| {
                let path = lexically_normalize(path);
                let parent = path.parent().map(Path::to_path_buf);
                std::iter::once(path).chain(parent)
            })
            .collect();
        if named.is_empty() {
            return;
        }
        let spellings: std::collections::HashSet<String> = named
            .iter()
            .map(|path| crate::paths::path_dedup_key(path))
            .collect();
        let identities: std::collections::HashSet<(u64, u64)> = named
            .iter()
            .filter_map(|path| {
                let mut counts = self.counts.get();
                counts.stats += 1;
                self.counts.set(counts);
                self.files.disk().stat(path).ok()
            })
            .map(|stamp| (stamp.dev, stamp.ino))
            .collect();
        let hints = self.hints.get() + 1;
        self.hints.set(hints);
        for dir in self.dirs.borrow_mut().iter_mut() {
            let by_identity = dir
                .stamp
                .is_some_and(|stamp| identities.contains(&(stamp.dev, stamp.ino)));
            if by_identity || spellings.contains(&crate::paths::path_dedup_key(&dir.path)) {
                dir.hinted = hints;
            }
        }
    }

    /// Whether `check`'s read gets the answer recorded, now. Outside a
    /// generation, the query is asked.
    pub fn holds(&self, check: &Check) -> bool {
        let read = &check.shared.read;
        // A racy stamp speaks for the file only when it was taken; what it
        // saw is confirmed by the file's contents, when it says what they were.
        // The stamp of a file a whole-file read could not read is asked
        // again as that read: asked as a `stat`, it would succeed, and never
        // hold again with nothing changing. The read is what it rests on, so
        // whether the stamp was racy then or is now does not enter into it.
        if let Observation::Stamp(Ok(recorded)) = &read.observation
            && recorded.unreadable
        {
            let mut counts = self.counts.get();
            counts.opens += 1;
            self.counts.set(counts);
            let now = self
                .files
                .disk()
                .read_stamped(&read.path, &read.observation);
            return borzoi_msbuild::observed::same_unreadable(recorded, &now);
        }
        if let Observation::Stamp(Ok(recorded)) = &read.observation
            && recorded.racy
        {
            return self.racy_stamp_holds(check, recorded);
        }
        if let Observation::Text(recorded) = &read.observation {
            return self.files.text_answer(&read.path) == *recorded;
        }
        let (Some(generation), Some(now)) = (self.files.generation(), self.files.now()) else {
            return self.ask(read) == read.observation;
        };
        if self.witnessed(check, generation) {
            return true;
        }
        // Stamp, then ask: a change after the stamps moves them, so the next
        // generation asks again.
        let witness = self.deciders(read).and_then(|dirs| {
            dirs.into_iter()
                .map(|dir| {
                    let id = self.dir_id(dir);
                    self.dir_stamp(id, generation).map(|stamp| (id, stamp))
                })
                .collect::<Option<Vec<_>>>()
                .map(|dirs| Witness {
                    checker: self.id,
                    dirs,
                    taken: now,
                    hints: self.hints.get(),
                })
        });
        let holds = self.ask(read) == read.observation;
        *check.shared.witness.borrow_mut() = witness.filter(|_| holds);
        holds
    }

    /// Whether the file a racy stamp `recorded` was taken of still holds the
    /// bytes the read that took it saw (git's rule for a racy index entry):
    /// never, when the stamp carries no digest of them; otherwise, by a
    /// stamp of the file confirmed so earlier (a `stat`), or by reading the
    /// file again (an open). A read that finds them, under a stamp no longer
    /// racy, leaves that stamp to confirm the next check by: a write after
    /// it moves it.
    fn racy_stamp_holds(&self, check: &Check, recorded: &FileStamp) -> bool {
        let Some(digest) = recorded.digest else {
            return false;
        };
        let path = &check.shared.read.path;
        let mut counts = self.counts.get();
        if let Some(confirmed) = check.shared.confirmed.get() {
            counts.stats += 1;
            self.counts.set(counts);
            let now = self
                .files
                .disk()
                .answer(path, &check.shared.read.observation);
            if now == Observation::Stamp(Ok(confirmed)) {
                return true;
            }
            check.shared.confirmed.set(None);
        }
        let mut counts = self.counts.get();
        counts.opens += 1;
        self.counts.set(counts);
        match self.files.disk().racy_content(path, digest) {
            Some(now) => {
                if !now.racy {
                    check.shared.confirmed.set(Some(now));
                }
                true
            }
            None => false,
        }
    }

    /// List `dir` now, as a reader that will depend on the listing does,
    /// and return the read as a check (counted as a listing). Inside a
    /// generation the directory is stamped first (a `stat`, shared with
    /// every check that names it this generation), and when the listing is
    /// one that stamp decides (a fact, and an answer: see the module docs)
    /// the check is born with that stamp as its witness, so the next
    /// generation confirms it by the `stat` alone. A change after the stamp
    /// moves it, so the witness is never newer than what the listing saw.
    pub fn list(&self, dir: &Path) -> Check {
        let query = Read {
            path: dir.to_path_buf(),
            observation: Observation::Listing(Ok(Vec::new())),
        };
        let stamped = match (self.files.generation(), self.files.now()) {
            (Some(generation), Some(now)) if dir.is_absolute() => {
                let id = self.dir_id(dir.to_path_buf());
                self.dir_stamp(id, generation).map(|stamp| (id, stamp, now))
            }
            _ => None,
        };
        let read = Read {
            observation: self.ask(&query),
            ..query
        };
        let witness = stamped
            .filter(|_| matches!(read.observation, Observation::Listing(Ok(_))))
            .filter(|_| a_fact(&read.observation))
            .map(|(id, stamp, taken)| Witness {
                checker: self.id,
                dirs: vec![(id, stamp)],
                taken,
                hints: self.hints.get(),
            });
        let check = self.check(&read);
        // A witness the check already carries confirms this read as well
        // as the one just taken would, so only a new one replaces it.
        if witness.is_some() {
            *check.shared.witness.borrow_mut() = witness;
        }
        check
    }

    /// Whether `check`'s witness, taken in an earlier generation, still holds:
    /// this checker took it, none of its stamps is racy, no watch event has
    /// named one of its directories or an entry of one since it was taken
    /// ([`Self::hint`]), and every directory's stamp is unchanged.
    fn witnessed(&self, check: &Check, generation: u64) -> bool {
        let witness = check.shared.witness.borrow();
        let Some(witness) = &*witness else {
            return false;
        };
        if witness.checker != self.id {
            return false;
        }
        let hinted = {
            let dirs = self.dirs.borrow();
            witness
                .dirs
                .iter()
                .any(|(id, _)| dirs[*id].hinted > witness.hints)
        };
        !hinted
            && !witness
                .dirs
                .iter()
                .any(|(_, stamp)| stamp.is_racy(witness.taken))
            && witness
                .dirs
                .iter()
                .all(|(id, stamp)| self.dir_stamp(*id, generation) == Some(*stamp))
    }

    /// The directories whose stamps decide `read`'s answer (see the module
    /// docs), for a read about to be asked; `None` when no directory does,
    /// and the read is asked in every generation.
    fn deciders(&self, read: &Read) -> Option<Vec<PathBuf>> {
        let path = &read.path;
        // A relative path is resolved from the working directory, and has
        // no witness. `lstat` follows a final symlink when the path ends
        // with a separator, so a path not spelt as its components has no
        // `LinkKind` witness; every other query resolves its whole path as
        // the walk does (a `..` from the directory reached).
        let spelt_as_components = lexically_normalize(path).as_os_str() == path.as_os_str();
        if !path.is_absolute()
            || (matches!(read.observation, Observation::LinkKind(_)) && !spelt_as_components)
            || !a_fact(&read.observation)
        {
            return None;
        }
        match &read.observation {
            Observation::Listing(Ok(_)) => Some(vec![path.clone()]),
            Observation::Listing(Err(_)) | Observation::Kind(_) | Observation::Canonical(_) => {
                self.resolution(path, true)
            }
            Observation::LinkKind(_) => self.resolution(path, false),
            // A file's stamp, like its text, moves with no directory's
            // stamp moving.
            Observation::Text(_) | Observation::Stamp(_) => None,
        }
    }

    /// The directories resolving `path` looks an entry up in, following
    /// every symlink (the final component's too, if `follow_last`) as the
    /// kernel resolves it: a link's target is taken component by component,
    /// so a separator it ends with is no different from none, and each `..`
    /// from the directory reached. `None` if `path` is relative, or the walk
    /// follows more symlinks than the kernel does or meets anything but a
    /// directory, a symlink, or the absence or final entry it stops at.
    fn resolution(&self, path: &Path, follow_last: bool) -> Option<Vec<PathBuf>> {
        if !path.is_absolute() {
            return None;
        }
        let mut current = PathBuf::from("/");
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut links = 0;
        // Segments still to resolve, last first, split on separators as the
        // kernel splits them (`Path::components` would drop a `.`, which
        // the kernel looks up). A path that is not UTF-8 has no witness.
        let mut rest: Vec<String> = Vec::new();
        let push_all = |rest: &mut Vec<String>, of: &Path| -> Option<()> {
            for segment in of.to_str()?.split('/').rev() {
                // An empty segment (a doubled or trailing separator) names
                // no entry.
                if !segment.is_empty() {
                    rest.push(segment.to_string());
                }
            }
            Some(())
        };
        push_all(&mut rest, path)?;
        while let Some(name) = rest.pop() {
            // Every segment, `.` and `..` included, is looked up in the
            // directory reached, which takes search permission there: a
            // `chmod` moves its stamp.
            if !dirs.contains(&current) {
                dirs.push(current.clone());
            }
            match name.as_str() {
                "." => continue,
                ".." => {
                    current.pop();
                    continue;
                }
                _ => {}
            }
            let child = current.join(&name);
            let last = rest.is_empty();
            let mut counts = self.counts.get();
            counts.stats += 1;
            self.counts.set(counts);
            match self.files.disk().entry(&child) {
                Ok(EntryAt::Dir) => current = child,
                Ok(EntryAt::Link(target)) if !last || follow_last => {
                    links += 1;
                    if links > MAX_LINKS {
                        return None;
                    }
                    if target.is_absolute() {
                        current = PathBuf::from("/");
                    }
                    push_all(&mut rest, &target)?;
                }
                Ok(_) if last => return Some(dirs),
                // A file in the middle of the path: the query fails, as no
                // answer a witness keeps does.
                Ok(_) => return None,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Some(dirs),
                Err(_) => return None,
            }
        }
        Some(dirs)
    }

    /// `read`'s query asked now, counted.
    fn ask(&self, read: &Read) -> Observation {
        let mut counts = self.counts.get();
        match read.observation {
            Observation::Text(_) => counts.opens += 1,
            Observation::Kind(_) | Observation::LinkKind(_) | Observation::Stamp(_) => {
                counts.stats += 1;
            }
            Observation::Listing(_) => counts.listings += 1,
            Observation::Canonical(_) => counts.canonicalizations += 1,
        }
        self.counts.set(counts);
        self.files.disk().answer(&read.path, &read.observation)
    }

    /// The index of `dir` in [`Self::dirs`], added unstamped if new.
    fn dir_id(&self, dir: PathBuf) -> usize {
        let mut ids = self.dir_ids.borrow_mut();
        if let Some(&id) = ids.get(&dir) {
            return id;
        }
        let mut dirs = self.dirs.borrow_mut();
        let id = dirs.len();
        ids.insert(dir.clone(), id);
        dirs.push(Dir {
            path: dir,
            stamp: None,
            checked: 0,
            hinted: 0,
        });
        id
    }

    /// The stamp in `generation` of the directory `id` names, `stat`ed once
    /// per generation; `None` when it has none.
    fn dir_stamp(&self, id: usize, generation: u64) -> Option<Stamp> {
        let mut dirs = self.dirs.borrow_mut();
        let dir = &mut dirs[id];
        if dir.checked != generation {
            let mut counts = self.counts.get();
            counts.stats += 1;
            self.counts.set(counts);
            dir.stamp = self.files.disk().stat(&dir.path).ok();
            dir.checked = generation;
        }
        dir.stamp
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix))]
mod host_tests {
    use super::*;
    use crate::observed::host_fs;
    use proptest::prelude::*;

    /// Names in the generated trees.
    const NAMES: [&str; 4] = ["a", "b", "l", "m"];

    /// Symlink targets, relative to the tree's root or to the link: the
    /// spellings the kernel resolves in ways a component walk must follow.
    const TARGETS: [&str; 8] = [
        "a", "a/", "b/a", "../a", "./b", "a/../b", "ROOT/b/", "ROOT/a",
    ];

    #[derive(Clone, Debug)]
    enum Change {
        File(Vec<usize>),
        Dir(Vec<usize>),
        Link(Vec<usize>, usize),
        Remove(Vec<usize>),
        Rename(Vec<usize>, Vec<usize>),
        /// Take away, or give back, a directory's search permission.
        Search(Vec<usize>, bool),
    }

    fn rel(names: &[usize]) -> PathBuf {
        names.iter().map(|&i| NAMES[i]).collect()
    }

    fn apply(root: &Path, change: &Change) {
        let at = |names: &[usize]| root.join(rel(names));
        // Failures (a missing parent, an existing entry) leave the tree as
        // it was, which is a change like any other for the property.
        let _ = match change {
            Change::File(p) => std::fs::write(at(p), b"x"),
            Change::Dir(p) => std::fs::create_dir(at(p)),
            Change::Link(p, t) => {
                let target = TARGETS[*t].replace("ROOT", &root.display().to_string());
                std::os::unix::fs::symlink(target, at(p))
            }
            Change::Remove(p) => {
                let path = at(p);
                std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path))
            }
            Change::Rename(from, to) => std::fs::rename(at(from), at(to)),
            Change::Search(p, allowed) => {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = if *allowed { 0o755 } else { 0o644 };
                std::fs::set_permissions(at(p), std::fs::Permissions::from_mode(mode))
            }
        };
    }

    fn restore_search(dir: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
        if let Ok(entries) = host_fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    restore_search(&entry.path());
                }
            }
        }
    }

    fn names() -> impl Strategy<Value = Vec<usize>> {
        prop::collection::vec(0..NAMES.len(), 1..3)
    }

    fn change() -> impl Strategy<Value = Change> {
        prop_oneof![
            names().prop_map(Change::File),
            names().prop_map(Change::Dir),
            (names(), 0..TARGETS.len()).prop_map(|(p, t)| Change::Link(p, t)),
            names().prop_map(Change::Remove),
            (names(), names()).prop_map(|(a, b)| Change::Rename(a, b)),
            (names(), any::<bool>()).prop_map(|(p, allowed)| Change::Search(p, allowed)),
        ]
    }

    /// A `..` is looked up in the directory it leaves, and a `.` in the
    /// directory it names, each needing search permission there: taking it
    /// away changes where the path leads without changing any directory the
    /// rest of the path names (Codex, twice). Each case is a path, or a link
    /// whose target is that path, to a directory through `gate`.
    #[test]
    fn a_directory_a_dot_is_looked_up_in_is_witnessed() {
        use std::os::unix::fs::PermissionsExt as _;
        for (spelling, through_link) in [
            ("gate/../target", true),
            ("gate/.", true),
            ("gate/.", false),
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            let root = host_fs::canonicalize(tmp.path()).unwrap();
            std::fs::create_dir(root.join("gate")).unwrap();
            std::fs::create_dir(root.join("target")).unwrap();
            let path = if through_link {
                std::os::unix::fs::symlink(spelling, root.join("through")).unwrap();
                root.join("through")
            } else {
                PathBuf::from(format!("{}/{spelling}", root.display()))
            };
            let check = Check::new(Read {
                observation: HostDisk.answer(&path, &Observation::Kind(Ok(EntryKind::Dir))),
                path,
            });
            let mut checker = ReadChecks::<HostDisk>::default();
            let mut at = Nanos(i128::MAX / 2);
            for _ in 0..2 {
                checker.begin_generation(at);
                assert!(checker.holds(&check));
                checker.end_generation();
                at = Nanos(at.0 + 1);
            }
            let gate = root.join("gate");
            std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o644)).unwrap();
            checker.begin_generation(at);
            let truth = HostDisk.answer(&check.read().path, &check.read().observation);
            let holds = checker.holds(&check);
            checker.end_generation();
            std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o755)).unwrap();
            if truth == check.read().observation {
                // The tests run with the permission to search any directory
                // (as root, say), so the change moved nothing to notice.
                eprintln!("{spelling}: search permission is not enforced here; skipped");
                continue;
            }
            assert!(!holds, "{spelling} (through a link: {through_link}) held");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

        /// On the host's file system, with symlinks of every spelling: a
        /// check that holds in a generation after a change gets the answer
        /// recorded. Each change is followed by a pause longer than a coarse
        /// filesystem tick, since the generations run on a clock ahead of the
        /// files (so that witnesses are used), which the racy rule would
        /// otherwise answer for.
        #[test]
        fn a_check_that_holds_is_right_on_the_host(
            setup in prop::collection::vec(change(), 0..12),
            rounds in prop::collection::vec(prop::collection::vec(change(), 0..3), 1..6),
        ) {
            let tmp = tempfile::TempDir::new().unwrap();
            let root = host_fs::canonicalize(tmp.path()).unwrap();
            for change in &setup {
                apply(&root, change);
            }
            let mut paths = vec![root.clone()];
            for a in NAMES {
                paths.push(root.join(a));
                // Spellings the kernel resolves otherwise than by components.
                paths.push(PathBuf::from(format!("{}/{a}/", root.display())));
                paths.push(root.join("l/..").join(a));
                paths.push(root.join("a/..").join(a));
                paths.push(PathBuf::from(format!("{}/{a}/.", root.display())));
                for b in NAMES {
                    paths.push(root.join(a).join(b));
                }
            }
            let queries = [
                Observation::Kind(Ok(EntryKind::File)),
                Observation::LinkKind(Ok(EntryKind::File)),
                Observation::Listing(Ok(Vec::new())),
                Observation::Canonical(Ok(PathBuf::new())),
            ];
            let checks: Vec<Check> = paths
                .iter()
                .flat_map(|path| {
                    queries.iter().map(move |query| {
                        Check::new(Read {
                            path: path.clone(),
                            observation: HostDisk.answer(path, query),
                        })
                    })
                })
                .collect();
            let mut checker = ReadChecks::<HostDisk>::default();
            let mut at = Nanos(i128::MAX / 2);
            for round in rounds.iter().chain([&Vec::new()]) {
                for change in round {
                    apply(&root, change);
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
                checker.begin_generation(at);
                for check in &checks {
                    if checker.holds(check) {
                        let truth = HostDisk.answer(&check.read().path, &check.read().observation);
                        prop_assert_eq!(&truth, &check.read().observation, "{:?}", check.read().path);
                    }
                }
                checker.end_generation();
                at = Nanos(at.0 + 1);
            }
            // Give every directory its search permission back, so the
            // temporary directory can be removed.
            restore_search(&root);
        }
    }

    /// A path the kernel resolves differently from its components (a
    /// trailing separator follows a final symlink, `..` is taken from the
    /// directory reached) has no witness: it is asked in every generation.
    /// Here `link/` names the target directory through a symlink, and the
    /// target's deletion is noticed though the link's directory is unchanged.
    #[test]
    fn a_spelling_the_stamps_cannot_follow_is_asked_every_time() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = host_fs::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(root.join("elsewhere/target")).unwrap();
        std::fs::create_dir(root.join("here")).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere/target"), root.join("here/link")).unwrap();
        // A link whose target ends with a separator, to a link (Codex).
        std::os::unix::fs::symlink(format!("{}/here/link/", root.display()), root.join("chain"))
            .unwrap();
        let spellings = [
            PathBuf::from(format!("{}/here/link/", root.display())),
            root.join("here/link/../target"),
            root.join("chain"),
        ];
        let queries = [
            Observation::Kind(Ok(EntryKind::Dir)),
            // `lstat` of `link/` follows the link.
            Observation::LinkKind(Ok(EntryKind::Dir)),
        ];
        let checks: Vec<Check> = spellings
            .iter()
            .flat_map(|path| {
                queries.iter().map(move |query| {
                    Check::new(Read {
                        path: path.clone(),
                        observation: HostDisk.answer(path, query),
                    })
                })
            })
            .collect();
        let mut checker = ReadChecks::<HostDisk>::default();
        // Far enough ahead that no stamp is racy, so a witness would be used.
        let mut at = Nanos(i128::MAX / 2);
        for _ in 0..2 {
            checker.begin_generation(at);
            assert!(checks.iter().all(|check| checker.holds(check)));
            checker.end_generation();
            at = Nanos(at.0 + 1);
        }
        std::fs::remove_dir(root.join("elsewhere/target")).unwrap();
        checker.begin_generation(at);
        for check in &checks {
            // Every answer moved but `lstat` of `chain`, which is the link.
            let truth = HostDisk.answer(&check.read().path, &check.read().observation);
            assert_eq!(
                checker.holds(check),
                truth == check.read().observation,
                "{:?} {:?}",
                check.read().path,
                check.read().observation
            );
        }
        checker.end_generation();
    }
}
