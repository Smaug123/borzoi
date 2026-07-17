# Plan: name resolution inside type-member bodies

> **Status:** Slice 1 (genuine-definition member bodies) **delivered**. The
> augmentation-body slice and a few small tails (`inherit` ctor args, member
> generic type parameters) remain — see "Still to do".

## Why

Sema indexed member *names* (so `Type.Member` resolves from *outside* a type —
`docs/project-type-member-plan.md`) but never descended **into** member bodies.
`resolve_type_defn` resolved the type-use side of a definition (field types,
abbreviation targets) and left the `ObjectModel` repr a no-op; `add_type_members`
filed member names for the qualified-static emit. Neither walked a `member` /
`new` / property body. So every local, parameter, and self-identifier *use*
inside a type member deferred.

A categorised sweep of the resolution differential (the `resolve_divergence`
report over the FCS corpus) measured the cost: **90.8% of the entire in-file
no-inference (B1) gap — 11 638 of 12 811 deferred B1 uses — lay inside a
type-member body.** By sub-tag, 98.8% of `value:local-or-param`, 88% of
`value:module-or-import`, 91% of in-member `union-case`, and 85% of in-member
`entity:type` uses. This one missing walk was the single largest item on the
name-resolution worklist; closing it lifts B1 in-file coverage from ~48.9%
toward ~95%.

The deferral was deliberate and sound (`exprs.rs`'s `Expr::ObjExpr` comment:
member bodies "are left for the dedicated member-resolution slice … under-
resolution here is sound"). This is that slice.

## What FCS does (the ground truth this slice reproduces)

Read off `fcs-dump uses` over a representative class:

```fsharp
type T(seed: int) =                 // primary-ctor param `seed`
    let mutable acc = seed          // class field `acc`; its RHS sees `seed`
    member x.Add(n: int) : int =    // self-id `x`, param `n`
        acc + n + seed              // acc→field, n→param, seed→ctor-param
    static member Make(v) = T(v)    // no self-id; `v`→param
    member x.Prop with get () = acc and set v = acc <- v
    new(a, b) = T(a + b)            // secondary ctor: `a`,`b`→params
```

Scoping, innermost last. Crucially, F# splits the field scope into **static**
and **instance** halves — a static context sees only the static half:

1. **Static-`let` fields** (`static let s`) — the **static scope**, visible from
   *every* body (static and instance).
2. **Instance scope** — primary-ctor params (`seed`), `as self`, and non-static
   `let`/`do` fields (`acc`) — visible **only** from an *instance* body. A
   `static member` / `static let` / static auto-property initialiser / secondary
   `new` runs before/without an instance, so these are **not** in scope there;
   FCS binds a same-named outer value instead (`let x = 0; type T(x) = static
   member S = x` is `M.x`, not the ctor param — verified with `fcs-dump uses`).
3. **Self-identifier** (`x` / `this`) — per *instance* member; its declaration
   range is the self-id token; a bare use (`= this`) resolves to it. `member _.M`
   binds nothing; static members / constructors have none.
4. **Member / accessor / secondary-ctor params** — scoped to that body only.

A field RHS sees the fields (of matching-or-wider staticness) declared before it,
in source order. Module values (`outer`) and opened names remain visible in a
member body (it resolves against the enclosing module scope too), and return-type
/ param-type annotations resolve as ordinary type uses.

## Design

`resolve_type_member_bodies(defn)` (in `resolve/types.rs`), called from the
`ModuleDecl::Types` walk right after `resolve_type_defn(defn)`. It reuses the
existing scope machinery wholesale — no new lookup path. Two owned accumulators,
`static_entries` and `instance_entries`, are built as the class-level
`MemberDefn::LetBindings` / `Do` are processed in source order (`resolve_class_let`
routes each group's binders to the accumulator of its staticness, sharing the new
`prepare_local_bindings` core factored out of `resolve_local_let`, so `let rec`
and active-pattern fields behave identically to block `let`s). Then each body is
resolved under `push_field_scope(is_static, …)` — which pushes the static frame
always and the instance frame only for an instance context — plus a per-member
frame (`resolve_member_body`) holding the self-id (`bind_self_ident`) +
`LongIdentPat::args` (or each `GetSetAccessor::args`), then `resolve_expr`.

Member shapes: `Member` (instance methods, static members, **and** secondary
`new` — a constructor / static member binds no self-id and resolves in the static
scope); `GetSetMember` (one shared self-id on an instance property, per-accessor
params + body, static scope when `static`); `AutoProperty` (initialiser only —
instance scope unless `static`, no self-id/params); `Interface` (recurse into
`with member …`, always instance). `AbstractSlot` / `ValField` / `MemberSig` /
`Inherit` carry no resolvable body.

## Soundness (D5: defer, never wrong-resolve)

The new hazard is scoping. Every body use is resolved against a scope that
*first* binds the self-id, params, ctor params, and (correct-staticness) fields,
so a param can never mis-resolve to an outer same-named binder (the exact failure
the old blanket deferral avoided). All resolution below the frame push reuses
`resolve_expr` / `pattern_locals`, already differentially sound.

The **static/instance split is a soundness requirement, not a nicety** (GPT-5.6
review): leaving the instance scope active for a static body would commit a
`static member` / secondary-`new` reference to a colliding ctor param where FCS
binds the outer value. Pinned by `static_member_body_binds_module_value_not_ctor_param`,
`secondary_ctor_body_binds_module_value_not_ctor_param`,
`static_auto_property_init_binds_module_value_not_ctor_param`, and
`static_member_sees_static_let_but_not_instance_let`.

**Augmentations are excluded.** A `type T with member …` augmentation does not
carry the original type's ctor-param / private-field scope (it may even be
cross-file), so walking its body blind could bind a same-named *module* value
where FCS binds the type's private field — a divergence. Augmentation bodies
defer wholesale (sound under-resolution); the `is_type_augmentation` gate at the
top of `resolve_type_member_bodies` enforces it, and
`augmentation_member_body_does_not_wrongly_bind_module_value` pins it.

## Tests

- **Hermetic, precise-range** (`tests/all/resolve_member_bodies.rs`): each
  construct (ctor param, class field, self-id, static member, secondary ctor,
  get/set, cross-member field visibility, module-value visibility) plus two
  soundness pins (param shadows module value; augmentation body defers). Target
  ranges read off FCS.
- **Enumerated collision-matrix differential**
  (`resolve_diff.rs::member_scope_collisions_never_wrong_resolve`): sweeps every
  combination of a colliding `x` (module value / ctor param / `static let` /
  instance `let`) against an instance-member / static-member / secondary-`new`
  reference and asserts D5 vs FCS — match-or-defer, never a *different* binder.
  This is the **systematic guard** for the static/instance + source-order
  scoping: both were fixed only after review found wrong-binds by hand, so the
  matrix now checks them mechanically.
- **Corpus differential** (`resolve_corpus_diff.rs`): the sweep's
  `MAX_RESOLUTION_DIVERGENCES = 0` gate is the whole-corpus soundness guard,
  `MIN_RESOLUTION_MATCHES` / `MIN_B1_COVERAGE_PERMILLE` the completeness ratchets
  (re-baselined up by this slice).

## Still to do

- **Augmentation bodies** — needs the original type's field/ctor scope threaded
  to the augmentation site (same-file first; cross-file is harder).
- **`inherit Base(args)`** — the base-constructor argument expression is not yet
  walked (a small `value:*`/`constructor` tail).
- **Member generic type parameters** (`member x.M<'T>`) — `'T` uses in the body
  are not bound (the `type-parameter` bucket, largely orthogonal to this slice).
- **Constructor-application resolution** (`T(v)` binding `T` to the type) is a
  separate feature (`docs/project-type-member-plan.md`), not this slice. Note a
  *pre-existing* consequence a member body now shares: when a construction-capable
  type collides with a same-named value (`let T x = x; type T(x) = …`), a bare
  applied head `T(v)` resolves through the value scope to the module value, where
  FCS binds the type constructor. This is a resolver-wide `resolve_expr` /
  head-slot limitation — module-level `let y = T 5` mis-resolves *identically*,
  independent of this slice — so walking member bodies inherits it (a `0`-corpus
  D5 exception, since no real project co-names a value and a type). Closing it is
  the constructor-resolution / head-slot work, not a member-body change (GPT-5.6
  review round 3).
- **Secondary-constructor `as this` self-id** — `new(z) as this = T(z) then
  this.P <- …` binds `this` on the member itself (a sibling identifier, not in the
  `LongIdent` head), which this slice does not yet bind (a `new` is treated as
  self-less). The `then`-block `this` therefore defers — sound unless a
  pathological outer `let this` is in scope (round 3, P2). Bind the optional
  `as`-identifier while keeping the constructor's static field scope.
- **Generative member-body differential** — the whole-corpus sweep is the
  soundness guard, but it only catches a hazard the corpus happens to contain:
  the static/instance-collision bug (a `static member` referencing a name a
  same-named ctor param would shadow) slipped past a `0`-divergence corpus run
  because no corpus file exercised it, and was found by review instead. The
  systematic fix is to extend `common::generator`
  (`resolution_agrees_with_fcs_on_generated_programs`) to emit types with members
  — static/instance methods and properties, ctor params, class `let`s — over a
  small colliding-name alphabet, so FCS adjudicates the scoping combinatorics
  this slice introduces rather than relying on corpus incidence.
