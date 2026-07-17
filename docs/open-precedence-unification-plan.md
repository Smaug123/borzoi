# Open-precedence unification plan (substep 3 enabler)

> **Status:** the open-frame unification and its follow-ons are landed (#515,
> #535, #538, #545) — all opens now contribute to one source-ordered,
> latest-wins value frame, interleaved with local bindings. Two open-precedence
> follow-ups remain (see "Still to do"); [`type-checker-plan.md`](type-checker-plan.md)
> points here for them. Everything above "Still to do" is a record of landed work.

## Precedence rule (as implemented)

One frame, source-ordered, latest entry wins — local `let`s, opened module
values, and opened type statics all participate equally:

| Source (in a `module N`)                                   | resolves to        | rule |
|------------------------------------------------------------|--------------------|------|
| `let x=99` then `open M` (M.x) then use `x`                | **M.x**            | open shadows an earlier local |
| `open M` (M.x) then `let x=99` then use `x`                | **N.x**            | later local shadows the open |
| `open M` (Zero) then `open type Demo.Calc` (Zero), use     | **Demo.Calc.Zero** | later open-type shadows earlier open-module |
| `open type Demo.Calc` (Zero) then `open M` (Zero), use     | **M.Zero**         | later open-module shadows earlier open-type |

## Landed stages (one line each)

- **substep 1 — `open type T` statics** (PR #515) — unqualified public-static
  members of an opened type resolve.
- **PR 1 — unify the open frame** (#535) — `ScopeEntry::from_open` with
  `::binding` / `::opened` constructors; `open type` statics become
  source-ordered frame entries (replacing the separate `open_types` list +
  `opened_static_member`); the per-block leak guard
  (`frame.entries.retain(|e| !e.from_open)`); simplified bare-name / dotted-head
  resolution. Two `open type`s sharing a static name are latest-wins, not
  ambiguous (pinned in `resolve_assembly_diff.rs`).
- **PR 2 — `open M` module-value opens (substep 3)** (#538) — a plain `open M`
  of an in-project module enumerates M's direct exported values into the frame
  as source-ordered `opened` entries (same-file *and* cross-file); the open path
  is resolved via the same precedence tiers as `open type` using the exact
  `is_project_module_path` predicate. Adds `opaque_dotted_open` (keeps a *dotted*
  head through M's unmodelled submodules conservative without suppressing M's
  modelled bare values, which `opaque_value_open` would). Hardened multi-open
  precedence via a per-`ScopeEntry` **generation** (a later opaque `open` bumps
  it to shadow earlier opens while its own modelled `let`s stay live) and a
  single source-ordered `open_shortening_prefixes` list that
  `resolved_project_module` walks latest-first (correct precedence *across* open
  kinds; `global.`-rooted opens bypass it).
- **module abbreviations** (#545) — `module Alias = Target` recorded in
  `module_aliases` (chains flattened at definition); `resolved_project_module`
  canonicalises a matched alias to its target, so `open Alias`, chained
  `open Alias; open Sub`, and qualified `Alias.foo` resolve through the target.

## Still to do

### Cross-file module-alias declaration

`module_aliases` is same-file only. An alias *declared* in an earlier
Compile-order file is not threaded forward, so `open Alias` / `Alias.foo` in a
*later* file where `module Alias = Target` was declared elsewhere remains a
conservative decline. Threading aliases through the `resolve_project` fold
(alongside the per-file exports it already carries) is the remaining work.

### Member-existence-aware qualifier precedence (String-qualifier bug, #949)

Ignored repro: `crates/sema/tests/all/resolve_string_qualifier_repro.rs`
(deterministic — real `FSharp.Core.dll` + `System.Runtime.dll`, no FCS; run with
`--ignored`).

For `String.Equals(...)` with `open System` then a *later*
`open Microsoft.FSharp.Core`, FCS resolves the `String` qualifier to the **type**
`System.String` (which carries the static `Equals`); we apply latest-open-wins
on the qualifier and land on the same-named FSharp.Core `String` **module**,
whose value set has no `Equals` — a wrong go-to-definition. The fault is in the
open-precedence step, not member lookup: when a qualifier is the long-identifier
head of a member access, the pick must prefer a candidate that actually carries
the accessed member rather than blindly taking the latest open. (The module's
`[<AutoOpen>]` alone does not trigger it — it is the later *explicit* open that
flips the pick.)
