# Plan: `open <assembly module>` — bring a referenced module's contents into scope

> **Status:** The fold has LANDED for all three halves — the assembly **module**
> half (Slices A + B/C), the assembly **namespace** half, and the **project**
> namespace half. `open M` now resolves M's value / union-case / exception /
> active-pattern / nested-type surface the way FCS folds it, precedence falling out
> of push order (latest-wins). Outstanding: submodule **dotted heads** (Slice B),
> option-B bare-name **slot eviction** (§8), **value-binding accessibility**, the two
> §5a over-conservative cells, and the **implicit-open** placement. Reference-order
> merges stay declined by design (§4c). Leave in `docs/` until those close.

## How it resolves now ("the fold")

Each interpretation path of an `open` is a **group**: the project module half, the
assembly module handles, and the namespace readings. FCS maps one head to *every*
same-named module/namespace and folds them all, so precedence is structural (latest
push wins), not proved.

`AssemblyEnv::open_fold_surface(handle)` (`crates/sema/src/assembly_env.rs`) folds a
module handle's complete-or-opaque bare-name surface in FCS's order:

1. exception constructors → value **and** pattern scope;
2. the **tycon tier** (`fold_tycon_tier`): nested type names as opaque value-slot
   contestants, non-RQA union **cases** (from the pickle, `Entity::union_case_names`)
   into value *and* pattern scope;
3. **vals** — definite targets, with active-pattern tags into pattern scope;
4. `[<AutoOpen>]` submodules, recursively.

For the assembly namespace, the namespace half joins the fold
(`AssemblyEnv::open_namespace_fold_surfaces`):
a referenced namespace's own tycon tier + `[<AutoOpen>]` submodules as one more
surface, reusing `fold_tycon_tier` with `push_type_names = false` (a namespace-level
type occupying FCS's constructor slot is the head-slot eviction channel,
`public_types_named`, not the bare-name fold). A namespace surface commits **no
definite `Entity` targets** — its exception constructors fold *opaque* (§8 option A).
The **project namespace half** joins as a contestant-only surface plus a generation
barrier when it hides names (`open_project_namespace_values`, `resolve/lookup.rs`).

Residue is name-unknown loss only, two-tier:
- `OpenFoldSurface::residue` — unknowable pickle / undecodable member / nameless
  case in a *recursed* auto-open child: bump the generation barrier (staling
  everything folded earlier) and demote own entries to `Deferred`.
- `OpenFoldSurface::residue_below_vals` — loss confined to the top container's own
  tycon tier: barrier rises and cases demote, but the module's own vals stay definite
  (FCS folds tycons before vals).

*Fold, don't prove.* Rounds 11–19 each proved one *pairing* couldn't be outranked
because we resolved against a lossy projection — a non-compositional proof per pairing.
The fold makes the model isomorphic to FCS's flat maps, so precedence is structural and
nothing needs proving. The old `certain` gate, the pre-loop barrier, and the
`ModuleOpenSurface`/`module_open_is_fully_enumerable` classifier are all **deleted**;
what remains conservative is named (`residue`, `residue_below_vals`, `cross_kind`).

## Landed (one line each)

- **Slice A** (found by the extension-visibility matrix, `crates/sema/tests/all/extension_visibility_matrix.rs`, PR #916) —
  `open_interpretations` gains `AssemblyModule`, pushes the module's values bare via
  `open_type_statics`, drops the blanket `opaque_value_open`; RQA imports nothing (Q5);
  the three `KNOWN_GAPS` cells deleted.
- **The fold — assembly module half (Slices B/C)** — `OpenFoldSurface` enumerates union
  cases, exception ctors, active-pattern tags and nested type names; the `certain` gate /
  pre-loop barrier / `ModuleOpenSurface` classifier deleted.
- **Assembly namespace half** — `open_namespace_fold_surfaces`; the `has_namespace` arm of
  the `cross_kind` demote deleted, replaced by per-name collision inside the fold writer
  plus the `cross_kind_ns_type` barrier (`decls.rs`).
- **§7 matrices** — `namespace_fold_matrix.rs`, `module_open_matrix.rs`,
  `project_half_matrix.rs` (harness `common/fold_matrix.rs`): the open-shapes product as
  FCS-diffed cells with a `KNOWN_GAPS` ratchet, the fold's executable specification.
- **Project-half machinery slice** — the `is_project_namespace_path` arm of the
  `cross_kind` blanket demote deleted; a project name outranks a same-named assembly one
  by push position (Q14); `open_project_namespace_values` recurses into `[<AutoOpen>]`
  submodules; a generation barrier when it may hide names.
- **§8 option A** — a namespace surface commits no definite `Entity` targets; exception
  ctors fold opaque; `demote_pattern_shadowed_exceptions` handles the module-half
  exception-vs-literal pattern contest.

## 1. The gap (why it mattered)

An explicit `open` of a **module of a referenced assembly** used to bring *nothing* into
scope and, worse, turn the whole file's open environment **opaque**: falling through to
no interpretation set `opaque_value_open`, so `lookup` skipped *every* opened entry — a
single `open MyLib.Helpers` blanked out bare-name resolution for every other open in the
file. In a multi-project solution every cross-project module open hit this (a referenced
project is an assembly to sema). Always a deferral, never a wrong target (D5-safe), which
is why it went unnoticed — but likely one of the largest availability losses left.

## 2. What FCS does — `AddModuleOrNamespaceContentsToNameEnv` (fold-order reference)

Opening a module adds, in order (`NameResolution.fs`): (1) its **exception definitions**
into value *and* pattern scope; (2) its **types** — each tycon's *static parts*
(`AddStaticPartsOfTyconRefToNameEnv`): union cases and record labels into
unqualified/pattern scope (unless IL or `[<RequireQualifiedAccess>]`), and the type name
as a constructor; (3) its **vals** — non-members into unqualified items, extension members
into the extension tables, active-pattern tags into pattern scope; (4) **recursively, its
`[<AutoOpen>]` submodules**. The fold above is isomorphic to this list.

## 3. Oracle answers (fsi, against a real referenced assembly — reference)

Probed with a purpose-built `ProbeLib.dll` (+ a second assembly for Q9/Q13). Each cell is
referenced by number from code and other sections.

| # | Question | **Answer** |
| --- | --- | --- |
| Q1 | `open M`: are `M`'s nested union's **cases** bare-resolvable? | **yes** (`Red`) |
| Q2 | …its nested **types** bare-nameable? | **yes** (`Nested`) |
| Q2b | …and does a later `open` of `M` shadow an earlier open's same-named type? | **yes** — latest open wins |
| Q3 | …its **exception** constructors? | **yes** (`MyErr 3`) |
| Q4 | …its **active patterns**, in pattern position? | **yes** (`match 4 with Even -> …`) |
| Q5 | `[<RequireQualifiedAccess>]` **module**: values bare after `open`? | **YES — and the `open` is *also* an error (FS0892).** FCS reports the error and still enters the module's contents. Dropping the module from the walk is a *wrong target*, not a deferral. Emitting FS0892 is a Phase-4 follow-up. |
| Q6 | `[<RequireQualifiedAccess>]` **union** nested in an opened module: cases bare? | **no** (FS0039) |
| Q7 | A transitive `[<AutoOpen>]` submodule of an *explicitly* opened module — opened too? | **yes** (`innerVal ()`) |
| Q8 | Precedence between two module opens with a colliding value name? | **latest open wins** |
| Q9 | A path that is **both** an assembly module and an assembly namespace? | **both are opened and merge** — only expressible *across two assemblies* (within one it is FS0247). |
| Q10 | Does an opened module's *nested module* become a dotted-path head (`open M` then `Sub.f`)? | **yes** (`NotAuto.subVal ()`) — **still deferred, Slice B below** |
| Q11 | …its nested record's **labels** (bare `{ F = 1 }`)? | **yes** — `eFieldLabels` |
| Q12 | Fully-qualified access through a submodule with no `open`? | **yes** (works today) |
| Q13 | Two referenced assemblies exposing the **same module FQN** — merged? | **yes**: unique values of both resolve; a colliding name binds the **later-referenced** assembly's. Reference order is not modelled, so a collision **defers**. |
| Q14 | A **project** module and a **referenced** module at the same FQN — merged? | **yes**: both halves import, a colliding name binds the **project**'s (folds last). |
| Q15 | A **type + suffixed companion module** sharing a name (`type Tagged` + `TaggedModule`)? | **the module is imported** (a module-path walk prefers the module). |
| Q16 | The same module FQN **encoded differently** across DLLs? | **yes** — both encodings contribute; a walk that stops at the first metadata split drops one. |
| Q17 | A `decimal` `[<Literal>]`? | **resolves** — the one literal the CLI cannot express as such: fsc emits an init-only field with `[DecimalConstantAttribute]`, not a CLI `Literal`, so a flag check under-reports. |

**Scope consequence.** Q1/Q3/Q4/Q11 all say *yes*: opening a module imports a
**pattern/label surface**, not just values — which is why Slice C (union cases,
exceptions, active patterns) was not optional polish.

## 4b. The blacklist→whitelist inversion (lesson — cited from code)

> The predicate this section once described (`module_open_is_fully_enumerable` /
> `ModuleOpenSurface`) is **deleted** — the fold's `OpenFoldSurface` enumerates the names
> instead of classifying their absence. The lesson stands and is why the fold's residue is
> **conservative by default**: anything not explicitly enumerated is residue.

Early review rounds found seven separate surfaces a barrier had missed (literals,
delegates, abbreviation markers, RQA struct unions, dropped types, undecodable members,
unknowable pickles). That is one mistake, not seven: **no blacklist can name what the
model does not represent** (a `[<Literal>]` was projected as *no member at all* —
invisible). Two consequences, both implemented: **fix the model, not the consumer** (a
module `[<Literal>]` is now projected, `is_module_literal` elides it in the differential
normaliser), and **default to conservative** (anything the fold cannot enumerate raises
the barrier). Corollary caught the hard way: `[<AutoOpen>]` is a per-*attribute* hazard,
not per-*kind* — `CanAutoOpenTyconRef` auto-opens any non-generic F# **type** (record,
class, interface, RQA union) carrying the attribute, so the auto-open check sits ahead of
the kind match and applies to every child (deliberately coarser than FCS's non-generic /
non-IL conditions — costs availability, never correctness).

## 4c. The merge cut (rule + invariant — cited from code)

FCS merges everything that lands at one module FQN (several assemblies, a module here and
a namespace there, two metadata encodings) and folds the halves in **reference order**.
Reference order is not a resolution input sema models. Rounds 5–12 each tried to enumerate
precisely *which* names the halves contest and each shipped a narrower version of the same
blacklist bug (§4b, one level up).

**The invariant, in one sentence:** *A merge names a definite target only when **every**
half is fully enumerable.* With the fold, a cross-kind or cross-assembly path is one group
whose halves collide **per-name** inside the writer (a name both supply defers; a name
unique to one resolves) and whose name-unknown residue feeds the group's generation
barrier — so a name unique to one half resolves, a contested one defers, and a hidden name
anywhere stales earlier groups. Reference-order genuinely-ambiguous collisions (two
assemblies) still defer; the project half's position is *fixed* (folds last, Q14), so it
resolves by push position rather than deferring.

**If you come to widen the declined cross-assembly merges**, the only sound way is to make
reference order a modelled resolution input (`AssemblyEnv` carries the reference sequence,
the folder respects it) — not to re-enumerate the contested set. Larger than Slice A; its
own plan.

## 5. Known residue kept conservative (reference)

Whatever the fold cannot enumerate keeps a *narrowed* conservative gate: active-pattern
tags in a residue-bearing recursed child; a module whose pickle did not decode
(`ExtensionMembers::Unknowable`); submodules / nested types as dotted-path heads
(`opaque_dotted_open`, Slice B below); an F# assembly with no authoritative host pickle
(abbreviations live only in the pickle).

---

## Still to do

### Slice B — submodule dotted heads (Q10)

`open M` then `Sub.f ()`. The tiered assembly walk roots a path at a *namespace* prefix,
and an opened module is not one — so the fold leaves this deferring
(`opaque_dotted_open`, raised only for a module that has nested members, since a childless
one can seed no dotted head and blanketing it would suppress the merged namespace half of
the same path — Q9). It needs a module-handle prefix channel the walk can descend through
`nested()`. Pinned by `a_submodule_of_an_opened_assembly_module_never_names_a_wrong_target`
(the safe half) plus the `#[ignore]`d target-behaviour test
`crates/sema/tests/all/resolve_autoopen.rs:994` — remove the `#[ignore]` and watch it fail
as step one. The `module_open_matrix.rs` `KNOWN_GAPS` include the submodule and
nested-type dotted-head cells; lifting this flips them.

### 5a. Over-conservative cells left standing (deliberate)

Two places where we **defer though FCS resolves** — both availability losses in the safe
direction (never a wrong target), left standing because closing them re-grows the
machinery §4c cut. Both pinned by `#[ignore]`d tests carrying the FCS-correct expectation;
whoever picks this up starts by removing the `#[ignore]` and watching them fail.

- **A companion module behind a type-index collision.** When a path carries both a
  type/abbreviation and a suffixed companion module, `opened_assembly_type` returns the
  type-index winner while `opened_assembly_module` returns the module, so the guard's
  `h == handle` identity test fails and the abbreviation branch raises `opaque_value_open`;
  `open Lib.Companion; fromCompanion` defers even though the fold can enumerate that module.
  The fix is to ask whether the path *has* a module interpretation rather than whether it is
  the *same handle* — it re-opens the kind-collision seam, so it belongs with Slice B's walk
  rework. Pinned by `resolve_fsharp_abbrev.rs:365` (`#[ignore]`).
- **The incomplete-prefix veto ignores precedence.** `incomplete_open_prefixes` is a
  non-empty-vector test, so any incomplete prefix in scope vetoes a later `open Sub` even
  when a *newer, definite* prefix would outrank it. Making the veto precedence-aware means
  modelling rank between shortening prefixes — the same "model the contest exactly" class as
  rounds 5–12. Its own slice with its own oracle work. Pinned by `resolve_assembly.rs:2772`
  (`#[ignore]`, `a_newer_definite_prefix_outranks_an_incomplete_one`).

### Value-binding accessibility (§7 residual debt — own feature)

The project-half fold surfaced several places where value-binding accessibility has **no
model anywhere in `borzoi-sema`**, so these apply to the pre-existing plain `open M`
path too, not just the new recursion:

- **`let private Secret`** in a project module is exported with a `qualified` path exactly
  like a public binding (`Resolver::qualified_export_path` only excludes an anonymous-root
  file; `prepare_binding` never inspects an accessibility modifier), so any `open` that
  reaches it brings it into scope.
- **A same-file `private` project type** remains an accessible contestant for descendants
  of its enclosing namespace, but `direct_project_type_contestants` reuses the cross-file
  (privacy-downgraded) `SlotClass`, wrongly treating it as a non-contestant. Needs a
  same-file, non-downgraded, accessibility-aware slot-class lookup that does not exist yet.

Closing these needs an accessibility flag on `ExportedItem`, threaded through
`ProjectItems::by_qualified_path` / `direct_value_children` and filtered in
`open_module_values` — its own feature, not a namespace-fold patch. (`extern` bindings are
separately, deliberately never interned as usable bare-name values — a pre-existing gap
that also predates this work.)

### 8. Bare-name slot eviction — option B (recovery slice, not started)

> **Option A is applied** (see "the fold" above): a namespace surface commits no definite
> `Entity` targets; its exception constructors fold opaque (`open_namespace_fold_surfaces`
> post-pass, covering exceptions in recursed `[<AutoOpen>]` modules too, and demoting-by-
> default any future `Entity` commitment — §4b's lesson). Cell **8a**
> (`a_later_namespace_type_evicts_an_earlier_namespace_exception`) is live and green; cell
> **8b** is pinned by the matrix's `exn-lit` cells (`Demo.NsFold.ExnLit`). The
> exception-availability cells stay `KNOWN_GAPS` — recovering them is option B.

FCS re-orders a bare name against a *later* same-named constructible type (its unqualified
constructor slot evicts an earlier value/exception, cell 8a) or against a *literal* (a
constant pattern, cell 8b). Sema's bare-name lookup models neither, and the module fold
commits exceptions definitely with the same unmodelled eviction. **Option B** keeps the
definite targets and teaches the resolver FCS's ordering: a later same-named constructible
type stales earlier **opened** bare entries (by `from_open`, *not* by generation alone —
that distinction is what round 4's cross-kind gate was avoiding, so a pure-namespace open
must not stale a preceding local), and a pattern contest becomes literal-aware (including
the `decimal` `DecimalConstantAttribute` case, Q17). Recovers the availability; a real
change to the eviction machinery that applies to the module fold too. Its own slice, with
`Demo.NsFold` matrix cells for the eviction and pattern-literal contests.

### Implicit-open placement (own slice)

The namespace fold runs only for an *explicit* `open`. The **implicit** auto-open of the
enclosing / assembly-level `[<AutoOpen>]` namespaces (`resolve.rs`,
`open_auto_open_modules_in`) still imports only auto-open *module* statics, so a direct
namespace-level exception or union case of an implicitly-opened namespace
(`MatchFailureException` in `Microsoft.FSharp.Core`) stays `Deferred` where FCS resolves it.
Sound, but an availability asymmetry. Not a mechanical reroute: the implicit set includes
`Microsoft.FSharp.Core`, whose direct tycons include `option` / `Result` — folding their
cases would put `Some` / `None` / `Ok` / `Error` into every file as *opaque* entries and
shadow their fundamental handling. So it needs the fundamental cases modelled (definite
targets, not opaque) first — its own slice, with `Demo.NsFold`-style cells for the implicit
placement.

## 6. Why this before A4/S4

A4/S4 (operator demangling + opening the module-shaped assembly auto-opens) is the bigger
user-visible prize, but it is also a "what enters unqualified scope" change and lands on
this same machinery. Fixing the module-open channel first means A4/S4 arrives with the
matrix already covering its failure modes — and unlike A4/S4, this gap silently degraded
every file that opens a library module.

## 7. The open-shapes matrix (systematic-testing debt — LANDED)

The twenty review rounds on this seam were a human-powered search over an **enumerable**
space: (child shape) × (placement: opened module / auto-open child / namespace half /
project half) × (contest). The matrix in the extension-visibility mould
(`extension_visibility_matrix.rs`, PR #916, which found Slice A's gap on its first run) now
covers all placements: `namespace_fold_matrix.rs`, `module_open_matrix.rs`,
`project_half_matrix.rs`, sharing `common/fold_matrix.rs`. Each generates a fixture assembly
(or a pair for cross-assembly cells) carrying every child shape, enumerates the open-shapes
product as cells, and diffs each against the fsi-verified expectation with the extension
matrix's `KNOWN_GAPS` bijection (**we commit `X` ⟹ FCS agrees** for soundness; **FCS
resolves ⟹ we resolve or a listed deferral** for the availability ratchet). The fold makes
each cell's expected value computable, so the matrix doubles as the fold's specification;
cells the fold defers on purpose (cross-kind, cross-assembly, dotted heads, opaque cases)
are pinned as deferrals, and lifting one flips a cell rather than waiting for review round 21
to find it.
