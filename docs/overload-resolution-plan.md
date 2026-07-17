# Overload resolution plan — sema Phase 3.x

> **Status:** OV-0, OV-0.5, OV-1–OV-7, and OV-9 are **done** (the batch
> #872–#899 and follow-ups, 2026-07-08…07-12); **OV-8 (betterness) is the only
> outstanding slice** and is optional/data-gated. Everything below the "Landed
> stages" list has detail *only* on OV-8 and the still-open follow-ups; §1–§5
> and §6.1 are retained as condensed **reference** (the FCS algorithm with
> citations, the landmine catalogue, and the two-sided commit rule), because
> code comments and [`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md) cite
> their section numbers (§2.*, §3.1, §4.*, §6.1(a)/(b)). Do not re-derive these
> semantics from memory; when in doubt, re-probe through the `fcs-dump`
> `overloads`/`types`/`uses` oracles.
>
> **OV-9's measurement is the input to the OV-8 go/no-go** (§6, OV-9 entry):
> over the generated matrix we commit **41.5 %** of the calls FCS cleanly
> resolves; betterness would address at most **39.9 %** more; the `must_apply`
> affirmation gap another **18.6 %** (which OV-8 does *not* recover). Real-corpus
> coverage is gated not by either, but by OV-6's extension-**presence** gate,
> which any project referencing FSharp.Core trips — its verified-enumeration
> refinement (now [`extension-scope-enumeration-plan.md`](extension-scope-enumeration-plan.md))
> is the higher-value next slice, and OV-9's differential is what makes both
> safe to attempt.

This is the de-risked design for the one inference hard pile the census says is
worth investing in
([type-checker-plan.md D9](type-checker-plan.md#d9-scoping-evidence-the-bucket-census):
overloaded calls are ≈ 97 % of the type-axis hard bucket). It exists because the
first attempt — the arity-unique shortcut — was abandoned after four `codex`
rounds (see
[`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md#remaining-3x-hard-piles-census-priority-order)),
and the failure had a *structural* cause named below. Everything here was
verified against the FCS source checkout (`../fsharp`, F# 10-era main; all `FCS:`
citations are paths under `src/Compiler/`) and against **17 empirical probes**
(§3) run through the real oracles.

## 1. Why the shortcut failed, and the keystone commit rule (reference)

The arity shortcut used **one approximate test for two jobs with opposite
soundness requirements**. To commit "candidate `c` wins" you must prove two
different things: `c` **is** applicable in FCS's judgment — an
**under**-approximation (everything we affirm, FCS affirms); and every other
candidate **is not** — an **over**-approximation (everything we reject, FCS
rejects). Arity-matching is neither (it over-affirms a wrong-typed same-arity
candidate and over-rejects optional/`params`/`out` shapes).

**The commit rule this plan is built on** (the keystone; every stage serves it):

> For a call with candidate group `G` and chosen `c* ∈ G`, commit iff
> **(a)** the group is provably complete (§4.1) — every candidate FCS sees,
> including inherited and in-scope extension members;
> **(b)** `must_apply(c*)` holds (`must_apply` under-approximates FCS
> applicability, §4.3); and
> **(c)** for every other `c ∈ G`, `may_apply(c)` is false (`may_apply`
> over-approximates FCS applicability, §4.2).
> Then exactly one candidate is FCS-applicable, so FCS picks `c*` and
> **betterness never runs** — none of its 14 rules need modelling.
> Anything else defers (D5 silence).

This is the ground/generalise asymmetry transplanted to candidate sets:
`must_apply` is the "sound under a subset" direction; `may_apply`-refutation is
the direction that needs completeness. Betterness (needed only when ≥ 2
candidates are FCS-applicable) is the *later, optional* OV-8 (§6); nothing in v1
depends on it.

## 2. FCS's algorithm (the reference; condensed, with citations)

### 2.1 Method group construction

For `recv.M(...)` (FCS: `NameResolution.fs:2779` `ResolveLongIdentInTypePrim`,
`IgnoreOverrides`):

- **Intrinsic walk** (`InfoReader.fs:455–481`, `TypeHierarchy.fs:232–307`):
  class/struct receiver → superclass chain only, **implemented interfaces do NOT
  contribute** (`TypeHierarchy.fs:306–307`, `followInterfaces=false`); interface
  receiver → `System.Object`'s members **plus** all transitively inherited
  interfaces (`TypeHierarchy.fs:256–260`). Accessibility filtered at enumeration
  (`InfoReader.fs:94`).
- **Kind hiding** (`InfoReader.fs:465–477`): the most-derived level declaring the
  name decides the *kind* wholesale (a derived **property** `M` hides inherited
  **methods** `M`, and vice versa).
- **Override dedup** (`InfoReader.fs:574–655`, under `IgnoreOverrides`): a virtual
  re-decl in a subtype is dropped iff signature-equivalent to a supertype virtual
  — `MethInfosEquivByNameAndSig` (`infos.fs:2544–2551`): name + generic arity +
  parameter types (byref kinds collapsed, `infos.fs:2527–2528`) + **return
  type**; unless subtype method is newslot/final/abstract/non-virtual.
- **Hiding of inherited overloads** (`InfoReader.fs:713–722`): a supertype method
  is dropped iff a subtype method matches by `MethInfosEquivByNameAndPartialSig`
  (`infos.fs:2532–2534`) — name + generic arity + parameter types, **return type
  ignored**. So hiding is *by parameter signature, not by name*: `Derived.M(int)`
  does **not** hide `Base.M(string)` (C# hides by name; F# does not).
- **Extension members** (`NameResolution.fs:730–755, 2874–2880`): all in-scope
  extension members (F#-style `type T with …` and C#-style `[<Extension>]`)
  compete **in one flat group**. Their lower priority is *only* betterness rule
  10 (§2.5); an applicable extension **can beat an applicable intrinsic** on any
  earlier rule (probe P15). **The single most dangerous fact in this document.**

### 2.2 Candidate normalisation (`CalledMeth`, `MethodCalls.fs:534–731`)

- Named caller args matched by exact name; unmatched named args may resolve as
  **property/field setters on the return type** (`MethodCalls.fs:578–620`).
- **Trailing optional/`out` trimming** (`MethodCalls.fs:638–653`): fewer args than
  declared may omit trailing params iff each is optional xor `out`; one violation
  disables all trimming. Omitted `out` folds into a **tuple return**
  (`MethodCalls.fs:772–779`).
- **Param-array split** (`MethodCalls.fs:657–691`): a trailing 1-D `[<ParamArray>]`
  admits surplus args; a `params` method enters as **two** candidates — expanded
  and direct-array (`CheckExpressions.fs:10229–10234`).
- `IsCandidate` (`MethodCalls.fs:846–850`) = accessible ∧ correct arity
  (post-normalisation) ∧ correct obj args ∧ all named args assigned.
- **Single-`IsCandidate` shortcut** (`ConstraintSolver.fs:3613–3632`): if exactly
  one candidate survives this **arity-based** pre-filter, it commits **with no
  applicability test** — wrong argument *types* surface as errors later but the
  call still elaborates with that member (why 3.3d's correct-arity/wrong-type
  calls type as the return; and why an elaborated `Call` node ≠ a resolved call,
  the load-bearing OV-9 finding).
- Curried (multi-arg-group) members: overloading between them is an error
  (`CheckExpressions.fs:10395–10403`).

### 2.3 Applicability (`ConstraintSolver.fs:3477–3544`)

Two speculative passes, each candidate under a fresh undo trace
(`FilterEachThenUndo`, `ConstraintSolver.fs:503–511` — mirrored by our `ena`
snapshot/rollback): **Pass A — exact match** (args `typeEquiv` the adjusted param
types, no subsumption, `ConstraintSolver.fs:3504–3526`; a unique exact match wins,
betterness skipped); **Pass B — applicable set** (must-subsume with real
unification `SolveTypeSubsumesType`, `ConstraintSolver.fs:3231–3291`; generic
method type args inferred by unification against freshened typars,
`:3091–3094`). The winner's trace is **replayed** into real solver state
(`:3705–3708`) — applicability testing has side effects on caller-side inference
variables that outlive resolution.

### 2.4 Type-directed conversions (F# 6+) — applied DURING applicability

`AdjustCalledArgType`/`AdjustRequiredTypeForTypeDirectedConversions`
(`MethodCalls.fs:449–470, 259–318`), per-argument: (1) delegate ← function /
`Expression<D>` ← function, ungated; (2) built-in widenings gated on
`LanguageFeature.AdditionalTypeDirectedConversions` — `int64`/`nativeint`/`float`
← `int32` (`MethodCalls.fs:288–297`); (3) `Nullable<T>` ← `T` (F# 5); (4)
**`op_Implicit`** (`MethodCalls.fs:176–230`) when no feasible subtype relation and
either side declares a static non-generic `op_Implicit` with `typeEquiv`
param/return (how a bare int literal reaches a `decimal` parameter, P10). So an
argument's ground type does **not** bound the applicable set: `M(float)` is
applicable to an `int32` arg.

### 2.5 Betterness (`ConstraintSolver.fs:3750–3960`) — v1/OV-8 territory

For ≥ 2 applicable candidates, a 14-rule ladder (first nonzero wins): no-TDC,
non-two-step TDC, nullable-only TDC, fewer "less generic" warnings,
non-params-expanded, more-specific params element, no out args, no defaulted
optionals, pairwise most-specific param types under *feasible*-subsumption
(Pareto), **intrinsic over extension** (rule 10), extension priority by most
recent `open` (rule 11), non-generic (rule 12), F# 5 all-args repeat, and a
property-setter tiebreak. The winner must be strictly better than **every** other
(`:3922–3931`); else ambiguity error. v1 never reaches this — it commits only
when one candidate is applicable at all.

### 2.6 Failure

Zero applicable, or a betterness tie ⇒ error; `ResolveOverloading` has **no `obj`
fallback** — the `obj`-typed node on failed calls (P7) is IDE recovery above the
checker. Symmetric for us: any call FCS errors on, we must *defer*, never type.

## 3. Empirical probe catalogue (reference)

Run 2026-07-06 through `fcs-dump` (`types`/`uses`); each is a regression the OV-1
differential encodes. "⇒ T" = the call node's `TypeCanon`.

| # | Snippet (essence) | FCS verdict | Rule |
|---|---|---|---|
| P1 | `M(float)/M(string)`, call `M(3)` | picks `M(float)` — widening ⇒ `Int32` | 2.4(2) |
| P2 | `V(params int[])/V(string)`: `V(1,2)`⇒int; **`V(7)`⇒int**; `V("x")`⇒string | params form applicable at arity 1 | 2.2 |
| P3 | `M(int, [<Optional;DPV 0>] int)/M(string)`, call `M(1)` | picks optional overload ⇒ `Double` | 2.2 |
| P4 | `"x".ToString()` / `s.Substring(1)` | `call:instance` / `-overloaded`, both ⇒ `String` | 2.1, 3.1 |
| P5 | `M(obj)/M(string)`: `M("hi")`⇒string; `M(3)`⇒int | most-specific; **int boxes to `obj` = applicable** | 2.3, 2.5(9) |
| P6 | `(e: IEnumerable<int>).GetEnumerator()`; `(d: IDisposable).GetHashCode()` | both type — interface receivers get inherited-interface **and `Object`** | 2.1 |
| P7 | `M(int64)/M(float)`, call `M(3)` | ambiguous (both widen) ⇒ error; recovery node `Object` | 2.5, 2.6 |
| P8 | `System.Math.Abs(3)`, `String.Compare("a","b")` | `call:static-overloaded` — statics share machinery | — |
| P9 | extension `String.Twice()` via `open` | `call:extension` ⇒ types fine | 2.1 |
| P10 | `M(int64)/M(string)`⇒int64 (widening); `M(decimal)/M(string)`⇒decimal via **`op_Implicit`** (nested conversion node) | 2.4(2),(4) |
| P11 | `M(obj)/M('T list)`: `M("hi")`⇒obj-overload; `M([1])`⇒**generic wins** | generic candidates compete fully | 2.3 |
| P12 | `Int32.TryParse "3"` | `call:static-overloaded` **with `System.Int32&` byref param** in all forms (the §5 byref defer trigger); `s.StartsWith "h"`⇒Boolean | 2.2 |
| P13 | `s.ToString()`/`s.GetHashCode()`⇒`call:instance`; `s.Equals("y")`⇒overloaded | 3.1 |
| P14 | `s.ToString(CultureInfo.InvariantCulture)` | `call:instance-overloaded` ⇒ `String` | 3.1 |
| P15 | intrinsic `M(obj)` vs **extension** `M(string)`, call `M("hi")` | **extension wins** (⇒ Double) — more specific at rule 9, before rule 10 | 2.1, 2.5 |
| P16 | named args `M(a = 1, b = "x")` | resolves fine (⇒ Double) — we defer named args, sound | 2.2 |
| P17 | intrinsic `M(string)` vs equally-specific extension `M(string)` | **intrinsic wins** (⇒ Int32) — rule 10 | 2.5 |

### 3.1 Oracle quirks (baked into OV-1)

Two facts the differential must honour: (1) **`isOverloadedMember` undercounts**
— `s.ToString()`/`s.GetHashCode()` classify as plain `call:instance` because the
elaborated `Call` node's `mfv` is the base virtual slot (`Object.ToString`), so
the `types` `Kind` **must not gate** the differential (compare **by signature**,
run the engine on every call). (2) The elaborated `mfv` can be
**override-retargeted**, so compare by `XmlDocSig` + parameter types, tolerating a
declaring entity that is a base of the one we record. The 2026-07-06 "out-arg
calls vanish" claim (P12) did **not** reproduce on net10 — a `System.Int32&`
byref param (not a missing node) is what defers out-arg calls; keep the "don't
require a `Call` node per source call" robustness rule anyway.

## 4. The sound v1: "unique applicable candidate" (reference; landed in OV-5/OV-6)

Scope: instance calls `recv.M(args)` (both 3.3d parse shapes) where
`instance_method` deferred "overloaded"; statics as OV-7. Extends the existing
`HasMember { kind: Method }` wake; the 3.3d call-site gates stay. Implemented in
[`crates/sema/src/overload.rs`](../crates/sema/src/overload.rs) and
[`assembly_env.rs`](../crates/sema/src/assembly_env.rs).

### 4.1 Group-completeness gate

Commit requires seeing every candidate FCS sees: (1) **chain** `Complete` or
`ObjectCapped` with `System.Object` resolvable, no skipped members of the name;
(2) **kind hiding** per §2.1 (owning-level rule); (3) **hiding + override dedup**
across levels — needs cross-assembly `TypeRef` identity (OV-3) and metadata
`newslot`/`final` (OV-2); (4) **extension absence** (the P15 landmine) — no
in-scope extension member of the name may exist, tested by *presence* not
enumeration (OV-6): the pickle-name–derived extension-member index (OV-0.5),
C#-style `[<Extension>]` attribute-read, in-project augmentations, and any opaque
`open`/auto-open surface all defer wholesale; (5) **accessibility** public-only
(`InternalsVisibleTo` unmodelled, sound because FCS filters at enumeration).

### 4.2 `may_apply` — the over-approximation (candidate elimination)

`may_apply(c)` must be **true whenever FCS finds `c` applicable**. Eliminate only
by positive proof down one of two prongs. **Arity prong:** window `[min, max]`
from metadata — `min` = params before the maximal trailing (optional xor `out`)
run, excluding a trailing `[<ParamArray>]`; `max` = ∞ if a trailing params array,
else the param count; eliminate iff caller count outside the window
(over-approximation stays generous — a param that is both optional and out breaks
the run, §2.2, but the *window* ignores that). **Type prong**, per unnamed
position (aligning both params expansions): eliminate iff **no conversion channel
can exist** from ground `A` to `P`, decidable only when both are in the **closed
decidable set** (sealed BCL primitives + 1-D arrays of them). Channels ruled out
mechanically: identity, subsumption (`P` sealed non-interface ⇒ only `A = P`), the
built-in widening table (§2.4), `op_Implicit` (finite table over the closed set —
`Decimal`), delegate/`Nullable`/auto-quoting (never for closed `P`). If `P` is
`obj`/interface/non-sealed named/typar/byref, the position cannot eliminate. A
candidate with no eliminating position and an in-window arity stays
`may_apply = true` — the honest price of soundness, measured in OV-9.

### 4.3 `must_apply` — the under-approximation (winner affirmation)

`must_apply(c*)` must imply FCS applicability: every arg type ground; caller count
in the strict form (exact for direct; ≥ `count-1` for expanded params with
surplus checked against the element type); per arg `A typeEquiv P` (OV-3
canonicaliser) **or** provable subsumption `A :> P` (base-chain/interface closure,
OV-5) — no TDC channel is *needed* (a widening/`op_Implicit`-only candidate fails
`must_apply` and defers, sound); no named args, no omitted optionals/outs in the
affirmed shape; obj-arg subsumption by construction; candidate non-generic,
non-curried (one metadata param group), return bridgeable or void-recorded.

### 4.4 Commit effects

As 3.3d's single-candidate wake: unify `result` with the bridged return type,
record `Resolution::Member` at the method-name range (recording the
**metadata-declaring entity**; differential compares by signature per §3.1), do
**not** discharge argument-side constraints (check-mode walk + poison stay — FCS's
trace-replay side effects are deliberately under-modelled). `may_apply`/`must_apply`
are pure over ground types/metadata, but OV-4's snapshot API exists for OV-8 and
generic-argument inference.

## 5. What stays deferred (reference; each sound, with its trigger)

Non-ground argument types; named args; omitted optional/out on the *winner*;
curried members; generic *winners* (generic *losers* fine if arity-eliminated);
op-conversion resolution and SRTP-driven resolution (`MethodCalls.fs:454–455`);
property setters via named args; byref/inref/outref anywhere in the winner
(`params` *winner* in expanded form allowed, P2, element type in the closed set);
interface-typed receivers (group rules differ, §2.1; `Ty` never produces one
anyway); receivers/args with generic instantiations (`Ty::Named` has no args yet);
langversion — the widening/`op_Implicit` channels are F# 6-gated but assumed ON
(mis-assuming can only cause deferral, never wrongness — `must_apply` never relies
on them).

## 6. Stages

Each stage was its own branch with its own oracle (oracle first, then
infrastructure, then engine).

### Landed stages (one line each)

- **OV-0** — research probes (2026-07-08; findings §6.1). Verdict:
  `is_extension_method` has false negatives even on the authoritative pickle
  path, so it is **not** usable as the absence gate's no-false-negative signal —
  hence the new prerequisite OV-0.5. C#-style `[<Extension>]` (attribute-read) is
  the one trustworthy per-method channel.
- **OV-0.5** — F#-native extension-member name index (2026-07-09).
  [`Entity::extension_member_names`](../crates/assembly/src/model.rs) read from the
  pickle's `ValFlags.IsExtensionMember ∧ IsInstance` bit *before any IL-method
  matching* ([`apply_extension_member_index`](../crates/assembly/src/fsharp_pickle_merge.rs)),
  so it carries none of the per-method flag's false negatives; surfaced as
  [`AssemblyEnv::module_extension_members`](../crates/sema/src/assembly_env.rs) →
  `ExtensionMembers::Known`/`Unknowable` (the `Unknowable` fallback wired to the
  same per-assembly signal as `AbbreviationVisibility`). Blocks OV-6.
- **OV-1** — the `overloads` oracle + probe regression corpus (2026-07-08).
  `fcs-dump overloads` ([`collectOverloads`/`dumpOverloads`](../tools/fcs-dump/Program.fs))
  + Rust [`parse_fcs_overloads`](../crates/sema/tests/all/common/mod.rs) + all 17 §3
  probes in [`overloads_oracle.rs`](../crates/sema/tests/all/overloads_oracle.rs).
  Range-keyed, invocation-node-only, compares by `XmlDocSig`, never gates on `Kind`.
- **OV-2** — assembly-model flags (2026-07-08). `Entity::is_sealed`,
  `MethodLike::{is_final, is_newslot, is_hide_by_sig}` from the TypeDef/MethodDef
  flag words, pinned against a controlled C# IL fixture
  ([`projector_overload_flags.rs`](../crates/assembly/tests/all/projector_overload_flags.rs))
  — raw IL bits, so pinned against C# not diffed against FCS's pickle view.
- **OV-3** — cross-assembly `TypeRef` identity + method-group dedup (2026-07-08).
  [`type_sig_key`](../crates/sema/src/assembly_env.rs) canonicaliser + partial-sig
  "nearest-wins" dedup (`method_partial_key`), observably equivalent to FCS's
  fuller hiding+override machinery for single-candidate typing; an overridden
  single method now resolves, a genuine cross-level overload split still defers.
- **OV-4** — `InferTable` scoped speculation API (2026-07-09).
  [`InferTable::probe`](../crates/sema/src/unify.rs), LIFO-nestable snapshot/commit;
  property tests `probe_rollback_is_identity`, `probe_commit_equals_direct`,
  `probe_nests_lifo`. Not needed by v1's pure gates but OV-8/generics need it.
- **OV-5** — the applicability matcher (2026-07-09). [`arity_window`],
  `may_apply`/`must_apply` per §4.2–4.3 in
  [`crates/sema/src/overload.rs`](../crates/sema/src/overload.rs), over a `ClosedTy`
  decidable set, with the subtype test (`super_types`) and `op_Implicit` refuter.
  Property-tested: `must_apply ⟹ may_apply`, identity/widening never eliminated,
  affirmation only at in-window arity.
- **OV-6** — the instance-call engine slice (2026-07-09).
  [`AssemblyEnv::instance_method_group`] surfaces the deduped group, the
  presence-based extension-absence gate ([`ExtensionScope`](../crates/sema/src/infer.rs))
  must pass, then the single-candidate arity shortcut (§2.2) or `resolve_overload`
  (the keystone) commits. Byref/out declines, interface receivers defer, every arg
  element check-walked for poison, not-yet-ground args retry. **OV-6.1**
  ([`completed/ov-6.1-curry-detection-plan.md`](completed/ov-6.1-curry-detection-plan.md))
  resolved the curry hole at assembly granularity — [`MethodLike::arg_group_count`]`= Some(1)`
  for C#/VB, `None` for any F# assembly (keyed on
  `FSharpInterfaceDataVersionAttribute`); defers a call when any candidate has ≥ 2
  params and is not provably `Some(1)`, restoring multi-param C# commits while
  deferring genuine F# curried members.
- **OV-7** — statics (2026-07-10).
  [`AssemblyEnv::static_method_group`](../crates/sema/src/assembly_env.rs) is the
  `instance_method_group` sibling over one shared `method_group(want_static)` walk;
  generation via [`Gen::static_callee`](../crates/sema/src/infer.rs). Four GPT-5.6
  review rounds landed: direct-unit-syntax `M()` reading, an inheritance-aware
  kind-agnostic path-ownership predicate
  ([`type_qualified_member_possible`](../crates/sema/src/assembly_env.rs)),
  interface-rooted ownership, and a documented parenthesised-callee decline.
  Differentials in [`infer_static_call_diff.rs`](../crates/sema/tests/all/infer_static_call_diff.rs).
- **OV-9** — the generator + corpus differential (2026-07-12).
  [`overload_corpus.rs`](../crates/sema/tests/all/common/overload_corpus.rs)
  generates one universe in two views (a C# assembly of 55 two-candidate sets +
  landmine shapes, referenced by both FCS and our `AssemblyEnv`, and an F#
  call-site matrix of 2584 sites);
  [`overload_corpus_diff.rs`](../crates/sema/tests/all/overload_corpus_diff.rs)
  asserts both directions of the keystone (we-commit ⇒ FCS-same-overload;
  FCS-resolved ⇒ `may_apply` does not refute FCS's choice), the D5 net, and two
  commit floors (`MIN_COMMITS` 380, `MIN_OVERLOAD_SET_COMMITS` 250 through genuine
  ≥ 2-candidate groups). **Load-bearing finding:** an elaborated `Call` node does
  **not** mean FCS *resolved* the call (the arity-based single-`IsCandidate`
  shortcut, §2.2, fires even inside a multi-candidate group), so `fcs-dump
  overloads` also emits **`Errors`** and the applicability direction is asserted
  only on cleanly-resolved sites. **Result: 0 violations over 2584 sites.** The
  coverage numbers (`coverage_report`, `#[ignore]`d): over 646 cleanly-resolved
  calls we commit **41.5 %**; OV-8 addresses at most **39.9 %** (≥ 2 `may_apply`
  survivors); the affirmation gap is **18.6 %** (OV-8 does *not* recover these);
  `GroupIncomplete`/`PossiblyCurried`/`NoSurvivor` are all **0** (observed, not
  assumed). Caveat: the matrix over-samples `obj`/class params, so 39.9 % is an
  upper bound on betterness's real-world value.

---

### Still to do

#### OV-8 — betterness (optional, data-gated)

The one outstanding overload slice, and **optional**: OV-9 measured its ceiling
(≤ 39.9 % of cleanly-resolved calls, on a matrix that over-samples the shapes it
helps), and that measurement is the go/no-go input. Model **Pass A** (unique
exact match, §2.3) and the **betterness ladder** (§2.5) over the decidable subset
— rules 1, 5, 9, 10, 12 are decidable for closed-set signatures;
feasible-subsumes on ground closed types is the OV-5 subtype test. Commit a
multi-applicable group only when **every pairwise comparison is decidable and a
strict winner exists** (`ConstraintSolver.fs:3922–3931`); otherwise defer. Each
rule only shrinks the deferred set, so land **rule-by-rule with oracle probes per
rule**. No `fn better`/`fn exact_match`/`fn pass_a` exists yet — the current
`resolve_overload` defers as soon as ≥ 2 candidates are `may_apply`
(`resolve_overload_defers_with_two_survivors`). The betterness bucket OV-9 found
is dominated by *unrefutable parameter positions* — class-typed (`DerivedTy` 56,
`BaseTy` 40) and `obj` (22, the P5 shape), plus `int` (34, widening/`op_Implicit`
keeping several alive). Before starting, re-read §2.5 and confirm the ceiling
justifies the work relative to the extension-gate refinement below.

#### Higher-value adjacent follow-ups (not part of OV-8)

- **Extension-gate verified enumeration** — now
  [`extension-scope-enumeration-plan.md`](extension-scope-enumeration-plan.md)
  (EX-0 landed #935; EX-1…EX-3 outstanding). OV-6's presence gate is what zeroes
  real-corpus coverage (every project references FSharp.Core), so moving it from
  *presence* to *by name* recovers more than OV-8 would; OV-9's differential is
  the instrument that makes it safe.
- **Real-corpus sweep** — blocked on a per-file `AssemblyEnv` story (sema tests
  run with an empty env); worth doing only after the extension gate is refined,
  since today the real-corpus number is structurally 0 %.
- **3.3d fold** — once OV-6 is stable and the differential is green both ways, the
  single-candidate `instance_method` wake is the degenerate case of the group
  engine; folding removes the separate path (a refactor, not a feature).
- **Extension-member *resolution*** (typing `s.Twice()` calls) — a separate slice;
  the absence-gate group construction is its seed, but applicability for extension
  candidates has extra wrinkles (obj-arg nullness skip, priority stamps).
- **`Ty` generic args** — several §5 deferrals trace to `Ty::Named` carrying no
  type arguments; its own pre-requisite plan (touches unification, rendering, the
  bridge, the oracle's canonical forms). The OV-1 oracle's `Params`/`Return`
  rendering is non-canonical for a generic overload (typar-in-instantiation falls
  back to FCS display text); consumers key generic overloads on `XmlDocSig`.
- **Do not chase** FCS's resolution cache, nullness adjustments, or the
  `ToString`-on-records hack (`MethodCalls.fs:554–564`) — none can change a
  *unique-applicable* outcome; they matter only to betterness/diagnostics.

## 6.1 OV-0 findings (reference, 2026-07-08)

Probed against `../fsharp` (name-resolution + `infos.fs`), our assembly reader,
and the `fcs-dump entities` oracle. Load-bearing conclusions:

- **(a) Which extension shapes the gate must account for.** For an
  instance-style call on a non-generic class/struct value receiver, FCS builds the
  group from the in-scope extension set filtered to `MethInfo.IsInstance = true`
  (`NameResolution.fs:2874–2880, :730–755`; `infos.fs:875–880`). **In scope (gate
  MUST see):** F#-style instance extensions incl. generic-method and
  generic-target, and C#-style `[<Extension>]` (`IsInstance` hard-wired true,
  `infos.fs:574–582`) — all arriving via explicit `open`, module `[<AutoOpen>]`,
  and FSharp.Core's implicit assembly-level auto-opens
  (`CheckDeclarations.fs:5606–5639`). **Safe to ignore:** F#-style *static*
  extensions (filtered by `IsInstance`). **Hazard:** a `[<Extension>]` whose `this`
  is a bare type variable lands in the *unindexed* bucket and is tried against
  every receiver.
- **(b) `is_extension_method` has false negatives — even on the authoritative
  path.** The overlay matches pickled vals to IL methods by compiled name + arity
  and under-flags real instance extensions FCS includes: **generic-method** vals
  (overlay skips generic vals, `ecma335_assembly.rs:2413`), **optional-parameter**
  vals (IL arity ≠ logical arity — the real FSharp.Core `Stream.AsyncRead` etc.),
  and same-arity collisions (`fsharp_pickle_merge.rs:466`). FSharp.Core takes the
  authoritative path (no overlay skips), so this is not a heuristic artefact.
  Therefore the flag is unusable as the gate's no-false-negative signal (⇒
  OV-0.5's pickle-name–derived index); C#-style `[<Extension>]` read straight from
  `[ExtensionAttribute]` in `project_method` (`ecma335_assembly.rs:1889`) is the
  one channel with no false negatives.
- **(c) FSharp.Core auto-open bite.** The implicitly-auto-opened instance
  extensions on concrete BCL types are few and exotically named
  (`Stream.AsyncRead*`, `WebRequest.AsyncGetResponse`, `WebClient.AsyncDownload*`,
  `GetReverseIndex`); **none collide with common BCL instance method names**, and
  the gate is only consulted when the intrinsic receiver already has an overloaded
  method of that name — so the implicit-auto-open bite on realistic sites is nil
  once OV-0.5's index exists.
- **(d) `isOverloadedMember` undercount.** Not worth fixing — the actionable
  consequence (§3.1, run on every `call:*` node, compare by signature) is already
  in OV-1.

## 7. Checklist for the implementing agent (OV-8 and beyond)

Before writing engine code: run the OV-1 oracle on your snippet and read what FCS
actually did. When a differential fails, suspect the gate you weakened, not the
oracle. When you want to commit on "obviously the right overload", name which of
§4's three proofs (group-complete, must-apply, all-others-refuted) you are missing
— if you cannot, it is P15 or P2 again. Never use one approximation for both
directions. Keep every defer silent: a deferred overload is invisible; a wrong one
is a bug shipped to every hover.
