# Multi-TFM csproj resolution plan

> **Status: implemented.** All five phases have landed:
> - Phase 1 (`framework` field + `Reference::ProjectRef::tfm`) — PR #109.
> - Phase 2a (`transitive_project_tfms` helper) — PR #114.
> - Phase 2b (platform-suffix recovery) — PR #117.
> - Phase 3 (protocol `projectTfms` field; `PROTOCOL_VERSION` bumped to
>   `0.4.0`) — PR #124.
> - Phase 4 (sidecar per-project workspace + PE-reference cascade) —
>   PR #140.
> - Phase 5 (F# top routes C# subtree through the closure walker) —
>   PR #144.
>
> Fixture coverage: `tools/csharp-sidecar/test-fixtures/multi-tfm/`,
> `multi-tfm-cond/`, and `multi-tfm-cond-fsharp/` exercise the leaf,
> conditional, and F#-rooted closures respectively. Differential test
> against `dotnet build`'s ref-assembly surface lives in
> `crates/lsp/tests/csharp_sidecar.rs`.

Design doc for honouring per-csproj TFM selection when an F# consumer references
a multi-targeted C# project. Captures decisions made before implementation
started so future work can resume from a cold pickup.

## Context

The LSP eventually picks a single TFM for the F# project it's serving (policy
sits on top of [`target_frameworks`](../crates/msbuild/src/target_frameworks.rs)
and `project.assets.json`'s TFM list). Today, that single TFM is also passed to
the C# sidecar as `BuildMetadataParams::target_framework` and applied as an
MSBuild *global* via `MSBuildWorkspace.Properties["TargetFramework"]`. The
sidecar then transitively emits every csproj in the closure under that one
global.

That breaks the very common shape:

- F# consumer declares `<TargetFramework>net10.0</TargetFramework>`.
- F# consumer has a `<ProjectReference>` to a C# library that declares
  `<TargetFrameworks>netstandard2.0;net6.0</TargetFrameworks>`.

MSBuild's inner-build dispatch sees `TargetFramework=net10.0` set globally,
notices it isn't in the leaf's `TargetFrameworks` list, and errors. The
sidecar surfaces this as the `TfmMismatch` failure mode documented in
[`csharp-sidecar-plan.md`](csharp-sidecar-plan.md) ("Out of scope").

The right answer for the consumer is whichever producer TFM NuGet's restore
already picked (probably `netstandard2.0` here, depending on the compatibility
ranking). That choice is recorded per-ProjectReference in the consumer's
`project.assets.json`. We ride on it instead of re-implementing NuGet's
compatibility algorithm in either the F# crate or the sidecar.

Existing plans this dovetails with:

- [`project-assets-plan.md`](project-assets-plan.md) — already parses
  `project.assets.json` and emits a `Reference::ProjectRef` for each
  `<ProjectReference>`. Extended here to carry the producer TFM.
- [`csharp-sidecar-plan.md`](csharp-sidecar-plan.md) — sidecar protocol +
  emit pipeline. Extended here with a per-project TFM map and matching
  emit-time handling.
- [`fsproj-parser-plan.md`](fsproj-parser-plan.md) — D9 (target framework
  enumeration). Selection policy on top of D9 is the LSP-layer concern that
  drives the consumer TFM into this plan's pipeline.

## Scope

- **Input.** A consumer fsproj that has been restored, plus the consumer TFM
  the LSP has selected.
- **Output.** Each csproj in the consumer's transitive `<ProjectReference>`
  closure is built by the sidecar for the producer TFM NuGet selected — not
  for the consumer's TFM.
- **Failure mode.** If restore is stale (closure node not in
  `targets[<consumer-tfm>]`, or no `framework` field), surface
  `ProjectRefUnresolved` rather than guessing or falling back to the
  consumer's TFM.
- **Out of scope.** Re-implementing NuGet's compatibility algorithm. C#
  consumers (the LSP is for F#). Multiple consumer TFMs in one session.
  Closures rooted at a csproj rather than an fsproj.

## Settled decisions

### D1. Data source: `targets[<consumer-tfm>][<lib-key>].framework`

`project.assets.json` writes, for each consumer TFM:

```json
"targets": {
  "net10.0": {
    "LeafFixture/1.0.0": {
      "type": "project",
      "framework": ".NETStandard,Version=v2.0",
      "compile": { "bin/placeholder/LeafFixture.dll": {} }
    }
  }
}
```

The `framework` field on a `project`-kind library is the **long-form moniker
of the producer TFM** NuGet selected. Short-form conversion
(`.NETStandard,Version=v2.0` → `netstandard2.0`) happens at the
`project_assets` boundary so the wire and sidecar layers see only short
monikers — matching the existing `Reference::Framework { tfm }` convention.

**Why this field, not the compile-path TFM segment.** Some producers
emit under a different folder name than the canonical short moniker (e.g.
when `<OutDir>` is overridden, or when the TFM has a platform suffix like
`net8.0-android`). The `framework` field is the authoritative restore-time
choice; the compile path is incidental.

**Verification.** Phase 1 synthesises a multi-TFM fixture and asserts the
`framework` field is exactly what we expect on a real `dotnet restore`
output. If the field is ever absent on a project-kind library — the
NuGet schema has carried it for years, but defensively — emit
`ProjectRefUnresolved` rather than silently fall through.

**Phase 1 limitation: platform-qualified TFMs.** Empirically verified on
`dotnet restore`: when the consumer's TFM has a platform suffix
(`net8.0-windows`, `net8.0-android`, etc.), NuGet writes only the base
framework moniker into the project library's `framework` field — for a
`net8.0-windows` ⇒ `net8.0-windows` reference the field reads
`.NETCoreApp,Version=v8.0` and round-trips here as `net8.0`. The
platform suffix is not in the consumer's assets file. Recovering it
requires reading the producer's own `project.frameworks` (which the
resolver already visits transitively) and matching the consumer's
selected framework against the producer's declared TFMs — strictly a
Phase 3 concern, since Phase 1 consumers don't dispatch on `tfm` yet.
The `platform_qualified_consumer_records_base_producer_tfm` test pins
the current behaviour so the cross-reference step has a clear locus.

### D2. Per-project TFM map in the protocol

`BuildMetadataParams` gains:

```rust
pub project_tfms: HashMap<String, String>,  // absolute csproj path → short TFM
```

The top csproj's TFM stays in `target_framework` (the existing field) for
clarity at the call site, but the map covers the entire closure including
the top — so the sidecar can look up every project in one place. Duplicating
the top's TFM in the map is cheap and removes a special case.

**Why a map keyed by csproj path rather than a list of `(path, tfm)`
tuples.** The sidecar already does a topological walk over the closure
keyed by csproj path (`emitted` dictionary in `EmitClosure`); a map matches
that shape and makes the per-project lookup O(1) without re-keying.

**Wire layout.** Plain JSON object: `{ "/abs/path/A.csproj": "net8.0", … }`.
Absolute paths — no normalisation policy beyond what the LSP already
applies when discovering ProjectReferences. The sidecar matches by
ordinal string compare, same as the existing closure dictionary.

### D3. Per-project TFM application in the sidecar

The current sidecar sets `Properties["TargetFramework"] = X` once at
workspace construction. That mechanism is fundamentally workspace-global —
`MSBuildWorkspace.Properties` is immutable post-construction. Two paths
to honour per-project TFMs:

1. **One workspace per project.** Construct N workspaces (one per closure
   node), each with its own `TargetFramework` global. Roslyn solutions
   can't cross workspace boundaries directly, so `CompilationReference`
   cascading needs reconstruction — emit each leaf's metadata DLL to a
   temp file, then load it as a `PortableExecutableReference` for
   downstream projects in the same closure walk.
2. **Single workspace, post-load Solution mutation.** Open the closure
   in one workspace with the top's TFM as the global, then walk
   `solution.Projects` and rewrite each project's MSBuild-derived
   properties to match its assigned TFM. Roslyn's `Project` exposes
   `CompilationOptions` and `ParseOptions` mutation but not arbitrary
   MSBuild property mutation — the source list, defines, and ref-asm set
   would not change to match the new TFM, so this is a non-starter for
   any leaf where the TFM affects sources / `DefineConstants` /
   transitive references.

**Decision: option 1, with the PE-reference cascade.** It's slower (N
workspace evaluations on a cold cache; cache hits remain workspace-free)
and the cascade is more code, but it's the only option that gives each
csproj its own complete MSBuild evaluation — sources, defines, references,
all evaluated under the correct TFM. The existing single-workspace
deterministic-cascade story already required forcing `Deterministic=true`
on every project; reading each leaf's emitted DLL as a
`PortableExecutableReference` for downstream projects preserves the same
content-addressed determinism property, since the DLL bytes participate
in Roslyn's `GetDeterministicKey`.

**Cache key.** Each per-project key already incorporates that project's
own properties (including `TargetFramework`) via Roslyn's
`GetDeterministicKey`, so a TFM swap naturally invalidates. No
explicit protocol-level cache key changes needed.

### D4. LSP-side resolver

New top-level helper in `crates/lsp/src/project_assets/`:

```rust
pub fn transitive_project_tfms(
    top_assets: &RawAssets,
    consumer_tfm: &str,
) -> Result<HashMap<PathBuf, String>, ProjectAssetsError>;
```

Walks `targets[consumer_tfm]`, picks out every project-kind entry, joins
the `libraries[<key>].msbuildProject` (or `.path`) against the
consumer's project directory, normalises the `framework` field to short
form, and collects. Includes the top csproj itself (under the consumer's
TFM) so the resulting map is self-contained.

**Platform-suffix recovery.** Phase 1 records the *base* TFM only (e.g.
`net8.0` even when the consumer was on `net8.0-windows`), because the
consumer's assets file genuinely doesn't carry the producer's platform
suffix. This resolver lifts the limitation: for each project-kind entry,
read the producer's own `project.frameworks` (already on the resolver's
walked list — Phase 1's recursive shell visits it transitively) and
match the base framework. If exactly one declared TFM has matching base
framework and matching version, that's the producer's full TFM; if
multiple match (e.g. `net8.0` and `net8.0-windows` both declared), pick
the platform-qualified one whose suffix matches the consumer's. If none
match, surface `RestoreMismatch`.

`Reference::ProjectRef` also grows a `tfm: String` field, populated from
the same source. Existing callers of `enumerate_one` (the recursion shell
in `project_assets::mod.rs`) get the per-project TFM for free without
having to walk `targets` separately.

### D5. Failure-mode taxonomy

New variants on the existing error types:

| Trigger | Error | Surfaced by |
|---|---|---|
| Closure node not in `targets[<consumer-tfm>]` | `ProjectRefUnresolved { csproj_path }` | LSP, during resolver walk |
| `framework` field absent on a project-kind library | `ProjectRefUnresolved { csproj_path }` (same — same recovery) | LSP, during resolver walk |
| Consumer's chosen TFM not in `assets.targets` | `RestoreMismatch { requested_tfm, available }` | LSP, before calling sidecar |
| Sidecar receives a `project_tfms` map missing a closure node | `MissingProjectTfm { csproj_path }` | sidecar, defensive |

All four mean "restore is stale or the LSP's TFM selection disagrees with
restore." None falls back silently — the user added a ProjectReference or
changed the consumer TFM, and the answer is "re-run `dotnet restore`," not
"guess."

### D6. Cross-TFM CompilationReference cascading

A consumer building for `net10.0` and a leaf built for `netstandard2.0` is
exactly what `dotnet build` does today; Roslyn's `CompilationReference`
(or in our per-workspace world, `PortableExecutableReference` over the
emitted ref DLL) handles the TFM jump natively. The leaf is emitted
against its own SDK ref-assembly set; the consumer reads the leaf's
emitted DLL and sees its public surface, regardless of the framework
difference. No special handling.

## Phased implementation

One PR per phase. Each phase ends with `cargo test` + `dotnet test` green
where applicable.

1. **`framework` field in raw assets + `Reference::ProjectRef::tfm`.** Pure
   parser change. Synthesises a multi-TFM fixture under
   `crates/lsp/tests/fixtures/project_assets/` and asserts the `framework`
   field round-trips into the short-form TFM. Snapshot test pins one known
   long→short conversion (`.NETStandard,Version=v2.0` → `netstandard2.0`).
2. **`transitive_project_tfms` helper.** New top-level resolver. Property
   test: every `ProjectRef` discovered by `enumerate_one` has an entry in
   the map; consumer's chosen TFM appears as exactly one `targets` key;
   the top csproj is in the map.
3. **Protocol additive change.** Add `project_tfms` to `BuildMetadataParams`
   (LSP side) and the corresponding C# DTO. The original plan called for
   a soft rollout (sidecar treats the map as optional during the transition
   so a stale sidecar binary keeps working with a fresh LSP and vice versa).
   The repo's sidecar is bundled with the LSP via `build.rs`, so in practice
   the two are always rebuilt together; we instead bump `PROTOCOL_VERSION`
   from `0.3.0` to `0.4.0` so a stale binary fails the handshake loudly
   rather than silently emitting under the consumer's TFM. The C# DTO
   accepts an absent `projectTfms` (deserialises to `null`) only so the
   version-mismatch path remains the diagnostic surface, not a JSON
   deserialisation error.
4. **Sidecar consumes the map.** Per-project workspace construction +
   PE-reference cascade between closure nodes. New
   `tools/csharp-sidecar/test-fixtures/multi-tfm/` fixture (top csproj
   targeting `net10.0`, leaf targeting `netstandard2.0;net6.0`). Three
   tests: xUnit unit test exercising the picker in isolation, Rust
   integration test (`crates/lsp/tests/csharp_sidecar.rs`) driving the
   end-to-end path, differential test against `dotnet build` confirming
   the emitted leaf DLL's surface matches the `netstandard2.0` build.
5. **End-to-end on a real fsproj.** Pick (or synthesise) an F# project
   that references a multi-TFM csproj. Confirm the metadata DLL the
   sidecar emits for the leaf is built for the producer TFM, and the
   consumer's emit successfully references it.

## Out of scope (deliberate)

- **NuGet's compatibility algorithm in the sidecar or the F# crate.** We
  ride on restore. If the user wants a different leaf TFM than restore
  picked, they edit `<ProjectReference>` (or the leaf's
  `<TargetFrameworks>`) and re-restore.
- **C# consumers.** This is an F# LSP. A pure-C# closure can still use
  the existing single-TFM path; nothing in this plan removes it.
- **Multiple consumer TFMs in one session.** The LSP picks one TFM at a
  time. If we eventually want hover-over-each-target-framework, that's
  N independent invocations of this pipeline.
- **Cross-workspace `CompilationReference` sharing.** Each per-project
  workspace stands alone; we cascade via emitted PE bytes, not in-memory
  Roslyn references. Means more re-evaluation on cold cache, but the
  cache amortises and the determinism story is simpler.

## Files to add

- `crates/lsp/tests/fixtures/project_assets/multi_tfm_proj_ref.json`
- `tools/csharp-sidecar/test-fixtures/multi-tfm/{top,leaf}/` (csprojs +
  `obj/project.assets.json` fixtures).
- Test files alongside existing ones in
  `tools/csharp-sidecar.tests/` and `crates/lsp/tests/`.

## Files to modify

- `crates/lsp/src/project_assets/{raw,enumerate,mod}.rs` — `framework`
  field on raw entries, `tfm` on `Reference::ProjectRef`,
  `transitive_project_tfms` helper.
- `crates/lsp/src/project_assets/error.rs` — new
  `ProjectRefUnresolved` / `RestoreMismatch` variants.
- `crates/lsp/src/csharp_sidecar/protocol.rs` — `project_tfms` field on
  `BuildMetadataParams`.
- `crates/lsp/src/csharp_sidecar/process.rs` — thread the map through.
- `tools/csharp-sidecar/Protocol.cs` — matching C# DTO.
- `tools/csharp-sidecar/BuildService.cs` — per-project workspace
  construction + PE-reference cascade.

## Risks

- **`MSBuildWorkspace` per-project overhead.** Cold-cache, N closure
  nodes ⇒ N workspace constructions. Each is a fresh MSBuild evaluation;
  in practice ~hundreds of ms each. Cache hits skip workspace
  construction entirely, so this hits steady-state only on the first
  invocation after a closure change. Acceptable if the cache hit rate
  stays high; revisit if telemetry shows pathological cold-call latency.
- **`framework` field semantics on TFM aliases.** Phase 1 fixture
  verification on a real `dotnet restore` output pins what we actually
  receive (`.NETStandard,Version=v2.0` vs `netstandard2.0` vs anything
  exotic). If the long→short conversion has edge cases we hit, the
  conversion logic centralises in one place.
- **PE-reference determinism.** Roslyn's `GetDeterministicKey` over a
  `PortableExecutableReference` reads the file's MVID, which is content-
  addressed when the producer used `Deterministic=true`. We already
  force `Deterministic=true` on every project in the closure, so this
  property carries over to the per-workspace world. Phase 4 explicitly
  asserts byte-identical re-emit across two cold invocations.
