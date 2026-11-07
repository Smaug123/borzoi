# Restore at the dgspec boundary — plan

> **Status:** stages 1–3 and 8 have landed. Stage 8 chose **A₁** (see its
> section for the numbers and the conditions). Supersedes slice 8 of
> [`nuget-restore-plan.md`](nuget-restore-plan.md).
>
> **Goal:** a route away from executing `dotnet restore`
> ([`restore.rs`](../crates/lsp/src/restore.rs)) that stays exact as the SDK
> moves more of restore into its own task code. Two replacements are kept open
> for the half that executes code: a Rust model of the restore targets and
> tasks, or a deterministic in-house .NET runtime that runs the real task DLLs.
> Stage 8 measured what each would have to cover and chose A₁.

## Why the boundary sits here

`restore.rs` runs the real `dotnet restore`. That is exact by construction, but
it spawns a process and executes the project's MSBuild targets, so it needs the
workspace-trust opt-in, and its isolation flags are explicitly not a security
boundary. The in-house route
([`nuget-restore-plan.md`](nuget-restore-plan.md)) was set aside because more
and more of restore is computed during target execution, which no static
evaluation can see.

That code is **compiled task code, not inline C#**. On SDK 10.0.402 none of the
shipped `.targets`/`.props` files uses `RoslynCodeTaskFactory` or
`CodeTaskFactory`. The growth is in compiled tasks: the 19 `NuGet.Build.Tasks.*`
tasks `NuGet.targets` registers, and SDK tasks from
`Microsoft.NET.Build.Tasks.dll` that hook into the restore-input targets,
including `ProcessFrameworkReferences`, `ApplyImplicitVersions` and
`GetPackagesToPrune`.

Restore splits into two functions with one data artefact between them:

- **Half A:** project tree + SDK + global properties → **dependency-graph
  spec** (the "dgspec": `NuGet.ProjectModel.DependencyGraphSpec`, versioned
  JSON, `"format": 1`). This half is MSBuild evaluation plus the restore-input
  targets and their tasks, and it is where the SDK's growth lands.
- **Half B:** dgspec + global packages folder → assets graph. This half is
  `NuGet.Commands`' `RestoreCommand`, which `borzoi-nuget` already reimplements
  and diffs against both restore engines.

`dotnet msbuild -t:GenerateRestoreGraphFile -p:RestoreGraphOutputPath=<file>`
runs half A alone. In a probe on 10.0.402 it finished in about 0.5 s, with
`RestoreSources` pointed at an invalid URL (so no feed was contacted) and no
file written under `obj/`.

### What the dgspec carries that evaluation alone would have to reconstruct

These come from probes of a two-project graph on 10.0.402. `app` targets
`net10.0;net8.0`, uses CPM with transitive pinning, sets
`TreatWarningsAsErrors`, and references `lib`, which targets
`net8.0;netstandard2.0`.

- **Pruning lists.** `app`'s TFMs carry 272 (`net10.0`) and 280 (`net8.0`)
  `packagesToPrune` entries; `lib`'s carry none. The lists name real packages:
  `System.Text.Json` is pruned up to `(,10.0.32767]` on `net10.0`. That covers
  `app`'s own *direct* `System.Text.Json` 8.0.5 reference, and how restore
  treats a pruned direct reference is a question for stage 5's oracle to
  settle, not something to model from memory.
- **Implicit references.** Without CPM, `FSharp.Core` appears as
  `[10.1.402, )`. Under CPM it is absent from both projects. `netstandard2.0`
  gains `NETStandard.Library` with `autoReferenced` and `suppressParent: All`.
- **Targeting packs as downloads.** The `net8.0` TFMs list
  `Microsoft.NETCore.App.Ref` and `Microsoft.AspNetCore.App.Ref` 8.0.32 in
  `downloadDependencies`; `net10.0` lists neither. If one of those is missing
  from the cache, the restore fails.
- **Restore policy.** `warningProperties.allWarningsAsErrors` (from
  `TreatWarningsAsErrors`), `restoreAuditProperties`, `SdkAnalysisLevel`,
  CPM flags, per-TFM `projectReferences`, and sources that include the SDK's
  `FSharp/library-packs` folder.

Every one of these would otherwise have to come out of a model of
evaluation plus targets. In the dgspec they arrive as plain data.

### The gap in what has landed

`borzoi_nuget::resolve_offline(root, tfm, direct)` takes direct requirements
only. It does not model pruning, project nodes, CPM transitive pinning,
download dependencies or the warning policy. The `restore` op in
`tools/nuget-oracle` builds a synthetic dgspec that omits framework
references, fallback imports and pruning, on the grounds that "none of them
names a package in the synthetic universe". Real dgspecs break that premise
(see `System.Text.Json` above), so the resolver has never been graded on a real
input. The resolver is not wired into the LSP, so no wrong answer is live
today.

## Design

### The currency is a value, not an interface

A typed `DependencyGraphSpec` model lives in `borzoi-nuget`, built by parsing,
not validation. Every field is either modelled or, when present with a
non-default value, a typed decline that names its JSON path. An **unknown field
is a decline, not an ignore**, so SDK drift in the dgspec schema shows up as a
decline rather than a silent wrong answer. Only `format: 1` is accepted.

The half-A producers are functions that return this value, and the half-B
consumers are functions that take it. There is no producer trait:

| | producer / consumer | role |
|---|---|---|
| A₀ | `dotnet msbuild -t:GenerateRestoreGraphFile` (spawn) | oracle; also what `restore.rs` runs internally |
| A₁ | `borzoi-msbuild` evaluation + a Rust model of the restore-target cone | chosen at stage 8 |
| A₂ | an in-house deterministic .NET runtime running real `Microsoft.Build` + task DLLs | not chosen at stage 8; see there for what would revisit it |
| B₀ | `RestoreRunner.RunWithoutCommit` over the dgspec | oracle |
| B₁ | `borzoi-nuget` | subject |

### The reference configuration is `restore.rs`'s own

The LSP serves the closure that `restore.rs`'s configuration produces: offline
against an empty source, audit off, package folder pinned, lock files off. It
does **not** serve what the user's own online restore would produce. Every
oracle run in this plan therefore uses exactly `restore.rs`'s global
properties, from one function both call sites share. If the two lists ever
diverged, the differentials below would grade a different function from the one
the LSP serves.

B₁ commits only where B₀ would succeed under that configuration. With an empty
source, a cache miss is a restore failure (NU1101/NU1102), so it declines; a
miss is never a substitution. A dgspec whose sources or fallback folders could
supply a package from outside the pinned cache gets a typed decline.

### Runtime composition

A spawn-free path (A₁ or A₂, then B₁) runs first. When it commits, the LSP
serves its answer. When it declines, `restore.rs` runs exactly as it does today
and stays the exact fallback. Composing A₀ with B₁ at runtime buys nothing:
it still spawns, and certain-implies-exact means it never commits where
`restore.rs` would fail. So nothing changes at runtime until a spawn-free
half A exists (stage 9).

### What A₂ would change

A deterministic runtime that ran the real `Microsoft.Build` evaluator and target
engine, plus `NuGet.Build.Tasks` and `Microsoft.NET.Build.Tasks`, would make
half A exact by construction. It could run half B (`NuGet.Commands`) as well. Its
kernel would control the filesystem and network, so it would also be a *real*
sandbox, which `restore.rs` is not, and that would remove the reason for the
trust opt-in. Its acceptance tests are stages 2 and 3 as they stand: its dgspec
must equal A₀'s, and its closure must equal `restore.rs`'s.

If A₂ arrives, B₁ is no longer needed for exactness. What it keeps is speed and
declines that say why. So stages 1–3 and 8 pay off under every route, while
stages 4–7 are what A₂ would make redundant. That is why stage 4 is kept to
plumbing plus a census. Stages 5–7 waited on stage 8, which chose A₁, so B₁
stays the half-B consumer and they go ahead.

### Per-SDK drift

`restore.rs` needs no per-SDK revalidation. Every replacement does: A₁ models
SDK target files that can change in any release, and B₁ models NuGet behaviour
that `NuGet.Commands` can change. Each therefore keys its trust on the identity
of the code it models:

- **A₁:** the content hash of each SDK file defining a target in the restore
  cone, *and* the identity of each task assembly the cone runs: its
  informational version, which names the commit the SDK was built from. The
  assembly's bytes and module MVID differ between the macOS and Linux builds of
  one SDK, so neither is a portable key. Stage 8 found the spec moving between SDKs in a
  field that task code writes and no executed target changed to produce, so
  target files alone are not the code A₁ models.
- **B₁:** the identity of the NuGet that would perform the restore. That is the
  informational version (or content hash) of `NuGet.Commands.dll` in the SDK
  that `dotnet` resolves for the project, honouring its `global.json`. B₁ takes
  this identity as an input and commits only for identities on a checked-in
  verified list.

B₁ needs its own gate because the dgspec schema check is not enough. A NuGet
release can change resolution behaviour while emitting the same `format: 1`
document, and when A₂ produces the dgspec there is no A₁ file hash in the
chain to catch it. An identity nobody has verified declines to `restore.rs`; it
is never served on the assumption that it behaves like its predecessor.

That puts the same requirement on the oracle: B₀ must run the NuGet it vouches
for. `tools/nuget-oracle` does: it loads the `NuGet.*` set from the directory
of the SDK `dotnet` resolves, and its `restore` op reports the
`NuGet.Commands` that ran (`oracle_identity.rs` pins it to the SDK's file). A
package reference could not do this, because SDKs bundle unpublished release
candidates: SDK 10.0.401 ships `7.9.0-rc.42413+e34a38d2…`, while nuget.org's
`7.9.0` is a different commit. Moving the oracle from the 7.6.0 packages to
that build (the devshell's SDK on 2026-10-30) moved none of the 321,768
distinct answers the `borzoi-nuget` suite and both soaks asked of it (9,572 of
them restores).

## Implementation plan

> Implement this plan with each stage on its own branch, stacked as necessary on
> previous branches, so that a reviewer can review each branch in isolation.

### Stage 1: typed dgspec model and parser — landed

`borzoi_nuget::dgspec::parse_dgspec` reads a dgspec into NuGet's
interpretation of it, or declines naming the JSON path. Its vocabulary is
closed: an unmodelled key, a repeated key, or a spelling outside the modelled
set declines. `tests/all/dgspec_diff.rs` holds it to certain-implies-exact
against `DependencyGraphSpec.Load`, through the oracle's `normaliseDgSpec` op.
That op projects NuGet's object model field by field, and refuses to answer if
the SDK's NuGet has a property the projection does not account for (the oracle
compiles against an older NuGet than it runs). Every case is graded under
the default environment *and* under each environment variable
`NuGet.ProjectModel` consults, since a reading that depends on the caller's
environment agrees with an oracle that shares it. That is how
`NUGET_ENABLE_EXPERIMENTAL_MACROS` was found: it makes the reader expand
`$(User)` in paths, so anything holding `$(` declines. The oracle gates are:

- hand cases pinning which documents must commit and which must decline;
- a generator mutating two templates (respelling values and keys, dropping,
  duplicating and inserting members, confusing types), which must agree at
  least once per mutation kind;
- the absolute property that a key inserted into any object declines at or
  beneath the insertion;
- the warning-code table equal to `NuGetLogCode`'s names;
- an ignored sweep over every `obj/*.nuget.dgspec.json` under
  `BORZOI_DGSPEC_SWEEP_ROOT`.

The generator in place of `resolver_diff`'s graphs reaches the reader's
spelling rules, which those graphs do not exercise. The rules it found, each of
which a naive reader gets wrong:
- a missing `target` is 47, not `Package`;
- `BuildTransitive` implies `Build` in every flags field;
- flag tokens trim spaces but not tabs, and unknown names are dropped;
- `exclude` folds into `include`;
- a `framework` field overrides its alias;
- an absent `targetAlias` is `""`;
- an empty `imports` entry is skipped, and a non-specific one fails the load;
- an empty `suppressedAdvisories` is null;
- an unnamed framework reference fails the load;
- a project reference's empty flags string is the default, though a
  dependency's empty `include` is no flags;
- framework references and `restore` names that differ only in case are
  merged (paths only on a case-insensitive file system);
- warning codes parse case-sensitively, numerically and as flag combinations;
- keys are case-sensitive;
- a non-boolean where a boolean belongs reads as `false`;
- the project key and `format` are ignored.

The generator also found two divergences in the landed framework parser,
reached through dgspec aliases:
- **Salvaged platform suffixes.** NuGet salvages `net8.0-1` (platform `1`); ours
  reads it as unsupported, by design. So the dgspec reader declines any
  framework it parses as unsupported.
- **Malformed PCL versions.** NuGet reads `portable.-net45+win8` as
  unsupported, and ours read it as `.NETPortable` 0.0. That is fixed and
  pinned in `framework_diff.rs`.

### Stage 2: the half-A oracle and the reference globals — landed

`borzoi::restore::restore_graph` obtains a project's spec in one of two ways:
generated (`GenerateRestoreGraphFile`, A₀), or the one `restore.rs`'s own
restore writes beside its assets file, which is by definition what that restore
read. NuGet writes it even when resolution then fails. Both, and the shipped
restore, go through one command builder in `restore.rs`, so they share the SDK,
working directory, environment and global properties by construction.

That makes the plan's structural test (comparing two argument lists)
tautological. In its place, `tests/all/restore_graph.rs` checks the stronger
claim the reference needs: the generated spec *equals* the one restore read, up
to the scratch directories each was written under. The checks run on:
- fixtures (a single project, a project reference, a locked project, a
  project in a symlinked directory, a symlinked project file, a project
  conditioned on restore's injected global);
- every corpus project, in CI's `corpus-diff` job (`restore_graph::pinned_projects`),
  where the stage-1 reader's verdict on each spec is pinned in
  `tests/manifests/restore_graph_projects.txt`. All six corpus projects agree
  and commit; none of them prunes (they target `net8.0` and `netstandard2.0`),
  so pruning's breadth comes from stage 1's local sweep, not this census.

The routes check turned up two findings:
- **Injected globals.** `dotnet restore` forwards one global property beyond its
  target, `NuGetInteractive=false`, that `dotnet msbuild` does not, so a project
  conditioned on it generated a different spec. The shared builder now passes it
  for every verb, which for the shipped restore only repeats what the CLI adds.
  A test diffs the MSBuild command lines the two verbs forward, so an SDK that
  starts forwarding another global fails by name.
- **Symlinked project paths: a project is the spelling it is evaluated
  under.** MSBuild runs a project's targets with the process's working
  directory set to the project's directory, which the OS reports by its real
  path. A project whose *directory* is reached through a symlink is therefore
  spelt two ways in one restore: the walk resolves its relative
  `ProjectReference`s by the real path, the evaluation by the symlinked one, and
  NuGet drops them from the spec. That is `dotnet`'s own behaviour, from any
  working directory: `dotnet build /abs/link/App.fsproj` compiles against the
  reference but writes an assets file without it. A project *file* that is a
  symlink, in a real directory, restores whole.

  Two spellings of one file are also two MSBuild projects. Probed with
  `dotnet msbuild -getProperty -getItem:Compile`: through a symlinked directory
  the `Directory.Build.props` search walks the link's ancestors, and a symlinked
  `.fsproj` compiles the sources beside the link, not beside its target. So the
  LSP identifies a project by its spelling (`semantic::ProjectId`: absolute,
  lexically normalised, case folded where the platform folds it), keys every
  per-project cache by it, and evaluates that spelling. Keying by real path
  merged distinct projects and served whichever spelling was evaluated first.
  Both the shipped restore and the oracle decline a project whose directory is
  not its real path; the decline is cached under that spelling alone, so the
  real spelling still restores.

  The cost: a never-restored project opened through a symlinked directory
  (including macOS `/tmp` and `/var` temporary directories) gets no on-demand
  restore. The alternative, evaluating every project under its real directory
  as `cd proj && dotnet build` does, would need file identity by real path
  across the LSP (membership, buffer overlays, the paths in responses) and
  would change which `Directory.Build.props` a symlink-opened project reads.

### Stage 3: the decomposition is lossless — landed

`tools/nuget-oracle`'s `restoreDgSpec` op runs NuGet's restore over a supplied
dgspec as `dotnet restore` runs it (`BuildTasksUtility.RestoreAsync`: the
spec's own request provider, settings from the config files the spec names,
the cross-platform machine-wide settings), stopping short of the commit
(`RestoreRunner.RunWithoutCommit`). It returns each restored project's assets
file as `LockFileFormat` renders it, plus the identity of the
`NuGet.Commands` that ran. The oracle runs the NuGet of the SDK the muxer
selects from the project's directory, which is the SDK `restore.rs` restores
with, and `tests/all/restore_graph.rs` checks that the identity it reports is
that SDK's file.

The gate compares the **whole assets file**, not only the package set and
compile assets the plan first named: B₀ run over A₀'s spec must produce
exactly the file `restore.rs`'s restore wrote, up to the scratch directories
each route was written under. It runs on every fixture that commits (a
`net10.0` project with its SDK pruning list, a project reference, a project
conditioned on restore's injected global) and on every corpus project in
`restore_graph::pinned_projects`, which pins each target's library, package
and compile-asset counts, whether each restore succeeded, and the graded
`NuGet.Commands` identity. A fixture that drops `FSharp.Core` from the spec
before handing it to NuGet must fail the comparison, so it cannot pass by
comparing nothing.

Every corpus project and every fixture decomposes losslessly. The run turned
up two things:
- **Log order is not content.** The one difference was the order of the
  assets file's `logs`. A missing package's message is built per element of
  the target graph's unresolved set (`UnresolvedMessages.LogAsync`), which
  iterates in hash order, and .NET randomises string hashes per process. The
  comparison sorts `logs`.
- **Stage 2's corpus restores were all failing.** The gate restored corpus
  projects against the devshell's NuGet store, which holds only what Borzoi's
  own builds need, so every one failed resolution with `NU1101`. Stage 2's
  comparison could not see it, because the spec is computed before resolution.
  The gate now restores against the packages folder the corpus's own restores
  filled (`BORZOI_PROJECT_PACKAGES`), and all six succeed.

What this does not cover:
- **One SDK.** Every corpus project resolves SDK 10.0.401, whose
  `NuGet.Commands` is `7.9.0-rc.42413+e34a38d2…`. The identity half holds for
  each SDK the corpus runs on, which is one.
- **No corpus project has a project reference or prunes** (they target
  `net5.0`, `net8.0` and `netstandard2.0`). Only the fixtures cover those.

### Stage 4: B₁ consumes the typed dgspec

**Dependencies:** stages 1, 3.
**Implements:** the B₁ row.
- Add `resolve_dgspec(nuget, spec, project, tfm)`. It is plumbing over
  `resolve_offline`. `nuget` is the restoring NuGet's identity (see *Per-SDK
  drift*), checked against the verified list. There is a typed decline for an
  unverified identity, and one for each field the resolver does not model
  (pruning, project references, transitive pinning, download dependencies,
  non-default warning policy).

**Correctness oracle:**
- Certain-implies-exact against B₀ running that same identity, on generated
  dgspecs and on every corpus dgspec.
- Absolute property: for any spec, an identity not on the verified list
  declines.
- The verified list is exactly the set of identities a B₀ run in CI has graded.
  A test derives it from the oracle runs, so nothing can be added by hand.
- A pinned census of B₁'s outcome per corpus project, giving decline counts by
  field. Expect pruning to decline nearly every `net10.0` project; the census
  is how stages 5–7 get ordered.

### Stages 5–7: close the declines, ordered by stage 4's census

**Dependencies:** stage 4. They are mutually independent and can run in
parallel. Stage 8 chose A₁, under which B₁ is the half-B consumer, so they are
not gated on it.

- **5 — pruning.** Oracle: the generator emits prune lists over the synthetic
  universe, including prune ranges that cover a *direct* reference.
  Certain-implies-exact against both engines. The census moves.
- **6 — project nodes.** Package flow through `projectReferences`, with
  `PrivateAssets`/`suppressParent`. Oracle: the generator emits multi-project
  dgspecs, graded the same way.
- **7a — CPM transitive pinning; 7b — download dependencies** (presence check
  only; a miss declines); **7c — the warning policy.** For 7c, decline under
  `allWarningsAsErrors`/`warnAsError` unless the resolver proves restore emits
  no warning, which needs a per-row warning verdict from B₀. The oracle for
  each is generator-extended, graded the same way.

### Stage 8: restore-cone census — landed; chose A₁

`tools/msbuild-condition-oracle`'s `restoreCone` op runs
`GenerateRestoreGraphFile` in-process through `BuildManager` with a logger
attached. The global properties are `restore.rs`'s own for
`Verb::GenerateRestoreGraph`, `NuGetInteractive=false` included. The census
takes them from `borzoi_oracle_harness::restore_globals`.
`borzoi`'s `restore_cone_globals.rs` reads back the command line that
`restore_graph` runs, through a stand-in `dotnet`, and holds that list to it
argument for argument.

For each target and task that starts, the op records:
- its name;
- the SHA-256 of the file defining each target;
- each task assembly's name, version and informational version.

The target files' hashes agree between the macOS and Linux builds of each
SDK. The task assemblies' bytes and module MVIDs do not: every one of the
eight checked differs, because each platform's build compiles the IL
separately and precompiles it natively. The informational version, which names
the commit the SDK was built from, agrees for all 46 SDK assemblies the
manifest names. So that is the identity the manifest pins. The op also records every SDK assembly loaded while the builds ran, which is
the code the tasks reach.

`borzoi-msbuild`'s `restore_cone_census.rs` drives the op. Two checks make it
a census of A₀ rather than of something nearby:

- **Same computation.** Under the SDK that `dotnet` resolves for the project,
  `dotnet msbuild -t:GenerateRestoreGraphFile` runs under the same globals with
  the oracle's logger attached. It must execute exactly the targets and tasks
  the in-process build does, and write the same spec up to the scratch
  directory. The cone is compared as well as the spec, because a target that
  runs on one side only can leave the spec unchanged. One such context
  difference is the startup directory: MSBuild exposes it to the build as
  `MSBuildStartupDirectory`, so each oracle runs from its project's directory,
  as `restore.rs` runs `dotnet`. The hooked fixture's target runs only from
  there. The spec comparison found the SDK directory's spelling to be part of the
  computation. The devshell's `DOTNET_ROOT` reaches each SDK through a link,
  and MSBuild records paths into its own directory (the runtime identifier
  graph's, the SDK's `library-packs` sources). So the oracle is pinned to the
  SDK's real path, which is the one the muxer runs it from.
- **Same SDK.** The oracle is pinned to one installed SDK (`--msbuild-sdk`), and
  the `NETCoreSdkVersion` its evaluation imported must be that SDK's version.

The census runs under every SDK the devshell installs, and labels each cone
`resolved` (the SDK `dotnet` picks for the project) or `forced`.
`restore_cone_projects.txt` pins each corpus project's cone; CI's
`corpus-diff` job runs it as the `restore-cone` step. `restore_cone_fixtures.txt`
does the same for generated projects that reach what the corpus does not:
- a `net10.0` project;
- a `net10.0;net8.0` project with central package management, transitive
  pinning, `TreatWarningsAsErrors` and a project reference to a
  `net8.0;netstandard2.0` library;
- a project with a user target hooked `BeforeTargets` a restore target.

The fixtures run in the ordinary `cargo test`. The census must see the hooked
target as a target from the project's own file; that is the positive control
for "no user targets" meaning something.

The census does not cover the following:
- **One SDK for the corpus.** No corpus project has a `global.json`, so every
  one resolves the devshell's 10.0.401. The devshell also installs 10.0.112,
  and the census runs every project under it, labelled `forced`. That
  measures how the cone moves across a feature band. It does not measure what
  any corpus project runs. No 10.0.3xx, 9.x or 8.x SDK was measured.
- **The environment.** The oracle child runs under the scrubbed environment
  every oracle here uses, not the LSP's full process environment, so the cone
  does not move with the host. A target conditioned on an environment variable
  the scrub removes could run under `restore.rs` and not here.
- **The corpus is narrow.** It has six F# projects targeting `net8.0` and
  `netstandard2.0`. None has a `NuGet.Config`, a workload, or a
  `Directory.Build.targets`.

#### The measured cone (SDK 10.0.401)

| | corpus (6 projects) | fixtures (3) |
|---|---|---|
| targets per project | 41–44 | 44–47 |
| distinct targets | 50 | 51 |
| files defining them | 9: `NuGet.targets` and 8 `Microsoft.NET.Sdk` targets files | the same 9, plus the hooked project |
| targets outside the SDK | 0 | 1, the planted hook |
| tasks per project | 20–24 | 24–27 |
| distinct tasks | 26 | 27 |
| task assemblies | 4, all in the SDK | 4, all in the SDK |
| SDK assemblies loaded during the builds | 19 | 19 |

The 27 tasks, by assembly:
- **MSBuild (4):** `MSBuild`, `Message`, `RemoveDuplicates`,
  `ConvertToAbsolutePath`.
- **`NuGet.Build.Tasks` (16):** `CheckForDuplicateNuGetItemsTask`,
  `GetCentralPackageVersionsTask` (central package management only),
  `GetGlobalPropertyValueTask`, `GetProjectTargetFrameworksTask`,
  `GetRestoreDotnetCliToolsTask`, `GetRestoreFrameworkReferencesTask`,
  `GetRestoreNuGetAuditSuppressionsTask`, `GetRestorePackageDownloadsTask`,
  `GetRestorePackageReferencesTask`, `GetRestoreProjectReferencesTask`,
  `GetRestoreProjectStyleTask`, `GetRestorePrunePackageReferencesTask`,
  `GetRestoreSettingsTask`, `NuGetMessageTask`, `WarnForInvalidProjectsTask`,
  `WriteRestoreGraphTask`.
- **`Microsoft.NET.Build.Tasks` (7):** `ApplyImplicitVersions`,
  `CheckForDuplicateFrameworkReferences`,
  `CheckForImplicitPackageReferenceOverrides`,
  `CheckIfPackageReferenceShouldBeFrameworkReference`, `GetPackagesToPrune`,
  `ProcessFrameworkReferences`, `ResolveAppHosts`.

The tasks are thin over libraries. The 19 SDK assemblies the builds load
include nine `NuGet.*` assemblies: `NuGet.Build.Tasks`, `NuGet.Commands`,
`NuGet.Common`, `NuGet.Configuration`, `NuGet.Frameworks`,
`NuGet.LibraryModel`, `NuGet.Packaging`, `NuGet.ProjectModel` and
`NuGet.Versioning`. They also include `Newtonsoft.Json`, the SDK resolvers
that evaluation runs (`Microsoft.Build.NuGetSdkResolver` and the workload
resolver and manifest reader), and two copies of
`Microsoft.Deployment.DotNet.Releases`.

#### Across SDKs (10.0.112 → 10.0.401)

- **Targets.** The target names and defining files are identical: 50 in the
  corpus and 51 in the fixtures, under both SDKs. Three of the nine files'
  hashes move: `NuGet.targets`, `Microsoft.NET.Publish.targets` and
  `Microsoft.NET.RuntimeIdentifierInference.targets`. But only 1 of the 50
  executed target bodies changed. `_GenerateRestoreProjectSpec` gains
  `NETCoreSdkVersion` and `RestoreDoNotWriteDependencyGraphSpec` metadata on
  the item it hands to the tasks.
- **Tasks.** The task names are identical (26 and 27). All four task
  assemblies move:
  - MSBuild 18.0.11 → 18.9.11;
  - NuGet 7.0.3 → 7.9.0;
  - `Microsoft.NET.Build.Tasks` 10.0.112 → 10.0.401.
- **The spec.** For every one of the nine subjects, the spec moves, in four to
  six schema places. The census pins them as `spec drift` lines:
  - `frameworks.*.framework` and `restore.frameworks.*.framework` are new,
    written by NuGet 7.9's task code. No executed target changed to produce
    them.
  - `restore.SdkAnalysisLevel` moves from 10.0.100 to 10.0.400.
  - `runtimeIdentifierGraphPath` and `restore.sources` (the SDK's
    `library-packs` folders) are paths into the SDK.
  - The implicit `FSharp.Core` version moves on the four subjects that take
    it.

#### Decision: A₁

The data meets the three conditions this stage set for A₁:

- **The cone is the same small, SDK-only set across the corpus.** The union is
  50 targets, all in SDK files, and every project runs a 41–44-target subset
  of it.
- **User targets hooking in are rare.** None of the six corpus projects has
  one. Under `restore.rs`'s configuration a package cannot add one:
  `MSBuildProjectExtensionsPath` is the empty scratch directory, so no
  package's `build/` targets are imported. A hook can only come from the
  project's own tree or from MSBuild's user-extension import directories. The
  fixture shows the census sees one when it is there.
- **The task leaves can be enumerated.** There are 27 by name, and 23
  excluding MSBuild's built-ins. The set is the same under both SDKs.

A₂'s conditions do not hold. The leaf *set* is neither large nor moving.
What moves across releases is the leaves' *code*, and that sets the terms A₁
is chosen on:

- **A₁ keys trust on the task assemblies too** (see *Per-SDK drift*). The new
  `framework` field is output moved by task code alone. A key on target files
  would have missed that if a release changed only the DLL.
- **A₁ declines on every new SDK until it is verified.** Each SDK release moved
  all four task assembly identities, so a model keyed on them declines on each
  new SDK. It stays declined until the per-task differentials (each Rust task
  against the real task, the plan's A₁ oracle) have run on that SDK in CI. As
  for B₁ in stage 4, the verified list is derived from those runs.
- **Modelling a leaf means modelling the library slice behind it.** That
  includes `NuGet.Commands`' item-to-`PackageSpec` construction
  (`WriteRestoreGraphTask`) and `NuGet.Configuration`'s settings resolution
  (`GetRestoreSettingsTask`). It also includes the SDK's
  `ProcessFrameworkReferences` and `GetPackagesToPrune`. Part of this overlaps
  with what `borzoi-nuget` already models.
- **A₁ also needs target execution.** That means ordering
  (`DependsOnTargets`, `BeforeTargets`, `AfterTargets`), intrinsic
  `PropertyGroup` and `ItemGroup` inside targets, and the `MSBuild` task's
  calls into inner per-framework builds and referenced projects.
  `borzoi-msbuild` evaluates projects but does not execute targets.

What would revisit this in favour of A₂:
- **The task set moves.** A census over more SDK releases shows a release
  adding a task to the cone, or most servicing releases moving a per-task
  differential's verdict. That needs more SDKs in the devshell; this census
  runs every one installed.
- **The cone widens.** A wider corpus shows user targets or tasks outside the
  27. Projects with a `NuGet.Config`, a workload, a `Directory.Build.targets`
  hook, or a target framework this corpus lacks are the likely sources.
- **A₁ costs more than A₂.** The target-execution engine plus the library
  slices above turn out to cost more than the runtime.

### Stage 9: spawn-free runtime path

**Dependencies:** stage 4 (and whichever of 5–7 the census needs), plus A₁
landed (stage 8's choice).
**Implements:** *Runtime composition.*

**Correctness oracle:** whole-project gate. For every corpus project where the
spawn-free path commits, its compile assemblies equal `restore.rs`'s, pinned
per project in `crates/corpus-diff/manifests/`. The global-perturbation sweep
runs over the half-A producer, since A₁ is exactly where a global it does not
model could move a value.
