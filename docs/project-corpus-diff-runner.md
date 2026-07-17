# Project Corpus Resolution Diff Runner

`borzoi-corpus-diff` is an unpublished workspace crate for empirical
checks of project-aware name resolution. It loads `.fsproj` files through the
same semantic path as the LSP, asks FCS for `uses-project` symbol-use data, and
compares our project and assembly resolutions against that oracle.

This is a proof harness only to the extent that the selected corpus actually ran
and produced comparable projects. Skips and low coverage are reported because
they are not evidence of correctness.

## Quick Runs

Run a sampled corpus sweep from a root directory:

```sh
BORZOI_PROJECT_CORPUS=/path/to/fsharp \
BORZOI_PROJECT_MSBUILD_PROPERTIES=DISABLE_ARCADE=true \
BORZOI_PROJECT_LIMIT=20 \
BORZOI_PROJECT_REPORT_JSONL=target/project-corpus-diff.jsonl \
nix develop -c cargo run -p borzoi-corpus-diff
```

Run an exhaustive corpus sweep:

```sh
BORZOI_PROJECT_CORPUS=/path/to/fsharp \
BORZOI_PROJECT_MSBUILD_PROPERTIES=DISABLE_ARCADE=true \
BORZOI_PROJECT_EXHAUSTIVE=1 \
BORZOI_PROJECT_REPORT_JSONL=target/project-corpus-diff.jsonl \
nix develop -c cargo run -p borzoi-corpus-diff
```

Run specific projects instead of walking a directory:

```sh
BORZOI_PROJECT_LIST=/path/to/A.fsproj:/path/to/B.fsproj \
nix develop -c cargo run -p borzoi-corpus-diff
```

`BORZOI_PROJECT_LIST` uses the platform path-list separator (`:` on Unix,
`;` on Windows).

The ignored integration test is a wrapper around the same library runner:

```sh
BORZOI_PROJECT_CORPUS=/path/to/fsharp \
nix develop -c cargo test -p borzoi-corpus-diff --test project_resolution \
  project_corpus_resolution_diff -- --ignored --nocapture
```

Prefer the CLI for long local runs; use the ignored test when you specifically
want `cargo test` to own the gate.

## Environment

Exactly one source must be set:

- `BORZOI_PROJECT_CORPUS`: recursively discovers `.fsproj` files under this
  root.
- `BORZOI_PROJECT_LIST`: platform-separated explicit `.fsproj` list.

Optional selection settings:

- `BORZOI_PROJECT_EXHAUSTIVE=1`: visits every discovered project and fails
  if project discovery had traversal errors. This forbids `STRIDE`, `LIMIT`, and
  `MAX_FILES`.
- `BORZOI_PROJECT_STRIDE`: non-zero sampling stride. Defaults to `13` for
  non-exhaustive corpus runs and `1` for exhaustive runs.
- `BORZOI_PROJECT_LIMIT`: non-zero maximum number of projects to visit.
- `BORZOI_PROJECT_MAX_FILES`: non-zero maximum compile-file count per
  project; larger projects are skipped before semantic loading.

Optional project-load settings:

- `BORZOI_PROJECT_MSBUILD_PROPERTIES`: semicolon-separated `Name=Value`
  MSBuild global properties passed to the LSP project loader. Names must be
  unique under MSBuild's case-insensitive property comparison, and override the
  loader defaults (`Configuration=Debug`, `Platform=AnyCPU`) case-insensitively.
  For the F# repo corpus, use `DISABLE_ARCADE=true` to avoid making the
  name-resolution sweep depend on resolving the repo's pinned Arcade SDK.

Optional failure ratchets:

- `BORZOI_PROJECT_MAX_DIVERGENCES`: maximum allowed project, assembly, and
  reverse divergences combined. Defaults to `0`.
- `BORZOI_PROJECT_MIN_COMPARABLE`: non-zero minimum number of comparable
  projects.
- `BORZOI_PROJECT_MAX_SKIPPED`: maximum number of visited projects allowed
  to skip before comparison.
- `BORZOI_PROJECT_MAX_SKIPPED_BPS`: maximum skipped-project rate in basis
  points, where `2500` means `25.00%`.
- `BORZOI_PROJECT_MIN_COVERAGE_BPS`: minimum compared-use coverage in basis
  points, where `9500` means `95.00%`.

Basis-point ratchets must be integers from `0` through `10000`.

Optional reporting and oracle settings:

- `BORZOI_PROJECT_REPORT_JSONL`: writes one newline-terminated JSON summary
  record to this path, replacing previous contents. The record includes the
  effective MSBuild property profile.
- `BORZOI_FCS_DUMP`: path to a prebuilt `fcs-dump` binary. If unset, the
  runner builds `tools/fcs-dump` and invokes the generated DLL.

Restore real corpora before running when package or framework references matter.
The runner reads `obj/project.assets.json` to supply FCS with reference DLLs and
records whether assets were missing or resolved.

## What The Runner Compares

For each visited project, the runner:

1. Evaluates the `.fsproj` through the LSP-facing MSBuild path.
2. Parses and resolves the compile files through `SemanticState`.
3. Reads project assets to provide FCS with extra references where possible.
4. Invokes `fcs-dump uses-project`.
5. Parses FCS ranges back to byte offsets using full path identity.
6. Compares every comparable FCS project declaration and assembly declaration
   against sema resolution.
7. Checks the reverse direction: every concrete sema resolution in a comparable
   file must be covered by an FCS use.

The default soundness gate allows zero divergences.

## Current Failure Gates

The CLI and ignored test fail when:

- no projects were visited;
- no project became comparable;
- an exhaustive run encountered project-discovery traversal errors;
- a configured comparable-project, skipped-project, skipped-rate, or coverage
  ratchet fails;
- more project, assembly, or reverse divergences are reported than
  `BORZOI_PROJECT_MAX_DIVERGENCES` allows.

Missing project assets are reported but do not directly fail the run. They often
reduce FCS comparability or coverage, so pair long corpus runs with explicit
skip and coverage ratchets when treating the result as evidence.

## Skips And Non-Proof Cases

A skipped project contributes no evidence of correctness. Common skip reasons
include:

- unsupported `.fsi` compile items;
- uncertain MSBuild compile items or define constants, including the first
  captured import/SDK/item/condition diagnostics that made them untrustworthy;
- projects over `BORZOI_PROJECT_MAX_FILES`;
- missing semantic project data;
- FCS invocation or JSON parse failures;
- FCS error diagnostics in one or more files.

Corpus discovery skips symlinks and descends around `.git`, `target`,
`artifacts`, `bin`, and `obj` directories. In non-exhaustive mode, discovery
errors are reported but do not fail the run.

## Reading The Report

The text report starts with project counts:

- `discovered`: `.fsproj` candidates found or listed.
- `visited`: candidates selected after stride/limit.
- `comparable`: projects that loaded, invoked FCS, had no FCS error files, and
  reached comparison.
- `skipped`: visited projects that did not become comparable.
- `discovery errors`: traversal errors while collecting candidates.

The uses section distinguishes all FCS-reported uses from the subset we compare.
Coverage is `matches / compared uses`, not `matches / all FCS uses`; definitions,
zero-width uses, non-project declarations, and oracle uses without declarations
are counted separately under skipped uses.

The project section also reports the skipped-project rate. The JSON report
contains the same summary in a machine-readable form for ratchets or dashboards.
