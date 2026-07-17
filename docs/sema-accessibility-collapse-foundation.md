# Foundation: cross-file accessibility + uniform collapse model

Groundwork for making the cross-file namespace-straddle resolution
(PR #973 / `sema/straddle-resolution`) sound **by construction** rather than by
patching corner after corner. Written after PR A (#967, value accessibility)
landed and three codex rounds on the straddle kept surfacing
`(tier × index × namespace × cross-file-privacy)` corners.

## Stage 1 — F# `private` accessibility, oracle-pinned (fcs-dump)

| probe | scenario | result |
| --- | --- | --- |
| S1 | sibling module opens a module's `let private` (same file) | inaccessible |
| S2 | nested submodule references `let private` (same file, no open) | accessible |
| S3 | descendant namespace `open`s a namespace's `type private` (cross-file) | **accessible** (with FS warning) |
| S4 | unrelated namespace opens `type private` | inaccessible |
| S5 | `internal` across a sibling open | accessible (intra-project) |
| Q1 | cross-file descendant `open N`, `let private X` in `N.A` (no public X) | **`N.A.X` accessible** |
| Q2 | cross-file `open M` from `M.Inner`, `let private X` in `M` | **`M.X` accessible** |
| Q3 | cross-file descendant references `let private` **without** an open | **unbound** |
| Q4 | cross-file descendant references `type private` **without** an open | **unbound** |

**The rule.** A `private` item with declaring container `C` is accessible from a
reference whose enclosing container `S` satisfies `S.starts_with(C)` — its own
subtree — **and this holds across files when the reference is through an `open`**
(Q1/Q2). The *implicit* enclosing-scope visibility (no `open`) is same-file only
(Q3/Q4 vs S2), but that path does not use the open-fold predicate. `internal` is
intra-project-visible; only `private` is tracked.

**Conclusion: the open-fold predicate `site.starts_with(container)` is already
correct** (it is what PR A shipped). Codex PR-B-round-2 P1 #1 (claiming cross-file
private is inaccessible to a descendant) is a **false positive** — probing our own
resolver on its exact scenario shows we `Deferred` (a sound over-defer), never a
wrong target. Stage 2 (a "file-boundary-aware predicate") is therefore a no-op
beyond confirming/​documenting the above.

## The one real remaining wrong target — the collapse model

Codex PR-B-round-2 P1 #2 (confirmed against FCS and our resolver): a **direct-tier**
collapse. `by_qualified_path` / `constructors` are latest-wins maps, so a later
`exception private X` collapses the id at a namespace-direct path to the private
one; `direct_tier_ids_at` then omits the name, and the straddle commits an
auto-open `N.A.X` where FCS binds the surviving public direct `N.X`. This is the
same class as PR A's P1 (value collapse) and PR-B-round-1's P2s (submodule
constructor collapse) — every `(tier × index)` cell is a separate corner because
the fix has been per-cell ad-hoc collapse-defers.

**The uniform fix (Stage 3).** Replace the latest-wins collapse with **per-path
export history**: `HashMap<Vec<String>, Vec<ExportRecord>>` where
`ExportRecord = { id, file, is_private, is_case }`, appended in Compile order.
Then two queries subsume every ad-hoc rule:

- *latest accessible value at `P` from `S`* = max-file record where
  `!is_private || S.starts_with(P[..len-1])` (values **and** cases — a case is a
  value). Feeds `direct_value_children` and the straddle's value slot.
- *latest accessible case at `P` from `S`* = the same, restricted to `is_case`.
  Feeds `direct_constructor_children` and the straddle's constructor slot.

Because no export is lost, a public export shadowed by a later inaccessible
`private` is *selected* (resolves — better than PR A's collapse-defer, which only
deferred) and the collapse corners are **impossible by construction**. The ad-hoc
`public_value_paths`, `collapsed_private_public_names`,
`collapsed_private_public_constructor_names`, and the `SubmoduleFold`
value-slot-from-cases patch all delete.

**Stage 4.** Rebuild the straddle fold on the two queries; the `(tier × index ×
namespace)` corners collapse into one code path. Re-validate against the FCS
differential corpus + codex.

## Stage 3½ — the accessibility model (oracle-pinned access-roots)

Three codex rounds on the collapse recovery proved the recovery is only sound
with a **complete** accessibility model: own `let private` is not enough —
privacy is *inherited* from a `private` enclosing module (D2/D6) and from a
`private` union/exception *type* for its cases (D3), and the accessible scope is
the private entity's **container**, which for a `module private` is the module's
*parent* — not the module itself (my first patch got this wrong and regressed
sibling access). Pinned via `fcs-dump uses-project` (cross-file, through `open`):

| construct (def of `X`)                         | accessible from        | access-root      |
| ---                                            | ---                    | ---              |
| `let private X` in `N.M`                        | descendants of `N.M`   | `[N,M]` (X's container) |
| value in `module private M`                     | within `N`             | `[N]` (M's container)   |
| case of `type private T` in `N.M`              | descendants of `N.M`   | `[N,M]` (T's container) |
| case of *public* `type T`                       | everywhere             | none (public)    |
| `exception private X` in `N.M`                 | descendants of `N.M`   | `[N,M]`          |
| value in *public* `Inner` in `module private M` | within `N`             | `[N]` (M dominates)     |

**The rule.** A `private` marker on entity `E` makes `E` and its members
accessible only from within `E`'s **container**'s subtree (`site.starts_with(container)`).
Members inherit; stacked private boundaries take the **deepest** (longest)
container. The access-root is always a prefix of the export's own qualified path,
so it is stored as a length. `internal` stays intra-project (public here).

Represented as `access_root_len: Option<usize>` on each export (`None` = public;
`Some(k)` = accessible only where `site.starts_with(path[..k])`), computed by
threading an `access_floor` (deepest enclosing `module private` container) through
the walk and combining with own-`private` (values) / type-`private` (cases).
`latest_accessible` becomes `access_root.is_none() || site.starts_with(prefix)`.

## Stage 3 — landed (`sema/uniform-collapse-model`, off `main`)

`ProjectItems` now holds `value_exports: HashMap<Vec<String>, Vec<ExportRecord>>`
(`ExportRecord = { id, is_private, is_case }`, appended in Compile order),
replacing `by_qualified_path` + `constructors` + `private_value_ids` +
`public_value_paths`. `latest_accessible_value` / `latest_accessible_case`
select the newest export the site can access; `direct_value_children` /
`direct_constructor_children` take the reference site and return the accessible
id, so `open_module_values` dropped its per-name privacy filter and the
collapse-defer block. Validation: the `let X = 20` then `let private X` collapse
now *resolves* to the surviving public value from outside (was a defer),
matching FCS; the FCS generative cross-file differential and the
WoofWare.{Zoomies, LiangHyphenation, Expect} real-project differentials all
stay at their `main` tallies (Zoomies keeps its 2 pre-existing `prevVdom`
alt-binders — a local-shadowing bug unrelated to this change; the others are
zero-divergence, zero-alt-binder).

### Residual: inherited privacy is still untracked (a case's `is_private`)

A union/exception case is value-exported with `is_private = false` regardless of
its type's accessibility (`type private T = X` → case `X` records public), and a
value inside a `private` nested module likewise does not inherit the module's
restriction. So `ExportRecord::is_private` is a *lower bound* on inaccessibility,
not exact. Codex flagged this against the recovery (round 1 of this PR): a
newest-first scan that skips a later inaccessible `private` value could recover
an older inherited-private case.

Verified false positive **for the recovery**: probing our own resolver
(`resolution_at` + `item_def`) on the exact scenario — file 0 `type private T =
X`, file 1 `let private X`, unrelated `open N.M; X` — we return
`Deferred(UnboundName)`, *identical to `main`*, never committing the case; FCS
reports `X` unbound (FS0039). The reason is orthogonal to the record flag: a
case-declaring module is opened *hidden*, so a cross-file case is never
committed into unqualified value scope through an `open` (it defers). Crucially,
the model must **not** suppress case recovery to "fix" this — with a fallback
`open A.F` supplying a public `X`, FCS binds the module's *own* public case
(`open N.M` shadows the fallback); dropping the case from `direct_value_children`
would let the fallback win a name the module provides — a wrong target. So the
recovery deliberately returns the case (letting it shadow the fallback), and the
hidden-open decline keeps the outcome sound. `resolve_module_opens.rs`'s
`an_inherited_private_case_is_not_committed_through_a_collapsed_open` is the
tripwire: it fires if cross-file case-through-`open` is ever made to resolve,
flagging that *case privacy* (inherited from the type) must land first.

The pre-existing "a cross-file public union case is not brought into unqualified
value scope through an `open`" gap (`open N.M; X` for `type T = X` defers, where
FCS binds the case) is on `main` too and orthogonal to the collapse model.

## Stage 4 — landed (straddle resolution, #980; supersedes #973)

Rebuilt the cross-tier namespace-straddle fold on the Stage-3 queries plus a new
**provenance primitive** — `ProjectItems::item_file_bases` / `file_of` /
`num_files`: an `ItemId` → its declaring Compile-order file. `direct_tier_ids_at`
/ `submodule_contributions_at` collect each tier's per-name contribution *with its
file* through the access-root-aware `direct_value_children` /
`direct_constructor_children`, so the collapse wrong target is gone by
construction. `open_project_namespace_values` resolves **conservatively** (commit
the direct-tier winner only when it out-files *every* submodule member — the one
sound conclusion, since a member's file `>=` its auto-open surface's fold
position — else defer). `extern`-bearing modules are marked hidden so the
`fold_hidden` gate covers the one unindexed value producer. Codex-clean over 3
rounds (two provenance-reliability corners found + fixed: augmentation
overstating a member's file; `extern` understating the slot).

## Stage 5 — landed (per-fragment file-ordered auto-open fold, #987)

Closed the one carried availability gap. **S1** used to defer: a later
un-augmented `[<AutoOpen>]` submodule value that genuinely beats an earlier
namespace direct case (`exception X`@f0, `[<AutoOpen>] module A = let X`@f1,
`open N; X` → FCS binds `N.A.X`@f1) deferred, because Stage 4 committed only the
*direct* winner and could not confirm "the submodule genuinely wins".

**Root cause (now fixed).** `submodule_contributions_at` folded *every* member at
`[A, …]` (via `direct_value_children([A])`), keyed by the member's own
`file_of`. But only an **`[<AutoOpen>]`-attributed fragment** of `A` is
auto-opened, and its members fold at *that fragment's* file. A member in a
*plain* `module A` augmentation is not auto-opened at all. So the Stage-4 fold
both **over-included** (counted plain-fragment members) and could not order the
genuine case — worse, the plain-fragment over-inclusion was a latent **wrong
target** for a name with no direct tier (`open N; X` resolved `N.A.X` from a
plain fragment where FCS reports unbound), which the green corpus never
exercised.

**Oracle-pinned fold rule** (`fcs-dump uses-project`, cross-file, `open N; X`):

| scenario | binds |
| --- | --- |
| direct@f0, `[<AutoOpen>]`-submodule value@f1 (S1) | submodule@f1 (latest file) |
| `[<AutoOpen>]`-submodule value@f0, direct@f1 (S2) | direct@f1 (latest file) |
| direct + auto-open submodule, same file | submodule (within a file, auto-opens fold after the direct tier) |
| two `[<AutoOpen>]` submodules @f0/@f1 | @f1 (latest file) |
| auto-open `A`@f0, **plain** `module A` adds `X`@f2, direct `X`@f1 | direct@f1 (the plain f2 fragment is **not** auto-opened) |
| auto-open `A`@f0, plain `module A` adds `X`@f2, no direct | **unbound** (f2 not auto-opened) |
| auto-open `A`@f0, **`[<AutoOpen>]`** `module A` adds `X`@f2 | `A.X`@f2 (that fragment folds at its own file) |
| auto-open `A.X`@f0, direct `X`@f1 | direct@f1 (`A.X` folds at f0 `<` f1) |

**What landed — a file-ordered fold over fragments.** The first attempt bolted a
per-fragment *gate* onto the old per-module-path/collapse fold, and a generative
differential (below) plus codex proved that structurally wrong: the collapse
query still lost an earlier auto-open fragment shadowed by a later plain one, and
folding "all of a module's members at the module's list position" mis-ordered
multi-file and nested fragments. The fold was **rebuilt around the fragment**:

- `ProjectItems::auto_open_module_paths` is `Vec<(Vec<String>, usize)>` — each
  non-`private` `[<AutoOpen>]` *fragment* with its declaring Compile-order file (a
  module may have several; the same-file half carries the file being resolved).
- `Resolver::auto_open_fragments_reachable(namespace)` returns every reachable
  fragment as `(path, file)` **sorted by file**, with **same-file parent-gated
  nesting**: a nested `[<AutoOpen>]` child lives in one parent block, in that
  block's file `f`, so it is reached only through a parent fragment `(P, f)` at
  the *same* file — a plain `module P` augmentation carrying an auto-open child
  therefore folds nothing.
- `open_project_namespace_values` folds the namespace's direct tier, then each
  fragment **in file order**, each contributing only its *own-file* members
  (`open_module_values` takes `fragment_file: Option<usize>`; the per-file member
  set comes from `ProjectItems::fragment_value_children` /
  `fragment_constructor_children`, which read the export declared *in that file*,
  not the collapsed latest). A name contested across fragments has its
  latest-file contribution pushed last, so it wins by push position — no per-path
  re-push. `submodule_contributions_at` reads the same fragment list, so the
  direct-tier straddle decision and the fold agree by construction.

The straddle machinery (direct-tier vs submodule) still re-pushes a direct winner
last when it out-files every fragment, and still defers the same-file straddle
(closed in Stage 6 below), the hidden fold (`extern` / active pattern), and the
type-eviction dimension — all sound availability gaps.

**The generative differential.** As the doc anticipated, the moment corners
appeared the fix stopped being one-off patches and became a systematic guard:
`crates/sema/tests/all/resolve_straddle_gen_diff.rs` *enumerates* multi-file
projects placing one probed name at every `(container, file)` position —
direct tier, `[<AutoOpen>]`/plain fragments, nesting — permutes their Compile
order, and checks FCS's resolution of `open N; X` **per reference site** (so it
also catches a target we commit where FCS is unbound, which the ordinary
`uses-project` agree-or-defer harness cannot). It found **15** divergences on the
first (gate-on-collapse) attempt — 5× the three codex flagged — and drove the
rebuild to green.

**Validated**: the generative differential; every oracle row a
`resolve_module_opens.rs` test (S1 and the plain-augmentation row flipped from
defer to resolve; rows 3/6/7 + an S1-pattern variant new) plus three FCS-diffed
`resolve_project_diff` entries; the FCS generative + corpus differentials; the
`WoofWare.{LiangHyphenation, Expect, WeakHashTable}` real-project diffs (zero
divergence, zero alt-binder); codex.

## Stage 6 — landed (same-file straddle resolves; no block-order provenance)

Closed the last carried straddle gap. Stage 4/5 **deferred** a straddle whose
direct tier and auto-open fragment sit in the **same file**, on the stated belief
that *"block order within a file decides"* and was untracked. Oracle probing
disproved the premise: I pinned the same-file contest against `fcs-dump
uses-project` in **both** block orders, both namespaces, values and cases (probes
A/B/D/E/F/G):

| file-0 block order | `open N; X` expr | pattern `X _` |
| --- | --- | --- |
| `exception X` then `[<AutoOpen>] A = let X` | `N.A.X` | `N.X` |
| `[<AutoOpen>] A = let X` then `exception X` (swapped) | `N.A.X` | `N.X` |
| `exception X` then `[<AutoOpen>] A = exception X` | `N.A.X` | `N.A.X` |
| `[<AutoOpen>] A = exception X` then `exception X` (swapped) | `N.A.X` | `N.A.X` |

**Block order does not decide.** The real rule is per-file: FCS folds a file's
direct tier **before** that file's auto-open fragments, block-order-independently,
with files in Compile order — so *later file wins, and a same-file tie goes to the
submodule*. (Block order matters only *among* sibling auto-opens — the later block
wins — which the fold already honours via its source-ordered fragment list.) No
within-file provenance is needed; the fold's push order (whole direct tier, then
fragments in file order) already realises it.

**The fix** (`open_project_namespace_values`): delete the same-file short-circuit
defer and break the value-slot tie to the submodule — the value comparison becomes
`submodule value file >= direct file` (`>=`, submodule wins ties) while the
direct-winner comparisons stay strict `>`. The constructor namespace already broke
ties to the submodule (strict `>` on the direct side). So an expression binds the
auto-open value (it folds after the direct case), and a pattern binds the direct
exception unless an auto-open also supplies a case.

**Validated**: the four oracle rows as `resolve_module_opens.rs` tests (expr →
auto-open, pattern → direct case, a block-swapped variant, and the auto-open-case
row); the generative straddle differential extended to **same-file** groupings —
`build` now packs several placements into one file under one `namespace` header
and enumerates their block orders, so the same-file fold (both namespaces, nested
and multi-auto-open, plus a split same-file+cross-file grouping) is covered by
construction; the full `borzoi-sema` suite; codex.

## Status

- **All six stages**: PR A (#967, value accessibility), #978 (Stage 3 uniform
  collapse model + Stage 3½ oracle-pinned accessibility access-roots), #980
  (Stage 4 straddle resolution + provenance), #987 (Stage 5 per-fragment
  file-ordered auto-open fold + generative straddle differential), Stage 6
  (same-file straddle resolves — the per-file "direct tier before auto-open
  fragments" rule, no block-order provenance needed). #967/#978/#980/#987 are in
  `main`; #973 was closed, superseded by #980.
- The **type-eviction** dimension (project type constructors unmodelled) and the
  **hidden fold** (`extern` / active-pattern value producers) remain sound
  *defers* — availability gaps, never wrong targets. The same-file straddle is no
  longer among them (Stage 6).
