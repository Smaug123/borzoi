# `project.assets.json` parser + resolver plan

> **Status: implemented.** Every D1–D6 decision is in place under
> `crates/lsp/src/project_assets/` (the module moved out of the
> originally-planned top-level `src/` when the workspace was split into
> crates). The original public API (`resolve_assemblies`) is intact and
> has since been extended by
> [`multi-tfm-resolution-plan.md`](multi-tfm-resolution-plan.md) with a
> `tfm: String` field on `Reference::ProjectRef`, the
> `resolve_transitive_project_tfms` helper, and three new error
> variants (`ProjectRefUnresolved`, `RestoreMismatch`,
> `ProducerAssetsNotProvided`). The supporting `closure.rs` and
> `tfm.rs` modules were added by the same work. Tests (unit, proptest,
> integration) cover the originally-planned cases plus the multi-TFM
> extensions.

Design doc for a module that reads `obj/project.assets.json` and produces the
list of compile-time assemblies an F# project depends on. Captures decisions
made before implementation started so future work can resume from a cold
pickup.

## Context

This LSP needs to know which assemblies an F# project depends on. For each
project we open, we want a list of absolute paths to:

1. NuGet package compile-time DLLs.
2. Transitive project reference projects (their `.fsproj` paths, plus their own
   package DLLs).
3. Shared-framework reference assemblies (e.g. `Microsoft.NETCore.App`).

The authoritative source for #1 and #2 is the `obj/project.assets.json` that
`dotnet restore` writes. For #3 the assets file lists the framework name but
the DLLs live under `$DOTNET_ROOT/packs/{Name}.Ref/{version}/ref/{tfm}/*.dll`.

Nothing in the current crate touches the build system (no references to
`.fsproj`, MSBuild, or NuGet anywhere in `src/`). This is greenfield work.

## Scope

- **Input.** A path to a `project.assets.json` and a `dotnet_root` for
  framework pack lookup. Both passed in explicitly (dependency rejection — no
  env-var sniffing in the core).
- **Output.** A struct of three deduplicated `Vec<PathBuf>`s: package DLLs,
  framework DLLs, transitive project file paths.
- **TFM handling.** Auto-pick the only TFM. If `targets` contains more than
  one TFM, error and force the caller to disambiguate (caller can pre-filter,
  or we add a TFM parameter later).
- **What's enumerated.** Compile-time DLLs only. Not `runtime`, not
  `resource`/satellites.

## Settled decisions

### D1. Module layout

Five files under `src/project_assets/`:

- `mod.rs` — public API (`resolve_assemblies`), recursion shell.
- `raw.rs` — serde-derived structs mirroring the JSON 1:1.
- `enumerate.rs` — pure `enumerate_one` returning `Vec<Reference>`.
- `framework.rs` — locate framework pack DLLs on disk.
- `error.rs` — `ProjectAssetsError` enum.

Rationale: matches the existing convention of one module per concern (`lexer`,
`lexfilter`, `parser`, `syntax`, `diagnostics`). The split also mirrors the
three concentric layers below.

### D2. Three concentric layers

**Layer 1 — Deserialize (pure).** serde-derived structs that mirror the JSON.
Use `BTreeMap` over `HashMap` for deterministic iteration. Model only the
fields we use; ignore `sha512`, `files`, `runtime`, `resource`, `dependencies`,
`projectFileDependencyGroups`, `restore`, etc. (the default
`deny_unknown_fields = false` does the right thing).

**Layer 2 — Enumerate (pure).** Given a `RawAssets` and the directory the
assets file lives in (for resolving relative project paths), return a
`Vec<Reference>` where:

```rust
pub enum Reference {
    PackageDll { absolute_path: PathBuf },
    ProjectRef { project_path: PathBuf },
    Framework { name: String, tfm: String },
}
```

Algorithm:

1. Pick the single TFM. If `assets.targets.len() != 1`, return
   `MultipleOrNoTargets`.
2. Emit a `Framework` per name in `assets.project.frameworks[tfm].frameworkReferences`.
3. For each `(name_version, entry)` in `targets[tfm]`:
   - `kind == "project"`: push `ProjectRef { absolute path }`.
   - `kind == "package"`: look up `libraries[name_version].path` (lowercase
     package dir). For each key in `entry.compile`, skip if filename is `_._`,
     otherwise join `package_folders.first() / library_path / compile_key` and
     emit `PackageDll`. The first `package_folder` is the canonical NuGet
     cache; subsequent entries are fallbacks for offline scenarios.

This layer is pure: it does not touch the filesystem.

**Layer 3 — Recurse + resolve frameworks (shell).** Public `resolve_assemblies`
runs a worklist over project.assets.json files, collecting package DLLs,
project file paths, and (via `framework::resolve`) framework DLL absolute
paths. Maintains a visited set of project paths to break cycles. If a
transitive project's `obj/project.assets.json` is missing, return
`MissingTransitiveAssets { project_path }` rather than silently dropping its
deps — an incomplete reference list is more dangerous than an error.

### D3. Framework pack resolution

```rust
pub fn resolve_framework(
    dotnet_root: &Path,
    name: &str,   // e.g. "Microsoft.NETCore.App"
    tfm: &str,    // e.g. "net8.0"
) -> Result<Vec<PathBuf>, ProjectAssetsError>;
```

1. Read `{dotnet_root}/packs/{name}.Ref/`.
2. List subdirs; each is a version string. Parse versions as `Vec<u32>` split
   on `.`. Pick the highest version whose `ref/{tfm}` dir exists.
3. Return all `*.dll` paths in that `ref/{tfm}/` directory.

This is not the full MSBuild rollForward logic — we just pick the highest
locally-installed pack that has the right TFM ref directory. Good enough for
LSP needs; revisit if a real project hits a mismatch.

### D4. Public API

Single entry point:

```rust
pub struct ResolvedAssemblies {
    pub package_dlls: Vec<PathBuf>,
    pub framework_dlls: Vec<PathBuf>,
    pub project_refs: Vec<PathBuf>,
}

pub fn resolve_assemblies(
    root_assets_path: &Path,
    dotnet_root: &Path,
) -> Result<ResolvedAssemblies, ProjectAssetsError>;
```

Both args explicit. No `FileSystem` trait — that would be an "interface for
one implementation." Tests use `tempfile`; production uses `std::fs`.

### D5. Error type

Hand-rolled enum (matches `LexError`, `ParseError` style — no `thiserror`):

```rust
pub enum ProjectAssetsError {
    Io { path: PathBuf, source: std::io::Error },
    Json { path: PathBuf, source: serde_json::Error },
    MultipleOrNoTargets { found: Vec<String> },
    LibraryEntryMissing { name_version: String },
    ProjectRefMissingPath { name_version: String },
    MissingTransitiveAssets { project_path: PathBuf },
    FrameworkPackNotFound { name: String, searched: PathBuf },
    FrameworkRefForTfmMissing { name: String, tfm: String },
}
```

### D6. Testing

Inline `#[cfg(test)]` for unit tests, `tests/` for integration — matches the
project convention.

**Fixtures.** Check in two real `project.assets.json` files:

1. `tests/fixtures/project_assets/single_tfm.json` — copy from
   `tools/fcs-dump/obj/project.assets.json` (one TFM `net10.0`, references
   `FSharp.Compiler.Service`).
2. `tests/fixtures/project_assets/with_proj_ref.json` — find one in the
   `../fsharp` checkout, or hand-craft a minimal one.

**Unit tests (inline).**

- `raw.rs`: deserialize each fixture; spot-check field values.
- `enumerate.rs`: single-TFM fixture → expected count of `PackageDll`, no
  `_._`, no `runtime`-only DLLs. Multi-TFM synthetic input →
  `MultipleOrNoTargets`. Project-ref synthetic input → `ProjectRef` with
  absolute path.
- `framework.rs`: tempfile tree mimicking
  `packs/Microsoft.NETCore.App.Ref/{8.0.1,8.0.11}/ref/net8.0/*.dll`; assert
  the highest version wins and all DLLs are returned.

**Property tests** (`proptest = "1"` as dev-dep). Generate `RawAssets` values
with 1–3 packages, 0–2 project refs, 0–1 framework refs, single TFM. Properties:

1. **Roundtrip**: serialize → parse → equal. Catches drift in serde derives.
2. **Enumeration soundness**: every `PackageDll` path starts with one of
   `assets.package_folders`'s keys.
3. **No `_._` leak**: inject `_._` keys; assert they never appear in output.
4. **Type filtering**: `type: "project"` entries never appear in `PackageDll`;
   `type: "package"` entries never appear in `ProjectRef`.
5. **TFM gate**: when the generator produces N TFMs, `enumerate_one` returns
   `Ok` iff `N == 1`.
6. **Determinism**: calling `enumerate_one` twice returns the same `Vec` in
   the same order (depends on `BTreeMap` iteration).

**Integration test.** `tests/project_assets_integration.rs`: end-to-end on
the single-TFM fixture. Assert `FSharp.Compiler.Service.dll` is in
`package_dlls`. Skip framework assertions (depends on host `$DOTNET_ROOT`);
covered by framework unit tests via `tempfile`.

## Out of scope (deliberate)

- `runtime` / `resource` / satellite assemblies. Compile-time DLLs only.
- `rollForward` / framework selection beyond "highest locally-installed match".
- Lockfile invalidation. We trust whatever's on disk; the caller restored.
- A `FileSystem` trait.
- `$DOTNET_ROOT` discovery defaults. Caller passes it explicitly.

## Files to add

- `src/project_assets/{mod,raw,enumerate,framework,error,tests}.rs`
- `tests/fixtures/project_assets/{single_tfm,with_proj_ref}.json`
- `tests/project_assets_integration.rs`

## Files to modify

- `src/lib.rs` — add `pub mod project_assets;`.
- `Cargo.toml` — add `proptest = "1"` to `[dev-dependencies]`.

## Verification

1. `cargo build` — clean.
2. `cargo test` — unit + property + integration green.
3. `cargo clippy` — clean.
4. `cargo fmt` applied.
5. Manual smoke: call `resolve_assemblies("tools/fcs-dump/obj/project.assets.json", &PathBuf::from("/usr/local/share/dotnet"))` and print results; expect `FSharp.Compiler.Service.dll` plus framework BCL DLLs.

## Suggested commit decomposition

To keep PRs reviewable:

1. `raw.rs` + `error.rs` + fixture + deserialization test.
2. `enumerate.rs` + unit tests + proptest infrastructure.
3. `framework.rs` + tempfile tests.
4. `mod.rs` recursion shell + integration test.
