//! Direct (FCS-free) tests for **enum-case name resolution**: an enum
//! `type Color = Red = 0 | Green = 1` introduces cases that are **require-
//! qualified** — reachable only as `Color.Red`, never bare `Red` (FCS reports
//! bare `Red` as `FS0039`). Pinned against FCS (`fcs-dump uses`), which resolves:
//!
//! - each case token in the definition (`Red`, `Green`) to its own range
//!   ([`DefKind::EnumCase`]);
//! - a qualified use `Color.Red` — the head `Color` to the enum *type* def, and
//!   the whole `Color.Red` span to the case def.
//!
//! These assert the resolver's output directly (no oracle), pinning exact
//! ranges/kinds; the FCS differential lives in `resolve_diff.rs`. Mirrors the
//! style of `resolve_union_cases.rs`.

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

/// Assert the occurrence of `needle` at `use_idx` resolves to an in-file def
/// whose range equals `def_range`, with `kind`.
fn assert_resolves_to(
    src: &str,
    needle: &str,
    use_idx: usize,
    def_range: TextRange,
    kind: DefKind,
) {
    let rf = resolve(src);
    let use_range = nth(src, needle, use_idx);
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use ({use_idx}) in {src:?}"));
    let def = rf.resolved_def(res).unwrap_or_else(|| {
        panic!("{needle:?} ({use_idx}) is not an in-file def in {src:?}: {res:?}")
    });
    assert_eq!(
        def.range, def_range,
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
    assert_eq!(
        def.kind, kind,
        "{needle:?} resolves to the wrong kind in {src:?}"
    );
}

#[test]
fn enum_case_definition_self_resolves() {
    // Each case token in the definition self-resolves to its own range, as an
    // `EnumCase`.
    let src = "type Color = Red = 0 | Green = 1\n";
    for name in ["Red", "Green"] {
        let tok = nth(src, name, 0);
        let rf = resolve(src);
        let res = rf
            .resolution_at(tok)
            .unwrap_or_else(|| panic!("enum case {name} self-resolves"));
        let def = rf.resolved_def(res).expect("names a def");
        assert_eq!(def.range, tok, "{name} should self-resolve");
        assert_eq!(def.kind, DefKind::EnumCase);
    }
}

#[test]
fn qualified_enum_case_use_resolves_head_and_whole() {
    // `Color.Red`: the head `Color` resolves to the enum *type* def; the whole
    // `Color.Red` span resolves to the case def. "Color": type def (0), head use
    // (1). "Red": case def (0), within `Color.Red` (1).
    let src = "type Color = Red = 0 | Green = 1\nlet c = Color.Red\n";
    // Head `Color` → the type def.
    assert_resolves_to(src, "Color", 1, nth(src, "Color", 0), DefKind::Type);
    // Whole `Color.Red` → the case def.
    let rf = resolve(src);
    let whole = nth(src, "Color.Red", 0);
    let res = rf.resolution_at(whole).expect("Color.Red resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Red", 0),
        "Color.Red points at the case def"
    );
    assert_eq!(def.kind, DefKind::EnumCase);
}

#[test]
fn qualified_enum_case_pattern_resolves() {
    // A qualified enum case used as a `match` pattern head resolves identically to
    // the expression form (FCS): head `Color` → the enum type, whole `Color.Red`
    // → the case. "Color": type def (0), pattern head (1). "Red": case def (0),
    // within the pattern `Color.Red` (1).
    let src = "type Color = Red = 0 | Green = 1\nlet f c = match c with Color.Red -> 1 | _ -> 0\n";
    assert_resolves_to(src, "Color", 1, nth(src, "Color", 0), DefKind::Type);
    let rf = resolve(src);
    let whole = nth(src, "Color.Red", 0);
    let res = rf.resolution_at(whole).expect("Color.Red pattern resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0));
    assert_eq!(def.kind, DefKind::EnumCase);
}

#[test]
fn nested_module_sees_enclosing_enum() {
    // A nested module sees an enclosing module/namespace's enum: `Color.Red` in
    // `Inner` resolves to `Outer`'s enum (head → the type, whole → the case),
    // walking the container path outward (FCS-verified). "Color": type def (0),
    // head use in Inner (1). "Red": case def (0), within `Color.Red` (1).
    let src =
        "module Outer\ntype Color = Red = 0 | Green = 1\nmodule Inner =\n    let c = Color.Red\n";
    assert_resolves_to(src, "Color", 1, nth(src, "Color", 0), DefKind::Type);
    let rf = resolve(src);
    let whole = nth(src, "Color.Red", 0);
    let res = rf
        .resolution_at(whole)
        .expect("enclosing Color.Red resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0));
    assert_eq!(def.kind, DefKind::EnumCase);
}

#[test]
fn bare_enum_case_does_not_resolve() {
    // Enum cases are require-qualified: a bare `Red` is not in the unqualified
    // value scope (FCS: FS0039), so it must NOT resolve to the case — resolving
    // it would be a wrong go-to-definition. "Red": case def (0), bare use (1).
    let src = "type Color = Red = 0 | Green = 1\nlet c = Red\n";
    let rf = resolve(src);
    let use_res = rf.resolution_at(nth(src, "Red", 1));
    assert!(
        !matches!(use_res, Some(Resolution::Local(_))),
        "a bare enum case must not resolve (require-qualified), got {use_res:?}"
    );
}

#[test]
fn qualified_use_of_a_value_named_like_the_type_is_not_an_enum_case() {
    // If a *value* shadows the type name, `Color.Red` is member access on the
    // value, not an enum-case path — so it must not resolve to the case. Here
    // `Color` is a value, so `Color.Red` is `Deferred` member access (FCS needs
    // the value's type). "Color": value def (0), use (1).
    let src = "type Color = Red = 0 | Green = 1\nlet Color = 0\nlet c = Color.Red\n";
    let rf = resolve(src);
    // The whole `Color.Red` must not resolve to the enum case.
    let whole = nth(src, "Color.Red", 0);
    let res = rf.resolution_at(whole);
    let to_case = matches!(res, Some(r)
        if rf.resolved_def(r).is_some_and(|d| d.kind == DefKind::EnumCase));
    assert!(
        !to_case,
        "a value-shadowed `Color.Red` must not resolve to the enum case, got {res:?}"
    );
}

#[test]
fn later_enum_type_wins_over_earlier_value_qualifier() {
    // The `Color.Red` qualifier resolves latest-wins across the value and type
    // namespaces (FCS). With a value `Color` *before* a later enum `type Color`,
    // the enum type is the latest `Color`, so `Color.Red` resolves to the enum
    // case (head → the type), not member access on the earlier value. "Color":
    // value def (0), type def (1), head use (2). "Red": case def (0), in path (1).
    let src = "let Color = 0\ntype Color = Red = 0 | Green = 1\nlet c = Color.Red\n";
    assert_resolves_to(src, "Color", 2, nth(src, "Color", 1), DefKind::Type);
    let rf = resolve(src);
    let whole = nth(src, "Color.Red", 0);
    let res = rf
        .resolution_at(whole)
        .expect("Color.Red resolves to the case");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "Red", 0));
    assert_eq!(def.kind, DefKind::EnumCase);
}

#[test]
fn redefined_type_drops_stale_enum_cases() {
    // Redefining the type name (last-wins, like `type_defs`) drops the old enum
    // cases: after `type Color = int`, `Color.Red` must NOT combine the new
    // `Color` type with the stale old `Red` case — it defers (the new `Color` has
    // no `Red`).
    let src = "type Color = Red = 0 | Green = 1\ntype Color = int\nlet c = Color.Red\n";
    let rf = resolve(src);
    let whole = nth(src, "Color.Red", 0);
    let res = rf.resolution_at(whole);
    let to_case = matches!(res, Some(r)
        if rf.resolved_def(r).is_some_and(|d| d.kind == DefKind::EnumCase));
    assert!(
        !to_case,
        "a redefined type must not leave stale enum cases, got {res:?}"
    );
}

#[test]
fn nested_module_named_like_enclosing_enum_shadows_it() {
    // A module-like name (here a module abbreviation) shadows a same-named
    // *enclosing* enum type for member access: `Color.Red` inside `Inner`, where
    // `module Color = Outer` aliases a module, is member access through the alias
    // (its members are unmodelled → defer), NOT the enclosing enum's case.
    let src = "module Outer\ntype Color = Red = 0 | Green = 1\nmodule Inner =\n    module Color = Outer\n    let c = Color.Red\n";
    let rf = resolve(src);
    let whole = nth(src, "Color.Red", 0);
    let res = rf.resolution_at(whole);
    let to_case = matches!(res, Some(r)
        if rf.resolved_def(r).is_some_and(|d| d.kind == DefKind::EnumCase));
    assert!(
        !to_case,
        "a module-shadowed `Color.Red` must not resolve to the enclosing enum case, got {res:?}"
    );
}
