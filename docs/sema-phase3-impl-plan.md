# Sema Phase 3 implementation plan — best-effort inference

> **Status:** Phases 3.1–3.3 and 3.x-inh are complete; overload resolution has
> landed through OV-7 and OV-9, with OV-8 (betterness, optional) the only
> outstanding overload slice. This is the implementation breakdown for Phase 3
> of [`type-checker-plan.md`](type-checker-plan.md) (best-effort type inference:
> hover + dot-completion), mirroring
> [`completed/sema-phase1-impl-plan.md`](completed/sema-phase1-impl-plan.md).
> Everything below the "Landed stages" list has detail *only* on what remains.

## Substrate (reference)

Per [D8](type-checker-plan.md#d8-inference-substrate-ena-worklist-not-smt),
inference is **generate → solve**: generation is a pure fold over the resolved
AST producing an inert `Vec<Constraint>`; the solver discharges equality
constraints by union-find (`ena`), with type-directed member lookup (`expr.Foo`)
and function application deferred to *suspended* constraints woken when a
receiver / callee type becomes concrete. `infer_file` only ever emits **ground**
types (`Ty::is_ground`), so an inference `Ty::Var` never reaches a consumer — the
D5 "say nothing when unsure" contract is enforced at read-off. Each new stage
lands on its own branch, stacked as necessary, so a reviewer can review each in
isolation.

## Landed stages (one line each)

- **3.1** (PR #633) — `Ty` + sound literal typing.
- **3.2a** (PR #643) — unification substrate: `ena` + `Ty::Var` + the
  generate→solve plumbing, with literal typing re-routed through it (output
  byte-identical to 3.1).
- **3.2b-1** (PR #646; hover surfacing #650) — value-reference propagation: a
  binder's type flows down an unannotated simple-name `let` chain, typed only in
  the coercion-free RHS position.
- **3.2b-2** (PR #663) — paren transparency + reference tuples (new `Ty::Tuple`).
- **R1** (PR #668) — resolver records `Deferred(ShadowableType)` at uncertain
  type-position defers, so "no record" is a reliable no-shadow-possible signal.
- **3.2b-3 / R2** (#851–#864, 2026-07-08) — annotated-binder typing gated on R1's
  no-shadow signal, per
  [`r2-annotation-typing-plan.md`](completed/r2-annotation-typing-plan.md) (stages
  R2-0–R2-e).
- **3.2c-1** (PR #678) — the bidirectional recursive typer
  (`infer_expr(e, expected)`, synth/check modes) + `if`/`then`/`else`.
- **3.2c-2a** (PR #685) — function/lambda/`while` body traversal carrying the
  correct bidirectional mode (the function *value* still defers).
- **3.2c-2b** (PR #701) — `Ty::Fun` + monomorphic function-type emission + sound
  condition typing via private per-parameter slots; validated by a new
  `fcs-dump binder-types` oracle.
- **3.2c-2c** — `let`-generalisation + instantiation + canonical typar rendering
  (`let f x = x` ⇒ `'a -> 'a`), built on per-binding walk-completeness, the
  check-mode poison set, per-binding Algorithm-W solve, and the new
  `Ty::Param` variant.
- **3.2c-3** — function application v1 (`Expr::App`, no worklist): a ground callee
  grounds the result; polymorphic / ill-typed calls defer (poisoned).
- **3.3a** — the suspended-constraint worklist + `HasMember` typing for fields /
  non-indexer readable properties on a non-generic `Ty::Named` receiver via
  `AssemblyEnv`.
- **3.3b** — LSP member-resolution enrichment (`InferredFile::member_resolutions`
  in the resolver's `Resolution::Member` shape) feeding hover / go-to-def, plus a
  new dot-completion handler.
- **3.3c** — the application wake rule (suspended `ArgCheck`, coercion-free
  domains), closing 3.2c-3's polymorphic gap with completeness-gated,
  deferred-poison discharge.
- **3.3d** — single-candidate method-call typing (`recv.Method(args)` ⇒ the
  method's return type) behind a well-formedness / arity gate; overloaded,
  generic, static, and void methods defer.
- **3.x-inh** — member inheritance (base-class walk) for the data-member and
  method wakes, single-candidate across the whole chain, honouring name-hiding
  and assembly-name identity.
- **Overloads OV-0–OV-7, OV-9** (#872–#899 and follow-ups) — the research probes,
  the `overloads` oracle, assembly flags, cross-assembly dedup, the speculation
  API, the applicability matcher, the instance-call engine with its
  extension-absence gate, OV-6.1 curry detection, OV-7 static calls, and OV-9's
  generator + corpus differential; per
  [`overload-resolution-plan.md`](overload-resolution-plan.md).

---

## Still to do

### Overload resolution — OV-8 (betterness, optional)

The one outstanding overload slice; fully specced in
[`overload-resolution-plan.md`](overload-resolution-plan.md) (§2.5, §6 stage
OV-8). It models FCS's betterness pass to commit calls where several candidates
are applicable but one is strictly better. It is **optional and data-gated**:
OV-9's corpus differential measured how many real calls betterness would recover
(the plan records the current commit rate and the ceiling OV-8 could add), and
that measurement is the input to the go/no-go decision. Read the overload plan's
status block and §6 OV-8 entry before starting.

Read [`overload-resolution-plan.md`](overload-resolution-plan.md) before touching
this pile: it carries the FCS algorithm with citations, the empirical landmine
catalogue (type-directed widening, `op_Implicit`, params/optional arity,
extension-vs-intrinsic betterness), and the two-sided `must_apply`/`may_apply`
sound commit rule.

### Remaining 3.x hard piles (census-priority order)

Each stays `Deferred` (D5 silence) until the corpus demands it
([D9](type-checker-plan.md#d9-scoping-evidence-the-bucket-census)); none has a
detailed sub-plan yet, so writing one is the first step of each:

- **Interface walk** — member resolution through implemented interfaces (the
  base-class walk landed in 3.x-inh covers base *classes* only). Also a
  prerequisite for the *complete* method group overload resolution needs: arity
  is not a sound overload proxy, so the real method group must include base-class
  **and** interface members plus argument-type matching (this is why 3.x-inh was
  sequenced ahead of the sound overload slice). **IW-0–IW-2 landed** (interface
  data members + methods + Object for an interface-typed receiver, via the new
  `interface_member_chain`; class/struct receivers unchanged — FCS does not walk
  their interfaces); **IW-3 (re-declaration hiding, qualified-path precision,
  inherited dot-completion) is the optional remainder.** Sub-plan (scope, the
  interface-DAG soundness rule, and the IW-0–IW-3 staging):
  [`interface-walk-plan.md`](interface-walk-plan.md).
- **Extension members** — currently out of scope for all of 3.3.
- **SRTP** (statically-resolved type parameters).
- **Computation-expression desugaring.**
- **Units of measure.**
- **Dot-completion of inherited members** — completion is still exact-entity
  only and does not offer members reached via the 3.x-inh base-class walk.
