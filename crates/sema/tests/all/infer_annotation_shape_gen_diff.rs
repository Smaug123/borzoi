//! Generative **type-annotation shape** differential vs the FCS `binder-types`
//! oracle — the instrument the generic `annotation_ty` bridge is built behind.
//!
//! The bridge was first written as "accept a generic application, then subtract
//! the cases that go wrong". Under a certain-implies-exact contract that is
//! backwards: three review rounds each subtracted one more guard and the next
//! found another shape (a source-renamed head, a structurally-rendered tycon, a
//! tuple buried in an argument's tree, an unsatisfiable constraint, an IL-only
//! name). A subtractive allow-list has no closure argument, so "no more findings"
//! only ever means "no reviewer thought of the next one". This harness
//! *enumerates* the annotation space instead, so the safe subset is read off a
//! measurement rather than argued.
//!
//! # The space
//!
//! One `let vN : <annotation> = failwith ""` per case, batched a chunk of cases
//! to a file. Two halves, and the split matters:
//!
//! - [`structural`] enumerates every array / tuple / function nesting to depth 3
//!   over two atoms. This is the surface we commit on **today** — an annotation
//!   built only from primitives and F#'s three syntactic type formers needs no
//!   generic bridge to reach inference — so it is enumerated rather than listed.
//! - [`applications`] applies arity-1 and arity-2 heads chosen for the ways they
//!   are known to diverge — an F#-renamed union (`Result` compiles to
//!   `FSharpResult`), a plain BCL generic (`KeyValuePair`), a constrained one
//!   (`System.Nullable`, `Map`), abbreviations (`option`, `list`, `ResizeArray`),
//!   a structurally-rendered tycon (`System.Tuple`), and an IL-only name
//!   (`Microsoft.FSharp.Quotations.FSharpExpr`, whose source spelling is `Expr`)
//!   — over structural arguments and a typar, then nests them one level further.
//!   This is the surface the bridge would add.
//!
//! # The oracle is diagnostic-aware — this is the design point
//!
//! Two of the shapes above are **invalid F#** that FCS *error-recovers* from:
//! `System.Nullable<string>` violates the value-type constraint and
//! `Microsoft.FSharp.Quotations.FSharpExpr<int>` names a type F# source cannot
//! write. In both cases FCS reports the binder as `System.Object` while emitting
//! FS0001 / FS0039. A type-only comparison reads that as an ordinary divergence —
//! or, if we happened to also say `System.Object`, as **agreement**, and the bug
//! is invisible. `resolve_qualified_path_access_gen_diff` records the identical
//! trap on its own axis.
//!
//! So the rule is three arms, not a comparison:
//!
//! 1. FCS types the binder cleanly **and we commit** → the rendered strings must
//!    match exactly;
//! 2. FCS types it cleanly and we **defer** → allowed (an availability loss);
//! 3. FCS emits an **error on the annotation's line** → we must commit *nothing*;
//!    an annotation FCS rejects has exactly one correct answer, and any type
//!    there is a wrong hover.
//!
//! Line granularity is exact here because the harness lays out one annotation per
//! line, and it is what the oracle payload already carries (`Errors`, the shape
//! `types` / `attrs` / `overloads` share). A misattributed error cannot hide a
//! defect: it can only *demand* a deferral we would otherwise be free to make.
//!
//! # What the report says a bridge must defer
//!
//! `renderTypeCanonical` holds one currency all the way down — F# surface syntax
//! (`a * b`, `a -> b`, `'a`) with fully-qualified named heads — through tuples,
//! functions, arrays and generic applications alike. So the report's FCS column
//! is directly comparable with [`borzoi_sema::Ty::render`] at every node, and a
//! divergence is a fact about the *type*, not about where in the tree it sat.
//!
//! What the measurement then shows is a small closed set of heads whose canonical
//! rendering is **not** an application at all, because FCS's own `IsTupleType` /
//! `IsFunctionType` / `IsArrayType` hold for them:
//!
//! | written                                       | FCS renders                        |
//! |-----------------------------------------------|------------------------------------|
//! | `System.Tuple<int, string>`                   | `System.Int32 * System.String`     |
//! | `Microsoft.FSharp.Core.FSharpFunc<int, bool>` | `System.Int32 -> System.Boolean`   |
//! | `array<int>`                                  | `System.Int32[]`                   |
//! | `System.ValueTuple<int, string>`              | `struct (System.Int32 * System.String)` |
//!
//! Those are exactly the tycons naming a shape a bridge cannot spell as an
//! application, so committing one commits a wrong string for the right type. The
//! set is closed because what it enumerates is closed: the first three are
//! [`borzoi_sema::Ty`]'s structural variants, one head each, and the fourth is
//! the struct tuple, which `Ty` cannot represent at all — which is why
//! `annotation_ty` already declines a written `struct (a * b)`, and the head form
//! has to decline for the same reason rather than a rendering-specific one.
//!
//! # Two properties the three arms structurally cannot check
//!
//! Both concern the *reference*, and neither can be seen by comparing against a
//! subject that defers — which is every generic shape today. So a green sweep is
//! not evidence for either, and they are asserted separately.
//!
//! [`an_alias_and_its_expansion_render_identically`] is the load-bearing one. A
//! canonical string can be in the right currency and still name the wrong type,
//! by losing a grouping: a renderer that recurses into an expanded shape while
//! asking the *unexpanded* one whether to parenthesise emits
//! `System.Int32 * System.String[]` for `PairAlias[]` — a 2-tuple of `int` and
//! `string[]`, not an array of pairs. An alias is what makes that reachable at
//! all, since it is the one way to put a tuple or a function somewhere the writer
//! could not have parenthesised it. Writing the same type both ways and demanding
//! one string catches it without any knowledge of which shapes are at risk.
//!
//! The second is the absence of the **display-currency fallback**. `go`'s
//! fallthrough (`try renderType with _ -> renderExprType`) silently switches to
//! FCS's display form for the whole type from the root, and the shapes that
//! trigger it — a struct tuple, a byref — are invisible from the head. Every arm
//! now recurses instead, and no row in the enumerated space reaches the fallback.
//!
//! # What it is green on today
//!
//! The structural half genuinely compares — over a hundred cases where we commit
//! and FCS confirms — and it is what caught a live wrong hover: our array
//! renderer dropped the parentheses around a structural element, so
//! `(int * string)[]` rendered `int * string[]`, a string denoting a different
//! type. The application half is all deferral, since we decline every generic
//! annotation.
//!
//! That asymmetry is why the vacuity floors are load-bearing: the sweep asserts
//! FCS genuinely rejected some annotations (arm 3 is exercised at all), that we
//! genuinely committed and matched on others (arm 1 compares something), and that
//! we still defer somewhere (the shapes it exists to size are still in it). A
//! sweep that has quietly stopped measuring fails instead of passing.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::common::{
    ensure_constrained_fixture_built, invoke_fcs_dump_with_refs,
    parse_fcs_binder_types_with_errors, temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{InferredFile, ProjectItems, ResolvedFile, infer_file, resolve_file};

/// Resolve and infer a generated chunk over the shared real FSharp.Core + BCL
/// closure — the same env the `binder-types` differential uses, and the honest
/// counterpart to the FCS side, which always compiles against the real
/// FSharp.Core.
fn resolve_and_infer(source: &str) -> (ResolvedFile, InferredFile) {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "generated chunk has parse errors — a shape outside our parseable subset \
         silences its whole chunk: {:?}\n{source}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    let env = crate::common::constrained_fixture_env();
    let resolved = resolve_file(&file, &ProjectItems::default(), env);
    let inferred = infer_file(&file, &resolved, env);
    (resolved, inferred)
}

/// The 1-based line `offset` falls on. The generated chunks are small and ASCII,
/// so a scan is cheaper than an index.
fn line_of(source: &str, offset: usize) -> u32 {
    let n = source[..offset].bytes().filter(|b| *b == b'\n').count() + 1;
    u32::try_from(n).expect("line fits u32")
}

/// A generated type annotation. Rendering is the case's identity, so a shape is
/// named by the F# it produces.
#[derive(Clone, Debug)]
enum Ann {
    /// A type with no arguments: a primitive, an F# alias, or a typar.
    Atom(&'static str),
    /// `T[]`.
    Array(Box<Ann>),
    /// `a * b`.
    Tuple(Box<Ann>, Box<Ann>),
    /// `a -> b`.
    Fun(Box<Ann>, Box<Ann>),
    /// `Head<a, …>` — always the prefix form, so one renderer covers both the
    /// BCL heads and the F# ones that also admit postfix (`int option`).
    App(&'static str, Vec<Ann>),
}

impl Ann {
    fn atom(s: &'static str) -> Ann {
        Ann::Atom(s)
    }

    fn array(t: Ann) -> Ann {
        Ann::Array(Box::new(t))
    }

    fn tuple(a: Ann, b: Ann) -> Ann {
        Ann::Tuple(Box::new(a), Box::new(b))
    }

    fn fun(a: Ann, b: Ann) -> Ann {
        Ann::Fun(Box::new(a), Box::new(b))
    }

    /// The annotation as F# source.
    fn render(&self) -> String {
        match self {
            Ann::Atom(s) => (*s).to_owned(),
            Ann::Array(t) => format!("{}[]", t.render_nested()),
            Ann::Tuple(a, b) => format!("{} * {}", a.render_nested(), b.render_nested()),
            Ann::Fun(a, b) => format!("{} -> {}", a.render_nested(), b.render_nested()),
            Ann::App(head, args) => {
                let args: Vec<String> = args.iter().map(Ann::render_nested).collect();
                format!("{head}<{}>", args.join(", "))
            }
        }
    }

    /// The annotation as F# source in a position where `*` / `->` would bind
    /// wrongly — an array element, a tuple/function operand, a generic argument.
    /// Parenthesised for exactly those two forms, so the generated source says
    /// what the tree says rather than what F#'s precedence would re-associate it
    /// into.
    fn render_nested(&self) -> String {
        match self {
            Ann::Tuple(..) | Ann::Fun(..) => format!("({})", self.render()),
            _ => self.render(),
        }
    }

    /// Whether a typar appears anywhere in the tree. Recorded on the report row
    /// rather than filtered out: a typar changes the oracle's *currency* (see the
    /// module docs), which is a fact the bridge's guard needs, not noise.
    fn has_typar(&self) -> bool {
        match self {
            Ann::Atom(s) => s.starts_with('\''),
            Ann::Array(t) => t.has_typar(),
            Ann::Tuple(a, b) | Ann::Fun(a, b) => a.has_typar() || b.has_typar(),
            Ann::App(_, args) => args.iter().any(Ann::has_typar),
        }
    }
}

/// Arity-1 heads. Each is here for a distinct reason the projection could get
/// wrong; see the module docs.
const HEADS1: [&str; 12] = [
    "option",
    "list",
    "ResizeArray",
    "System.Nullable",
    "System.Collections.Generic.List",
    "Microsoft.FSharp.Quotations.Expr",
    "Microsoft.FSharp.Quotations.FSharpExpr",
    // The three arity-1 heads a *written* application can carry that name a
    // shape [`Ty`] spells with a variant of its own rather than as an
    // application — see [`HEADS2`] for why they are in the alphabet at all.
    "array",
    "seq",
    "byref",
    // The **constraint** dimension, from `tests/fixtures/constrained_env`. F#
    // constraints have no IL encoding, and FCS recovers a violating annotation's
    // binder to `System.Object` — so a bridge blind to them commits a type FCS
    // positively disagrees with. Every constrained head in the shipped
    // FSharp.Core is *also* excluded by an unrelated guard (`Map` and `Set` are
    // source-renamed, `System.Nullable`'s constraint is IL-visible), which is
    // why this dimension needs a fixture rather than a library head: without one
    // the sweep is green on the defect. `Free` is the twin that makes a decline
    // on `Constrained` attributable to the constraint rather than to genericity.
    "ConstrainedFixture.Constrained",
    "ConstrainedFixture.Free",
];

/// Arity-2 heads.
///
/// `System.Tuple`, `System.ValueTuple` and `Microsoft.FSharp.Core.FSharpFunc`
/// earn their places by being the heads whose canonical rendering is **not** an
/// application: FCS's own `IsTupleType` / `IsFunctionType` hold for them, so it
/// renders `System.Tuple<int, string>` as `System.Int32 * System.String`. A
/// bridge that treats every application alike commits `System.Tuple<…>` for
/// them, which is a wrong string for the right type. They sit in the alphabet so
/// that stays a *measured* defer rather than a guard someone reasoned out.
const HEADS2: [&str; 10] = [
    "Result",
    "Map",
    "System.Tuple",
    "System.ValueTuple",
    "Microsoft.FSharp.Core.FSharpFunc",
    "System.Func",
    "System.Collections.Generic.KeyValuePair",
    "System.Collections.Generic.Dictionary",
    // The arity-2 half of the constraint dimension: `ConstrainedKey` constrains
    // only its *first* parameter, so a verdict computed per entity rather than
    // per position is visible; `BothConstrained` constrains both.
    "ConstrainedFixture.ConstrainedKey",
    "ConstrainedFixture.BothConstrained",
];

/// The atoms the structural enumeration is built from. Two suffice: the shapes
/// that diverge are structural, and a third atom multiplies the space without
/// reaching a new one.
const ATOMS: [&str; 2] = ["int", "string"];

/// Every **structural** annotation — array, tuple, function — to depth 3 over
/// [`ATOMS`], each nesting level combined against the atoms.
///
/// Enumerated rather than listed, because this is exactly the surface we commit
/// on *today*: an annotation built only from primitives and F#'s three syntactic
/// type formers needs no generic bridge to reach inference. Listing it by hand is
/// how the array-of-function corner stayed hidden while the array-of-tuple one
/// beside it was written down.
fn structural() -> Vec<Ann> {
    let mut level: Vec<Ann> = ATOMS.iter().map(|a| Ann::atom(a)).collect();
    let mut all = level.clone();
    for _ in 1..3 {
        let mut next = Vec::new();
        for t in &level {
            next.push(Ann::array(t.clone()));
            for a in ATOMS {
                next.push(Ann::tuple(t.clone(), Ann::atom(a)));
                next.push(Ann::tuple(Ann::atom(a), t.clone()));
                next.push(Ann::fun(t.clone(), Ann::atom(a)));
                next.push(Ann::fun(Ann::atom(a), t.clone()));
            }
        }
        all.extend(next.iter().cloned());
        level = next;
    }
    all
}

/// The argument alphabet for the generic-head sweep: a sample of the structural
/// space (strided so it spans the nesting depths rather than exhausting the
/// shallowest), the atoms in both an F# and a BCL spelling, and a **typar** —
/// which changes the oracle's currency rather than merely its content, so it has
/// to be measured, not assumed.
fn generic_args() -> Vec<Ann> {
    let structural = structural();
    let mut out = vec![
        Ann::atom("int"),
        Ann::atom("System.Int32"),
        Ann::atom("bool"),
        Ann::atom("'a"),
        Ann::atom(NESTED_ENUM),
    ];
    out.extend(structural.iter().step_by(11).cloned());
    out
}

/// A type nested inside a **non-generic** one. The oracle's metadata renderer
/// normalises FCS's `+` separator to `/`, but this one arrives already dotted, so
/// the two conventions are not distinguishable from the head spelling alone.
const NESTED_ENUM: &str = "System.Environment.SpecialFolder";

/// A type nested inside a **generic** one, which is a different shape again: the
/// oracle renders it `Dictionary`2.KeyCollection<System.Int32, System.String>` —
/// the enclosing type's arity suffix survives (only the *outermost* segment's is
/// stripped) and the arguments hoist to the leaf. Nothing on our side spells that
/// today; the sweep is where that becomes a measured fact rather than a surprise
/// in a later review round.
const NESTED_IN_GENERIC: &str = "System.Collections.Generic.Dictionary<int, string>.KeyCollection";

/// Every arity-1 head over every [`generic_args`] argument, and every arity-2
/// head over two argument pairings — a rotation (so each argument appears in both
/// positions across the sweep) and a `(arg, int)` row (so the *first* position
/// sees every shape against a fixed second).
fn applications() -> Vec<Ann> {
    let args = generic_args();
    let mut out = Vec::new();
    for head in HEADS1 {
        for a in &args {
            out.push(Ann::App(head, vec![a.clone()]));
        }
    }
    for head in HEADS2 {
        for (i, a) in args.iter().enumerate() {
            let rotated = &args[(i + 1) % args.len()];
            out.push(Ann::App(head, vec![a.clone(), rotated.clone()]));
            out.push(Ann::App(head, vec![a.clone(), Ann::atom("int")]));
        }
    }
    out
}

/// An arity-1 head wrapped around a sample of [`applications`], so an argument
/// that is *itself* an application — the tree a head-only `matches!` guard lets
/// through — is covered at every arity-1 head.
fn nested_applications(applications: &[Ann]) -> Vec<Ann> {
    // Stride rather than take a prefix, so the sample spans every head instead of
    // exhausting the first one.
    let sample: Vec<&Ann> = applications.iter().step_by(23).collect();
    let mut out = Vec::new();
    for head in HEADS1 {
        for inner in &sample {
            out.push(Ann::App(head, vec![(*inner).clone()]));
        }
    }
    out
}

/// The whole enumerated space, de-duplicated (distinct trees can render the same
/// source — a rotation that lands on `(int, int)`, or a structural level that
/// rebuilds a shallower shape).
fn enumerate() -> Vec<Ann> {
    let structural = structural();
    let applications = applications();
    let nested = nested_applications(&applications);
    // A type nested in a generic one is not an `App`, so it rides in directly —
    // bare, under an array, and as a generic argument.
    let nested_in_generic = vec![
        Ann::atom(NESTED_IN_GENERIC),
        Ann::array(Ann::atom(NESTED_IN_GENERIC)),
        Ann::App("option", vec![Ann::atom(NESTED_IN_GENERIC)]),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for a in structural
        .into_iter()
        .chain(applications)
        .chain(nested)
        .chain(nested_in_generic)
    {
        if seen.insert(a.render()) {
            out.push(a);
        }
    }
    out
}

/// What FCS said about one annotation.
#[derive(Debug, Clone)]
enum Verdict {
    /// FCS checked the line cleanly and reports this canonical rendering.
    Clean(String),
    /// FCS reported an error on the annotation's line: `FS####: message`.
    Rejected(String),
    /// FCS emitted no binder record for the line and no error explaining why —
    /// the harness cannot classify it, so it is a failure rather than a skip.
    Missing,
}

/// One case's outcome, for the report and the assertions.
struct Row {
    source: String,
    has_typar: bool,
    verdict: Verdict,
    ours: Option<String>,
}

/// How the case counts split. Every floor below is a claim that some arm is
/// genuinely exercised — without them the sweep passes by measuring nothing.
#[derive(Default)]
struct Tally {
    /// Arm 1 satisfied by an actual comparison: FCS clean, we committed, equal.
    agreed: usize,
    /// Arm 2: FCS clean, we declined.
    deferred: usize,
    /// Arm 3 exercised: FCS rejected the annotation.
    rejected: usize,
    /// Arm 3 exercised *and* we correctly stayed silent.
    rejected_silent: usize,
}

/// A case whose outcome violates one of the three arms.
struct Violation {
    source: String,
    arm: &'static str,
    fcs: String,
    ours: String,
}

/// Check one chunk of annotations: emit them as a single file, run the oracle
/// over it, infer our side, and classify each case.
fn run_chunk(chunk_index: usize, anns: &[Ann]) -> Vec<Row> {
    let mut src = format!("module AnnSweep{chunk_index}\n");
    // 1-based line of each case's binder, matching the oracle's error lines.
    let mut lines = Vec::with_capacity(anns.len());
    for (i, ann) in anns.iter().enumerate() {
        lines.push(src.lines().count() + 1);
        let _ = writeln!(src, "let v{i} : {} = failwith \"\"", ann.render());
    }

    let (resolved, inferred) = resolve_and_infer(&src);
    let ours: HashMap<String, String> = inferred
        .def_types()
        .iter()
        .map(|(id, ty)| (resolved.def(*id).name.clone(), ty.render()))
        .collect();

    let path = temp_fs_file(&format!("ann_sweep{chunk_index}"), &src);
    // The oracle reads exactly our reference set: a head only one side can see
    // would decline for want of the assembly rather than for anything a guard
    // decided, and assert nothing.
    let json =
        invoke_fcs_dump_with_refs("binder-types", &path, &[ensure_constrained_fixture_built()]);
    let _ = std::fs::remove_file(&path);
    let (fcs, errors) = parse_fcs_binder_types_with_errors(&json, &src);

    // FCS's binder records keyed by the line they sit on: the harness knows which
    // line each case owns, and a binder's declaration range never spans lines.
    let by_line: HashMap<u32, String> = fcs
        .into_iter()
        .map(|((start, _), ty)| (line_of(&src, start), ty))
        .collect();

    let mut rows = Vec::with_capacity(anns.len());
    for (i, ann) in anns.iter().enumerate() {
        let line = u32::try_from(lines[i]).expect("line fits u32");
        let error = errors.iter().find(|e| e.line == line);
        let verdict = match (error, by_line.get(&line)) {
            (Some(e), _) => Verdict::Rejected(format!("FS{:04}: {}", e.code, e.message)),
            (None, Some(ty)) => Verdict::Clean(ty.clone()),
            (None, None) => Verdict::Missing,
        };
        rows.push(Row {
            source: ann.render(),
            has_typar: ann.has_typar(),
            verdict,
            ours: ours.get(&format!("v{i}")).cloned(),
        });
    }
    rows
}

#[test]
fn annotation_shapes_agree_with_fcs() {
    let anns = enumerate();
    // A chunk is one oracle request and one of our inference runs; 25 keeps a
    // failure's file small enough to read while paying .NET's per-request cost
    // only ~10 times.
    let rows: Vec<Row> = anns
        .chunks(25)
        .enumerate()
        .flat_map(|(i, chunk)| run_chunk(i, chunk))
        .collect();

    let mut tally = Tally::default();
    let mut violations: Vec<Violation> = Vec::new();
    let mut report = String::new();
    for row in &rows {
        let ours = row.ours.clone();
        match (&row.verdict, &ours) {
            (Verdict::Clean(fcs), Some(ours)) => {
                if fcs == ours {
                    tally.agreed += 1;
                } else {
                    violations.push(Violation {
                        source: row.source.clone(),
                        arm: "clean/commit",
                        fcs: fcs.clone(),
                        ours: ours.clone(),
                    });
                }
            }
            (Verdict::Clean(_), None) => tally.deferred += 1,
            (Verdict::Rejected(diag), ours) => {
                tally.rejected += 1;
                match ours {
                    None => tally.rejected_silent += 1,
                    Some(ours) => violations.push(Violation {
                        source: row.source.clone(),
                        arm: "rejected/commit",
                        fcs: diag.clone(),
                        ours: ours.clone(),
                    }),
                }
            }
            (Verdict::Missing, ours) => violations.push(Violation {
                source: row.source.clone(),
                arm: "unclassifiable",
                fcs: "no binder record and no error on the line".to_owned(),
                ours: ours.clone().unwrap_or_else(|| "-".to_owned()),
            }),
        }
        let _ = writeln!(
            report,
            "{}\t{}\t{}\t{}",
            row.source,
            if row.has_typar { "typar" } else { "ground" },
            match &row.verdict {
                Verdict::Clean(t) => format!("clean\t{t}"),
                Verdict::Rejected(d) => format!("rejected\t{d}"),
                Verdict::Missing => "missing\t-".to_owned(),
            },
            row.ours.as_deref().unwrap_or("(deferred)"),
        );
    }

    // The measurement, for reading the bridge's guard off rather than arguing it.
    // Captured unless the run asks for output.
    println!("{report}");
    println!(
        "annotation sweep: {} cases | agreed {} | deferred {} | rejected {} (silent {})",
        rows.len(),
        tally.agreed,
        tally.deferred,
        tally.rejected,
        tally.rejected_silent
    );

    assert!(
        tally.agreed > 0,
        "vacuous: we committed a type for no cleanly-typed annotation, so arm 1 compared nothing"
    );
    assert!(
        tally.rejected > 0,
        "vacuous: FCS rejected no annotation, so arm 3 — the only arm that can see an \
         error-recovered `System.Object` — was never exercised"
    );
    assert!(
        tally.deferred > 0,
        "vacuous: we committed on every cleanly-typed annotation, so the sweep no longer \
         covers the deferring shapes it exists to size"
    );
    assert!(
        violations.is_empty(),
        "{} annotation-shape violation(s) vs FCS:\n{}",
        violations.len(),
        violations
            .iter()
            .map(|v| format!(
                "  [{}] `{}`: FCS {} | we gave {}",
                v.arm, v.source, v.fcs, v.ours
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Source-declared aliases whose targets are *precedence-sensitive* shapes, with
/// the parenthesised spelling that denotes the same type.
///
/// An alias is the only way to put a tuple or a function somewhere the writer
/// cannot parenthesise it, which is exactly where a renderer that inspects the
/// **unexpanded** type loses the grouping.
/// The struct-tuple target is parenthesised in the *declaration* too: a bare
/// `type T = struct (…)` is read as a struct class definition, not as a struct
/// tuple, and the resulting syntax error cascades through the whole file.
const ALIASES: [(&str, &str, &str); 3] = [
    ("PairAlias", "int * string", "(int * string)"),
    ("FunAlias", "int -> string", "(int -> string)"),
    (
        "StructPairAlias",
        "(struct (int * string))",
        "(struct (int * string))",
    ),
];

/// How a context groups the child it wraps — the *rule*, stated once, so that
/// checking it is not a matter of remembering it at each position.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grouping {
    /// The operator binds tighter than both ` * ` and ` -> `, so a child
    /// rendering with either at its top level must be parenthesised or the
    /// string names a different type: `[]`, `&`, and a tuple element's join.
    Tight,
    /// A function's *domain*. `->` is right-associative, so only a function
    /// child regroups; a tuple domain (`a * b -> c`) needs no parens.
    FunctionDomain,
    /// Nothing regroups here — a function's range, or a comma-separated generic
    /// argument list.
    None,
}

/// The contexts an alias is placed in, each with the canonical shape the
/// oracle must produce. `{}` takes the alias spelling in the first, and the
/// child's own canonical rendering — parenthesised per [`Grouping`] — in the
/// second.
///
/// The pair is what makes the check **absolute** rather than relative: comparing
/// two spellings of one type only catches a rendering that depends on how the
/// type was *written*, and a position that drops its parens does so for every
/// spelling alike. Byref is exactly that case, and it is why this table exists.
const ALIAS_CONTEXTS: [(&str, &str, Grouping); 9] = [
    ("{}", "{}", Grouping::None),
    ("{}[]", "{}[]", Grouping::Tight),
    ("{}[][]", "{}[][]", Grouping::Tight),
    ("{} * int", "{} * System.Int32", Grouping::Tight),
    ("int * {}", "System.Int32 * {}", Grouping::Tight),
    ("{} -> int", "{} -> System.Int32", Grouping::FunctionDomain),
    ("int -> {}", "System.Int32 -> {}", Grouping::None),
    (
        "option<{}>",
        "Microsoft.FSharp.Core.FSharpOption<{}>",
        Grouping::None,
    ),
    // Only legal in a parameter position, so it is emitted differently — and it
    // is the position the alias-invariance half cannot see.
    ("byref<{}>", "{}&", Grouping::Tight),
];

/// Whether `rendered` has a ` * ` or a ` -> ` at **paren depth zero** — the
/// property that decides whether wrapping it changes what it denotes. Angle
/// brackets count as depth too: the `*` inside `H<a * b>` is already enclosed.
///
/// The `>` of an arrow is not a closing bracket. Counting it as one drives the
/// depth negative inside `H<a -> b>`, which then reads a later top-level ` * `
/// as nested and silently expects the wrong grouping.
fn splits_at_top_level(rendered: &str, needle: &str) -> bool {
    let bytes = rendered.as_bytes();
    let mut depth = 0i32;
    for (i, b) in bytes.iter().enumerate() {
        let arrow_tail = *b == b'>' && i > 0 && bytes[i - 1] == b'-';
        match b {
            b'(' | b'<' => depth += 1,
            b')' | b'>' if !arrow_tail => depth -= 1,
            _ if depth == 0 && rendered[i..].starts_with(needle) => return true,
            _ => {}
        }
    }
    false
}

#[test]
fn top_level_split_ignores_the_arrow_and_nesting() {
    // The arrow's `>` is not a bracket …
    assert!(splits_at_top_level(
        "H<System.Int32 -> System.String> * System.Boolean",
        " * "
    ));
    // … and a genuinely nested operator is still nested.
    assert!(!splits_at_top_level(
        "H<System.Int32 * System.String>",
        " * "
    ));
    assert!(!splits_at_top_level(
        "(System.Int32 -> System.String)",
        " -> "
    ));
    assert!(splits_at_top_level("System.Int32 -> System.String", " -> "));
}

/// The child rendering as it must appear inside `grouping`'s context.
fn grouped(rendered: &str, grouping: Grouping) -> String {
    let needs = match grouping {
        Grouping::Tight => {
            splits_at_top_level(rendered, " * ") || splits_at_top_level(rendered, " -> ")
        }
        Grouping::FunctionDomain => splits_at_top_level(rendered, " -> "),
        Grouping::None => false,
    };
    if needs {
        format!("({rendered})")
    } else {
        rendered.to_owned()
    }
}

/// **Alias invariance**: an annotation written through an alias and the same
/// annotation written out denote *one type*, so the oracle must render them to
/// one string.
///
/// This is a property of the reference alone — our side defers a source-declared
/// alias head, so no comparison against `Ty` could see it. It catches the defect
/// class that the three-arm sweep structurally cannot: a canonical rendering that
/// is wrong about the *type* rather than merely in a different currency. A
/// renderer that recurses into an expanded shape while asking the unexpanded one
/// whether to parenthesise emits `System.Int32 * System.String[]` for
/// `PairAlias[]` — a well-formed canonical string denoting an entirely different
/// type, which every string comparison against a deferring subject passes.
///
/// It is also spelling-agnostic by construction, so it keeps holding for shapes
/// nobody enumerated: any future arm whose output depends on how the user wrote
/// the type rather than on what the type is fails here.
///
/// # Why that is not enough on its own
///
/// Invariance is a *relative* property: it compares two spellings against each
/// other. A position that drops its parens for **every** spelling passes it. The
/// byref arm did exactly that — `byref<PairAlias>` and `byref<(int * string)>`
/// both rendered `System.Int32 * System.String&`, agreeing with each other and
/// both naming a tuple whose second element is a byref.
///
/// So each context also carries the canonical shape it must produce, built from
/// the child's *own* rendering by the grouping rule ([`Grouping`], [`grouped`]).
/// That is a reference implementation of one narrow thing — where parens go —
/// checked against the oracle at every context, which is what makes "did I apply
/// the rule at this position?" a machine question instead of a review question.
#[test]
fn an_alias_and_its_expansion_render_identically() {
    let mut src = "module AliasInvariance\n".to_owned();
    for (name, target, _) in ALIASES {
        let _ = writeln!(src, "type {name} = {target}");
    }
    // A context legal only in parameter position is emitted as a function
    // binding; `binder-types` reports the parameter as its own binder, so the
    // annotation is still one binder on one line. The enclosing function is
    // named `fnOf…` so it is the *wider* binder of the two and the narrowest-
    // range rule below picks the parameter, whose type is the annotation itself
    // rather than the function's `T -> unit`.
    let emit = |src: &mut String, i: usize, tag: char, ann: &str| -> u32 {
        let line = u32::try_from(src.lines().count() + 1).expect("line fits u32");
        if ann.contains("byref<") {
            let _ = writeln!(src, "let fnOf{tag}{i} ({tag}{i} : {ann}) = ()");
        } else {
            let _ = writeln!(src, "let {tag}{i} : {ann} = failwith \"\"");
        }
        line
    };

    struct Case {
        aliased: String,
        expanded: String,
        alias_line: u32,
        expanded_line: u32,
        bare_line: u32,
        grouping: Grouping,
        canon_template: &'static str,
    }
    // The bare rendering of each alias target is itself read off the oracle, so
    // the expected string is never hand-written.
    let mut bare_line_of: HashMap<&str, u32> = HashMap::new();
    let mut cases: Vec<Case> = Vec::new();
    for (name, _, parenthesised) in ALIASES {
        let bare = emit(&mut src, cases.len(), 'z', name);
        bare_line_of.insert(name, bare);
        for (ctx, canon_template, grouping) in ALIAS_CONTEXTS {
            let aliased = ctx.replace("{}", name);
            let expanded = ctx.replace("{}", parenthesised);
            let i = cases.len();
            let alias_line = emit(&mut src, i, 'a', &aliased);
            let expanded_line = emit(&mut src, i, 'b', &expanded);
            cases.push(Case {
                aliased,
                expanded,
                alias_line,
                expanded_line,
                bare_line: bare,
                grouping,
                canon_template,
            });
        }
    }

    let path = temp_fs_file("alias_invariance", &src);
    // No fixture head appears in this property's source, but the oracle is
    // handed the same reference set as the sweep so the two cannot drift.
    let json =
        invoke_fcs_dump_with_refs("binder-types", &path, &[ensure_constrained_fixture_built()]);
    let _ = std::fs::remove_file(&path);
    let (fcs, errors) = parse_fcs_binder_types_with_errors(&json, &src);
    // A `let f (p : T) = ()` line carries two binders, `f` and `p`; the parameter
    // is the narrower range, so prefer it.
    let mut by_line: HashMap<u32, (usize, String)> = HashMap::new();
    for ((start, end), ty) in fcs {
        let line = line_of(&src, start);
        let width = end.saturating_sub(start);
        match by_line.get(&line) {
            Some((w, _)) if *w <= width => {}
            _ => {
                by_line.insert(line, (width, ty));
            }
        }
    }
    let rendered = |line: u32| -> Option<&String> { by_line.get(&line).map(|(_, t)| t) };
    let clean = |line: u32| !errors.iter().any(|e| e.line == line);

    let mut invariance_compared = 0usize;
    let mut shape_compared = 0usize;
    let mut mismatches = Vec::new();
    for case in &cases {
        if !clean(case.alias_line) || !clean(case.expanded_line) {
            // A line FCS rejected makes no claim.
            continue;
        }
        let (Some(via_alias), Some(written_out)) =
            (rendered(case.alias_line), rendered(case.expanded_line))
        else {
            continue;
        };
        invariance_compared += 1;
        if via_alias != written_out {
            mismatches.push(format!(
                "  [spelling-dependent] `{}` rendered {via_alias}\n                       `{}` rendered {written_out}",
                case.aliased, case.expanded
            ));
        }
        // The absolute half: the child's own rendering, grouped by the rule, must
        // be what the context actually produced.
        if let (true, Some(bare)) = (clean(case.bare_line), rendered(case.bare_line)) {
            let expected = case
                .canon_template
                .replace("{}", &grouped(bare, case.grouping));
            shape_compared += 1;
            if *via_alias != expected {
                mismatches.push(format!(
                    "  [wrong grouping] `{}` rendered {via_alias}\n                   expected {expected} (child renders {bare})",
                    case.aliased
                ));
            }
        }
    }

    assert!(
        invariance_compared >= cases.len() / 2 && shape_compared >= cases.len() / 2,
        "vacuous: {invariance_compared} invariance and {shape_compared} shape comparisons over \
         {} cases — the property is measuring almost nothing",
        cases.len()
    );
    assert!(
        mismatches.is_empty(),
        "{} canonical-grouping violation(s) — a rendering that names a different type than the \
         one annotated:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
