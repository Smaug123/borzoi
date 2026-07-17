# .fsproj consumption plan (LSP)

> Companion to [`fsproj-parser-plan.md`](completed/fsproj-parser-plan.md), which
> covers the msbuild parser (a self-contained crate producing a `ParsedProject`);
> this one covers **consuming that output inside the LSP**.
>
> **Status.** All three consumers' LSP-side wiring has landed (see "Landed"
> below). The only fsproj-consumption work this doc still *owns* is the
> non-ancestor linked-file tail (flagged, shelved — see
> [Workspace index tail](#workspace-index-tail)); consumer #2's remaining
> resolution coverage is sema-crate work tracked in its own plans, and consumer
> #3's detail lives in
> [`fsproj-project-graph-plan.md`](completed/fsproj-project-graph-plan.md).

`Workspace::evaluate_project`
([`workspace.rs`](../crates/lsp/src/workspace.rs)) runs the parser at runtime —
`parse_fsproj_with_imports` with the SDK and filesystem-backed glob resolvers, on
first lookup of each `.fsproj`, caching the `Option<ParsedProject>`. The three
consumers below turn its `items` (Compile list, each `ResolvedItem` carrying a
`span`), `project_references`, `define_constants`, and `diagnostics` into LSP
behaviour.

## Landed

### Consumer #1 — project-ownership refinement (#255)

Ownership became "the project whose evaluated `<Compile>` list contains the
file" rather than the old alphabetically-first-nearest-ancestor directory
heuristic. Stages 1.0–1.2, all in [`workspace.rs`](../crates/lsp/src/workspace.rs):

- **1.0** — pure membership predicate `project_contains`, plus the shared
  [`crate::paths`](../crates/lsp/src/paths.rs) module (`lexically_normalize` /
  `paths_equal`, extracted from `publish.rs`). Property-tested (self-membership,
  spelling invariance, non-membership, per-platform casing).
- **1.1** — `Workspace::owning_project` enumerates candidates and picks by
  membership (sibling ambiguity); `symbols_for` routes through it, falling back
  to the old rule so it can only refine a pick, never turn `Some` into `None`.
- **1.2** — keep-climbing past the first directory-with-project so a higher
  linking ancestor can claim the file.
- **Refinement (#611)** — ownership is three-valued `Membership`
  (`Member`/`NotMember`/`Unknown`) via `Workspace::membership`. A project whose
  Compile set is untrustworthy (`items_uncertain`) or that failed to evaluate is
  `Unknown`, so the walk neither climbs past it nor trusts its listed items.
  Gated on the narrow `items_uncertain`, **not** `is_partial` (which flips on
  essentially every real SDK project and would suppress ownership).

**Decision C3 (durable — cited from `workspace.rs`).** Membership comparison is
**lexical** (via `lexically_normalize`), not `std::fs::canonicalize`: the parser
passes literal `<Compile>` includes through whether or not they exist on disk, so
a freshly-added file must still count as owned. Consequence: two paths differing
only by a symlink compare unequal. Acceptable — F# Compile includes are lexical
relative paths the parser has already joined+normalised against the project dir.

### Consumer #2 — compile-order-aware semantic layer

The sema→LSP wiring the [`sema` plans](type-checker-plan.md) defer. Built in
[`semantic.rs`](../crates/lsp/src/semantic.rs): `parses_for_project` /
`assembly_env_for_project` / `resolved_project_for` feed the FCS-differential
`sema` crate the real Compile order + `AssemblyEnv` and cache the
`ResolvedProject`; the definition / references / hover handlers and
`workspace/symbol` consume it. `ResolvedItem.span` is the parse-don't-validate
anchor for project-level diagnostics. No further LSP-side wiring is blocked:
remaining resolution gaps (overload resolution, extension members, SRTP,
units-of-measure, project-defined receiver/member typing) are sema-crate work
tracked in the sema implementation plans, not here.

### Consumer #3 — inter-project dependency graph

Fully landed; stage-by-stage status in
[`fsproj-project-graph-plan.md`](completed/fsproj-project-graph-plan.md). In brief: pure
builder + `Workspace::project_graph` (#261), buffer-local `<ProjectReference>`
diagnostics (#267), `RestoreStale` (#365), entry-anchored `ReferenceCycle`
(#371), and Stage 3.3 (F# project-ref outputs #854 via
`semantic::fsharp_project_ref_dlls`, C# sidecar metadata #866, entry-TFM
selection #878/#879, graph-sourced reference edges 3.3d) folding into the runtime
`AssemblyEnv`. Graph diagnostics are anchored on the open buffer by design; no
cross-file publishing.

## Still to do

### Workspace index tail

The **non-ancestor linked-file** case is out of scope for ownership and remains
unsolved: a project that is not an ancestor directory of the file links it in
(e.g. `/repo/Shared/Foo.fs` linked into `/repo/ProjA/ProjA.fsproj`, with no
project under `Shared/`). An ancestor walk cannot reach `ProjA`. (This is the
"link case" `semantic.rs` cites as out-of-scope; note `semantic.rs`'s cache
invalidation already handles a shared file living in *multiple* projects'
Compile lists.)

Closing it needs a workspace-wide file→project index (enumerate every project,
index its `items` by file). That is a **separate follow-up**, *not* delivered by
consumer #3 — #3's graph is a targeted edge-following project→project walk from
one entry, not a whole-workspace `items` scan. A `ProjectIndex` was designed, its
pure core spiked, then shelved: it cannot be both sound and useful with the
current msbuild diagnostics (skipped conditions / `<Choose>` / imports can hide a
`<Compile Remove>`, so admitting them over-claims ownership while excluding them
makes the index inert on real SDK projects). Post-mortem and what would unblock a
future attempt: [`workspace-index-plan.md`](workspace-index-plan.md).

## Risks / residuals

- **Lexical (not symlink-aware) comparison** (decision C3 above). Residual; F#
  includes are lexical in practice. Revisit only if a real project breaks.
- **First-lookup cost** in an ambiguous/deep tree: evaluating several candidate
  projects. Bounded by the ancestor-project count; evaluation is cached (one
  `Workspace.projects` memo, no second ownership memo added — measure first).
- **Cache staleness** is now closed by `workspace/didChangeWatchedFiles`
  invalidation (#353/#355; see
  [`file-watch-invalidation-plan.md`](completed/file-watch-invalidation-plan.md)),
  not left to server-lifetime caching.
