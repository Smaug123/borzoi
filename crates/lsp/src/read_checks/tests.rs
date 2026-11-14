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
    for op in ops {
        if !matches!(op, Op::Generation) {
            model.borrow_mut().apply(op);
            continue;
        }
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
        seen.witnessed > 50 && seen.moved > 50 && seen.born_witnessed > 50 && seen.born_moved > 50,
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
/// read records the read's failure, and that holds, by reading again, while
/// the file stays unreadable (asked as a `stat`, it would never hold, and an
/// env reading it would be rebuilt in every generation); once the file can
/// be read it fails.
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
        matches!(&read.observation, Observation::Stamp(Err(f)) if !f.is_absent()),
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
