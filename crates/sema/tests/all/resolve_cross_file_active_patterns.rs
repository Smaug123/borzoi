//! FCS-free tests for **cross-file active-pattern case** resolution (Stage 3a of
//! `docs/export-decl-model-plan.md`): a module-level active-pattern recognizer
//! (`let (|Even|Odd|) …`) declared in an earlier Compile-order file becomes
//! visible to a later file's `open` in the **pattern (constructor) namespace
//! only**, carrying the recognizer *shape* so a parameterized use splits its
//! arguments exactly as a same-file one does.
//!
//! FCS pins (via `fcs-dump uses-project`, every fixture diagnostics-clean):
//!
//! - `open A; match x with Even` resolves the head cross-file to the recognizer
//!   span in file A (`A.(|Even|Odd|).Even`);
//! - bare `Even` in *expression* position after `open A` is **FS0039** — AP cases
//!   are pattern-namespace-only, never in the value namespace;
//! - a parameterized `DivBy divisor` (partial, arity 1, `k = 1`) resolves
//!   `divisor` to the **outer value**, no fabricated binder;
//! - a total single-case `Scale g` (`k = 1`) *binds* `g` (frontAndBack).
//!
//! Mirrors the same-file `resolve_active_patterns.rs` split tests and the
//! cross-file style of `resolve_cross_file_cases.rs`.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, DefKind, Resolution, resolve_project};
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

#[test]
fn opened_cross_file_active_pattern_case_resolves_to_recognizer() {
    // `open A` brings A's active-pattern cases into pattern scope; `match n with
    // Even`/`Odd` resolve cross-file to the recognizer span in file A (FCS:
    // `A.(|Even|Odd|).Even`, decl at the `|Even|Odd|` name range).
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\nopen A\nlet f n = match n with Even -> 1 | Odd -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    for case in ["Even", "Odd"] {
        // The use in file B is the *last* occurrence of the case name.
        let use_range = nth(src1, case, 0);
        let res = proj
            .file(1)
            .resolution_at(use_range)
            .unwrap_or_else(|| panic!("a resolution at bare `{case}` pattern head"));
        let (file_idx, def) = proj
            .item_def(res)
            .unwrap_or_else(|| panic!("`{case}` resolves to a cross-file item"));
        assert_eq!(file_idx, 0, "`{case}` declared in file0");
        assert_eq!(
            def.range,
            nth(src0, "|Even|Odd|", 0),
            "`{case}` points at file0's recognizer span"
        );
        assert_eq!(
            def.kind,
            DefKind::ActivePattern,
            "`{case}` is an AP case use"
        );
    }
}

#[test]
fn cross_file_active_pattern_case_is_not_an_expression_value() {
    // Bare `Even` in *expression* position after `open A` is FS0039 in FCS: AP
    // cases are pattern-namespace-only. Our resolver must not point it at the
    // recognizer (it declines — Deferred/unrecorded).
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\nopen A\nlet x = Even\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    let use_range = nth(src1, "Even", 0);
    match proj.file(1).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("expression-position `Even` must not resolve as the case, got {other:?}"),
    }
    assert!(
        proj.file(1)
            .resolution_at(use_range)
            .and_then(|r| proj.item_def(r))
            .is_none(),
        "expression-position `Even` must not point at any def"
    );
}

#[test]
fn value_namespace_still_resolves_while_active_pattern_case_is_excluded() {
    // A module A exporting BOTH a value `payload` and an AP case `Even`: after
    // `open A`, the expression `payload` resolves (value namespace intact), while
    // the expression `Even` does not (AP cases provably excluded from the value
    // namespace).
    let src0 = "module A\nlet payload = 3\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\nopen A\nlet a = payload\nlet b = Even\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    // `payload` resolves cross-file to file0's value.
    let payload_use = nth(src1, "payload", 0);
    let (pf, pdef) = proj
        .item_def(
            proj.file(1)
                .resolution_at(payload_use)
                .expect("payload resolves"),
        )
        .expect("payload is a cross-file item");
    assert_eq!(pf, 0);
    assert_eq!(pdef.range, nth(src0, "payload", 0));

    // `Even` in expression position does NOT resolve to the AP case.
    let even_use = nth(src1, "Even", 0);
    assert!(
        proj.file(1)
            .resolution_at(even_use)
            .and_then(|r| proj.item_def(r))
            .is_none(),
        "expression-position `Even` must be excluded from the value namespace"
    );
}

#[test]
fn cross_file_partial_parameterized_case_resolves_param_to_outer_value() {
    // `(|DivBy|_|) d n` is partial, single-case, arity 1. Cross-file `open A;
    // match n with DivBy divisor` (k = 1 = paramCount) makes `divisor` a
    // parameter → FCS resolves it to the outer `let divisor` in file B, NOT a
    // fabricated pattern-local. The head `DivBy` resolves to file0's recognizer.
    let src0 =
        "module A\nlet (|DivBy|_|) (d: int) (n: int) = if n % d = 0 then Some () else None\n";
    let src1 =
        "module B\nopen A\nlet divisor = 3\nlet f n = match n with DivBy divisor -> 1 | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    // `DivBy` head → file0's recognizer.
    let head = nth(src1, "DivBy", 0);
    let (hf, hdef) = proj
        .item_def(rf.resolution_at(head).expect("DivBy head resolves"))
        .expect("DivBy is a cross-file item");
    assert_eq!(hf, 0);
    assert_eq!(hdef.range, nth(src0, "|DivBy|_|", 0));

    // `divisor` argument → the outer `let divisor` in file B (NOT a binder).
    let divisor_use = nth(src1, "divisor", 1); // 0 = the `let`, 1 = the use
    let res = rf
        .resolution_at(divisor_use)
        .expect("`divisor` argument resolves");
    let def = rf.resolved_def(res).expect("names a same-file def");
    assert_eq!(
        def.range,
        nth(src1, "divisor", 0),
        "`divisor` points at the outer `let divisor`, not a fabricated binder"
    );
    assert_eq!(def.kind, DefKind::Value { is_function: false });
}

#[test]
fn cross_file_total_single_case_partial_application_binds_result() {
    // `(|Scale|) k x` is total, single-case, arity 1. Cross-file `Scale g` (k = 1)
    // splits frontAndBack — the lone arg `g` is the *result*, binding at itself,
    // NOT the outer `let g`. "g": outer `let g` (0), the pattern binder (1), the
    // body use (2).
    let src0 = "module A\nlet (|Scale|) (k: int) (x: int) = k * x\n";
    let src1 = "module B\nopen A\nlet g = 7\nlet s n = match n with Scale g -> g\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    let body_use = nth(src1, "g", 2);
    let res = rf.resolution_at(body_use).expect("`g` body use resolves");
    let def = rf.resolved_def(res).expect("names a same-file def");
    assert_eq!(
        def.range,
        nth(src1, "g", 1),
        "`g` binds at its own pattern occurrence, not the outer value"
    );
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn cross_file_split_leaves_no_scope_entry_for_arm_body() {
    // In `open A; match n with DivBy divisor -> divisor`, the arm body's `divisor`
    // resolves to the outer value (the skipped parameter binder must not leave a
    // scope entry that the body would see).
    let src0 =
        "module A\nlet (|DivBy|_|) (d: int) (n: int) = if n % d = 0 then Some () else None\n";
    let src1 = "module B\nopen A\nlet divisor = 3\nlet f n = match n with DivBy divisor -> divisor | _ -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    // The body `divisor` (occurrence index 2: the `let`, the pattern arg, the body).
    let body_use = nth(src1, "divisor", 2);
    let res = rf
        .resolution_at(body_use)
        .expect("`divisor` body use resolves");
    let def = rf.resolved_def(res).expect("names a same-file def");
    assert_eq!(
        def.range,
        nth(src1, "divisor", 0),
        "arm body `divisor` resolves to the outer value"
    );
}

#[test]
fn latest_open_wins_for_cross_file_active_pattern_case() {
    // Two modules both export `Even`; `open A1; open A2` — the later open wins in
    // pattern position (FCS: `A2.(|Even|NotEven|).Even`).
    let src0 = "module A1\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module A2\nlet (|Even|NotEven|) n = if n % 2 = 0 then Even else NotEven\n";
    let src2 = "module B\nopen A1\nopen A2\nlet f n = match n with Even -> 1 | _ -> 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );

    let use_range = nth(src2, "Even", 0);
    let (file_idx, def) = proj
        .item_def(
            proj.file(2)
                .resolution_at(use_range)
                .expect("`Even` resolves"),
        )
        .expect("`Even` is a cross-file item");
    assert_eq!(file_idx, 1, "the later `open A2` wins");
    assert_eq!(def.range, nth(src1, "|Even|NotEven|", 0));
}

#[test]
fn sole_active_pattern_trigger_no_longer_suppresses_sibling_union_cases() {
    // A module hidden ONLY by its AP cases becomes enumerable once they cross the
    // boundary — so its union cases, today over-suppressed, resolve cross-file too
    // (FCS: `A.Color.Red`). Guards the narrowed hidden-value trigger.
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\ntype Color = Red | Green\n";
    let src1 = "module B\nopen A\nlet name c = match c with Red -> 1 | Green -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let rf = proj.file(1);

    for (case, def_needle) in [("Red", "Red"), ("Green", "Green")] {
        let use_range = nth(src1, case, 0);
        let (file_idx, def) = proj
            .item_def(
                rf.resolution_at(use_range).unwrap_or_else(|| {
                    panic!("union case `{case}` resolves despite the sibling AP")
                }),
            )
            .unwrap_or_else(|| panic!("`{case}` is a cross-file item"));
        assert_eq!(file_idx, 0);
        assert_eq!(def.range, nth(src0, def_needle, 0));
    }
}

#[test]
fn same_file_and_cross_file_active_pattern_case_uses_share_one_identity() {
    // Find-references / rename span BOTH same-file and cross-file uses of an AP
    // case, so both must resolve to the SAME project-global `Resolution::Item`
    // (the union-case precedent). File0 uses `Even` in its own `g`; file1 uses it
    // cross-file via `open A`.
    let src0 = "module A\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\nlet g x = match x with Even -> 1 | Odd -> 0\n";
    let src1 = "module B\nopen A\nlet f x = match x with Even -> 1 | Odd -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());

    // Same-file use: the 3rd `Even` occurrence (0 = recognizer name, 1 = body
    // construction, 2 = `g`'s match head).
    let same_file = proj
        .file(0)
        .resolution_at(nth(src0, "Even", 2))
        .expect("same-file `Even` use resolves");
    let cross_file = proj
        .file(1)
        .resolution_at(nth(src1, "Even", 0))
        .expect("cross-file `Even` use resolves");
    assert!(
        matches!(same_file, Resolution::Item(_)),
        "same-file AP case use is a project-global Item, not a file-local Local"
    );
    assert_eq!(
        same_file, cross_file,
        "same-file and cross-file uses of one AP case share one identity"
    );
}

#[test]
fn later_file_case_wins_over_earlier_auto_open_active_pattern() {
    // FCS-verified (`uses-project`, diagnostics-clean): `open P` folds an earlier
    // file's `[<AutoOpen>]` submodule active pattern `(|Red|_|)` and a LATER file's
    // direct union case `Red`; Compile-order provenance makes the later union case
    // win (FCS: `P.C.Red`). Guards that AP cases participate in the constructor
    // namespace's straddle provenance (they ride `value_exports`), so an earlier AP
    // never wrongly out-positions a later same-named case.
    let src0 = "namespace P\n\n[<AutoOpen>]\nmodule Sub =\n    let (|Red|_|) (x: int) = if x = 0 then Some () else None\n";
    let src1 = "namespace P\n\ntype C = Red | Blue\n";
    let src2 = "module Client\nopen P\nlet f x = match x with Red -> 1 | _ -> 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let (file_idx, def) = proj
        .item_def(
            proj.file(2)
                .resolution_at(nth(src2, "Red", 0))
                .expect("`Red` resolves"),
        )
        .expect("`Red` is a cross-file item");
    assert_eq!(file_idx, 1, "the later union case (file1) wins");
    assert_eq!(def.range, nth(src1, "Red", 0), "points at `P.C.Red`");
}

#[test]
fn public_active_pattern_recovered_under_a_later_inaccessible_private() {
    // A public `N.A.(|P|_|)` in file0 and a later `let private (|P|_|)` augmenting
    // `N.A` in file1: from a module outside `N`, the private is inaccessible, so
    // `open N.A; match x with P` must recover the earlier PUBLIC recognizer (FCS:
    // the accessibility-history recovery, exactly as for a value shadowed by a
    // later `private`). Guards that AP cases keep the per-path export history, not
    // a latest-wins slot.
    let src0 =
        "namespace N\nmodule A =\n    let (|P|_|) (x: int) = if x = 0 then Some () else None\n";
    let src1 = "namespace N\nmodule A =\n    let private (|P|_|) (x: int) = if x = 1 then Some () else None\n";
    let src2 = "module Client\nopen N.A\nlet f x = match x with P -> 1 | _ -> 0\n";
    let proj = resolve_project(
        &[impl_file(src0), impl_file(src1), impl_file(src2)],
        &AssemblyEnv::default(),
    );
    let (file_idx, _def) = proj
        .item_def(
            proj.file(2)
                .resolution_at(nth(src2, "P", 0))
                .expect("`P` resolves"),
        )
        .expect("`P` recovers the public recognizer");
    assert_eq!(
        file_idx, 0,
        "the accessible PUBLIC recognizer (file0) is recovered under the later private"
    );
}

#[test]
fn active_pattern_case_does_not_mask_an_ordinary_value_qualifier() {
    // FCS-verified (`uses-project`, diagnostics-clean): a container with a VALUE
    // `Color` (record with a `Red` field), a TYPE `Color` (union case `Red`), and
    // an AP case `Color` — a later `Lib.Container.Color.Red` binds the VALUE's
    // `Red` field (`Lib.Container.Color` → the value), NOT the union case: the
    // value shadows the type qualifier. Guards that a trailing pattern-only AP
    // record does not mask the underlying ordinary value in `ordinary_value_at`
    // (which would wrongly commit the union case). We DEFER the value member access
    // (Phase 3) — the sound outcome, never a committed case.
    let src0 = "module Lib\nmodule Container =\n    let Color = {| Red = 42 |}\n    type Color = Red | Blue\n    let (|Color|_|) (x: int) = if x = 0 then Some () else None\n";
    let src1 = "module Client\nlet v = Lib.Container.Color.Red\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    match proj
        .file(1)
        .resolution_at(nth(src1, "Lib.Container.Color.Red", 0))
    {
        None | Some(Resolution::Deferred(_)) => {}
        other => {
            panic!("must not commit the union case over the shadowing value, got {other:?}")
        }
    }
}

#[test]
fn active_pattern_declaring_module_still_hidden_by_another_trigger_declines() {
    // A module hidden for ANOTHER reason (an `extern`, which marks its container
    // hidden) stays hidden even though it also declares an AP: its AP cases are
    // not trusted in pattern position (a hidden value/constructor could shadow
    // them). Sound decline — never a wrong commit.
    let src0 = "module A\nextern int X()\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\n";
    let src1 = "module B\nopen A\nlet f n = match n with Even -> 1 | Odd -> 0\n";
    let proj = resolve_project(&[impl_file(src0), impl_file(src1)], &AssemblyEnv::default());
    let use_range = nth(src1, "Even", 0);
    // Decline (Deferred / unrecorded) — never a wrong commit.
    match proj.file(1).resolution_at(use_range) {
        None | Some(Resolution::Deferred(_)) => {}
        other => panic!("hidden-module AP case must decline, got {other:?}"),
    }
}
