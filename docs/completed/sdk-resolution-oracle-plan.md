# SDK-resolution differential oracle — plan (landed)

> **Status:** complete. All three stages landed; the executable counterpart to
> [`completed/sdk-chain-exactness-plan.md`](sdk-chain-exactness-plan.md)
> now gates SDK resolution against real MSBuild. Two comprehensive extensions
> remain explicitly out of scope for a later plan (see below).

A differential oracle for `crates/lsp/src/sdk_discovery.rs`'s resolution
*decision* (`SdkDiscovery::resolve` → `SdkResolution`/`SdkResolveError`), which
prior tests only covered synthetically. It carries two contracts:

- **Exactness (Surface A):** when we commit `Single(props)`, MSBuild's resolved
  `Sdk.props` must equal it; when we commit `NotFound`/`VersionNotSatisfied`,
  MSBuild must also fail. Declines (`UnsupportedLayout`) make no claim.
- **Decline soundness (Surface B):** we must decline *whenever* MSBuild's
  resolution depends on the unmodelled `MSBuildSDKsPath` override (a one-sided
  `depends ⟹ decline`; `SdkDiscovery::resolve` deliberately over-declines).

Surface A reads MSBuild through the resident `tools/msbuild-condition-oracle`
`project` op via a `$(MSBuildThisFileFullPath)` marker property on a synthetic
NuGet-pinned SDK (offline GPF layout); `MSBuildSDKsPath` is not honoured
in-process, so Surface B reads ground truth from a `dotnet msbuild` subprocess
per probe.

## Landed stages (one line each)

- **Stage 1** (PR #943) — plan + harness + fixtures: `SdkOracle` (forces
  `NUGET_PACKAGES` past the devshell pin), `write_nuget_sdk` offline GPF
  fixtures, smoke case (`sdk_resolution_oracle.rs`).
- **Stage 2** (PR #944) — Surface A committing-resolution exactness sweep
  (proptest + fixed-seed companion) with anti-vacuity floor
  (`sdk_resolution_exactness_diff.rs`).
- **Stage 3** (PR #945) — Surface B `MSBuildSDKsPath`-override decline
  classification (named scenarios with pinned `expected_depends`, two-probe
  `msbuild_depends`, workload-locator witness, anti-vacuity;
  `sdk_resolution_override_classification.rs`), plus the production fix it
  surfaced: `SdkDiscovery::resolve` drops its workload-locator exemption and
  declines every name under the override.

## Out of scope (comprehensive extensions, for a later plan)

- **Host-SDK roll-forward.** `select_sdk_version`'s full `RollForward` cascade
  for `Microsoft.NET.Sdk` selected via `global.json` `sdk.version` — the in-box
  path, which cannot be synthetically redirected (verified). Needs a
  subprocess-per-case diff against the real multi-version devshell install
  (`dotnet --info` `Base Path:`), in the `fsproj_environment_diff.rs` mould.
- **Workload-locator multi-root.** `SdkResolution::Roots` cannot be read back
  through the marker-property trick; it needs a dedicated `resolve-sdk` F# op
  reading MSBuild's `SdkResult` (`Path` + `AdditionalPaths` + success), diffed
  against `resolve_workload_locator`.
