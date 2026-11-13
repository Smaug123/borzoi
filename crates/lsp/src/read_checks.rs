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
//! - **a listing** of `d` holds (its entries and their types);
//! - **what a path names**, following symlinks or not, whether it is there,
//!   and **its canonical spelling** hold while the stamp of every directory
//!   its resolution looks an entry up in holds: each step of the resolution
//!   is an entry of the directory it is looked up in, and none can change
//!   without moving that directory's stamp. `ReadChecks::resolution` walks
//!   it as the kernel does (following symlinks, a link's target component by
//!   component, and taking each `..` from the directory reached), and a
//!   listing that found its directory absent is witnessed the same way.
//!
//! A read whose path is not spelt as its components (a trailing separator,
//! a `.` or `..`, a doubled separator, a relative path) is resolved by the
//! kernel in ways these rules do not follow, and has no witness.
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
use borzoi_msbuild::observed::{Failure, Observation, Read, ReadCounts};

use crate::observed::HostDisk;
use crate::paths::lexically_normalize;
use crate::vfs::{Disk, DiskCache, Nanos, Stamp};

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
}

/// One recorded read, and the witness that confirmed it last.
#[derive(Debug)]
pub struct Check {
    read: Read,
    witness: RefCell<Option<Witness>>,
}

impl Check {
    pub fn new(read: Read) -> Check {
        Check {
            read,
            witness: RefCell::new(None),
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
    dirs: Vec<(PathBuf, Stamp)>,
    taken: Nanos,
}

/// Confirms [`Check`]s for the open generation.
#[derive(Debug)]
pub struct ReadChecks<D: Probe = HostDisk> {
    /// Whole-file reads, confirmed by the file's stamp.
    files: DiskCache<D>,
    /// Each directory's stamp, or `None` where it has none (it is absent, or
    /// cannot be `stat`ed), and the generation that took it: one `stat` per
    /// directory per generation.
    dirs: RefCell<HashMap<PathBuf, (Option<Stamp>, u64)>>,
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
            files: DiskCache::with_disk(disk),
            dirs: RefCell::new(HashMap::new()),
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
            opens: files.opens,
            stats: files.stats + counts.stats,
            ..counts
        }
    }

    /// The open generation's number, or `None` between generations.
    pub fn generation(&self) -> Option<u64> {
        self.files.generation()
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
                .map(|dir| self.dir_stamp(&dir, generation).map(|stamp| (dir, stamp)))
                .collect::<Option<Vec<_>>>()
                .map(|dirs| Witness { dirs, taken: now })
        });
        let holds = self.ask(read) == read.observation;
        *check.witness.borrow_mut() = witness.filter(|_| holds);
        holds
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
                .all(|(dir, stamp)| self.dir_stamp(dir, generation) == Some(*stamp))
    }

    /// The directories whose stamps decide `read`'s answer (see the module
    /// docs), for a read about to be asked; `None` when no directory does,
    /// and the read is asked in every generation.
    fn deciders(&self, read: &Read) -> Option<Vec<PathBuf>> {
        let absent_or_answered = |answer: Result<(), &Failure>| match answer {
            Ok(()) => true,
            Err(failure) => failure.is_absent(),
        };
        let path = &read.path;
        // A spelling the kernel resolves otherwise than component by
        // component (a trailing separator follows a final symlink, `..` is
        // taken from the directory reached, a relative path from the working
        // directory) has no witness.
        if !path.is_absolute() || lexically_normalize(path).as_os_str() != path.as_os_str() {
            return None;
        }
        match &read.observation {
            Observation::Listing(Ok(_)) => Some(vec![path.clone()]),
            Observation::Listing(Err(failure)) if failure.is_absent() => {
                self.resolution(path, true)
            }
            Observation::LinkKind(answer) if absent_or_answered(answer.as_ref().map(drop)) => {
                self.resolution(path, false)
            }
            Observation::Kind(answer) if absent_or_answered(answer.as_ref().map(drop)) => {
                self.resolution(path, true)
            }
            Observation::Canonical(answer) if absent_or_answered(answer.as_ref().map(drop)) => {
                self.resolution(path, true)
            }
            _ => None,
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
        use std::path::Component;
        if !path.is_absolute() {
            return None;
        }
        let mut current = PathBuf::from("/");
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut links = 0;
        // Components still to resolve, last first, as owned names.
        let mut rest: Vec<std::ffi::OsString> = Vec::new();
        let push_all = |rest: &mut Vec<std::ffi::OsString>, of: &Path| {
            for component in of.components().rev() {
                match component {
                    Component::Normal(name) => rest.push(name.to_os_string()),
                    Component::ParentDir => rest.push("..".into()),
                    Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                }
            }
        };
        push_all(&mut rest, path);
        while let Some(name) = rest.pop() {
            if name == ".." {
                current.pop();
                continue;
            }
            if !dirs.contains(&current) {
                dirs.push(current.clone());
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
                    push_all(&mut rest, &target);
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
            Observation::Kind(_) | Observation::LinkKind(_) => counts.stats += 1,
            Observation::Listing(_) => counts.listings += 1,
            Observation::Canonical(_) => counts.canonicalizations += 1,
        }
        self.counts.set(counts);
        self.files.disk().answer(&read.path, &read.observation)
    }

    /// `dir`'s stamp in `generation`, `stat`ed once per generation; `None`
    /// when it has none.
    fn dir_stamp(&self, dir: &Path, generation: u64) -> Option<Stamp> {
        if let Some((stamp, checked)) = self.dirs.borrow().get(dir)
            && *checked == generation
        {
            return *stamp;
        }
        let mut counts = self.counts.get();
        counts.stats += 1;
        self.counts.set(counts);
        let stamp = self.files.disk().stat(dir).ok();
        let mut dirs = self.dirs.borrow_mut();
        match dirs.get_mut(dir) {
            Some(entry) => *entry = (stamp, generation),
            None => {
                dirs.insert(dir.to_path_buf(), (stamp, generation));
            }
        }
        stamp
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
        };
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
        ]
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
            for a in 0..NAMES.len() {
                paths.push(root.join(NAMES[a]));
                // Spellings the kernel resolves otherwise than by components.
                paths.push(PathBuf::from(format!("{}/{}/", root.display(), NAMES[a])));
                paths.push(root.join("l/..").join(NAMES[a]));
                for b in 0..NAMES.len() {
                    paths.push(root.join(NAMES[a]).join(NAMES[b]));
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
            assert!(!checker.holds(check), "{:?} held", check.read().path);
        }
        checker.end_generation();
    }
}
