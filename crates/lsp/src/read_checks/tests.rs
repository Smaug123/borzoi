//! [`ReadChecks`] against a model file system with a generated clock.
//!
//! The model holds files, directories and absolute symlinks. A change to a
//! directory's entries stamps the directory with the clock rounded down to a
//! generated granularity up to [`RACY_WINDOW`], so the property covers
//! coarse-clock filesystems the machine running the tests does not have.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use borzoi_msbuild::observed::{EntryKind, Failure, Listed};
use proptest::prelude::*;
use proptest::strategy::ValueTree as _;

use super::*;
use crate::vfs::RACY_WINDOW;

const SECOND: i128 = 1_000_000_000;
const WINDOW: i128 = RACY_WINDOW.as_nanos() as i128;

#[derive(Clone, Debug)]
enum Node {
    File(Vec<u8>),
    Dir,
    Link(PathBuf),
}

#[derive(Clone, Debug)]
struct Entry {
    node: Node,
    ino: u64,
    mtime: i128,
    ctime: i128,
}

/// A file system whose clock the test moves.
#[derive(Debug)]
struct Model {
    entries: BTreeMap<PathBuf, Entry>,
    clock: i128,
    granularity: i128,
    next_ino: u64,
    /// A change made at the next query: one that lands between a check's
    /// stamps and its question.
    interleaved: Option<Op>,
    /// A change made at the next `stat`, just before it takes its stamp:
    /// one that lands between a query and a stamp taken after it, which
    /// the stamp sees and the query did not.
    interleaved_at_stat: Option<Op>,
    /// Every listing also reports an entry it could not read, as a
    /// transient I/O error during iteration does.
    unreadable_entry: bool,
}

fn failure(kind: io::ErrorKind) -> Failure {
    Failure::of(&io::Error::from(kind))
}

impl Model {
    fn new(granularity: i128) -> Model {
        let mut model = Model {
            entries: BTreeMap::new(),
            clock: 100 * SECOND,
            granularity,
            next_ino: 1,
            interleaved: None,
            interleaved_at_stat: None,
            unreadable_entry: false,
        };
        model.entries.insert(
            PathBuf::from("/"),
            Entry {
                node: Node::Dir,
                ino: 1,
                mtime: 0,
                ctime: 0,
            },
        );
        model
    }

    fn tick(&self) -> i128 {
        self.clock.div_euclid(self.granularity) * self.granularity
    }

    /// Stamp `dir` as changed now.
    fn touch(&mut self, dir: &Path) {
        let now = self.tick();
        if let Some(entry) = self.entries.get_mut(dir) {
            entry.mtime = now;
            entry.ctime = now;
        }
    }

    /// Put `node` at `path`, whose parent must be a directory and which must
    /// be absent; `false` if it cannot be.
    fn create(&mut self, path: &Path, node: Node) -> bool {
        let parent = path.parent().expect("not the root");
        if !matches!(
            self.entries.get(parent),
            Some(Entry {
                node: Node::Dir,
                ..
            })
        ) || self.entries.contains_key(path)
        {
            return false;
        }
        self.next_ino += 1;
        let now = self.tick();
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                node,
                ino: self.next_ino,
                mtime: now,
                ctime: now,
            },
        );
        self.touch(parent);
        true
    }

    /// Remove `path` and everything below it.
    fn remove(&mut self, path: &Path) {
        if path == Path::new("/") || !self.entries.contains_key(path) {
            return;
        }
        self.entries.retain(|p, _| !p.starts_with(path));
        self.touch(path.parent().expect("not the root"));
    }

    /// Move `from` and everything below it to `to`, which must be absent
    /// under a directory: the entries keep their inodes, and both parents
    /// are stamped.
    fn rename(&mut self, from: &Path, to: &Path) {
        let Some(parent) = to.parent() else { return };
        if from == Path::new("/")
            || !self.entries.contains_key(from)
            || self.entries.contains_key(to)
            || to.starts_with(from)
            || !matches!(
                self.entries.get(parent),
                Some(Entry {
                    node: Node::Dir,
                    ..
                })
            )
        {
            return;
        }
        let moved: Vec<(PathBuf, Entry)> = self
            .entries
            .iter()
            .filter(|(p, _)| p.starts_with(from))
            .map(|(p, e)| (to.join(p.strip_prefix(from).unwrap()), e.clone()))
            .collect();
        self.entries.retain(|p, _| !p.starts_with(from));
        self.entries.extend(moved);
        self.touch(from.parent().expect("not the root"));
        self.touch(parent);
    }

    /// Rewrite the file at `path` in place.
    fn write(&mut self, path: &Path, bytes: &[u8]) {
        let now = self.tick();
        if let Some(Entry {
            node: Node::File(old),
            mtime,
            ctime,
            ..
        }) = self.entries.get_mut(path)
        {
            *old = bytes.to_vec();
            *mtime = now;
            *ctime = now;
        }
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::Advance(ms) => self.clock += i128::from(*ms) * 1_000_000,
            Op::File(p) => {
                self.create(&path(*p), Node::File(b"x".to_vec()));
            }
            Op::Dir(p) => {
                self.create(&path(*p), Node::Dir);
            }
            Op::Link(p, target) => {
                self.create(&path(*p), Node::Link(path(*target)));
            }
            Op::Remove(p) => self.remove(&path(*p)),
            Op::Write(p) => self.write(&path(*p), b"yy"),
            Op::Rename(from, to) => self.rename(&path(*from), &path(*to)),
            Op::MoveAndLink(from) => {
                let (from, to) = (path(*from), PathBuf::from("/w/z"));
                self.rename(&from, &to);
                self.create(&from, Node::Link(to));
            }
            Op::Interleave(op) => self.interleaved = Some((**op).clone()),
            Op::InterleaveAtStat(op) => self.interleaved_at_stat = Some((**op).clone()),
            Op::Unreadable(on) => self.unreadable_entry = *on,
            Op::Generation => {}
            Op::Stealth(op) => {
                let stamps: BTreeMap<PathBuf, (i128, i128)> = self
                    .entries
                    .iter()
                    .map(|(p, e)| (p.clone(), (e.mtime, e.ctime)))
                    .collect();
                self.apply(op);
                for (p, entry) in &mut self.entries {
                    if let Some(&(mtime, ctime)) = stamps.get(p) {
                        entry.mtime = mtime;
                        entry.ctime = ctime;
                    }
                }
            }
        }
    }

    /// Where `path` leads, following symlinks (the last one too, if
    /// `follow`): the path of the entry it names, or why it names none.
    fn resolve(&self, path: &Path, follow: bool) -> Result<PathBuf, io::ErrorKind> {
        let mut current = PathBuf::from("/");
        let mut rest: Vec<std::ffi::OsString> = path
            .components()
            .skip(1)
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        rest.reverse();
        let mut hops = 0;
        while let Some(name) = rest.pop() {
            let next = current.join(&name);
            match self.entries.get(&next).map(|e| &e.node) {
                None => {
                    return Err(match self.entries.get(&current).map(|e| &e.node) {
                        Some(Node::File(_)) => io::ErrorKind::NotADirectory,
                        _ => io::ErrorKind::NotFound,
                    });
                }
                Some(Node::Link(target)) if follow || !rest.is_empty() => {
                    hops += 1;
                    if hops > 8 {
                        return Err(io::ErrorKind::InvalidInput);
                    }
                    let mut target: Vec<std::ffi::OsString> = target
                        .components()
                        .skip(1)
                        .map(|c| c.as_os_str().to_os_string())
                        .collect();
                    target.reverse();
                    rest.extend(target);
                    current = PathBuf::from("/");
                }
                Some(Node::File(_)) if !rest.is_empty() => {
                    return Err(io::ErrorKind::NotADirectory);
                }
                Some(_) => current = next,
            }
        }
        Ok(current)
    }

    fn kind_of(node: &Node) -> EntryKind {
        match node {
            Node::File(_) => EntryKind::File,
            Node::Dir => EntryKind::Dir,
            Node::Link(_) => EntryKind::Symlink,
        }
    }

    fn answer(&self, path: &Path, query: &Observation) -> Observation {
        let at = |follow: bool| self.resolve(path, follow).map_err(failure);
        match query {
            Observation::Text(_) => unreachable!("whole-file reads go through the DiskCache"),
            Observation::Stamp(_) => unreachable!("an evaluation records no stamps"),
            Observation::Kind(_) => {
                Observation::Kind(at(true).map(|p| Model::kind_of(&self.entries[&p].node)))
            }
            Observation::LinkKind(_) => {
                Observation::LinkKind(at(false).map(|p| Model::kind_of(&self.entries[&p].node)))
            }
            Observation::Canonical(_) => Observation::Canonical(at(true)),
            Observation::Listing(_) => Observation::Listing(at(true).and_then(|dir| {
                if !matches!(self.entries[&dir].node, Node::Dir) {
                    return Err(failure(io::ErrorKind::NotADirectory));
                }
                let mut listed: Vec<Listed> = self
                    .entries
                    .iter()
                    .filter(|(p, _)| p.parent() == Some(dir.as_path()) && **p != dir)
                    .map(|(p, e)| Listed::Entry {
                        name: p.file_name().unwrap().to_os_string(),
                        kind: Ok(Model::kind_of(&e.node)),
                    })
                    .collect();
                if self.unreadable_entry {
                    listed.push(Listed::Unreadable(failure(io::ErrorKind::Other)));
                }
                Ok(listed)
            })),
        }
    }

    fn stamp(&self, path: &Path) -> io::Result<Stamp> {
        let target = self.resolve(path, true).map_err(io::Error::from)?;
        let entry = &self.entries[&target];
        let size = match &entry.node {
            Node::File(bytes) => bytes.len() as u64,
            _ => 0,
        };
        Ok(Stamp {
            dev: 1,
            ino: entry.ino,
            size,
            mtime: Nanos(entry.mtime),
            ctime: Some(Nanos(entry.ctime)),
        })
    }
}

impl Disk for Rc<RefCell<Model>> {
    fn stat(&self, path: &Path) -> io::Result<Stamp> {
        let pending = self.borrow_mut().interleaved_at_stat.take();
        if let Some(op) = pending {
            self.borrow_mut().apply(&op);
        }
        self.borrow().stamp(path)
    }

    fn read(&self, path: &Path) -> io::Result<(Stamp, Vec<u8>)> {
        let model = self.borrow();
        let stamp = model.stamp(path)?;
        let target = model.resolve(path, true).map_err(io::Error::from)?;
        match &model.entries[&target].node {
            Node::File(bytes) => Ok((stamp, bytes.clone())),
            _ => Err(io::Error::from(io::ErrorKind::IsADirectory)),
        }
    }
}

impl Probe for Rc<RefCell<Model>> {
    fn entry(&self, path: &Path) -> io::Result<EntryAt> {
        let model = self.borrow();
        let at = model.resolve(path, false).map_err(io::Error::from)?;
        Ok(match &model.entries[&at].node {
            Node::Dir => EntryAt::Dir,
            Node::Link(target) => EntryAt::Link(target.clone()),
            Node::File(_) => EntryAt::Other,
        })
    }

    fn answer(&self, path: &Path, query: &Observation) -> Observation {
        let pending = self.borrow_mut().interleaved.take();
        if let Some(op) = pending {
            self.borrow_mut().apply(&op);
        }
        self.borrow().answer(path, query)
    }
}

const PATHS: [&str; 13] = [
    "/w",
    "/w/a",
    "/w/a/b",
    "/w/c",
    "/w/x",
    "/w/a/x",
    "/w/a/b/x",
    "/w/c/x",
    "/w/a/y",
    "/w/c/a",
    // Through the symlink the setup makes at `/w/c/a`.
    "/w/c/a/x",
    "/w/c/a/b/x",
    "/w/z",
];

fn path(i: usize) -> PathBuf {
    PathBuf::from(PATHS[i])
}

#[derive(Clone, Debug)]
enum Op {
    /// Move the clock forward by this many milliseconds.
    Advance(u32),
    File(usize),
    Dir(usize),
    Link(usize, usize),
    Remove(usize),
    Write(usize),
    /// Move the first path to the second.
    Rename(usize, usize),
    /// Move a directory to `/w/z`, and leave a symlink to it where it was:
    /// every path through it leads where it did, under another canonical
    /// spelling.
    MoveAndLink(usize),
    /// Make this change at the next query a check asks.
    Interleave(Box<Op>),
    /// Make this change at the next `stat`, just before it takes its stamp.
    InterleaveAtStat(Box<Op>),
    /// Make every listing report an entry it could not read, or stop. The
    /// generator does not make it: a transient failure appearing is no
    /// change to the disk, and nothing stamped can see it; what the checker
    /// owes is to notice one clearing
    /// (`a_listing_with_a_failed_entry_is_asked_every_time`).
    Unreadable(bool),
    /// Begin a generation at the clock's time and check every read.
    Generation,
    /// Make this change on a file system whose stamps do not move (D1's
    /// "not covered"): every entry that was there keeps its stamps. Then
    /// hint the checker with the paths it changed, as a watch event naming
    /// them does ([`ReadChecks::hint`]).
    Stealth(Box<Op>),
}

impl Op {
    /// The paths a watch event for this change names: what it created,
    /// removed, wrote or moved.
    fn named(&self) -> Vec<PathBuf> {
        match self {
            Op::File(p) | Op::Dir(p) | Op::Link(p, _) | Op::Remove(p) | Op::Write(p) => {
                vec![path(*p)]
            }
            Op::Rename(from, to) => vec![path(*from), path(*to)],
            Op::MoveAndLink(from) => vec![path(*from), PathBuf::from("/w/z")],
            _ => Vec::new(),
        }
    }
}

fn change() -> impl Strategy<Value = Op> {
    let p = 0..PATHS.len();
    prop_oneof![
        2 => p.clone().prop_map(Op::File),
        2 => p.clone().prop_map(Op::Dir),
        1 => (p.clone(), p.clone()).prop_map(|(a, b)| Op::Link(a, b)),
        2 => p.clone().prop_map(Op::Remove),
        1 => p.clone().prop_map(Op::Write),
        2 => (p.clone(), p).prop_map(|(a, b)| Op::Rename(a, b)),
        2 => (1usize..4).prop_map(Op::MoveAndLink),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => prop_oneof![1u32..50, 50u32..1_500, 1_500u32..4_000].prop_map(Op::Advance),
        6 => change(),
        1 => change().prop_map(|op| Op::Interleave(Box::new(op))),
        1 => change().prop_map(|op| Op::InterleaveAtStat(Box::new(op))),
        2 => change().prop_map(|op| Op::Stealth(Box::new(op))),
        4 => Just(Op::Generation),
    ]
}

fn granularity() -> impl Strategy<Value = i128> {
    prop_oneof![Just(1), Just(1_000_000), Just(SECOND), Just(WINDOW)]
}

/// What a run saw, for the non-vacuity check.
#[derive(Default, Debug)]
struct Seen {
    /// Generations in which some check held on its witness alone, asking
    /// fewer queries than there are checks.
    witnessed: usize,
    /// Checks that found their answer moved.
    moved: usize,
    /// Listings [`ReadChecks::list`] made that a later generation confirmed
    /// by their born witness alone, with no listing asked.
    born_witnessed: usize,
    /// Listings [`ReadChecks::list`] made that a later generation found
    /// moved.
    born_moved: usize,
    /// Checks that found their answer moved in the first generation after a
    /// change no stamp saw ([`Op::Stealth`]): only the hint can have told.
    hinted_moved: usize,
}

/// Every non-text query at every path, recorded against the model as it is.
fn checks(model: &Model) -> Vec<Check> {
    let queries = [
        Observation::Kind(Ok(EntryKind::File)),
        Observation::LinkKind(Ok(EntryKind::File)),
        Observation::Listing(Ok(Vec::new())),
        Observation::Canonical(Ok(PathBuf::new())),
    ];
    PATHS
        .iter()
        .flat_map(|p| {
            queries.iter().map(move |query| {
                let path = PathBuf::from(p);
                let observation = model.answer(&path, query);
                Check::new(Read { path, observation })
            })
        })
        .collect()
}

fn run(granularity: i128, setup: &[Op], ops: &[Op], seen: &mut Seen) -> Result<(), TestCaseError> {
    let model = Rc::new(RefCell::new(Model::new(granularity)));
    for op in setup {
        model.borrow_mut().apply(op);
    }
    let checks = checks(&model.borrow());
    let mut checker = ReadChecks::with_disk(Rc::clone(&model));
    // Listings the checker made itself, as a reader that depends on them
    // does (`ReadChecks::list`): one more per generation, held to the same
    // property in every later generation.
    let mut born: Vec<Check> = Vec::new();
    // Whether a change no stamp saw has been made since the last
    // generation, and whether any other change has.
    let (mut stealthy, mut seen_by_stamps) = (false, false);
    for op in ops {
        if let Op::Stealth(change) = op {
            model.borrow_mut().apply(op);
            checker.hint(change.named().iter().map(PathBuf::as_path));
            stealthy = true;
            continue;
        }
        if !matches!(op, Op::Generation) {
            seen_by_stamps |= !matches!(op, Op::Advance(_));
            model.borrow_mut().apply(op);
            continue;
        }
        let (stealthy_since, seen_since) = (
            std::mem::take(&mut stealthy),
            std::mem::take(&mut seen_by_stamps),
        );
        let after_stealth = stealthy_since && !seen_since;
        let now = model.borrow().clock;
        // What every read answered as the generation began: a change the
        // generation makes (an interleaved one) may be seen or not within
        // it, but not after it.
        let before: Vec<Observation> = checks
            .iter()
            .map(|check| {
                model
                    .borrow()
                    .answer(&check.read().path, &check.read().observation)
            })
            .collect();
        checker.begin_generation(Nanos(now));
        // First in the generation, so that a pending interleaved change
        // lands around its stamp and its listing.
        let listed = checker.list(&path(born.len() % PATHS.len()));
        for (check, before) in checks.iter().zip(&before) {
            let holds = checker.holds(check);
            let truth = model
                .borrow()
                .answer(&check.read().path, &check.read().observation);
            if holds {
                prop_assert!(
                    truth == check.read().observation || *before == check.read().observation,
                    "{:?} held, but the disk moved: {:?}",
                    check.read().path,
                    truth
                );
            } else {
                seen.moved += 1;
                if after_stealth {
                    seen.hinted_moved += 1;
                }
            }
        }
        let counts = checker.end_generation();
        if counts.listings + counts.canonicalizations < 2 * PATHS.len() as u64 {
            seen.witnessed += 1;
        }
        // The born listings, each alone in a generation of its own, so
        // that a confirmation asking no listing shows its witness held.
        for check in &born {
            let before = model
                .borrow()
                .answer(&check.read().path, &check.read().observation);
            checker.begin_generation(Nanos(now));
            let holds = checker.holds(check);
            let asked = checker.end_generation().listings;
            let truth = model
                .borrow()
                .answer(&check.read().path, &check.read().observation);
            if holds {
                prop_assert!(
                    truth == check.read().observation || before == check.read().observation,
                    "{:?}, listed by the checker, held, but the disk moved: {:?}",
                    check.read().path,
                    truth
                );
                if asked == 0 {
                    seen.born_witnessed += 1;
                }
            } else {
                seen.born_moved += 1;
            }
        }
        born.push(listed);
    }
    Ok(())
}

fn setup() -> impl Strategy<Value = Vec<Op>> {
    // Most of the vocabulary exists, so most reads start answered.
    Just(vec![
        Op::Dir(0),
        Op::Dir(1),
        Op::Dir(2),
        Op::Dir(3),
        Op::File(4),
        Op::File(6),
        Op::Link(9, 1),
        Op::Advance(10_000),
    ])
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// For all generated changes and clocks, a check that holds is right: its
    /// query, asked of the disk at that moment, gets the answer recorded.
    #[test]
    fn a_check_that_holds_is_right(
        granularity in granularity(),
        setup in setup(),
        ops in prop::collection::vec(op(), 1..40),
    ) {
        run(granularity, &setup, &ops, &mut Seen::default())?;
    }
}

/// One step of a run whose reads are recorded again
/// ([`run_recording_again`]).
#[derive(Clone, Debug)]
enum Step {
    /// A change to the disk, or the clock (no [`Op::Generation`]).
    Op(Op),
    /// Record the reads of holder `holder` again, as the disk answers now,
    /// through [`ReadChecks::check`], as a value evaluated again does. With
    /// `retain`, its old checks are released only once the new ones are made,
    /// as the workspace holds a dropped evaluation's
    /// (`crate::workspace::Workspace::retire`); otherwise first. With
    /// `inside`, the record is made inside a generation of its own, as the
    /// server evaluates; otherwise between generations.
    Record {
        holder: usize,
        retain: bool,
        inside: bool,
    },
    /// Release every check holder `holder` holds.
    Release { holder: usize },
    /// Begin a generation and confirm every holder's checks.
    Generation,
}

/// How many holders a run keeps. Holders 0 and 1 are confirmed by the
/// checker that made their checks, as an evaluation and an env sharing reads
/// are; holder 2's checks are made by that checker and confirmed by another,
/// which must not take the first's witnesses for its own. No holder of the
/// server's does that (`crate::workspace::Workspace::owns`), and the two
/// checkers then take turns replacing each other's witness on the checks
/// they share, so holder 2 starts empty.
const HOLDERS: usize = 3;

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        10 => op()
            .prop_filter("a generation is its own step", |op| !matches!(op, Op::Generation))
            .prop_map(Step::Op),
        4 => (0..HOLDERS, any::<bool>(), any::<bool>())
            .prop_map(|(holder, retain, inside)| Step::Record { holder, retain, inside }),
        1 => (0..HOLDERS).prop_map(|holder| Step::Release { holder }),
        4 => Just(Step::Generation),
    ]
}

/// What a run whose reads are recorded again saw, for the non-vacuity check.
#[derive(Default, Debug)]
struct Recorded {
    /// Checks a record made that shared a check another holder, or the
    /// holder's own retained read set, held.
    shared: usize,
    /// Listings and canonicalisations that held in the first generation
    /// after their record without being asked: confirmed by a witness the
    /// record shared.
    reused: usize,
    /// Checks that came into a generation with a witness, shared or taken
    /// for themselves, and found the disk moved.
    shared_moved: usize,
}

/// Every non-text query at every path, recorded against the model as it is,
/// each as `checker`'s check of it, which must be a check of that read: a
/// check shared with another read would confirm the other's answer.
fn recorded(model: &Model, checker: &ReadChecks<Rc<RefCell<Model>>>) -> Vec<Check> {
    let queries = [
        Observation::Kind(Ok(EntryKind::File)),
        Observation::LinkKind(Ok(EntryKind::File)),
        Observation::Listing(Ok(Vec::new())),
        Observation::Canonical(Ok(PathBuf::new())),
    ];
    PATHS
        .iter()
        .flat_map(|p| {
            queries.iter().map(move |query| {
                let path = PathBuf::from(p);
                let observation = model.answer(&path, query);
                let read = Read { path, observation };
                let check = checker.check(&read);
                assert_eq!(check.read(), &read, "a check of another read");
                check
            })
        })
        .collect()
}

/// The property behind [`ReadChecks::check`]: for all generated changes, and
/// records of the reads again at any point, a check confirmed with the
/// witnesses its record shared accepts exactly when a fresh confirmation
/// (the query asked of the disk now) gets the answer recorded, in every
/// generation no change lands inside of; in one that a change does land
/// inside of, as [`run`] holds unshared checks, it accepts only if the
/// answer was right as the generation began or is right now.
fn run_recording_again(
    granularity: i128,
    setup: &[Op],
    steps: &[Step],
    seen: &mut Recorded,
) -> Result<(), TestCaseError> {
    let model = Rc::new(RefCell::new(Model::new(granularity)));
    for op in setup {
        model.borrow_mut().apply(op);
    }
    let mut checker = ReadChecks::with_disk(Rc::clone(&model));
    let mut other = ReadChecks::with_disk(Rc::clone(&model));
    let mut holders: Vec<Vec<Check>> = (0..HOLDERS)
        .map(|holder| match holder {
            2 => Vec::new(),
            _ => recorded(&model.borrow(), &checker),
        })
        .collect();
    // Which holders were recorded since the last generation.
    let mut fresh = [false; HOLDERS];
    for step in steps {
        match step {
            Step::Op(Op::Stealth(change)) => {
                model.borrow_mut().apply(&Op::Stealth(change.clone()));
                let named = change.named();
                checker.hint(named.iter().map(PathBuf::as_path));
                other.hint(named.iter().map(PathBuf::as_path));
            }
            Step::Op(op) => model.borrow_mut().apply(op),
            Step::Record {
                holder,
                retain,
                inside,
            } => {
                if *inside {
                    checker.begin_generation(Nanos(model.borrow().clock));
                }
                let old = if *retain {
                    std::mem::take(&mut holders[*holder])
                } else {
                    holders[*holder].clear();
                    Vec::new()
                };
                let new = recorded(&model.borrow(), &checker);
                let held: Vec<&Check> = holders.iter().flatten().chain(&old).collect();
                seen.shared += new
                    .iter()
                    .filter(|check| held.iter().any(|held| held.shares(check)))
                    .count();
                holders[*holder] = new;
                drop(old);
                fresh[*holder] = true;
                if *inside {
                    checker.end_generation();
                }
            }
            Step::Release { holder } => holders[*holder].clear(),
            Step::Generation => {
                let now = model.borrow().clock;
                let inside = {
                    let model = model.borrow();
                    model.interleaved.is_some() || model.interleaved_at_stat.is_some()
                };
                let truth = |check: &Check| {
                    model
                        .borrow()
                        .answer(&check.read().path, &check.read().observation)
                };
                let before: Vec<Vec<Observation>> = holders
                    .iter()
                    .map(|checks| checks.iter().map(truth).collect())
                    .collect();
                checker.begin_generation(Nanos(now));
                other.begin_generation(Nanos(now));
                for (holder, checks) in holders.iter().enumerate() {
                    let confirming = if holder == 2 { &other } else { &checker };
                    for (check, before) in checks.iter().zip(&before[holder]) {
                        let shared = check.shared.witness.borrow().is_some();
                        let asked = |counts: ReadCounts| counts.listings + counts.canonicalizations;
                        let asked_before = asked(confirming.counts_so_far());
                        let holds = confirming.holds(check);
                        if fresh[holder]
                            && holds
                            && asked(confirming.counts_so_far()) == asked_before
                            && matches!(
                                check.read().observation,
                                Observation::Listing(_) | Observation::Canonical(_)
                            )
                        {
                            seen.reused += 1;
                        }
                        let now = truth(check);
                        let recorded = &check.read().observation;
                        if inside {
                            prop_assert!(
                                !holds || now == *recorded || before == recorded,
                                "{:?} held, but the disk moved: {:?}",
                                check.read().path,
                                now
                            );
                        } else {
                            prop_assert_eq!(
                                holds,
                                now == *recorded,
                                "holder {} at {:?}: recorded {:?}, the disk answers {:?}",
                                holder,
                                check.read().path,
                                recorded,
                                now
                            );
                        }
                        if shared && !holds {
                            seen.shared_moved += 1;
                        }
                    }
                }
                checker.end_generation();
                other.end_generation();
                fresh = [false; HOLDERS];
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// For all generated changes and records again, a shared witness
    /// accepts exactly when a fresh confirmation would
    /// ([`run_recording_again`]).
    #[test]
    fn a_shared_witness_accepts_exactly_when_a_fresh_confirmation_would(
        granularity in granularity(),
        setup in setup(),
        steps in prop::collection::vec(step(), 1..48),
    ) {
        run_recording_again(granularity, &setup, &steps, &mut Recorded::default())?;
    }
}

/// The generator reaches what [`run_recording_again`] is about: records that
/// share checks, generations after a record confirmed by shared witnesses
/// alone, and shared witnesses that found the disk moved.
#[test]
fn the_record_generator_reaches_shared_and_moved_witnesses() {
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let mut seen = Recorded::default();
    for _ in 0..256 {
        let granularity = granularity().new_tree(&mut runner).unwrap().current();
        let setup = setup().new_tree(&mut runner).unwrap().current();
        let steps = prop::collection::vec(step(), 1..48)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        run_recording_again(granularity, &setup, &steps, &mut seen).unwrap();
    }
    assert!(
        seen.shared > 500 && seen.reused > 200 && seen.shared_moved > 50,
        "{seen:?}"
    );
}

/// The generator reaches what the property is about: checks held on their
/// witness alone, and checks that found the disk moved.
#[test]
fn the_generator_reaches_witnessed_and_moved_checks() {
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let mut seen = Seen::default();
    for _ in 0..256 {
        let granularity = granularity().new_tree(&mut runner).unwrap().current();
        let setup = setup().new_tree(&mut runner).unwrap().current();
        let ops = prop::collection::vec(op(), 1..40)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        run(granularity, &setup, &ops, &mut seen).unwrap();
    }
    assert!(
        seen.witnessed > 50
            && seen.moved > 50
            && seen.born_witnessed > 50
            && seen.born_moved > 50
            && seen.hinted_moved > 50,
        "{seen:?}"
    );
}

/// A listing confirmed by its directory's stamp alone costs one `stat`, and
/// an entry created in the same tick as the stamp is not missed: the stamp is
/// racy, so the listing is asked again until it is old.
#[test]
fn a_change_within_the_tick_of_the_stamp_is_not_missed() {
    let model = Rc::new(RefCell::new(Model::new(SECOND)));
    model.borrow_mut().apply(&Op::Dir(0));
    let check = Check::new(Read {
        path: path(0),
        observation: model
            .borrow()
            .answer(&path(0), &Observation::Listing(Ok(Vec::new()))),
    });
    let mut checker = ReadChecks::with_disk(Rc::clone(&model));
    let generation = |checker: &mut ReadChecks<_>| {
        let now = model.borrow().clock;
        checker.begin_generation(Nanos(now));
        let holds = checker.holds(&check);
        (holds, checker.end_generation())
    };
    // Taken within the window of the directory's stamp: racy, so asked.
    let (holds, counts) = generation(&mut checker);
    assert!(holds);
    assert_eq!(counts.listings, 1);
    // An entry made in the same one-second tick leaves the stamp unchanged.
    model.borrow_mut().apply(&Op::File(4));
    model.borrow_mut().apply(&Op::Advance(10));
    let (holds, _) = generation(&mut checker);
    assert!(!holds, "the racy stamp let a same-tick change through");
    // Undo it and let the stamp grow old: then the stamp alone confirms.
    model.borrow_mut().apply(&Op::Remove(4));
    model.borrow_mut().apply(&Op::Advance(5_000));
    let (holds, counts) = generation(&mut checker);
    assert!(holds);
    assert_eq!(counts.listings, 1, "a witness is taken by asking once more");
    model.borrow_mut().apply(&Op::Advance(5_000));
    let (holds, counts) = generation(&mut checker);
    assert!(holds);
    // The stamp of `/w`, listed.
    assert_eq!((counts.listings, counts.stats), (0, 1), "{counts:?}");
}

/// A listing that failed at some entry is asked again in every generation:
/// the failure can clear without the directory changing (Codex).
#[test]
fn a_listing_with_a_failed_entry_is_asked_every_time() {
    let model = Rc::new(RefCell::new(Model::new(1)));
    model.borrow_mut().apply(&Op::Dir(0));
    model.borrow_mut().apply(&Op::Advance(10_000));
    model.borrow_mut().apply(&Op::Unreadable(true));
    let check = Check::new(Read {
        path: path(0),
        observation: model
            .borrow()
            .answer(&path(0), &Observation::Listing(Ok(Vec::new()))),
    });
    let mut checker = ReadChecks::with_disk(Rc::clone(&model));
    let generation = |checker: &mut ReadChecks<_>| {
        let now = model.borrow().clock;
        checker.begin_generation(Nanos(now));
        let holds = checker.holds(&check);
        checker.end_generation();
        model.borrow_mut().clock += SECOND;
        holds
    };
    assert!(generation(&mut checker));
    assert!(generation(&mut checker));
    model.borrow_mut().apply(&Op::Unreadable(false));
    assert!(
        !generation(&mut checker),
        "a cleared failure was not noticed"
    );
}

/// On the host: a racy stamp a whole-file read took holds while the file
/// holds the bytes that read saw, in a generation or out of one, and not
/// once a same-size rewrite within the window changes them. Once a read that
/// finds them takes a stamp no longer racy, that stamp confirms the check
/// by a `stat`, with no read, until a write moves it.
#[test]
fn a_racy_stamp_holds_while_its_bytes_do_on_the_host() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("A.dll");
    std::fs::write(&path, b"abcd").unwrap();
    let observer = borzoi_msbuild::observed::Observer::new();
    borzoi_msbuild::observed::read_stamped(&observer, &path).unwrap();
    let (reads, _) = observer.finish();
    let check = Check::new(reads.reads()[0].clone());
    assert!(check.read().observation.is_racy_stamp(), "precondition");
    let mut checker: ReadChecks = ReadChecks::default();
    assert!(checker.holds(&check), "outside a generation");
    checker.begin_generation(Nanos(i128::MAX / 2));
    assert!(checker.holds(&check), "inside a generation");
    let racy = checker.end_generation();
    assert_eq!(racy.opens, 1, "{racy:?}");

    std::thread::sleep(RACY_WINDOW + std::time::Duration::from_millis(200));
    checker.begin_generation(Nanos(i128::MAX / 2 + 1));
    assert!(checker.holds(&check));
    assert_eq!(
        checker.end_generation().opens,
        1,
        "the read that takes a settled stamp"
    );
    checker.begin_generation(Nanos(i128::MAX / 2 + 2));
    assert!(checker.holds(&check));
    let settled = checker.end_generation();
    assert_eq!(
        (settled.opens, settled.stats),
        (0, 1),
        "a settled stamp confirms by a `stat`"
    );

    std::fs::write(&path, b"wxyz").unwrap();
    checker.begin_generation(Nanos(i128::MAX / 2 + 3));
    assert!(!checker.holds(&check), "a rewrite left it standing");
    checker.end_generation();
    std::fs::write(&path, b"abcd").unwrap();
    checker.begin_generation(Nanos(i128::MAX / 2 + 4));
    assert!(
        checker.holds(&check),
        "the same bytes written back are what it saw"
    );
    checker.end_generation();

    // A same-size rewrite within the window, with no settled stamp to lean on.
    let fresh = tmp.path().join("B.dll");
    std::fs::write(&fresh, b"abcd").unwrap();
    let observer = borzoi_msbuild::observed::Observer::new();
    borzoi_msbuild::observed::read_stamped(&observer, &fresh).unwrap();
    let check = Check::new(observer.finish().0.reads()[0].clone());
    std::fs::write(&fresh, b"wxyz").unwrap();
    assert!(
        !checker.holds(&check),
        "a same-size rewrite in the window held"
    );
}

/// On the host: a whole-file read of a file that can be `stat`ed but not
/// read records the file's stamp marked unreadable, and that holds, by
/// reading again, while the file stays unreadable (asked as a `stat`, it
/// would never hold, and an env reading it would be rebuilt in every
/// generation); once the file can be read it fails. A `stat` that fails is
/// still recorded and asked as one: a directory above the file that denies
/// search fails the `stat`, and restoring it, with the file left
/// unreadable, must fail it.
#[cfg(unix)]
#[test]
fn a_failed_whole_file_read_holds_while_it_keeps_failing_on_the_host() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("A.dll");
    std::fs::write(&path, b"abcd").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let observer = borzoi_msbuild::observed::Observer::new();
    if borzoi_msbuild::observed::read_stamped(&observer, &path).is_ok() {
        // Running as a user no permission stops: nothing to test.
        return;
    }
    let read = observer.finish().0.reads()[0].clone();
    assert!(
        matches!(&read.observation, Observation::Stamp(Ok(stamp)) if stamp.unreadable),
        "precondition: {read:?}"
    );
    assert!(read.still_holds(), "an unchanged failed read was stale");
    let check = Check::new(read.clone());
    let mut checker: ReadChecks = ReadChecks::default();
    for at in 0..2 {
        checker.begin_generation(Nanos(i128::MAX / 2 + at));
        assert!(checker.holds(&check), "generation {at}");
        checker.end_generation();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!read.still_holds(), "a read that now succeeds still held");
    checker.begin_generation(Nanos(i128::MAX / 2 + 2));
    assert!(
        !checker.holds(&check),
        "a read that now succeeds still held"
    );
    checker.end_generation();

    // A directory above that denies search fails the `stat` itself, which is
    // recorded and asked as one.
    let dir = tmp.path().join("locked");
    let inner = dir.join("B.dll");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&inner, b"abcd").unwrap();
    std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let observer = borzoi_msbuild::observed::Observer::new();
    let _ = borzoi_msbuild::observed::read_stamped(&observer, &inner);
    let read = observer.finish().0.reads()[0].clone();
    let failed_stat = matches!(&read.observation, Observation::Stamp(Err(f)) if !f.is_absent());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(failed_stat, "precondition: {read:?}");
    assert!(
        !read.still_holds(),
        "a `stat` that now succeeds, the file still unreadable, held"
    );
    let check = Check::new(read);
    checker.begin_generation(Nanos(i128::MAX / 2 + 3));
    assert!(
        !checker.holds(&check),
        "a `stat` that now succeeds, the file still unreadable, held"
    );
    checker.end_generation();
}

/// A racy stamp with no digest of what was read never holds, in a
/// generation or out of one, whatever the file now says: a same-size write
/// within its tick leaves every field the same. The window is the `Vfs`'s.
#[test]
fn a_racy_stamp_never_holds() {
    assert_eq!(
        borzoi_msbuild::observed::STAMP_RACY_WINDOW,
        crate::vfs::RACY_WINDOW
    );
    let model = Rc::new(RefCell::new(Model::new(1)));
    let stamp = borzoi_msbuild::observed::FileStamp {
        dev: 1,
        ino: 1,
        len: 3,
        mtime_ns: 0,
        ctime_ns: Some(0),
        racy: true,
        digest: None,
        unreadable: false,
    };
    let check = Check::new(Read {
        path: path(0),
        observation: Observation::Stamp(Ok(stamp)),
    });
    let mut checker = ReadChecks::with_disk(Rc::clone(&model));
    assert!(!checker.holds(&check));
    checker.begin_generation(Nanos(model.borrow().clock));
    assert!(!checker.holds(&check));
    checker.end_generation();
    assert!(!crate::read_checks::a_fact(&check.read().observation));
}
