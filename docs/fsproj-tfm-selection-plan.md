# TFM selection & configuration policy (fsproj consumption 3.3c)

> Detailed design for **Stage 3.3c** of
> [`fsproj-project-graph-plan.md`](completed/fsproj-project-graph-plan.md). Sibling of the
> completed [`multi-tfm-resolution-plan.md`](completed/multi-tfm-resolution-plan.md)
> (the *assets-side* producer-TFM resolver) and
> [`csharp-sidecar-plan.md`](completed/csharp-sidecar-plan.md).
>
> **Status:** implemented (3.3c-1 #878; 3.3c-2 #879, with 3.3c-3/E4 folded in).
> The LSP now picks one entry TFM per project (first-declared) and threads it
> coherently through both the parse (defines + Compile items, so multi-targeted
> projects fold at all) and the assembly env (assets-target selection +
> platform-suffix recovery for C# refs). One documented follow-up remains: **E7**
> — the `.fsproj`-buffer diagnostics path stays deliberately TFM-unseeded (detail
> under [Still to do](#still-to-do)). The E-decision IDs below are referenced from
> code comments (`workspace.rs` E1/E2, `lib.rs` E4).

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
- **E2** — Two-pass `evaluate_project` (`select_target_framework`): parse
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

## Still to do

### E7 — Align the `.fsproj`-buffer diagnostics path (documented follow-up)

`fsproj_diagnostics.rs` evaluates the open `.fsproj` *buffer* with its own global
seeds (`default_global_properties`: `Configuration` + `Platform` only) and **no**
`TargetFramework`. This remains true post-3.3c, so the two surfaces diverge on
`$(TargetFramework)` conditions: workspace resolution evaluates them cleanly while
the buffer still shows the undefined-property diagnostic.

v1 deliberately keeps the buffer path unseeded — it describes the project file's
evaluability *in general*, it works on unsaved text (so the workspace's
disk-derived `chosen_tfm` may not even match the buffer), and changing its
diagnostics is not needed for coherent resolution. Aligning it is a two-pass over
the buffer text (mirroring E2's `select_target_framework` but reading the buffer's
own `target_frameworks()`); the divergence is recorded here so it isn't
rediscovered as a bug.

## Out of scope

- **TFM override** — an LSP init option or per-file selection. First-declared is
  v1; the E5 accessor (`target_framework_for_project`) is the natural seam to add
  it later.
- **Modelling SDK-injected per-TFM defines** (`NET8_0`, `NETCOREAPP…`). Still the
  accepted `define_constants_uncertain` limitation; 3.3c resolves *user-authored*
  TFM-gated defines by seeding the property, not by running SDK targets.
- **SDK-derived TFM properties in conditions.** Seeding `TargetFramework` fixes
  only conditions written directly against `$(TargetFramework)`. A condition on
  `$(TargetFrameworkIdentifier)` / `$(TargetFrameworkVersion)` (SDK-computed)
  remains unevaluable, still flips the `*_uncertain` flags, and the project still
  refuses to fold.
- **RID selection** (`RuntimeIdentifier`) — orthogonal; the assets resolver
  already prefers the bare-TFM target over RID-qualified ones.
