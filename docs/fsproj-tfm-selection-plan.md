# TFM selection & configuration policy (fsproj consumption 3.3c)

> Detailed design for **Stage 3.3c** of
> [`fsproj-project-graph-plan.md`](completed/fsproj-project-graph-plan.md). Sibling of the
> completed [`multi-tfm-resolution-plan.md`](completed/multi-tfm-resolution-plan.md)
> (the *assets-side* producer-TFM resolver) and
> [`csharp-sidecar-plan.md`](completed/csharp-sidecar-plan.md).
>
> **Status:** implemented (3.3c-1 #878; 3.3c-2 #879, with 3.3c-3/E4 folded in;
> E7 last). The LSP picks one entry TFM per project (first-declared) and threads
> it coherently through the parse (defines + Compile items, so multi-targeted
> projects fold at all), the assembly env (assets-target selection +
> platform-suffix recovery for C# refs), and the `.fsproj`-buffer diagnostics.
> The E-decision IDs below are referenced from code comments (`tfm_policy.rs`
> E1/E2/E7, `lib.rs` E4).

## Background (reference)

Evaluating a `.fsproj` **without** a chosen TFM degraded four things for
multi-targeted projects (`<TargetFrameworks>net8.0;net10.0</TargetFrameworks>`)
and platform-qualified references: TFM-gated `<DefineConstants>` and `<Compile>`
went `*_uncertain` (blocking the fold to single-file fallback), the assets
resolver hit `MultipleOrNoTargets` (empty env), and C# project refs missed the
producer's real platform-qualified target. `$(Configuration)` was separately
hard-coded to `Debug` across several sites with no single policy. 3.3c settles
all of these by choosing **one entry TFM per project** and threading it
everywhere — a project parsed under `net8.0`'s defines but resolved against
`net10.0`'s assemblies is incoherent, worse than under-resolution (gospel P5).

## Landed (one line each)

Stages (each on its own branch):

- **3.3c-1** (#878) — entry-TFM selection, parse side: implements E1, E2, and the
  E5 `Workspace` accessor.
- **3.3c-2** (#879) — thread the TFM into the assembly env + C# refs: implements
  E3, E5 (env side), E6; E4 (`$(Configuration)` policy) and stage 3.3c-3 folded
  in here.
- Follow-up hardening (#906, under 3.3d) — distrusts the entry project's *own*
  untrusted-provenance body-written TFM; added `Workspace::served_tfm_for_project`
  / `ServedTfm`.

Settled decisions (IDs cited from code):

- **E1** — Policy: serve the **first-declared** TFM (`target_frameworks()` returns
  document order). Deterministic and guess-free; matches VS/Ionide design-time
  convention. Override deferred (see [Out of scope](#out-of-scope)).
- **E2** — Two-pass `evaluate_project` (`select_target_framework`, deciding via
  `tfm_policy::tfm_choice` since E7): parse
  TFM-unseeded to read `target_frameworks()`, then re-evaluate with
  `TargetFramework=<first>` seeded **only when it changes the answer** (caller
  didn't own the global; pass-1 evaluated `TargetFramework` empty; ≥1 TFM
  declared). Records `chosen_tfm: Option<String>` on `EvaluatedProject` for
  *every* project (making E5's accessor total).
- **E3** — Thread the chosen TFM into the assembly env: `resolve_assemblies_for_tfm`
  selects the assets target by TFM (via `lookup_target_for_tfm`, alias fallback);
  the `assembly_envs` cache key gains the TFM; `csharp_project_ref_dlls` roots
  Phase 2b at the entry (`resolve_transitive_project_tfms(entry_fsproj, entry_tfm)`)
  to recover each C# ref's platform-qualified producer TFM, falling back to the
  base-TFM behaviour on `None`/partial restore.
- **E4** — One `$(Configuration)` policy value (`borzoi::BUILD_CONFIGURATION
  = "Debug"`), collapsing the four production sites: `Workspace::default_build_properties`,
  `csharp_project_ref_dlls`, `fsproj_diagnostics::default_global_properties`, and
  `path_has_debug_config`. No functional change; init-option exposure deferred.
- **E5** — Coherence invariant (machine-enforced): parse and env both source the
  TFM from one `Workspace::target_framework_for_project`, so env-TFM == parse-TFM
  for any project (property-tested).
- **E6** — Under-resolve, never cross-resolve (D5): a stale restore missing the
  chosen TFM's target degrades to **empty for that TFM**, never another TFM's
  assemblies.
- **E7** — The `.fsproj`-buffer diagnostics path serves the same inner build.
  Detail below, because what it gives up is worth stating.

### E7 — Align the `.fsproj`-buffer diagnostics path

`fsproj_diagnostics.rs` evaluated the open `.fsproj` *buffer* with its own global
seeds (`Configuration` + `Platform` only) and no `TargetFramework`, on the reading
that the buffer describes the project file's evaluability *in general*. Measured,
that reading did not hold. Unseeded, `$(TargetFramework)` reads empty, so **every**
`'$(TargetFramework)' == 'netX'` gate is cleanly false and no inner build's content
is evaluated at all: on a two-TFM fixture the buffer produced four
`UndefinedProperty` warnings the real inner build never emits and diagnosed the
content of neither branch. It was describing the outer dispatch build, which has
no content.

The fix is a two-pass over the **buffer text** — never the workspace's
disk-derived choice, so an unsaved `<TargetFrameworks>` edit takes effect
immediately — sharing the decision, not the parse, with workspace resolution:
`crates/lsp/src/tfm_policy.rs` holds `TfmChoice` and `tfm_choice`, and each
surface performs its own re-evaluation. Only `TfmChoice::Reseed` (a multi-targeted
project with no body singular and trusted provenance) costs a second parse.
The same seam fixed a second divergence found on the way: `diagnostics_for` built
its own defaults bag and ignored `Workspace::extra_build_properties` entirely, so
a caller who pinned `Configuration=Release` saw it honoured in resolution and
ignored in the squiggles. There is now one bag, `Workspace::build_properties`.

**What this gives up.** Seeding *moves* the diagnosed region rather than only
adding to it. Content gated on `'$(TargetFramework)' == ''` or
`!= '<served>'` was reached by the outer build and is not reached by the inner
one. That is a deliberate consequence of serving one TFM — every other LSP surface
already speaks about that same inner build — and it is pinned in both directions
by `served_region_follows_the_served_tfm`, which asserts each gate shape against
both columns rather than leaving the tradeoff to prose.

**What it costs.** Measured on a real SDK-resolved two-TFM project, release
profile: one pass 7.5 ms, two passes 36.4 ms. The extra 29 ms is the *inner*
build's evaluation, not the second parse as such — a seeded evaluation walks far
more of the SDK's targets chain than the outer dispatch build does, so even a
hypothetical single-pass implementation that went straight to the seeded
evaluation would pay it. Single-TFM, body-pinned and no-TFM projects pay nothing:
they never reach `TfmChoice::Reseed`.

`diagnostics_for` is deliberately left uncached. It runs per publish and, via
`workspace/diagnostic`, once per discovered `.fsproj` per sweep — but that sweep
also parses and semantically analyses every `<Compile>` item in every project, so
36 ms per *project* is not where its time goes. The lever if that ever changes is
a memo on `(path, text, build globals)`, which is sound because the function is
pure in exactly those; reaching for the workspace's cached evaluation instead
would only be correct when buffer == disk, and is what
`the_served_tfm_comes_from_the_buffer_not_from_disk` exists to catch.

**What guards it.** `buffer_diagnostics_follow_the_workspace_served_tfm` is E5's
coherence invariant extended to this third surface: for a buffer matching disk,
the branch the diagnostics evaluate is exactly the branch
`Workspace::target_framework_for_project` serves — no more, no fewer. Nothing
asserted that before, which is how this path could diverge for two whole stages
without a test going red; the property is the durable half of the change.

It immediately earned itself. A seeded `TargetFramework` global is read-only to
the document *unless* the document says otherwise with `<Project
TreatAsLocalProperty="TargetFramework">`, and a body write gated on the seed
being non-empty then fires in pass 2 only — invisible to pass 1, so the policy
never sees it. `select_target_framework` published the seed it asked for rather
than the value pass 2 evaluated under, so since 3.3c-1
`target_framework_for_project` had been naming a branch the parse did not take:
defines and Compile items from one TFM, assets selection from another. The E7
work did not introduce that; it made it *observable*, because a second surface
finally existed to disagree with. Both now read the effective value out of pass
2's property table, which is where MSBuild puts an override — pinned
certain-implies-exact by `fsproj_global_perturbation_diff`'s
`TreatAsLocalProperty` corner.

### The override that is not worth modelling

A document may opt `TargetFramework` out of global read-only-ness with `<Project
TreatAsLocalProperty="TargetFramework">` and then overwrite the very seed we set
to serve its inner build. Six review rounds went into serving such a document
before the answer turned out to be that it cannot be served at all. The rounds
are recorded because the sequence, not any one finding, is the lesson.

Rounds 1–4 each fixed one consumer of the resulting state: the value published
(the seed, not the override), the provenance verdict (pass 1 cannot judge a write
pass 1 never performs), the absent-vs-empty distinction (a value-shaped
`Option<String>` cannot tell "no override" from "overridden to empty" — the
`absent-vs-unread` class), and the graph node's label. Round 5 both carried a
regression from round 4's fix *and* landed the decisive observation: the final
property-table value **cannot classify the pass at all**. MSBuild evaluates in
document order, so a `$(TargetFramework)`-gated `<PropertyGroup>` above the
override has already contributed the *seed's* defines and items while the table
ends at the override's value. The pass is a chimera of two builds. Round 6 then
found the sixth consumer — an outer-build-only `<ProjectReference>` reaching the
compile closure — which is when the pattern became unmistakable.

**So the seeded pass is discarded, not flagged.** On detecting an override,
`select_target_framework` keeps **pass 1** — the outer dispatch build, whose one
virtue is being internally consistent — reports no TFM for it, and sets
`EvaluatedProject::not_an_inner_build`, which withholds the reference list as
well as the TFM. A flagged chimera has to be audited at every surface that reads
the evaluation, and six rounds found six such surfaces one at a time; a discarded
one has no surfaces.

The reference list needs withholding *separately* from the TFM, which is the
subtlety rounds 1–5 kept missing. `tfm_untrusted`'s consumers keep pass 1's edges
on the argument that a TFM-dependent edge read the unpinned `TargetFramework` and
so already flagged itself — true when the singular is unpinned, false here, since
a multi-targeted document never writes the singular and therefore decides
`'$(TargetFramework)' == ''` cleanly under the environment model. An
outer-build-only edge is thus captured as fact
(`an_override_document_does_not_publish_outer_build_only_edges`).

**Declining costs nothing measurable.** Zero projects in the pinned F# corpus or
the local NuGet cache opt `TargetFramework` out — the 18 real occurrences of the
attribute opt out `RepoRoot` (15), `OutDir` (2), `WasmNativeWorkload` and
`RestoreAdditionalProjectSources`. That measurement is what settled it: the
alternative to declining is per-property TFM provenance through the whole
evaluation, and no observed project would benefit.

Round 4's graph change was reverted along the way: an override now returns
`Unresolved` from the untrusted arm above and never reaches the single-target
arm, so that arm reads the declaration again — which is also what a
caller-supplied *empty* global needs
(`an_empty_tfm_global_keeps_the_sole_declaration_on_the_node` pins it; reading
`chosen_tfm` there was round 4's regression). `declared_tfms` likewise stays a
pass-1 capture: it is the *outer* build's declaration list, which is what the
TFM-invariant intersection wants.

**Three surfaces publish a TFM** — what the parse ran under, what the assembly
env may key assets selection on, and how a graph node is labelled for a consumer
locating its output — plus the reference list makes four things this shape
touches. `every_tfm_surface_agrees_on_a_pass_two_override` is a table over the
surfaces against one fixture, so the next cross-cutting shape costs one round
instead of six. Its rows vary what the document says and agree on what we serve;
the variety exists so that an implementation which *starts* distinguishing
overrides again fails there.

The generator axes (`treat_as_local`, `seed_conditional`, `untrusted_gate`,
`override_empty`) exist because these shapes are not ones a reviewer should have
to think of twice. A property whose generator cannot build a shape agrees
vacuously on it; widening the axes is the fix, not adding case N+1. Each round's
axis reproduced that round's finding independently before it was fixed, and
`untrusted_gate` additionally corrected the property itself — stated against
`served_tfm_for_project` it demanded the buffer diagnose no branch for a project
whose parse legitimately took one, which is why `parsed_tfm_for_project` exists
and is the currency.

## Out of scope

- **TFM override** — an LSP init option or per-file selection. First-declared is
  v1; the E5 accessor (`target_framework_for_project`) is the natural seam to add
  it later.
- **Modelling SDK-injected per-TFM defines** (`NET8_0`, `NETCOREAPP…`). Still the
  accepted `define_constants_uncertain` limitation; 3.3c resolves *user-authored*
  TFM-gated defines by seeding the property, not by running SDK targets.

## Settled: SDK-derived TFM properties in conditions

A condition on `$(TargetFrameworkIdentifier)` / `$(TargetFrameworkVersion)` read
from `Directory.Build.targets` used to see `''`, which made
`'$(TargetFrameworkIdentifier)' == '.NETFramework'` *cleanly false* rather than
undecidable — so the gate committed the wrong branch, and
`FSharp.Profiles.props` in the pinned F# corpus published two `#if` symbols the
real build never defines.

The cause was **position, not a missing derivation**: the walker already
computes the pair correctly, by walking the SDK's own
`Microsoft.NET.TargetFrameworkInference.targets` (both intrinsics it needs are
implemented). But `Directory.Build.targets` was spliced *before* `Sdk.targets`,
i.e. before the inference — and the walk's duplicate-import skip then suppressed
the real chain's import at the real position. So the file was walked exactly
once, at the wrong time. Walking `Sdk.targets` first and splicing only when the
chain did not import it for itself fixes it for every SDK-derived name at once,
and the corpus project now matches MSBuild exactly.

It cost coverage, honestly: reaching the F# repo's `Directory.Build.targets` at
its real position also reaches an import gate that was previously false, pulling
in constructs we cannot model, so `compile` and `define_constants` on two test
projects moved from *matched* to *declined* (corpus facets 33→30, divergences
2→1). Those declines have precise, actionable causes (`<Target>`,
`Regex::Replace`) rather than being a blanket refusal; the previous matches were
luck, not capability.

Guarded by `tests/fsproj_derived_tfm_diff.rs` — a generative differential over
{SDK kind × TFM spelling × pre-set pair × read position} against the real
evaluator. It exists because three review rounds of hand-written fixtures each
asserted behaviour real MSBuild does not have; see its module docs.
- **`Directory.Build.props` for SDK-less projects.** MSBuild reaches
  `Directory.Build.props` / `.targets` only through `Microsoft.Common.props` /
  `.targets`, so a bare `<Project>` with no `Sdk` attribute and no `<Import>`
  gets neither. The walker splices both unconditionally, and for such a document
  therefore commits values MSBuild's property table does not contain — a wrong
  commit, measured by `tests/fsproj_derived_tfm_diff.rs` (which excludes it by
  not planting witnesses at those positions under its SDK-less arm, and says so).
  The targets side was fixable by walking `Sdk.targets` first and splicing only
  if the chain did not; the props side is not, because the splice has to happen
  *before* the body that holds the `<Import>`s which would tell us. Old-style
  projects import `Microsoft.Common.props` explicitly, so the splice is right for
  them and wrong only for genuinely bare documents.
- **RID selection** (`RuntimeIdentifier`) — orthogonal; the assets resolver
  already prefers the bare-TFM target over RID-qualified ones.
