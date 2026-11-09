# In-house NuGet restore plan (warm-cache assets graph)

> **Status:** the whole `crates/nuget` crate has landed — the
> version/range/framework models, cache + `.nuspec` reading, the offline
> resolver, and compile-asset selection (slices 1–3, 5–7) — together with the
> msbuild-side direct `PackageReference`/`FrameworkReference` capture (4a) and
> inline Central Package Management (4b), and the 4b remainder
> ([`sdk-chain-exactness-plan.md`](completed/sdk-chain-exactness-plan.md)).
> The assets-absent case is served by running the real `dotnet restore`
> ([`restore.rs`](../crates/lsp/src/restore.rs)). **Slice 8** (wiring the
> resolver into the LSP) is superseded by
> [`nuget-dgspec-boundary-plan.md`](nuget-dgspec-boundary-plan.md), which
> splits restore at the dependency-graph spec and feeds the resolver real
> restore inputs.
>
> Motivation: the LSP is blind until `dotnet restore` has written
> `obj/project.assets.json` ([`project_assets/`](../crates/lsp/src/project_assets/)
> reads it; [`fsproj_diagnostics.rs`](../crates/lsp/src/fsproj_diagnostics.rs)
> warns when it's absent). That's the right *fallback*, but the common painful
> case is a project whose packages are already **on disk** in the global
> packages folder (restored once by any other project, or by CI) while *this*
> project's assets file is missing or stale. This plan builds enough of a NuGet
> restore, in Rust, to compute the assets graph ourselves in that warm-cache
> case.

## Scope

- **In scope (tier 1):** offline resolution against the global packages
  folder (`$NUGET_PACKAGES`, default `~/.nuget/packages`): direct-dependency
  gathering from the msbuild evaluation, NuGet version/range semantics, TFM
  compatibility, the dependency-graph walk, and compile-asset selection.
- **Out of scope (tier 2+, not currently planned):** any HTTP — feed
  protocol, `NuGet.config` source lists, credential providers, package
  signing; RID/runtime asset selection; `contentFiles`/`build` assets;
  satellite packages. The cold-cache path stays `dotnet restore`.
- **No NuGet sidecar in production.** The .NET oracle tool
  (`tools/nuget-oracle`, linking the real `NuGet.Versioning` /
  `NuGet.Frameworks` / `NuGet.Packaging`) exists **for differential tests
  only**, following the `tools/fcs-dump` batch pattern. A deliberately-deferred
  future exception: if the cache turns out cold, the friendly move may be to
  spin up a real restore *for* the user — a separate, explicitly-gated feature.

## The correctness policy: resolve identically or degrade

The house rule ("under-resolve, never wrong") applied to restore:

- The resolver must produce **exactly** the package closure `dotnet restore`
  would produce, or **decline** and leave today's behaviour (empty
  `AssemblyEnv` + the `dotnet restore` diagnostic). It must never resolve to a
  *different* closure.
- Consequences:
  - A non-floating version range resolves as restore's lookup does with the
    packages folder as its only source: to the **lowest installed version
    satisfying it** (NuGet is a minimal resolver), which is the lower bound
    when that is installed. Whether a configured source could answer
    differently is checked at the dgspec boundary
    ([`check_sources`](../crates/nuget/src/dgspec/resolve.rs)), which
    declines on a source holding the package or one restore cannot read.
  - **Floating versions** (`1.*`) resolve against feed state we can't see
    offline → always degrade.
  - Any msbuild input we couldn't evaluate confidently (an `Unsupported`
    condition outcome on a `PackageReference`-bearing construct, an unresolved
    import) → degrade.
  - An existing fresh `obj/project.assets.json` always wins; the in-house
    resolver is the fallback layer beneath it, never a replacement for real
    restore output.
- **We never write `obj/project.assets.json`.** That file is NuGet's;
  msbuild's `ResolvePackageAssets` reads far more of it than we model, and a
  partial file would poison real builds and NuGet's no-op check. The resolver
  produces the in-memory `ResolvedAssemblies` equivalent directly
  ([`project_assets/mod.rs`](../crates/lsp/src/project_assets/mod.rs) already
  only consumes compile-asset paths, package folders, and framework-reference
  names).

The crate is self-contained (`borzoi-nuget`, depends only on `roxmltree` and
`serde`) and never reads the process environment: the LSP/host shell owns SDK
discovery and hands the crate explicit roots, typed inputs and, where NuGet
reads it, the environment as a map. `borzoi_nuget::settings` reads the
`NuGet.Config` hierarchy that way to find the global packages folder NuGet
would use for a project (`$NUGET_PACKAGES`, else the configured
`globalPackagesFolder`, else `~/.nuget/packages`), which the on-demand restore
pins its package folder to, and which SDK discovery resolves versioned MSBuild
SDKs (`Sdk="Name/Version"`, `msbuild-sdks`) from, as NuGet's SDK resolver does.

## Landed slices (one line each)

- **Slice 1** (#720) — `NuGetVersion` (SemVer 2.0.0 plus NuGet's deviations),
  `tools/nuget-oracle`, the batch + differential + property harnesses, CI
  gating (`nuget` filter, `test-nuget` job).
- **Slice 2** (#723) — `VersionRange`: bracket/float parse and `satisfies`
  against the float's resolved base min; float *selection* deferred to the
  resolver. Fresh-seed `soak` differential (`tests/all/soak.rs`) landed alongside.
- **Slice 3** — `NuGetFramework`: TFM parse (short + long), compatibility, and
  nearest-match. Over the canonical `GetShortFolderName` spelling, compatibility
  is *exact in both directions* for live frameworks (`.NETFramework`,
  `.NETCoreApp` incl. net5.0+ platform TFMs, `.NETStandard`); dead frameworks
  are unmodelled and never resolved against. `framework_diff.rs` +
  `framework_exhaustive.rs`.
- **Slice 4a** — msbuild direct `PackageReference`/`FrameworkReference` capture
  (no CPM): own `ParsedProject` fields (`package_references`,
  `framework_references`, `package_references_uncertain`), a `package_context`
  flag mirroring `compile_context`, raw `$(…)`-expanded version/asset metadata.
  Followed-SDK implicit items ride the same path; unresolved SDKs stay uncertain.
- **Slice 4b (inline CPM)** — `PackageVersion`/`GlobalPackageReference` as
  first-class outputs, `PackageReference Update` folding onto the `Include` it
  modifies (three-state `MetadataValue`), and inline CPM version selection
  (`VersionOverride` > central `PackageVersion` > local `Version`) within a
  bounded envelope. `Directory.Packages.props` is genuinely followed through the
  real `NuGet.props` import point (toolset properties seeded at canonical-layout
  SDK resolution). The SDK-chain remainder is still open — see below.
- **Slice 5a** — global-packages addressing (`{id-lower}/{version-lower}`),
  `.nupkg.metadata` installed-marker check, `.nuspec` dependency groups per TFM.
- **Slice 5b** — resolver-facing dependency-group selection
  (`FrameworkReducer.GetNearest`, order-sensitive duplicate handling).
- **Slice 5c** — committed-version enumeration for one package id (canonical
  entries only, strict NuGet version identity, real IO failures surfaced so the
  resolver can decline).
- **Slice 6a** — first conservative `resolve_offline` (explicit cache root,
  non-floating inclusive-lower-bound only, transitive nuspec walk; declines on
  everything uncertain), plus the end-to-end `resolve` oracle op driving the
  genuine restore engine (`RemoteDependencyWalker` + `GraphOperations.Analyze`).
- **Slice 6b** — the resolver's real semantics: nearest-wins eclipsing, cousins
  merge upward, decline on downgrade (NU1605) / conflict (NU1107) / cycle
  (NU1108). Multi-version differential (900 graphs, zero false declines) plus a
  rejected-version-*presence* (never contents) property.
- **Slice 6c** — both restore engines. The oracle's `restore` op runs a real
  restore through `RestoreRunner`, under the legacy resolver and under the
  .NET 10 SDK's default `DependencyGraphResolver` (calibrated against real
  `dotnet restore` on 80 generated graphs, 40 of them ones the engines
  disagree on). The contract is the default engine's, the engine a
  dependency-graph spec restores under, and the resolver is that engine
  reimplemented pass for pass (`resolver.rs`: breadth-first choosing with
  evictions and restarts, then the flattening that reports cycles, conflicts
  and downgrades), with the packages folder as restore's only source. Every
  closure and every decline naming a restore failure is held to it; the
  legacy engine's outcome is recorded, and pinned where the engines part
  (`docs/nuget-dgspec-boundary-plan.md`, *The resolver is the default
  engine*).
- **Slice 7** — compile-asset selection (`crates/nuget/src/assets.rs`):
  `ManagedCodeConventions` restricted to the compile patterns + the nuspec
  `<references>` allow-list. Ref-beats-lib at *group* level, `lib/any` ≠
  `lib/Any`, decline (`AmbiguousAssetGroup`) on order-dependent ties. Layered
  tests: `compile_assets_diff.rs` (reversed-file-list tie check),
  `compile_assets_restore.rs` (real offline `dotnet restore`),
  `compile_assets_properties.rs`.

## Still to do

Slice 8 (LSP integration) is superseded by
[`nuget-dgspec-boundary-plan.md`](nuget-dgspec-boundary-plan.md): the resolver
consumes a real dependency-graph spec rather than direct requirements from the
msbuild evaluation, and the runtime path waits on a spawn-free producer of that
spec. Until then the assets-absent case is served by
[`restore.rs`](../crates/lsp/src/restore.rs), and the LSP crate does not depend
on `borzoi-nuget`.

Optional later: `packages.lock.json` fast path (trust the lock, skip resolution,
go straight to asset selection). `restore.rs` declines a lock-file project today.
