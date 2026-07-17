# Workspace-wide file→project ownership index — **SHELVED**

> **Status: shelved (2026-06-06; still current 2026-07-09).** The index was
> designed and a Stage-1 pure core spiked, then shelved. The original
> *correctness* blocker is resolved (#611); it stays unbuilt only because no
> per-file consumer has yet demonstrated demand. This is a post-mortem plus a
> ready-to-go design sketch, not a live plan.

## What it would have solved

`Workspace::owning_project` ([`workspace.rs`](../crates/lsp/src/workspace.rs))
walks a file's **ancestor** directories to find the project whose evaluated
`<Compile>` list contains it. That cannot reach a project which **links** a file
from outside the file's ancestor chain:

```
/repo/Shared/Foo.fs                                  (no .fsproj under Shared/)
/repo/ProjA/ProjA.fsproj   <Compile Include="../Shared/Foo.fs"/>
```

`Foo.fs`'s ancestors are `/repo/Shared` and `/repo` — neither holds
`ProjA.fsproj`, so ownership falls back to a directory heuristic. The
consequence: `symbols_for` hands back the implicit symbol set for the file kind
(e.g. `{COMPILED, EDITING}`) instead of ProjA's `DefineConstants`, so `Foo.fs`'s
diagnostics are evaluated against the wrong `#if` branches. This is the
"consumer #1 tail" flagged in
[`fsproj-consumption-plan.md`](fsproj-consumption-plan.md).

## The approach

A pure `file → owning project(s)` index: discover every `.fsproj` under the
workspace roots, evaluate each (cached), and **invert** each project's resolved
`<Compile>` items into a map, consulted **only after a conclusively-clean
ancestor-walk miss** (so it never overrides the sound ancestor logic). The pure
core (incremental builder + `owners` lookup) is straightforward. The
load-bearing piece is the **admission gate**: which projects' item lists can be
trusted for ownership.

A sound gate cannot admit any project whose skipped constructs might have
*hidden a removal* — a skipped `<Compile Remove>`, `<Choose>`, or unfollowed
`Directory.Build.targets` can leave a file in `items` that MSBuild would have
removed, so admitting it can assign a file to a project MSBuild excluded (wrong
owner → wrong `DefineConstants`). The original blocker was that the only signal
available then, `is_partial` (`!diagnostics.is_empty()` in
[`evaluator.rs`](../crates/msbuild/src/evaluator.rs)), was far too coarse:
essentially every real SDK project is `is_partial` from benign SDK noise
(`<Target>`, `<UsingTask>`, property functions), so a sound gate excluded ~all
real projects and the index was inert on exactly its target population.

## Why it stays shelved

The correctness objection is **resolved**. #611 added the narrow signal the gate
needed: `ParsedProject::items_uncertain`, set *positionally* when any Compile
item/group is skipped (or an item-carrying import/SDK/`<Choose>` fails), distinct
from `is_partial`'s benign-noise blanket. `!items_uncertain` is exactly the
"items are exact" admission gate, and `Workspace::membership`
([`workspace.rs`](../crates/lsp/src/workspace.rs)) already uses it for its
Member/NotMember/Unknown verdict. A sound admission gate is therefore now
expressible, and the residual followed-but-unevaluable-import case keeps
`items_uncertain` set (conservatively excluded).

What has **not** changed is demand. The index only pays off for the intersection
of "has a non-ancestor linked file" (a minority but idiomatic F# pattern — a
shared `AssemblyInfo.fs`, `<Compile Include="../Shared/…">`) and a per-file
consumer that lacks the linking project in hand. No such consumer has yet needed
it. Cost is no longer the objection either: `workspace/diagnostic`
([`workspace_diagnostic.rs`](../crates/lsp/src/handlers/workspace_diagnostic.rs),
#352) already performs the O(workspace) discover-and-evaluate-all through the
same cached `Workspace`, so an index would be an inversion of already-cached
data, not a new hot-path sweep.

## What is sound today, and what stays unsolved

- **The ancestor-walk `owning_project` (consumer #1) is the sound core and
  stays.** It was always correct; the index was only ever the unsound extension.
- **The one shipped place the gap fired is fixed.** The `workspace/diagnostic`
  sweep enumerates a linked file *from the linking project's own `<Compile>`
  list*, so the linking project *is* in hand there. `Workspace::symbols_for_linked`
  / `lang_version_for_linked` thread it through: a conclusive ancestor `Member`
  still wins, else a conclusive `Membership::Member` in the enumerating project
  donates its defines and language version, else the old fallback. The
  `items_uncertain` gate keeps it sound — an untrustworthy item list donates
  nothing.
- **Per-file paths with no linking project in hand stay unsolved** —
  `textDocument/diagnostic`, the push path, and `symbols_for` generally still
  degrade a non-ancestor linked file to the implicit symbol set. So for such a
  file the workspace pull is deliberately better-informed than the document pull.
- **`workspace/symbol` stays scoped to open buffers' projects** — its documented
  limitation ([`workspace_symbol.rs`](../crates/lsp/src/handlers/workspace_symbol.rs))
  stands.

## Before building the index proper

The correctness prerequisite (`!items_uncertain`) is in place, and the cost is
bounded (invert the project cache `workspace/diagnostic` already populates, plus
an invalidation story). The remaining trigger is a per-file consumer that needs
non-ancestor ownership *without* the linking project in hand — closing the
`textDocument/diagnostic` / push-path gap above. Until one demonstrates that
demand, the index stays unbuilt; `owning_project` remains ancestor-walk only.
