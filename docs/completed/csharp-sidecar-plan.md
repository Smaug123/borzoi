# C# project-reference sidecar plan

> **Status: implemented.** All eight phases below have landed: scaffold
> + JSON-RPC protocol, `MSBuildWorkspace` load, metadata-only emit,
> content-addressed cache, transitive `<ProjectReference>` walk, source
> generators + analyzers + IVT coverage, diagnostics polish, and
> `build.rs`-driven distribution. Phase 5's closure walker was later
> extended (PR #144) to route F#-rooted closures through the same
> machinery on behalf of
> [`multi-tfm-resolution-plan.md`](multi-tfm-resolution-plan.md);
> phase 4's per-project workspace handling was the multi-TFM
> implementation, replacing the originally-planned single-workspace
> emit.
>
> The retired `invalidate` JSON-RPC method (originally phase 6) remains
> deliberately absent — D9 records why the cache-key design made it
> unnecessary.

Design doc for a managed (.NET) sidecar process that, given a `.csproj` on
disk, produces an up-to-date metadata-only assembly the Rust LSP can hand to
the assembly reader. Captures decisions made before implementation started so
future work can resume from a cold pickup.

## Context

The LSP needs to surface symbols from C# `<ProjectReference>` targets of F#
projects. The end state we want, from Rust's perspective: for every C# project
referenced (directly or transitively) by an F# project the user has open,
there is a `.dll` on disk we can hand to the assembly reader, and that `.dll`
reflects the current state of the C# sources.

Roslyn is the only practical way to produce that DLL:

- It owns the language (C# parser, semantic model, code generation).
- It exposes `Compilation.EmitMetadataOnly`, which produces a metadata-only
  PE without compiling method bodies — exactly what we want.
- It honours source generators, analyzers, `<Nullable>`, `<LangVersion>`,
  implicit usings, IVT, and every other csproj knob that affects the emitted
  surface. Reimplementing any of that in Rust is a non-starter.

Roslyn is C# (managed). Rust can't link it. The standard answer is a sidecar
process talking JSON-RPC over stdio — same shape as the LSP itself.

Existing plans this dovetails with:

- The `borzoi-assembly` reader — consumes the metadata DLL the sidecar emits.
  "Assume we can already read ref assembly metadata" — that's this reader.
- `fsproj-parser-plan.md` — the Rust side that discovers
  `<ProjectReference Include="…/Foo.csproj"/>` in an fsproj and asks the
  sidecar for `Foo.dll`.
- `project-assets-plan.md` — owns the F# side's `project.assets.json`
  reading. The sidecar owns the C# side's; the two never share code.

## Scope

- **Input.** A path to a `.csproj`, plus configuration/TFM properties the
  Rust LSP wants to pin (typically inherited from the referencing fsproj).
- **Output.** A path to a metadata-only `.dll` on disk, plus a content hash
  (so callers can cheaply detect "same as last time"), plus structured
  diagnostics from the C# compilation.
- **Sidecar responsibility.** Load the csproj via Roslyn's
  `MSBuildWorkspace`, which evaluates MSBuild (including SDK targets,
  `project.assets.json` resolution, and source-generator wiring), then drive
  `CSharpCompilation.Emit(…, options.WithEmitMetadataOnly(true))`.
- **Rust responsibility.** Spawn and supervise the sidecar; dispatch
  build-metadata requests; hand the resulting DLL path to the assembly
  reader.
- **Out of scope for v1.** Running `dotnet restore` from the sidecar
  (caller must have restored). Watching files (Rust pushes invalidations).
  Razor / XAML / `.resx` codegen beyond what Roslyn's standard pipeline
  handles. Concurrent builds within one sidecar (requests serialise).
- **References.**
  - Roslyn — `Microsoft.CodeAnalysis.CSharp`,
    `Microsoft.CodeAnalysis.Workspaces.MSBuild`.
  - `EmitOptions.MetadataOnly` / `IncludePrivateMembers` —
    `dotnet/roslyn/src/Compilers/Core/Portable/Emit/EmitOptions.cs`.
  - rust-analyzer's proc-macro server — same architectural shape (sidecar
    over stdio for code its host runtime can't load).
  - Existing fcs-dump tool — convention for the in-repo managed-tool layout.

## Settled decisions

### D1. Sidecar over in-process embedding

The sidecar runs as a separate process spawned by the Rust LSP, talking JSON
over stdio. Embedding CoreCLR in the Rust LSP via hostfxr is technically
possible but adds a large surface area (interop ABI, GC pinning, marshalling
managed exceptions) for no payoff over a subprocess.

Rationale (gospel P1): a subprocess boundary is a clean
imperative-shell/functional-core split. The sidecar is the shell that owns
Roslyn's stateful workspace; the Rust LSP treats it as an opaque "give me a
metadata DLL for this csproj" service.

Alternatives rejected:

- **`dotnet build -t:ReferenceAssembly`.** Works without a sidecar but pays
  cold-MSBuild start (~1–3s) per call and has to re-derive output paths from
  MSBuild's evaluation. Loses incremental Roslyn state between calls. For a
  long-lived LSP session with dozens of C# project refs, this dominates
  user-visible latency.
- **`csc.exe /refonly`.** Same per-call cost, plus we'd reimplement
  reference resolution / SG wiring outside Roslyn.
- **Hosting Roslyn via hostfxr in-process.** All the cost of a sidecar
  (managed runtime startup, P/Invoke layer) with none of the isolation
  benefits.

### D2. Sidecar language: C#

The sidecar is written in C#, not F#.

`fcs-dump` is F# because it wraps an F#-native library (FCS). Roslyn is
C#-native: every sample, every API doc, every Stack Overflow answer about
`MSBuildWorkspace` / `CSharpCompilation` / source generators is in C#. The
sidecar is small (parse arg, drive Roslyn, write JSON) and most of its body
is Roslyn calls; matching the Roslyn idiom matters more than matching the
fcs-dump idiom.

Project layout:

```
tools/csharp-sidecar/
  csharp-sidecar.csproj
  Program.cs              # entry, stdin/stdout JSON-RPC loop
  Protocol.cs             # request/response DTOs (System.Text.Json)
  BuildService.cs         # MSBuildWorkspace lifecycle, build orchestration
  Cache.cs                # cache-key derivation, on-disk lookup/write
  Diagnostics.cs          # Roslyn Diagnostic → wire-format mapping
```

Targets `net10.0`, matches the host SDK fcs-dump uses.

### D3. Protocol: JSON-RPC over stdio, length-prefixed

Wire format: `Content-Length: <n>\r\n\r\n<json>` frames — same encoding the
LSP itself uses. The Rust side already has `lsp-server` in the dep graph;
the same framing layer is reused.

Methods:

```jsonc
// req
{ "method": "initialize",
  "params": { "workspaceRoot": "/abs/path", "dotnetRoot": "/abs/path" } }
// res
{ "result": { "sdkVersion": "10.0.100", "roslynVersion": "5.0.0" } }

// req
{ "method": "buildMetadata",
  "params": {
    "csprojPath": "/abs/foo.csproj",
    "configuration": "Debug",
    "targetFramework": "net10.0",
    "extraProperties": { … }
  } }
// res
{ "result": {
    "metadataDllPath": "/abs/.cache/csharp-sidecar/<hash>.dll",
    "contentHash": "sha256:…",
    "fromCache": true,
    "diagnostics": [ { … } ],
    "transitiveProjectRefs": [
      { "csprojPath": "…", "metadataDllPath": "…" }
    ]
  } }

// req
{ "method": "shutdown" }
// res
{ "result": null }
```

Requests serialise on the sidecar side (one request in flight at a time).
Roslyn workspaces are not safe for concurrent project loads; queueing is the
sidecar's job, not the caller's.

`buildMetadata` returns the metadata DLLs for the requested csproj **and**
every transitive `<ProjectReference>` it built along the way — the caller
will need all of them anyway, and forcing it to re-request each one wastes
round-trips.

Errors are returned as JSON-RPC error responses with a structured `data`
field carrying the kind. No silent fallbacks.

### D4. csproj evaluation: `MSBuildWorkspace`, not a hand-rolled parser

The sidecar uses `MSBuildWorkspace.OpenProjectAsync` to load each csproj.
That delegates to the real MSBuild engine via `Microsoft.Build.Locator`,
which evaluates the SDK targets, reads `project.assets.json`, resolves
`PackageReference` / `ProjectReference` / `FrameworkReference`, surfaces
source generators, applies `<Nullable>` / `<ImplicitUsings>` / `<LangVersion>`,
and hands Roslyn a fully-realised `Project` with the right
`CompilationOptions` and `ParseOptions`.

Rationale (gospel P3): we do not want to reimplement a meaningful subset of
MSBuild + the .NET SDK targets. The fsproj parser gets away with a tiny
subset because F# fsproj files are tiny and our needs (compile-file order)
are tiny. C# csprojs we're asked to bind to are arbitrary user code; we
need everything the SDK provides. `MSBuildWorkspace` is the supported entry
point.

Cost: requires `dotnet` SDK on PATH and a successful `Microsoft.Build.Locator.RegisterDefaults()`
at process start. If either fails, the sidecar exits non-zero with a
structured error; the Rust LSP surfaces "C# project references unavailable;
install .NET SDK" as a workspace diagnostic and continues.

Alternatives rejected:

- **Hand-roll csproj parsing in the sidecar.** Same speculative-generality
  trap as fsproj would be if we ever pushed it past trivial files. C# csprojs
  use the full SDK; we'd be reimplementing it.
- **Use a fixed csproj template and inject sources.** Doesn't survive
  arbitrary user csprojs (analyzers, multi-target, custom targets).

### D5. Emit: `EmitMetadataOnly = true`, `IncludePrivateMembers = true`

```csharp
var options = new EmitOptions(
    metadataOnly: true,
    includePrivateMembers: true);
compilation.Emit(peStream, options: options);
```

- `metadataOnly`: produces a PE with type and member tables but no method
  IL. Faster than full emit; sufficient for consumers (us) who only need
  the public/internal API surface.
- `includePrivateMembers: true`: also emits private and internal members.
  We **want** internals because of `[InternalsVisibleTo]` — an F# consumer
  marked as an IVT friend of the C# project must see internals. Setting
  this to `false` produces a strict ref assembly that elides internals,
  which would silently break IVT consumers.

This is metadata-only, not a true ref assembly (`ProduceReferenceAssembly`
+ `RefOnly`). The two emit modes diverge in subtle ways (ref assemblies
throw away `private`/`internal` and rewrite some attributes); for our
purposes the metadata-only mode is strictly more permissive and therefore
safer for downstream binding. The size cost (private member rows on disk)
is negligible — these DLLs live in a cache, they're never shipped.

### D6. Up-to-date detection: content-addressed cache

The single load-bearing concern. The cache key for a project Foo is a
SHA-256 over the canonical encoding of:

1. The csproj content (bytes). MSBuild evaluates the csproj before Roslyn
   sees it; the on-disk bytes are not otherwise part of Roslyn's view, so
   we hash them ourselves.
2. **Roslyn's internal `Compilation.GetDeterministicKey` output.** This
   API is exactly the right thing: it's the same data the Roslyn team uses
   to drive its own deterministic-emit pipeline, so it covers every
   emit-affecting input the compiler has identified — every source file
   (with SHA-1 content checksums), every field of `CompilationOptions`
   and `ParseOptions`, every metadata reference's identity and MVID, the
   compiler version, the emit options, and the analyzer/source-generator
   set. We pin `CSharpCompilationOptions.Deterministic = true` before
   asking, which makes referenced compilations' MVIDs content-derived —
   so the cascade through `CompilationReference` (and therefore
   `<ProjectReference>`) is structural without us walking the closure.
3. Each `Project.AdditionalDocuments`'s path + bytes. These sit on the
   Roslyn `Project` but are not part of the `Compilation`, so the detkey
   above doesn't see them.
4. Each `Project.AnalyzerConfigDocuments`'s path + bytes. Same reason.
5. Every transitive `<ProjectReference>` target's **cache key**, sorted
   by path. Roslyn's detkey already cascades through MVIDs (D7's topo
   walk hands the referenced project's emitted DLL to MSBuild as the
   `CompilationReference`); this explicit section is a belt-and-braces
   structural Merkle proof we can expose on the wire.

Implementation lives in `Cache.cs`; the reflection wrapper that calls
the internal `GetDeterministicKey` API lives in
`RoslynDeterministicKey.cs`. The wrapper probes at `initialize` time and
returns an `IncompatibleRoslyn` error if the API shape has changed
under a Roslyn upgrade — better to fail the handshake loudly than to
silently produce wrong keys.

The output DLL lives at
`<cache_root>/csharp-sidecar/<sha256-prefix>/<sha256>.dll`.

`<cache_root>` defaults to `obj/borzoi/csharp-sidecar/` under the
workspace root passed in `initialize`. Inside the obj tree so it doesn't
pollute the source tree and so users' existing `.gitignore` already covers
it.

On `buildMetadata`:

1. Resolve the project; compute the cache key.
2. If the keyed file exists on disk, return its path with `fromCache: true`.
3. Otherwise drive Roslyn, write atomically (write to `<key>.dll.tmp`,
   `fsync`, rename), return with `fromCache: false`.

Atomic rename matters: a concurrent reader (the Rust assembly importer)
must never see a half-written DLL. The rename is the publish point.

Rationale (gospel P5): wrong metadata is worse than no metadata. A
content-addressed cache is the simplest scheme that makes "is this DLL
correct for the current input state?" a tautology. Anything timestamp-based
(mtime comparison, build hash files) has subtle failure modes (clock skew,
touched-but-unchanged, partial writes) that this design sidesteps.

Cache invalidation: there isn't any. Old entries are never used because the
key won't match; they accumulate until a periodic GC removes entries not
touched in the last N days. The GC is out of scope for v1; we'll add it
when the cache directory becomes problematic.

### D7. Transitive project-reference build order

The sidecar maintains an in-process graph of csproj → csproj edges, built
lazily by `MSBuildWorkspace`. When `buildMetadata(Foo)` arrives:

1. Topologically sort Foo's `<ProjectReference>` closure (leaves first).
2. For each project in topo order, compute its cache key (which requires
   its dependencies' DLLs to exist on disk and be hashable — guaranteed by
   the topo order).
3. If the keyed DLL is cached, skip; otherwise emit.

Cycles in `<ProjectReference>` are a build error in C# itself; if Roslyn /
MSBuildWorkspace flags one, the sidecar returns the error verbatim.

Build failures partway down the tree: the offending project's metadata
isn't emitted (or is emitted in degraded form — see D8). Dependents that
need it can't be built; the sidecar returns their diagnostics too. The
caller (Rust LSP) sees a structured per-project result and decides what to
surface.

### D8. Diagnostics: emit best-effort, surface everything

Roslyn distinguishes errors from warnings; some errors permit emit (the
public surface is still well-typed), some don't.

Policy:

- If `Emit().Success == true`, return the DLL path and any non-error
  diagnostics. The DLL is canonical for the current sources.
- If `Emit().Success == false`, the DLL is incomplete or absent. Return
  the diagnostics and a `BuildFailed` error. **Do not** return a stale
  cached DLL from a previous successful build — its surface won't match
  what the user currently sees in their editor and the F# checker would
  silently bind to ghosts.

Diagnostic wire format mirrors Roslyn's:

```jsonc
{ "id": "CS0103",
  "severity": "Error",
  "message": "The name 'Foo' does not exist in the current context",
  "filePath": "/abs/Bar.cs",
  "range": { "start": { "line": 12, "char": 4 }, "end": { … } } }
```

Rationale: matches what the F# LSP can already render. The Rust side
forwards C# diagnostics to the editor through the same channel it uses for
F# diagnostics on the referring fsproj — the user sees "this F# project
can't bind to Bar.cs:12 because of CS0103" rather than "Foo undefined" with
no context.

### D9. Invalidation: structural via the content-addressed cache

Originally we intended an `invalidate` JSON-RPC notification so the LSP
could push file-change events and the sidecar could mark its in-memory
`Project`s stale. D6 made that step structural instead: each
`buildMetadata` request creates a fresh `MSBuildWorkspace`, re-reads the
csproj from disk, and computes the cache key from Roslyn's
deterministic key — which itself opens every source file and includes a
content checksum. Mutating any source / csproj / AdditionalDocument
already shifts the cache key on the next call, so the next call either
serves the new content from cache (because *this* content has been
emitted before) or re-emits. No explicit invalidation step is needed.

Consequence: `invalidate` is not on the wire and there is no client
responsibility to send one. The retired phase 6 (see the
*Phased implementation* section) documents why this slot is empty. If
we later add a warm-workspace performance optimisation that retains
loaded projects across calls, this decision is revisited.

### D10. Failure modes and where each surfaces

| Failure | Where caught | What the user sees |
|---|---|---|
| Sidecar binary missing | Rust spawn | "C# project refs unavailable; csharp-sidecar not found" |
| `dotnet` SDK missing | Sidecar `initialize` | "Install .NET SDK to bind C# project refs" |
| csproj malformed | `MSBuildWorkspace` load | `LoadFailed` error with the MSBuild diagnostic |
| `project.assets.json` missing | `MSBuildWorkspace` load | `RestoreRequired` error with the csproj path |
| Source-file syntax error | Roslyn parse | `Diagnostic` in the response; emit attempted |
| Hard emit failure | Roslyn emit | `BuildFailed` error + diagnostics |
| Sidecar process crash | Rust stdin EOF | Respawn-on-next-request; one-time crash diagnostic |
| Cache directory unwritable | Sidecar emit | `CacheUnwritable` error; no fallback to in-memory |

No silent fallbacks. Every failure has a typed wire-format error.

### D11. Rust-side module layout

```
src/csharp_sidecar/
  mod.rs           # public API: SidecarHandle, build_metadata
  process.rs       # spawn / supervise / restart logic
  protocol.rs      # request/response types (serde)
  error.rs         # SidecarError enum
```

Public API:

```rust
pub struct SidecarHandle { /* … */ }

pub fn start_sidecar(
    workspace_root: &Path,
    dotnet_root: &Path,
    sidecar_binary: &Path,
) -> Result<SidecarHandle, SidecarError>;

impl SidecarHandle {
    pub fn build_metadata(
        &mut self,
        csproj: &Path,
        configuration: &str,
        tfm: &str,
    ) -> Result<BuildResult, SidecarError>;

    pub fn shutdown(self) -> Result<(), SidecarError>;
}

pub struct BuildResult {
    pub metadata_dll: PathBuf,
    pub content_hash: [u8; 32],
    pub from_cache: bool,
    pub diagnostics: Vec<CompilerDiagnostic>,
    pub transitive: Vec<(PathBuf, PathBuf)>, // (csproj, metadata dll)
}
```

Dependency rejection: caller supplies `workspace_root`, `dotnet_root`,
`sidecar_binary`. No env-var sniffing in the core. The LSP-shell layer
discovers those via the usual `$DOTNET_ROOT` / `which dotnet` dance and
passes them in.

### D12. Testing

**Sidecar-side (C# xUnit, inside `tools/csharp-sidecar/`).**

- Hand-built csproj fixtures under `tools/csharp-sidecar/test-fixtures/`:
  - `empty/Empty.csproj` — one file, one public class, no refs.
  - `pkg-ref/PkgRef.csproj` — references Newtonsoft.Json (smallest
    well-known package).
  - `proj-ref/{Lower,Upper}.csproj` — Upper references Lower; Lower has a
    public type Upper uses.
  - `nullable/Nullable.csproj` — `<Nullable>enable</Nullable>` with
    nullable annotations in the public surface.
  - `ivt/Ivt.csproj` — `[assembly: InternalsVisibleTo("Consumer")]` with
    an internal type; assert the type is present in metadata output.
  - `source-gen/SourceGen.csproj` — references a source generator;
    assert generated types appear in metadata.
- Unit tests: cache-key determinism (same inputs → same hash; one byte
  flip → different hash); atomic rename safety (write fault during emit
  doesn't leak a half-written DLL into the cache).

**Rust-side (`tests/csharp_sidecar.rs`).**

- Integration test: spawn the sidecar binary (built by a `cargo test`
  pre-step that runs `dotnet publish`), issue `buildMetadata` on the
  `empty` fixture, assert the resulting DLL is readable by the assembly
  reader and contains the expected public class. Skipped if `dotnet` not
  on PATH.
- Idempotency property: calling `build_metadata` twice with no file
  changes returns `from_cache: true` on the second call.
- Source-change propagation: call once with starting bytes; mutate
  the source file; call again — second call must re-emit with a
  different `content_hash` (per D9 this is structural; no explicit
  invalidation step).

**Differential test against `dotnet build -p:ProduceReferenceAssembly=true`**
(gated on `dotnet` being on PATH, like fsproj phase 6). For each fixture:
build via the sidecar, build via `dotnet`, read both DLLs with the
assembly reader, normalise both to `NormalisedEntity`, assert equality.
This is the canonical "are we producing the right metadata" check.

**Property: cache-key is a function of inputs.** Generate small synthetic
csprojs (1–3 sources, 0–2 package refs, 0–1 project ref), compute cache
keys for two arrangements of the same inputs (e.g. different file
iteration orders), assert keys are equal. Mutate any single input byte,
assert key differs. This bounds the "wrong metadata served from cache"
risk (gospel P4).

### D13. Distribution

The sidecar is built into `target/csharp-sidecar/` by a `build.rs`
(or a cargo xtask) that calls `dotnet publish -c Release -r <rid>` on
`tools/csharp-sidecar/csharp-sidecar.csproj`. The Rust LSP discovers the
binary relative to its own executable at runtime.

`build.rs` is gated: if `dotnet` is absent, the sidecar isn't built and the
LSP runs without C# support. Cargo build does not fail.

Self-contained vs framework-dependent publish: framework-dependent for v1
(smaller, requires the user already has the SDK they're using). Revisit if
distribution to users without the SDK becomes a requirement.

## Phased implementation

One PR per phase. Each phase ends with a green `cargo test` (Rust side) and
green `dotnet test` (sidecar side) where applicable.

1. **Sidecar scaffold + protocol.** Create `tools/csharp-sidecar/` with
   `csharp-sidecar.csproj` targeting net10.0, an `initialize`/`shutdown`
   JSON-RPC loop over stdio, and the wire-format DTOs. No Roslyn yet — the
   sidecar just answers `initialize` with version strings. Rust-side
   `src/csharp_sidecar/process.rs` spawns it, completes a handshake, shuts
   it down cleanly. End-to-end integration test covering the handshake.
2. **MSBuildWorkspace load.** Sidecar calls `MSBuildLocator.RegisterDefaults()`
   and `MSBuildWorkspace.OpenProjectAsync` for a single csproj. Surfaces
   load diagnostics through the protocol. `buildMetadata` returns
   `NotImplemented` for now but the project is loaded and inspectable.
3. **Emit metadata-only.** Drive `CSharpCompilation.Emit` with the right
   `EmitOptions` (D5). Write to a temp file inside the cache dir; no cache
   yet — every call re-emits. Differential test (D12) against the `empty`
   and `pkg-ref` fixtures: assembly reader sees the same surface in both
   sidecar output and `dotnet build` output.
4. **Cache.** Implement D6: cache-key derivation, content-addressed
   lookup, atomic publish. Property tests for key determinism. Idempotency
   integration test.
5. **Transitive ProjectReferences.** Topo-sort `<ProjectReference>` closure
   (D7), build leaves first, include their metadata DLL paths in the
   response. Differential test against the `proj-ref` fixture.
6. **Source generators + analyzers + IVT.** Verify the existing pipeline
   handles these correctly via fixtures; no new code expected — this
   phase is "the differential test passes on `source-gen`, `nullable`,
   and `ivt` fixtures." The `ivt` fixture asserts internal members
   survive in the sidecar's DLL directly (ref-assembly mode strips
   internals, so it can't be the comparator).
7. **Diagnostics polish.** Map Roslyn diagnostics to the wire format (D8),
   handle `Emit.Success == false` correctly (no stale-cache fallback).
   Tests covering an intentional CS0103 in a fixture.
8. **Distribution.** `build.rs` that publishes the sidecar and a
   discovery shim in `src/csharp_sidecar/process.rs`. Smoke test on a
   real F# project that has a C# project reference.

**Note on retired phase 6 (`invalidate`).** Earlier drafts of this plan
included a phase that wired the JSON-RPC `invalidate` method (D9):
the LSP would push file events, the sidecar would clear cached
project state so the next `buildMetadata` re-evaluated from disk.
Phase 4 made that step structural rather than imperative — each
`buildMetadata` already creates a fresh `MSBuildWorkspace`, reads the
csproj from disk, computes the cache key from Roslyn's deterministic
key (which itself reads source content), and either returns the
keyed DLL or re-emits. There is no in-process workspace state to
invalidate; source mutations naturally change the cache key on the
next call. If we ever add a warm-workspace optimisation, `invalidate`
becomes meaningful again and can be reintroduced at that point.

## Out of scope (deliberate)

- Running `dotnet restore`. Caller must restore. If
  `project.assets.json` is absent, return `RestoreRequired`.
- Watching files. Rust LSP pushes invalidations.
- Concurrent builds within one sidecar. Requests serialise.
- Razor / Blazor / XAML / WinForms designer codegen. Roslyn handles the
  `.cs` outputs of these if they exist; we don't drive the generators
  ourselves.
- Solution (`.sln`) loading. We operate per-csproj. If a csproj depends
  on out-of-solution projects, MSBuildWorkspace handles it via direct
  csproj references.
- Cache GC. Manual cleanup for now; add when the cache directory becomes
  a real footprint problem.
- Self-contained sidecar publish. Framework-dependent for v1.
- Multi-targeting csproj (`<TargetFrameworks>foo;bar</TargetFrameworks>`).
  V1 picks the single TFM the caller asks for; if the csproj has multiple
  and the caller's choice doesn't match any, return `TfmMismatch`. Same
  policy the F# `project_assets` plan takes.

## Risks carried forward

- **MSBuildLocator coupling.** `MSBuildLocator.RegisterDefaults` must run
  before any Microsoft.Build type is touched, including transitively
  through Roslyn workspaces. Get the order wrong and you get a confusing
  "could not load assembly" error at first project load. Documented in
  Program.cs and asserted with a debug check (gospel P2).
- **Roslyn version drift.** Roslyn ships in lockstep with the SDK; the
  sidecar references a specific `Microsoft.CodeAnalysis.CSharp` version
  that may not match the user's SDK exactly. Same-major-version drift is
  fine; cross-major may be. The differential test catches semantic
  differences; if it fails after an SDK bump, that's our signal.
- **Source generator side effects.** Some real-world SGs (Razor, EF Core)
  do filesystem IO, log to disk, or assume a specific MSBuild property
  set. We invoke them in a context they may not have been tested in.
  Mitigation: differential test against `dotnet build`. Any SG whose
  output we can't reproduce becomes a known limitation.
- **`InternalsVisibleTo` correctness with metadata-only emit.** Roslyn's
  metadata-only path historically had bugs around `IVT` (rewriting
  attribute targets in older versions). The `ivt` fixture (D12) pins
  current behaviour; revisit if it ever fails after an SDK bump.
- **Cache key completeness.** If we miss an input (e.g. an environment
  variable that affects Roslyn behaviour, or an MSBuild property that
  changes the emitted surface), we'll serve stale metadata. The
  differential test against `dotnet build` is the canary: if Roslyn
  produces output A and the sidecar's cached output B differs while the
  key matches, the key is missing an input. Treat any differential-test
  flake as a key-completeness bug and add the missing input.
- **Sidecar protocol versioning.** When we add fields or methods, old
  Rust talking to new sidecar (or vice versa) should fail loudly, not
  silently misinterpret. `initialize` response carries a protocol
  version; mismatches are fatal.
