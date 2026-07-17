//! Direct (FCS-free) tests for **exception-constructor name resolution**: an
//! `exception E of …` definition introduces a constructor `E` in the *value*
//! namespace (and an exception type, a later slice). It is interned as a
//! [`DefKind::ExceptionCase`] binder, its defining occurrence resolves to
//! itself, and a same-file *use* — as a constructor in an expression
//! (`E "x"` / `Bang`) or as a pattern head (`match e with E m -> …`) — resolves
//! to that binder. An exception is never `[<RequireQualifiedAccess>]`, so its
//! constructor is always in unqualified scope.
//!
//! These assert the resolver's output directly (no oracle), pinning exact
//! ranges/kinds; the FCS differential over exception snippets lives in
//! `resolve_diff.rs`. Mirrors the style of `resolve_union_cases.rs`.

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
/// exception-constructor definition at occurrence index `def_idx`, with
/// [`DefKind::ExceptionCase`].
fn assert_exception_use(src: &str, needle: &str, use_idx: usize, def_idx: usize) {
    let rf = resolve(src);
    let use_range = nth(src, needle, use_idx);
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use ({use_idx}) in {src:?}"));
    // An exception constructor resolves as a `Local` (anonymous-root file) or an
    // `Item` (a real-root file, where it is exported with one identity for
    // cross-file resolution); either way it names an in-file def.
    assert!(
        matches!(res, Resolution::Local(_) | Resolution::Item(_)),
        "expected an in-file exception resolution for {needle:?} in {src:?}, got {res:?}"
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
        DefKind::ExceptionCase,
        "{needle:?} should be an exception case"
    );
}

#[test]
fn exception_definition_resolves_to_itself() {
    let src = "exception MyErr of string\n";
    let rf = resolve(src);
    let def_range = nth(src, "MyErr", 0);
    let res = rf
        .resolution_at(def_range)
        .expect("exception def occurrence resolves to itself");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, def_range);
    assert_eq!(def.kind, DefKind::ExceptionCase);
}

#[test]
fn nullary_exception_use_in_expression_resolves() {
    // `let e = Bang` — the `Bang` expression use jumps to the exception def.
    assert_exception_use("exception Bang\nlet e = Bang\n", "Bang", 1, 0);
}

#[test]
fn exception_with_payload_used_as_constructor_resolves() {
    // `let e = MyErr "x"` — `MyErr` (a constructor application) resolves to the
    // exception def.
    assert_exception_use(
        "exception MyErr of string\nlet e = MyErr \"x\"\n",
        "MyErr",
        1,
        0,
    );
}

#[test]
fn nullary_exception_pattern_head_resolves() {
    // `match x with Bang -> …`: the nullary pattern head resolves to the
    // exception def rather than binding a fresh variable.
    let src = "exception Bang\nlet f x = match x with Bang -> 1 | _ -> 0\n";
    assert_exception_use(src, "Bang", 1, 0);
}

#[test]
fn payload_exception_pattern_resolves_head_and_binds_arg() {
    // `match x with MyErr z -> z | _ -> ""` — `MyErr` resolves to the exception;
    // the payload binder `z` is a fresh local and the body `z` resolves to it.
    // The binder is named `z` — a letter that appears nowhere else in the snippet
    // — so the single-char `nth(.., "z", ..)` search is unambiguous (`m` would
    // collide with the `m` in `match`).
    let src = "exception MyErr of string\nlet f x = match x with MyErr z -> z | _ -> \"\"\n";
    assert_exception_use(src, "MyErr", 1, 0);
    let rf = resolve(src);
    let z_use = nth(src, "z", 1);
    let res = rf
        .resolution_at(z_use)
        .expect("payload binder `z` use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "z", 0), "`z` use points at its binder");
    assert!(
        !matches!(def.kind, DefKind::ExceptionCase),
        "the payload binder `z` is a local, not an exception case"
    );
}

#[test]
fn abbreviation_introduces_constructor() {
    // `exception Alias = MyErr` introduces a new constructor `Alias` in the value
    // namespace (FCS reports `Alias` as its own in-file symbol); `Alias "x"`
    // resolves to that definition. "Alias": def (0), use (1).
    let src = "exception MyErr of string\nexception Alias = MyErr\nlet e = Alias \"x\"\n";
    assert_exception_use(src, "Alias", 1, 0);
}

#[test]
fn abbreviation_target_resolves_to_original_exception() {
    // The abbreviation *target* `= MyErr` is an in-file exception reference: it
    // resolves to the original exception def (go-to-definition on the target),
    // even though FCS's symbol-use dump does not surface it. "MyErr": def (0),
    // abbreviation target (1).
    let src = "exception MyErr of string\nexception Alias = MyErr\nlet e = Alias \"x\"\n";
    assert_exception_use(src, "MyErr", 1, 0);
}

#[test]
fn abbreviation_target_is_shadowed_by_a_later_value() {
    // An abbreviation target is resolved through the ordinary *value* namespace
    // (latest-wins), NOT a type/exception-only lookup: a later same-named value
    // shadows the exception, so `exception Other = E` resolves its target `E` to
    // the `let E = 0` value (occurrence 1), not the earlier exception. F# then
    // reports FS0921 "Not an exception" — a *type* error, but the *name* still
    // resolves to the value, and FS0921 only fires because a value (not a type)
    // was found. Verified directly against the F# compiler. "E": exception def
    // (0), value def (1), abbreviation target (2).
    let src = "exception E of string\nlet E = 0\nexception Other = E\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "E", 2))
        .expect("abbreviation target resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "E", 1),
        "target resolves to the shadowing value, not the earlier exception"
    );
    assert!(
        matches!(def.kind, DefKind::Value { .. }),
        "target resolves to the value that shadows the exception"
    );
}

#[test]
fn later_value_shadows_exception_in_expression() {
    // `exception MyErr of string` then `let MyErr = 0` — the later *value* shadows
    // the earlier exception constructor for an *expression* use (FCS resolves the
    // use to the value): source order, latest binding wins via `lookup`.
    // "MyErr": exception def (0), value def (1), use (2).
    let src = "exception MyErr of string\nlet MyErr = 0\nlet y = MyErr\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "MyErr", 2))
        .expect("MyErr use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "MyErr", 1),
        "expression use points at the later value"
    );
    assert!(matches!(def.kind, DefKind::Value { .. }));
}

#[test]
fn pattern_head_resolves_to_exception_despite_shadowing_value() {
    // In *pattern* position an exception constructor is resolved through F#'s
    // constructor namespace, which ordinary values do not enter — so a later value
    // `MyErr` does NOT shadow the exception `MyErr` for a pattern head (FCS
    // resolves it to the exception), even though it *would* in an expression.
    // "MyErr": exception def (0), value def (1), pattern head (2).
    let src = "exception MyErr of string\nlet MyErr = 0\nlet f x = match x with MyErr m -> m | _ -> \"\"\n";
    assert_exception_use(src, "MyErr", 2, 0);
}

#[test]
fn exception_does_not_leak_across_namespace_blocks() {
    // An exception declared directly under `namespace A` is not visible
    // unqualified in a later `namespace B` block — distinct namespaces are
    // separate scopes, so resolving `Bang` in B's module to A's exception would
    // be wrong.
    let src = "namespace A\nexception Bang\nnamespace B\nmodule M =\n    let e = Bang\n";
    let rf = resolve(src);
    let use_res = rf.resolution_at(nth(src, "Bang", 1));
    assert!(
        !matches!(use_res, Some(Resolution::Local(_))),
        "an exception from namespace A must not leak into namespace B, got {use_res:?}"
    );
}

#[test]
fn nested_module_sees_enclosing_namespace_exception() {
    // An exception at `namespace N` level: a nested `module M` sees its
    // constructor `Bang` unqualified (FCS: `N.Bang`). "Bang": def (0), use in M
    // (1).
    let src = "namespace N\nexception Bang\nmodule M =\n    let e = Bang\n";
    assert_exception_use(src, "Bang", 1, 0);
}
