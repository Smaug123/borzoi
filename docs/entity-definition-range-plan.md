# Go-to-definition on referenced entities via the pickled `entity_range`

> **Status:** planned, not started. Follow-up to PR #157 (go-to-definition on
> referenced module *values* via the pickled val `DefinitionRange`), which
> built the machinery this plan reuses: `FsharpSourceRange`,
> `range_definition_source_in_pdb`, `definition_document_for_range`.
>
> **Grounded in a pickle probe (2026-07-18)** that walked FSharp.Core's
> signature pickle and dumped every entity's `entity_range` — the three
> design-shaping questions below (what the range spans, which file an
> `.fsi`-constrained assembly records, whether degenerate ranges occur) are
> probe answers, not conjecture.
>
> **Revised over four GPT-5.6 draft-review rounds**, every finding verified
> against the code before adoption. The load-bearing corrections: the range
> overlay declines arity-ambiguous FQNs (`find_entity_mut` matches by name
> only, and `type A` / `type A<'T>` both project to `A`), with each
> direction of the guard pinned by its own test; measure leaves pickle as
> `IsType::Namespace` and need the measure overlay's collection predicate —
> contributing range-only targets so a `None` source-name can never clear a
> real one; *type*-abbreviation markers are unreachable from
> `Resolution::Entity` (sema defers them by design) and move to non-goals,
> but **exception**-abbreviation markers are `EntityKind::Exception`, which
> the defer predicate does not match — reachable, so §3 stamps marker
> synthesis; and two test-target corrections (`UnitSymbols.m` is an erased
> abbreviation with no `Entity` — use `UnitNames.metre`; generic F# entities
> sink the MiniLibFs differential — arity twins get a non-differential
> fixture).

## The problem

Go-to-definition on a referenced-assembly **entity** (type or module)
navigates via PDB sequence points: `assembly_entity_location`
(crates/lsp/src/handlers/definition.rs) sweeps the entity's physical
`method_def_tokens` for the lowest-rid method with a sequence point. Entities
whose methods carry no sequence points — or that have no methods at all — find
nothing:

- **a module whose members are all values** (`RqaModule` in the MiniLibFs
  fixture; the motivating real-world case): every member is a property getter
  reading a backing field, and PR #157 established empirically that such
  getters never carry sequence points (0 of FSharp.Core's 747 module values
  have one);
- **measure types** (`[<Measure>] type m`): no methods at all;
- **enums**: no methods at all;
- **interfaces**: method rows without bodies, hence no sequence points;
- **exception-abbreviation markers** (`exception Alias = Original`):
  synthesised from the pickle with no ECMA row and an empty
  `method_def_tokens`, yet — unlike *type*-abbreviation markers — reachable
  as a `Resolution::Entity` (sema's defer predicate matches only
  `EntityKind::Abbreviation`, and these are `EntityKind::Exception`);
- **type-abbreviation markers** (`type IntId = int`): also structurally
  unnavigable, but *deliberately out of scope here*: sema defers any lookup
  that lands on one rather than resolving it, so there is no
  `Resolution::Entity` to navigate from (see non-goals).

Values got fixed in PR #157 by falling back to the F# signature pickle's
per-val source range. Entities have the exact analogue sitting unread:
`PickledEntity.range` is already decoded by the pickle walker
(`u_entity_spec_data` reads it at crates/assembly/src/fsharp_pickle/entity.rs;
the field is on the model at crates/assembly/src/fsharp_pickle/model.rs) and
then never consulted.

## Probe findings

Walking FSharp.Core 10's pickle (420 entities):

```
Operators    @ …\FSharp.Core\prim-types.fsi:2766:11-2766:20  (width 9)
ListModule   @ …\FSharp.Core\list.fsi:16:7-16:11             (width 4 = "List")
List (type)  @ …\FSharp.Core\prim-types.fsi:2733:39-2733:43  (width 4)
UnitSymbols  @ …\FSharp.Core\SI.fs:117:47-117:58
m (measure)  @ …\FSharp.Core\SI.fs:124:5-124:6
extension histogram: {"fs": 75, "fsi": 344, "unknown": 1}
```

1. **The range spans exactly the source binder identifier**, same convention
   as vals (1-based lines, 0-based columns). `ListModule`'s range covers the
   four columns of the *source* name `List`, not the IL name — renamed and
   suffixed modules navigate to the right identifier for free.
2. **An `.fsi`-constrained assembly records the `.fsi` position.** Unlike
   vals — where FCS pickles the `(sig_range, DefinitionRange)` *pair* via
   `p_ranges` — `p_entity_spec_data` pickles a single `p_range x.entity_range`
   (FCS `TypedTreePickle.fs:2798`). `entity_other_range` lives on the
   unpickled `entity_opt_data` and never crosses the assembly boundary, so
   FCS itself has nothing better cross-assembly: landing on the `.fsi` is
   full fidelity with the compiler, not a shortcut. The `.fsi` never appears
   in the PDB Document table, but PR #157's range leg deliberately does not
   require a Document row — SourceLink prefix-mapping works on the path
   string alone.
3. **Exactly one degenerate range exists: the synthetic root CCU entity**
   (`"unknown"`, 1:0–1:0). The entity walk's root-name suppression already
   keeps it away from any ECMA match, but the stamping step still declines a
   range whose file resolves to `"unknown"` — one line of belt-and-braces
   that keeps a hypothetical degenerate range from becoming a bogus
   navigation target (D5: say nothing rather than guess).
4. **Namespace entities carry arbitrary ranges** (whichever contributing file
   pickled first — `Microsoft` points into `fslib-extra-pervasives.fsi`).
   Harmless: namespaces have no ECMA TypeDef row, so the FQN-keyed overlay
   can never stamp one.

## Current state (survey)

- **Merge side.** `apply_source_name_overlay`
  (crates/assembly/src/fsharp_pickle_merge.rs) already does precisely the
  walk this plan needs: `walk_entity_tree` over the host CCU's pickle,
  collecting per-entity facts into `EntityOverlayTarget`, then FQN-matching
  each onto the ECMA tree via `find_entity_mut` (single CCU, exact
  namespace + type-chain match; a miss under-sets, never mis-sets). It
  currently carries one fact: `source_name`.
- **Abbreviation markers.** `apply_abbreviation_markers` (same file)
  synthesises name-only `Entity` records for pickled abbreviations with no
  ECMA row, in two kinds with *different reachability*. A type abbreviation
  becomes `EntityKind::Abbreviation`, and sema treats a hit on it as a
  *defer signal* (`AssemblyEnv::is_abbreviation`: "a lookup that lands on it
  must defer rather than resolve") — no such handle ever reaches
  `Resolution::Entity`. An **exception** abbreviation becomes
  `EntityKind::Exception`, which that predicate does *not* match: sema
  resolves it like any exception entity, so the LSP's entity-navigation
  entry points are reachable for it — and today find nothing, since the
  marker has no metadata row and no method tokens.
- **Model.** `Entity` (crates/assembly/src/model.rs) has no source-position
  field. `MethodLike` gained `definition_range: Option<Box<FsharpSourceRange>>`
  in PR #157 (boxed only for clippy's `large_enum_variant` on `Member`;
  `Entity` is a plain struct, so no box needed here).
- **LSP side.** Two entity consumers, both sequence-point-only:
  - `assembly_entity_location` (definition.rs) — the `Location` for
    textDocument/definition; returns `None` when `method_def_tokens` is
    empty or no token has a sequence point.
  - `entity_definition_document` (definition.rs) — the "defined in
    \<document\>" line consumed by hover (crates/lsp/src/handlers/hover.rs).
  Both delegate to `entity_definition_source_in_pdb` /
  `entity_definition_document_in_pdb` (crates/lsp/src/goto_source.rs).
  The member-side composition they should mirror already exists:
  `definition_source_with_range_fallback` (token path first, pickled range
  only when it yields nothing) and `member_definition_document`'s
  `from_pdb().or_else(range → definition_document_for_range)` — the latter
  works even when no PDB exists at all.
- **Caching.** `Entity` derives serde for the on-disk projection cache; the
  cache validity tag includes the running binary's mtime, so the new field
  needs no schema bump (verified in PR #157 for the `MethodLike` change).

## Design

### 1. Model: `Entity::definition_range`

```rust
/// Where the entity's source declaration is, per the host CCU's signature
/// pickle (`entity_range`): 1-based lines, 0-based columns, spanning exactly
/// the source binder identifier. For an `.fsi`-constrained assembly this
/// names the `.fsi` — FCS pickles only the single `entity_range`, so the
/// signature position is the full cross-assembly fidelity, not a fallback.
/// `None` for C# assemblies, decode failures, and the degenerate
/// `"unknown"`-file range.
pub definition_range: Option<FsharpSourceRange>,
```

Unboxed (struct field, no enum-variant pressure). Mechanical
`definition_range: None` at every `Entity` literal — `project_entity` in
ecma335_assembly.rs, the abbreviation-marker synthesis, and test builders.

### 2. Merge: stamp via the entity overlay, declining ambiguous FQNs

Extend the source-name overlay's target with the resolved range and stamp it
in the same apply loop. Resolution mirrors PR #157's
`resolve_definition_range`: look the `PickledRange.file` index up in
`header.strings`, decline (`None`) on a bad index or an `"unknown"` file.
Rename the function to reflect that it now applies the entity overlay
generally (e.g. `apply_entity_overlay`).

Two corrections over the naive "ride the existing collector as-is" (both
review findings, both verified in code):

- **Collect measure leaves too.** The source-name collector records only
  `IsType::ModuleOrType | FSharpModuleWithSuffix` entities, and a standalone
  `[<Measure>] type m` pickles with an `IsType::Namespace` `module_type` — it
  is a type-chain *leaf* that is not a type-chain *extender*, which is
  exactly why `walk_entity_tree` reports container-relative positions ("one
  traversal serves all three collectors"). The range collector therefore
  records a target when the entity is a module/type **or** a measure leaf,
  reusing `merge_measure_entities`'s predicate
  (`typar_kind == TyparKind::Measure`, fsharp_pickle_merge.rs:1348); in both
  cases the FQN is `{namespace, type_chain + clr_name(entity)}`.
  Participation is **per fact**: a measure leaf contributes a *range-only*
  target — its `entity_source_name` is `None` (namespace-shaped
  `module_type`), and letting it flow through the source-name apply loop
  could *clear* a legitimately-set `source_name` on a name-colliding row
  (a ``[<CompiledName("A`1")>] type B<'T>`` alongside `[<Measure>] type A`
  both project to the name `A`). Source-name stamping keeps its current
  module/type-only target set.
- **Decline arity-ambiguous FQNs.** `find_entity_mut` matches by name alone,
  and both `type A` and `type A<'T>` project to the ECMA name `A`
  (backtick-arity stripped on both sides — `strip_arity` in
  `project_entity`, `clr_name` on the pickle side). Naively reusing it would
  let one twin's range overwrite the other's, navigating to the wrong
  declaration. The range overlay stamps only when the correspondence is
  unambiguous in *both* directions: at most one collected target per FQN
  key, and at most one ECMA sibling matching the addressed name at each
  chain step. An ambiguous FQN under-sets every involved entity (D5) —
  arity twins keep their sequence-point navigation, which generic types
  essentially always have. (Keying by generic arity instead would stamp the
  twins precisely, but requires settling whether ECMA
  `generic_parameters` on a *nested* type includes the enclosing type's
  typars the way raw IL metadata does — a probe for a follow-up sharpening,
  not this slice. Note the name-only lossiness pre-exists for
  `source_name`, where it is harmless: twins share their source name.)

Otherwise the conservative posture is inherited unchanged: exact FQN match,
single CCU, a missed match under-sets.

### 3. Merge: marker synthesis carries the range

`apply_abbreviation_markers` sets `definition_range` on each marker it
synthesises, resolved from the same `PickledEntity` it is already reading
(`AbbrevMarkerSite` gains the field; same string-table resolution and
degenerate-file decline as §2). Stamping is uniform across both marker
kinds — one code path, no branch — but the consumer that justifies it is the
**exception**-abbreviation marker: `exception Alias = Original` is a
reachable `Resolution::Entity` whose only conceivable source location is
this range, having no metadata row at all. The `EntityKind::Abbreviation`
markers carry the stamp inertly behind sema's defer until the
type-abbreviation navigation slice lands (non-goals).

Ordering note: marker synthesis runs after the §2 overlay, but reads its
range from its own `PickledEntity` — there is no cross-pass dependency.

### 4. LSP: compose the fallback, mirroring the member path

- `assembly_entity_location`: drop the `tokens.is_empty() → None` early
  return (an entity with zero methods but a range must proceed); compute the
  token-sweep result and fall back to the range:
  `entity_definition_source_in_pdb(&pdb_image, tokens)` then, on `None`,
  `range_definition_source_in_pdb(&pdb_image, range)`. Both need the PDB
  image (the range leg reads SourceLink JSON from it), so the existing
  PDB-acquisition flow stands; a small
  `entity_definition_source_with_range_fallback` in goto_source.rs keeps the
  composition testable without a workspace, exactly like the member-side
  `definition_source_with_range_fallback`.
- `entity_definition_document`: mirror `member_definition_document` —
  `from_pdb().or_else(|| range.map(definition_document_for_range))`, which
  needs no PDB at all. Hover's "defined in" line then works for value-only
  modules, measures, and abbreviation markers even for a PDB-less DLL.

No new goto_source primitives: `range_definition_source_in_pdb` and
`definition_document_for_range` (PR #157) are reused as-is.

### What deliberately stays unstamped

- **Namespaces** — no ECMA row to stamp (probe finding 4).
- **The root CCU entity** — suppressed by the walk already.
- **Arity-overloaded siblings** — declined as ambiguous (§2); they keep
  their sequence-point navigation.
- **C# assemblies** — no signature pickle, so the overlay never runs; the
  C#-sweep test pins it.
- **Foreign CCUs in `--standalone` builds** — the overlay is already gated on
  the host pickle being authoritative for the image; that gate stands.

## Test plan (write first, watch them fail)

Assembly crate — extend `crates/assembly/tests/all/pickled_ranges.rs`, using
its existing `binder_position` self-referential oracle against the fixture
source (no hardcoded line numbers):

1. `module Hello` entity carries the range at
   `binder_position(src, "module ", "Hello")`, spanning exactly `Hello` —
   pins the span-the-identifier convention for entities.
2. `RqaModule` (module whose only member is a value) carries its range — the
   motivating shape.
3. `type Choice` (a type, not a module) at
   `binder_position(src, "type ", "Choice")`.
4. `[<Measure>] type m` — a method-less entity — carries its range.
5. `Suffixed` (the `ModuleSuffix` module): the entity's IL name is
   `SuffixedModule` but the range spans the *source* binder `Suffixed` —
   pins probe finding 1's rename behaviour.
6. Arity twins (`type ArityTwin = { AtField: int }` plus
   `type ArityTwin<'T> = { AtGeneric: 'T }`): *neither* carries a range —
   pins §2's decline-on-ambiguity in both directions. These must live in a
   **new, non-differential fixture** (same `build_fixture` machinery, its
   own source file), *not* in MiniLibFs's `Library.fs`: `fcs-dump entities`
   explicitly fails on an F#-defined generic entity (the IL-typar surface
   is only reachable for IL-imported types — see the Phase-4l comment in
   the fixture), so adding the generic twin to the shared fixture would
   sink `diff_assembly_minilib_fs` before the new assertion ever ran.
   This shape makes *both* directions ambiguous at once, so on its own it
   cannot catch an implementation that guards only the collected-target
   side; pin the ECMA-side guard separately with a **one-target/two-rows**
   case — give the same non-differential fixture an `.fsi` that exports
   only `type ArityTwin` while the `.fs` also defines a private
   `ArityTwin<'T>`: the pickle then supplies one target while metadata
   holds two arity-stripped `ArityTwin` rows, and the stamp must decline
   rather than hit whichever row the name-only walk finds first.
7. An exception-abbreviation marker (new fixture line,
   `exception MyErrorAlias = MyError`): the synthesised
   `EntityKind::Exception` marker carries the range of the `MyErrorAlias`
   binder — pins §3 on the reachable marker kind. Check empirically whether
   `diff_assembly_minilib_fs` tolerates the alias in `Library.fs` (marker
   synthesis for it exists, but the fcs-dump side may not mirror the
   entity); if not, it joins the arity twins in the non-differential
   fixture.
8. C# sweep (MiniLib): every entity's `definition_range` is `None`.

LSP crate — extend `crates/lsp/tests/all/goto_source_fsharp_core.rs` (real
FSharp.Core oracle, same style as the PR #157 value tests):

9. An entity whose token sweep already succeeds returns a byte-identical
   result with a decoy range supplied — the range must not preempt the token
   path.
10. `Microsoft.FSharp.Data.UnitSystems.SI.UnitNames.metre` (a *standalone*
    measure — ECMA TypeDef row, zero methods): precondition that the token
    path finds nothing, then the range fallback resolves to `SI.fs` via
    SourceLink. Not `UnitSymbols.m`: that is `[<Measure>] type m = metre`,
    an *erased measure abbreviation* with no ECMA row —
    `merge_measure_entities` deliberately excludes it, so no `Entity`
    handle exists to navigate from (it sits with the type-abbreviation
    non-goal). The fixture's `m`/`kg` are standalone and unaffected.
11. `ListModule`: the pickled range names `list.fsi` — pin that the `.fsi`
    path (absent from the PDB Document table) still SourceLink-maps to a
    Remote URL, i.e. probe finding 2 end-to-end.
12. `entity_definition_document` with no PDB: range-only fallback yields the
    pickled document/position.

End-to-end: an existing definition-handler integration test extended (or a
sibling case added) so textDocument/definition on a *reference to a
value-only module* returns the module binder's location.

## Implementation order

1. Tests 1–8 (failing: field doesn't exist → then stamps missing), including
   the arity-twin and exception-alias fixture additions.
2. §1 model field + mechanical `None` sweep
   (`cargo check --workspace --all-targets` drives the sweep).
3. §2 overlay stamping (module/type + measure collection, ambiguity decline)
   and §3 marker stamping → tests 1–8 green.
4. Tests 9–12 (failing).
5. §4 LSP composition → all green.
6. Full gate, codex review, manual spot-check on a real workspace (hover
   "defined in" + definition on a value-only module from a NuGet package).

## Non-goals

- **Type-abbreviation navigation** (`type IntId = int`). The marker carries
  its range after §3, but nothing can read it: sema's contract for a hit on
  an `EntityKind::Abbreviation` marker is *defer* (`is_abbreviation`;
  `assembly_path_records` / `assembly_type_path_core` turn marker lands
  into deferred or project-shadowed readings), so no `Resolution::Entity`
  ever carries one and `assembly_entity_location` is unreachable for it.
  (Exception-abbreviation markers are *not* in this bucket — they are
  `EntityKind::Exception`, reachable, and covered by §3.) Making type
  abbreviations navigable means giving definition (and hover's "defined
  in") a *definition-only* way to retain the marker handle where resolution
  deliberately declines — a sema/LSP-surface design question that belongs
  with the abbreviation-target work
  (`docs/abbreviation-target-projection-plan.md`), not a by-product of this
  slice.
- **Union cases / record fields** as navigation targets: their pickled
  idents carry ranges too, but member-level navigation already works via
  sequence points on their methods; no observed gap.
- **Preferring the `.fs` over the `.fsi` for entities**: the implementation
  position is simply not in the pickle (probe finding 2). Recovering it
  would mean correlating the entity's *members'* ranges (vals do carry
  `DefinitionRange`), which conflates "where the module's contents are" with
  "where the module is declared" — FCS itself shows the `.fsi`
  cross-assembly, and matching FCS is this codebase's definition of correct.
