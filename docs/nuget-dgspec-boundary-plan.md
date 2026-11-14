# Restore at the dgspec boundary — plan

> **Status:** stages 1–6, 7a, 7b, 7c and 8 have
> landed, and so have the
> resolver's handling of dependency asset filters, B₁'s of `<contentFiles>`, and
> B₁'s reading of the environment restore runs under.
> Stage 8 chose **A₁** (see its section for the numbers and the conditions);
> *A₁: the restore cone, staged* splits it into five stages, of which A₁.1
> (the item seam) and A₁.2 (the seam on real projects) have landed, and of
> A₁.3 its read census (A₁.3a).
> B₁ runs in production in shadow: every on-demand restore hands it the spec
> it read and logs whether B₁ agreed (see *B₁ in shadow*).
> The resolver is NuGet's default dependency resolver reimplemented, and
> declines on no corpus project, and B₁ commits on all six, exactly as the
> restore does (`crates/lsp/tests/manifests/restore_graph_projects.txt`). Supersedes
> slice 8 of
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
only. At stage 4 it modelled neither pruning, project nodes, CPM transitive
pinning, download dependencies nor the warning policy. The `restore` op in
`tools/nuget-oracle` builds a synthetic dgspec that omits framework
references, fallback imports and pruning, on the grounds that "none of them
names a package in the synthetic universe". Real dgspecs break that premise
(see `System.Text.Json` above). Stage 4 put the resolver behind a real spec,
where it declines every field it does not model, and graded it against B₀ on
real-shaped specs; doing so found a closure it committed that restore fails
(see stage 4). The LSP runs the resolver only in shadow (see *B₁ in
shadow*), so no wrong answer it computes is served.

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
`restore.rs` would fail. So nothing the LSP serves changes until a spawn-free
half A exists (stage 9). Until then B₁ runs beside the restore, in shadow,
so that it meets real projects before it is trusted to serve them.

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

### Stage 4: B₁ consumes the typed dgspec — landed

`borzoi_nuget::dgspec::resolve_dgspec(nuget, spec, project)` resolves every
target framework of a project in a parsed spec, or declines. It differs from
the signature first planned in one way: it takes no framework. Restore
succeeds or fails per project, and the LSP reads no assets file from a failed
restore, so committing one framework while another fails would commit where B₀
fails. It resolves them all and commits only when every one commits.

`nuget` is the restoring NuGet's identity (`NuGetIdentity`, the informational
version of `NuGet.Commands.dll`). Only an identity on the verified list,
`crates/nuget/src/dgspec/verified_nuget.txt`, commits. The list is an exact
manifest of the identity the differential's oracle runs:
`dgspec_resolve_diff::the_verified_list_is_the_graded_nuget` fails if the
two differ, so an identity reaches the list only by being graded, and a
devshell SDK bump fails until the list is regenerated.

Every field of the spec model has a verdict. The function that classifies them
destructures each struct in full, so a field added to the model does not
compile until it has one. The verdicts are:
- **Modelled:** direct dependencies, the packages folder, and the
  asset-target fallback list.
- **Inert, with the reason at the point of discard:** paths and outputs
  (beyond being present and free of control characters); `noWarn`; include
  and suppress-parent flags; `autoReferenced`; `SdkAnalysisLevel`; framework
  references.
- **Declined when present, all reported at once so the census can count
  them:** pruning, project references, central package management (all of
  it, not only transitive pinning), download dependencies, a warning policy
  other than `warnAsError` ⊆ {`NU1605`}, fallback folders, a source that is
  not a local absolute path, audit not explicitly off, a lock file that is or
  might be in use, a dependency target other than `Package`, the legacy
  `PackageTargetFallback`, a config file outside its closed vocabulary, any
  project or restore entry besides this one, and required metadata that is
  absent or outside the vocabulary the SDK writes.

The input domain B₁ claims is therefore narrow by design: an entry-only spec,
in the shape `restore.rs`'s `GenerateRestoreGraphFile` writes, over a packages
folder whose every package that restore may read is in a stated domain
(`crates/nuget/src/dgspec/nuspec.rs` for the nuspec, `commit_marker_is_admitted`
for the marker), with config files that only list sources. Each input is held
to a positive domain rather than to a list of known refusals, and each has a
sweep that grades perturbations of a committing input against B₀.

Review kept finding malformed inputs no SDK writes for ten rounds. What that
non-convergence turned out to mean: a positive domain is a closure argument
only for the *layer* it is stated at, and a nuspec has several — bytes, the
XML declaration, the document, the element vocabulary, the identity it states.
The vocabulary had a domain and a single-slot text sweep; the declaration had
neither, and twenty hand shapes at that layer found three more inputs B₁
committed and restore threw on (`version="1.1"`, `encoding="utf-16"` over UTF-8
bytes, an encoding `System.Xml` does not know) in ninety seconds. So the nuspec
domain is now stated per layer, in one total walk that declines every node it
does not name, and `every_nuspec_layer_is_held_to_its_domain` sweeps each
layer in its own terms: every declaration and encoding form, every published
namespace and the prefixed spellings, and eleven structural mutations
(duplicate, foreign or namespaced attribute, foreign child, text, comment,
processing instruction, CDATA, prefix, foreign namespace, removal) at every
element of a committing nuspec. Each variant is classed by the stated domain
before any code runs — in-domain variants must commit and B₀ grades the
commit; out-of-domain ones must decline whatever B₀ does, and the ones B₀
accepts are printed as the over-decline census; graph-changing ones are graded
only — so a walker that forgets a node class fails the sweep, not a review.

After resolving, B₁ also applies restore's compatibility check in the
direction that can only decline (see the findings below).

`tests/all/dgspec_resolve_diff.rs` holds it to certain-implies-exact against
B₀ (`restoreDgSpec`) over specs it generates. B₀ restores straight from the
packages folder the resolver reads, with only empty or missing folders as
sources, as `restore.rs`'s offline restore does, and B₁ is handed the identity
B₀ reports. The generator varies each inert field, and the sweep fails unless
each variation, and each assets-file version, was graded on a committed spec.
Hand cases pin the rest:
- each unmodelled field declines by name when it is the only change to a
  committing spec;
- an unverified identity declines on every generated spec;
- a promoted `NU1608` fails a restore whose closure the resolver computes,
  which is what makes the warning-policy decline load-bearing.

`restore_graph.rs` grades B₁ against the real restore as well: on a
`netstandard2.0` fixture it commits, implicit references included, and on
every corpus project in `pinned_projects` it must decline or agree.

The differential, the probes behind it, and review turned up seven things:
- **Asset-target fallback picks dependency groups.** A package with no
  dependency group for `net8.0` contributes its `net461` group's dependencies
  to a project with the SDK's fallback list. The resolver read no group, so
  it now takes the list (`resolve_offline_with_fallback`). A group for the
  project's own framework shadows the list even when it is empty.
- **`resolve_offline` committed where restore fails.** A dependency group that
  names one package twice, with different lower bounds, is a downgrade
  (`NU1605`) to both restore engines, and the resolver committed the higher
  one. `resolver_diff`'s generators never repeated an id within a group. The
  resolver declined on a repeated id in any group it read until it became
  the default engine itself, which reports the downgrade; `resolver_diff`
  pins the case.
- **The assets file's target names depend on its version.** Version 4 names a
  target by its alias, and version 3 by `NuGetFramework.ToString()`
  (`.NETStandard,Version=v2.0`). Restore writes version 3 when the spec has no
  `SdkAnalysisLevel`. The closures are the same, so both differentials compare
  by framework.
- **Restore checks compatibility after resolving.** A package with `lib/` or
  `ref/` assets of which none suits the framework fails restore (`NU1202`);
  through the asset-target fallback list restore may instead take them with a
  warning (`NU1701`). The resolver reads only nuspecs, so B₁ now reads each
  resolved package's files and commits only for a package with no assets or
  with compile assets for the framework. The generator's packages carry asset
  files so that this is graded; it was found by probing, not by the first
  generator, whose packages had none.
- **Restore validates package metadata even from a warm cache, on every
  version it reads.** It refuses a `DotnetTool` package in a
  `PackageReference` project (`NU1212`), and throws on a package type with a
  malformed version or a `minClientVersion` above its own version, losing
  versions included (the oracle's `restoreDgSpec` reports a thrown restore as
  a failure, as `dotnet restore` fails). It throws, too, on content-file rules
  it rejects and on a commit marker without a content hash. Three review
  rounds each found another such refusal, so B₁ holds every package to a
  stated domain rather than to a list of known refusals. The nuspec's is in
  `dgspec/nuspec.rs`, layer by layer: UTF-8 bytes; a declaration that is absent
  or `version="1.0"` with an encoding absent or `utf-8`; a document
  `roxmltree` parses without a DTD; a `<package>` in a published namespace or
  none holding one `<metadata>`, both in the default namespace (NuGet's reader
  looks for `<metadata>`'s children there, so a nuspec prefixed throughout
  with no default namespace has no identity to it); `<metadata>` children
  that are each descriptive (never read by restore, so their subtrees may
  hold anything) or modelled with exactly the attributes and children `pack`
  writes (an `id` and `version` each stated once and naming the package's
  folder, `dependencies`, `references`, `packageTypes` naming only
  `Dependency` without a version, `contentFiles`, `frameworkAssemblies`), or
  declined by name; and a `minClientVersion` below the restoring NuGet's
  version. The commit marker
  must be one restore reads, with no key repeated (a Nix-built cache's
  `{ "contentHash": "" }` is one). The vocabulary was measured over the 3,910
  nuspecs of a package cache before it was written: every one fits it except
  for the elements it declines by name. It checks every version restore may
  read: the version each enqueued edge resolves to among the installed
  ones, which the closure reports ([`examined`](../crates/nuget/src/resolver.rs)).
  Checking every installed version instead declined every F# project on a
  cache holding an old FSharp.Core with `contentFiles`. The generator writes descriptive elements (malformed
  ones included), declined ones and broken markers, and a hand case grades
  each descriptive element alone. The vocabulary declines
  `frameworkReferences` by name; modelling it is coverage work for whichever
  stage the census next points at (`contentFiles`, `developmentDependency`
  and `frameworkAssemblies`, each once a decline here, have their own
  sections below).
- **Restore loads the config files the spec names.** One it cannot load
  (malformed XML, an empty package source mapping pattern) fails the restore
  with every package in the packages folder. A valid package source mapping
  does not filter the packages folder, but B₁ holds config files to a closed
  vocabulary too: well-formed XML with only source lists
  (`packageSources`, `disabledPackageSources`), which is what a default user
  `NuGet.Config` holds. Everything those lists could change is overridden in
  the spec.
- **Restore requires some metadata the resolver does not read.** An absent or
  empty project name, project path or output path, or
  `originalTargetFrameworks` not naming exactly the frameworks, each alias
  once, makes restore throw. After two review rounds found these and the metadata above one at a
  time, `every_single_deletion_from_a_committing_spec_is_graded` replaced the
  hunt: it deletes or empties each key, element and string of a committing
  spec in turn and grades every variant the reader accepts against B₀ (102
  variants, 52 committed). It found the empty project name on its first run.
  Deletion cannot reach a cross-field rule (two frameworks sharing an alias),
  so `every_substitution_of_a_committing_specs_strings_is_graded` also
  replaces each string of a committing spec by each other string the spec
  holds, and by adversarial transforms of them (case flipped, a path separator
  appended, a NUL embedded): 1,408 variants, 213 committed. A single
  substitution cannot reach a rule that needs several fields to change
  together (an alias renamed consistently in its three places), so those
  cases, which review found, are graded by hand: aliases differing only by
  case (the reader refuses them), an alias with a separator (`NU1019`), a NUL
  in a path, and a nuspec version equal to the folder's in order but not in
  NuGet's identity (`1.0.0--0` against `1.0.0-0`). B₁ admits only aliases in
  the characters the SDK writes, unique ignoring case, and paths without
  control characters. The nuspec and config vocabularies get
  the same treatment at the level of their text:
  `every_perturbation_of_nuspec_and_config_text_is_graded` empties, blanks,
  pads, and puts a comment beside or in place of each attribute and text value
  of a committing nuspec and config file (107 variants, 41 committed). On its
  first run it found that the nuspec reader trimmed a dependency id NuGet
  keeps padded, and so finds no package for; the reader now refuses one.
  Review found the rest of this class (an XML comment read as an element's
  text, a whitespace-only config key, a symlinked version folder the version
  listing skipped, a symlinked asset folder the file listing did not descend
  into, two aliases naming one framework). Each is now declined, except the
  symlinked folders, which the listings now follow, as restore does (a folder
  that links back into itself declines).
  Review also found that a relative `nuGetLockFilePath` is resolved against
  the project; B₁ declines one.
- **The packages folder needs no sources.** A source folder that does not
  exist is never read, and package source mapping that maps no package to any
  source changes nothing, when every version restore needs is in the packages
  folder.

The census, pinned in `restore_graph_projects.txt`, is the input to stages 5–7:

| unmodelled field | corpus projects it blocks (of 6) |
|---|---|
| warning policy (`TreatWarningsAsErrors`) | 6 |
| download dependencies (`net5.0`, `net8.0` targeting packs) | 2 |
| pruning | 0 (no corpus project targets `net10.0`) |

The four `netstandard2.0` projects are blocked by the warning policy alone.

### Stages 5–7: close the declines, ordered by stage 4's census

**Dependencies:** stage 4. They are mutually independent and can run in
parallel. Stage 8 chose A₁, under which B₁ is the half-B consumer, so they are
not gated on it. By stage 4's census the order is **7c, 7b, then 5, 6, 7a**;
the corpus has nothing that 5, 6 or 7a would unblock, so the order among those
comes from stage 1's local sweep and the stage-8 fixtures, not from the census.

- **7c — the warning policy.** Landed; see below.
- **7b — download dependencies.** Landed; see below.
- **5 — pruning.** Landed; see below.
- **6 — project nodes.** Landed; see below.
- **7a — central package management.** Landed, transitive pinning included;
  see below.

The oracle for each is generator-extended, graded the same way.

### Stage 7c: the warning policy — landed

Each framework B₁ resolves now carries the warning codes restore logs for
its graph, after the project's `noWarn`, and the policy is applied to them
after resolving rather than declined on up front. A predicted code the policy
promotes (`warnAsError`, or `allWarningsAsErrors` unless `warnNotAsError`
demotes it) declines as `PromotedWarning`, since restore fails; a policy that
promotes nothing B₁ predicts commits. Within the envelope B₁ commits in, the
warnings are `NU1608`, a package in the closure depending on a range whose
upper bound the version the graph settled on exceeds, because a nearer edge
eclipsed the dependency with a higher version (a settled version below a
range's lower bound logs nothing; review found B₁ predicting it), and the
lower-bound family restore judges on the range chosen for each package of the
final graph (`UnexpectedDependencyMessages`, which the resolver reports as
`resolved_dependencies` since it became the default engine): `NU1602` for a
range without an inclusive lower bound, `NU1603` for a lower bound that is not
installed, so the package resolved to the next installed version, and
`NU1604` for a direct reference without an inclusive lower bound, whose
package the other two then pass over. A dependency's own `noWarn` suppresses
warnings attributed to it, which B₁ does not model, so a framework with one
predicts nothing exact, and declines under any promoting policy.

The differential holds every exact prediction to the codes B₀ logs, on every
commit and under every policy, which is a stronger claim than that restore
succeeds; the corpus test does the same against the real restore. The
generator varies the policy (`allWarningsAsErrors`, `warnAsError`,
`warnNotAsError`, `noWarn`, a dependency's `noWarn`) and bounded dependency
ranges, and the sweep requires a commit that predicts a warning and one under
each promoting policy.

Lifting the policy decline moved the census onto what it had hidden:

- **`developmentDependency` is inert.** Restore ignores the element whatever
  its value, so it joins the descriptive vocabulary. Nerdbank.GitVersioning,
  in three corpus projects, declares it.
- **Assets outside framework folders are compatible and silent.**
  Nerdbank.GitVersioning has no compile assets, only `build/` files, some in
  folders like `build/MSBuildCore/` that are not framework names. A package
  with no compile assets for the framework is now admitted when it has no file
  in `lib/` or `ref/` and none in a `build/` or `buildTransitive/` folder whose
  name the content model reads as a framework the project or one of its
  fallbacks takes: restore fails on the first (`NU1202`, even for a `lib/`
  file that is not an assembly) and takes the second through the fallback
  list with `NU1701`. The content model reads a folder name in the short
  form, and falls back to `ParseFrameworkName`, the long `FrameworkName` form,
  with no comma check: `build/.NETFramework,Version=v4.8/` is a `net48`
  folder and warns `NU1701` like one, and ` .NETFramework` and the synonym
  `NETFramework` are `.NETFramework` at 0.0, which the fallback list takes
  too; `MSBuildCore` is a framework named `MSBuildCore`, compatible with
  nothing. So the check parses as it does (`assets::content_model_framework`)
  and asks compatibility, where the short form alone read all three as
  unsupported and would have admitted the package. Restore throws on any
  folder in a framework position of its content model that it cannot parse
  (`build/portable-net45-client/`), whatever else the package holds, so such
  a package declines. `asset_folder_shapes_are_graded` holds each top-level
  folder, with and without a framework subfolder, with long-form and
  unparseable ones, to B₀, and pins B₀'s own verdict per row;
  `framework_folder_names_are_graded` sweeps names built from an alphabet of
  the pieces both parsers read, under `build/` and `lib/`.

| corpus project (6) | B₁ |
|---|---|
| TypeEquality, WoofWare.LiangHyphenation, WoofWare.WeakHashTable | commits, closure and warnings equal to the restore's |
| WoofWare.Incremental (`net5.0`), WoofWare.PawPrint.Domain (`net8.0`) | declines: download dependencies (7b) |
| WoofWare.Expect | declines: a nuspec dependency with an asset filter, which the resolver does not model |

So the next order was **7b**, then the resolver's dependency asset filters,
then 5, 6 and 7a.

### Stage 7b: download dependencies — landed

A framework's `downloadDependencies` no longer decline. Restore only checks
that a download is present: it fails on a missing one (`NU1101`) and, unlike
a dependency, validates nothing else about it. A download's package type
(the targeting packs are `DotnetPlatform`), minimum client version, commit
marker contents and metadata are all ignored, which a hand case grades
against B₀. So B₁ commits when each download names one exact version that is
committed in the packages folder with its nuspec, and declines as
`DownloadUnavailable` otherwise. The SDK writes each download as an exact
range; restore's handling of any other range is not modelled, so one
declines (restore accepted an open range satisfied only by a higher version).
The generator writes present, missing and open-ranged downloads.

The census: the two projects downloads blocked (`net5.0` and `net8.0`
targeting packs) now pass the check, and decline on what stood behind it.

| corpus project (6) | B₁ |
|---|---|
| TypeEquality, WoofWare.LiangHyphenation, WoofWare.WeakHashTable | commits |
| WoofWare.Expect, WoofWare.Incremental, WoofWare.PawPrint.Domain | declines: a nuspec dependency with an asset filter (`include`/`exclude`), which the resolver does not model |

So the next stage is the resolver's dependency asset filters, which the
corpus now names as the one blocker, then 5, 6 and 7a.

### Dependency asset filters — landed

`resolve_offline` declined whenever a version it settled on, or a losing
version, declared a dependency with an `include` or `exclude` asset filter.
`dotnet pack` writes `exclude="Build,Analyzers"` on every dependency, so the
decline blocked half the corpus. A filter decides which of a package's assets
flow to the project, not which packages are in the graph, so the resolver now
ignores it. `resolver_diff` holds that to both restore engines: its random
generator attaches filters (`exclude="Build,Analyzers"`, `include="None"`,
`exclude="All"` and others, chosen without drawing from its random stream, so
its graphs are unchanged), its wide-graph generator filters every seventh
dependency, which reaches losing versions with filtered dependencies, and the
soak passes. The filters still matter for compile assets, which stage 9's
consumer will have to apply.

| corpus project (6) | B₁ |
|---|---|
| TypeEquality, WoofWare.Incremental, WoofWare.LiangHyphenation, WoofWare.WeakHashTable | commits |
| WoofWare.Expect | declines: a losing version that is not a leaf of the legacy engine's tree (the resolver's engine-agreement envelope) |
| WoofWare.PawPrint.Domain | declines: System.Diagnostics.DiagnosticSource 9.0.2's nuspec declares `<contentFiles>` (ILLink descriptors, as Microsoft's `System.*` packages do), outside B₁'s vocabulary |

The two declines are the next targets: modelling `<contentFiles>` (restore
validates its `buildAction`, `copyToOutput` and `flatten`), and the losing
versions the engine-agreement envelope turns away.

### `<contentFiles>` — landed

Restore reads a package's `<contentFiles>` rules and throws on one it
rejects: a rule without an `include`, a `buildAction` it does not know, or a
`copyToOutput` or `flatten` that is not a boolean. The nuspec reader now keeps
each rule's attributes, and B₁ admits one `<contentFiles>` holding only
`<files>` rules in a positive subset: a non-empty `include`, an optional
`exclude`, a `buildAction` among `None`, `Compile`, `Content` and
`EmbeddedResource` (compared ignoring case, as restore does), and boolean
`copyToOutput` and `flatten` (ignoring case). Rules restore accepts outside
that subset (`Page`, an empty `include`, an unknown attribute) decline. A hand
case grades each rule shape against B₀, and the text perturbation sweep's
nuspec carries a rule whose four values it perturbs.

| corpus project (6) | B₁ |
|---|---|
| TypeEquality, WoofWare.Incremental, WoofWare.LiangHyphenation, WoofWare.PawPrint.Domain, WoofWare.WeakHashTable | commits |
| WoofWare.Expect | declines: a losing version that is not a leaf of the legacy engine's tree |

The one remaining corpus decline was then the resolver's own engine-agreement
envelope, where the legacy and default engines could treat a loser's subtree
differently; the next two sections are its history.

### The resolver's closure is the default engine's — landed

The resolver committed only where both of NuGet's dependency resolvers write
the same closure, and so declined whenever a version that loses its conflict
has any dependency: the legacy engine walks a loser's subtree before rejecting
it and can fail on what it finds. Real graphs have such losers everywhere
(System.Memory 4.5.5 under System.Text.Json), and WoofWare.Expect declined on
one. A dependency-graph spec restores under the default engine alone (the key
that selects the legacy one declines in the reader), so the contract became
the default engine's, first through an envelope around the legacy walker's
tree that held every loser's whole subtree inert (readable throughout, naming
no package the closure holds), then — since no envelope reached real graphs,
whose losers name closure packages everywhere, and ten review rounds showed
that positions read off the legacy tree never bound the default engine — by
reimplementing that engine, below.

### The resolver is the default engine — landed

`resolver.rs` is now `DependencyGraphResolver` (the .NET 10 SDK's
`NuGet.Commands`, 7.9) reimplemented pass for pass, in place of the legacy
walk and the envelope around it:

- **Choosing** (`ResolveDependencyGraphItemsAsync`). Items, one per edge
  with the path of edges that reached it (each edge keyed by name and range,
  NuGet's `LibraryRangeIndex`), are processed breadth-first from the
  project's references. The first item of a name is chosen; a later one is
  skipped when the chosen range is at least as high by lower bound, evicts
  the chosen item when it demands more, and at precisely the same range
  replaces the chosen item with its own path unless the paths share a
  prefix. A direct package reference is never evicted. Evictions are keyed
  by range, start the pass over when a chosen item descends from the
  evictee, and take the evictee's own evictions with them; a lower item
  records its parent as eclipsed, which protects the chosen item from later
  demands below that parent. `PrivateAssets="All"` suppressions aggregate
  down the subtree, an edge to a direct reference is never enqueued, and an
  edge to a name already chosen at that range is not enqueued again.
- **Flattening** (`CreateRestoreTargetGraphAsync`). The chosen items are
  walked again, by name, over every dependency they declare, suppressed ones
  included: a dependency named like its parent or the project, or whose
  range lies on the parent's path, is a cycle (`NU1108`); a second visit
  demanding more than the chosen range is a downgrade (`NU1605`) unless the
  chosen version satisfies it or a suppression or an eclipsed parent covers
  it; a first visit the chosen version does not satisfy is a version
  conflict (`NU1107`), reported after cycles and before downgrades, as
  restore reports them. The closure is the chosen packages the flattening
  reaches, each with the range chosen for it: the `resolved_dependencies`
  B₁'s lower-bound warnings are judged on.
- **What restore reads.** Every edge resolves as restore's lookup does with
  the packages folder as its only source
  (`SourceRepositoryDependencyProvider.FindLibraryCoreAsync`): the range's
  inclusive lower bound itself when a folder spells it, else the lowest
  installed version satisfying it, a prerelease only where a bound is one.
  So an edge whose lower bound is not installed resolves to the next
  installed version (restore warns `NU1603`, judged by NuGet's version
  identity, which compares release labels as strings where its ordering
  compares them as numbers), and one no installed version satisfies is
  restore's unresolved item, with no dependencies, which fails the restore
  only if it stays in the graph; a chosen item is read in full and declines
  on anything unreadable. Review found two folder shapes the first cut read
  wrongly, both now declines: two installed versions that compare equal but
  are spelled apart (`1.0.0-0`, `1.0.0--0`) with no folder spelling the
  lower bound, where restore keeps whichever it enumerated first; and a
  version folder with the `.nupkg.sha512` hash file but no `.nupkg.metadata`
  marker, which restore counts as installed and repairs, and nothing here
  can read. A third it read as absent: restore finds a version's folder by
  its canonical path and parses every folder's name as a version, so a
  folder spelled in another case counts on a case-insensitive file system,
  and the reader now lists and looks up folders the same way. Every version
  an edge resolves to is `examined`.
  Whether a source could answer differently stays B₁'s question:
  `check_sources` declines a folder source holding a package restore looks
  up past the packages folder, and now one that does not exist or cannot be
  listed, since restore consults every source for such a package and fails
  (`NU1301`) on one it cannot read; a missing `library-packs` folder still
  passes when every lower bound is installed. Floats decline; every other
  range shape, exclusive and open lower bounds included, is the engine's.
- **What the differentials hold.** `resolver_diff` holds every closure to
  the default engine's, and every decline that names a restore failure (a
  cycle, a conflict, a downgrade) to a failure the default engine reports;
  the legacy engine's outcome is recorded in the census and pinned on the
  hand cases where the engines part. Where the default engine writes a
  closure over committed, non-floating edges, the multi-version and
  cross-dispute sweeps require one. Both verdicts the envelope got wrong are
  hand cases: a same-range occurrence from a cousin path that moves the
  chosen item's path and closes a cycle the legacy walker eclipses away
  (seed 1794148483370034000), and a deeper demand above a direct exact
  reference, which the legacy walker eclipses through an ancestor's
  open-lower-bound sibling edge and the default engine fails as `NU1605`
  (seed 1794151809963611000). The former hand cases that documented the
  envelope's price (an uninstalled version below a loser, a loser's subtree
  reaching a winner, a missing package below a loser, a back edge at an
  uninstalled bound) now pin the engine's verdict on each, and
  `a_resolved_closure_reads_only_the_dependency_lists_of_the_versions_it_rejected`
  states the read set exactly: a loser is read when the engine chooses it in
  some pass, and not otherwise. Review found three slips, one spelling at a
  time, where NuGet's version *identity* (release labels as strings) and its
  *ordering* (numeric labels, so `-0` is `0`) part: the lookup's exact
  lower bound, the engine's range keys and B₁'s `NU1603`. Twin versions
  (`1.0.0-0` and `1.0.0--0`, one folder each) are now a generator of their
  own (`generate_twins`, in the soak's rotation), whose parents reach the
  twins by every spelling that tells them apart or fails to, with the
  obligation that we resolve wherever the default engine does except on a
  genuine tie; keying ranges by ordering again, or dropping the lookup's
  exact lower bound, fails it within a few graphs. The wide generator's pool
  grows twins too and the census counts them, which alone was too weak to
  catch either.

| corpus project (6) | B₁ |
|---|---|
| TypeEquality, WoofWare.Expect, WoofWare.Incremental, WoofWare.LiangHyphenation, WoofWare.PawPrint.Domain, WoofWare.WeakHashTable | commits |

The resolver declines on no corpus project. WoofWare.Expect was the last
decline, at the nuspec vocabulary rather than the resolver: System.Buffers
4.5.1, a version the engine examines (System.Memory 4.5.5's edge resolves to
it) without choosing, declares `<frameworkAssemblies>`, which the next
section admits.

### `<frameworkAssemblies>` — landed

839 nuspecs of a developer's cache declare `<frameworkAssemblies>`
(Microsoft's `System.*` packages among them), and B₁'s nuspec vocabulary
declined the element by name. Two questions had to be settled against
restore before admitting it, both as positive domains:

- **What restore reads of the element for a version it only examines.** The
  edge lookup (`FindLibraryCachedAsync` → `GetDependencyInfo`) reads the
  framework assembly groups beside the dependency groups and the minimum
  client version, so the element is read for every version the engine
  examines, chosen or not. What that read does
  (`NuspecReader.GetFrameworkAssemblyGroups`): an absent or empty
  `targetFramework` is `Any`, any other is split on commas and each piece
  trimmed (`String.Trim`, every Unicode white space) and parsed as a folder
  name, which yields `Unsupported` for what it cannot read except in its
  portable branch, where a member carrying a profile throws
  (`portable-net45-client`, under any spelling of the identifier); an empty
  `assemblyName` is dropped and duplicates merge. Three review rounds each
  produced a spelling the crate's framework parser admits and NuGet throws
  on (the hyphenated member itself; the same member under the dotted
  `.NETPortable` identifier and behind non-breaking spaces; then a
  zero-padded version, `portable4.00000000000-net45-client`, the parser
  reads as unsupported), so the domain does not lean on the parser at all
  for the one branch that throws: each piece must hold no `%` (NuGet
  unescapes it first), no white space inside, have a leading identifier —
  the run of ASCII letters and dots NuGet's `RawParse` cuts off the front —
  that is not `portable`, `NETPortable` or `.NETPortable` in any case (the
  three spellings NuGet's identifier table maps to the portable framework),
  and parse with `NuGetFramework::parse`. The portable family is one the
  measured cache never spells in this element (its values are
  `.NETFramework4.x`, `MonoAndroid`, `Xamarin.*` and comma lists of them).
  The `assemblyName` is held to nothing. The
  shape is held to `pack`'s: one element without attributes, holding
  `<frameworkAssembly>` elements that each carry an `assemblyName`, at most
  a `targetFramework` besides, and no child or text. The parser still
  reads the zero-padded version as unsupported where NuGet throws, which
  keeps the rule textual rather than a reading of the parser's verdict.
- **What restore does with it for a package in the closure.**
  `LockFileUtils.AddFrameworkReferences` consults the groups only for a
  framework that is not package based (`.NETFramework`; never `.NETCoreApp`
  or `.NETStandard`), and writes the names of the group nearest the
  framework into the target library's `frameworkAssemblies`. That output
  reaches nothing the LSP reads (`crates/lsp/src/project_assets/` reads
  compile assets, package folders and framework-reference names), and its
  one effect inside restore — the names count as a compatible asset in the
  `NU1202` check and in the asset-target-fallback decision — can only make
  compatible, without a fallback, a package B₁ already declines:
  `check_compatibility` admits a package only with compile assets for the
  framework itself or with no framework-specific file at all, and restore
  finds both compatible on the primary criteria whatever the element says.

So `nuspec.rs` admits the element in `pack`'s shape and nothing moves in
`resolve.rs`. It is graded three ways in `dgspec_resolve_diff.rs`:
`framework_assemblies_shapes_are_graded_at_the_lookup_and_in_the_closure`
generates elements over an alphabet of framework spellings (parseable,
unsupported, portable profiles sound and throwing, platform-bearing,
escaped, the long form, whitespace and separators) and element shapes
(attribute sets, children, text, duplicates), classifies each by the stated
domain before any code runs, and
grades it on one spec that reads the element every way restore does — on a
chosen package and on a version the engine only examines, under a
package-based framework and under `net472` — asserting that in-domain shapes
commit, out-of-domain ones decline, the package-based target never has
`frameworkAssemblies`, and every name the `net472` target has is one the
nuspec declares; `framework_assemblies_land_on_targets_that_are_not_package_based`
pins B₀'s output on one nuspec (nearest group, merged, sorted);
`framework_assemblies_naming_a_profile_nuget_refuses_decline` pins the
throw and the decline row by row; and the nuspec perturbation sweep, the layer sweep's committing nuspec and the
generator's package universe each carry the element, so the whitespace,
comment and structural decisions are graded with the rest of the
vocabulary. With it, WoofWare.Expect commits (`b1: committed frameworks=1`
in `crates/lsp/tests/manifests/restore_graph_projects.txt`).

### Stage 5: pruning — landed

A framework's `packagesToPrune` no longer declines; the resolver applies it
(`TargetContext::prune`, through `resolve_offline_in`). Probing restore over
a spec settled the rule, which is not the one the field's shape suggests:

- Restore drops a dependency a package declares on a pruned id when the
  dependency's range accepts the prune range's **upper bound**. The prune
  range's lower bound and the inclusivity of its upper bound play no part:
  `[2.0.0]` and `(, 1.0.0)` both prune an edge `[1.0.0, )`, and nothing
  prunes an edge `[1.0.0]` against `(, 2.0.0]`. (B₁ first tested the edge's
  lower bound against the upper bound; review found the exact edge.)
- The edge goes before the walk, so its subtree goes with it, and another
  path can still bring the package in at a version above the bound.
- A project's direct references are never pruned. Restore may warn
  `NU1510` about one the list covers, deciding across the project's
  frameworks once any is `net10.0` or later. B₁ predicts it exactly for a
  single-framework project (`net10.0` with a covered reference warns; no
  earlier framework does, whatever `SdkAnalysisLevel` says) and predicts
  nothing exact otherwise: several frameworks with one `net10.0` and a
  covered reference (restore warned there for a reference only `net9.0`
  had), or a `.NETCoreApp` after 10.

An entry without an upper bound, or a floating one, still declines. A spec
cannot select the legacy restore engine, so this is graded against the
default engine only: a hand case pins each part of the rule against B₀,
the generator writes prune lists, and the `net10.0` fixture in
`restore_graph.rs`, whose spec carries the SDK's few hundred entries, now
commits with exactly the restore's closure. No corpus project targets
`net10.0`, so the census does not move.

### Stage 7a: central package management — landed

Central package management no longer declines. Probing restore showed it
moves no direct reference's closure on its own: the SDK writes each centrally
managed dependency's range into the spec, and restore resolves that range,
whatever the dependency's central entry says (one that differs, or none at
all) and whatever its `versionOverride` says. What it adds is validation,
which fails the restore: a dependency not centrally managed (`NU1008`), an
implicit reference whether or not it is (`NU1009`; the generator found the
managed case), and a floating central version (`NU1011`). B₁ declines each,
and a floating override.

**Transitive pinning** does change the closure, and is modelled in the
resolver's engine (`resolver.rs`, *Transitive pinning*), ported from the 7.9
`DependencyGraphResolver` and settled shape by shape against the oracle.
Restore reads it only under central package management and only on the
restored project: the flag without central package management, and a
referenced project's flag, are inert. Each transitive package edge whose name
has a central version for the framework is raised to that version and walked
with the project's path, as though the project referenced it; a direct
reference keeps its own range over its central entry, and a package pruned
from a parent's dependencies is never pinned back in. What follows from that,
each a hand case in `dgspec_resolve_diff.rs`:

- The pinned version's dependencies are walked, and since its subtree is
  rooted at the project, eclipsing and eviction below it move: a deeper edge
  that shared its parent's path, and was downgraded, now evicts.
- A pinned package's dependencies are walked whatever its parents suppress
  (a referenced project's `PrivateAssets="All"` dependency comes back in under
  a pinned package), though a suppressed edge is itself never pinned.
- Restore records no downgrade of a direct reference at all, so a graph that
  fails `NU1605` without pinning restores with it.
- A pin below what an edge demands is `NU1109`, an error whatever the warning
  policy; B₁ declines it, naming the code.
- A pin above an edge's upper bound is a version conflict restore records and
  never reports: the package leaves the graph, without a warning, and restore
  succeeds. The legacy engine keeps the package and warns `NU1608`, one of
  the places the two engines part, so pinning shapes are graded against the
  default engine alone, as pruning is.
- The lower-bound warnings judge a pinned package on its central range
  (`NU1602`, `NU1603`).

`resolver_diff.rs` sweeps pinned multi-version graphs (the oracle's `restore`
op takes central versions), requiring soundness, completeness where restore
resolves, and enough graphs where pinning moved a closure, rescued a
downgrade, and caused one; the dgspec generator pins on both sweeps and
requires the same effects on committed specs. No corpus project uses central
package management, so the census does not move.

### The restore environment — landed

B₁ committed a wrong closure on a well-formed input that no differential
saw: with `NUGET_USE_LEGACY_ASSET_TARGET_FALLBACK_DEPENDENCY_RESOLUTION` set,
restore drops the dependencies of a group chosen through the asset-target
fallback list, and B₁ kept them. Every oracle child ran under the test
process's environment, and B₁ read none, so the two always agreed on it.
`restore.rs` hands `dotnet` the project's whole build environment, so the
LSP's restore sees whatever the user has set.

So `resolve_dgspec` now takes the environment `restore.rs` computes
(`restore_environment`, which `restore.rs` itself spawns with), and every
variable in it has a verdict (`dgspec/environment.rs`). Probing restore under
each variable NuGet names settled them:

- **Modelled: the home variables.** Restore loads `NuGet.Config` and every
  `.config` file of `config/` in `{home}/.nuget/NuGet`, whether or not the
  spec names them, and throws on one it cannot load. Home is
  `DOTNET_CLI_HOME`, else `HOME`; an empty `DOTNET_CLI_HOME` is not skipped
  (restore wrote its default config under the working folder). Valid user
  configs naming sources, fallback folders or a packages folder moved
  nothing, and a missing one is replaced by the default, which moved nothing
  either. B₁ holds these files to the vocabulary it holds the spec's config
  files to, and declines an empty, relative or absent home.
- **Inert, each graded with a value that would expose a read:** the packages
  and fallback folders (the SDK resolves them into the spec), the HTTP cache
  and the credential plugins (HTTP sources only), extraction and signature
  verification (a warm cache extracts nothing), the concurrency limit,
  `NUGET_SHOW_STACK`, `DOTNET_HOST_PATH` and the CLI's message language.
- **Declined:** every other `NUGET_` variable, read or not, since NuGet
  builds some names at run time. That includes `NUGET_SCRATCH`: pointed at a
  missing folder, it wedges restore rather than failing it.
- **Unread:** everything else. A test scans the verified NuGet's assemblies
  for every `NUGET_` and `DOTNET_` name they hold and fails if B₁ ignores one.

What the scan cannot see is a variable NuGet reaches only through the .NET
runtime: `HOME` (modelled) and the temporary folder, which B₁ assumes is
writable, as it assumes the machine can run a restore at all. On Windows NuGet
finds the user config through `APPDATA`, which is not modelled, so a Windows
host declines.

`restore_is_graded_under_every_environment_b1_admits` restores generated and
hand specs under each admitted variable and each home layout, against an
oracle restoring under the same environment, and requires each admitted
variable to be graded on a committed spec. The legacy variable is in it too,
so the sweep is seen to move restore. Removing any one part of the check
(variables, the user config, the extra configs, the home precedence) fails it.
Every other case in `dgspec_resolve_diff.rs`, and every restore in
`restore_graph.rs`, runs under one controlled environment shared by the real
restore, the oracle and B₁: each variable B₁ has a verdict on removed, and a
home of the suite's own. Review found the caller's environment deciding what a
case graded three times, one test at a time, before that replaced the
per-test fixes. The census does not move.

### Stage 6: project nodes — landed

A project reference is a node of the graph (`borzoi_nuget::resolve_graph`,
which `resolve_offline_in` is now the package-only case of). The entry-only
spec `restore.rs` generates holds every project its references reach, and B₁
walks them as NuGet does:

- **The node.** A referenced project's framework nearest the entry's
  (`PackageSpec.GetTargetFramework`) is walked. Its edges are that
  framework's dependencies, then its project references for it, each as
  `[version, )` of the referenced project and overriding a same-named package
  dependency (`PackageSpecReferenceDependencyProvider`). A reference settles
  on the referenced project's one version, so it never loses a conflict. The
  closure holds packages only.
- **Suppression.** A dependency or reference with `PrivateAssets="All"`, or
  one the project's own prune list covers, is left out when the project is
  reached as a reference (the entry's own are walked whatever they say). The
  legacy engine still counts it as a sibling in its eclipse check; the default
  engine leaves the name out down the whole subtree, and its flattening
  checks every dependency for a cycle before leaving any out. B₁ declined
  where the two part ways until the resolver became the default engine; now
  a node below the project depending on the suppressed name commits with the
  edge left out, and a suppressed edge that names the project, an ancestor
  or the root is `NU1108`.
- **Pruning.** The entry's prune list prunes a referenced project's package
  edges as it prunes a package's (`ShouldPrunePackage`, default engine only,
  as stage 5 graded); a referenced project's own list suppresses its edges;
  a project reference is never pruned, and one the entry's list covers warns
  `NU1511` on `net10.0`, which B₁ does not predict.
- **Names.** A project's name is its identity in the graph: each must be a
  package id, no two (the entry included) may share one, and no package
  edge, in the spec or in a nuspec, may carry one (NuGet prefers the project
  by rules the engines state differently, and an edge named like the entry
  is a cycle). A reference to a project the spec does not hold fails restore
  (`NU1105`); a project no framework of which is compatible with the entry's
  itself declines, whether restore fails (`NU1201`) or selects one through
  the fallback list. Every project of the spec is admitted on the entry's
  terms, and one no reference reaches declines (`OtherProjects`).
- **Warnings.** A referenced project's package edges are checked for
  `NU1608` like a package's; a project with a suppressed edge, a dependency
  with its own `noWarn`, or a reference the entry's prune list covers makes
  the framework's warnings unpredicted.

The oracle: two hand tables (`project_references_resolve_identically` pins
the closure of each shape — flow, suppression at each level, chains, nearest
framework, cousins, overriding, versions, pruning at each level, eclipsing,
asset flags, CPM; `project_reference_declines_are_graded` pins each decline
with restore's own verdict where it is settled), each graded against **both**
engines (`grade_both`: the spec rendered again with
`restoreUseLegacyDependencyResolver`, which the reader declines, for the
oracle alone; a shape with a prune list is graded against the default engine
only, as stage 5 grades pruning). The generator emits referenced projects in
one spec of three, over the adversarial package universe, and a second sweep
(`generated_project_graphs_resolve_identically_on_both_engines`) emits them
in every spec over a clean universe (every version installed, no malformed
metadata, no duplicate dependencies), against both engines, with a commit
floor and a per-variation obligation (chains, each `PrivateAssets` value,
asset flags, versions, CPM, cross-targeting on either side, pruning, a
dependency's private assets, overriding, a project named like a package).
The deletion and substitution sweeps perturb a referenced project's fields
too. Two rows of the tables were wrong as first written and the oracle
corrected them: a cycle both of whose inner edges the entry also references
is no cycle to either engine, and `NU1605` is a warning that only the SDK's
default `WarningsAsErrors` makes fatal.

No corpus project has a project reference, so the census does not move; the
LSP's two-project fixture (`a_project_reference_brings_its_project_into_the_spec`)
now requires B₁ to commit.

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

### B₁ in shadow — landed

B₁ ran only in tests. Now every on-demand restore also runs it: after
`restore_to_scratch` restores, it hands B₁ the spec that restore read, with
the restoring NuGet's identity and the restore's environment, and checks a
commit against what the restore did (`crates/lsp/src/restore/shadow.rs`). The
restore's answer is served whatever B₁ says. A commit must name, per
framework, exactly the packages the assets file's target lists (ids and
versions compared ignoring case, as NuGet's identity does), predict exactly
the codes it logs where it predicts any, and not be made where NuGet's
restore failed. That is read from the `success` NuGet records in
`project.nuget.cache`, not from the command's exit status, which a target the
project hooks after `Restore` can fail once NuGet has restored. Anything else
is logged as an error, since under certain-implies-exact it is a B₁ bug. A
decline is logged with its reason. The check costs a few milliseconds in a
release build.

The identity is read from what the restore did
(`restore::nuget_identity::restoring_nuget`). The restore writes a detailed
log, in which MSBuild names the assembly each task ran from; NuGet's
`RestoreTask` loads `NuGet.Commands` from beside its own assembly, whose
`AssemblyInformationalVersionAttribute`
(`Ecma335Assembly::informational_version`) is the identity. The log costs
nothing measurable. The first cut predicted the identity instead: the SDK
that SDK discovery selects, and then, as review found a way round each
check, the evaluated `RestoreTaskAssemblyFile` and `NuGetRestoreTargets`
under the restore's environment and then under its global properties. The
fourth round found a `UsingTask Override` re-registering the task, which
neither property shows. Each round was another route by which MSBuild
decides which code runs, and predicting that is A₁'s model of the restore
cone, not a check in front of it. So the shadow reads it off the run, which
follows every route. A spawn-free restore (stage 9) has no run to read, and
its identity is A₁'s to predict.

The oracles:

- `restore_graph.rs` pins a `global.json` to each installed SDK in turn and
  requires the identity to be the one the oracle reports for the
  `NuGet.Commands` it loads from the SDK `dotnet` selects; and redirects the
  restore task to the other installed SDK by `RestoreTaskAssemblyFile` (in
  the project, in the environment, conditioned on a global the restore
  passes) and by `UsingTask Override`, under each of which the identity must
  be the other SDK's, as the oracle reports it, and B₁ declines when that
  NuGet is not verified.
- `restore_graph.rs` grades B₁ through the shadow's own comparison, with the
  identity read from the restore's log, which must equal the oracle's, on
  every fixture and corpus project.
- `borzoi-assembly`'s `informational_version` sweep holds the attribute
  reader to the version resource's `ProductVersion` over every managed
  assembly of every installed SDK.
- The comparison's properties: the restore B₁ describes agrees, under either
  target spelling and any casing of its versions; every change to a closure,
  a predicted log, or the restore's success disagrees; an assets file outside
  the restore's shape is not compared.

What it does not cover: only projects the LSP restores on demand (the host
has opted in, and the project has no assets file) are shadowed. A project
the user restored themselves is read from its own assets file, whose spec
names the user's feeds, which B₁ declines. MSBuild's log line is in the
CLI's language, so on a host whose language is not English the identity
declines.

### A₁: the restore cone, staged

The cone splits at one seam. Every target in it works towards a flat list of
`_RestoreGraphEntry` items, each a GUID with a `Type` and a bag of metadata.
`GenerateRestoreGraphFile` drops duplicates (`RemoveDuplicates`) and hands the
list to `WriteRestoreGraphTask`, which calls `NuGet.Commands`'
`MSBuildRestoreUtility.GetDependencySpec` and saves the result. Everything
before the seam is MSBuild: evaluation, target ordering, intrinsic property and
item groups, the `MSBuild` task's inner builds, and the task leaves that turn
items into entries. Everything after it is NuGet library code with no MSBuild
in it.

So the stages work from the seam outwards. Each one has an oracle of its own,
and the items at the seam are the currency between them: a stage before the
seam is graded by the entries it produces, and the stage after it by the spec
it makes of them.

#### A₁.1: the item seam — landed

`borzoi_nuget::dgspec::write_restore_graph` takes three inputs:
- the entries as the task reads them: a type plus metadata, after
  `ITaskItem.GetMetadata` has unescaped it;
- the task's `RestoreRecursive` flag, true by default, which puts every
  `PackageReference` project in `restore`;
- the restoring NuGet's identity and the process environment.

It returns the *text* of the file `WriteRestoreGraphTask` would save, along
with the spec `parse_dgspec` reads from it, or it declines.

The text, not a spec, is the output because NuGet's writer reshapes what it
saves:
- it sorts most lists, by four different comparers;
- it drops a warning policy that has only `WarningsNotAsErrors`;
- it folds two downloads of one package into a single `;`-joined entry;
- it writes a central version's range as the item spelt it.

What NuGet's reader then makes of that file is already modelled and graded by
`parse_dgspec`. Emitting the text keeps the writer's effects in one model, the
text, rather than re-deriving them as values.

**Oracle:** `tools/nuget-oracle`'s `restoreGraphItems` op wraps the items as
`MSBuildTaskItem` wraps them and runs `GetDependencySpec`. It then runs the
task's own `BuildTasksUtility.AddAllProjectsForRestore`, loaded by reflection
from the SDK, saves, and returns the text.

**Differential** (`restore_items_diff`): every commit's text equals NuGet's
byte for byte, under each of `dgspec_diff`'s environments. The cases are:
- **Base entry lists.** These are modelled on `dotnet msbuild
  -getTargetResult:_GenerateRestoreGraph` output from SDK 10.0.401, and every
  one must commit. They cover:
  - a single-target project;
  - a cross-targeting `net8.0;netstandard2.0` project;
  - a central-package-management project with transitive pinning and
    `TreatWarningsAsErrors`, referencing a second project;
  - a `net10.0` project with pruning on.
- **Mutated lists.** 1,500 a run, made by eleven mutation kinds:
  - values drawn from per-name pools of spellings;
  - padded values;
  - dropped, re-cased and foreign metadata;
  - re-cased types;
  - dropped, duplicated, swapped and added items;
  - retargeted `TargetFrameworks`.

  Each kind must produce at least one agreement. A 30,000-case soak found no
  violation, and between 55% and 70% of the value and item mutations agree.
- **Every single-value mutation.** Each metadata name of the first item of
  each kind in each base is set to every value in its pool, both in place and
  in a changed copy added after the original: 7,812 cases, of which 4,740
  commit. It runs under the default environment only. The random sweep reaches any one name and value only
  once in thousands of cases. Review found violations it had missed: an
  `SdkAnalysisLevel` with build metadata, an audit mode set on a second
  `ProjectSpec`, and Czech download order. This sweep reaches the first two
  kinds, and the culture oracles the third.

The oracles run under `dgspec_diff`'s environments and under six process
environments that move the culture: Czech, Swedish, English, `C`, no locale,
and invariant mode. A locale has to be set in the oracle's *process*
environment, because ICU reads it from the C library, which a variable the
oracle sets on itself never reaches.

Most of the work was declining where NuGet's result is not a function of the
items:
- **Host-dependent comparisons.** Project paths are compared and sorted
  case-insensitively on a case-insensitive file system and ordinally elsewhere.
- **OS-dependent output.** A `file:/` source is completed with a number of
  slashes that depends on the OS.
- **Culture-dependent order.** Downloads are ordered by the culture's
  collation, and ASCII names do not settle that: under Czech, `ch` sorts after
  `h`. An order is committed only when every name is letters, digits and
  dots, the ordinal and ignore-case orders agree, and the process runs in
  invariant mode or under an English, `C` or unset locale.
- **Windows.** The writer ends lines with CRLF there, so every Windows host
  declines, as B₁ does.
- **Unmodelled .NET behaviour.** This covers `Enum.TryParse`'s numeric forms
  and .NET's case folding beyond ASCII.

The function also declines what the typed model cannot hold:
- runtime identifiers;
- `PackageTargetFallback`;
- profiles;
- platform-qualified `net5.0`-era frameworks;
- every project style but `PackageReference`.

A `netstandard2.0` project's `TargetPlatformMoniker` of `Windows,Version=7.0`
is not declined, because `NuGetFramework.ParseComponents` ignores a platform
on any framework before `net5.0`.

The function models NuGet code, so like B₁ it is keyed on the restoring
NuGet's identity. It shares B₁'s verified list, which
`dgspec_resolve_diff::the_verified_list_is_the_graded_nuget` holds to the
oracle's NuGet, the one both differentials grade.

What it does not cover:
- **Multi-version downloads.** The writer joins two exact versions of one
  download with `;`, and `parse_dgspec` declines that spelling, so such lists
  decline.
- **Synthetic entries.** Everything graded here was generated. Real entry
  lists arrive at A₁.2.

#### A₁.2: the seam on real projects — landed

`tools/msbuild-condition-oracle`'s `restoreCone` op runs the census build
with `LogTaskInputs` set. It records the inputs of every `WriteRestoreGraphTask`
invocation, grouped by invocation, and returns those of the one that wrote
the requested `RestoreGraphOutputPath`:
- its `RestoreGraphItems`, each with its spec and custom metadata read as the
  task reads them (unescaped);
- its `RestoreRecursive`, as it was handed.

These are the writer's own inputs, after `RemoveDuplicates`.

`restore_cone_census.rs` grades the seam only on a cone whose every target
comes from the SDK. It puts that cone's entries through `write_restore_graph`
with two further inputs:
- the identity of the `NuGet.Commands` the census build loaded;
- the oracle's scrubbed environment.

A commit must equal the spec the cone wrote, byte for byte, or the census
panics. A cone with a user target declines with the verdict "a user target
ran in the cone".

That gate came out of review. A first cut built `_GenerateRestoreGraph`
separately, and the next four rounds each found another way a user target
could hand the writer inputs the census did not see:
- editing the entries before the writer;
- running the writer again for another file, under a qualified or
  differently cased task name, or from a child project;
- changing `RestoreRecursive` after the writer.

A user target can run anything around the writer. A₁ declines any cone with
one anyway, since A₁.5 keys trust on the cone's identity. So such a cone is
not graded, rather than chased hook by hook.

Each cone's verdict is pinned as a `seam` line in the census manifests:

| | SDK 10.0.401 (resolved) | SDK 10.0.112 (forced) |
|---|---|---|
| the six corpus projects | all six agree | all six decline: its NuGet is not verified |
| `cpm` (central package management, a project reference) and `net10` (pruning, an escaped `;` in metadata) | both agree | both decline: its NuGet is not verified |
| `hooked` | declines: a user target ran | declines: a user target ran |

So the seam commits on every real entry list of SDK targets the census sees
under a verified NuGet.

From here on, the entries are what the stages before the seam are graded
against: a stage that produces entries can be held to this op's output.

#### A₁.3: target execution

**What the cone reads.** A static census of the 50 targets' bodies on SDK
10.0.401 (references in conditions, bodies and task parameters) finds:
- 178 distinct properties;
- 56 item types, among them the SDK's `Known*` tables
  (`KnownFrameworkReference`, `KnownRuntimePack`, `KnownAppHostPack`, …)
  and many target-local `_Foo` lists;
- 16 metadata names;
- two static functions, `[MSBuild]::IsTargetFrameworkCompatible` and
  `[System.Guid]::NewGuid`.

The `MSBuild` task re-enters the project with `TargetFramework=<x>` (inner
builds) and enters referenced projects, so evaluation under other global
properties is needed too.

**How much of that evaluation already commits.** A one-off measurement
evaluated five corpus projects as the LSP does, under `restore_globals`,
which gives 890 property reads (5 × 178). Against MSBuild's
`-getProperty` it found:

| outcome | reads |
|---|---|
| committed, equal to MSBuild | 241 |
| committed, differing only in the SDK path's spelling (the devshell's linked `DOTNET_ROOT`, as stage 8 found) | 14 |
| global or reserved, outside the property table | 95 |
| absent, and empty in MSBuild | 537 |
| absent, but set in MSBuild | 3 |
| untrusted | 0 |
| committed and wrong | 0 |

The three absent-but-set reads are `PackageVersion` and
`RuntimeIdentifierGraphPath`, which the SDK's `.targets` half defines. That
half is not walked. So the property side is close, with two gaps:
- the targets half of the import chain;
- treating absence as a claim of "unset".

Untrusted reads, the risk that would sink A₁.3 whatever its design, do not
occur on these projects. The item side is the gap. `walk_generic_item_child`
(`evaluator/item_pass.rs`) already captures arbitrary item types with their
metadata into `State::evaluated_items`, with a taint per list, but
`into_project` discards those tables, and `<Target>`/`<UsingTask>` elements
are recorded only as unsupported constructs.

**Design: one evaluation, a second output.** Neither a separate evaluator
nor a hand-written model of the cone:
- **Not a separate evaluator.** The hard part of `borzoi-msbuild` is the
  import walk: explicit imports, the `Directory.Build.*` splices, the toolset,
  sticky globals and `TreatAsLocalProperty`. A second evaluation would
  duplicate it, and only one copy would sit under
  `fsproj_property_table_diff` and `fsproj_global_perturbation_diff`.
- **Not a hand-written cone.** It would have no oracle per piece, since
  `targetItems` runs XML. It would also forfeit the property that a release
  changing only target text needs re-verifying, not new code.

So the one evaluation gains an opt-in mode that also exports:
- the target definitions, carried by the retained-file mechanism the item
  pass already uses;
- the item tables, with their taint.

Execution lives in its own module, over copies of those tables. Every read
joins the trust of what it reads into what it writes. Target conditions go
through `decide_without`'s Kleene evaluation. Batching or `%(…)` outside
the modelled shapes declines. Trust is per property name and per item list,
which is fine enough: the answer is one `_RestoreGraphEntry` list, every
entry of which must be trusted. `NewGuid` yields a synthetic fresh identity,
which `write_restore_graph` never reads.

The stages are:

1. **A₁.3a, the read census — landed.** See *A₁.3a: what the cone reads*
   below.
2. **A₁.3b, the evaluation's second output.** Behind the opt-in mode, the
   evaluation retains `<Target>` elements (name, attributes, file, order) and
   exports the item tables instead of dropping them (landed; see the *A₁.3b*
   sections below). Two checks:
   - an invariant test: `ParsedProject` is identical with the mode on and
     off over the corpus, which is what protects the LSP;
   - an oracle: the `itemsMeta` op, widened to any item type on SDK
     projects, grades the exported tables.
3. **A₁.3c, the executor.** It covers:
   - intrinsic `PropertyGroup` and `ItemGroup` in targets;
   - `DependsOnTargets`, `BeforeTargets` and `AfterTargets`;
   - `Output` and `Returns`;
   - MSBuild's built-in tasks (`Message`, `RemoveDuplicates`,
     `ConvertToAbsolutePath`);
   - the `MSBuild` task within a project.

   A registry declines every other task. There are two oracles:
   - **Generated targets.** The `targetItems` op, widened to return properties
     and item metadata, runs generated targets as
     `fsproj_compile_order_generative_diff` does.
   - **Replay on real cones.** `restoreCone`'s `LogTaskInputs` capture widens
     to every task's inputs and outputs (`TaskParameterEventArgs` of both
     kinds). The executor runs with every leaf task replaced by its recorded
     outputs. That grades the engine at about two dozen checkpoints per
     corpus project before any A₁.4 leaf exists. It later grades each leaf
     against its recorded inputs and outputs, so A₁.3 and A₁.4 do not wait on
     each other.

##### A₁.3a: what the cone reads — landed

The restore-cone census reads, for each cone under the SDK its project
resolves, every property and item type its executed targets reference. It
takes those names from the targets' own XML (each target's last definition
in the file MSBuild ran it from). It then reads them in two places, both
under the census globals:
- at the end of our evaluation, through a probe on the walk
  (`test_support::parse_fsproj_reads`);
- at the end of MSBuild's, through the oracle's `evaluated` op.

The probe classifies each property read as one of:
- a trusted value;
- unset, exactly;
- unset but possibly set in the real build (a refused write, the
  environment, the toolset, or opacity);
- untrusted, with its cause.

For an item type it reports the evaluated table with its taint. The probe
changes nothing about the walk: the census asserts that the probed
`ParsedProject` equals the unprobed one. A trusted value MSBuild disagrees
with panics. Every other verdict is pinned, in
`restore_cone_reads_projects.txt` and `restore_cone_reads_fixtures.txt`.
A verdict says whether the cone itself writes the name, which separates the
targets' own locals from what they read off the evaluation.

On SDK 10.0.401 (six corpus projects, three fixtures):

| property reads | corpus | fixtures |
|---|---|---|
| value agrees with MSBuild | 319 | 169 |
| unset, and unset in MSBuild | 440 | 205 |
| unset, inexact (opacity), unset in MSBuild | 96 | 124 |
| unset, inexact (opacity), set in MSBuild | 1 | 2 |
| unset, inexact (the toolset), set in MSBuild | 12 | 6 |
| untrusted | 12 | 9 |
| `MSBuildThisFile*`, per file | 6 | 3 |
| **committed and wrong** | **0** | **0** |

The single-target projects read four properties off:
- `MSBuildAssemblyVersion` and `MSBuildStartupDirectory`, which the walk does
  not seed;
- `TargetFrameworkMoniker`, which rests on unmodelled text;
- `RuntimeIdentifierGraphPath`, an SDK package input.

The remaining inexact and untrusted reads belong to the outer builds of the
multi-targeting projects (`TypeEquality`, the `cpm` fixture). An undefined
`TargetFramework` is kept inexact there, by design, so everything past it is
opaque. Those reads matter only to the outer build's dispatch. The inner
builds run under a `TargetFramework` global, which A₁.5 evaluates.

The item side is the gap the census was built to size:
- **The SDK's `Known*` tables** (`KnownFrameworkReference`,
  `KnownRuntimePack`, `KnownAppHostPack`, `KnownILCompilerPack`, and others)
  and `ImplicitPackageReferenceVersion` sit untainted in our tables, and their
  identities equal MSBuild's, in order. But the generic capture keeps only
  NuGet's package metadata names, so **none of their metadata** survives
  (0 of 2 to 12 names per type). `ProcessFrameworkReferences` reads exactly
  that metadata.
- **`KnownWebAssemblySdkPack`** is tainted on four of the five cones that
  read it.
- **`PackageDownload`** is tainted on the two multi-targeting outer builds,
  where MSBuild's list is empty.
- **`PackageReference`, `ProjectReference`, `FrameworkReference` and
  `PackageVersion`** live in this crate's dedicated buckets, not the generic
  tables. Their counts agree with MSBuild's.

So A₁.3b's work, in order, is:
1. keep every metadatum on the generic tables, under the same taint
   discipline (landed; see *A₁.3b: every metadatum* below);
2. expose the dedicated buckets as item lists the executor can read
   (every dependency type and `ProjectReference` landed; see the *A₁.3b*
   sections below);
3. seed the two toolset names.

Nothing in the census argues against going on. No trusted read is wrong,
and the gaps have known causes.

##### A₁.3b: every metadatum — landed

The generic item tables keep every metadatum an item declaration gives,
not just NuGet's five package metadata names. Each value is kept as MSBuild
holds it, untrimmed, beside the trimmed value the dependency capture reads.
Three kinds of value are uncertain in the table:
- an attribute or body text whose source XML normalisation or entity
  decoding touches (a tab or line break in an attribute, a CRLF, a `&`),
  which MSBuild reads its own way;
- a body holding more than text and comments, which MSBuild takes as inner
  XML;
- a value MSBuild's Unix path fixup may rewrite.

The dependency capture keeps its own reading of all three (#642). The package
consumers still read only their five names, and ignore an uncertainty on any
other name. Nothing they commit to moves.

The probe makes a claim about a list only when the walk itself would take
that list as exact: it reads the list through `item_list_view`, the
predicate an `@(…)` reference in the walk uses. So an opaque walk, a tainted
list and an untracked one all decline together. A row copied from
`@(ProjectReference)`, whose metadata this crate keeps in its own model, says
its metadata is incomplete, so a name it lacks is unknown rather than
absent.

One side effect needed care. Reading a metadatum can read an undefined
`$(TargetFramework)`, and the walk records such a read: it is what decides
whether the walk keeps its first pass. A metadatum only a target reads must
not steer that choice. So the extra names are read with
`quiet_target_framework_reads` set, under which such a read is inexact and
records nothing. An item definition's default for any name marks that name
uncertain on the type's items, since the walk does not apply defaults.

Two checks hold it:
- **`fsproj_generic_items_generative_diff`** generates projects over the
  constructs the taint and uncertainty marks guard. These are literal,
  `$(…)` and escaped identities, `@(…)` copies and transforms, `Remove`,
  attribute and child metadata, conditions on either, `%(…)` and `@(…)` in
  metadata, padded, backslash-bearing and escaped path values, attributes
  with tabs, line breaks and entities, CRLF and entity bodies, bodies
  holding elements, processing instructions, comments or CDATA, the
  structural `Label` attribute, item definitions, an untrusted property, an
  imported item file (plain, or under a condition that turns the walk
  opaque), and copies of `@(ProjectReference)`.
  It holds every clean list to MSBuild's `itemsMeta`: identities in order,
  and every metadatum not marked uncertain, on every name either side holds.
  Reverting any of the capture's guards fails it.
- **The read census** compares metadata on real projects. On every corpus
  project and fixture, each `Known*` table and
  `ImplicitPackageReferenceVersion` now agrees with MSBuild on every name:
  12 for `KnownFrameworkReference`, 6 for `KnownRuntimePack`, and so on. No
  name is uncertain or missing.

##### A₁.3b: `PackageReference` rows — landed

`PackageReference` lives in the dependency capture, not the generic tables.
Its five metadata names are typed and trimmed there for NuGet's resolver. The
capture now also carries each reference's row as an item table would read it:
- every metadatum, read by the generic reader quietly, so it reads nothing
  the capture has not already read;
- copied whole from a helper item for `Include="@(Helper)"`;
- collapsed through `Update` onto prior `Include`s as MSBuild collapses it
  (`EvaluatedItem::updated_by`).

A generic list likewise counts as exact only when no identity keeps a
backslash, the evaluator's mark of a spelling that turns on MSBuild's working
directory.

An `Update` that writes no package metadatum is inert for the dependency
set and skipped there. But it may write other metadata on whichever
references it matches. So every name it may write is marked unknown on every
row, without resolving the match. Item-definition defaults for dependency
item types are recorded too, and mark their names uncertain.

The probe calls the list exact when the dependency set is certain and the
walk is not opaque, and every identity has NuGet's package-id shape (which
no path normalisation or Unix path fixup respells), which is the certainty `fsproj_packageref_generative_diff`
already grades for identities. `fsproj_generic_items_generative_diff` now
generates `PackageReference` items and `Update`s for every type, including a
versioned `Include` followed by an `Update`. It fails if the collapse or the
inert-`Update` marking is reverted.

On the fixtures, both single-target projects' references agree with MSBuild
on every name but one. `GeneratePathProperty` is uncertain: its value is the
F# SDK's item-definition default, which the walk records but does not
apply. Applying item-definition defaults, rather than declining them, is
what would make it exact.

##### A₁.3b: item-definition defaults — landed

The walk records each type's item-definition defaults with their values
(`State::item_definition_values`), as MSBuild's pass 2 leaves them:
- a later definition of a name replaces an earlier one;
- a definition whose group or own condition the walk cannot decide makes
  every name it sets unknown;
- so does a metadatum whose value or condition it cannot decide.

An item declared directly takes them beneath its own metadata. A copy
(`<K Include="@(H)"/>`) takes K's definitions beneath everything it carries
from H, H's own definitions included; probed, MSBuild does the same. Such
rows are `definitions_applied`, and the probe no longer marks a name
uncertain because a definition may set it. A copy of `@(ProjectReference)`,
whose definitions are not known, keeps that mark. As before, the dependency
capture keeps its own reading. It only runs in the differential tests'
build.

`fsproj_generic_items_generative_diff` now writes up to two definition
groups per project, with conditions on the group, the definition and each
metadatum: decidable either way, and undecidable for the walk while either
true or false in MSBuild. A fixed case pins the copy semantics. Each of
these reversals fails it:
- the first definition winning;
- trusting an undecidable condition;
- an item's definitions overriding its own metadata;
- a copy without its destination's definitions.

On the fixtures, every `PackageReference` now agrees with MSBuild on every
name, `GeneratePathProperty` included.

`FrameworkReference` takes the same rows (`included_row`, shared with
`PackageReference`) and the same exactness: a certain dependency set, no
opacity, and package-id-shaped identities. Every fixture's reference agrees
with MSBuild on every name. `PackageVersion` needs its own exactness first.
Until central versions are applied, any `PackageVersion` marks the whole
dependency set uncertain, so that flag cannot vouch for its rows.

##### A₁.3b: `ProjectReference` rows — landed

`ProjectReference` lives in the reference capture, which the LSP reads for
its project graph. Each reference declared by a literal identity also
carries its row, read the way `PackageReference`'s are: every metadatum,
quietly, over its type's item-definition defaults. The probe takes the list
as exact by the walk's own `@(ProjectReference)` view: no `Update` or
unapplied `Remove` may have run, no `Include` was hidden from the walk, no
item definition gives the type a default, and no identity keeps a
backslash. So the rows add metadata to a list whose identities the capture
already vouched for, and claim nothing it did not.

`fsproj_generic_items_generative_diff` generates `ProjectReference` items
like the other types. Dropping a row's metadata, or taking the list as exact
without the view, fails it. The census reads the list on every cone, and an
exact list whose identities are not MSBuild's panics there. Every corpus
project and single-target fixture holds no reference, so each reads as an
exact empty list. The one fixture with a reference, `cpm`, is a
multi-targeting outer build, whose walk is opaque.

##### A₁.3b: `PackageVersion` rows — landed

`PackageVersion` takes the same rows as `PackageReference` (`included_row`),
but not its certainty. Every `PackageVersion` marks the package set uncertain
until central versions are applied, so that flag cannot vouch for the list.
The list asks the set's recorded causes instead. Each cause raised while
walking a dependency element is attributed to that element's type, and each
raised while collapsing `PackageReference`s to `PackageReference`
(`State::package_cause_owners`). The `PackageVersion` list is exact when
every cause is attributed to another type or is a `PackageVersion`'s own
existence, no `Update` or `Remove` of one ran, the
walk is not opaque, and every identity has the package-id shape. A cause
raised anywhere else declines: on a `PackageVersion`, an item group, an
import, an item definition.

Two rules of the dependency capture, for every dependency type, keep a
cause attributed elsewhere from leaking into the list:
- A copy of a list this crate keeps in its own bucket
  (`Include="@(PackageReference)"`, `@(Compile)`, and so on) is unevaluable:
  there is no item table to read it from.
- An `Exclude` is matched by normalised path (`item_spec::comparison_key`),
  as MSBuild matches it and a `Remove`, so `Exclude="./a"` removes `a`. An
  identity outside that key's domain declines. Matching as lowercased text
  would keep an excluded `PackageReference` as well.

The attribution is sound only because every write of the package set's flag
records a cause. `fsproj_generic_items_generative_diff` generates
`PackageVersion` items like the other types. Counting every cause as
attributed fails it. Ignoring an `Update` or `Remove` fails the soak and a
unit test. On every single-target corpus project and fixture the list is
exact (and empty), beside a `PackageReference` set that stays uncertain.
`cpm` and `TypeEquality` are multi-targeting outer builds, whose walks are
opaque.

`GlobalPackageReference` is the same with its own marker: its list is exact
when every cause is attributed to another type or is a
`GlobalPackageReference`'s existence, no `Update` or `Remove` of one ran, the
walk is not opaque, and every identity has the package-id shape. The
generator writes it as well. Ignoring its `Update` or `Remove`, or a cause
attributed to it that is not its marker, fails the soak. No cone the census
runs reads it.

##### A₁.3b: target definitions — landed

The walk records every `<Target>` element it reaches: its `Name`, the file,
and the element's byte range there (`State::target_definitions`, test builds
only). The probe answers a target name with the last definition of it, as
MSBuild keeps the last and drops the rest. The executor (A₁.3c) reads a
target's XML from that file and range. The answer is exact only when:
- the walk is not opaque, so no file it did not reach can define the name
  later;
- every `Directory.Build.*` file the walk spliced in itself stands for an
  import an SDK chain really makes. The props splice must have first run at
  `Microsoft.Common.props`'s import point, whose condition, decided before
  the splice ran, is exactly true and names the file spliced. Every other
  props import point the chain reaches, each of which the walk suppresses,
  must decide exactly false: MSBuild imports nothing there. The targets
  splice is a fallback the walk makes only when no chain reached its import
  point first, which MSBuild never makes, so it always declines;
- the name, and every name defined, is plain: printable ASCII, no `%`, no
  whitespace. MSBuild compares names unescaped, untrimmed and
  `OrdinalIgnoreCase`, which on plain names is ASCII case folding.


None of this makes a read more exact than the walk's import graph, and that
graph has known divergences from MSBuild's (#677: a re-import through a
symlink alias, which the walk dedupes by canonical path and MSBuild by
lexical path). They bound the property table equally. #677 proposes the
systematic fix, a differential over generated SDK import graphs.

The oracle's `evaluated` op reports where MSBuild's own target table keeps
each definition (file, line, column).
`fsproj_target_definitions_generative_diff` generates targets before and
after imports that are taken, skipped or undecidable, a nested import, a
`Directory.Build.*` file beside the project, and names that differ by case,
escape, padding and non-ASCII case, and holds every exact read to that
table. Its soak fails if any of the three conditions is dropped, or if the
first definition wins.

The census asks MSBuild's target table for every target MSBuild ran in the
subject project and holds our exact reads to it. On SDK 10.0.401 every
single-target corpus project and fixture retains every target of its cone
that way (39 to 42 each). The two multi-targeting outer builds are opaque.
No target in these cones is defined twice, so the override rule is held by
the generator, not the census.

##### A₁.3c: the schedule and intrinsic groups — first slice

The executor (`evaluator/executor.rs`, test builds only, behind the probe's
`execute`) runs targets over a copy of the walk's end state. It ports
MSBuild's own algorithm rather than a reading of the documentation, because
the two differ where it matters:
- **The schedule** is `TargetBuilder`'s stack. When a target comes up, its
  after-targets go beneath it, then the target, then its before-targets, then
  its dependencies on top. A target's condition is decided when its
  dependencies are worked out (`TargetEntry.GetDependencies`). A false one
  completes it as skipped, with no dependencies, and its before- and
  after-targets still run. A skipped target is decided again the next time
  it comes up; an executed one never runs again. An after-target already
  anywhere on the stack is not pushed again, and a dependency or
  before-target already in its parent chain is a circular-dependency error.
- **The definitions** are the walk's retained ones. Each name keeps its last,
  standing where that last one does, and the before- and after-maps are
  built from them in that order, with `BeforeTargets`/`AfterTargets`
  expanded against the evaluation and dropped where the named target does
  not exist (`Evaluator.ReadTargetElement`,
  `AddBeforeAndAfterTargetMappings`).
- **Item references.** The walk's item tables hold identities decoded, and
  MSBuild expands a list from their escaped spellings, which decoding loses.
  So item lists are read only where spelling cannot tell:
  - in a condition, a plain `@(Type)` bounded by quotes, parentheses,
    whitespace or its ends reads as MSBuild compares it, through a property
    no project can write (`-ItemList-Type`). A list holding an identity
    whose spelling is ambiguous (padded, or with `%`, `@`, `$`, `(`, `)`,
    `*`, `?` or `'`) declines;
  - in an `Include`, a piece that is a lone plain `@(Type)` copies those
    items, metadata and all, with the destination's item-definition
    defaults beneath and the element's own metadata on top.

  An item list in a value (a property, a metadatum, `Message` text) declines
  until the tables carry escaped spellings: with `J` spelt `%34%31`, a
  property holding `@(J)` later run into a `%` reads `%41` in MSBuild. So do
  other item expressions (transforms, separators, `@(0)`, which MSBuild
  leaves literal), a property value that could join into one, and a read of
  a property starting with `-`.
- **Property functions** run as the walk's expression evaluator runs them
  (graded by `property_expr_diff`): every property an expression reads, a
  receiver or an argument, must be one the run may read, and what the
  evaluator does not model, or reports as failing, declines, as does an
  item reference in a function's arguments (MSBuild expands properties
  first, so the function receives the reference as text).
- **Conditions and target lists** (`DependsOnTargets`, `BeforeTargets`,
  `AfterTargets`) are read as MSBuild reads them: the source text with XML's
  five entities decoded and its whitespace kept. MSBuild does not normalise
  attribute values, so a tab or line feed inside a quoted string is itself,
  not the space the XML reader makes it. The reader's value only checks that
  this is the one difference; a carriage return, a character reference or
  another entity declines. The condition and expression evaluators are
  graded on such text by the whitespace axes of `condition_diff` and
  `property_expr_diff` (the oracle's `eval` op writes the condition's
  whitespace as character references, so MSBuild sees it unnormalised). The
  walk still reads its own conditions normalised (#750).
- **A task whose condition is false** does not run, in MSBuild or here: only
  one that would run declines. But MSBuild looks for batching in a task's
  parameters before its condition, so a task mentioning `%(…)` declines
  whatever its condition.
- **Bodies**: intrinsic `PropertyGroup`s, `ItemGroup` `Include`s without
  batching (each new item taking its type's item-definition defaults beneath
  its own metadata, `Label` not metadata), and `Message` with the parameters
  it accepts. Text and attributes are read by the walk's own XML rules:
  padding kept, and a value that XML normalisation or entity decoding
  touched declines, as does inner XML.

A target's `Returns` shapes only what it hands a caller, and no caller is
modelled. But MSBuild expands it once the target has run, against the
properties the target left, and an expression there can fail the build: a
property function that throws, a transform, an item list run together with
text (`prefix@(I)`, `@(I)@(I)`). So it is checked once the target has run.
Each `;`-separated piece must be a lone plain `@(Type)`, or text with plain
`$(Name)` references and no `@`, and no value it reads may hold `@`, `%`, `$`
or `;`. `KeepDuplicateOutputs`, a condition evaluated then too, declines.

It declines everything else: another task, a built-in task a `UsingTask`
may rebind, an empty request (the default targets), `InitialTargets` (not
run yet),
`Inputs`/`Outputs`, a backslash in an item identity or metadatum
(MSBuild's Unix path fixup may respell it),
`%(…)` and `@(…)` (including one properties assemble, which MSBuild then
expands as such), a property function, `MSBuildThisFile*` (which name the
defining file inside a target), a read of any value the walk does not vouch
for, a write to a reserved or global property, and a graph in which a target
can bring itself up again through hooks or literal dependencies. MSBuild
hangs on some of those graphs (two targets `AfterTargets` each other).

The oracle's `runTargets` op builds targets in-process and reports every
target that started, in order, with the properties and items left.
`fsproj_target_execution_generative_diff` generates target graphs
(dependencies literal or read from a property, hooks literal or expanded,
redefinitions, cycles, conditions on what earlier targets write) and holds
every committed run to it. The scheduling rules above came out of its
failures. Its soak fails when any of the three push rules is changed.

The census runs `GenerateRestoreGraphFile` on every cone and pins where the
executor stops. Every single-target cone stops at item batching, a
condition reading `%(RestoreGraphProjectInputItems.Extension)`. The two
multi-targeting outer builds stop at opacity.

**`MSBuildProjectDefaultTargets`.** The hook
`BeforeTargets="$(BuildDependsOn);Build;$(RebuildDependsOn);…"` in
`Microsoft.Common.CurrentVersion.targets` reads `RebuildDependsOn`, which
rests on the reserved `MSBuildProjectDefaultTargets`. The walk sets it as
MSBuild's `Evaluator.UpdateDefaultTargets` does. The first project or
import root, in depth-first import order and before its own implicit
imports, whose `DefaultTargets` expands (properties only) to anything sets
it, unescaped and untrimmed. MSBuild stores that unescaped text as the
escaped value, so `%2541` reads back as `A`. The first that lists a target
settles it. Blanks or separators alone leave it open to a later root. On SDK
projects it is `Build`, from `Microsoft.Common.targets`. It is left unset
and inexact, settling the offer, by a root whose expansion the walk cannot
vouch for, by a root in a `Directory.Build.*` file the walk spliced in
itself, by any root outside an SDK's files once such a splice has run
(whatever it wrote may not exist in MSBuild's build, #676), or by opacity
while the offer is open. Beyond that it is exactly as faithful as the walk's
import graph, like every other property: an SDK that never imports
`Directory.Build.props` while the walk splices it in can feed it a value
MSBuild never holds, as it can feed any property (#677).
`fsproj_default_targets_generative_diff` holds it to MSBuild over entry and
imported roots, literal, padded, escaped, list, empty and property-expanded
values, blanks and separators, a `Directory.Build.props` beside an SDK-less
project, and an import the walk cannot decide. Each of these fails its soak:
trimming, the last root winning, settling on blanks, latching in a splice,
ignoring opacity, or re-escaping.

#### A₁.4: the task leaves

These are the 23 non-built-in tasks of stage 8's census. (`RemoveDuplicates`
and `ConvertToAbsolutePath` are MSBuild built-ins, which A₁.3c covers.) Each
gets a Rust model and two oracles:
- a per-task differential, which runs the real task inside a generated
  project;
- A₁.3c's replay, which holds the model to the inputs and outputs the task
  recorded on every real cone.

They come in this order:
1. **NuGet's `GetRestore*Task` converters.** These are thin.
2. **`GetRestoreSettingsTask`.** This is `NuGet.Configuration`, and it overlaps
   `settings::global_packages_folder`.
3. **The SDK's tasks.** `ProcessFrameworkReferences` and `GetPackagesToPrune`
   read data the SDK ships (`KnownFrameworkReference` items and the pruning
   lists).

#### A₁.5: composition

The `MSBuild` task's inner builds re-evaluate the project under
`TargetFramework` and walk project references, using `borzoi-msbuild`'s
evaluation under global properties. **Gate:** A₁'s spec equals A₀'s on every
corpus project and fixture it commits on. A₁ is keyed on the cone's identity,
meaning the target file hashes and task-assembly identities that stage 8
pins, through a verified list derived from those runs. Stage 9 then puts it in
front of `restore.rs`.

### Stage 9: spawn-free runtime path

**Dependencies:** stage 4 (and whichever of 5–7 the census needs), plus A₁
landed (stage 8's choice).
**Implements:** *Runtime composition.*

**Correctness oracle:** whole-project gate. For every corpus project where the
spawn-free path commits, its compile assemblies equal `restore.rs`'s, pinned
per project in `crates/corpus-diff/manifests/`. The global-perturbation sweep
runs over the half-A producer, since A₁ is exactly where a global it does not
model could move a value.
