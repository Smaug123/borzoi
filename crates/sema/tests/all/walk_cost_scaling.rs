//! Resolution and inference cost time linear in the length of every flat
//! repetition form of the grammar.
//!
//! A walk that re-reads what came before at each step — a scope scanned from
//! the top for every name, a dotted path's segments counted again for each
//! segment, a module's declarations searched for its attributes once per
//! declaration — is quadratic in the repeat count, and nothing else notices:
//! the answers are right, the corpus files are short, and a generated file of
//! a few thousand bindings takes seconds on every keystroke.
//!
//! Each shape is resolved and inferred at `n` and [`FACTOR`]` * n` repeats, and
//! each walk's time must grow by at most [`MAX_RATIO`] (linear is 8, quadratic
//! 64). A shape that nests once per repeat stops growing at the parser's
//! height bound, so its ratio falls to about 1.
//!
//! So the tree's *height* is a second dimension, held by its own test: a walk
//! that re-reads the path from the root at each level — a container path
//! hashed once per prefix for every type name, each pattern's subtree walked
//! again for every pattern above it — is quadratic in the height, and a
//! repeat count that crosses the bound cannot see it. Every shape the parser
//! stops at the bound is measured at an eighth of its tallest clean count and
//! at that count; every shape that can sit in nested modules is measured,
//! at a fixed length, under [`MODULE_NEST`]` / `[`FACTOR`] modules and under
//! [`MODULE_NEST`].
//!
//! The shapes are [`SHAPES`], every repetition form of the grammar, and
//! [`FLAT_FORMS`]: repeats that real files make long — bindings that use
//! earlier ones, statements, types, opens — which the grammar's table holds
//! only in a form that binds or uses nothing.
//!
//! Timing is the measure because a cost per repeat is what is held, and only
//! the clock sees every walk at once. The clock is the walking thread's CPU
//! time ([`thread_cpu_time`](borzoi_oracle_harness::thread_cpu)), not the
//! wall: the suite runs on loaded machines, where a wall clock reads a walk of
//! a few milliseconds as however long the scheduler kept its thread off a
//! core. What keeps the CPU time from flaking is the spread: load still slows
//! each instruction somewhat (shared caches, a sibling hyperthread), but a
//! quadratic walk grows 64x, so the bound sits a factor of four below it and
//! twice above linear, and a shape over it is measured again, keeping each
//! length's fastest run, before it fails.

use std::path::PathBuf;
use std::time::Duration;

use borzoi_cst::parser::{parse, parse_sig};
use borzoi_cst::syntax::{AstNode, ImplFile, SigFile};
use borzoi_oracle_harness::alloc_limit;
use borzoi_oracle_harness::repetition_shapes::{
    Case, Form, Kind, MODULE_NEST, SHAPES, SHORT, Shape, nestable_shapes, nested_in_modules_at,
    tallest_clean_count,
};
use borzoi_oracle_harness::stack_probe;
use borzoi_oracle_harness::thread_cpu::cpu_timed;
use borzoi_sema::{
    AssemblyEnv, ProjectFile, SourceFile, SyntaxRecovery, WALK_STACK_BYTES, infer_project,
    qualified_names, resolve_project_files,
};

/// How many times longer the long form is than the short. A walk's fixed cost
/// per repeat is large beside a quadratic term's at any length a test can
/// afford, so the spread must be wide for the quadratic term to show: from
/// `n` to `4n`, a scope scanned once per binding grew a debug build's
/// resolution only 7.7x.
const FACTOR: usize = 8;

/// The most a walk's time may grow from `n` to [`FACTOR`]` * n` repeats: twice
/// linear, and a quarter of quadratic.
const MAX_RATIO: f64 = 16.0;

/// Added to the allowance, so a walk too fast to time at either length (a
/// shape the walk declines at once, a nested shape stopped at the bound) does
/// not fail on the noise CPU time keeps.
const SLACK: Duration = Duration::from_millis(20);

/// How often a shape over the allowance is measured again before it fails:
/// each length keeps its fastest run, which a loaded machine slows least.
const RETRIES: usize = 3;

/// The repeat count at the short length. `BORZOI_SCALING_N` overrides it.
const N: usize = 512;

const fn flat(
    name: &'static str,
    head: &'static str,
    unit: &'static str,
    tail: &'static str,
) -> Shape {
    Shape {
        name,
        kind: Kind::Impl,
        head,
        unit,
        tail,
        indent: 0,
    }
}

/// Flat repeats real files make long, with names that resolve to something:
/// [`PRELUDE`](borzoi_oracle_harness::repetition_shapes::PRELUDE) binds `f`
/// and `b`.
const FLAT_FORMS: &[Shape] = &[
    flat("lets_using_earlier", "", "let a = f b\n", ""),
    flat("functions_using_earlier", "", "let a (x: int) = f x\n", ""),
    flat("module_lets", "module M =\n", "  let a = b\n", ""),
    flat(
        "namespace_module_lets",
        "namespace N\nmodule M =\n",
        "  let a = 0\n",
        "",
    ),
    flat("opens", "", "open System\n", ""),
    flat("records", "", "type R = { X: int }\n", ""),
    // Distinct names: a walk sets duplicate declarations aside, so identical
    // repeats never reach its per-declaration work.
    flat("distinct_unions", "", "type T{i} = C{i} of int\n", ""),
    flat("distinct_records", "", "type R{i} = { X{i}: int }\n", ""),
    flat("distinct_lets", "", "let a{i} = b\n", ""),
    flat(
        "statements",
        "let g () = ()\nlet h () =\n",
        "  g ()\n",
        "  ()\n",
    ),
    flat(
        "member_statements",
        "let h (s: string) =\n",
        "  s.Length |> ignore\n",
        "  ()\n",
    ),
];

/// A walk of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Walk {
    Resolution,
    Inference,
}

/// The walks known to grow faster than linear in a shape, which are not held
/// to the bound (#525). Each says why. An exempt resolution exempts the
/// shape's inference too, which reads what resolution returns.
const EXEMPT: &[(&str, Walk, &str)] = &[
    (
        "module_long_ident",
        Walk::Resolution,
        "a header of n segments declares its n - 1 enclosing namespaces, \
         each held as its own path: n² segments in all",
    ),
    (
        "distinct_records",
        Walk::Inference,
        "quadratic before the flat syntax table too (59x from 512 to 4096 \
         records on 7b4a7975); the cause is not yet diagnosed",
    ),
    (
        "member_statements",
        Walk::Inference,
        "each statement settles the binding's constraints so far, and a \
         member access that cannot resolve stays parked and is read again \
         by every later settle",
    ),
];

fn exempt(shape: &Shape, walk: Walk) -> bool {
    EXEMPT
        .iter()
        .any(|&(name, w, _)| name == shape.name && (w == walk || w == Walk::Resolution))
}

fn parse_kind(kind: Kind, src: &str) -> borzoi_cst::parser::Parse {
    match kind {
        Kind::Impl => parse(src),
        Kind::Sig => parse_sig(src),
    }
}

/// The calling thread's CPU time resolving, then (if `infer`) inferring, `src`
/// as a one-file project.
fn walk_times(kind: Kind, src: &str, infer: bool) -> (Duration, Duration) {
    let parsed = parse_kind(kind, src);
    let root = parsed.root.clone();
    let (file, path) = match kind {
        Kind::Impl => (
            SourceFile::Impl(ImplFile::cast(root).expect("an implementation file")),
            "/probe/Probe.fs",
        ),
        Kind::Sig => (
            SourceFile::Sig(SigFile::cast(root).expect("a signature file")),
            "/probe/Probe.fsi",
        ),
    };
    let qnof = qualified_names(std::slice::from_ref(&file), &[PathBuf::from(path)])
        .pop()
        .expect("one name per file");
    let files = vec![ProjectFile::new(file, qnof, SyntaxRecovery::of(&parsed))];
    let env = AssemblyEnv::default();
    let (resolved, resolving) = cpu_timed(|| resolve_project_files(&files, &env));
    let ((), inferring) = cpu_timed(|| {
        if infer {
            drop(infer_project(&files, &resolved, &env));
        }
    });
    drop(resolved);
    (resolving, inferring)
}

/// `shape` at `n` repeats, after the prelude where it can stand.
fn source(shape: &'static Shape, n: usize) -> String {
    Case {
        shape,
        nested: false,
        form: Form::Long,
    }
    .source(n)
}

/// A shape's fastest times at `n` and [`FACTOR`]` * n` over `runs` runs each.
#[derive(Clone, Copy)]
struct Timing {
    short: (Duration, Duration),
    long: (Duration, Duration),
}

impl Timing {
    fn measure(shape: &'static Shape, n: usize, runs: usize) -> Timing {
        Timing::measure_sources(
            shape.kind,
            &source(shape, n),
            &source(shape, FACTOR * n),
            !exempt(shape, Walk::Inference),
            runs,
        )
    }

    /// The fastest times of `short_src` and `long_src` over `runs` runs each.
    fn measure_sources(
        kind: Kind,
        short_src: &str,
        long_src: &str,
        infer: bool,
        runs: usize,
    ) -> Timing {
        let mut timing: Option<Timing> = None;
        for _ in 0..runs {
            let short = walk_times(kind, short_src, infer);
            let long = walk_times(kind, long_src, infer);
            timing = Some(match timing {
                None => Timing { short, long },
                Some(t) => Timing {
                    short: (t.short.0.min(short.0), t.short.1.min(short.1)),
                    long: (t.long.0.min(long.0), t.long.1.min(long.1)),
                },
            });
        }
        timing.expect("at least one run")
    }

    /// The walks not `exempt` whose time grew past the bound:
    /// `(walk, short, long)`.
    fn over(&self, exempt: impl Fn(Walk) -> bool) -> Vec<(Walk, Duration, Duration)> {
        let allowed = |short: Duration| short.mul_f64(MAX_RATIO) + SLACK;
        [
            (Walk::Resolution, self.short.0, self.long.0),
            (Walk::Inference, self.short.1, self.long.1),
        ]
        .into_iter()
        .filter(|&(walk, short, long)| !exempt(walk) && long > allowed(short))
        .collect()
    }
}

/// Run `f` on a thread with [`WALK_STACK_BYTES`] of stack: a nesting shape
/// reaches the parser's tallest tree at these lengths.
fn on_case_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(WALK_STACK_BYTES)
            .spawn_scoped(scope, f)
            .expect("spawn a case thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// The short repeat count for a layout-nested shape, given the flat shapes'
/// `n`: a quarter of it, since the source is quadratic in the count, and low
/// enough that [`FACTOR`] times it still parses cleanly.
///
/// Past the parser's height bound the parse stops, and the rest of the
/// source, indentation included, becomes one `ERROR` token: a non-trivia
/// token whose length is quadratic in the count. A walk that reads each
/// token's text once (a signature's name screen splits it into pieces) is
/// linear in the file, yet grows 64x from `n` to [`FACTOR`]` * n` repeats
/// there, so a long form past the bound measures that token, not the walk.
fn indented_count(shape: &'static Shape, n: usize) -> usize {
    let case = Case {
        shape,
        nested: false,
        form: Form::TallestClean,
    };
    let tallest = tallest_clean_count(&case, |src| parse_kind(shape.kind, src).errors.is_empty());
    (n / 4).min(tallest / FACTOR)
}

#[test]
fn every_flat_repetition_walks_in_linear_time() {
    let n = std::env::var("BORZOI_SCALING_N")
        .ok()
        .map_or(N, |s| s.parse().expect("BORZOI_SCALING_N is a count"));
    for (name, _, _) in EXEMPT {
        assert!(
            SHAPES.iter().chain(FLAT_FORMS).any(|s| s.name == *name),
            "exempt shape {name} is not a shape"
        );
    }
    let only = std::env::var("BORZOI_SCALING_ONLY").ok();
    let mut failures = Vec::new();
    for shape in SHAPES.iter().chain(FLAT_FORMS) {
        if only
            .as_deref()
            .is_some_and(|only| !only.split(',').any(|o| o == shape.name))
        {
            continue;
        }
        let n = if shape.indent > 0 {
            let n = on_case_stack(|| indented_count(shape, n));
            for count in [n, FACTOR * n] {
                assert!(
                    on_case_stack(|| parse_kind(shape.kind, &source(shape, count))
                        .errors
                        .is_empty()),
                    "{}: a measured source must parse cleanly at {count} repeats",
                    shape.name
                );
            }
            n
        } else {
            n
        };
        let mut timing = on_case_stack(|| Timing::measure(shape, n, 1));
        println!(
            "{:28} resolution {:?} -> {:?}, inference {:?} -> {:?}",
            shape.name, timing.short.0, timing.long.0, timing.short.1, timing.long.1
        );
        let exempt = |walk| exempt(shape, walk);
        if !timing.over(exempt).is_empty() {
            timing = on_case_stack(|| Timing::measure(shape, n, RETRIES));
        }
        for (walk, short, long) in timing.over(exempt) {
            failures.push(format!(
                "{}: {walk:?} took {short:?} at {n} repeats and {long:?} at {} ({:.1}x)",
                shape.name,
                FACTOR * n,
                long.as_secs_f64() / short.as_secs_f64().max(1e-9),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "walks grew faster than {MAX_RATIO}x from n to {FACTOR}n repeats:\n{}",
        failures.join("\n")
    );
}

/// The most repeats of a shape's body under nested modules: long enough that
/// its names, not the modules, are most of what the walks look up.
const NESTED_BODY: usize = 64;

/// A tall input at two heights: the short one and one [`FACTOR`] times taller.
struct HeightPair {
    name: String,
    shape: &'static Shape,
    short: String,
    long: String,
}

/// Every shape the parser stops at the height bound, at an eighth of its
/// tallest clean count and at that count; then every nestable shape at up to
/// [`NESTED_BODY`] repeats under [`MODULE_NEST`]` / `[`FACTOR`] and under
/// [`MODULE_NEST`] modules. Each source parses cleanly, so no walk declines
/// a recovered declaration and passes vacuously.
fn height_pairs() -> Vec<HeightPair> {
    let clean = |kind: Kind, src: &str| parse_kind(kind, src).errors.is_empty();
    let mut pairs = Vec::new();
    for shape in SHAPES.iter().filter(|shape| shape.indent == 0) {
        let case = Case {
            shape,
            nested: false,
            form: Form::TallestClean,
        };
        let tallest = tallest_clean_count(&case, |src| clean(shape.kind, src));
        if tallest == case.long() {
            // Flat: as long as a case builds it, it is not near the bound.
            continue;
        }
        pairs.push(HeightPair {
            name: shape.name.to_string(),
            shape,
            short: case.source(tallest / FACTOR),
            long: case.source(tallest),
        });
    }
    for shape in nestable_shapes() {
        let case = Case {
            shape,
            nested: true,
            form: Form::Long,
        };
        let at = |repeats: usize, depth: usize| {
            format!(
                "{}{}",
                case.prelude(),
                nested_in_modules_at(&shape.source(repeats), depth)
            )
        };
        // A body that nests once per repeat can reach the bound under the
        // modules at [`NESTED_BODY`]: halve it until it parses cleanly there.
        let mut repeats = NESTED_BODY;
        while repeats > SHORT && !clean(shape.kind, &at(repeats, MODULE_NEST)) {
            repeats /= 2;
        }
        pairs.push(HeightPair {
            name: format!("{} (nested)", shape.name),
            shape,
            short: at(repeats, MODULE_NEST / FACTOR),
            long: at(repeats, MODULE_NEST),
        });
    }
    for pair in &pairs {
        for src in [&pair.short, &pair.long] {
            assert!(
                clean(pair.shape.kind, src),
                "{}: a measured source must parse cleanly",
                pair.name
            );
        }
    }
    pairs
}

#[test]
fn every_nesting_walks_in_linear_time_in_its_height() {
    let only = std::env::var("BORZOI_SCALING_ONLY").ok();
    let pairs = on_case_stack(height_pairs);
    let mut failures = Vec::new();
    for pair in pairs {
        if only
            .as_deref()
            .is_some_and(|only| !only.split(',').any(|o| o == pair.shape.name))
        {
            continue;
        }
        let infer = !exempt(pair.shape, Walk::Inference);
        let measure = |runs| {
            on_case_stack(|| {
                Timing::measure_sources(pair.shape.kind, &pair.short, &pair.long, infer, runs)
            })
        };
        let mut timing = measure(1);
        println!(
            "{:28} resolution {:?} -> {:?}, inference {:?} -> {:?}",
            pair.name, timing.short.0, timing.long.0, timing.short.1, timing.long.1
        );
        let exempt = |walk| exempt(pair.shape, walk);
        if !timing.over(exempt).is_empty() {
            timing = measure(RETRIES);
        }
        for (walk, short, long) in timing.over(exempt) {
            failures.push(format!(
                "{}: {walk:?} took {short:?} and {long:?} at {FACTOR}x the height ({:.1}x)",
                pair.name,
                long.as_secs_f64() / short.as_secs_f64().max(1e-9),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "walks grew faster than {MAX_RATIO}x over {FACTOR}x the tree height:\n{}",
        failures.join("\n")
    );
}

/// The most a resolution's working memory may grow from a layout-nested
/// shape's depth `d` to [`FACTOR`]` * d`. Unlike time, the bytes a walk holds
/// are the same on every run and every machine, so the bound sits nearer
/// linear (8) than the timing tests' [`MAX_RATIO`] does, and far below
/// quadratic (64).
const MAX_MEMORY_RATIO: f64 = 12.0;

/// `nested_modules` under a `module` header: an anonymous root's modules
/// export nothing, so only a named root reaches the walk's export path.
static NAMED_NESTED_MODULES: Shape = Shape {
    name: "named_nested_modules",
    kind: Kind::Impl,
    head: "module N\n",
    unit: "module M =\n",
    tail: "let x = 0\n",
    indent: 2,
};

/// The shapes whose working memory is known to grow faster than linear, each
/// with why. Memory is deterministic, so an entry is held exactly: a shape
/// listed here that comes back within [`MAX_MEMORY_RATIO`] fails until its
/// entry is removed.
const MEMORY_EXEMPT: &[(&str, &str)] = &[(
    "named_nested_modules",
    "each nested module's export is held by its full qualified path, in the \
     walk's name-shadow, real-module and export-declaration sets alike: d \
     paths of up to d segments",
)];

/// The shapes [`nested_modules_resolve_in_linear_memory_in_their_depth`]
/// measures: every layout-nested shape, and [`NAMED_NESTED_MODULES`].
fn memory_shapes() -> impl Iterator<Item = &'static Shape> {
    SHAPES
        .iter()
        .filter(|shape| shape.indent > 0)
        .chain([&NAMED_NESTED_MODULES])
}

/// The [`memory_probe_child`] entry's path, for
/// [`run_case`](stack_probe::run_case).
const MEMORY_CHILD: &str = "walk_cost_scaling::memory_probe_child";

/// The variable a [`memory_probe_child`] reads its case from: a shape's name
/// and a repeat count.
const MEMORY_CASE_VAR: &str = "BORZOI_SEMA_WALK_MEMORY_CASE";

/// What resolving a source holds, in bytes beyond what was held before it
/// began.
#[derive(Clone, Copy, Debug)]
struct ResolutionBytes {
    /// The most held at once.
    peak: usize,
    /// What the result holds once the walk has returned.
    result: usize,
}

impl ResolutionBytes {
    /// What the walk held at its peak beyond its result: its working state.
    fn working(self) -> usize {
        self.peak.saturating_sub(self.result)
    }

    fn spell(self) -> String {
        format!("{} {}", self.peak, self.result)
    }

    fn parse(facts: &str) -> ResolutionBytes {
        let (peak, result) = facts.split_once(' ').expect("a peak and a result");
        ResolutionBytes {
            peak: peak.parse().expect("a byte count"),
            result: result.parse().expect("a byte count"),
        }
    }
}

/// What resolving `src` as a one-file project holds.
fn resolution_bytes(kind: Kind, src: &str) -> ResolutionBytes {
    let parsed = parse_kind(kind, src);
    let root = parsed.root.clone();
    let (file, path) = match kind {
        Kind::Impl => (
            SourceFile::Impl(ImplFile::cast(root).expect("an implementation file")),
            "/probe/Probe.fs",
        ),
        Kind::Sig => (
            SourceFile::Sig(SigFile::cast(root).expect("a signature file")),
            "/probe/Probe.fsi",
        ),
    };
    let qnof = qualified_names(std::slice::from_ref(&file), &[PathBuf::from(path)])
        .pop()
        .expect("one name per file");
    let files = vec![ProjectFile::new(file, qnof, SyntaxRecovery::of(&parsed))];
    let env = AssemblyEnv::default();
    let base = alloc_limit::live_bytes();
    alloc_limit::reset_peak();
    let resolved = resolve_project_files(&files, &env);
    let bytes = ResolutionBytes {
        peak: alloc_limit::peak_bytes().saturating_sub(base),
        result: alloc_limit::live_bytes().saturating_sub(base),
    };
    drop(resolved);
    bytes
}

/// Child entry: resolve one shape at one repeat count, alone in its process
/// so no other test's allocations reach the counter, and report its
/// [`ResolutionBytes`].
#[test]
#[ignore = "child-process entry for walk_cost_scaling's memory probe"]
fn memory_probe_child() {
    let Some(case) = stack_probe::requested_case(MEMORY_CASE_VAR) else {
        return;
    };
    let (name, count) = case.split_once(' ').expect("a shape and a count");
    let count: usize = count.parse().expect("a count");
    let shape = memory_shapes()
        .find(|shape| shape.name == name)
        .expect("a shape's name");
    let bytes = stack_probe::on_stack(WALK_STACK_BYTES, move || {
        resolution_bytes(shape.kind, &source(shape, count))
    });
    stack_probe::report(&bytes.spell());
}

/// A walk that copies the enclosing container's path at every nested module
/// holds `d` paths of up to `d` segments at a depth of `d`: quadratic in the
/// nesting, and invisible to the timing tests at any depth the parser
/// accepts, where the copies are cheap beside the rest of the walk. The bytes
/// the walk holds see it exactly. Each layout-nested shape is resolved at an
/// eighth of its tallest clean count and at that count, both parsing cleanly,
/// so neither depth measures a recovered tail.
///
/// What is held to the bound is the walk's working state, its peak beyond
/// the result it returns. The result names each nested module's export by its
/// full qualified path, so an implementation file's result is itself
/// quadratic in the nesting, as `module_long_ident`'s is in a header's
/// segments ([`EXEMPT`]); it is printed, not bounded. Under a named root the
/// walk holds those paths too ([`MEMORY_EXEMPT`]).
#[test]
fn nested_modules_resolve_in_linear_memory_in_their_depth() {
    for (name, _) in MEMORY_EXEMPT {
        assert!(
            memory_shapes().any(|shape| shape.name == *name),
            "memory-exempt shape {name} is not measured"
        );
    }
    let mut failures = Vec::new();
    for shape in memory_shapes() {
        let case = Case {
            shape,
            nested: false,
            form: Form::TallestClean,
        };
        let tallest = on_case_stack(|| {
            tallest_clean_count(&case, |src| parse_kind(shape.kind, src).errors.is_empty())
        });
        let short = tallest / FACTOR;
        let long = FACTOR * short;
        let bytes = |count: usize| -> ResolutionBytes {
            match stack_probe::run_case(
                MEMORY_CHILD,
                MEMORY_CASE_VAR,
                &format!("{} {count}", shape.name),
            ) {
                stack_probe::Outcome::Survived(facts) => ResolutionBytes::parse(&facts),
                stack_probe::Outcome::Overflowed => {
                    panic!("{}: resolving {count} repeats overflowed", shape.name)
                }
            }
        };
        let (at_short, at_long) = (bytes(short), bytes(long));
        let ratio = at_long.working() as f64 / at_short.working().max(1) as f64;
        println!(
            "{:28} working {} B at {short} repeats -> {} B at {long} ({ratio:.1}x); \
             result {} B -> {} B",
            shape.name,
            at_short.working(),
            at_long.working(),
            at_short.result,
            at_long.result,
        );
        let exempt = MEMORY_EXEMPT.iter().any(|&(name, _)| name == shape.name);
        if exempt && ratio <= MAX_MEMORY_RATIO {
            failures.push(format!(
                "{}: exempt, yet its working memory grew only {ratio:.1}x: remove its \
                 MEMORY_EXEMPT entry",
                shape.name
            ));
        } else if !exempt && ratio > MAX_MEMORY_RATIO {
            failures.push(format!(
                "{}: working {} B at {short} repeats and {} B at {long} ({ratio:.1}x)",
                shape.name,
                at_short.working(),
                at_long.working(),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "resolution's working memory grew faster than {MAX_MEMORY_RATIO}x over {FACTOR}x the \
         nesting:\n{}",
        failures.join("\n")
    );
}
