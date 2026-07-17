# Plan: project-type member modeling (`Type.Member` statics + the Bexpr retirement)

> **Status:** Same-file static-member resolution (**D1**) has landed (PR #710),
> as has the **M20** definite-value head-slot gate (PR #730). Still outstanding:
> cross-file member export (**D1** cross-file), **D2** member-absence
> backtracking (the Bexpr retirement), and extension-member emit. Everything
> below the "Landed" list has detail *only* on what remains.

Sema models values, union/enum cases, modules, namespaces, and (since the
cross-file type index) types-as-case-carriers. On top of that, a per-file member
index resolves `Color.Red` / `Pal.Color.Red` / `MyType.Create …` when the
segment is a *static member* of a **same-file** project type. Two things remain
open:

- **No cross-file positive resolution.** The cross-file member export table is
  still absent (assembly types already resolve via `Resolution::Member`; a
  cross-file *project* member keeps deferring through the existing shadow
  indexes).
- **The Bexpr sacrifice.** In expression position a type at the segment *without*
  the modeled case defers unconditionally — sema cannot prove member absence,
  even though FCS backtracks past member-less types. Every such defer is an
  availability gap on valid code.

## Landed stages (one line each)

- **Stage 1 — CST accessors** (PR #710) — member name / staticness / kind /
  accessibility on `ObjectModel` reprs and standalone `type … with`
  augmentations (read-side only over the existing `SynMemberDefn` shapes).
- **Stage 2 — Same-file D1** (PR #710) — the in-file member index
  (`Resolver::type_members`, a per-type `TypeMemberSet` with position-tagged
  `visible_from` and an owns-set / emit-set split); `Color.Red` /
  `Pal.Color.Red` emits when `Red` is a public static, through the 2- and
  3-segment paths of `classify_module_qualified_segment`; a new member `DefKind`
  verified workspace-wide.
- **M20 head-slot gate** (PR #730) — the definite-value head gate reframed as a
  source-ordered latest-wins slot across the value **and** type namespaces:
  construction-capable types (`SlotClass`) evict a co-named value, opened
  values/types enter at their `open`'s position, accessibility gates the import,
  and an evicted head resolves through the module side only (else defers).
  Assembly-open eviction Stage 1 shipped separately — see
  [`head-slot-assembly-eviction-plan.md`](head-slot-assembly-eviction-plan.md).

---

## Still to do

### The model (shared by the outstanding stages)

**Member entry.** Per type path, `name → { emit, name_range, visible_from }`:
`emit` is `Some(DefId)` only for a public static member / `member val` /
auto-property. Instance members (they commit the qualifier: FCS errors FS0806
rather than backtracking), access-restricted members, `static val` fields
(forcibly private — FS0881), interface-impl members, and unrecognised shapes are
in the index (they *own* the name → commit/defer) but never emit.

**Owns-set vs emit-set.** "Does the type own this name?" uses the full index (→
never fall through past it → DeferStop). "Can sema resolve it?" uses the
emit-eligible subset. Owned-but-not-emittable → DeferStop (today's behaviour).

**Enumerability (D2 only).** Absence is unprovable — keep the Bexpr defer — when
the type has an `inherit` (base statics leak through the derived name), is an
abbreviation sema does not chase, or contains any unmodelled member shape.
Absence at a *use site* additionally requires no member-adding construct in
scope: same-file augmentations are in the index (position-tagged); cross-file
augmentations are illegal (FS0644); module-housed optional extensions mean any
open of an extension-carrying module — or any opaque open — poisons absence for
the affected type(s). D1 needs none of this.

**Collision constraint.** A member's qualified path (`Demo.Pal.Color.Red`) can
*equal* a companion module value's path (`module Color = let Red`). Member
exports therefore need their **own cross-file table** (mirroring
`type_qualified_cases` / `ProjectItems::type_paths`), never `by_qualified_path`
— the cross-file-constructor-namespace history shows what riding the value index
costs.

**Qualifier ownership (2-segment).** Latest-wins between a co-named type and
value is the machinery the enum-qualifier work already implements; the member
emit reuses it. This holds same-file (landed); cross-file must reuse it too.

### Stage 3 — Cross-file D1 (OUTSTANDING)

`ResolvedFile::type_member_exports` → `ProjectItems::type_members` (the separate
table above, populated in the `extend_with` fold beside `type_qualified_cases`
and `type_paths`); the classifier's type arm emits through opens and qualified
heads. Abbreviation chasing stays same-file; cross-file abbreviation targets keep
deferring. A cross-file module at the head still outranks the same-file/opened
type for the dotted head (the r13 / M20 rules already landed — the classifier
consults `head_contested_by_project_module` and the slot gate first, so the
member emit only fires once those stand down).

### Stage 4 — D2 absence, the Bexpr retirement (OUTSTANDING)

The `members_enumerable` flag (per the enumerability rules above) plus use-site
poisoning; then flip the Bexpr arms — the same-file rule and the cross-file
`exported_type_at` arm — from DeferStop to fall-through when absence is *proven*.
This retires the Bexpr sacrifice for enumerable types. Lands strictly after
Stage 3, and only for types whose full member set the walker can enumerate;
everything else keeps deferring.

### Stage 5 — Extension emit (DEFERRED)

Resolve `Color.Red` *to* an opened optional static extension member — needs an
extension index keyed by resolved target type; only worth it once D2's poisoning
shows up in corpus numbers. NOTE: substantial extension-member work has since
landed under separate plans (EX-0, PR #935; the extension-visibility slices, and
[`extension-scope-enumeration-plan.md`](extension-scope-enumeration-plan.md)) —
re-evaluate the overlap before starting this slice.

### Envelope & tripwires

- Agree-or-defer on legal code at every stage; everything unmodeled lands in the
  owns-set (defer) or clears `members_enumerable` (defer). The corpus sweep's
  0-divergence gate is the backstop.
- Unprobed contests stay deferred: extension vs real member, private-member
  accessibility from within the type's own file, `open type` on project types.
- Doom-loop tripwire: if a precedence contest needs more than two probes to pin,
  leave it DeferStop and ship the uncontested emit.

## Boundaries left open

These are pre-existing (each is today's behaviour); referenced from
[`head-slot-assembly-eviction-plan.md`](head-slot-assembly-eviction-plan.md).

- **Bare-name eviction blindness.** A **bare** (non-compound) name use keeps
  `resolve_name_use`'s eviction blindness (FCS binds the evicted type's ctor for
  `let Color = 3; type Color(); let x = Color`) — unprobed, needs its own look.
- **Qualifier branches compare opened values at their *definition* position.**
  The member/enum qualifier branches still use `value_def_range`, so a
  member-carrying type + an `open M` (`M.Color`) written *after* it can emit the
  member where FCS binds member access on the opened value (the M20 open-position
  rule not yet applied to those branches); the fix is routing them through the
  slot-position machinery (`lookup_entry` + `open_pos`).
- **Capitalized-pattern-binder hole.** `let g (Color: {| Red: int |}) =
  Color.Red` binds the parameter in FCS, but sema's capitalized-pattern-binder
  conservatism leaves `Color` unbound; with an earlier file's `module Color / let
  Red` the qualified-value branch then resolves the module's value — a wrong
  target. Belongs to the pattern-binder classification family, not the head-slot
  gate.
- **Split get/set accessor declarations defer** (`static member Red with get`
  plus a separate `… with set`): the repeated-name rule treats them as an
  overload and withholds the emit, while FCS resolves a read to the getter. Sound
  (an honest defer); lifting it needs member-kind tracking so split accessors
  merge instead of conflicting. Pinned by
  `split_get_set_accessor_declarations_defer`.
