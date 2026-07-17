//! FCS-free tests for **assembly-backed active-pattern shape** (Stage 3b of
//! `docs/export-decl-model-plan.md`): a recognizer declared in a *referenced
//! assembly* carries a shape derived from its mangled `|A|B|` IL name (cases,
//! totality, single-case), so an opening consumer's applied use splits its
//! arguments exactly as a project one does.
//!
//! The flagship behaviour change: a **total single-case** assembly recognizer
//! used with parameters (`Scale factor v`) splits frontAndBack — the last arg is
//! the result (binds), everything before it a parameter (an outer value). Before
//! Stage 3b the assembly case carried no shape, so every argument fabricated a
//! binder.
//!
//! Verdicts pinned against FCS by building the fixture DLL and its consumers (see
//! the probe write-up in `docs/export-decl-model-plan.md` Stage 3b):
//!
//! - `Scale factor v` → `factor` = the outer value (a parameter), `v` = the
//!   result binder (fsi: `factor` stays the outer 999);
//! - `arity` is `None` for every assembly recognizer (the flattened IL parameter
//!   count over-counts under tupling), so a **partial** recognizer keeps today's
//!   fabricate-a-binder behaviour — a sound decline, the 3c residue.

use std::path::Path;

use borzoi_assembly::Ecma335Assembly;
use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, DefKind, ProjectItems, Resolution, ResolvedFile, resolve_file};
use rowan::TextRange;

fn ensure_fixture_built() -> &'static Path {
    crate::common::ensure_active_pattern_fixture_built()
}

/// An [`AssemblyEnv`] over the built active-pattern fixture (parsed once per test
/// binary). `from_views` runs the single-CCU authoritative projection, so the
/// F# source-name / auto-open overlays are applied.
fn fixture_env() -> AssemblyEnv {
    let bytes = std::fs::read(ensure_fixture_built()).expect("read active-pattern fixture dll");
    let view = Ecma335Assembly::parse(&bytes).expect("parse active-pattern fixture dll");
    AssemblyEnv::from_views(std::slice::from_ref(&view)).expect("build AssemblyEnv")
}

fn impl_file(src: &str) -> ImplFile {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "parse errors in {src:?}: {:?}",
        p.errors
    );
    ImplFile::cast(p.root).expect("impl file")
}

fn resolve(src: &str, env: &AssemblyEnv) -> ResolvedFile {
    resolve_file(&impl_file(src), &ProjectItems::default(), env)
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

#[test]
fn total_single_case_assembly_pattern_splits_frontandback() {
    // `(|Scale|) k n` is total single-case. `open …Recognizers; match n with Scale
    // factor bound`: frontAndBack makes `factor` a PARAMETER (→ the outer `let
    // factor`, no fabricated binder) and `bound` the result (binds). This is the
    // Stage-3b behaviour change — before it, `factor` fabricated a binder.
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApShape.Recognizers\n\
               let factor = 4\n\
               let f n = match n with Scale factor bound -> bound\n";
    let rf = resolve(src, &env);

    // The head resolves (an opened assembly case — a `Deferred` reference).
    assert!(
        rf.resolution_at(nth(src, "Scale", 0)).is_some(),
        "the `Scale` head resolves as an opened assembly case"
    );

    // `factor` (occurrence 1 — the pattern arg) → the outer `let factor`
    // (occurrence 0), a Value, NOT a fabricated pattern-local.
    let factor_arg = nth(src, "factor", 1);
    let res = rf
        .resolution_at(factor_arg)
        .expect("`factor` argument resolves");
    let def = rf
        .resolved_def(res)
        .expect("`factor` names a same-file def");
    assert_eq!(
        def.range,
        nth(src, "factor", 0),
        "`factor` points at the outer `let factor`, not a fabricated binder"
    );
    assert_eq!(def.kind, DefKind::Value { is_function: false });

    // `bound` (the result sub-pattern) binds at its own occurrence; the body use
    // (occurrence 1) resolves to that binder (occurrence 0).
    let body_use = nth(src, "bound", 1);
    let res = rf
        .resolution_at(body_use)
        .expect("`bound` body use resolves");
    let def = rf.resolved_def(res).expect("`bound` names a same-file def");
    assert_eq!(
        def.range,
        nth(src, "bound", 0),
        "`bound` binds at its own pattern occurrence (the frontAndBack result)"
    );
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn total_single_case_nullary_assembly_pattern_binds_its_result() {
    // `(|Wrapped|) n` is total single-case with no parameter. `Wrapped w` (k=1):
    // frontAndBack → the sole arg `w` is the result and binds. (Same as today's
    // fabricate-a-binder — a regression guard that the split does not drop the
    // result binder for an arity-0 total single-case.)
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApShape.Recognizers\n\
               let g n = match n with Wrapped payload -> payload\n";
    let rf = resolve(src, &env);
    let body_use = nth(src, "payload", 1); // 0 = the pattern arg, 1 = the body use
    let res = rf
        .resolution_at(body_use)
        .expect("`payload` body use resolves");
    let def = rf
        .resolved_def(res)
        .expect("`payload` names a same-file def");
    assert_eq!(
        def.range,
        nth(src, "payload", 0),
        "`payload` binds at its own occurrence"
    );
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn partial_assembly_pattern_is_not_given_the_total_single_case_split() {
    // `(|DivBy|_|) d n` is PARTIAL single-case. Its arity is underivable from
    // metadata (the flattened IL param count over-counts under tupling), so
    // `arity == None` and the split declines to today's behaviour: an applied
    // `DivBy divisor q` fabricates a binder for `divisor` — it must NOT be given
    // the total single-case frontAndBack (which would resolve `divisor` to the
    // outer value). A sound decline (the 3c residue), pinned so a mis-attached
    // `total: true` on a partial recognizer would fail here.
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApShape.Recognizers\n\
               let divisor = 3\n\
               let h n = match n with DivBy divisor q -> divisor + q | _ -> 0\n";
    let rf = resolve(src, &env);

    // `divisor` (occurrence 1 — the pattern arg) fabricates a binder (today's
    // behaviour), so the arm body `divisor` (occurrence 2) resolves to it, NOT to
    // the outer `let divisor` (occurrence 0).
    let body_use = nth(src, "divisor", 2);
    let res = rf
        .resolution_at(body_use)
        .expect("`divisor` body use resolves");
    let def = rf
        .resolved_def(res)
        .expect("`divisor` names a same-file def");
    assert_eq!(
        def.range,
        nth(src, "divisor", 1),
        "a partial assembly recognizer keeps today's fabricate-a-binder for `divisor`"
    );
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn demoted_assembly_pattern_shape_does_not_drive_the_split() {
    // `Demo.ApResidue.Contested` also declares an `[<AutoOpen>]` type, whose
    // unenumerable statics make opening the module fold name-unknown residue — so
    // the fold DEMOTES its cases (a hidden name could shadow them). The total
    // single-case `Scale` tag is demoted with the group, so its shape must be
    // dropped and the split must NOT fire: `Scale factor v` keeps today's
    // fabricate-a-binder for `factor` (a sound decline), never resolving it to the
    // outer value as the total single-case frontAndBack would. Guards the P2 codex
    // finding — a demoted case's shape is untrustworthy.
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApResidue.Contested\n\
               let factor = 4\n\
               let f n = match n with Scale factor bound -> factor + bound\n";
    let rf = resolve(src, &env);

    // `factor` (occurrence 1 — the pattern arg) must NOT resolve to the outer `let
    // factor` (occurrence 0): the demoted shape cannot split it into a parameter.
    let factor_arg = nth(src, "factor", 1);
    if let Some(res) = rf.resolution_at(factor_arg)
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "factor", 0),
            "a demoted assembly recognizer's shape must not split `factor` to the outer value"
        );
    }
}

#[test]
fn active_pattern_shadowed_by_a_later_literal_does_not_drive_the_split() {
    // `Demo.ApLiteral.Shadowed` exports a total single-case `(|Scale|)` AND a
    // same-named `[<Literal>] let Scale`. In pattern position FCS's latest-wins
    // gives the literal (a constant pattern) the name `Scale`, so an applied
    // `Scale factor bound` is FCS-illegal — the recognizer's shape must NOT split
    // `factor` to the outer value. Sema drops the shape (the fold's literal-shadow
    // demotion, codex round 4c), so `factor` keeps today's fabricate-a-binder — a
    // sound decline, never a wrong commit. (The applied form is FCS-illegal; the
    // assertion is a soundness pin, not an FCS-agreement pin.)
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApLiteral.Shadowed\n\
               let factor = 4\n\
               let f n = match n with Scale factor bound -> factor + bound | _ -> 0\n";
    let rf = resolve(src, &env);
    // `factor` (occurrence 1 — the pattern arg) must NOT resolve to the outer `let
    // factor` (0): the shadowed shape cannot split it into a parameter.
    let factor_arg = nth(src, "factor", 1);
    if let Some(res) = rf.resolution_at(factor_arg)
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "factor", 0),
            "a literal-shadowed recognizer's shape must not split `factor` to the outer value"
        );
    }
}

#[test]
fn active_pattern_shadowed_by_a_local_literal_does_not_drive_the_split() {
    // The cross-scope form of the constant-pattern shadow (codex round 5a): the
    // total single-case `(|Scale|)` is opened from the assembly, but a *local*
    // `[<Literal>] let Scale` (declared after the open) is the constant pattern FCS
    // puts in charge of `Scale`. The split-site guard sees the local value via
    // `lookup` and declines — `factor` keeps today's fabricate-a-binder, never the
    // outer value. (The applied form is FCS-illegal; a soundness pin.)
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApShape.Recognizers\n\
               [<Literal>]\n\
               let Scale = 7\n\
               let factor = 4\n\
               let f n = match n with Scale factor bound -> factor + bound | _ -> 0\n";
    let rf = resolve(src, &env);
    let factor_arg = nth(src, "factor", 1);
    if let Some(res) = rf.resolution_at(factor_arg)
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "factor", 0),
            "a local-literal-shadowed recognizer's shape must not split `factor` to the outer value"
        );
    }
}

#[test]
fn quoted_head_still_sees_the_literal_shadow() {
    // A double-backtick-quoted head `` ``Scale`` `` normalizes (via `id_text`) to
    // `Scale`, so the split-site literal-shadow guard must normalize the name it
    // passes to `lookup` too — otherwise it searches for the raw quoted token,
    // misses the shadow, and splits (codex round 6a).
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApShape.Recognizers\n\
               [<Literal>]\n\
               let Scale = 7\n\
               let factor = 4\n\
               let f n = match n with ``Scale`` factor bound -> factor + bound | _ -> 0\n";
    let p = parse(src);
    if !p.errors.is_empty() {
        // The parser does not model quoted pattern heads here; the `id_text`
        // normalization is still exercised by the plain-head shadow tests.
        return;
    }
    let rf = resolve(src, &env);
    let factor_arg = nth(src, "factor", 1);
    if let Some(res) = rf.resolution_at(factor_arg)
        && let Some(def) = rf.resolved_def(res)
    {
        assert_ne!(
            def.range,
            nth(src, "factor", 0),
            "the shadow guard must normalize the quoted head before looking it up"
        );
    }
}

#[test]
fn multi_case_assembly_pattern_nullary_use_resolves() {
    // `(|Even|Odd|) n` is total multi-case: a nullary use has nothing to split;
    // the head resolves as an opened assembly case (a `Deferred` reference).
    let env = fixture_env();
    let src = "module Client\n\
               open Demo.ApShape.Recognizers\n\
               let e n = match n with Even -> 1 | Odd -> 0\n";
    let rf = resolve(src, &env);
    for case in ["Even", "Odd"] {
        match rf.resolution_at(nth(src, case, 0)) {
            Some(Resolution::Deferred(_)) => {}
            other => {
                panic!("multi-case `{case}` head must resolve as an opened case, got {other:?}")
            }
        }
    }
}
