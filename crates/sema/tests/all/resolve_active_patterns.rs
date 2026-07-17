//! Direct (FCS-free) tests for **active-pattern name resolution**: a
//! `let (|Even|Odd|) … = …` definition introduces an active-pattern *recognizer*
//! (a top-level value spanning the `|Even|Odd|` name, [`DefKind::ActivePattern`])
//! and per-case tokens ([`DefKind::ActivePatternCase`]). Pinned against FCS
//! (`fcs-dump uses`), which resolves:
//!
//! - each case *token* in the definition (`Even`) to its own range;
//! - the recognizer name (`(|Even|Odd|)`) to its `|Even|Odd|` span (parens
//!   excluded);
//! - every case *use* — a `match`/expression occurrence (`match x with Even`,
//!   or `then Even` constructing the case in the recognizer body) — to the
//!   **recognizer span**, not the individual case token.
//!
//! These assert the resolver's output directly (no oracle), pinning exact
//! ranges/kinds; the FCS differential over active-pattern snippets lives in
//! `resolve_diff.rs`. Mirrors the style of `resolve_union_cases.rs`.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{
    ActivePatternShape, AssemblyEnv, DefKind, ProjectItems, Resolution, ResolvedFile,
    SemanticClass, resolve_file,
};
use rowan::{TextRange, TextSize};

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
    assert!(
        matches!(res, Resolution::Local(_)),
        "expected a Local resolution for {needle:?} in {src:?}, got {res:?}"
    );
    let def = rf
        .resolved_def(res)
        .expect("a Local resolution names an in-file def");
    assert_eq!(
        def.range, def_range,
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
    assert_eq!(
        def.kind, kind,
        "{needle:?} resolves to the wrong kind in {src:?}"
    );
}

/// Assert the occurrence of `needle` at `use_idx` resolves to an in-file def
/// (a [`Resolution::Local`] *or* a same-file [`Resolution::Item`]) whose range
/// equals `def_range`, with `kind`. Unlike [`assert_resolves_to`] this does not
/// require the resolution to be `Local` — a module-level `let divisor` is an
/// `Item`, and a shape-split parameter argument resolves to exactly that.
fn assert_use_resolves_to_def(
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
    let def = rf
        .resolved_def(res)
        .unwrap_or_else(|| panic!("{needle:?} use ({use_idx}) names no in-file def in {src:?}"));
    assert_eq!(
        def.range, def_range,
        "{needle:?} use ({use_idx}) points at the wrong def in {src:?}"
    );
    assert_eq!(
        def.kind, kind,
        "{needle:?} use ({use_idx}) resolves to the wrong kind in {src:?}"
    );
}

#[test]
fn recognizer_name_self_resolves() {
    // The recognizer name occurrence resolves to itself at its `|Even|Odd|` span
    // (parens excluded), as an `ActivePattern`.
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\n";
    let recog = nth(src, "|Even|Odd|", 0);
    let rf = resolve(src);
    let res = rf
        .resolution_at(recog)
        .expect("recognizer name resolves to itself");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, recog);
    assert_eq!(def.kind, DefKind::ActivePattern);
}

#[test]
fn case_token_in_definition_self_resolves() {
    // Each case token in the definition (`Even`, `Odd`) self-resolves to its own
    // range, as an `ActivePatternCase` — distinct from the recognizer span.
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\n";
    for name in ["Even", "Odd"] {
        let tok = nth(src, name, 0);
        let rf = resolve(src);
        let res = rf
            .resolution_at(tok)
            .unwrap_or_else(|| panic!("case token {name} self-resolves"));
        let def = rf.resolved_def(res).expect("names a def");
        assert_eq!(def.range, tok, "{name} token should self-resolve");
        assert_eq!(def.kind, DefKind::ActivePatternCase);
    }
}

#[test]
fn case_is_not_an_expression_value() {
    // An active-pattern case is *not* a value in expression position (FCS:
    // `let v = Even` is FS0039, even though `match x with Even` resolves). So a
    // post-definition expression use of a case name does not resolve to the
    // recognizer; it falls through to `Deferred` (a sound coverage gap, never a
    // wrong resolution). "Even": case token in def (0), `then Even` body
    // construction (1), `let v = Even` post-definition use (2).
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\nlet v = Even\n";
    let rf = resolve(src);
    let use_res = rf.resolution_at(nth(src, "Even", 2));
    assert!(
        !matches!(use_res, Some(Resolution::Local(_))),
        "an active-pattern case is not an expression value, got {use_res:?}"
    );
}

#[test]
fn active_pattern_case_does_not_shadow_a_same_named_value_in_expression_position() {
    // FCS-verified (`uses-project`, diagnostics-clean): in a NAMED module, `let
    // Even = 42; let (|Even|Odd|) …; let x = Even`, the expression `Even` resolves
    // to the ordinary VALUE (`M.Even`), not the recognizer — an AP case is
    // pattern-namespace-only, so it must not shadow a same-named value in
    // expression position, even though it is now `Item`-backed (Stage 3a). "Even":
    // value decl (0), recognizer name (1), body construction (2), `let x` use (3).
    let src = "module M\nlet Even = 42\nlet (|Even|Odd|) n = if n % 2 = 0 then Even else Odd\nlet x = Even\n";
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, "Even", 3))
        .expect("`Even` expression use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "Even", 0),
        "resolves to the ordinary value `let Even`, not the recognizer"
    );
    assert_eq!(def.kind, DefKind::Value { is_function: false });
}

#[test]
fn active_pattern_value_reference_does_not_misresolve() {
    // An active-pattern name in *expression* position (`(|Foo|_|)`) is an
    // `opName` value reference; the recognizer is not keyed in value scope, so
    // the reference is deferred (a coverage gap, never wrong). Crucially, the
    // surrounding path tokens must NOT be mis-resolved: in `(|Foo|_|).Bar` the
    // `.Bar` is member access on the active-pattern value, never the in-scope
    // local `Bar`. (The active-pattern name sits in the `LONG_IDENT` as an
    // `ACTIVE_PAT_NAME` node, invisible to `idents()`, so a naive token
    // projection would feed `["Bar"]` to the path resolver and grab the local.)
    // "Bar": the `let Bar` def (0), the `.Bar` member use (1). A value
    // resolution here is either `Item` (a top-level binding, as `let Bar`) or
    // `Local`; both are wrong for a member-access segment.
    let src = "let Bar = 1\nlet f = (|Foo|_|).Bar\n";
    let rf = resolve(src);
    let member_use = rf.resolution_at(nth(src, "Bar", 1));
    assert!(
        !matches!(member_use, Some(Resolution::Local(_) | Resolution::Item(_))),
        "`.Bar` member access on an active-pattern value must not resolve to the in-file value `Bar`, got {member_use:?}"
    );
}

#[test]
fn dot_qualified_active_pattern_value_reference_does_not_misresolve() {
    // `Foo.(|Bar|_|)` — the active-pattern name qualified by module `Foo`. The
    // truncated `idents()` would be `["Foo"]`; resolving that as a value would
    // grab a same-named local. Deferred instead (coverage gap, never wrong).
    // "Foo": the `let Foo` def (0), the `Foo.` qualifier use (1).
    let src = "let Foo = 1\nlet g = Foo.(|Bar|_|)\n";
    let rf = resolve(src);
    let qualifier_use = rf.resolution_at(nth(src, "Foo", 1));
    assert!(
        !matches!(
            qualifier_use,
            Some(Resolution::Local(_) | Resolution::Item(_))
        ),
        "the `Foo` qualifier of an active-pattern path must not resolve to the in-file value `Foo`, got {qualifier_use:?}"
    );
}

#[test]
fn case_construction_in_recognizer_body_is_declined() {
    // A total active pattern constructs its cases in its own body (`then Even`),
    // which FCS resolves to the recognizer. A bare case name in the body is,
    // however, ambiguous with a fresh uppercase *pattern* rebinding of that name
    // (see `fresh_uppercase_pattern_in_recognizer_body_is_not_the_case`), which a
    // resolution-only pass cannot tell apart — so we DECLINE the body construction
    // (a decline barrier around the RHS stops it committing an outer same-named
    // value). A sound coverage gap, not a wrong answer. "Even": token in def (0),
    // `then Even` body use (1).
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\n";
    let rf = resolve(src);
    let body_use = rf.resolution_at(nth(src, "Even", 1));
    assert!(
        !matches!(body_use, Some(Resolution::Local(_) | Resolution::Item(_))),
        "body case construction must decline, got {body_use:?}"
    );
}

#[test]
fn body_case_construction_does_not_bind_a_shadowed_outer_value() {
    // The original soundness bug: an earlier `let USome` value in scope while the
    // recognizer's own body constructs `USome`. Without the decline barrier the
    // body `USome` resolved to the outer *value* (wrong: FCS reports an
    // `ActivePatternCase`). It must not commit that value — it declines. "USome":
    // outer `let` (0), case token in the name (1), `then USome` body use (2).
    let src = "let USome x = x + 1\n\
               let (|UNone|USome|) x = if x > 0 then USome x else UNone\n";
    let rf = resolve(src);
    let body_use = rf.resolution_at(nth(src, "USome", 2));
    assert!(
        !matches!(body_use, Some(Resolution::Local(_) | Resolution::Item(_))),
        "the body case use must not bind the shadowed outer value, got {body_use:?}"
    );
}

#[test]
fn qualified_head_sharing_a_case_name_still_resolves_in_recognizer_body() {
    // The recognizer-body case-name decline must affect only BARE case-name
    // expressions, never a *qualified* head. With `type A` (a static-member type)
    // sharing the case name `A`, `let q = A.X` inside `let (|A|B|) n = …` resolves
    // — in FCS — to the type `A` and its member `X`, NOT case construction. The
    // decline barrier must not intercept the qualified head: `A` still classifies
    // as a Type, and the member tail `X` as a Member.
    let src = "module Probe\n\
               type A() =\n    static member X = 1\n\
               let (|A|B|) n =\n    let q = A.X\n    n\n";
    let rf = resolve(src);
    let base = src.rfind("A.X").expect("the qualified use");
    let head = TextRange::new(
        u32::try_from(base).unwrap().into(),
        u32::try_from(base + 1).unwrap().into(),
    );
    assert_eq!(
        rf.classification_at(head),
        Some(SemanticClass::Type),
        "the qualified head `A` (a type sharing the case name) must still resolve as a Type"
    );
    let tail_off = base + 2; // skip "A."
    let tail = TextRange::new(
        u32::try_from(tail_off).unwrap().into(),
        u32::try_from(tail_off + 1).unwrap().into(),
    );
    assert_eq!(
        rf.token_classifier()(tail),
        Some(SemanticClass::Member),
        "the qualified member tail `X` must resolve as a Member"
    );
}

#[test]
fn fresh_uppercase_pattern_in_recognizer_body_is_not_the_case() {
    // `let (|A|B|) n = match n with A -> A | B -> B` type-checks, and FCS binds the
    // body pattern `A`/`B` as FRESH LOCALS (Mfv) — the branch expressions are those
    // locals, NOT the active-pattern cases. A resolution-only pass drops the fresh
    // uppercase binder and cannot distinguish this from a case construction, so the
    // body expression `A` must DECLINE rather than commit the recognizer (which
    // would be a wrong `ActivePattern` classification). "A": case token (0), body
    // pattern (1), branch expr (2).
    let src = "let (|A|B|) n = match n with A -> A | B -> B\n";
    let recog = nth(src, "|A|B|", 0);
    let rf = resolve(src);
    let branch = rf.resolution_at(nth(src, "A", 2));
    let to_recognizer = matches!(branch, Some(res)
        if rf.resolved_def(res).is_some_and(|d| d.range == recog));
    assert!(
        !to_recognizer,
        "a fresh uppercase pattern rebinding must not resolve to the recognizer, got {branch:?}"
    );
}

#[test]
fn distinct_cases_have_distinct_use_resolutions() {
    // Each case keeps a distinct `Resolution` identity, so find-references / rename
    // on one case does not pull in its siblings — even though both go-to-definition
    // to the same recognizer span. The `Even` and `Odd` pattern uses must resolve
    // to *different* resolutions, both pointing at a def ranged at the recognizer.
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\nlet c x = match x with Even -> 0 | Odd -> 1\n";
    let recog = nth(src, "|Even|Odd|", 0);
    let rf = resolve(src);
    let even = rf
        .resolution_at(nth(src, "Even", 2))
        .expect("Even use resolves");
    let odd = rf
        .resolution_at(nth(src, "Odd", 2))
        .expect("Odd use resolves");
    assert_ne!(
        even, odd,
        "distinct cases must have distinct resolutions (else find-refs collapses them)"
    );
    assert_eq!(rf.resolved_def(even).unwrap().range, recog);
    assert_eq!(rf.resolved_def(odd).unwrap().range, recog);
}

#[test]
fn nullary_case_pattern_head_resolves_to_recognizer() {
    // A case used as a `match` pattern head resolves to the recognizer span (not
    // the case token). "Even": token in def (0), `then Even` (1), `match … Even`
    // (2). "Odd": token (0), `else Odd` (1), `| Odd` (2).
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\nlet c x = match x with Even -> 0 | Odd -> 1\n";
    let recog = nth(src, "|Even|Odd|", 0);
    assert_resolves_to(src, "Even", 2, recog, DefKind::ActivePattern);
    assert_resolves_to(src, "Odd", 2, recog, DefKind::ActivePattern);
}

#[test]
fn partial_active_pattern_case_use_resolves_and_binds_payload() {
    // A partial active pattern `(|Parse|_|)`: the `_` is not a case; `Parse` used
    // as an applied pattern head resolves to the recognizer span, and the payload
    // binder `v` binds. "Parse": token in def (0), `match … Parse` use (1).
    let src = "let (|Parse|_|) s = if s = \"\" then None else Some s\nlet f x = match x with Parse v -> v | _ -> \"\"\n";
    let recog = nth(src, "|Parse|_|", 0);
    assert_resolves_to(src, "Parse", 1, recog, DefKind::ActivePattern);
    // payload binder `v`: def (0), use (1).
    let rf = resolve(src);
    let v_use = nth(src, "v", 1);
    let res = rf.resolution_at(v_use).expect("payload `v` use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "v", 0), "`v` use points at its binder");
    assert!(matches!(
        def.kind,
        DefKind::PatternLocal | DefKind::Parameter
    ));
}

#[test]
fn partial_underscore_is_not_a_case() {
    // The trailing `_` of `(|Parse|_|)` is not a case: it must not be interned as
    // an `ActivePatternCase`, so its token in the definition records no
    // self-resolution (FCS reports no symbol there).
    let src = "let (|Parse|_|) s = if s = \"\" then None else Some s\nlet f x = match x with Parse v -> v | _ -> \"\"\n";
    let rf = resolve(src);
    // The `_` inside `|_|` in the definition.
    let bars = nth(src, "|_|", 0);
    let us_range = TextRange::new(
        bars.start() + TextSize::from(1),
        bars.start() + TextSize::from(2),
    );
    assert!(
        rf.resolution_at(us_range).is_none(),
        "the partial `_` must not be interned as a case"
    );
}

#[test]
fn parameterized_active_pattern_head_resolves_and_binds_args() {
    // A parameterized active pattern `(|DivBy|_|) d n`: `d` and `n` bind as
    // params; in a use `match n with DivBy 3 -> …` the head resolves to the
    // recognizer and the literal `3` binds nothing. "DivBy": token (0), use (1).
    let src = "let (|DivBy|_|) d n = if n % d = 0 then Some() else None\nlet h n = match n with DivBy 3 -> 1 | _ -> 0\n";
    let recog = nth(src, "|DivBy|_|", 0);
    assert_resolves_to(src, "DivBy", 1, recog, DefKind::ActivePattern);
    // `d` is a parameter: def (0), use in `n % d` (1).
    let rf = resolve(src);
    let d_use = nth(src, "d", 1);
    let res = rf.resolution_at(d_use).expect("`d` use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "d", 0));
    assert_eq!(def.kind, DefKind::Parameter);
}

// ---- Stage 2: shape-keyed split of applied active-pattern arguments --------
//
// A parameterized active-pattern *use* splits its arguments into parameters
// (expressions, resolved in the enclosing scope) and the result sub-pattern (a
// binder), keyed on the recognizer's shape — mirroring FCS's
// `TcPatLongIdentActivePatternCase`. See
// `docs/parameterized-active-pattern-args-plan.md`.

#[test]
fn split_partial_param_resolves_to_outer_value() {
    // `(|DivBy|_|) d n` is partial, single-case, arity 1; a use `DivBy divisor`
    // (k = 1 = paramCount) makes `divisor` a *parameter* — FCS resolves it to the
    // outer `let divisor`, NOT a fabricated pattern-local. "divisor": the outer
    // `let` def (0), the pattern-param use (1).
    let src = "let divisor = 3\n\
               let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n\
               let h n = match n with DivBy divisor -> 1 | _ -> 0\n";
    assert_use_resolves_to_def(
        src,
        "divisor",
        1,
        nth(src, "divisor", 0),
        DefKind::Value { is_function: false },
    );
}

#[test]
fn split_partial_param_then_result_binder() {
    // `DivBy divisor q` (k = 2 = paramCount + 1): `divisor` is the parameter
    // (outer value), `q` is the result sub-pattern (a fresh binder). "divisor":
    // def (0), use (1). "q": pattern binder (0), body use (1).
    let src = "let divisor = 3\n\
               let (|DivBy|_|) d n = if n % d = 0 then Some (n / d) else None\n\
               let h n = match n with DivBy divisor q -> q | _ -> 0\n";
    assert_use_resolves_to_def(
        src,
        "divisor",
        1,
        nth(src, "divisor", 0),
        DefKind::Value { is_function: false },
    );
    let rf = resolve(src);
    let q_use = nth(src, "q", 1);
    let res = rf.resolution_at(q_use).expect("`q` body use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "q", 0), "`q` use points at its binder");
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn split_partial_param_then_nested_active_pattern_result() {
    // `DivBy divisor (Parse z)`: `divisor` → parameter (outer value); the result
    // `(Parse z)` is a nested applied active-pattern head, which re-enters the
    // same split — `Parse` is partial arity 0, so its argument `z` binds.
    // "divisor": def (0), use (1). "z": pattern binder (0), body use (1).
    let src = "let divisor = 3\n\
               let (|Parse|_|) s = if s = 0 then None else Some s\n\
               let (|DivBy|_|) d n = if n % d = 0 then Some (n / d) else None\n\
               let h n = match n with DivBy divisor (Parse z) -> z | _ -> 0\n";
    assert_use_resolves_to_def(
        src,
        "divisor",
        1,
        nth(src, "divisor", 0),
        DefKind::Value { is_function: false },
    );
    let rf = resolve(src);
    let z_use = nth(src, "z", 1);
    let res = rf.resolution_at(z_use).expect("`z` body use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(def.range, nth(src, "z", 0), "`z` use points at its binder");
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn split_total_single_case_partial_application_binds_result() {
    // `(|Scale|) k x` is total, single-case, arity 1. A use `Scale g` (k = 1)
    // splits `frontAndBack` — arity is NEVER consulted for a total single-case —
    // so the lone arg `g` is the *result*, binding at itself (the partially-applied
    // recognizer), NOT the outer `let g`. This pins the frontAndBack branch: the
    // original draft's positional rule would have wrongly treated `g` as a
    // parameter. "g": outer `let g` (0), the pattern binder (1), the body use (2).
    let src = "let g = 7\n\
               let (|Scale|) k x = k * x\n\
               let s n = match n with Scale g -> g\n";
    let rf = resolve(src);
    let body_use = nth(src, "g", 2);
    let res = rf.resolution_at(body_use).expect("`g` body use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "g", 1),
        "`g` must bind at its own pattern occurrence, not the outer value"
    );
    assert_eq!(def.kind, DefKind::PatternLocal);
}

#[test]
fn split_parenthesised_param_resolves_to_outer_value() {
    // `DivBy (divisor)`: the parameter is parenthesised. The exclusion must key on
    // the *ident-token* range (`divisor`), not the `Paren` node range — else the
    // fabricated binder escapes. `divisor` resolves to the outer value, with no
    // fabricated binder. "divisor": def (0), parenthesised param use (1).
    let src = "let divisor = 3\n\
               let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n\
               let h n = match n with DivBy (divisor) -> 1 | _ -> 0\n";
    assert_use_resolves_to_def(
        src,
        "divisor",
        1,
        nth(src, "divisor", 0),
        DefKind::Value { is_function: false },
    );
}

#[test]
fn split_tuple_shaped_param_binds_nothing_and_commits_no_local() {
    // A tuple-shaped parameter (`DivBy (aa, bb)` for the arity-1 `DivBy`): the
    // exclusion prevents fabricated `aa` / `bb` binders, and each tuple element is
    // resolved as an *expression* (FCS resolves them as outer values). Here `aa` /
    // `bb` are not in scope, so the expression resolution declines to `Deferred` —
    // never a committed in-file local binder (the key soundness property).
    let src = "let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n\
               let h n = match n with DivBy (aa, bb) -> 1 | _ -> 0\n";
    let rf = resolve(src);
    for name in ["aa", "bb"] {
        let occ = nth(src, name, 0);
        let res = rf.resolution_at(occ);
        let committed_local = matches!(res, Some(r) if rf.resolved_def(r).is_some());
        assert!(
            !committed_local,
            "tuple-shaped active-pattern parameter {name:?} must not fabricate a binder or \
             commit to an in-file def, got {res:?}"
        );
    }
}

#[test]
fn split_param_leaves_no_scope_entry_for_arm_body() {
    // The skipped parameter binder must not leave a scope entry: in
    // `match n with DivBy divisor -> divisor`, the arm body's `divisor` resolves
    // to the outer value, exactly as the pattern-position use does. "divisor":
    // def (0), pattern param (1), arm body (2).
    let src = "let divisor = 3\n\
               let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n\
               let h n = match n with DivBy divisor -> divisor | _ -> 0\n";
    assert_use_resolves_to_def(
        src,
        "divisor",
        2,
        nth(src, "divisor", 0),
        DefKind::Value { is_function: false },
    );
}

#[test]
fn split_nullary_uppercase_param_resolves_as_value() {
    // A nullary uppercase parameter argument that names a same-file union case
    // (`match x with Eq A -> …`, `Eq` an arity-1 partial AP): `A` is a value-position
    // expression — the union-case constructor — which FCS resolves to the case
    // (fsi-verified legal). The CST models `A` as a `Pat::LongIdent`, so the param
    // helper must resolve it through expression-namespace lookup, not decline it.
    // "A": the case token in `type T = A | B` (0), the `Eq A` param use (1).
    let src = "type T = A | B\n\
               let (|Eq|_|) (t: T) (x: T) = if t = x then Some() else None\n\
               let classify x = match x with Eq A -> 1 | _ -> 0\n";
    assert_use_resolves_to_def(src, "A", 1, nth(src, "A", 0), DefKind::UnionCase);
}

#[test]
fn split_param_in_let_head_does_not_wrong_commit_to_outer() {
    // `let f p (DivBy p) = p` — the inner `p` (in `DivBy p`) is an active-pattern
    // parameter that FCS scopes to the FIRST curried parameter `p` (fsi-verified:
    // `let f d (DivBy d)` matches on the first param). Our split resolves parameter
    // expressions against the enclosing scope, which does NOT yet contain the
    // earlier curried param — so committing to the module-level `let p` would be a
    // WRONG target. In a binding-head position we therefore DECLINE the parameter
    // expression (sound coverage gap) rather than wrong-commit. "p": module `let p`
    // (0), first curried param (1), the inner AP argument (2), the body (3).
    let src = "let p = 100\n\
               let (|DivBy|_|) k n = if n % k = 0 then Some() else None\n\
               let f p (DivBy p) = p\n";
    let rf = resolve(src);
    let inner = rf.resolution_at(nth(src, "p", 2));
    let module_p = nth(src, "p", 0);
    let wrong = matches!(inner, Some(res)
        if rf.resolved_def(res).is_some_and(|d| d.range == module_p));
    assert!(
        !wrong,
        "the inner active-pattern parameter must not wrong-commit to the module-level \
         value (FCS scopes it to the earlier curried param), got {inner:?}"
    );
}

#[test]
fn split_param_in_lambda_head_does_not_wrong_commit_to_outer() {
    // The lambda counterpart of the above: `fun p (DivBy p) -> p`. The inner `p`
    // must not wrong-commit to the module-level `let p`. "p": module `let p` (0),
    // the `fun p` param (1), the inner AP argument (2), the body (3).
    let src = "let p = 100\n\
               let (|DivBy|_|) k n = if n % k = 0 then Some() else None\n\
               let f = fun p (DivBy p) -> p\n";
    let rf = resolve(src);
    let inner = rf.resolution_at(nth(src, "p", 2));
    let module_p = nth(src, "p", 0);
    let wrong = matches!(inner, Some(res)
        if rf.resolved_def(res).is_some_and(|d| d.range == module_p));
    assert!(
        !wrong,
        "the inner active-pattern parameter of a lambda must not wrong-commit to the \
         module-level value, got {inner:?}"
    );
}

#[test]
fn split_binding_head_param_still_resolves_type_annotations() {
    // Declining a binding-head parameter *expression* must not also drop its *type
    // annotations*: type names live in a separate namespace, unaffected by the
    // curried-parameter shadowing risk. In `let f (Eq (expected: Ann)) = …`, the
    // annotation `Ann` (a same-file type) must still resolve even though the value
    // `expected` is declined. "Ann": the type def (0), the annotation use (1).
    let src = "type Ann = M0 | N0\n\
               let (|Eq|_|) a b = if a = b then Some() else None\n\
               let f (Eq (expected: Ann)) = 0\n";
    assert_use_resolves_to_def(src, "Ann", 1, nth(src, "Ann", 0), DefKind::Type);
}

#[test]
fn split_binding_head_tuple_param_resolves_nested_type() {
    // A compound (tuple) parameter argument must still be traversed for nested type
    // annotations even when its value uses are declined: `EqPair ((a: Ann), b)` in a
    // let head must resolve `Ann`. "Ann": the type def (0), the annotation use (1).
    let src = "type Ann = A0 | B0\n\
               let (|EqPair|_|) pair x = if x = 0 then Some pair else None\n\
               let f (EqPair ((a: Ann), b)) = 0\n";
    assert_use_resolves_to_def(src, "Ann", 1, nth(src, "Ann", 0), DefKind::Type);
}

#[test]
fn split_binding_head_quote_param_resolves_nested_type() {
    // A quotation parameter argument must be traversed for nested type annotations:
    // `EqQ <@ (1: Ann) @>` in a let head must resolve `Ann` (the previous
    // `Pat::Quote` recursion did). "Ann": the type def (0), the annotation inside
    // the quote (1).
    let src = "type Ann = A0 | B0\n\
               let (|EqQ|_|) q x = if x = 0 then Some q else None\n\
               let f (EqQ <@ (1: Ann) @>) = 0\n";
    assert_use_resolves_to_def(src, "Ann", 1, nth(src, "Ann", 0), DefKind::Type);
}

#[test]
fn split_not_applied_to_direct_let_function_head() {
    // A *direct `let` head* that is an applied single-segment `LongIdent`
    // (`let DivBy x = x`) DEFINES a function `DivBy` (a binder), even when an
    // active-pattern case `DivBy` is in scope — it is a function-binding form, not
    // an active-pattern *use*. The split must be suppressed here, so the genuine
    // parameter `x` still binds and the body resolves to it (FCS-legal:
    // `let DivBy x = x` after the recognizer type-checks). "x": the param binder
    // (0), the body use (1).
    let src = "let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n\
               let DivBy x = x\n";
    let rf = resolve(src);
    let body = nth(src, "x", 1);
    let res = rf.resolution_at(body).expect("`x` body use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "x", 0),
        "`x` body must resolve to the function's parameter binder, not be dropped"
    );
    assert_eq!(def.kind, DefKind::Parameter);
}

#[test]
fn split_not_applied_to_local_let_function_head() {
    // The same, at the *local* `let` site (`resolve_local_let`, not
    // `prepare_binding`): `let DivBy x = x` inside a function body still defines a
    // function whose parameter `x` binds. "x": the param binder (0), body use (1).
    let src = "let outer =\n    \
               let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n    \
               let DivBy x = x\n    \
               DivBy 5\n";
    let rf = resolve(src);
    let body = nth(src, "x", 1);
    let res = rf.resolution_at(body).expect("`x` body use resolves");
    let def = rf.resolved_def(res).expect("names a def");
    assert_eq!(
        def.range,
        nth(src, "x", 0),
        "`x` body must resolve to the function's parameter binder, not be dropped"
    );
    assert_eq!(def.kind, DefKind::Parameter);
}

#[test]
fn case_in_scope_for_later_decls() {
    // An active pattern's cases are visible to later top-level decls in the same
    // container (like union cases). "Even": token (0), `then Even` (1), later use
    // (2).
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\nlet test x = match x with Even -> true | Odd -> false\n";
    let recog = nth(src, "|Even|Odd|", 0);
    assert_resolves_to(src, "Even", 2, recog, DefKind::ActivePattern);
}

#[test]
fn local_active_pattern_resolves_like_module_level() {
    // An active pattern defined *inside* an expression (`let f x = let (|Even|Odd|)
    // … in …`) resolves identically in pattern position: a later `match` head in
    // the same expression body resolves to the recognizer span. "Even": token (0),
    // `then Even` body construction (1, a declined gap), `match … Even` (2).
    let src = "let f x =\n    let (|Even|Odd|) n = if n = 0 then Even else Odd\n    match x with Even -> 0 | Odd -> 1\n";
    let recog = nth(src, "|Even|Odd|", 0);
    assert_resolves_to(src, "Even", 2, recog, DefKind::ActivePattern);
}

#[test]
fn nonrec_recognizer_body_pattern_case_is_not_the_recognizer() {
    // A case used as a *pattern* inside a NON-`rec` recognizer's own body is a
    // fresh variable, not the case (FCS: the body `Even` declares itself; the
    // recognizer is not in scope in its own non-`rec` RHS). So it must NOT resolve
    // to the recognizer span. "Even": token in def (0), body pattern (1).
    let src = "let (|Even|Odd|) n = match n with Even -> 1 | Odd -> 2\n";
    let recog = nth(src, "|Even|Odd|", 0);
    let rf = resolve(src);
    let body = rf.resolution_at(nth(src, "Even", 1));
    let to_recognizer = matches!(body, Some(res)
        if rf.resolved_def(res).is_some_and(|d| d.range == recog));
    assert!(
        !to_recognizer,
        "a non-rec recognizer's own body pattern must not resolve to itself, got {body:?}"
    );
}

#[test]
fn rec_recognizer_body_pattern_case_resolves_to_recognizer() {
    // With `let rec`, the recognizer IS in scope in its own body, so a case used
    // as a pattern there resolves to the recognizer span (FCS). "Even": token in
    // def (0), body pattern (1).
    let src = "let rec (|Even|Odd|) n = match n with Even -> 1 | Odd -> 2\n";
    let recog = nth(src, "|Even|Odd|", 0);
    assert_resolves_to(src, "Even", 1, recog, DefKind::ActivePattern);
}

#[test]
fn case_does_not_leak_across_namespace_blocks() {
    // A case from an active pattern under `namespace A` is not visible unqualified
    // in a later `namespace B` block.
    let src = "namespace A\nmodule M =\n    let (|Even|Odd|) n = if n = 0 then Even else Odd\nnamespace B\nmodule N =\n    let c x = match x with Even -> 0 | Odd -> 1\n";
    let rf = resolve(src);
    // The `Even` in namespace B's `match` (the last occurrence) must not resolve
    // to namespace A's recognizer.
    let last_even = nth(src, "Even", 2);
    let res = rf.resolution_at(last_even);
    assert!(
        !matches!(res, Some(Resolution::Local(_))),
        "an active-pattern case from namespace A must not leak into namespace B, got {res:?}"
    );
}

/// The [`ActivePatternShape`] the resolver stored for a recognizer, observed
/// through a *use* of one of its cases: resolve `src`, take the resolution of
/// the `use_idx`th occurrence of case name `case`, and read back its stored
/// shape. (The shape is keyed by the per-case *use def id* — the identity
/// `case_reference` returns — so any pattern use of the case reaches it.)
fn shape_of_case_use(src: &str, case: &str, use_idx: usize) -> ActivePatternShape {
    let rf = resolve(src);
    let res = rf
        .resolution_at(nth(src, case, use_idx))
        .unwrap_or_else(|| panic!("no resolution at {case:?} use ({use_idx}) in {src:?}"));
    rf.active_pattern_shape(res)
        .unwrap_or_else(|| panic!("no stored active-pattern shape for {case:?} use in {src:?}"))
}

#[test]
fn shape_total_multi_case_zero_arity() {
    // `(|Even|Odd|) n` — total (no trailing `|_|`), multi-case (two idents),
    // function form with one curried arg (the matched value), so arity 0.
    let src = "let (|Even|Odd|) n = if n = 0 then Even else Odd\n\
               let c x = match x with Even -> 0 | Odd -> 1\n";
    assert_eq!(
        shape_of_case_use(src, "Even", 2),
        ActivePatternShape {
            total: true,
            single_case: false,
            arity: Some(0),
        }
    );
}

#[test]
fn shape_partial_single_case_one_arity() {
    // `(|DivBy|_|) d n` — partial (trailing `|_|`), single-case, two curried
    // args (`d`, `n`), so one parameter (`d`); arity 1.
    let src = "let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n\
               let h n = match n with DivBy 3 -> 1 | _ -> 0\n";
    assert_eq!(
        shape_of_case_use(src, "DivBy", 1),
        ActivePatternShape {
            total: false,
            single_case: true,
            arity: Some(1),
        }
    );
}

#[test]
fn shape_total_single_case_one_arity() {
    // `(|Scale|) k x` — total, single-case, two curried args, one parameter
    // (`k`); arity 1.
    let src = "let (|Scale|) k x = k * x\n\
               let s v = match v with Scale y -> y\n";
    assert_eq!(
        shape_of_case_use(src, "Scale", 1),
        ActivePatternShape {
            total: true,
            single_case: true,
            arity: Some(1),
        }
    );
}

#[test]
fn shape_of_named_module_case_use_via_public_accessor() {
    // The public `active_pattern_shape` accessor must resolve the shape for a
    // same-file MODULE-LEVEL case use, which is now `Item`-backed (Stage 3a), not
    // only the anonymous-root `Local` form — so `match n with DivBy 3` in a named
    // module still splits its arguments. "DivBy": recognizer name (0), match use (1).
    let src = "module M\nlet (|DivBy|_|) d n = if n % d = 0 then Some () else None\nlet f n = match n with DivBy 3 -> 1 | _ -> 0\n";
    assert_eq!(
        shape_of_case_use(src, "DivBy", 1),
        ActivePatternShape {
            total: false,
            single_case: true,
            arity: Some(1),
        }
    );
}

#[test]
fn shape_partial_single_case_zero_arity() {
    // `(|Parse|_|) s` — partial, single-case, one curried arg (the matched
    // value), zero parameters; arity 0.
    let src = "let (|Parse|_|) s = if s = \"\" then None else Some s\n\
               let f x = match x with Parse v -> v | _ -> \"\"\n";
    assert_eq!(
        shape_of_case_use(src, "Parse", 1),
        ActivePatternShape {
            total: false,
            single_case: true,
            arity: Some(0),
        }
    );
}

#[test]
fn shape_partial_single_case_two_arity() {
    // `(|P|_|) a b n` — partial, single-case, three curried args, two
    // parameters (`a`, `b`); arity 2.
    let src = "let (|P|_|) a b n = if n > a then Some b else None\n\
               let g n = match n with P 1 2 v -> v | _ -> 0\n";
    assert_eq!(
        shape_of_case_use(src, "P", 1),
        ActivePatternShape {
            total: false,
            single_case: true,
            arity: Some(2),
        }
    );
}

#[test]
fn shape_point_free_has_no_arity() {
    // A *point-free* (bare-name) recognizer `let (|Nil|Cons|) = f` carries no
    // syntactic parameter count (the head is a `Pat::Named`, no curried args),
    // so `arity` is `None` — not `Some(0)`. Totality / case count are still
    // read from the name: total, multi-case.
    let src = "let f = id\n\
               let (|Nil|Cons|) = f\n\
               let test x = match x with Nil -> 0 | Cons -> 1\n";
    assert_eq!(
        shape_of_case_use(src, "Nil", 1),
        ActivePatternShape {
            total: true,
            single_case: false,
            arity: None,
        }
    );
}

#[test]
fn shape_local_function_form_recognizer() {
    // A recognizer defined *inside a function* goes through the other call site
    // (`resolve_local_let`, not `prepare_binding`), which must thread arity the
    // same way. Local `(|DivBy|_|) d n` — partial, single, arity 1.
    let src = "let outer x =\n    \
               let (|DivBy|_|) d n = if n % d = 0 then Some() else None\n    \
               match x with DivBy 3 -> 1 | _ -> 0\n";
    assert_eq!(
        shape_of_case_use(src, "DivBy", 1),
        ActivePatternShape {
            total: false,
            single_case: true,
            arity: Some(1),
        }
    );
}

#[test]
fn shape_local_point_free_recognizer_has_no_arity() {
    // The local call site's bare-name (point-free) branch also passes `None`
    // arity, not `Some(0)`. Local `let (|Nil|Cons|) = id` — total, multi, None.
    let src = "let outer x =\n    \
               let (|Nil|Cons|) = id\n    \
               match x with Nil -> 0 | Cons -> 1\n";
    assert_eq!(
        shape_of_case_use(src, "Nil", 1),
        ActivePatternShape {
            total: true,
            single_case: false,
            arity: None,
        }
    );
}

#[test]
fn value_use_inside_quote_pattern_resolves_to_enclosing_binder() {
    // A quotation `<@ … @>` in *pattern* position (a parameterised active-pattern
    // argument, `SynPat.QuoteExpr`) captures the enclosing scope, exactly like an
    // expression-position quote. The `q` inside `<@ q @>` must resolve to the
    // enclosing local `let q`, not be silently dropped (which would break
    // go-to-def inside these patterns). The clause binder `y` is *not* in scope
    // for the parameter expression, so this resolves against the enclosing frame.
    // `q` is a *local* binding, so the resolution is `Local` (a top-level `let q`
    // would be a module `Item`); this pins the scope capture precisely.
    // "q": the local `let q` def (0), the `<@ q @>` use (1).
    let src = "let (|Foo|_|) v x = if x = v then Some () else None\n\
               let test y =\n    \
               let q = 1\n    \
               match y with\n    \
               | Foo <@ q @> () -> 1\n    \
               | _ -> 0\n";
    let def = nth(src, "q", 0);
    assert_resolves_to(src, "q", 1, def, DefKind::Value { is_function: false });
}
