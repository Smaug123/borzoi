# R2 annotation typing plan — sema Stage 3.2b-3 and its extensions

> **Status: all stages landed** (2026-07-08). R2 types an annotated binder from
> its annotation (`let x : int64 = e` ⇒ `x : System.Int64`), flowing to hover
> and to `x`'s uses, via a primitive-alias table applied **only when the
> resolver proves no shadow of the alias is possible** — the R1
> `Deferred(ShadowableType)` "no record ⇒ no shadow possible" signal. The gate:
> apply the alias iff `resolution_at(head_token_range) == None` on a
> single-segment, bare (non-`App`) `Type::LongIdent` head. Superseded by the
> "Landed stages" list below; see
> [`sema-phase3-impl-plan.md`](../sema-phase3-impl-plan.md) for surrounding phases.

## Landed stages (one line each)

- **R2-0** (#848–#852) — resolver hardening: closed the three R1-invariant
  violations so "no record" regains its meaning — V1 (`[<AutoOpen>]`-module
  nested types), V2 (`module rec` / `namespace rec` forward declarations), V3
  (referenced F# assemblies' metadata-invisible type abbreviations). The V3 fix
  landed as pickle-derived name-only `EntityKind::Abbreviation` marker entities
  (`apply_abbreviation_markers` in `crates/assembly/src/fsharp_pickle_merge.rs`,
  public/non-measure/non-FSharp.Core), with `AbbreviationVisibility::Unknowable`
  / `AssemblyProjectionSkips::fsharp_abbreviations_unknowable` as the coarse
  per-namespace fallback for pickles that fail to decode.
- **R2-a** (#853) — the alias table + annotated value binders:
  `fsharp_primitive_alias` (17 arms + source synonyms, `infer.rs`), `Gen::annotation_ty`
  (bare single-segment `Type::LongIdent` gated on the R1 signal, structurally
  recursing `Fun`/non-struct-tuple/`Array`), and `bind_annotated_named`
  replacing the blanket `let_binding` skip.
- **R2-b** (#855) — annotated parameters: `Gen::param_var` gains a `Pat::Typed`
  arm; a table-annotated parameter grounds its slot and curries into the
  function type.
- **R2-c** (#858) — function return-type annotations (`annotated_function_binding`):
  the return slot is grounded by the annotation; the body is check-walked and
  the dropped body↔annotation subsumption suspended as an `ArgCheck`, discharging
  as equality only for a no-subsumption annotation on a walk-complete binding.
- **R2-d** (#859) — entity-backed annotations (`entity_annotation_ty`): a
  concrete `Resolution::Entity` at the head (single-segment) or tail
  (multi-segment, e.g. qualified `System.Int64`) bridges to `Ty::Named`, under
  the `member_ty.rs` non-generic/non-nested conventions.
- **R2-e** (#863) — RHS check-walk under annotated bindings: the RHS is walked
  in check mode against the annotation variable for coverage (member accesses
  wake and record) with no RHS-node emission; the binder's ground type is
  unaffected by check-mode poison.

## Deliberately deferred (not implemented)

Documented v1 narrowings against the original §5 out-of-scope list, each with
its trigger, should the corpus ever demand them:

- R2-b's check-mode-else mixed shape
  (`let f (b: bool) x = if b then (1, x) else (2, x)`) defers — the else branch
  poisons `x`; the synth-position variant emits the full scheme.
- R2-d's project-defined-type half (`Local`/`Item` heads) stays deferred — its
  canonical `<Project>.M.T` rendering against the oracle is unprobed.
- Numeric-literal defaulting (`let x : int64 = 42` typing the literal node);
  expression ascriptions `(e : T)`; `let rec` / mutable / `let!`/`use!` typed
  binders; measure-carrying types; generic instantiations (pending `Ty` args);
  attribute/object-model/augmentation type positions.
