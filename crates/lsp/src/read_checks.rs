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
//! - **a listing** of `d` holds (its entries and their types), and a
//!   listing that found `d` absent holds while `d` is, as below;
//! - **what `d/n` is**, not following it, holds, and so does whether it is
//!   there; following it holds too while, if `d/n` is a symlink, the stamp
//!   of the directory its target is looked up in holds, and so on along the
//!   chain;
//! - **the canonical spelling** of a path is a function of the names on the
//!   way, not only of where it leads, so it holds while the stamp of every
//!   directory the path's resolution looks an entry up in holds
//!   ([`ReadChecks::resolution`] walks it as the kernel does: following
//!   symlinks, and taking each `..` from the directory reached).
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

use borzoi_msbuild::observed::{Failure, Observation, Read, ReadCounts};

use crate::observed::HostDisk;
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
        let parent = || path.parent().map(|dir| vec![dir.to_path_buf()]);
        match &read.observation {
            Observation::Listing(Ok(_)) => Some(vec![path.clone()]),
            // An absent directory is an entry absent from the one it would
            // be looked up in.
            Observation::Listing(Err(failure)) if failure.is_absent() => self.link_chain(path),
            Observation::LinkKind(answer) if absent_or_answered(answer.as_ref().map(drop)) => {
                parent()
            }
            Observation::Kind(answer) if absent_or_answered(answer.as_ref().map(drop)) => {
                self.link_chain(path)
            }
            Observation::Canonical(answer) if absent_or_answered(answer.as_ref().map(drop)) => {
                self.resolution(path)
            }
            _ => None,
        }
    }

    /// The directories `path`'s final entry is looked up in, following it
    /// while it is a symlink: its parent, then each link target's parent.
    /// `None` if the chain is longer than the kernel follows, or meets an
    /// entry it cannot read.
    fn link_chain(&self, path: &Path) -> Option<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        let mut at = path.to_path_buf();
        for _ in 0..=MAX_LINKS {
            let dir = at.parent()?.to_path_buf();
            let mut counts = self.counts.get();
            counts.stats += 1;
            self.counts.set(counts);
            let entry = self.files.disk().entry(&at);
            dirs.push(dir.clone());
            match entry {
                Ok(EntryAt::Link(target)) => at = dir.join(target),
                Ok(_) => return Some(dirs),
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Some(dirs),
                Err(_) => return None,
            }
        }
        None
    }

    /// The directories resolving `path` looks an entry up in, following
    /// every symlink, as the kernel resolves it; `None` if `path` is
    /// relative, or the walk follows more symlinks than the kernel does or
    /// meets anything but a directory, a symlink, or the absence or final
    /// entry it stops at.
    fn resolution(&self, path: &Path) -> Option<Vec<PathBuf>> {
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
                Ok(EntryAt::Link(target)) => {
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
