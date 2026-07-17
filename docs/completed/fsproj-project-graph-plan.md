# Inter-project dependency graph (fsproj consumption #3)

> Completed record for **consumer #3** of
> [`fsproj-consumption-plan.md`](../fsproj-consumption-plan.md). Companion to
> [`csharp-sidecar-plan.md`](csharp-sidecar-plan.md),
> [`project-assets-plan.md`](project-assets-plan.md), and
> [`multi-tfm-resolution-plan.md`](multi-tfm-resolution-plan.md).

> **Status: complete.** The transitive project→project graph, its entry-anchored
> `.fsproj` diagnostics, and the full resolve-to-references fold (F# outputs +
> C# sidecar metadata + package/framework assets, under a coherent TFM policy)
> all shipped. Everything below the landed-stages list is durable reference
> (the settled decisions, still cited from code, plus the out-of-scope items
> and residual gaps).

## Landed stages (one line each)

- **3.0** (#261) — pure graph builder over an injected resolver
  ([`project_graph.rs`](../../crates/lsp/src/project_graph.rs)): `NodeResult`/`Edge`,
  `ProjectKind`/`classify`, `build_graph`, and `GraphProblem`
  (`NotFound`/`Cycle`/`UnsupportedKind`); recurses only through F# nodes;
  property-tested for termination, closure completeness, determinism, dedup, and
  problem detection.
- **3.1** (#261) — real resolver `Workspace::project_graph` in
  [`workspace.rs`](../../crates/lsp/src/workspace.rs): evaluates each project fresh
  off-cache (E7) and maps `project_references` to normalised edges; integration-tested.
- **3.2** — entry-anchored `.fsproj` diagnostics in
  [`fsproj_diagnostics.rs`](../../crates/lsp/src/fsproj_diagnostics.rs): buffer-local
  `ProjectReferenceNotFound` + `UnsupportedReferenceKind` (#267), `RestoreStale`
  (#365), and `ReferenceCycle` (#371). The graph is built off-cache, gated on
  buffer == disk, and feeds `graph_diagnostics` / `restore_diagnostic`. Every graph
  diagnostic is anchored on the open buffer's own participating `<ProjectReference>`
  (no cross-file publishing, no cross-entry dedup); a dependency's own problems
  surface only when it is the open entry.
- **3.3a** (#854) — F# project-reference outputs fold into the runtime
  `AssemblyEnv` via `semantic::fsharp_project_ref_dlls` / `locate_fsharp_output_dll`,
  locating the built `bin/<config>/<tfm>/…dll` under the producer's *evaluated*
  output name (`ProjectNode::output_name`, assets-name fallback); an unpinnable
  name or a multi-TFM output with no restore data declines rather than guesses.
- **3.3b** (#866; bundling/discovery #856/#861) — C# project-reference metadata
  via `semantic::csharp_project_ref_dlls` driving the sidecar (`SidecarManager`)
  for each direct `.csproj` ref in the entry's closure; transport failures mark the
  env non-cacheable, every other failure degrades to under-resolution.
- **3.3c** (#878/#879) — TFM and `$(Configuration)` selection policy, threaded
  through both the parse/defines side and the assembly env; full design in
  [`fsproj-tfm-selection-plan.md`](../fsproj-tfm-selection-plan.md).
- **3.3d** — graph-sourced reference edges: `semantic::graph_ref_targets` +
  `Workspace::project_graph_with_producer_tfms` derive the fold's edge sets from the
  *parsed* graph (E1), not stale assets — the transitive F# closure feeds the output
  locator, the `.csproj` boundary nodes feed the sidecar. The compile-reference walk
  drops `ReferenceOutputAssembly="false"` / compile-`ExcludeAssets` edges, evaluates
  each node under its restore-selected TFM (`NodeTfm` `Known`/`NoneDeclared`/`Unresolved`),
  refuses a node's edge set when `ParsedProject::project_references_uncertain` is set,
  and distrusts an unpinned/SDK-tainted body `TargetFramework` (`tfm_untrusted`,
  `ServedTfm::Untrusted`) and shape-sensitive `LangVersion`. Differentially pinned by
  [`reference_set_msbuild_diff.rs`](../../crates/lsp/tests/all/reference_set_msbuild_diff.rs)
  against `dotnet build`'s actual reference set (certain-implies-exact, on assembly
  simple-name sets).

---

## Settled decisions (reference)

Still cited from code (e.g. `project_graph.rs` references E3/E5).

- **E1. Edges come from `project_references`, not `project.assets.json`.** The
  parsed `project_references` is the source-of-truth edge set: editor-current,
  available before `dotnet restore`, and each edge carries the XML span diagnostics
  need. `project.assets.json` is a post-restore artifact, not the edge source.
- **E2. Two views, two roles.** Edges (who references whom) come from the parsed
  graph; resolved *artifacts* backing an edge (package/framework DLLs, producer TFMs,
  C# metadata) come from `project.assets.json` and the sidecar. On drift,
  correctness-over-availability decides: an edge with no backing assets emits
  `RestoreStale`/`RestoreNeeded` (never fabricate); an assets-only edge the parsed
  graph lacks is ignored (parsed graph wins for edges).
- **E3. Node classification by extension; recurse only through F#.** `.fsproj` →
  recurse; `.csproj` → terminal boundary (existence checked, edges not followed — the
  sidecar owns the C# subtree, §D7); anything else → `UnsupportedReferenceKind`,
  terminal.
- **E4. Pure graph core; IO/evaluation in the shell.** The builder is a pure
  function over an injected `resolve: &Path -> NodeResult` (`Resolved(Vec<Edge>)` |
  `NotFound`): it owns traversal, classification, cycle-breaking, and ordering; the
  shell supplies `resolve`.
- **E5. Cycles and missing targets are reported, not fatal.** Cycles broken by a
  visited set on the lexically-normalised (not case-folded) path; a cycle is a
  `ReferenceCycle` diagnostic, a missing target `ProjectReferenceNotFound`. Neither
  aborts the graph.
- **E6. Deterministic traversal.** Edges kept in document order; walk is
  depth-first, document order, visited-set dedup — a tested property.
- **E7. Fresh graph evaluation with existing SDK resolution.** `Workspace::project_graph`
  evaluates projects fresh off-cache using the existing SDK/import/glob machinery, so
  graph diagnostics stay current without a second evaluation semantics.

## Out of scope

- **Workspace-wide file→project index** (would close consumer #1's non-ancestor
  linked-file gap) — a *different* structure from this edge-following graph; a
  separate follow-up, not delivered here.
- **Parsing C# project semantics in Rust** — C# subtrees are the sidecar's domain (E3).
- **Publishing transitive problems onto other files**; cycles/broken-refs not
  involving the entry (surface when that file is opened); restore staleness for
  dependencies.

## Residual gaps

Documented limitations, all under-resolving (never wrong):

- A C# edge added but not yet restored is skipped (no recorded producer TFM;
  `RestoreStale` warns). Likewise an unrestored multi-TFM F# sibling with several
  built variants and no restore data to select one.
- An F# project referenced *by* a C# project is invisible to the fold (the graph
  never recurses into C#); the boundary csproj's own metadata is also missing because
  the sidecar hard-fails a C# build whose closure contains an `.fsproj`. Pinned as
  exactly those two names by `fsharp_behind_csharp_boundary_is_a_pinned_gap`; the fix
  lives in the sidecar.
- A dependency `.fsproj` that fails to *evaluate* contributes no further edges (E4).
- **`project_references` partiality.** The env fold now refuses an uncertain node's
  edge set (`project_references_uncertain`, 3.3d), but the graph/diagnostics side
  still follows every declared `<ProjectReference>`; graph *absence* is not yet a
  proof of no-edge.
- **No file-watch invalidation of the graph itself** — cycle diagnostics rebuild
  fresh when the buffer matches disk; a saved `.fsproj`/sibling-rebuild reaches the
  env fold via `didChangeWatchedFiles` → `invalidate_all`
  ([`file-watch-invalidation-plan.md`](file-watch-invalidation-plan.md)).
