//! The [`Vfs`] against a model disk with a generated clock.
//!
//! The model's timestamps tick at a generated granularity up to
//! [`RACY_WINDOW`], so the properties cover coarse-clock filesystems (HFS+'s
//! one second, FAT's two) that the machine running the tests does not have.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use proptest::prelude::*;

use super::*;

const SECOND: i128 = 1_000_000_000;
const WINDOW: i128 = RACY_WINDOW.as_nanos() as i128;

#[derive(Clone, Debug)]
struct ModelFile {
    bytes: Vec<u8>,
    ino: u64,
    mtime: i128,
    ctime: i128,
}

/// A disk whose clock the test moves, and whose timestamps are that clock
/// rounded down to `granularity`.
#[derive(Debug)]
struct Model {
    files: BTreeMap<PathBuf, ModelFile>,
    clock: i128,
    granularity: i128,
    next_ino: u64,
    /// A write that lands between the next read's stamp and its bytes.
    interleaved: Option<(PathBuf, Vec<u8>)>,
}

impl Model {
    fn new(granularity: i128) -> Model {
        Model {
            files: BTreeMap::new(),
            clock: 100 * SECOND,
            granularity,
            next_ino: 1,
            interleaved: None,
        }
    }

    fn tick(&self) -> i128 {
        self.clock.div_euclid(self.granularity) * self.granularity
    }

    fn stamp(file: &ModelFile) -> Stamp {
        Stamp {
            dev: 1,
            ino: file.ino,
            size: file.bytes.len() as u64,
            mtime: Nanos(file.mtime),
            ctime: Some(Nanos(file.ctime)),
        }
    }

    /// Write `bytes` in place (an existing file keeps its inode). Returns
    /// whether the file's stamp is unchanged although its bytes moved: the
    /// write only the racy rule can see.
    fn write(&mut self, path: &Path, bytes: &[u8]) -> bool {
        let now = self.tick();
        let before = self.files.get(path).cloned();
        let ino = match &before {
            Some(file) => file.ino,
            None => {
                self.next_ino += 1;
                self.next_ino
            }
        };
        let file = ModelFile {
            bytes: bytes.to_vec(),
            ino,
            mtime: now,
            ctime: now,
        };
        let hidden = before
            .is_some_and(|old| Model::stamp(&old) == Model::stamp(&file) && old.bytes != bytes);
        self.files.insert(path.to_path_buf(), file);
        hidden
    }

    fn delete(&mut self, path: &Path) {
        self.files.remove(path);
    }

    /// What a client would hold for `path` if it opened it now.
    fn truth(&self, path: &Path) -> Option<String> {
        let file = self.files.get(path)?;
        let text = String::from_utf8(file.bytes.clone()).ok()?;
        Some(crate::disk_text::without_bom(text))
    }
}

impl Disk for Rc<RefCell<Model>> {
    fn stat(&self, path: &Path) -> io::Result<Stamp> {
        let model = self.borrow();
        model
            .files
            .get(path)
            .map(Model::stamp)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn read(&self, path: &Path) -> io::Result<(Stamp, Vec<u8>)> {
        let mut model = self.borrow_mut();
        let stamp = model
            .files
            .get(path)
            .map(Model::stamp)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if model
            .interleaved
            .as_ref()
            .is_some_and(|(target, _)| target == path)
        {
            let (target, bytes) = model.interleaved.take().unwrap();
            model.write(&target, &bytes);
        }
        let bytes = model.files[path].bytes.clone();
        Ok((stamp, bytes))
    }
}

const PATHS: [&str; 3] = ["/w/A.fs", "/w/B.fs", "/w/sub/C.fs"];

/// Contents, several of one size, one an earlier one behind a byte-order mark
/// (so the same text again), one not UTF-8.
const CONTENTS: [&[u8]; 6] = [
    b"let a = 1\n",
    b"let b = 1\n",
    b"let a = 2\n",
    b"\xef\xbb\xbflet a = 1\n",
    b"x\n",
    b"\xff\xfe\n",
];

fn path(i: usize) -> PathBuf {
    PathBuf::from(PATHS[i])
}

fn uri(i: usize) -> Uri {
    Uri::from_file_path(path(i)).unwrap()
}

#[derive(Clone, Debug)]
enum Op {
    /// Move the clock forward by this many milliseconds.
    Advance(u32),
    Write(usize, usize),
    Delete(usize),
    /// Open, or change, the buffer for a path.
    Buffer(usize, usize),
    Close(usize),
    /// End any open generation, begin one at the clock's time, and read every
    /// path.
    Generation,
    /// Inside the open generation, write a path, then read every path again.
    MidGenerationWrite(usize, usize),
    /// The next read of a path finds this write landing between its stamp
    /// and its bytes.
    Interleave(usize, usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let path = 0..PATHS.len();
    let content = 0..CONTENTS.len();
    prop_oneof![
        4 => prop_oneof![Just(0u32), Just(1), Just(400), Just(900), Just(1900), Just(2100), Just(3000)]
            .prop_map(Op::Advance),
        4 => (path.clone(), content.clone()).prop_map(|(p, c)| Op::Write(p, c)),
        1 => path.clone().prop_map(Op::Delete),
        1 => (path.clone(), content.clone()).prop_map(|(p, c)| Op::Buffer(p, c)),
        1 => path.clone().prop_map(Op::Close),
        5 => Just(Op::Generation),
        1 => (path.clone(), content.clone()).prop_map(|(p, c)| Op::MidGenerationWrite(p, c)),
        1 => (path, content).prop_map(|(p, c)| Op::Interleave(p, c)),
    ]
}

fn granularity_strategy() -> impl Strategy<Value = i128> {
    prop_oneof![Just(1), Just(1_000_000), Just(SECOND), Just(WINDOW),]
}

/// What the run exercised, so a generator that never reaches a case cannot
/// pass silently.
#[derive(Debug, Default)]
struct Exercised {
    /// A generation read new bytes behind a stamp the write left unchanged.
    racy_catches: usize,
    /// A generation in which some disk path was confirmed by its stamp alone.
    stamp_only_reads: usize,
    /// A read of a path that was absent at the previous generation and still is.
    absent_rechecks: usize,
}

/// Per path, what the model knows about the [`Vfs`]'s last open of it.
#[derive(Clone, Copy, Debug, Default)]
struct LastOpen {
    /// The generation time of the open, if the path has been opened.
    at: Option<i128>,
    /// Whether the file has been written or deleted since.
    moved: bool,
}

fn run(granularity: i128, ops: &[Op], exercised: &mut Exercised) -> Result<(), TestCaseError> {
    let model = Rc::new(RefCell::new(Model::new(granularity)));
    for (i, content) in [0, 4].into_iter().enumerate() {
        model.borrow_mut().write(&path(i), CONTENTS[content]);
    }
    let mut vfs = Vfs::with_disk(Rc::clone(&model));
    let mut buffers: BTreeMap<usize, String> = BTreeMap::new();
    // What each path read from disk in the open generation, for the
    // within-a-generation property.
    let mut snapshot: BTreeMap<usize, Option<Arc<str>>> = BTreeMap::new();
    let mut in_generation = false;
    let mut last_open = [LastOpen::default(); PATHS.len()];
    let mut hidden: BTreeSet<usize> = BTreeSet::new();
    let mut absent_at_last_generation: BTreeSet<usize> = BTreeSet::new();

    for op in ops {
        match *op {
            Op::Advance(ms) => model.borrow_mut().clock += i128::from(ms) * 1_000_000,
            Op::Write(p, c) => {
                if model.borrow_mut().write(&path(p), CONTENTS[c]) {
                    hidden.insert(p);
                }
                last_open[p].moved = true;
            }
            Op::Delete(p) => {
                model.borrow_mut().delete(&path(p));
                last_open[p].moved = true;
            }
            Op::Buffer(p, c) => {
                let text = String::from_utf8_lossy(CONTENTS[c]).into_owned();
                vfs.insert(uri(p), text.clone());
                buffers.insert(p, text);
            }
            Op::Close(p) => {
                vfs.remove(&uri(p));
                buffers.remove(&p);
            }
            Op::Interleave(p, c) => {
                model.borrow_mut().interleaved = Some((path(p), CONTENTS[c].to_vec()));
                last_open[p].moved = true;
            }
            Op::Generation => {
                if in_generation {
                    vfs.end_generation();
                }
                let now = model.borrow().clock;
                vfs.begin_generation(Nanos(now));
                in_generation = true;
                snapshot.clear();
                let mut confirmed_by_stamp = false;
                #[allow(clippy::needless_range_loop)] // `p` names a path, not only an index
                for p in 0..PATHS.len() {
                    let before = vfs.generation_counts();
                    let got = vfs.text_of(&path(p));
                    let after = vfs.generation_counts();
                    let (opens, stats) = (after.opens - before.opens, after.stats - before.stats);
                    if let Some(buffer) = buffers.get(&p) {
                        prop_assert_eq!(got.as_deref(), Some(buffer.as_str()), "buffer {}", p);
                        prop_assert_eq!((opens, stats), (0, 0), "a buffer read touched the disk");
                        continue;
                    }
                    // The first disk read of a generation is exact, whatever
                    // the granularity and however recent the write.
                    let truth = model.borrow().truth(&path(p));
                    prop_assert_eq!(
                        got.as_deref(),
                        truth.as_deref(),
                        "path {} at {} (granularity {})",
                        p,
                        now,
                        granularity
                    );
                    if hidden.remove(&p) {
                        exercised.racy_catches += 1;
                    }
                    // Cost: at most one stat and one open, and an unchanged,
                    // non-racy file costs exactly one stat.
                    prop_assert!(
                        opens <= 1 && stats <= 1,
                        "path {p}: {opens} opens, {stats} stats"
                    );
                    let present = model.borrow().files.get(&path(p)).cloned();
                    if let (
                        Some(file),
                        LastOpen {
                            at: Some(at),
                            moved: false,
                        },
                    ) = (&present, last_open[p])
                        && file.mtime + WINDOW < at
                        && file.ctime + WINDOW < at
                    {
                        prop_assert_eq!(
                            (opens, stats),
                            (0, 1),
                            "path {} was not confirmed by its stamp",
                            p
                        );
                        confirmed_by_stamp = true;
                    }
                    if present.is_none() && absent_at_last_generation.contains(&p) {
                        prop_assert_eq!(opens, 0, "a known-absent path was opened");
                        exercised.absent_rechecks += 1;
                    }
                    if present.is_none() {
                        absent_at_last_generation.insert(p);
                    } else {
                        absent_at_last_generation.remove(&p);
                    }
                    if opens == 1 {
                        last_open[p] = LastOpen {
                            at: Some(now),
                            moved: false,
                        };
                    }
                    snapshot.insert(p, got);
                }
                if confirmed_by_stamp {
                    exercised.stamp_only_reads += 1;
                }
            }
            Op::MidGenerationWrite(p, c) => {
                if model.borrow_mut().write(&path(p), CONTENTS[c]) {
                    hidden.insert(p);
                }
                last_open[p].moved = true;
                if !in_generation {
                    continue;
                }
                let before = vfs.generation_counts();
                for (q, seen) in &snapshot {
                    if buffers.contains_key(q) {
                        continue;
                    }
                    let got = vfs.text_of(&path(*q));
                    match (&got, seen) {
                        (Some(got), Some(seen)) => prop_assert!(
                            Arc::ptr_eq(got, seen),
                            "path {} changed within a generation",
                            q
                        ),
                        _ => prop_assert_eq!(&got, seen, "path {} changed within a generation", q),
                    }
                }
                prop_assert_eq!(
                    vfs.generation_counts(),
                    before,
                    "a re-read within a generation touched the disk"
                );
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// For all disks ticking at a granularity up to [`RACY_WINDOW`], and all
    /// interleavings of writes, deletes, buffer edits and clock moves, every
    /// generation's first read of a path is the buffer if open, else the
    /// disk's text without a byte-order mark; later reads in that generation
    /// return the same allocation without touching the disk; and an unchanged
    /// file whose stamp is no longer racy costs exactly one `stat`.
    #[test]
    fn every_generation_reads_the_current_disk(
        granularity in granularity_strategy(),
        ops in prop::collection::vec(op_strategy(), 1..60),
    ) {
        run(granularity, &ops, &mut Exercised::default())?;
    }
}

/// The property above must reach the cases it exists for. Run over a fixed
/// seed's worth of generated cases, it must see a write that left the stamp
/// unchanged be caught, an unchanged file be confirmed by its stamp alone, and
/// a known-absent path be re-checked without an open.
#[test]
fn the_generator_reaches_the_racy_and_the_stamp_only_cases() {
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config {
            cases: 512,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[7; 32]),
    );
    let exercised = RefCell::new(Exercised::default());
    runner
        .run(
            &(
                granularity_strategy(),
                prop::collection::vec(op_strategy(), 1..60),
            ),
            |(granularity, ops)| run(granularity, &ops, &mut exercised.borrow_mut()),
        )
        .unwrap();
    let exercised = exercised.into_inner();
    eprintln!("{exercised:?}");
    assert!(
        exercised.racy_catches > 0
            && exercised.stamp_only_reads > 0
            && exercised.absent_rechecks > 0,
        "{exercised:?}"
    );
}

/// The racy hole, step by step, on a disk with one-second timestamps: a
/// same-size rewrite within the tick of the stamp leaves the stamp unchanged,
/// and must still be read.
#[test]
fn a_same_size_write_within_the_window_of_the_stamp_is_not_missed() {
    let model = Rc::new(RefCell::new(Model::new(SECOND)));
    let a = path(0);
    model.borrow_mut().clock = 100 * SECOND;
    model.borrow_mut().write(&a, b"let a = 1\n");
    let mut vfs = Vfs::with_disk(Rc::clone(&model));
    let generation_at = |vfs: &mut Vfs<Rc<RefCell<Model>>>, at: i128| {
        model.borrow_mut().clock = at;
        vfs.begin_generation(Nanos(at));
        let text = vfs.text_of(&a);
        let counts = vfs.end_generation();
        (
            text.as_deref().map(str::to_string),
            counts.opens,
            counts.stats,
        )
    };

    assert_eq!(
        generation_at(&mut vfs, 100 * SECOND + SECOND / 2),
        (Some("let a = 1\n".into()), 1, 0)
    );
    // Same size, same one-second tick: the stamp does not move.
    model.borrow_mut().clock = 100 * SECOND + 7 * SECOND / 10;
    assert!(model.borrow_mut().write(&a, b"let b = 1\n"));
    assert_eq!(
        generation_at(&mut vfs, 101 * SECOND),
        (Some("let b = 1\n".into()), 1, 0),
        "a racy stamp is re-read"
    );
    // Still within the window of the mtime when last read: read again.
    assert_eq!(
        generation_at(&mut vfs, 103 * SECOND + SECOND / 2),
        (Some("let b = 1\n".into()), 1, 0)
    );
    // Read more than the window after the mtime: the stamp alone now suffices.
    assert_eq!(
        generation_at(&mut vfs, 104 * SECOND),
        (Some("let b = 1\n".into()), 0, 1)
    );
}

/// An unchanged text keeps its allocation across a racy re-read, so folds and
/// parse caches holding it still compare by pointer.
#[test]
fn a_re_read_of_unchanged_text_keeps_its_allocation() {
    let model = Rc::new(RefCell::new(Model::new(1)));
    let a = path(0);
    model.borrow_mut().write(&a, b"let a = 1\n");
    let mut vfs = Vfs::with_disk(Rc::clone(&model));
    vfs.begin_generation(Nanos(model.borrow().clock));
    let first = vfs.text_of(&a).unwrap();
    vfs.end_generation();
    vfs.begin_generation(Nanos(model.borrow().clock));
    let second = vfs.text_of(&a).unwrap();
    assert_eq!(vfs.end_generation().opens, 1, "the stamp is racy");
    assert!(Arc::ptr_eq(&first, &second));
}

/// A leading byte-order mark is dropped from disk text, and kept in a buffer.
/// A stamp with no change time cannot rule a rewrite out, so it is racy
/// however old its mtime.
#[test]
fn a_stamp_without_a_change_time_is_always_racy() {
    let stamp = Stamp {
        dev: 0,
        ino: 0,
        size: 10,
        mtime: Nanos(0),
        ctime: None,
    };
    assert!(stamp.is_racy(Nanos(1_000 * SECOND)));
    assert!(
        !Stamp {
            ctime: Some(Nanos(0)),
            ..stamp
        }
        .is_racy(Nanos(1_000 * SECOND))
    );
}

#[test]
fn a_disk_read_drops_a_leading_mark() {
    let model = Rc::new(RefCell::new(Model::new(1)));
    model
        .borrow_mut()
        .write(&path(0), "\u{feff}let x = \"\u{feff}\"\n".as_bytes());
    let mut vfs = Vfs::with_disk(Rc::clone(&model));
    assert_eq!(
        vfs.text_of(&path(0)).as_deref(),
        Some("let x = \"\u{feff}\"\n")
    );
    vfs.insert(uri(1), "\u{feff}let y = 1\n".to_string());
    assert_eq!(
        vfs.text_of(&path(1)).as_deref(),
        Some("\u{feff}let y = 1\n")
    );
}

/// A watch event's hint makes the next read re-read, even in the same
/// generation and behind an unchanged stamp.
#[test]
fn a_hint_forgets_what_was_read() {
    let model = Rc::new(RefCell::new(Model::new(1)));
    let a = path(0);
    model.borrow_mut().write(&a, b"let a = 1\n");
    let mut vfs = Vfs::with_disk(Rc::clone(&model));
    vfs.begin_generation(Nanos(model.borrow().clock));
    assert_eq!(vfs.text_of(&a).as_deref(), Some("let a = 1\n"));
    // Rewritten behind an identical stamp, as a remote clock could.
    model.borrow_mut().files.get_mut(&a).unwrap().bytes = b"let b = 1\n".to_vec();
    assert_eq!(vfs.text_of(&a).as_deref(), Some("let a = 1\n"));
    vfs.hint([Path::new("/w/./A.fs")]);
    assert_eq!(vfs.text_of(&a).as_deref(), Some("let b = 1\n"));
}

/// A file open under two spellings reads the first-opened buffer, and
/// `buffer_uri` names that same buffer, so a location computed from the text
/// carries the URI of the text it was computed from.
#[test]
fn text_and_uri_agree_on_which_spelling_is_read() {
    let model = Rc::new(RefCell::new(Model::new(1)));
    let mut vfs = Vfs::with_disk(Rc::clone(&model));
    let first = uri(0);
    let second = Uri::from_file_path("/w/sub/../A.fs").unwrap();
    assert_ne!(first, second);
    vfs.insert(first.clone(), "let first = 1\n".to_string());
    vfs.insert(second.clone(), "let second = 2\n".to_string());
    for _ in 0..2 {
        assert_eq!(vfs.buffer_uri(&path(0)), Some(&first));
        assert_eq!(vfs.text_of(&path(0)).as_deref(), Some("let first = 1\n"));
        // Editing the first keeps it first.
        vfs.insert(first.clone(), "let first = 1\n".to_string());
    }
    vfs.remove(&first);
    assert_eq!(vfs.buffer_uri(&path(0)), Some(&second));
    assert_eq!(vfs.text_of(&path(0)).as_deref(), Some("let second = 2\n"));
    vfs.remove(&second);
    assert_eq!(vfs.buffer_uri(&path(0)), None);
}
