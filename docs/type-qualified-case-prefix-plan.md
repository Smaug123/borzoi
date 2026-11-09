# Follow-up: resolve the type-qualifier *prefix* through the qualified-value machinery

> **Status:** landed. All three gaps, the head-widening (Stage 4), and the
> two sound defers it left — multi-segment same-file module heads (#69) and
> same-file namespace-rooted qualifiers (#70) — are closed. Everything below is
> a one-line landed record plus the FCS-verified precedence reference /
> landmine log any further change must respect.

This was the follow-up to PR #653 (`sema/type-qualified-cases`), which shipped
`Type.Case` resolution — `Color.Red` / `Lib.Color.Red` / `open Lib; Color.Red`
for unions, RQA-unions, and enums, same-file and cross-file, in expression and
pattern position. #653 resolved the *case* but resolved the *type-qualifier
prefix* with a bespoke tier walk plus a value-shadow check; this plan replaced
that with a complete per-container name environment.

## Landed (one line each)

- **Gap B** (PR #666) — a non-`rec` `let … and …` group's eagerly-interned
  binders are marked *pending* (`Resolver::pending_items`) while their RHSs
  resolve, so `ordinary_value_at` skips them and a binding's own qualified
  self-reference (`let Color = Lib.Container.Color.Red`) reaches the earlier
  file's case. Pinned by
  `cross_file_type_qualified_case_resolves_for_a_non_rec_self_reference`.
- **Gap A** (PR #688) — same-file module-qualified `Pal.Color.Red` resolves via a
  complete per-container declared-name view (`Resolver::container_decls`,
  `DeclKinds`), replacing a reverted bespoke attempt. The head resolves through
  the lexical container chain only; the case emits **iff** the segment is
  unambiguously a type there, deferring on any contention or non-enumerable open.
  Expression and pattern position.
- **Gap C** (PR #689) — closed *as already-correct*. A value and a type at the
  same qualified path is legal **only** same-block (cross-block is FS0248/FS0247),
  where the value always commits; `value_shadows_case`'s order-insensitive defer
  is therefore the FCS-faithful answer. No provenance / no `ContainerDecls`
  extension needed. (Probe matrix retained below.)
- **Stage 4 — open-supplied heads** (PR #691, branch
  `sema/gap-a-open-supplied-heads`) — an `open` whose target is a same-file
  module or a same-file-only namespace is a full head candidate in the
  source-position-ordered environment, its residual classified by the same
  complete-information machinery. Opens interleave with lexical `module`
  declarations by pure latest-wins source position
  (`Resolver::explicit_open_prefixes`, `DeclKinds::module_pos`,
  `Resolver::open_contests_candidate`).
- **Stage 4 — cross-file open targets** (PR #696) — every exported type's
  qualified path is indexed (`ProjectItems::type_paths`, case-enumerable unless
  abbreviation / bodyless / inline-IL repr), plus a module-only nested index
  (`real_nested_modules`). A cross-file target owning nothing at the segment is
  transparent (FCS backtracks); one that may own it unprovably → defer.
- **Stage 4 — head walk & root** (incl. PR #702 for the raw `global` head
  marker) — the head is a candidate *loop*, not a first-stop search: it skips
  containers whose `Pal` declaration cannot own a dotted head
  (`DeclKinds::stops_dotted_head`), continues past a candidate whose residual
  fails to resolve, and searches the root (`k == 0`) last. Bare-head in-project
  module aliases are done (alias is definitive for the head).

- **Multi-segment qualifiers and namespace roots** (#69, #70) — the residual
  after the head is walked segment by segment through `container_decls`
  (`Resolver::classify_module_qualified_residual`): each segment before the
  type must be *solely* a module (`DeclKinds::is_sole_module`) or a same-file
  sub-namespace, a `module private` one only from inside its container; any
  co-declaration defers, as does a type declared twice. A same-file namespace
  of the head's name is the last head candidate
  (`Resolver::namespace_head_candidate`): in scope only once an *earlier*
  block declared it, and below everything the current block brings into
  scope, so it emits only when nothing else may name the head
  (`Resolver::root_head_rival`, which a root module candidate must clear too)
  and the namespace has no other contributor (an earlier file, an assembly, a
  root module of its name, the current block). Graded by
  `crates/sema/tests/all/type_qualified_prefix_grid.rs`, which found further
  wrong-target classes on the way, all now declining: a `module rec` /
  `namespace rec` block (FCS puts its *later* modules and opens in scope at
  the use — `DeclineCause::RecursiveModuleActive`), a module or type declared
  twice in one container (FCS reads the first), a same-file `[<AutoOpen>]`
  module supplying a module of the head's name, an `open` in an earlier
  `namespace global` block (FCS carries it into later blocks), and an
  earlier file's module in the enclosing namespace outranking a root module.

## Invariant

**The invariant (sound by construction):** build the
*complete* declared-name view per container (`container_decls`), resolve the head
through the **lexical** container chain and the source-position-ordered open
list only, and emit the case **only** when the qualifier segment is
unambiguously a type there — deferring on any contention or any non-enumerable
open. Complete information → decide; incomplete → defer. Do **not** reuse
`resolved_project_module` for a same-file lexical head: it has the wrong
precedence (opens-before-enclosing, blind to anonymous roots) and was the source
of the r13 wrong-target family.

## FCS-verified precedence reference (regression guard)

From `fcs-dump uses-project`; pinned in
`crates/sema/tests/all/resolve_type_qualified_cases.rs`. The design must satisfy
these and the widening must not regress them.

| shape | FCS resolves to |
| --- | --- |
| `Color.Red`, only a type `Color` in scope | the case |
| `Color.Red`, a `let Color` value also in scope, **union** | member access (value), **any** order |
| `Color.Red`, a `let Color` value also in scope, **enum** | the case iff the value is *earlier* than the type; else member access |
| `Color.Red`, a same-named union **case constructor** `Color` (`type Color = Color \| Red`) | the case (a case ctor is not a dottable value); the *qualified* `Lib.Container.Color.Red` form is instead FS0812 at the use |
| `Container.Color.Red`, `Container.Color` a value | member access (the value); `.Red` a field we don't model → defer |
| `A.B.Color.Red`, `A.B` a value (shorter prefix) | member access on `A.B` |
| value & type at same path (same module block — the only legal shape, FS0248/FS0247 otherwise) | the **value**, union *and* enum, either order, both positions |
| `open M (module Color); type Color = …; Color.Red` | `M.Color.Red` (opened module out-ranks the same-file type when the open is *later* in source) |
| `open A (type Color); open B (module Color); Color.Red` | `B.Color.Red` (later module open wins) |
| module alias `module P = Lib.Pal; P.Color.Red` | `Lib.Pal.Color.Red` (alias is definitive for the head) |

Head/segment classification as shipped: a dotted head `Pal` is owned by the
module namespace (nested module or module abbreviation), plus — in **expression**
position only — a `let`-bound value that commits member access; a type / union
ctor / active pattern / exception ctor named `Pal` never hides a farther module.
Opens interleave with lexical module declarations by pure source position
(latest-wins), residual backtracking across both kinds. In the resolved
container: a dottable value/ctor (and, in pattern position, an active-pattern
case) → defer (same-file member access); a type carrying the case → emit; a type
owning the segment without the case → defer in expression position (members may
be added by later augmentations, unprovable) but search outward in pattern
position (a static member is not a pattern).

## Landmines (each cost a codex round)

- **Same-file `type_cases` lookup must not fight `type_case_path`.** The
  2-segment `Type.Case` is owned by `type_case_path` (with its shadow check and
  enum/union collision rule); gate any new same-file lookup to genuinely
  module-qualified (3+-segment) paths.
- **`prepare_binding` eager insert.** A non-`rec` binder is in `self.items`
  during its own RHS; reuse the `pending_items` / self-ancestor guard rather than
  reinventing it.
- **Opened values have no in-file `Def` range.** Use `head_is_definite_value`,
  not `value_def_range`, for "is the head a value".
- **Keep the conservatism set.** Defer under `opaque_value_open` /
  `opaque_dotted_open` / `unmodelled_open_active` exactly as the qualified-value
  branch does.
- **Do not re-attempt an order / `ItemId` "latest-wins" comparison for
  value-vs-type at a path (Gap C).** The only legal coexistence is same-block,
  where order is irrelevant and the value always wins; any order rule is provably
  wrong.

### Gap C probe matrix (legality, `dotnet build`)

| shape | verdict |
| --- | --- |
| same-named module in two files (`namespace Lib; module Container` ×2) | **FS0248** |
| same-named module in two blocks of one file | **FS0248** |
| `module Lib.Container` + `namespace Lib.Container` | **FS0247** |

So "both exist at the path" *implies* same-block; the value commits in every
same-block variant (union/enum, either order, expression and pattern position).

## Code pointers

- `crates/sema/src/resolve/lookup.rs` — the `Type.Case` branch, the cross-file
  dispatch, `open_contests_candidate`, `head_is_definite_value`,
  `value_shadows_case`, `ordinary_value_at`.
- `crates/sema/src/resolve/state.rs` — `Resolver::container_decls`, `DeclKinds`
  (`stops_dotted_head`, `module_pos`), `explicit_open_prefixes`, `pending_items`.
- `crates/sema/src/resolve/model.rs` — `ProjectItems::type_qualified_cases`,
  `type_cases`, `type_paths`, `real_nested_modules` (cross-file / same-file case
  and type indices).
- Tests: `crates/sema/tests/all/resolve_type_qualified_cases.rs`
  (`nix develop -c cargo test -p borzoi-sema --test all
  resolve_type_qualified_cases::`).
- Memory: `sema-resolver-name-resolution-correctness` (point 8 onwards) records
  the per-round history and the "model precedence up front" lesson.

## Landed: type accessibility on the cross-file case index

The `type_qualified_cases` index carried each case's handle but not the declaring
type's **accessibility**, so an inaccessible `type private Foo`'s case resolved
cross-file (`open B; Foo.Red` from an unrelated namespace bound `B.Foo.Red`, where
FCS reports `Foo` inaccessible — FS0039). The case's `ExportedItem` already
computes an access-root from the type's privacy; that access-root is now threaded
into the index and `ProjectItems::type_qualified_case(path, site)` gates on
`accessible_from`. Accessible cases (public, or `private` from a descendant of the
container) are unchanged; only inaccessible ones flip from a wrong target to a
sound defer. This is the type-accessibility foundation the cross-tier
compound-name fold (`docs/project-type-member-plan.md`) needs.
