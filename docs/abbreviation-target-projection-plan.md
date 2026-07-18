# Abbreviation-target projection plan

> Closes the type-abbreviation half of
> [#87](https://github.com/Smaug123/borzoi/issues/87) ("F# pickle reader:
> projection gaps"). Scope: **decode the *target* of a referenced-assembly F#
> type abbreviation** (`type IntId = int`) from the host signature pickle, so
> consumers can resolve *through* the alias instead of deferring. The other #87
> bullets (`TType_anon` tag 9, SRTP arms 4/5, the extension-member projection
> holes, the arity-collision under-set) are out of scope.

## 1. Current state — what is and isn't done

The divergence-catalogue entry that #87 cites
(`docs/fcs-divergences.md`: *"Type abbreviations not projected from the pickle
merge … deferred"*) is **stale in one direction**: the abbreviation *name* is
already projected. `apply_abbreviation_markers`
(`crates/assembly/src/fsharp_pickle_merge.rs:1480`) synthesises a **name-only**
marker `Entity` for every public, non-measure, non-FSharp.Core metadata-invisible
abbreviation the host pickle declares — `EntityKind::Abbreviation` for a plain
abbreviation, `EntityKind::Exception` for an exception abbreviation
(`exception Alias = Original`) — carrying the F# source name, typar names, and an
`[<AutoOpen>]` flag. This is the R2-0 V3 fix
(`docs/completed/r2-annotation-typing-plan.md:15`).

What is **not** done is the *target*. Every marker is built by
`abbreviation_marker` (`fsharp_pickle_merge.rs:1606`) with
`base_type: None`, `members: []`, and **no target field at all**. The module
header states the deferral verbatim (`fsharp_pickle_merge.rs:1453`):

> The markers are deliberately **name-only**: no target type (decoding
> `type_abbrev` into the owned `TypeRef` model is the wider-merge slice this
> module's header defers) … A consumer can recognise them by kind and treat a
> hit as "this name is taken by an abbreviation whose target we cannot see" — a
> defer signal, not a resolution target.

There is **no `PickledType → owned-model` conversion anywhere** in the crate
today (confirmed by survey: `TypeRef` appears in `fsharp_pickle_merge.rs` only in
`#[cfg(test)]` and doc comments; `simpletys` is never resolved). This plan builds
that bridge, for the abbreviation-target slice only.

## 2. Consumers that will benefit

Every downstream consumer currently **defers** on a marker (D5: "defer, never a
wrong target"). A decoded target lets each one resolve where FCS resolves:

| Consumer | Site | Today | With a decoded target |
| --- | --- | --- | --- |
| Dotted-path member tail | `crates/sema/src/resolve/assembly.rs:57` (`ProjectShadowed`) | defers whole path | `S.Format` where `type S = System.String` resolves the tail on `System.String` |
| `open type Alias` | `crates/sema/src/resolve/decls.rs:786` (opaque branch) | opaque | opens the target's static content |
| Annotation → `Ty` | `crates/sema/src/infer.rs:1386` (`entity_annotation_ty` returns `None`) | `let x : IntId = …` doesn't bridge | bridges to `Ty::Named` for `int` |
| Hover | `crates/lsp/src/handlers/hover.rs:627` | renders `"type abbreviation"` | can render `IntId = int` |

Because each of these is currently a *defer*, an **absent (`None`) target is a
no-op**: the consumer keeps deferring exactly as today. This makes the whole
change strictly additive — a decoded target can only turn a defer into a correct
resolution, never into a wrong one.

## 3. Design

### 3.1 Representation: a *logical* target, not a `TypeRef`

The decoded target must **not** be the owned `TypeRef` (`crates/assembly/src/model.rs:49`).
Two hard constraints make `TypeRef::Named` the wrong shape:

1. **No namespace/nested split.** An IL-projected `TypeRef::Named` gets its
   `{ namespace, name }` split and nesting chain directly from the ECMA
   `TypeRef` table (`crates/assembly/src/reader/typedefs.rs:170` reads explicit
   `TypeNamespace`/`TypeName` columns + a `ResolutionScope::Nested` chain). A
   pickle **nleref** is a flat `ccu`-index + dotted string-index `path`
   (`PickledNleRef`, `fsharp_pickle/model.rs`) with *no* split — `M.T` (type `T`
   nested in module `M`) is indistinguishable from namespace `M`, type `T`. FCS
   itself does not split it at decode time; it resolves the nleref *lazily
   against the referenced CCU once loaded*. The single-assembly reader does not
   have the referenced assembly, so it cannot perform the split without guessing.

2. **No assembly identity.** The pickle's `ccu_refs` carry the referenced
   assembly's **logical name only** — no version, no public-key-token
   (`fsharp_pickle/model.rs`: *"assembly identity is resolved via the host
   loader"*). `TypeRef::Named.assembly` is an `Option<AssemblyIdentity>` needing
   both.

So we represent the target as a **logical reference** carrying exactly what the
pickle knows, and resolve it — the split and the identity — at the **sema
layer**, which has every referenced assembly loaded in `AssemblyEnv`. This
mirrors FCS's own lazy-nleref architecture and honours "parse, don't validate"
(we store what we read; we do not fabricate a split or an identity we cannot
know) and "correctness over availability" (a shape we cannot faithfully model
becomes an honest `None`, never a guess).

New owned type in `crates/assembly/src/model.rs`:

```rust
/// A referenced/same-assembly F# type-abbreviation target, decoded from the
/// host signature pickle's `type_abbrev` into a *logical* reference the sema
/// layer resolves. NOT a `TypeRef`: see the plan's §3.1 for why the ECMA
/// namespace/nested split and the assembly identity are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AbbreviationTarget {
    /// A tycon application. `ccu = None` for a same-assembly (`Local`) tycon,
    /// `Some(name)` for a referenced one (the CCU *logical name*; the loader
    /// resolves the full identity). `path` is the unsplit dotted logical path
    /// (`["System", "String"]`, `["Microsoft","FSharp","Collections","list"]`).
    Named {
        ccu: Option<String>,
        path: Vec<String>,
        args: Vec<AbbreviationTarget>, // empty until Stage 3
    },
    /// The abbreviation's own generic parameter, by position into the marker
    /// `Entity::generic_parameters` (`type MyList<'T> = 'T list` ⇒ `Var(0)`).
    Var(u16),
    // Stage 3 adds: Array { element, rank }, Fun(Box, Box),
    // Tuple { struct_kind, elems }.
}
```

It hangs off the marker as a new `Entity` field
`abbreviation_target: Option<AbbreviationTarget>` — `None` on every non-marker
entity and on any marker whose target the decoder cannot (yet) model. This is
consistent with the crate's existing optional-fact fields (`union_case_names:
Option<…>`, `obsolete: Option<…>`).

### 3.2 The decoder (`PickledType → AbbreviationTarget`)

A new `decode_abbreviation_target(pickled, entity, &PickledType) ->
Result<Option<AbbreviationTarget>, ImportError>`, living beside the other
overlays in `fsharp_pickle_merge.rs` (or a new `fsharp_pickle/type_bridge.rs`).
Primitives it needs, each with an existing precedent or a single new step:

- **Non-local tcref → `(ccu_name, path)`.** Reuse the nleref→strings
  reconstruction already in `has_auto_open_attribute`
  (`fsharp_pickle_merge.rs:1400`), **plus** the missing step it omits: map
  `PickledNleRef.ccu` through `header.ccu_refs[ccu].name` to the CCU logical
  name (`has_auto_open_attribute` ignores `ccu`; nothing maps it today).
- **`AppSimple { simpletyp_index }` → nleref.** `header.simpletys[idx]` is an
  nleref index (`fsharp_pickle/model.rs:70`); resolve it exactly as a non-local
  tcref. **No code resolves `simpletys` today** — this is new but mechanical.
- **Local tcref → same-assembly FQN.** `PickledTcRef::Local(stamp)` is an osgn
  index into `tables.tycons` — the *same* index space `walk_entity_tree` walks
  (`entity_stamp as usize`). Build a `HashMap<u32 stamp, (namespace,
  type_chain)>` in one extra pass of the existing walk (it already computes each
  entity's container position); a Local tcref resolves to `Named { ccu: None,
  path: namespace ++ type_chain }`.
- **`Var { typar_index }` → position.** `typar_index` is an osgn index into
  `tables.typars`; the abbreviation entity's own typars are `entity.typars`
  (also osgn indices — see the marker code's `pickled.tables.typars.get(idx)`).
  The position is `entity.typars.iter().position(|&t| t == typar_index)`.
- **`Forall { typars, body }`** — the generic-abbreviation quantifier wrapper;
  decode `body` (its `Var`s reference `typars`, already bound as the entity's
  own typars).

**Fail-closed policy (load-bearing).** Any `PickledType` node the current stage
cannot faithfully model — `Fun`, `Tuple`, `Measure`, `UCase` (and, before
Stage 3, generic args and array/byref/pointer intrinsic heads) — makes the
**whole target** decode to `Ok(None)`, never a partial or fabricated value. A
`None` target keeps every consumer deferring, so this can never regress a
resolution. A genuinely malformed pickle (dangling index) is still a loud
`Err(ImportError::…)`, consistent with the crate's fail-loud contract; only
*unmodelled-but-well-formed* shapes are the quiet `None`.

`Measure`-kinded entities are already excluded upstream
(`apply_abbreviation_markers` skips `typar_kind == Measure`), so measure targets
never reach the decoder.

### 3.3 The differential oracle (how "correct" is defined)

The gold standard in this crate is byte-faithful agreement with FCS via
`tools/fcs-dump`, driven by the existing entity-level differential harness
(`crates/assembly/tests/all/assembly_diff.rs`: `normalise_entities` on our side,
`parse_fcs_dump` on FCS's, over `invoke_fcs_dump("entities", dll)`). The
per-entity JSON record fcs-dump emits (`Program.fs:4313`) carries **no
abbreviation-target field** today — the oracle identifies an abbreviation only by
`Kind = "Abbreviation"`. So the differential requires **extending fcs-dump** to
emit each `IsFSharpAbbreviation` entity's target, plus a matching
`NormalisedEntity` field. Per the user's steer, this oracle **leads** the work
(Stage 1), so the decoder is born differentially-tested.

**Canonical rendering — the immediate, unchased, *logical* target.** This is the
load-bearing oracle-design decision. FCS's convenient renderer `renderTypeInScope`
(`Program.fs:684`) is the *wrong* form: it **strips every abbreviation layer**
(line 695) and **compiles** F# structure — `type S = string` renders as
`System.String`, `type Pair = int * string` as `System.Tuple<…>`. But the
pickle's `type_abbrev` stores the **immediate** target with a **logical** tcref
(`type S = string` ⇒ `App(Microsoft.FSharp.Core.string, [])`, *not* chased to
`System.String`), and the single-assembly reader **cannot** chase cross-assembly
(chasing `string`→`System.String` needs FSharp.Core's own pickle). Matching the
compiled/chased form is therefore impossible on our side and also *unwanted*: the
consumer (sema) does its own further chasing and hard-codes the primitive aliases
(`fsharp_primitive_alias`), so the immediate logical target is the correct unit.

So fcs-dump needs a **new** renderer (`renderAbbreviationTargetLogical`) that
emits `e.AbbreviatedType`'s *immediate* tycon by *logical* FQN — **not**
`renderTypeInScope`, and **not** `chaseAbbreviation` (`Program.fs:4653`, which
walks to the terminal). It renders the head tycon's logical `AccessPath ++
LogicalName` (abbreviation entities return `None` from `TryFullName`, so
reconstruct from `AccessPath`/`LogicalName`), a leading `'` typar as its
positional index, and — Stage 2 — `int list`'s args, `int * string` as an
*immediate tuple* form, `int -> int` as an *immediate function* form (the F#
structure the pickle stores, not the compiled `System.Tuple`/`FSharpFunc` shape).
Our `AbbreviationTarget` renders to the identical string.

**Empirical grounding first.** Whether the pickle preserves the source-level
alias (`int` vs. its own target `int32`) and whether FCS's `AccessPath`+
`LogicalName` matches the pickle's nleref `path` segment-for-segment are settled
**by observation**, not assumption: extend fcs-dump first, run it over
`FSharp.Core.dll` and the F# fixtures, read the ground-truth strings, then build
the Rust decoder to match them.

**Assertion shape — certain-implies-exact, as its own test.** The whole-tree
`assert_eq!` in `assembly_diff.rs` cannot express the asymmetry (a declined
target must assert nothing while FCS still emits one), so the abbreviation-target
differential is a **dedicated** test in the `condition_diff.rs` mould: iterate
entities; for each where **our** side decodes `Some(target)`, assert FCS's
rendered target equals it; where we decline (`None`), assert nothing. FSharp.Core
is a rich, free source — although markers are not *emitted* for it, the *decoder*
is run over its pickle purely for differential coverage (hundreds of
primitive-alias and collection abbreviations), which is where most of the
oracle's power comes from.

## 4. Implementation plan

Implement this plan with each stage on its own branch, stacked as necessary on
previous branches, so that a reviewer can review each branch in isolation.

Fixtures: the pickle-bearing test DLLs are `MiniLibFs.dll` and `FsExtIndex.dll`
(`crates/assembly/tests/all/common/mod.rs`, `build_fixture`). `FsExtIndex`
already has a **local** abbreviation `[<AutoOpen>] type TalliedAlias = Tallied`
(`fixtures/assembly/FsExtIndex/Library.fs:105`) and the exception abbreviation
`exception PatternAlias = PatternProblem` (line 98). No fixture yet has a
**referenced/primitive** target, so stages that need one add e.g. `type IntId =
int` and `type S = System.String` to a fixture `.fs`.

---

### Stage 1 — the oracle: fcs-dump immediate-logical target + differential harness

**Dependencies**: none.

**Implements**: §3.3.

The differential leads. Teach `tools/fcs-dump/Program.fs` to emit each
`IsFSharpAbbreviation` entity's **immediate, unchased, logical** target — a new
`renderAbbreviationTargetLogical` (§3.3), *not* `renderTypeInScope`/
`chaseAbbreviation` — as a new field on the entity record. Add
`abbreviation_target: Option<String>` to `NormalisedEntity` and read it in
`parse_fcs_dump` (`crates/assembly/src/test_support.rs`).

Because no Rust decoder exists yet, our side emits `None` everywhere, so the
*decoder* half of the differential is vacuous this stage — but the oracle
infrastructure is proven and FCS's rendering is pinned directly.

**Status (in progress).** The fcs-dump half is **done** in the worktree:
`renderAbbreviationTargetLogical` is added, and `projectEntity` now takes an
**early minimal-projection branch** for `IsFSharpAbbreviation` entities. That
branch was *forced* by an empirical discovery — the full projection **crashes**
on abbreviation entities, because FCS surfaces the *target's* members and typars
through the transparent abbreviation: a `list`/`string` target trips
`F#-defined indexed property … not supported`, and a *generic* abbreviation
trips `non-IL generic entity … not supported`. The marker is name-only anyway,
so the minimal branch emits just the target (no members / interfaces / nested /
generic-param projection), which both fixes the crash and matches the marker.

**Observed ground truth** (from a throwaway probe lib, fcs-dump `entities`):

| Source | fcs-dump `AbbreviatedTarget` |
| --- | --- |
| `type IntId = int` | `Microsoft.FSharp.Core.int` |
| `type S = System.String` | `System.String` |
| `type S = string` | `Microsoft.FSharp.Core.string` |
| `type ObjId = obj` | `Microsoft.FSharp.Core.obj` |
| `type MyList = int list` | `Microsoft.FSharp.Collections.list``1<Microsoft.FSharp.Core.int>` |
| `type IntArr = int[]` | `Microsoft.FSharp.Core.[]``1<Microsoft.FSharp.Core.int>` |
| `type IntFn = int -> int` | `Microsoft.FSharp.Core.int -> Microsoft.FSharp.Core.int` |
| `type Pair = int * string` | `(Microsoft.FSharp.Core.int * Microsoft.FSharp.Core.string)` |
| `type SP = struct(int*string)` | `struct (Microsoft.FSharp.Core.int * Microsoft.FSharp.Core.string)` |
| `type Generic<'T> = 'T list` | `Microsoft.FSharp.Collections.list``1<!T0>` |
| `type SelfVar<'T> = 'T` | `!T0` |
| `type AliasOfLocal = Concrete` (same-asm) | `AbbrevProbe.Concrete` |
| `module Inner` / `type NestedAbbrev = int` | `Microsoft.FSharp.Core.int` (nested) |

Load-bearing confirmations for the Rust decoder to match: (1) the target is the
**immediate** form (`int`, `string`, `nativeint`), never chased to `System.*`;
(2) a **same-assembly** target renders **path-only** (`AbbrevProbe.Concrete`, no
assembly/ccu qualifier) — so the canonical string carries the logical path but
**not** the ccu (which the model still stores for sema); (3) typars render
`!T<pos>`; (4) arrays/lists/functions/tuples arrive as `App`/structural forms
carrying the tycon's **backtick-arity** logical name (`list``1`, `[]``1`) — all
Stage-3 shapes. **Not settled until the Rust side runs**: whether the pickle's
nleref `path` for `int` equals `Microsoft.FSharp.Core.int` segment-for-segment
(the differential decides).

**Remaining Stage-1 work** (refined from the discovery above): **do not** add
`abbreviation_target` to `NormalisedEntity` — the whole-tree `assert_eq!` diffs
(`diff_assembly_minilib_fs`) would then break on the FCS-`Some`/our-`None`
asymmetry. Instead:
- The abbreviation-target differential is a **separate extraction** (parse
  fcs-dump JSON → `HashMap<fqn, target>`; compare against our decoded targets),
  so it never touches the whole-tree comparison.
- Add nullary abbreviations (`type IntId = int`, `type S = System.String`) to the
  `MiniLibFs` fixture. This is *safe for the existing whole-tree diff*: fcs-dump's
  minimal projection is now name-only and our side already synthesises a name-only
  `EntityKind::Abbreviation` marker (MiniLibFs is not FSharp.Core), so both
  normalise to the identical entity — which *itself* becomes a free oracle that
  the marker shape matches fcs-dump's abbreviation entity.
- Add the separate-extraction differential skeleton (our side `None` ⇒ asserts
  nothing until Stage 2).

**Correctness oracle**:
- A `#[test]` asserting fcs-dump's emitted target for known fixture
  abbreviations verbatim (`TalliedAlias` ⇒ `"Tallied"`; a new `IntId` ⇒
  `"Microsoft.FSharp.Core.int"`), and that abbreviation entities no longer crash
  the `entities` dump.
- The dedicated abbreviation-target differential runs green over
  `MiniLibFs`/`FsExtIndex` (our side `None` ⇒ asserts nothing; FCS side parsed).

---

### Stage 2 — the decoder: `AbbreviationTarget` model + nullary decode, attached

**Dependencies**: Stage 1.

**Implements**: §3.1, §3.2 (the `Named{ccu,path}` / `Var` / `Forall` subset,
**no generic args**), §2 wiring point (the new `Entity` field only).

Add the `AbbreviationTarget` type, the `Entity::abbreviation_target` field, and a
renderer (in `test_support.rs`) producing the exact string Stage 1 observed from
fcs-dump. Implement `decode_abbreviation_target` for a **nullary** named head
(`AppSimple`, or `App` with empty args) via non-local nleref / simplety /
local-stamp resolution; `Var`; and `Forall`-unwrap. Every other shape — anything
with args, `Fun`, `Tuple`, `UCase` — returns `Ok(None)`. Populate the field in
`abbreviation_marker`; leave `None` elsewhere. Covers `type IntId = int`, `type S
= System.String`, same-assembly `type Alias = LocalType`.

**Correctness oracle**:
- **The Stage-1 differential is now two-sided and meaningful**:
  certain-implies-exact over `MiniLibFs`/`FsExtIndex` and — run the decoder over
  it purely for coverage — the FSharp.Core pickle. Every `Some` we decode equals
  fcs-dump's rendered target; `None` asserts nothing.
- Unit tests over synthetic pickles (extend `ccu_with_abbreviations()`): `IntId =
  int` ⇒ `Named { ccu: Some(_), path: [..] }`; same-assembly alias ⇒
  `Named { ccu: None, .. }`; `type MyList<'T> = 'T` head-var ⇒ `Var(0)`.
- Real-fixture assertion (`projector_open_surface.rs`): `TalliedAlias = Tallied`
  ⇒ `Named { ccu: None, path: ["Tallied"] }` (the Local path on a real pickle).
- **Property**: `decode_abbreviation_target` is total and fail-closed —
  arbitrary well-formed `PickledType` trees never panic, and any tree with an
  unmodelled node yields `None` for the *whole* target (no partial decode).
  (`property-based-testing` skill.)
- Existing marker/resolve tests stay green — the field is additive.

**Status (done).** Landed with three refinements the implementation forced:

1. **Same-assembly targets pickle as *non-local-to-self*, not `Local`.** A public
   signature is written to be read from other assemblies, so fsc pickles a
   reference to the current CCU's own type as a non-local ref whose ccu is the
   assembly *itself* (`FsExtIndex`'s `TalliedAlias`, `MiniLibFs`'s `PointAlias`
   both do this). The decoder **normalises** a self-ccu to `ccu = None`, so the
   model's invariant is *`None` iff same-assembly* regardless of the pickle's
   `Local`/non-local-to-self encoding — one same-assembly path for sema, not two.
   `decode_abbreviation_target` therefore takes the current assembly's name. (The
   `Local` decode path stays — exercised by the synthetic unit test — but real
   fixtures reach the non-local-to-self path.)
2. **The two-sided differential rides entirely on `MiniLibFs`.** `MiniLibFs`
   dumps cleanly through `entities`, so its fixtures were widened to exercise
   *every* decode path two-sided: `IntId`/`ObjId` (referenced NonLocal), `S`
   (BCL), `PointAlias` (same-assembly → `MiniLibFs.Point`), `SelfVar<'T>` (typar
   → `!T0`), and `MyList<'T> = 'T list` (declined). The **FSharp.Core sweep is
   deferred**: `fcs-dump entities` still aborts on FSharp.Core's first
   indexer-property type (only *abbreviation* entities got the minimal-projection
   branch), so a whole-assembly dump is unavailable. Reaching that coverage needs
   a narrower `fcs-dump abbrev-targets` mode that skips non-abbreviation
   projection — a follow-up, noted here so the coverage gap is explicit.
3. **The extraction keys by `(fqn, arity)` with the container path threaded in**
   (Stage-1 codex review): a nested alias keys by its full path and an
   arity-overloaded pair (`type A = int` / `type A<'T> = …`) does not collide.

---

### Stage 3 — generic args + structural shapes (`Array`, `Fun`, `Tuple`)

**Dependencies**: Stage 2 (and its Stage-1 differential, which pins the
intrinsic-tycon recognition).

**Implements**: §3.1 (the deferred variants), §3.2 (arg recursion + intrinsic
recognition).

Add `Named.args` recursion and the structural variants. This forces
recognition of the compiler-intrinsic heads (`int[]` pickles as `App` of the
array tycon; `FSharpFunc`; the byref/pointer intrinsics) so they decode to the
dedicated shapes rather than a bogus `Named`. Each newly-modelled shape is a
separate concern; land array, function, and tuple in whatever sub-order the
oracle makes cheapest, but keep each behind its own test.

**Correctness oracle**:
- New fixtures `type Bytes = byte[]`, `type IntFn = int -> int`, `type Pair =
  int * string`, `type MyList = int list` decode to the expected shapes.
- The Stage-2 differential extends over these and over FSharp.Core /the corpus
  (rich in `'T list`, `'T[]`, function-typed aliases): certain-implies-exact.
- The fail-closed property still holds for the residue (`Measure`/`UCase`).

---

### Stage 4 — sema resolve-through (dotted paths + `open type`)

**Dependencies**: Stage 2 (Named targets suffice; Stage 3 widens coverage).
Parallel with Stage 5.

**Implements**: §2 rows 1–2.

`AssemblyEnv` gains a resolver: given `AbbreviationTarget::Named { ccu, path,
args }`, find the target `EntityHandle` — `ccu: None` ⇒ the same assembly,
`Some(name)` ⇒ the loaded assembly with that logical name — trying the
namespace/nested splits of `path` against the loaded tree (this is where the
split deferred in §3.1 finally happens, with the referenced assembly present).
At the `is_abbreviation ⇒ ProjectShadowed` sites in `resolve/assembly.rs` (both
the value/member and type-position fns) and the `open type` opaque branch in
`resolve/decls.rs:786`, follow a resolvable target through; fall back to the
current defer when the target is `None` or does not resolve.

**Correctness oracle**:
- Extend `crates/sema/tests/all/resolve_fsharp_abbrev.rs`: `S.Format` where
  `type S = System.String` resolves the member tail; `open type` of a marker
  opens the target's statics; an unresolvable/`None` target still defers
  (no regression — every existing assertion in that file must stay green).
- The whole-project name-resolution differential
  (`resolve_real_project_diff`, `resolve-real-project-diff` skill): abbreviation
  uses that FCS resolves and we used to defer now agree, with no new
  divergences.

---

### Stage 5 — annotation bridge + hover rendering

**Dependencies**: Stage 2 (Stage 3 for structural targets). Parallel with
Stage 4.

**Implements**: §2 rows 3–4.

`entity_annotation_ty` (`infer.rs:1386`): for a marker with a decoded target,
bridge to `Ty` — `Named` targets to `Ty::Named` under the existing
`member_ty.rs` conventions; `Fun`/`Tuple` targets to `Ty::Fun`/`Ty::Tuple`
(sema's `Gen::annotation_ty` already recurses those shapes,
`r2-annotation-typing-plan.md:27`). Hover (`hover.rs:627`) renders `IntId =
int` from the target.

**Correctness oracle**:
- `infer`/hover unit tests: `let x : IntId = e` grounds `x : int` and flows to
  hover and `x`'s uses; hover shows the RHS.
- Hover differential where available; `entity_kind_words_are_stable` and other
  existing hover pins stay green.

---

### Closeout (part of the last-landed stage)

Update `docs/fcs-divergences.md`: the *"Type abbreviations not projected from the
pickle merge"* entry moves to *"Resolved since introduction"* (or is narrowed to
the residual `None`-target shapes — measure/ucase/anon), and the `#87` roadmap
item is ticked for the abbreviation slice.

## 5. Out of scope / deferred

- The other #87 bullets: `TType_anon` (tag 9) — note an *anonymous-record*
  abbreviation target makes the whole pickle decode fail loudly today
  (`UnsupportedPickleTag`), so it is already bounded by
  `fsharp_abbreviations_unknowable` and never reaches this decoder; SRTP arms
  4/5; the extension-member projection holes; the arity-collision under-set.
- **FSharp.Core marker emission** stays excluded — its primitive aliases are the
  semantics sema hard-codes (`fsharp_primitive_alias`), not a shadow risk
  (`fsharp_pickle_merge.rs:1463`). Its pickle is used only as differential
  fodder (§3.3), never to emit markers.
- **Generic *instantiation* through an abbreviation** in sema (substituting
  `MyList<int>`'s `'T`) — the R2 plan already defers "generic instantiations
  (pending `Ty` args)" (`r2-annotation-typing-plan.md:57`); this plan decodes
  `Var` faithfully but does not require the consumer to instantiate.
