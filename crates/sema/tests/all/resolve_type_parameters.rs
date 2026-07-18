//! Direct (FCS-free) tests for **type-parameter** binding: a generic
//! parameter declared by a `type` / `let` / `member` header (`<'T>`) is
//! interned as a binder, its declaring occurrence resolves to itself, and a
//! same-scope `'T` *use* in a type position (or a `'T.Member` expression)
//! resolves to that binder.
//!
//! These assert the resolver's own output directly (no oracle), so they run
//! fast and pin the exact ranges. The FCS differential over generic snippets
//! lives in `resolve_diff.rs` (the strict corpus); the whole-corpus coverage
//! ratchet is `resolve_corpus_diff.rs`.
//!
//! Type parameters live in F#'s *type* namespace, disjoint from values, and are
//! **definition-scoped** (a member's `<'T>` is visible only inside that member),
//! so the resolver models them as a stack of typar frames separate from both the
//! value scope stack and the container-keyed `type_defs` map.

use borzoi_cst::parser::parse;
use borzoi_cst::syntax::{AstNode, ImplFile};
use borzoi_sema::{AssemblyEnv, ProjectItems, Resolution, ResolvedFile, resolve_file};
use rowan::TextRange;

fn resolve(src: &str) -> ResolvedFile {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "snippet has parse errors (out of subset?): {src:?}: {:?}",
        parsed.errors
    );
    let file = ImplFile::cast(parsed.root).expect("impl file");
    resolve_file(&file, &ProjectItems::default(), &AssemblyEnv::default())
}

/// The byte range of the `n`th (0-based) occurrence of `needle` in `src`.
fn nth(src: &str, needle: &str, n: usize) -> TextRange {
    let mut from = 0;
    let mut found = None;
    for _ in 0..=n {
        let i = src[from..].find(needle).expect("occurrence") + from;
        found = Some(i);
        from = i + needle.len();
    }
    let i = found.unwrap();
    TextRange::new(
        u32::try_from(i).unwrap().into(),
        u32::try_from(i + needle.len()).unwrap().into(),
    )
}

/// Assert the `use_idx`-th occurrence of `needle` resolves to a local binder
/// whose range is the `decl_idx`-th occurrence of `needle` (its declaration).
/// `needle` is the whole typar including the sigil (`"'T"`), matching FCS's
/// apostrophe-inclusive range.
fn assert_typar_use(src: &str, needle: &str, use_idx: usize, decl_idx: usize) {
    let rf = resolve(src);
    let use_range = nth(src, needle, use_idx);
    let res = rf
        .resolution_at(use_range)
        .unwrap_or_else(|| panic!("no resolution at {needle:?} use #{use_idx} in {src:?}"));
    assert!(
        matches!(res, Resolution::Local(_)),
        "expected a Local typar resolution for {needle:?} use #{use_idx} in {src:?}, got {res:?}"
    );
    let def = rf
        .resolved_def(res)
        .expect("a Local resolution names an in-file def");
    assert_eq!(
        def.range,
        nth(src, needle, decl_idx),
        "{needle:?} use #{use_idx} points at the wrong decl in {src:?}"
    );
}

/// Assert we record *nothing* at the `use_idx`-th occurrence of `needle` — the
/// sound-deferral boundary (a typar with no explicit declaration in scope).
fn assert_typar_unrecorded(src: &str, needle: &str, use_idx: usize) {
    let rf = resolve(src);
    let use_range = nth(src, needle, use_idx);
    let res = rf.resolution_at(use_range);
    assert!(
        !matches!(res, Some(Resolution::Local(_)) | Some(Resolution::Item(_))),
        "expected no in-file binder for {needle:?} use #{use_idx} in {src:?}, got {res:?}"
    );
}

// ---- let / function headers -------------------------------------------------

#[test]
fn let_typar_annotation_use_resolves_to_decl() {
    // `<'T>` is decl #0; the annotation `(x: 'T)` is use #1.
    assert_typar_use("let f<'T> (x: 'T) = x\n", "'T", 1, 0);
}

#[test]
fn let_typar_decl_occurrence_self_resolves() {
    // FCS reports the `<'T>` decl occurrence itself as a resolvable use
    // (IsFromDefinition=False) pointing at itself.
    assert_typar_use("let f<'T> (x: 'T) = x\n", "'T", 0, 0);
}

#[test]
fn let_typar_return_annotation_resolves() {
    // return-type annotation position: `<'T>` #0, `: 'T =` use #1.
    assert_typar_use("let f<'T> (x: 'T) : 'T = x\n", "'T", 2, 0);
}

// ---- type-definition headers ------------------------------------------------

#[test]
fn abbrev_rhs_typar_resolves_to_type_header() {
    // `type Foo<'T> = 'T -> 'T`: decl #0, both RHS uses (#1, #2) resolve to it.
    assert_typar_use("type Foo<'T> = 'T -> 'T\n", "'T", 1, 0);
    assert_typar_use("type Foo<'T> = 'T -> 'T\n", "'T", 2, 0);
}

#[test]
fn record_field_typar_resolves_to_type_header() {
    let src = "type Box<'T> = { Value: 'T }\n";
    assert_typar_use(src, "'T", 1, 0);
}

#[test]
fn union_case_field_typar_resolves_to_type_header() {
    let src = "type Opt<'T> = Nothing | Just of 'T\n";
    assert_typar_use(src, "'T", 1, 0);
}

// ---- member bodies & signatures ---------------------------------------------

#[test]
fn member_sees_enclosing_type_typar() {
    // The type header's `<'T>` is visible in a member signature.
    let src = "type C<'T>() =\n    member _.M(x: 'T) = x\n";
    assert_typar_use(src, "'T", 1, 0);
}

#[test]
fn member_own_typar_resolves_to_member_header() {
    // A member declares its own `<'T>`; the sig use resolves to the member decl.
    let src = "type C() =\n    member _.M<'T>(x: 'T) = x\n";
    assert_typar_use(src, "'T", 1, 0);
}

#[test]
fn member_typar_colliding_with_type_typar_defers() {
    // Both the type and the member declare `'T`. FCS binds the annotation use to
    // the *enclosing type*'s `'T` (verified against the oracle) — the member
    // re-declaration does not win. That is the opposite of the nested-`let` case
    // below, so the shadow rule is context-dependent; rather than model it we
    // defer the colliding use (sound — never a wrong binder). Each *declaration*
    // still self-resolves. Occurrences: #0 type header, #1 member header, #2 use.
    let src = "type C<'T>() =\n    member _.M<'T>(x: 'T) = x\n";
    assert_typar_use(src, "'T", 0, 0); // type-header decl self-resolves
    assert_typar_use(src, "'T", 1, 1); // member-header decl self-resolves
    assert_typar_unrecorded(src, "'T", 2); // colliding annotation use defers
}

// ---- nested generic `let` (local-let typar frames) --------------------------

#[test]
fn local_let_typar_resolves() {
    // A block-level generic `let` pushes its own typar frame: the annotation `'T`
    // resolves to the local header's decl. Occurrences: #0 header, #1 annotation.
    let src = "let outer () =\n    let g<'T> (x: 'T) = x\n    g\n";
    assert_typar_use(src, "'T", 1, 0);
}

#[test]
fn nested_generic_let_distinct_typar_resolves() {
    // Distinct names nest cleanly: the inner `'U` resolves to the inner header
    // (a single declaring frame) and does not leak to the outer `'T`.
    let src = "let outer<'T> () =\n    let inner<'U> (y: 'U) = y\n    inner\n";
    assert_typar_use(src, "'U", 1, 0);
}

#[test]
fn nested_generic_let_colliding_typar_defers() {
    // `let inner<'T>` inside `let outer<'T>`: both declare `'T`. FCS binds the
    // inner annotation to the *inner* `'T` (opposite of the member/type case), so
    // we defer the collision. The inner decl still self-resolves; without the
    // local-`let` frame the outer `'T` would silently (and wrongly) bind the use.
    // Occurrences: #0 outer header, #1 inner header, #2 inner-sig use.
    let src = "let outer<'T> () =\n    let inner<'T> (y: 'T) = y\n    inner\n";
    assert_typar_use(src, "'T", 1, 1); // inner-header decl self-resolves
    assert_typar_unrecorded(src, "'T", 2); // colliding inner use defers
}

// ---- range precision (leading-trivia regression) ----------------------------

#[test]
fn typar_decl_range_excludes_leading_whitespace() {
    // A space inside `<…>` before the typar (`type Box< 'T>`) attaches to the
    // `TYPAR_DECL` node as leading trivia. The binder's range must still be the
    // sigil-inclusive `'T`, not ` 'T` — else it sits one byte before FCS's span
    // and reads as a spurious divergence (the WellKnownAttribs.fs `'TFlags`
    // regression). `nth("'T", 0)` is the 2-byte `'T`, excluding the space, so a
    // node-range binder would fail this.
    assert_typar_use("type Box< 'T> = 'T -> 'T\n", "'T", 1, 0);
}

// ---- sound-deferral boundary ------------------------------------------------

#[test]
fn implicitly_generalized_typar_is_unrecorded() {
    // No explicit `<'a>` header: implicit generalization is a later slice, so
    // the `'a` annotation must defer (record nothing) — never wrong-bind.
    assert_typar_unrecorded("let id (x: 'a) = x\n", "'a", 0);
}
