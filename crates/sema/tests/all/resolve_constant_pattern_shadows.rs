//! FCS-free tests for **constant-pattern shadows of constructor cases**: a
//! `[<Literal>]` value *is* a constant pattern, so unlike a plain value it
//! contests the pattern (constructor) namespace — F#'s `ePatItems` holds
//! exactly the constructor cases (union / exception / active-pattern) and the
//! literal values, latest-wins.
//!
//! FCS pins (every probe `dotnet build`-clean; `uses-project` verdicts):
//!
//! - `open A; [<Literal>] let Even = 7; match n with Even` binds the **literal**
//!   (`B.Even`), not A's active-pattern case — and the same with a union case
//!   (`B.Red`, not `A.C.Red`). Committing the case here is a wrong target.
//! - The slot is **position-ordered**: the literal declared *before* `open A`
//!   loses to the opened case (the open re-takes the name).
//! - Within ONE opened module the literal wins **regardless of source order**:
//!   FCS folds a module's contents as exceptions → tycons → vals
//!   (`AddModuleOrNamespaceContentsToNameEnv`), so a val always post-dates its
//!   module's own cases in the environment.
//! - In a file's own sequential scope (no `open`), plain position order decides.
//! - A **non-literal** attribute (`[<System.Obsolete>]`) does not contest — but
//!   sema cannot verify attribute *identity* (a shadowing `LiteralAttribute`
//!   alias is undetectable), so any *attributed* module-level value is treated
//!   as maybe-literal and the contested case **defers** (sound, not exact).
//!   An **unattributed** `let` provably cannot be a literal, so the case still
//!   commits over it.
//! - A **qualified** case pattern (`A.Green`) resolves to the case even with a
//!   same-path literal in the module — the qualified path is untouched.
//! - An **applied** head is never a literal on a clean program (FS3191 "This
//!   literal pattern does not take arguments"), so the applied-head split keeps
//!   committing the case.
//!
//! Assembly side: a CLI `Literal`-flagged field folded in by an assembly `open`
//! is *definitely* a constant pattern (`value_may_be_constant_pattern`), so it
//! contests an earlier project case the same way.

use std::path::Path;

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    AssemblyEnv, DefKind, ProjectItems, Resolution, ResolvedFile, resolve_file, resolve_project,
};
use rowan::TextRange;

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

/// The byte range of the `n`th (0-based) occurrence of `needle` in `src`.
fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    let mut idx = 0;
    loop {
        let i = src[from..].find(needle).expect("occurrence") + from;
        if idx == n {
            return TextRange::new(
                u32::try_from(i).unwrap().into(),
                u32::try_from(i + needle.len()).unwrap().into(),
            );
        }
        from = i + needle.len();
        idx += 1;
    }
}

/// Assert the bare pattern head at `range` is **not committed** to any target —
/// unrecorded (the provisional head was dropped) or an honest `Deferred`. FCS
/// binds a (maybe-)literal here, which sema does not commit to, so any
/// `Local` / `Item` commit is a wrong target.
fn assert_defers(rf: &ResolvedFile, range: TextRange, what: &str) {
    match rf.resolution_at(range) {
        None | Some(Resolution::Deferred(_)) => {}
        Some(res) => panic!("{what}: expected no committed resolution, got {res:?}"),
    }
}

// --- the P2: a later same-file literal shadows an opened case ---

#[test]
fn later_literal_defers_opened_active_pattern_case() {
    // FCS binds `B.Even` (the literal); committing A's AP case is the reviewed
    // wrong target.
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\nopen A\n[<Literal>]\nlet Even = 7\nlet f (n: int) = match n with Even -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    assert_defers(
        proj.file(1),
        nth(src1, "Even", 1),
        "bare `Even` after literal",
    );
}

#[test]
fn later_literal_defers_opened_union_case() {
    // Same hole, pre-existing constructor-namespace machinery: FCS binds
    // `B.Red` (the literal), not `A.C.Red`.
    let src0 = "module A\ntype C =\n    | Red\n    | Blue\n";
    let src1 = "module B\nopen A\n[<Literal>]\nlet Red = 9\nlet g (n: int) = match n with Red -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    assert_defers(
        proj.file(1),
        nth(src1, "Red", 1),
        "bare `Red` after literal",
    );
}

// --- position order: the open after the literal re-takes the name ---

#[test]
fn literal_before_open_keeps_the_opened_case() {
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\n[<Literal>]\nlet Even = 7\nopen A\nlet f (n: int) = match n with Even -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Even", 1))
        .expect("bare `Even` resolves — the open post-dates the literal");
    let (file_idx, def) = proj.item_def(res).expect("cross-file item");
    assert_eq!(file_idx, 0);
    assert_eq!(def.kind, DefKind::ActivePattern);
    assert_eq!(def.range, nth(src0, "|Even|Odd|", 0));
}

// --- an unattributed value provably cannot be a literal ---

#[test]
fn unattributed_value_does_not_contest_the_case() {
    // Plain values never enter the pattern namespace (the #593 invariant), and
    // without attributes a `let` cannot be `[<Literal>]`, so the case commits.
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\nopen A\nlet Even = 7\nlet f (n: int) = match n with Even -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let res = proj
        .file(1)
        .resolution_at(nth(src1, "Even", 1))
        .expect("bare `Even` resolves — an unattributed value does not contest");
    let (file_idx, def) = proj.item_def(res).expect("cross-file item");
    assert_eq!(file_idx, 0);
    assert_eq!(def.kind, DefKind::ActivePattern);
}

// --- in-file sequential scope: plain position order ---

#[test]
fn in_file_later_literal_defers_the_case() {
    let src = "module S\ntype C =\n    | Red\n    | Blue\n\n[<Literal>]\nlet Red = 9\n\nlet g (n: int) = match n with Red -> 1 | _ -> 0\n";
    let rf = resolve_file(
        &impl_file(src),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
    );
    assert_defers(&rf, nth(src, "Red", 2), "bare `Red` after in-file literal");
}

#[test]
fn in_file_later_case_beats_the_earlier_literal() {
    let src = "module S\n[<Literal>]\nlet Green = 5\ntype D =\n    | Green\n    | Teal\n\nlet h (d: D) = match d with Green -> 1 | _ -> 0\n";
    let rf = resolve_file(
        &impl_file(src),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
    );
    let res = rf
        .resolution_at(nth(src, "Green", 2))
        .expect("bare `Green` resolves — the case post-dates the literal");
    let def = rf.resolved_def(res).expect("same-file def");
    assert_eq!(def.range, nth(src, "Green", 1), "the case in `type D`");
}

// --- one opened module: vals fold after tycons, so source order is irrelevant ---

#[test]
fn same_module_literal_after_case_defers_through_open() {
    let src0 = "module A\ntype C =\n    | Red\n    | Blue\n\n[<Literal>]\nlet Red = 9\n";
    let src1 = "module B\nopen A\nlet g (n: int) = match n with Red -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    assert_defers(proj.file(1), nth(src1, "Red", 0), "bare `Red` via open");
}

#[test]
fn same_module_literal_before_case_defers_through_open() {
    // The case is declared AFTER the literal, and still loses: FCS folds the
    // opened module's tycons before its vals.
    let src0 = "module A\n[<Literal>]\nlet Green = 5\ntype D =\n    | Green\n    | Teal\n";
    let src1 = "module B\nopen A\nlet h (n: int) = match n with Green -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    assert_defers(proj.file(1), nth(src1, "Green", 0), "bare `Green` via open");
}

#[test]
fn same_file_module_literal_defers_through_open_either_order() {
    // The same vals-after-tycons rule for a module opened from its OWN file.
    let src_a = "module S\nmodule M =\n    type C =\n        | Red\n        | Blue\n\n    [<Literal>]\n    let Red = 9\n\nopen M\nlet g (n: int) = match n with Red -> 1 | _ -> 0\n";
    let rf = resolve_file(
        &impl_file(src_a),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
    );
    assert_defers(&rf, nth(src_a, "Red", 2), "bare `Red` via same-file open");

    let src_b = "module S\nmodule M =\n    [<Literal>]\n    let Green = 5\n    type D =\n        | Green\n        | Teal\n\nopen M\nlet h (n: int) = match n with Green -> 1 | _ -> 0\n";
    let rf = resolve_file(
        &impl_file(src_b),
        &ProjectItems::default(),
        &AssemblyEnv::default(),
    );
    assert_defers(
        &rf,
        nth(src_b, "Green", 2),
        "bare `Green` via same-file open",
    );
}

// --- the qualified path: FCS commits the CASE even with a same-path literal ---

#[test]
fn qualified_case_pattern_never_commits_the_literal() {
    // FCS pin (build-clean, both source orders): a qualified case pattern
    // (`A.Green`) resolves to the **case**, ignoring the same-path literal —
    // the qualified path is decided in the module's content namespace, where
    // the case wins. Today sema *declines* this shape (a sound miss — the
    // literal-bearing path is not committed either way); committing the case
    // here is a possible follow-up. This test pins the soundness half: the
    // resolution is never the literal value.
    for (src0, case_occurrence) in [
        (
            "module A\n[<Literal>]\nlet Green = 5\ntype D =\n    | Green\n    | Teal\n",
            1,
        ),
        (
            "module A\ntype D =\n    | Green\n    | Teal\n[<Literal>]\nlet Green = 5\n",
            0,
        ),
    ] {
        let src1 = "module B\nopen A\nlet q (d: D) = match d with A.Green -> 2 | _ -> 0\n";
        let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
        match proj.file(1).resolution_at(nth(src1, "A.Green", 0)) {
            None | Some(Resolution::Deferred(_)) => {}
            Some(res) => {
                let (file_idx, def) = proj.item_def(res).expect("cross-file item");
                assert_eq!(file_idx, 0);
                assert_eq!(
                    def.range,
                    nth(src0, "Green", case_occurrence),
                    "a committed qualified `A.Green` must be the case, never the literal"
                );
            }
        }
    }
}

// --- assembly: a CLI-literal field contests an earlier project case ---

#[test]
fn assembly_literal_defers_an_earlier_project_case() {
    // `Demo.ApLiteral.Consts.Marker` is a `[<Literal>]` (a CLI `Literal`-flagged
    // field — *definitely* a constant pattern). The `open` post-dates the
    // project case, so FCS binds the literal; committing the case is wrong.
    let bytes = std::fs::read(fixture_path()).expect("read active-pattern fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse active-pattern fixture dll");
    let env = AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv");
    let src = "module P\ntype C =\n    | Marker\n    | Other\nopen Demo.ApLiteral.Consts\nlet f (n: int) = match n with Marker -> 1 | _ -> 0\n";
    let rf = resolve_file(&impl_file(src), &ProjectItems::default(), &env);
    assert_defers(
        &rf,
        nth(src, "Marker", 1),
        "bare `Marker` after assembly open",
    );
}

fn fixture_path() -> &'static Path {
    crate::common::ensure_active_pattern_fixture_built()
}

// --- namespace straddle: an auto-open submodule literal vs a direct case ---

#[test]
fn auto_open_submodule_literal_defers_a_direct_namespace_case() {
    // FCS pin (build-clean): `open N` folds the namespace's direct tycon tier
    // first, then its `[<AutoOpen>]` submodules — so the submodule's literal
    // post-dates the direct case `N.C.X` and wins the bare pattern
    // (`N.Sub.X`). The straddle's ctor-winner re-push must not override it
    // (codex round 1).
    let src0 = "namespace N\n\ntype C =\n    | X\n    | Zed\n\n[<AutoOpen>]\nmodule Sub =\n    [<Literal>]\n    let X = 4\n";
    let src1 =
        "module B\n\nopen N\n\nlet f (n: int) =\n    match n with\n    | X -> 1\n    | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    assert_defers(
        proj.file(1),
        nth(src1, "X", 0),
        "bare `X` with an auto-open submodule literal",
    );
}
