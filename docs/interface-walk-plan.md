# Interface-walk member resolution plan

> **Status: IW-0–IW-2 landed** (2026-07-17). The sub-plan for the "interface
> walk" hard pile listed in
> [`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md) ("Remaining 3.x hard
> piles"). The base-class walk landed in 3.x-inh covers base **classes** only;
> this slice adds member resolution for a receiver whose **static type is an
> interface**. It is the last structural gap before the *complete* method group
> the sound overload slice needs, and it pays off on both the name-resolution
> (go-to-def / find-refs of an interface member) and the expression-type (hover)
> axes. The primitive (`interface_member_chain`), interface data members, and
> interface methods have landed on branch `sema-interface-walk` (one FCS
> differential + five synthetic unit tests, GPT-5.6-reviewed); **IW-3
> (re-declaration hiding, `qualified_path_occupied` precision, inherited
> dot-completion) remains, optional and data-gated.**

## Scope — exactly one situation

Per FCS's intrinsic member walk
([`overload-resolution-plan.md` §2.1](overload-resolution-plan.md#21-method-group-construction),
`TypeHierarchy.fs:232–307`):

- **class / struct receiver → superclass chain only; implemented interfaces do
  NOT contribute** (`followInterfaces=false`, `TypeHierarchy.fs:306–307`). So the
  3.x-inh `base_chain` walk is **already complete** for class/struct receivers —
  this slice changes nothing there.
- **interface receiver → `System.Object`'s members PLUS all transitively
  inherited interfaces** (`TypeHierarchy.fs:256–260`).

So the entire slice is: **when the receiver `Ty::Named` resolves to an
`EntityKind::Interface`, build the member surface from the interface's own
members + the transitive closure of its inherited interfaces + `System.Object`.**
An interface receiver is reachable today: `entity_annotation_ty`
(`crates/sema/src/infer.rs:1379`) does **not** filter `EntityKind::Interface`, so
`let f (x : System.Collections.IList) = …` produces a ground `Ty::Named` interface
receiver that reaches the `HasMember` wake. Only non-generic interfaces reach it
(`entity_annotation_ty` declines generic entities), so the *receiver* interface is
always non-generic; its inherited interfaces may be generic (see completeness).

### What currently happens for an interface receiver (the baseline this fixes)

- `x.OwnProperty` / `x.OwnField` — **already resolves**: `instance_data_member`
  (`assembly_env.rs:2783`) checks the receiver's own level *before* the base
  chain, and an interface's own readable property has a public getter. (Confirmed
  by the `IsReadOnly` probe below.)
- `x.InheritedInterfaceData` — **defers**: `base_chain` of an interface is
  `Complete([iface])` (interfaces have `base_type: None`), so the walk over the
  bases is empty.
- `x.AnyMethod()` — **defers**: `method_group` (`assembly_env.rs:3032`) bails
  outright on `EntityKind::Interface` (`:3047`), so even an *own* interface method
  defers.
- `x.GetHashCode()` / `.ToString()` / `.Equals(o)` / `.GetType()` (`Object`
  members) — **defers**: an interface has no `base_type`, so `Object`'s members
  are invisible to `base_chain`.

## FCS ground-truth (`fcs-dump types`, verified 2026-07-17)

Non-generic BCL interfaces only (so `Ty::Named`'s no-generic-args limit does not
bite the receiver):

| Snippet | FCS `TypeCanon` | Exercises |
|---|---|---|
| `(e: System.Collections.IEnumerable).GetHashCode()` | `System.Int32` | `Object` method on iface receiver |
| `(e: System.Collections.IEnumerable).GetEnumerator()` | `System.Collections.IEnumerator` | own iface method |
| `(l: System.Collections.IList).Count` | `System.Int32` | inherited-iface data (IList→ICollection) |
| `(l: System.Collections.IList).IsReadOnly` | `System.Boolean` | own iface property |
| `(l: System.Collections.IList).GetEnumerator()` | `System.Collections.IEnumerator` | inherited 2 levels (IList→…→IEnumerable) |
| `(d: System.IDisposable).GetHashCode()` | `System.Int32` | `Object` method on iface receiver |

All six type as `call:instance` / a property read in FCS's reduced tree. All but
`IsReadOnly` defer on our side today.

## The primitive: `interface_member_chain`

Analogous to `base_chain`, but over the interface DAG. Produces the member-source
levels for an interface receiver, **nearest-first, deduplicated by handle** (a
diamond `IDerived : IA, IB` with `IA, IB : IBase` visits `IBase` once):

```
enum InterfaceChain {
    /// All transitively-inherited interfaces resolvable AND `System.Object`
    /// present. `levels` = receiver iface, its interfaces (transitive), then
    /// `System.Object` last (the Object-members source).
    Complete(Vec<EntityHandle>),
    /// All inherited interfaces resolvable, but `System.Object` absent from the
    /// env (single-assembly view). `levels` excludes Object. A data lookup is
    /// still complete (Object declares no data members); a method call naming an
    /// Object method must defer.
    ObjectCapped(Vec<EntityHandle>),
    /// Some transitively-inherited interface is unresolvable (generic — the
    /// common `ICollection<T> : IEnumerable<T>` — nested, absent, wrong-assembly)
    /// ⇒ the member surface is unknowable ⇒ defer everything.
    Incomplete,
}
```

Built by DFS over each entity's `interfaces: Vec<TypeRef>`, resolving each edge
with the existing `resolve_base` (non-generic, top-level, present, assembly-name
matched; declines otherwise → `Incomplete`). DFS-from-receiver visits a derived
interface before the base it extends, so the linearization is a valid nearest-first
order. `System.Object` resolved via the same `is_system_object` / lookup path
`base_chain` uses. Bounded against metadata cycles by a visited set.

**Coverage honesty (D5):** generic inherited interfaces are pervasive, so many
interface receivers will land in `Incomplete` and defer. That is the correct
silent outcome — `Ty::Named` carries no generic-argument list yet, so we could not
render `IEnumerable<T>`'s members soundly regardless. A later slice that teaches
`Ty` generic arguments lifts this; until then, the *non-generic* interface surface
(and Object) is what resolves.

## Soundness — why the interface DAG is not a base-class chain

The 3.x-inh `method_group` dedup (OV-3, `assembly_env.rs:3135`) drops a member
whose partial signature a **strictly nearer** level already claimed. That is sound
for a base-*class* chain because every nearer level *is* a subtype of every farther
one, so nearer-hides-farther is always a genuine override/hide. **It is NOT sound
across an interface DAG:** two *sibling* interfaces `IA, IB` (neither a supertype
of the other) may both declare `M(int)`; FCS does **not** hide one by the other
(hiding is only supertype→subtype), so collapsing them would resolve — possibly to
the wrong member — where FCS sees an ambiguity.

**v1 rule (conservative, sound): no cross-level hiding for interface receivers.**
Require the name be declared on **exactly one** level of the closure (own or
inherited) → resolve; **≥ 2 declaring levels → defer** (single-candidate) or feed
the whole set to the overload engine (which will itself defer a genuine ambiguity).
This defers the rare *re-declaration* case (`IDerived` re-declares `IBase.M`, which
FCS hides down to one) — sound but incomplete — while never collapsing unrelated
siblings. Recovering re-declaration hiding along genuine subtype edges (via
`super_types`) is the optional IW-3 refinement, gated on the corpus showing it
matters.

## Stages

Each stage its own branch, stacked, with an FCS differential (the `fcs-dump types`
oracle) that iterates **our** inferred types and asserts FCS agrees at the range
(the D5 soundness direction — we never over-claim). New cases go in
`crates/sema/tests/all/infer_member_access_diff.rs` (the 3.3a/3.3d harness, which
already builds an `AssemblyEnv` over the real BCL `System.Runtime.dll`).

- **IW-0 — the primitive. ✅ landed.** `InterfaceChain` + `interface_member_chain`,
  with synthetic-entity unit tests (a diamond that dedups the shared base and
  appends Object, a generic-inherited-interface → `Incomplete`, an Object-capped
  single-assembly env, and an impostor-`System.Object` rejection — the codex IW-P2
  soundness guard). The Object slot is trusted only when it is the unique base-less
  class, else the chain caps.
- **IW-1 — interface data members. ✅ landed.** `instance_data_member` walks
  `interface_member_chain` for an interface receiver (own level first, then the
  exactly-one-declaring-level gate over inherited levels); `Incomplete` → defer.
  Differential: `(l:IList).Count` (inherited), `(l:IList).IsReadOnly` (own);
  synthetic sibling-ambiguity defer + single-inherited-resolves control.
- **IW-2 — interface methods. ✅ landed.** `method_group` dispatches an interface
  receiver to `interface_method_group` (Object appended for `Complete`; `Object`
  method name on `ObjectCapped` → defer), **no cross-level dedup** (v1 rule above),
  exactly-one-declaring-level. Unlocks `instance_method` (single-candidate typing)
  and feeds the OV engine for interface receivers; static interface calls stay
  deferred (non-goal). Differential: `GetHashCode()` / own `GetEnumerator()` /
  inherited `GetEnumerator()`.
- **IW-3 — refinements (optional, data-gated). ⬜ outstanding.** (a) re-declaration hiding along
  `super_types` edges to recover the single-candidate cases IW-1/IW-2 defer;
  (b) `qualified_path_occupied` (`assembly_env.rs:2706`) precision — replace the
  conservative `return true` on an interface root with a precise membership probe
  over the closure, so an interface-rooted qualified path with a genuinely-absent
  member falls through instead of deferring; (c) inherited-member dot-completion
  for interface receivers (ties into the separate "inherited dot-completion" pile).

## Non-goals

- Class/struct receivers walking their interfaces — FCS explicitly does not
  (`followInterfaces=false`); adding it would be *wrong*.
- Generic interfaces (receiver or inherited) — deferred until `Ty` carries generic
  arguments; `Incomplete` covers them silently.
- Default-interface-method resolution on a *class* receiver — a DIM is reached
  through the class's own/interface-map, a distinct lookup; out of scope.
- Static abstract/virtual interface members — different lookup rules; out of scope
  (the static method group keeps its interface-receiver conservatism).
