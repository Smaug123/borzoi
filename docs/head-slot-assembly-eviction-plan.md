# Plan: assembly types evict the head-slot value (the `head_value_slot` assembly gap)

> **Status:** Stage 1 (the correctness fix) SHIPPED (2026-07-03); the probe
> matrix is complete. `head_value_slot` (`crates/sema/src/resolve/lookup.rs`)
> now consults assembly types when deciding whether a definite-value head still
> holds FCS's unqualified-name slot, so a class/struct/enum brought in by an
> `open` written *after* a same-named local value evicts the value and the head
> defers instead of being mis-recorded as member access on the local. The only
> outstanding work is Stage 2 (assembly-member availability), deliberately
> deferred — see "Still to do".

## What landed (Stage 1 — the eviction, correctness)

Assembly types reuse the M20 project-type slot model wholesale: FCS's
`mayHaveConstruction = isClassTy || isStructTy || isDelegateTy`
(`AddPartsOfTyconRefToNameEnv`) governs eviction for both project and assembly
types. Eviction fires **only** through an explicit `open` written after the
value — enclosing-namespace, root, and implicit FSharp.Core opens all behave as
if opened at position 0, so a value declared later always re-takes the slot and
they can never evict it (probe A6). The one new piece is mapping an assembly
`EntityKind` (`crates/assembly/src/model.rs`) to a `SlotClass`.

- `AssemblyEnv::public_types_named` (`crates/sema/src/assembly_env.rs`) —
  arity-agnostic, public-filtered; returns `(EntityKind, is_struct)` for every
  public type of that name directly under a namespace (a generic-only type
  evicts a bare head, so all arities must be scanned; probe Ageneric).
- `assembly_slot_class(kind, is_struct)` (`lookup.rs`) — checks the reliable IL
  value-type flag `is_struct` first (catches `Struct`/`Enum`/`[<Struct>]`
  record-union, more precise than the spoofable source-attribute the project
  side had to treat as undecidable). `is_struct` or `Class`/`Struct`/`Enum` →
  `Evicts`; `Interface`/`Union`/`Record`/`Module`/`Measure` → `Keeps`;
  `Delegate`/`Abbreviation`/`Exception` → `Unknown`. Errs toward
  `Evicts`/`Unknown`, never `Keeps`, because under-eviction is the only unsound
  direction (over-eviction merely defers a resolvable head — availability loss).
- `assembly_open_prefixes` (`crates/sema/src/resolve/state.rs`) — every assembly
  *reading* captured before the `project_readings_only` filter, so a direct
  `open Demo` that merges a project module with the assembly namespace `Demo` is
  included while a module *alias* (which produces no reading) is excluded.
  Consulted in `head_value_slot`'s open loop **in addition** to the three
  project-index checks (a merged path can hold both a project type and an
  assembly type; a constructible assembly type evicts even if a co-named project
  type keeps).
- An evicted head then defers: the qualified block's assembly tiers are already
  barred for an evicted head (the M20t/M20u rule), so Stage 1 converts the
  wrong-target (recording the local value's field) into a sound defer. It does
  not resolve `Math.PI` to `System.Math.PI` — that is Stage 2.
- Tests: 12 FCS-free unit tests in `resolve_assembly.rs`
  (class/struct/enum/generic-only/`[<Struct>]`-record evict;
  interface/union/record/module keep; open-before-value keeps; module-alias does
  not consult; direct-merge evicts), built by cloning a fixture class and
  retargeting `.kind`/`.is_struct`; plus a live-FCS differential `pa_evict` in
  `resolve_project_assembly_diff.rs` (its panic guard is the wrong-target check).
  Probe matrix A1–A6 / Aenum / Ageneric / Amodule / Aunion / Arecord / Adelegate
  complete, all BCL-pinned via `fcs-dump uses-project` + `dotnet build`.

---

## Still to do

### Stage 2 — assembly-member resolution (availability). DEFERRED — the cost/risk does not clear the bar.

The idea: for an `Evicted` head whose evictor is an *assembly* type, relax the
round-5 bar so the qualified block resolves `Math.PI` → `System.Math.PI` (A1's
FCS pin). On inspection this is not a small lift:

- It must distinguish an assembly evictor (members knowable) from a project
  evictor (opaque — M20t/M20u must still defer), so `head_value_slot` would have
  to surface *which* source won the slot.
- Worse, it must **reconcile two different precedence models**:
  `head_value_slot` orders evictors by **source position** (FCS's
  latest-open-wins slot), while the assembly tiered walk
  (`resolve_assembly_path_tiered`) orders by **proximity tier** (opens >
  enclosing > root). With more than one same-named assembly type across opens
  (`open A; open B; let Color = …; Color.Red`, both `A.Color` and `B.Color`
  classes), FCS binds `B.Color` (latest open) while the tiered walk may pick
  `A.Color` — a **fresh wrong target**. A safe Stage 2 would have to verify the
  walk resolved through the *evictor's own open*, not merely that some assembly
  reading matched.

This is the precedence-reconciliation complexity that drove the M20 review
rounds, spent on a **pure availability** gain over a **niche, gotcha-ish**
construct (shadowing an assembly type with a local value, then dotting the
type's member — FCS itself resolves it surprisingly). Per "correctness over
availability" and the doom-loop discipline, Stage 1's correctness fix stands
alone; an assembly-evicted head **defers** (sound, an availability gap). Take
Stage 2 only if the gap shows up in real corpus numbers, and then only as the
narrow single-evictor subset with the walk-through-the-evictor's-open check and
its own probes. The `pa_evict` differential's `expected_assembly: 0` is the line
Stage 2 would lift.

### Boundaries left open (note, do not grind)

- **Stage-2 availability** if not taken: an assembly-evicted head defers rather
  than resolving the assembly member. Sound (never wrong), an availability gap.
- **`open type` / opaque assembly opens** keep their pre-existing conservatism
  (the qualified block is barred; an evicted head defers regardless).
- The pre-existing `docs/project-type-member-plan.md` §5 boundaries (bare-name
  eviction blindness; the member/enum qualifier branches comparing opened values
  at their *definition* position; the capitalized-pattern-binder hole) are
  unaffected and out of scope here.

### Doom-loop tripwire

This is a single, well-bounded consultation reusing the M20 slot model. If a
review finding needs the enclosing-namespace or root tier after all
(contradicting A6), re-probe A6 before expanding scope — the position argument
says those tiers cannot evict a later value.
