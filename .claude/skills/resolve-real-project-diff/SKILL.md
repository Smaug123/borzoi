---
name: resolve-real-project-diff
description: How to run the end-to-end whole-project name-resolution differential (`resolve_real_project_diff`) against a real, restored F# project, comparing the LSP's full runtime chain (Compile order + assets-file assembly closure + sema fold) to FCS. Use whenever you want to answer "can the LSP correctly analyse a real project?", validate the runtime resolution chain against a new project, or triage a real-world resolution divergence.
---

# Whole-project resolution differential vs FCS

`crates/lsp/tests/all/resolve_real_project_diff.rs` drives the **real LSP
runtime chain** end-to-end over one on-disk F# project and diffs every name
use against FCS:

1. Compile order + `#if` defines from the workspace's `.fsproj` evaluation
   (`SemanticState::parses_for_project`).
2. The referenced-assembly closure from the project's
   `obj/project.assets.json` (`resolve_assemblies_root_only`) → an
   `AssemblyEnv` built by reading each package/framework DLL with
   `borzoi-assembly`.
3. `resolve_project` folds cross-file + assembly resolution over the lot.

FCS (`fcs-dump uses-project`) is the oracle: it type-checks the same
Compile-ordered files as one project with the project's NuGet DLLs injected.
This is the sharpest single test for "does dependency resolution actually work
on a real project", because it exercises the whole chain, not a unit slice.

## Prerequisites

- The target project must have been **`dotnet restore`d** — the test reads
  `<project_dir>/obj/project.assets.json`. The LSP reads *only* that standard
  location (no `BaseIntermediateOutputPath` support), so a project that
  relocates `obj/` — e.g. the F# compiler's `artifacts/obj/` layout — will not
  resolve.
- Run under `nix develop` (the harness builds/drives `fcs-dump`). The first run
  builds `fcs-dump` via `dotnet build -c Release`, so budget several minutes.

## Choosing a project

The oracle is faithful only for projects that are:

- **signature-free** — any `.fsi` in the Compile set makes the LSP refuse the
  whole project (no CST signature model yet), so the test panics at the
  Compile-order step. Pick a project with no `.fsi`.
- **SDK-default framework** — a non-default `<FrameworkReference>`
  (`Microsoft.AspNetCore.App`, `WindowsDesktop`) is out of scope (FCS isn't
  handed it, so those framework symbols diverge).
- **multi-file with imported-assembly uses** — the test *gates* on
  `cross_file_match > 0` and `asm_match > 0`, so a single-file project or one
  with only local references is rejected as vacuous. (A `<TargetFramework>` in
  the SDK-default set, no duplicate Compile basenames.)

A non-default `<LangVersion>` *is* supported (threaded to FCS as
`--langversion`), unless the pin needs an SDK newer than the oracle's.

Quick candidate scan for a restored, signature-free, multi-file project:

```sh
# multi-file (>1 Compile), no .fsi, no FrameworkReference, restored:
for f in $(find ~ -maxdepth 6 -name '*.fsproj' 2>/dev/null); do
  d=$(dirname "$f")
  [ -f "$d/obj/project.assets.json" ] || continue
  [ "$(ls "$d"/*.fsi 2>/dev/null | wc -l)" -eq 0 ] || continue
  grep -q FrameworkReference "$f" && continue
  n=$(grep -c 'Compile Include' "$f")
  [ "$n" -gt 1 ] && echo "$n  $f"
done
```

## Running

```sh
BORZOI_PROJECT_FSPROJ=/abs/path/to/Foo/Foo.fsproj \
  nix develop -c cargo test -p borzoi --test all \
  resolve_real_project_diff:: -- --ignored --nocapture
```

`#[ignore]`d by default; skips with guidance if `BORZOI_PROJECT_FSPROJ` is
unset. Note the `--test all <group>::` filter form (one test binary per crate —
see AGENTS.md); `--test resolve_real_project_diff` does **not** resolve.

## Reading the result

The report line tallies (see `report`):

```
resolve-real-project <path>: <N> in-proj match (<M> cross-file) | <A> asm match | <D> diverge | <B> alt-binder | <G> gaps
```

- **in-proj / asm match** — uses where our resolution equals FCS (in-project
  binder, or `(assembly simple name, full name)` for imported symbols).
- **gaps** — `Deferred`/unmodelled uses. Expected; **counted, not gated**.
- **divergences + alt-binders** — both **gated to zero**. A divergence is a
  wrong/`Unresolved` resolution where FCS resolved concretely; an alt-binder is
  a same-named binder at the wrong range/file (a wrong-shadow go-to-def). Each
  gated site is printed as `"<file>":<range> "<text>" -> FCS <x>, we gave <y>`.

A divergence here is a **sema** (name-resolution) finding, not necessarily a
dependency-resolution one: if the "we gave" side names a symbol in a referenced
assembly at all, that assembly *was* resolved and read — the disagreement is
about resolution *precedence*. A genuine dependency-resolution failure shows up
instead as `asm_match == 0`, an empty env, or the vacuity assertion firing.

Validated (zero divergences) against `WoofWare.{WeakHashTable, LiangHyphenation,
Expect}`; `WoofWare.PawPrint.Domain` surfaced one `String`-abbreviation
precedence divergence (FSharp.Core `Microsoft.FSharp.Core.String` vs
`System.String`) — a sema precedence bug, with dependency resolution otherwise
matching FCS across ~4.8k in-project + ~1.4k imported-assembly uses.

## What this test does *not* cover

- The in-house NuGet resolver (`borzoi-nuget`) is **not** wired into the
  runtime (Slice 8 of `docs/nuget-restore-plan.md` outstanding); resolution
  depends entirely on a pre-existing `dotnet restore`. This test therefore only
  exercises the assets-file path, never the offline-resolve fallback.
- C# `<ProjectReference>`s go through the sidecar; an F# project *behind* a C#
  boundary is a known under-resolution.
