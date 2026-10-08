//! Named-argument labels against FCS: a label names the callee's *parameter*,
//! never the same-named value in scope at the call.
//!
//! `M(x = x)` and `M(?x = x)` put the label `x` in expression position, so a
//! resolver that walks method arguments as ordinary expressions binds it to
//! whatever `x` is in scope. FCS binds it to the parameter `x` of `M` (an
//! in-file member's parameter, or one in a referenced assembly), and binds the
//! value side to the local. Committing the label to the local is a wrong
//! answer, and the corpus gate (`resolve_corpus_diff`) found one in the wild:
//! `Async.RunSynchronously(w, ?cancellationToken = cancellationToken)`.
//!
//! The check is soundness first: every use we commit must be the binder FCS
//! names, and a use FCS resolves outside the file must not be committed to any
//! in-file binder. Deferring a label is always allowed. Two kinds of planted
//! occurrence keep it from holding vacuously:
//!
//! * a **label** (`«x»`): FCS must report a use there, or the program is not
//!   exercising a named argument at all;
//! * a **value** (`‹x›`): the argument's value side, and both sides of an
//!   equality passed to a let-bound function, which F# does not read as a
//!   named argument. These must still be committed, to the binder FCS names,
//!   so that "never resolve inside an argument list" cannot pass for a fix.
//!
//! Each program must also type-check cleanly: FCS's answer about a program it
//! rejects is recovery, not semantics.

use std::path::PathBuf;

use crate::common::{
    CensusDecl, ResolveDiffUse, census_resolve_uses, full_bcl_env, invoke_fcs_dump_census,
    invoke_fcs_dump_census_project, parse_census_jsonl, temp_fs_file,
};
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, ProjectItems, Resolution, SyntaxRecovery, resolve_file, resolve_project,
};
use rowan::TextRange;

/// The declarations every call shape is checked against.
const PRELUDE: &str = "\
module M
type T(a: int) =
    static member S(a: int, ?b: int) = a + defaultArg b 0
    member _.I(a: int) = a
let g (c: bool) = c
let h (c: bool) (d: bool) = c && d
let k<'t> (c: bool) = c
let run (a: int) (b: int option) (p: bool -> bool) =
    let lf (c: bool) = c
";

/// Method, constructor and equality calls written out. The callee's parameters
/// are named like the locals in scope, which is the collision that matters.
const CALLS: &[&str] = &[
    // An in-file static method: named, optional named, and both.
    "    T.S(«a» = ‹a›)\n",
    "    T.S(‹a›, «?b» = ‹b›)\n",
    "    T.S(«a» = ‹a›, «?b» = ‹b›)\n",
    // An in-file constructor and instance method.
    "    (T(«a» = ‹a›)).I(«a» = ‹a›)\n",
    "    (new T(«a» = ‹a›)).I(‹a›)\n",
    // Methods in referenced assemblies.
    "    let count = ‹a›\n    System.String('x', «count» = ‹count›).Length\n",
    "    let timeout = ‹b›\n    Async.RunSynchronously(async { return ‹a› }, «?timeout» = ‹timeout›)\n",
    "    let cancellationToken = None\n    Async.RunSynchronously(async { return ‹a› }, \
     «?cancellationToken» = ‹cancellationToken›)\n",
    // An equality under an operator is never an argument list's element.
    "    g (true && (‹a› = ‹a›))\n",
];

/// Every way of applying a **function** to `(a = a)` the matrix covers: each
/// function head — a module function, a block-local one, a function-typed
/// parameter, a generic one under type arguments, a curried application —
/// under each wrapper that leaves it a function, plus heads that are not names
/// at all. F# reads `a = a` there as an equality, so both operands must
/// resolve.
fn function_calls() -> Vec<String> {
    let names = [
        "g",
        "lf",
        "p",
        "k<int>",
        "h true",
        "(h true)",
        "(h : bool -> bool -> bool) true",
    ];
    let wrappers: [fn(&str) -> String; 4] = [
        |h| h.to_string(),
        |h| format!("({h})"),
        |h| format!("(({h}))"),
        |h| format!("({h} : bool -> bool)"),
    ];
    let mut heads: Vec<String> = names
        .iter()
        .flat_map(|n| wrappers.iter().map(move |w| w(n)))
        .collect();
    heads.extend(
        [
            "(fun (c: bool) -> c)",
            "(if true then g else lf)",
            "(match 0 with _ -> g)",
            "(let q = g in q)",
        ]
        .map(str::to_string),
    );
    heads
        .into_iter()
        .map(|head| format!("    {head} (‹a› = ‹a›)\n"))
        .collect()
}

/// Calls whose value sides can only be committed once FSharp.Core is
/// referenced: the head is a module function there (`not`, `List.contains`),
/// so `a = a` is an equality — but under an empty environment the head is
/// unknown, and could as well be a method.
const CALLS_NEEDING_CORE: &[&str] = &[
    "    not (‹a› = ‹a›)\n",
    "    List.contains (‹a› = ‹a›) [ true ]\n",
];

/// A program and the ranges planted in it, by kind.
struct Program {
    src: String,
    labels: Vec<TextRange>,
    values: Vec<TextRange>,
}

/// [`render_after`] the shared [`PRELUDE`].
fn render(call: &str) -> Program {
    render_after(PRELUDE, call)
}

/// `prefix` followed by `call` with its `«…»` (label) and `‹…›` (value)
/// markers stripped. A `?` sigil inside a label marker is kept in the source
/// but excluded from the range, which is the label *name* FCS reports.
fn render_after(prefix: &str, call: &str) -> Program {
    let mut src = String::from(prefix);
    let mut labels = Vec::new();
    let mut values = Vec::new();
    let mut chars = call.chars();
    let mut open: Option<(char, usize)> = None;
    while let Some(c) = chars.next() {
        match c {
            '«' | '‹' => {
                assert!(open.is_none(), "nested markers in {call:?}");
                if c == '«' && chars.as_str().starts_with('?') {
                    src.push(chars.next().unwrap());
                }
                open = Some((c, src.len()));
            }
            '»' | '›' => {
                let (kind, start) = open.take().expect("balanced markers");
                let range = TextRange::new(
                    u32::try_from(start).unwrap().into(),
                    u32::try_from(src.len()).unwrap().into(),
                );
                if kind == '«' {
                    labels.push(range);
                } else {
                    values.push(range);
                }
            }
            c => src.push(c),
        }
    }
    assert!(open.is_none(), "unclosed marker in {call:?}");
    Program {
        src,
        labels,
        values,
    }
}

fn range_of(start: usize, end: usize) -> TextRange {
    TextRange::new(
        u32::try_from(start).unwrap().into(),
        u32::try_from(end).unwrap().into(),
    )
}

#[test]
fn named_argument_labels_never_bind_a_local() {
    // `(program, needs FSharp.Core to commit its values)`.
    let programs: Vec<(Program, bool)> = CALLS
        .iter()
        .map(|c| (render(c), false))
        .chain(function_calls().iter().map(|c| (render(c), false)))
        .chain(CALLS_NEEDING_CORE.iter().map(|c| (render(c), true)))
        .collect();
    let paths: Vec<PathBuf> = programs
        .iter()
        .map(|(p, _)| temp_fs_file("named_args", &p.src))
        .collect();
    let census = parse_census_jsonl(&invoke_fcs_dump_census(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), programs.len(), "one census line per program");

    let empty = AssemblyEnv::default();
    let mut wrong = Vec::new();
    for ((program, needs_core), file) in programs.iter().zip(&census) {
        let src = &program.src;
        assert!(
            file.ok && !file.has_check_errors,
            "the program must type-check cleanly, or FCS's answer is recovery: {src}"
        );
        let uses: Vec<_> = census_resolve_uses(file, src)
            .into_iter()
            .filter(|u| !u.is_from_definition && u.start != u.end)
            .collect();
        for label in &program.labels {
            assert!(
                uses.iter().any(|u| range_of(u.start, u.end) == *label),
                "FCS reports no use at the label {:?}, so this program does not \
                 exercise a named argument: {src}",
                &src[*label]
            );
        }

        for (env, has_core) in [(&empty, false), (full_bcl_env(), true)] {
            check_program(program, &uses, env, has_core || !needs_core, &mut wrong);
        }
    }
    assert!(
        wrong.is_empty(),
        "{} uses committed to a binder FCS does not name:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

/// Resolve `program` under `env` and check it against FCS's `uses`: every
/// in-file commitment is FCS's binder, and — when `values_must_commit` — every
/// planted value is committed.
fn check_program(
    program: &Program,
    uses: &[ResolveDiffUse],
    env: &AssemblyEnv,
    values_must_commit: bool,
    wrong: &mut Vec<String>,
) {
    let src = &program.src;
    let parsed = parse(src);
    assert!(parsed.errors.is_empty(), "{src}: {:?}", parsed.errors);
    let recovery = SyntaxRecovery::of(&parsed);
    let impl_file = ImplFile::cast(parsed.root).expect("impl file");
    let rf = resolve_file(&impl_file, &ProjectItems::default(), env, &recovery);

    if values_must_commit {
        for value in &program.values {
            let fcs = uses
                .iter()
                .find(|u| range_of(u.start, u.end) == *value)
                .unwrap_or_else(|| panic!("FCS reports no use at {:?}: {src}", &src[*value]));
            let CensusDecl::InFile(s, e) = fcs.decl else {
                panic!(
                    "FCS binds the value {:?} outside the file: {src}",
                    &src[*value]
                );
            };
            let ours = rf
                .resolution_at(*value)
                .and_then(|r| rf.resolved_def(r))
                .map(|d| d.range);
            assert_eq!(
                ours,
                Some(range_of(s, e)),
                "the value {:?} at {value:?} must resolve to the binder FCS names: {src}",
                &src[*value]
            );
        }
    }
    for u in uses {
        let range = range_of(u.start, u.end);
        let Some(res @ (Resolution::Local(_) | Resolution::Item(_))) = rf.resolution_at(range)
        else {
            continue;
        };
        let ours = rf.resolved_def(res).map(|d| d.range);
        let agrees = match u.decl {
            CensusDecl::InFile(s, e) => ours == Some(range_of(s, e)),
            CensusDecl::OtherFile => false,
            // No declaration from FCS: no claim to grade.
            CensusDecl::Absent => true,
        };
        if !agrees {
            wrong.push(format!(
                "{:?} at {range:?}: FCS {:?}, we gave {ours:?} in\n{src}",
                &src[range], u.decl
            ));
        }
    }
}

/// A function from an **earlier file** in Compile order is a function head
/// too, qualified or opened: `a = a` stays an equality, and both operands
/// resolve. Its proof is the preceding file's export, not this file's arena.
#[test]
fn a_cross_file_function_head_keeps_both_equality_operands() {
    let first = "module First\nlet g (c: bool) = c\nlet h (c: bool) (d: bool) = c && d\n";
    let later = [
        render_after(
            "module Qualified\nlet run (a: int) =\n",
            "    First.g (‹a› = ‹a›) && First.h true (‹a› = ‹a›)\n",
        ),
        render_after(
            "module Opened\nopen First\nlet run (a: int) =\n",
            "    g (‹a› = ‹a›) && h true (‹a› = ‹a›)\n",
        ),
    ];
    let mut paths = vec![temp_fs_file("named_args_first", first)];
    paths.extend(
        later
            .iter()
            .map(|p| temp_fs_file("named_args_later", &p.src)),
    );
    let census = parse_census_jsonl(&invoke_fcs_dump_census_project(&paths));
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    assert_eq!(census.len(), paths.len(), "one census line per file");
    for file in &census {
        assert!(
            file.ok && !file.has_check_errors,
            "{} must type-check cleanly",
            file.path
        );
    }

    let sources: Vec<&str> = std::iter::once(first)
        .chain(later.iter().map(|p| p.src.as_str()))
        .collect();
    let asts: Vec<ImplFile> = sources
        .iter()
        .map(|src| {
            let parsed = parse(src);
            assert!(parsed.errors.is_empty(), "{src}: {:?}", parsed.errors);
            ImplFile::cast(parsed.root).expect("impl file")
        })
        .collect();
    let project = resolve_project(&asts, &AssemblyEnv::default());
    for (i, program) in later.iter().enumerate() {
        let rf = project.file(i + 1);
        let uses: Vec<_> = census_resolve_uses(&census[i + 1], &program.src)
            .into_iter()
            .filter(|u| !u.is_from_definition && u.start != u.end)
            .collect();
        for value in &program.values {
            let fcs = uses
                .iter()
                .find(|u| range_of(u.start, u.end) == *value)
                .expect("FCS reports every planted value");
            let CensusDecl::InFile(s, e) = fcs.decl else {
                panic!("FCS binds {value:?} outside the file");
            };
            let ours = rf
                .resolution_at(*value)
                .and_then(|r| rf.resolved_def(r))
                .map(|d| d.range);
            assert_eq!(
                ours,
                Some(range_of(s, e)),
                "the value at {value:?} must resolve to the binder FCS names:\n{}",
                program.src
            );
        }
    }
}
