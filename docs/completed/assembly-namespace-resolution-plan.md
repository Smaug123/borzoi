# Assembly namespace-resolution unification plan

Implement this plan with each stage on its own branch, stacked as necessary on
previous branches, so that a reviewer can review each branch in isolation.

Status: **complete — Stages 1–4 all landed** (Stage 4's one-walk north star
arrived via the #667 review rounds as `open_interpretations`; see the Stage 4
note below). Follow-up to #627
(referenced-assembly type-position resolution), which landed sound but left three
deliberately-deferred incompletenesses, each pinned by a "defers/under-resolves
soundly" test and a `KNOWN BOUNDARY` comment. This plan closes them by giving the
*value/member* path and the *type* path **one** tiered assembly-namespace
precedence model, and by **merging** project and assembly namespace opens (F#
opens both). Stage 1 extracted `resolve_assembly_path_tiered`; Stage 2 routed the
value/member path through it (G1) — the codex rounds added the
module-is-not-a-type rule, the partial-vs-complete tier preference, and a broad
no-wrong-target sweep; Stage 3 merged the namespace opens (G2), which also fixed a
latent value-path unsoundness (the raw project-namespace import binding a
same-named root assembly namespace) and a nested-`module`-type-prefix wrong target
(the type path now defers a path descending into a nested module).

**Stage 3.5 — uniform priority walk (landed).** A post-Stage-3 review found two
more FCS-confirmed wrong targets, both symptoms of the walker special-casing
project-shadow handling per tier and of an open's project/assembly readings being
assembled in two places:

- a project entity at a *lower* tier that completes the path lost to a held
  higher-tier **partial** (`namespace Demo; open Demo.Sub; (x: Calc.Inner)` with a
  preceding `module Calc = type Inner` — FCS binds `Demo.Calc.Inner`, we bound the
  partial `Demo.Sub.Calc`): the R7-A complete-beats-partial rule, which Stage 3
  applied only within the opens tier, holds across *all* tiers;
- a **project-only relative** reading was appended *after* the assembly readings
  in its open's group, so a complete assembly *root* reading won before the
  relative project shadow was consulted (`open Sub` in `namespace Other` with a
  project `namespace Other.Sub` — FCS binds `Other.Sub.Calc`, we bound the root
  assembly `Sub.Calc`).

The fix made the model correct by construction rather than patching the corners:
`open_namespace_readings` (né `assembly_open_readings`; since absorbed into
`open_interpretations`, see Stage 4) computed **one** priority-ordered reading
group per open over the union predicate (assembly ∨ project namespace), recorded
as an `OpenGroup`; a single iterator (`assembly_prefixes_by_priority`) fixes the
walk order for every consumer; and the walker applies one uniform rule — first
*complete* reading wins, first `ProjectShadowed` reading defers, a held partial
only survives a clean walk. The walker returns a three-state `TieredResolution`
(resolved / shadow-deferred / no-match), deleting the `type_path_shadow_possible`
re-derivation. Both corners are pinned in the
`namespace_merge_resolution_is_sound_against_fcs` sweep.

This is the assembly-side analogue of the (completed)
[`open-precedence-unification-plan.md`](../open-precedence-unification-plan.md),
which unified the *value-open frame*. That work is the precedent for the
multi-round, FCS-pinned care this needs.

## Why

#627 added F#'s tiered name-lookup precedence (opens → current enclosing
namespace → root, FS0039: never an ancestor) for referenced-assembly **types**.
Three gaps remained, all facets of the same thing — assembly namespace paths are
not yet resolved under that precedence *uniformly* for value/member and type
positions, nor merged with project namespaces:

- **G1 — value/member path is incomplete for the enclosing namespace.**
  `resolve_type_path` resolves `namespace Demo; (x : Sub.Thing)` →
  `Demo.Sub.Thing` (tier 2), but `resolve_long_ident` resolves the *expression*
  `namespace Demo; Sub.Calc.Zero()` by trying the **root** first and only a defer
  *guard* (added late in #627) keeps it from binding the wrong root `Sub.Calc`.
  FCS resolves it to `Demo.Sub.Calc.Zero`; we defer. Pinned by
  `nested_namespace_direct_expression_ref_defers_rather_than_binding_root`.

- **G2 — project and assembly namespaces are not merged (the "7c" boundary).**
  When `open Sub` (or a path's head) names **both** a project `namespace Sub`
  *and* a relative assembly namespace `Demo.Sub`, F# opens both. The open arm's
  `!self.is_project_namespace_path(&path)` gate suppresses the assembly
  interpretation entirely, so a later name living only in the assembly namespace
  under-resolves. Pinned by
  `project_namespace_suppressing_a_relative_assembly_open_under_resolves_soundly`.

- **G3 — the two paths are divergent implementations.** The type path is tiered
  (`resolve_type_path`, tiers 1–3); the value/member path is root-first + a
  bolt-on defer guard (`resolve_long_ident`). Two copies of "F# precedence" drift
  — exactly the parallel-copy tax that produced the #627 review cycles. They
  should share one tier walker.

All three are currently **sound** (defer / under-resolve, never a wrong target).
Closing them is pure completeness: turning FCS-validated deferrals into
FCS-validated resolutions.

## FCS-probed precedence (the oracle — re-probe with `fcs-dump uses` if unsure)

Validated in #627 against the `tests/fixtures/assembly_env` fixture (which has a
nested `Demo.Sub` *and* a root `Sub`, plus an internal-only `Demo.Hush` vs public
root `Hush`).

| Source (referenced assembly has the namespaces shown)            | Resolves to | Rule |
|------------------------------------------------------------------|-------------|------|
| `namespace Demo; (x : Sub.Thing)` — type position                | **Demo.Sub.Thing** | enclosing ns before root (already done, type path) |
| `namespace Demo; Sub.Calc.Zero()` — value/member position        | **Demo.Sub.Calc.Zero** | same rule; **G1** makes the value path agree |
| `namespace Demo.Sub; (x : Sub.Calc)`                             | **root Sub.Calc** | FS0039: relative reaches the *current* ns + root, never an ancestor `Demo` |
| `namespace Demo; open Sub; …Calc`                                | **Demo.Sub.Calc** | relative open canonicalised through the enclosing ns |
| `open Demo; open Sub; …Deep` (Deep only in `Demo.Sub`)           | **Demo.Sub.Deep** | chained open shortens through the prior open |
| `open Hush` in `namespace Demo`, `Demo.Hush` internal-only       | **root Hush** | only *public* namespaces drive canonicalisation |
| project `namespace Sub` + assembly `Demo.Sub`, `open Sub`; name only in assembly | **Demo.Sub.<name>** | **G2**: F# opens **both**; today we open only the project one |

The unifying rule: **opens > current enclosing namespace > root**, evaluated
per-tier, first tier with a unique result wins; within a tier, distinct
candidates defer; project and assembly interpretations are **both** opened at the
tier they match.

## Design

Both positions already share the leaf record-generators and the `AssemblyPath`
result type:
- value/member: `assembly_path_records(prefix, segments) -> AssemblyPath`
  (no arity; resolves a trailing **static member** as a `Member`);
- type: `assembly_type_path_records(prefix, segments, arity) -> AssemblyPath`
  (arity-aware; no member tail).

They diverge only in the **tier walk** around those generators. Extract that walk
once; parameterise it by the leaf generator.

`enclosing_namespace()` (added in #627) already gives the single FS0039 tier-2
prefix; `imports` (canonicalised by `canonical_assembly_open`) is tier 1; the
empty prefix is tier 3. The walk is exactly `resolve_type_path`'s tiers 1–3
(resolve.rs ~2290–2336), lifted out.

The **merge (G2)** is orthogonal to the walk: it is about which prefixes reach
`imports`, i.e. the *open arm* (resolve.rs ~1564), not the path resolution.

## Stages

### Stage 1 — Extract the shared tier walker (refactor; no behaviour change)

**Dependencies**: none.

**Implements**: G3 (the vehicle).

Lift `resolve_type_path`'s tier loop into
`resolve_assembly_path_tiered(&self, segments, gen) -> Option<Vec<(TextRange, Resolution)>>`
where `gen: impl Fn(&[String]) -> AssemblyPath` is the per-tier leaf generator.
It walks tier 1 (`&self.imports`, gather distinct → unique-wins / ambiguous-defer
/ project-shadow-defer), then tier 2 (`enclosing_namespace()`), then tier 3
(root `&[]`); returns `Some(recs)` for the winning tier or `None` to defer.
Re-express `resolve_type_path` to call it with
`|p| self.assembly_type_path_records(p, segs, arity)`. The opaque-open guard and
the in-file-type shadow stay in `resolve_type_path`.

**Correctness oracle**: pure refactor — every existing `resolve_assembly.rs`,
`resolve_assembly_diff.rs`, `resolve_project_assembly_diff.rs`, `resolve_types.rs`
test passes unchanged; `cargo clippy`/`doc` clean. No test edits.

---

### Stage 2 — Apply the tier walker to the value/member path (G1 completeness)

**Dependencies**: Stage 1.

**Implements**: G1.

Replace `resolve_long_ident`'s root-first block + the
"`SOUNDNESS: inside a namespace…`" defer guard (resolve.rs ~4170–4210) with a
call to `resolve_assembly_path_tiered(segments, |p| self.assembly_path_records(p, segments))`.
Now `namespace Demo; Sub.Calc.Zero()` resolves to `Demo.Sub.Calc.Zero` (tier 2)
instead of deferring; fully-qualified and `open`-shortened paths are unchanged
(they win at tier 3 / tier 1 respectively). Keep the project-shadow and
opaque-open gates that precede the assembly block.

**Correctness oracle**:
- Flip `nested_namespace_direct_expression_ref_defers_rather_than_binding_root`
  → `…resolves_through_the_enclosing_namespace`, asserting the `Calc` qualifier
  is `Entity(Demo.Sub.Calc)` and the whole path the `Member` of it.
- New `assert_value_use_complete` in `resolve_assembly_diff.rs` (the value-side
  analogue of the existing `assert_type_use_complete`): a value/member use FCS
  resolves into the fixture **must** resolve on our side too. Add cases:
  `namespace Demo; Sub.Calc.Zero()` → `Demo.Sub.Calc.Zero`; the existing FQ /
  open cases as regressions.
- Existing `assembly_resolution_agrees_with_fcs` (sound-only) unchanged and green.

---

### Stage 3 — Merge project and assembly namespace opens (G2 / the 7c boundary)

**Dependencies**: Stage 1 (not Stage 2 — different code region; can land in
parallel, but sequence after for a clean diff).

**Implements**: G2.

In the open arm (resolve.rs ~1564), drop the `!self.is_project_namespace_path`
gate that suppresses the assembly-namespace interpretation, so a path that is
*both* a project namespace and an assembly namespace records **both**: the
project namespace via the existing `ProjectOpen::Namespace` machinery **and** the
canonicalised assembly namespace into `imports`/`open_shortening_prefixes`. Verify
no double-count / mis-order against the existing precedence (the assembly open is
already lowest-priority; the project namespace is pushed after it — preserve
that). Remove the `KNOWN BOUNDARY (#595)` comment.

Also fold in the **nested-module type-prefix** gap inherited from before Stage 2
(surfaced by the Stage-2 codex review): the type path tries the opens tier before
the as-written shadow, so `(x : Calc.Something)` where `Calc` is a project
*nested* module and an assembly `Demo.Sub.Calc.Something` exists under an `open`
would resolve to the assembly even though F# binds the nested module first (a
wrong target, not just an under-resolution). It is latent — no fixture constructs
the collision and it predates Stage 2 — but the merge is where the
proper-prefix-vs-exact distinction belongs (a nested module shadows a type only as
a *member access* through it, never as the bare name; the same insight that put
`is_exact_project_module` in the value-only shadow predicate). Add a
`resolve_assembly.rs` soundness test (nested `module Sub`; `(x : Sub.Calc)` under
`open Demo`) when the merge lands.

**Correctness oracle**:
- Flip `project_namespace_suppressing_a_relative_assembly_open_under_resolves_soundly`
  → assert it resolves the assembly-only name to its `Demo.Sub.*` entity.
- New `resolve_project_assembly_diff.rs` case: a project file declaring
  `namespace Sub`, a second file `namespace Demo; open Sub; <name-only-in-assembly>`,
  with the per-project expected-count bumped — FCS resolves the assembly name,
  so we must too.
- All existing `resolve_project_assembly_diff.rs` cases unchanged and green
  (the merge must not regress project-shadowing precedence).

---

### Stage 4 (done) — North star: one interpretation walk over project *and* assembly

**Dependencies**: Stages 1–3.

**Implements**: the deeper unification G3 gestures at.

Today `resolved_project_opens` computes the project interpretations (modules /
namespaces) of an `open` across the precedence tiers, and the assembly side is a
parallel `canonical_assembly_open` + tier walker. Fold the assembly-namespace
interpretation into a single `resolved_opens` that emits project modules, project
namespaces, **and** assembly namespaces from one tier walk, so there is exactly
one place precedence lives for *both* sides. This subsumes Stage 3's merge and
removes the last parallel copy. Defer until Stages 1–3 prove the model; only
worth it if a third consumer (or a third bug) shows the parallel still drifts.

*Done — landed via the #667 review rounds, not as a separate refactor.* The
review found two FCS-confirmed wrong targets that were both symptoms of the
parallel copy (the two base-walks were applied in disjoint phases, so every
module out-ranked every reading regardless of base), which forced the fold-in
this stage predicted: `open_interpretations` is the single walk, emitting
`OpenInterpretation::Module` (project modules, alias tier included) and
`OpenInterpretation::Reading` (assembly ∨ project namespaces) from one tiered
base walk (alias → prior opens latest-first → enclosing → root), applied
lowest-priority-first by the open arm. `resolved_project_opens`,
`open_namespace_readings`, and the `debug_assert` cross-check between them are
gone — precedence lives in exactly one place. ("A third bug shows the parallel
still drifts" was the trigger condition written above, and that is precisely
what happened.)

**Correctness oracle**: all of Stages 1–3's oracles, plus the chained-open and
auto-open-vs-case orderings pinned in `resolve_project_diff.rs` /
`resolve_autoopen.rs` by the review round that landed it.

## Tests to write first (TDD)

Each stage's oracle above is written **before** the change. The two completeness
differentials (`assert_type_use_complete`, the new `assert_value_use_complete`)
are the key safety net — they *fail* on incompleteness, so they drive Stages 2–3
and stop a silent regression to deferral.

## Risk

The value/member path (`resolve_long_ident`) and the open arm are the most
heavily FCS-differentially-tested, most precedence-subtle code in the resolver —
the #627 and `open-precedence-unification` histories both warn of ~5–10
`codex review` rounds on exactly this surface. Mitigations: Stage 1 is a
behaviour-preserving refactor (cheap to verify); Stages 2–3 each flip exactly one
deferral to a resolution and are gated by the completeness differential; do each
with a fresh context budget, tests-first, FCS-pinned, and watch for the
doom-loop signal (a sequence of reviews that keeps finding precedence corners
means the tier model is wrong, not the patch — fix the model).
