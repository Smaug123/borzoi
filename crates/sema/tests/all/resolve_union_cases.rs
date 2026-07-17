//! Direct (FCS-free) tests for **union-case name resolution**: a union type's
//! cases (`type T = A | B of …`) are interned as [`DefKind::UnionCase`] binders
//! in the *value* scope, their defining occurrence resolves to itself, and a
//! same-file *use* of a case — as a constructor in an expression (`B 3`) or as a
//! pattern head (`match x with B n -> …`) — resolves to that binder.
//!
//! These assert the resolver's output directly (no oracle), pinning exact
//! ranges/kinds; the FCS differential over union snippets lives in
//! `resolve_diff.rs`. Mirrors the style of `resolve_types.rs`.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, DefKind, ProjectItems, Resolution, ResolvedFile, resolve_file};
use rowan::TextRange;

/// Parse `src` (asserting it is in the parser subset) and resolve it as a lone
/// single file.
fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors: {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

/// The byte range of the `n`th (0-based) occurrence of `needle` in `src`.
fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    for _ in 0..n {
        let i = src[from..].find(needle).expect("occurrence") + from;
        from = i + needle.len();
    }
    let i = src[from..].find(needle).expect("occurrence") + from;
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// Assert the use of `needle` at occurrence index `use_idx` resolves to the
/// union-case definition at occurrence index `def_idx`, with
/// [`DefKind::UnionCase`].
fn assert_case_use(src: &str, needle: &str, use_idx: usize, def_idx: usize) {
    let rf = resolve(src);
    let use_range = nth(src, needle, use_idx);
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use ({use_idx}) in {src:?}"));
    // A union case resolves as a `Local` (anonymous-root file) or an `Item` (a
    // real-root file, where a non-qualified case is exported with one identity for
    // cross-file resolution); either way it names an in-file def.
    assert!(
        matches!(res, Resolution::Local(_) | Resolution::Item(_)),
        "expected an in-file case resolution for {needle:?} in {src:?}, got {res:?}"
    );
    let def = rf
        .resolved_def(res)
        .expect("the resolution names an in-file def");
    assert_eq!(
        def.range,
        nth(src, needle, def_idx),
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
    assert_eq!(
        def.kind,
        DefKind::UnionCase,
        "{needle:?} should be a union case"
    );
}

#[test]
fn case_definition_resolves_to_itself() {
    let src = "type Color = Red | Green\n";
    let rf = resolve(src);
    for name in ["Red", "Green"] {
        let def_range = nth(src, name, 0);
        let res = rf
            .resolution_at(def_range)
            .unwrap_or_else(|| panic!("case {name} def occurrence resolves to itself"));
        let def = rf.resolved_def(res).expect("names a def");
        assert_eq!(def.range, def_range);
        assert_eq!(def.kind, DefKind::UnionCase);
    }
}

#[test]
fn nullary_case_use_in_expression_resolves() {
    // `let c = Red` — the `Red` expression use jumps to the case def.
    assert_case_use("type Color = Red | Green\nlet c = Red\n", "Red", 1, 0);
}

#[test]
fn case_with_payload_used_as_constructor_resolves() {
    // `let x = B 3` — `B` (a constructor application) resolves to the case.
    assert_case_use("type T = A | B of int\nlet x = B 3\n", "B", 1, 0);
}

#[test]
fn nullary_case_pattern_heads_resolve() {
    // `match c with Red -> 0 | Green -> 1` — both case heads resolve to their
    // case defs rather than binding a fresh variable.
    let src = "type Color = Red | Green\nlet f c = match c with Red -> 0 | Green -> 1\n";
    assert_case_use(src, "Red", 1, 0);
    assert_case_use(src, "Green", 1, 0);
}

#[test]
fn payload_case_pattern_resolves_head_and_binds_arg() {
    // `match t with B v -> v | A -> 0` — `B` and `A` resolve to their cases; the
    // payload binder `v` is a fresh local and the body `v` resolves to it. The
    // binder is named `v` — a letter that appears nowhere else in the snippet —
    // so the single-char `nth(.., "v", ..)` search is unambiguous (`n` collides
    // with `int`, `y` with `type`).
    let src = "type T = A | B of int\nlet f t = match t with B v -> v | A -> 0\n";
    assert_case_use(src, "B", 1, 0);
    assert_case_use(src, "A", 1, 0);
    let rf = resolve(src);
    let v_use = nth(src, "v", 1);
    let res = rf
        .resolution_at(v_use)
        .expect("payload binder `v` use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "v", 0), "`v` use points at its binder");
    assert!(
        !matches!(def.kind, DefKind::UnionCase),
        "the payload binder `v` is a local, not a case"
    );
}

#[test]
fn value_of_other_name_is_not_a_case() {
    // A normal value binding next to a union type is unaffected: `g`'s use
    // resolves to the value `g`, never to a case.
    let src = "type Color = Red | Green\nlet g x = x\nlet h = g\n";
    let rf = resolve(src);
    let res = rf.resolution_at(nth(src, "g", 1)).expect("g resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "g", 0));
    assert!(matches!(def.kind, DefKind::Value { .. }));
}

#[test]
fn require_qualified_access_case_is_not_in_unqualified_scope() {
    // `[<RequireQualifiedAccess>]` removes a union's cases from the unqualified
    // value scope, so `let c = Red` must NOT resolve to `Color.Red` — FCS leaves
    // it unbound (qualified `Color.Red` is the only path, a later slice).
    // Resolving it would be a wrong go-to-definition (correctness over
    // availability). The case *definition* may still self-resolve.
    let src = "[<RequireQualifiedAccess>]\ntype Color = Red | Green\nlet c = Red\n";
    let rf = resolve(src);
    let use_res = rf.resolution_at(nth(src, "Red", 1));
    assert!(
        !matches!(use_res, Some(Resolution::Local(_))),
        "a require-qualified case must not resolve unqualified, got {use_res:?}"
    );
}

#[test]
fn case_does_not_leak_across_namespace_blocks() {
    // A case declared directly under `namespace A` is not visible unqualified in
    // a later `namespace B` block — distinct namespaces are separate scopes, so
    // resolving `Red` in B's module to A's case would be wrong.
    let src = "namespace A\ntype Color = Red | Green\nnamespace B\nmodule M =\n    let c = Red\n";
    let rf = resolve(src);
    let use_res = rf.resolution_at(nth(src, "Red", 1));
    assert!(
        !matches!(use_res, Some(Resolution::Local(_))),
        "a case from namespace A must not leak into namespace B, got {use_res:?}"
    );
}

#[test]
fn later_case_shadows_earlier_value() {
    // `let Red = 0` then `type Color = Red | Green` — the later *case* shadows the
    // earlier value (FCS resolves the use to `Color.Red`): source order, latest
    // binding wins. Occurrences of "Red": value def (0), case def (1), use (2).
    let src = "let Red = 0\ntype Color = Red | Green\nlet c = Red\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "Red", 2))
        .expect("Red use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 1),
        "use points at the later case"
    );
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn later_value_shadows_earlier_case() {
    // `type Color = Red | Green` then `let Red = 0` — the later *value* shadows
    // the earlier case (FCS resolves the use to the value). Occurrences of "Red":
    // case def (0), value def (1), use (2).
    let src = "type Color = Red | Green\nlet Red = 0\nlet c = Red\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "Red", 2))
        .expect("Red use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 1),
        "use points at the later value"
    );
    assert!(matches!(def.kind, DefKind::Value { .. }));
}

#[test]
fn nested_module_sees_enclosing_namespace_case() {
    // A union at `namespace N` level: a nested `module M` sees its case `A`
    // unqualified (FCS: `N.T.A`). "A": case def (0), use in M (1).
    let src = "namespace N\ntype T = A | B\nmodule M =\n    let x = A\n";
    let rf = resolve(src);
    let res = rf.resolution_at(nth(src, "A", 1)).expect("A use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "A", 0));
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn same_namespace_blocks_merge_cases() {
    // Two `namespace N` blocks merge: a case defined in the first is visible
    // unqualified in the second (FCS: `N.T.A`). "A": case def (0), use (1).
    let src = "namespace N\ntype T = A | B\nnamespace N\nmodule M =\n    let x = A\n";
    let rf = resolve(src);
    let res = rf.resolution_at(nth(src, "A", 1)).expect("A use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "A", 0));
    assert_eq!(def.kind, DefKind::UnionCase);
}

#[test]
fn pattern_head_resolves_to_case_despite_shadowing_value() {
    // In *pattern* position a union case is resolved through F#'s constructor
    // namespace, which ordinary values do not enter — so a later value `Red` does
    // NOT shadow the case `Red` for a pattern head (FCS resolves it to
    // `Color.Red`), even though it *would* in an expression. "Red": case def (0),
    // value def (1), pattern head (2).
    let src =
        "type Color = Red | Green\nlet Red = 0\nlet f c = match c with Red -> 1 | Green -> 2\n";
    assert_case_use(src, "Red", 2, 0);
}

/// Operator-named union cases (`([])` / `( :: )`, FSharp.Core's `list`) carry no
/// identifier token; the resolver must still record them, under FCS's compiled
/// `op_Nil` / `op_ColonColon` names, with the defining occurrence self-resolving
/// at the operator span. (Regression guard: the name derivation lives on the
/// production AST path, not only in the differential-test normaliser.)
#[test]
fn operator_union_cases_are_defined() {
    let src =
        "type L<'T> =\n    | ([]) : 'T list\n    | ( :: ) : Head:'T * Tail:'T list -> 'T list\n";
    let rf = resolve(src);
    for (needle, op_name) in [("[]", "op_Nil"), ("::", "op_ColonColon")] {
        let range = nth(src, needle, 0);
        let res = rf
            .resolution_at(range)
            .unwrap_or_else(|| panic!("the {needle} case has no defining resolution"));
        let def = rf
            .resolved_def(res)
            .unwrap_or_else(|| panic!("the {needle} case resolution has no def"));
        assert_eq!(def.name, op_name, "{needle} → {op_name}");
        assert_eq!(def.kind, DefKind::UnionCase);
    }
}

/// The same for an operator-named *enum* case (`| ([]) = 0`, FCS's bar-led
/// `unionCaseName EQUALS`), interned as a [`DefKind::EnumCase`] under `op_Nil`.
#[test]
fn operator_enum_case_is_defined() {
    let src = "type E =\n    | ([]) = 0\n    | ( :: ) = 1\n";
    let rf = resolve(src);
    for (needle, op_name) in [("[]", "op_Nil"), ("::", "op_ColonColon")] {
        let range = nth(src, needle, 0);
        let res = rf
            .resolution_at(range)
            .unwrap_or_else(|| panic!("the {needle} enum case has no defining resolution"));
        let def = rf
            .resolved_def(res)
            .unwrap_or_else(|| panic!("the {needle} enum case resolution has no def"));
        assert_eq!(def.name, op_name, "{needle} → {op_name}");
        assert_eq!(def.kind, DefKind::EnumCase);
    }
}
