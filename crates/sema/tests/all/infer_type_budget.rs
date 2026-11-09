//! Inference stays within a bounded time, memory and stack on programs whose
//! inferred types grow faster than their text, and declines there rather than
//! publish a type it did not finish.
//!
//! The tree bounds the walks over syntax (`walk_stack_bound.rs`), but not the
//! types inference builds: each line of
//!
//! ```fsharp
//! let g0 x = (x, 0)
//! let g1 x = g0 (g0 x)
//! let g2 x = g1 (g1 x)
//! ```
//!
//! doubles `gK`'s type, and a local `let` that pairs the one before it doubles
//! the type its binder resolves to. Without a bound, a dozen such lines take
//! gigabytes and overflow the stack of every recursive walk over a type. With
//! one ([`MAX_TYPE_NODES`], [`MAX_TYPE_DEPTH`]), inference declines the file
//! once a type passes it ([`InferredFile::type_budget_exceeded`]).
//!
//! Each case runs in a child copy of this binary
//! ([`stack_probe::run_case_limited`]) under a memory allowance, a time limit,
//! and half of [`WALK_STACK_BYTES`], so a regression shows up as the case that
//! ran out of one of them rather than as a wedged or aborted suite.
//!
//! [`InferredFile::type_budget_exceeded`]: borzoi_sema::InferredFile::type_budget_exceeded

use std::path::PathBuf;
use std::time::Duration;

use borzoi_cst::parser::{MAX_TREE_HEIGHT, parse};
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_oracle_harness::alloc_limit;
use borzoi_oracle_harness::par_map::par_map;
use borzoi_oracle_harness::stack_probe::{self, Limited};
use borzoi_sema::{
    AssemblyEnv, MAX_TYPE_DEPTH, MAX_TYPE_NODES, ProjectFile, SourceFile, SyntaxRecovery,
    TypeBudgetExceeded, WALK_STACK_BYTES, infer_project, qualified_names, resolve_project_files,
};

/// The child entry's full test path, for [`stack_probe::run_case_limited`].
const CHILD: &str = "infer_type_budget::probe_child";

/// The variable that hands a child its case.
const CASE_VAR: &str = "BORZOI_SEMA_TYPE_BUDGET_CASE";

/// The memory a case may allocate on top of what the child holds when it
/// starts. A case within the budget needs a few MiB; the doubling family
/// without one passed this at its eleventh line.
const MEMORY_ALLOWANCE: usize = 256 << 20;

/// How long a case may take. A case within the budget takes well under a
/// second when the machine is idle; this bounds a blow-up, not latency.
const CASE_TIMEOUT: Duration = Duration::from_secs(180);

/// The stack every case is walked on, as in `walk_stack_bound.rs`: half of
/// [`WALK_STACK_BYTES`], the margin the bound promises.
const PROBE_STACK_BYTES: usize = WALK_STACK_BYTES / 2;

/// How many lines of each family the sweep tries. Every family passes the
/// budget well before this; without one, each would need exabytes here.
const LINES: usize = 40;

/// A program shape whose inferred type grows faster than its text, by the
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `let gK x = g(K-1) (g(K-1) x)` over `let g0 x = (x, 0)`: a tuple whose
    /// depth doubles by the line.
    Tuple,
    /// The same over `let g0 x y = x`: a curried function whose depth doubles.
    Function,
    /// The same over `let g0 x = Some x`: a generic application whose depth
    /// doubles.
    Application,
    /// `let gK x = g0 (g(K-1) x)` over `let g0 x = (x, x)`: a tuple whose
    /// node count doubles by the line while its depth grows by one.
    PairChain,
    /// Top-level values that pair the one before: `let aK = (a(K-1),
    /// a(K-1))`. Every unification is small; the type first passes the
    /// budget when the read-off resolves the last value.
    TopLevelPairs,
    /// One function whose locals pair the one before: `let aK = (a(K-1),
    /// a(K-1))`, over an `int` parameter. No generalisation is involved: the
    /// doubling is the table's own sharing, which resolving a local's type
    /// expands.
    SharedLocal,
    /// [`Self::SharedLocal`] over an unannotated parameter, so every local is
    /// open and the binding incomplete: inference publishes nothing for it,
    /// but still resolves each local to see that.
    OpenLocal,
}

impl Family {
    const ALL: [Family; 7] = [
        Family::Tuple,
        Family::Function,
        Family::Application,
        Family::PairChain,
        Family::TopLevelPairs,
        Family::SharedLocal,
        Family::OpenLocal,
    ];

    fn name(self) -> &'static str {
        match self {
            Family::Tuple => "tuple",
            Family::Function => "function",
            Family::Application => "application",
            Family::PairChain => "pair-chain",
            Family::TopLevelPairs => "top-level-pairs",
            Family::SharedLocal => "shared-local",
            Family::OpenLocal => "open-local",
        }
    }

    fn named(name: &str) -> Family {
        Family::ALL
            .into_iter()
            .find(|f| f.name() == name)
            .unwrap_or_else(|| panic!("no family {name:?}"))
    }

    /// The family's program, `lines` steps long.
    fn source(self, lines: usize) -> String {
        let doubling = |g0: &str| {
            let mut src = format!("{g0}\n");
            for k in 1..lines {
                src.push_str(&format!("let g{k} x = g{j} (g{j} x)\n", j = k - 1));
            }
            src
        };
        match self {
            Family::Tuple => doubling("let g0 x = (x, 0)"),
            Family::Function => doubling("let g0 x y = x"),
            Family::Application => doubling("let g0 x = Some x"),
            Family::PairChain => {
                let mut src = "let g0 x = (x, x)\n".to_string();
                for k in 1..lines {
                    src.push_str(&format!("let g{k} x = g0 (g{j} x)\n", j = k - 1));
                }
                src
            }
            Family::TopLevelPairs => {
                let mut src = "let a0 = 0\n".to_string();
                for k in 1..lines {
                    src.push_str(&format!("let a{k} = (a{j}, a{j})\n", j = k - 1));
                }
                src
            }
            Family::SharedLocal | Family::OpenLocal => {
                let param = if self == Family::SharedLocal {
                    "(x: int)"
                } else {
                    "x"
                };
                let mut src = format!("let f {param} =\n    let a0 = (x, x)\n");
                for k in 1..lines {
                    src.push_str(&format!("    let a{k} = (a{j}, a{j})\n", j = k - 1));
                }
                src.push_str(&format!("    a{}\n", lines.saturating_sub(1)));
                src
            }
        }
    }
}

/// What a child reports of its case.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Facts {
    /// The bound the file's inference reached, if any.
    exceeded: Option<TypeBudgetExceeded>,
    /// The node count and depth of the largest type the file published (an
    /// expression's or a binder's), `0 0` for none.
    published_nodes: usize,
    published_depth: usize,
    /// The most nodes one resolution built, and the deepest type walk
    /// inference made.
    walked_nodes: usize,
    walked_depth: usize,
    /// The most expression levels inference had open at once.
    expression_nesting: usize,
    /// The most bytes the case held on top of what the child held before it.
    peak_bytes: usize,
}

impl Facts {
    fn spell(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}",
            match self.exceeded {
                None => "within",
                Some(TypeBudgetExceeded::Nodes) => "nodes",
                Some(TypeBudgetExceeded::Depth) => "depth",
            },
            self.published_nodes,
            self.published_depth,
            self.walked_nodes,
            self.walked_depth,
            self.expression_nesting,
            self.peak_bytes
        )
    }

    fn read(s: &str) -> Facts {
        let fields: Vec<&str> = s.split(' ').collect();
        let [exceeded, nodes, depth, walked_nodes, walked, nesting, peak] = fields[..] else {
            panic!("seven fields: {s:?}")
        };
        let num = |f: &str| {
            f.parse::<usize>()
                .unwrap_or_else(|_| panic!("a number: {s:?}"))
        };
        Facts {
            exceeded: match exceeded {
                "within" => None,
                "nodes" => Some(TypeBudgetExceeded::Nodes),
                "depth" => Some(TypeBudgetExceeded::Depth),
                other => panic!("a budget verdict: {other:?}"),
            },
            published_nodes: num(nodes),
            published_depth: num(depth),
            walked_nodes: num(walked_nodes),
            walked_depth: num(walked),
            expression_nesting: num(nesting),
            peak_bytes: num(peak),
        }
    }
}

/// Resolve and infer `src` as a one-file project against FSharp.Core and the
/// BCL, as the language server does every file, on the calling thread.
fn infer_facts(src: &str, env: &AssemblyEnv) -> Facts {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "a case parses cleanly: {:?}",
        parsed.errors
    );
    let recovery = SyntaxRecovery::of(&parsed);
    let file = SourceFile::Impl(ImplFile::cast(parsed.root.clone()).expect("impl root"));
    let path = PathBuf::from("/probe/Probe.fs");
    let qnof = qualified_names(std::slice::from_ref(&file), &[path])
        .pop()
        .expect("one name per file");
    let files = vec![ProjectFile::new(file, qnof, recovery)];
    let resolved = resolve_project_files(&files, env);
    let inferred = infer_project(&files, &resolved, env);
    let inferred = inferred.file(0).expect("an implementation file");
    let largest = inferred
        .types()
        .values()
        .chain(inferred.def_types().values())
        .map(|ty| ty.size())
        .max_by_key(|size| (size.nodes, size.depth))
        .unwrap_or_default();
    let walks = inferred.type_walks();
    Facts {
        exceeded: inferred.type_budget_exceeded(),
        published_nodes: largest.nodes,
        published_depth: largest.depth,
        walked_nodes: walks.resolve.nodes,
        walked_depth: walks.resolve.depth.max(walks.unify.depth),
        expression_nesting: inferred.deepest_expression_walk(),
        peak_bytes: 0,
    }
}

/// A case spelled for the child: `<family> <lines>`, or `deep <additions>`
/// for [`deep_case`], then a tab and [`references`].
fn spell(kind: &str, n: usize) -> String {
    format!("{kind} {n}\t{}", references())
}

/// The references every case is inferred against, FSharp.Core's and
/// `System.Runtime`'s paths, tab-separated. The parent finds them, building
/// fcs-dump (whose output holds FSharp.Core) if it must, so that the children
/// do not race to build it.
fn references() -> String {
    format!(
        "{}\t{}",
        crate::common::ensure_fsharp_core_dll().display(),
        crate::common::ensure_system_runtime_dll().display()
    )
}

/// FSharp.Core, `System.Runtime` and `netstandard`, from [`references`]: the
/// env `crate::common::full_bcl_env` builds.
fn env_from(references: &str) -> AssemblyEnv {
    use borzoi_assembly::Ecma335Assembly;
    let (core, sysrt) = references.split_once('\t').expect("two paths");
    let sysrt = PathBuf::from(sysrt);
    let netstd = sysrt.parent().expect("ref dir").join("netstandard.dll");
    let bytes: Vec<Vec<u8>> = [PathBuf::from(core), sysrt, netstd]
        .iter()
        .map(|p| std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display())))
        .collect();
    let views: Vec<_> = bytes
        .iter()
        .map(|b| Ecma335Assembly::parse(b).expect("parse a reference"))
        .collect();
    AssemblyEnv::from_views(&views).expect("build AssemblyEnv")
}

/// The program [`the_deepest_admissible_type_fits_under_the_tallest_expression`]
/// walks: a function `h` whose type is exactly [`MAX_TYPE_DEPTH`] deep, applied
/// by a local in the innermost operand of a left-nested chain of `ops`
/// additions, where the expression walk is deepest.
fn deep_case(ops: usize) -> String {
    let params: Vec<String> = (1..MAX_TYPE_DEPTH).map(|i| format!("p{i}")).collect();
    format!(
        "let a = 0\nlet (+) (x: int) (y: int) = x\nlet h {} = 0\n\
         let deep x = (let y = h 0 in 0){}\n",
        params.join(" "),
        " + a".repeat(ops)
    )
}

/// Child entry: infer one case under the memory allowance, on half the walk
/// stack, and report its [`Facts`].
#[test]
#[ignore = "child-process entry for infer_type_budget"]
fn probe_child() {
    let Some(spec) = stack_probe::requested_case(CASE_VAR) else {
        return;
    };
    let (case, refs) = spec.split_once('\t').expect("a case and its references");
    let (kind, n) = case.split_once(' ').expect("a kind and a count");
    let n: usize = n.parse().expect("a count");
    let src = match kind {
        "deep" => deep_case(n),
        family => Family::named(family).source(n),
    };
    // The env is loaded before the allowance is measured from: it is shared
    // by every file, not part of what a case costs.
    let env: &'static AssemblyEnv = Box::leak(Box::new(env_from(refs)));
    let base = alloc_limit::live_bytes();
    alloc_limit::limit_live_bytes(base + MEMORY_ALLOWANCE);
    alloc_limit::reset_peak();
    let mut facts = stack_probe::on_stack(PROBE_STACK_BYTES, move || infer_facts(&src, env));
    facts.peak_bytes = alloc_limit::peak_bytes().saturating_sub(base);
    stack_probe::report(&facts.spell());
}

/// Every family, at every length up to [`LINES`], finishes within its memory,
/// time and stack, and its inference publishes a growing type up to some
/// length and declines the file from there on.
#[test]
fn inference_declines_past_the_type_budget_within_bounds() {
    let cases: Vec<(Family, usize)> = Family::ALL
        .into_iter()
        .flat_map(|family| (1..=LINES).map(move |lines| (family, lines)))
        .collect();
    let outcomes = par_map(cases.len(), |i| {
        let (family, lines) = cases[i];
        stack_probe::run_case_limited(CHILD, CASE_VAR, &spell(family.name(), lines), CASE_TIMEOUT)
    });
    let mut failures = Vec::new();
    let mut report = Vec::new();
    for family in Family::ALL {
        let runs: Vec<(usize, Facts)> = cases
            .iter()
            .zip(&outcomes)
            .filter(|((f, _), _)| *f == family)
            .filter_map(|(&(_, lines), outcome)| match outcome {
                Limited::Survived(facts) => Some((lines, Facts::read(facts))),
                other => {
                    failures.push(format!("{} at {lines} lines: {other:?}", family.name()));
                    None
                }
            })
            .collect();
        if runs.len() < LINES {
            continue;
        }
        // The first length whose inference reaches the budget.
        let Some(first_over) = runs.iter().position(|(_, f)| f.exceeded.is_some()) else {
            failures.push(format!(
                "{} never reached the type budget in {LINES} lines",
                family.name()
            ));
            continue;
        };
        // A walk that passed the budget counted the node it stopped at, so
        // the file must say it declined.
        for (lines, facts) in &runs {
            let passed = facts.walked_nodes > MAX_TYPE_NODES || facts.walked_depth > MAX_TYPE_DEPTH;
            if passed && facts.exceeded.is_none() {
                failures.push(format!(
                    "{} at {lines} lines walked past the budget but did not decline: {facts:?}",
                    family.name()
                ));
            }
        }
        let (within, over) = runs.split_at(first_over);
        for (lines, facts) in over {
            if facts.exceeded.is_none() || facts.published_nodes != 0 {
                failures.push(format!(
                    "{} at {lines} lines, past the budget, published or came back \
                     within it: {facts:?}",
                    family.name()
                ));
            }
        }
        // Below the budget the family is live: the types inference builds
        // grow by the line, and so do the ones it publishes where the family
        // publishes one, so the sweep is not passing on a family inference
        // declines for some other reason.
        if within.len() < 3 {
            failures.push(format!(
                "{} reached the budget within {} lines: the family does not grow \
                 the way it is meant to",
                family.name(),
                within.len()
            ));
        }
        for pair in within.windows(2) {
            let ((_, a), (lines, b)) = (&pair[0], &pair[1]);
            let walked = |f: &Facts| (f.walked_nodes, f.walked_depth);
            let published = |f: &Facts| (f.published_nodes, f.published_depth);
            let grew = walked(b) > walked(a)
                && (family == Family::OpenLocal || published(b) > published(a));
            if !grew {
                failures.push(format!(
                    "{} at {lines} lines built or published no larger a type than \
                     the line before: {a:?} then {b:?}",
                    family.name()
                ));
            }
        }
        if let Some((_, last)) = within.last()
            && (last.published_nodes > MAX_TYPE_NODES || last.published_depth > MAX_TYPE_DEPTH)
        {
            failures.push(format!(
                "{} published a type past the budget: {last:?}",
                family.name()
            ));
        }
        let peak = runs.iter().map(|(_, f)| f.peak_bytes).max().unwrap_or(0);
        let largest = within
            .last()
            .map(|(_, f)| (f.published_nodes, f.published_depth));
        report.push(format!(
            "{:<13} declines from {} lines (largest published type {largest:?} \
             nodes/depth); peak {} KiB",
            family.name(),
            first_over + 1,
            peak >> 10
        ));
    }
    for line in &report {
        eprintln!("{line}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The deepest type the budget admits, walked on top of the tallest
/// expression inference walks, fits on half of [`WALK_STACK_BYTES`].
///
/// The walks over a type recurse once per level, and some run in the middle
/// of the expression walk (a local's type is settled where the local is), so
/// the stack they need is the expression walk's at its deepest plus theirs at
/// [`MAX_TYPE_DEPTH`]. The case is a function of `MAX_TYPE_DEPTH - 1`
/// parameters — a type exactly at the depth bound — applied by a local in the
/// innermost operand of as long an operator chain as the parser accepts
/// cleanly.
#[test]
fn the_deepest_admissible_type_fits_under_the_tallest_expression() {
    let clean = |ops: usize| parse(&deep_case(ops)).errors.is_empty();
    let (mut lo, mut hi) = (1, 2 * MAX_TREE_HEIGHT);
    assert!(
        clean(lo) && !clean(hi),
        "the parse stops somewhere below the bound"
    );
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if clean(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let outcome = stack_probe::run_case_limited(CHILD, CASE_VAR, &spell("deep", lo), CASE_TIMEOUT);
    let Limited::Survived(facts) = outcome else {
        panic!(
            "the deepest admissible type under {lo} additions did not fit in a {} MiB \
             stack, {} MiB of memory and {CASE_TIMEOUT:?}: {outcome:?}",
            PROBE_STACK_BYTES >> 20,
            MEMORY_ALLOWANCE >> 20
        );
    };
    let facts = Facts::read(&facts);
    assert_eq!(facts.exceeded, None, "{facts:?}");
    assert_eq!(
        facts.walked_depth, MAX_TYPE_DEPTH,
        "a walk reached exactly the depth bound: {facts:?}"
    );
    assert!(
        facts.expression_nesting >= MAX_TREE_HEIGHT - 128,
        "the expression walk reached near the tree bound under {lo} additions: {facts:?}"
    );
}

/// One parameter more than [`the_deepest_admissible_type_fits_under_the_tallest_expression`]'s
/// function is a type one level past the depth bound, which inference
/// declines without building it.
#[test]
fn a_function_one_level_past_the_depth_bound_declines() {
    let params = |n: usize| {
        (1..=n)
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let env = crate::common::full_bcl_env();
    let at = infer_facts(&format!("let h {} = 0\n", params(MAX_TYPE_DEPTH - 1)), env);
    assert_eq!(at.exceeded, None, "{at:?}");
    assert_eq!(at.published_depth, MAX_TYPE_DEPTH, "{at:?}");
    let past = infer_facts(&format!("let h {} = 0\n", params(MAX_TYPE_DEPTH)), env);
    assert_eq!(past.exceeded, Some(TypeBudgetExceeded::Depth), "{past:?}");
    assert_eq!(past.published_nodes, 0, "{past:?}");
}
