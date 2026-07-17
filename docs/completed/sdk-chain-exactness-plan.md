# SDK-chain exactness plan (nuget slice 4b remainder) — COMPLETE

> **Status:** done. A plain `<Project Sdk="Microsoft.NET.Sdk">` / `net10.0`
> project evaluates through the **real** SDK import chain with a *certain*
> dependency set (`package_references_uncertain == false`, net10.0 fixture: 0
> causes). The five `sdk_style_*` acceptance fixtures in
> [`fsproj_packageref_diff.rs`](../crates/msbuild/tests/fsproj_packageref_diff.rs)
> run un-ignored against `dotnet msbuild -getItem`. This was the last 4b
> increment of [`nuget-restore-plan.md`](../nuget-restore-plan.md); the
> property-expression prerequisite
> ([`property-expression-plan.md`](property-expression-plan.md)) was done
> first.

## Landed stages (one line each)

- **Stage A** (PR #884) — `<Choose>`/`<When>`/`<Otherwise>` evaluate
  first-match-wins in the property pass; discharged the 4 `UnsupportedChoose`
  causes and defined the FSharp shim properties two hook imports hung on.
- **Stage B** (PR #888) — SDK resolution generalised to
  `SdkResolution::{Single, Roots}` (`<Import Project=P Sdk=S/>` imports `P`
  against every root; zero roots is an exact no-op); the two workload locators
  resolve in `sdk_resolver::workloads` (`WorkloadEnvironment`) with an
  `UnsupportedLayout` degrade envelope; discharged the 2 `SdkNotFound`s and
  pulled the 16 manifest files into the walk.
- **C.1** (PR #890) — pinned `[System.IO.Path]::IsPathRooted`,
  `[MSBuild]::IsOSPlatform`, `[MSBuild]::AreFeaturesEnabled` (oracle-grounded;
  ChangeWaves threshold treated as the reserved `MSBuildDisableFeaturesFromVersion`).
- **C.2a** (PR #893) — environment-backed properties:
  `parse_fsproj`/`parse_fsproj_with_imports` take an `environment` snapshot;
  referenceable env names seed as overridable starting values (reserved /
  toolset-computed / case-colliding names are not promoted).
- **C.2b** (PR #894, LSP tail #963/#964) — the exact-undefined-read guard
  (`State::undefined_read_is_exact`, `walk_opaque` latch) plus the
  `EnvExtensionsPath::{Absent, Value, Unspecified}` opaque-vs-absent state;
  `<Import Project>` semicolon-list semantics. #964 seeds
  `MSBuildUserExtensionsPath` in the LSP (in-process `LocalApplicationData`
  derivation) so a real editor walk stays transparent, and #963 refines the
  ProjectReference item-definition-default certainty guard the transparent walk
  now reaches.
- **Stage C keystone / residuals** (#956 `[System.String]::IsNullOrEmpty`;
  #957 inert item-definition default on uncaptured metadata; #958 inert
  `Update` items) — drove the net10.0 fixture's last causes to 0.
- **C.2c** (in #958) — the five `sdk_style_*` fixtures un-ignored; the three
  keystone sentinels kept as focused per-mechanism regression guards.

## Acceptance gate & ratchet

`crates/msbuild/tests/sdk_chain_expression_census.rs` takes every `$(…)` call
expression and `Condition` from the pinned SDK's own `.props`/`.targets` and
runs each through the evaluator against the MSBuild oracle, asserting
certain-implies-exact plus coverage ratchet floors (declined shapes print
bucketed by function under `--nocapture` — the historical worklist). It stays
live as a regression ratchet.
