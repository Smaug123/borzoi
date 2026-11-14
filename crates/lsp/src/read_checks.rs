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

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

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
        Observation::Stamp(answer) => answered_or_absent(answer) && !observation.is_racy_stamp(),
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
}

/// One recorded read, and the witness that confirmed it last.
#[derive(Debug)]
pub struct Check {
    read: Read,
    witness: RefCell<Option<Witness>>,
    /// For a racy stamp a whole-file read took: a stamp of the file, not
    /// racy, taken before a read that found the bytes the racy one saw.
    /// While the file keeps it, the read holds with no read of the file
    /// ([`ReadChecks::racy_stamp_holds`]).
    confirmed: std::cell::Cell<Option<FileStamp>>,
}

impl Check {
    pub fn new(read: Read) -> Check {
        Check {
            read,
            witness: RefCell::new(None),
            confirmed: std::cell::Cell::new(None),
        }
    }

    pub fn read(&self) -> &Read {
        &self.read
    }
}

/// The stamps of the directories that decide a read's answer, and the time
/// of the generation that took them, before the query was asked.
#[derive(Clone, Debug)]
struct Witness {
    /// Each directory as its index in [`ReadChecks::dirs`], and its stamp.
    dirs: Vec<(usize, Stamp)>,
    taken: Nanos,
}

/// One directory a witness names: its path, and its stamp (or `None` where
/// it has none: it is absent, or cannot be `stat`ed) as the generation that
/// last `stat`ed it saw it.
#[derive(Debug)]
struct Dir {
    path: PathBuf,
    stamp: Option<Stamp>,
    checked: u64,
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
        }
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
    /// next check reads them again whatever their stamps say.
    pub fn hint<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        self.files.hint(paths);
    }

    /// Whether `check`'s read gets the answer recorded, now. Outside a
    /// generation, the query is asked.
    pub fn holds(&self, check: &Check) -> bool {
        let read = &check.read;
        // A racy stamp speaks for the file only when it was taken; what it
        // saw is confirmed by the file's contents, when it says what they were.
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
                .map(|dirs| Witness { dirs, taken: now })
        });
        let holds = self.ask(read) == read.observation;
        *check.witness.borrow_mut() = witness.filter(|_| holds);
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
        let path = &check.read.path;
        let mut counts = self.counts.get();
        if let Some(confirmed) = check.confirmed.get() {
            counts.stats += 1;
            self.counts.set(counts);
            let now = self.files.disk().answer(path, &check.read.observation);
            if now == Observation::Stamp(Ok(confirmed)) {
                return true;
            }
            check.confirmed.set(None);
        }
        let mut counts = self.counts.get();
        counts.opens += 1;
        self.counts.set(counts);
        match self.files.disk().racy_content(path, digest) {
            Some(now) => {
                if !now.racy {
                    check.confirmed.set(Some(now));
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
                dirs: vec![(id, stamp)],
                taken,
            });
        Check {
            read,
            witness: RefCell::new(witness),
            confirmed: std::cell::Cell::new(None),
        }
    }

    /// Whether `check`'s witness, taken in an earlier generation, still holds:
    /// none of its stamps is racy, and every directory's stamp is unchanged.
    fn witnessed(&self, check: &Check, generation: u64) -> bool {
        let witness = check.witness.borrow();
        let Some(witness) = &*witness else {
            return false;
        };
        !witness
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
