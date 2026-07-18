# Plan: type-parameter binding in name resolution

> **Status:** Slice 1 (type / `let` / `member` header typars) **delivered**.
> Implicit generalisation, `when` constraints, local-`let` typars, and `.fsi`
> `val` typars remain — see "Still to do". This slice completes the "member
> generic type parameters" tail noted in `docs/member-body-resolution-plan.md`.

## Why

The resolver interned a `type` name as a `DefKind::Type` binder and descended
into member bodies, but a **type parameter** (`'T` / `^T`) was never entered as a
binder. `resolve_type`'s `Type::Var` arm and `resolve_expr`'s `Expr::Typar` arm
were documented no-ops, and `typar_decls()` was consulted only as a boolean at
`resolve.rs`. So every `'T` use — in an abbreviation RHS, a field type, a member
signature, a member body, a binding annotation — deferred.

A categorised sweep of the resolution differential (the `resolve_divergence`
report over the FCS corpus, 2026-07-18) measured the cost: **`type-parameter`
was the second-largest in-file no-inference (B1) gap — 776 of 3306 deferred B1
uses**, of which 711 were genuine hand-written parameters (`'T` ×250, `'a` ×188,
`'Char`, `'TFlags`, …) and only 65 the generated `'gentype_*` of `fslexpars.fs`.
It sat behind only `value:local-or-param`.

The deferral was deliberate and sound (uses declined, never wrong-bound). This
slice makes them resolve.

## What FCS does (the ground truth this slice reproduces)

Read off `fcs-dump uses` over `let f<'T> (x: 'T) = x`:

- The declaring `<'T>` occurrence is itself reported as a **use**
  (`IsFromDefinition = false`) whose declaration is itself.
- Each `'T` use (the `(x: 'T)` annotation, an RHS/body occurrence) reports its
  declaration as the `<'T>` span.
- Every typar range is **apostrophe-inclusive** (`'T` = 2 cols): FCS keys typars
  on the sigil + name, not the bare name.

So the resolver must: intern each declared typar as a binder ranged over the
sigil-inclusive `'T`, self-record the declaring occurrence, and resolve every
use to it.

## Design: a separate typar namespace stack

Type parameters live in F#'s **type** namespace (disjoint from values) and are
**definition-scoped** (a member's `<'T>` is visible only inside that member) —
unlike the container-keyed `type_defs` map, which is visible container-wide. So
they are modelled as a stack of typar frames, `Resolver::typar_scopes:
Vec<Vec<(String, DefId)>>`, mirroring the value `scopes` stack:

- `DefKind::TypeParam` / `SemanticClass::TypeParameter` — the new binder kind
  (the LSP maps it to the standard `typeParameter` semantic-token type and hovers
  it as "type parameter"; it is not a document-outline symbol).
- `intern_typars(&TyparDecls) -> Vec<(String, DefId)>` — interns each typar as a
  `TypeParam` binder and self-records its declaring occurrence; the mirror of
  `define_type`. Returns the frame so a two-phase caller can re-activate it
  without re-interning.
- `enter_typars(Option<TyparDecls>) -> bool` / `leave_typars(bool)` — push/pop a
  frame (nothing for a non-generic header).
- `lookup_typar(name)` — binds only when **exactly one** open frame declares the
  name. A same-name collision across frames **defers**: F#'s shadow rule is
  context-dependent (a `member _.M<'T>` inside `type C<'T>` binds the *enclosing
  type*'s `'T`; a nested `let inner<'T>` inside `let outer<'T>` binds the *inner*
  — both verified against FCS), and modelling that split is a later slice, so a
  collision defers rather than risk a wrong bind (D5).
- `typar_occurrence(node)` — the sigil-start-to-name-end range and display text
  of a `'T`/`^T` occurrence, **derived from the tokens** so leading whitespace
  trivia the `TYPAR_DECL` node can span (`type X< 'T>`) does not shift the binder
  one byte off FCS's span.

`Type::Var` and `Expr::Typar` resolve their name against `lookup_typar` and
record at `typar_occurrence`'s range; a miss stays unrecorded (a sound deferral).

### Where the frames are pushed

- **Type headers** (`type Foo<'T>`): pushed around `resolve_type_defn` +
  `resolve_type_member_bodies`, so the abbreviation/record/union RHS and every
  member signature and body see the type's parameters.
- **Member headers** (`member _.M<'T>`): pushed inside `resolve_member_body`,
  nested above the type's, around the member's param annotations, return type,
  and body.
- **`let`/function headers** (`let f<'T>`): interned once in `prepare_binding`
  and activated around the head annotations there, then re-activated in
  `resolve_rhss` around the RHS (the two-phase binding split).
- **local `let` / class-`let` headers**: the same two-phase split in
  `prepare_local_bindings` / `resolve_local_let_rhss` (shared by block-`let` and
  a type's class-level `let` fields). Pushing these frames is what makes a nested
  generic `let` *shadow* an enclosing typar to a collision instead of silently
  leaking the outer binder into its body — the soundness fix for the nested case.

## Testing

- **FCS-free** (`resolve_type_parameters.rs`): decl self-resolution, annotation /
  RHS / return-type / member-signature uses, member-shadows-type, the
  leading-whitespace range regression, and the implicit-generalisation
  sound-deferral boundary.
- **Differential** (`resolve_diff.rs` strict corpus): generic function, abbrev,
  return-annotation, and generic-union snippets — FCS asserts exact binder
  ranges.
- **Classifier** (`classify_diff.rs`): a typar snippet in the corpus, plus the
  `TypeParameter` case in the "commits each in-file category" coverage test.
- **Gate** (`resolve_corpus_diff.rs`): 22058 matches / **0 divergences** / 230
  alt-binders, B1 in-file coverage 882‰ (up from 867‰). The `MIN_*` floors were
  ratcheted to lock in the gain.

## Still to do

- **Implicit generalisation** — `let id (x: 'a) = x` with no explicit `<'a>`:
  FCS synthesises the parameter at first use. Currently deferred (sound).
- **`when` constraints** — the subject/constraint typars of `<'T when 'T : …>`.
- **Same-name shadow disambiguation** — a typar name declared in two open scopes
  currently *defers* (see `lookup_typar`). Modelling FCS's context-dependent rule
  (member re-declaration → enclosing type; nested `let` → inner) would recover
  those uses.
- **Signature files** — `ValSig::typar_decls()`; `.fsi` resolution is a later
  slice overall.
- **Get/set & auto-property member typars** — rare; only the `Member` method arm
  is wired.
