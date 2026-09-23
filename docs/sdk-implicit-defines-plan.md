# SDK implicit defines: the `#if` symbols fsc actually receives

> Implement this plan with each stage on its own branch, stacked as necessary on
> previous branches, so that a reviewer can review each branch in isolation.

## The defect

The LSP folds every file under `{COMPILED, EDITING} ∪ define_constants`
(`workspace.rs`, `symbols_for_project` → `extend_with_define_constants`). For
an SDK F# project fsc receives more than that. On a plain `net8.0` project with
`<DefineConstants>$(DefineConstants);MINE</DefineConstants>` (audit,
2026-09-23):

| layer | value |
|---|---|
| ours (`define_constants`, `define_constants_uncertain = false`) | `MINE` |
| MSBuild, evaluation time | `TRACE;MINE;DEBUG` |
| MSBuild, after targets (what `Fsc` is passed) | the above plus `NET`, `NET8_0`, `NETCOREAPP`, `NET5_0_OR_GREATER` … `NET8_0_OR_GREATER`, `NETCOREAPP1_0_OR_GREATER` … `NETCOREAPP3_1_OR_GREATER` |

So `#if DEBUG` and `#if NET8_0_OR_GREATER` fold as false in every SDK project,
and the value is published as certain. That is a wrong answer, not a decline:
the wrong branch is parsed, resolved and shown to an agent.

The doc comment on `ParsedProject::define_constants_uncertain` records this as
an "accepted limitation" on the grounds that flagging it would force the
single-file fallback for every SDK project. That trade-off still holds, which
is why this plan **models** the symbols rather than flagging them. What does not
hold is the doc comment's claim that the divergence is bounded and harmless.

### Why no oracle saw it

- `fsproj_msbuild_corpus_diff` reads MSBuild's **evaluation-time**
  `DefineConstants`, so target-added symbols are invisible to it by
  construction.
- It then accepts extra SDK symbols on the MSBuild side
  (`sdk_injected_define_constants_extra`). Over 124 real projects that gave 0
  exact matches, 107 passes by leniency and 17 skips.
- The perturbation diff reports DefineConstants as "untrusted", but that is the
  *property table's* provenance channel. The LSP reads `define_constants`,
  gated by the separate user-authored-only flag.

The general lesson: **the oracle must observe the value at the point the
consumer's counterpart consumes it.** Here that point is the `Fsc` task's
`DefineConstants`, not the evaluation.

## Where the symbols come from (SDK 10.0.301)

| symbol | file | phase |
|---|---|---|
| `TRACE` | `FSharp/Microsoft.FSharp.NetSdk.props`, a `Choose` on `'$(DefineConstants)' == ''`. It is imported through `$(FSharpPropsShim)` from `Microsoft.NET.Sdk.FSharp.props`, gated on `UseBundledFSharpTargets`. | evaluation, before the project body |
| `DEBUG` / `RELEASE` / any `$(Configuration)` | `Microsoft.NET.Sdk.FSharpTargetsShim.targets`: `$(Configuration.ToUpperInvariant())` with `-` and `.` mapped to `_`, gated on `DisableImplicitConfigurationDefines` and `UseBundledFSharpTargets` | evaluation, after the project body |
| `NET`, `NETx_y`, `NETCOREAPP`, `NETSTANDARD…`, `…_OR_GREATER`, platform symbols | `Microsoft.NET.Sdk.BeforeCommon.targets`: the targets `GenerateTargetFrameworkDefineConstants`, `GenerateTargetPlatformDefineConstants`, `GenerateNETCompatibleDefineConstants`, `GeneratePlatformCompatibleDefineConstants` and `AddImplicitDefineConstants` (`AfterTargets="PrepareForBuild"`, depending on all four plus `_DisableDiagnosticTracing`) | **target execution** |
| removal of `TRACE` | `_DisableDiagnosticTracing` (`BeforeTargets="CoreCompile"`), gated on `DisableDiagnosticTracing` | target execution |

The target-phase inputs are:
- evaluation-time properties: `TargetFrameworkIdentifier`,
  `TargetFrameworkVersion`, `TargetPlatformIdentifier`,
  `TargetPlatformVersion`, `EffectiveTargetPlatformVersion`, and the
  `Disable*` flags;
- evaluation-time items: `SupportedNETCoreAppTargetFramework`,
  `SupportedNETFrameworkTargetFramework`,
  `SupportedNETStandardTargetFramework` and `SdkSupportedTargetPlatformVersion`.

None of them needs a restore.

Why our evaluation also lacks `TRACE`/`DEBUG` is **not yet diagnosed**.
`MSBuildToolsPath` is modelled, so the likely suspects are:
- the `FSharpPropsShim` import is being declined;
- the SDK-subtree tolerance is swallowing a write it should apply;
- `is_define_self_reference` treats an SDK-present `$(DefineConstants)` as
  `""`.

Stage 3 settles it.

## Design

1. **Oracle: the defines fsc receives.** Add a `defines` op to
   `tools/msbuild-condition-oracle`. It restores, then runs in-process the
   targets a real `Build` runs through `Compile`, read from the project's own
   `$(BuildDependsOn)`/`$(CoreBuildDependsOn)`. It sets
   `ProvideCommandLineArgs` and `SkipCompilerExecution`, so fsc's arguments
   are computed but fsc is not run. It reads the `--define:`
   tokens of the `FscCommandLineArgs` that the real `Fsc` task computed.
   - Nothing about `Fsc`'s parameter binding is re-implemented: `DefineConstants`,
     `Nullable` and `OtherFlags` are expanded exactly as task parameters are,
     item references included.
   - It restores, as a real build does, because a package's build props can
     write `DefineConstants`.
   - Only the exact canonical `--define:X` is accepted. Any other token that
     could define a symbol declines the request rather than being parsed:
     `-d:X`, `/define:X`, whitespace-padded or quoted, or a response file.
   - Project references are resolved without being built.
   - A nonexistent `CustomAdditionalCompileInputs` item keeps an
     already-built project's `CoreCompile` from being skipped.
   - Its own oracle is a *real* `Build`'s arguments: restored, compiling, with
     `ProvideCommandLineArgs` only.

   Four review rounds shaped this. The first version ran only the SDK's two
   define targets and re-read `$(Nullable)`/`$(OtherFlags)` by hand. That
   answered successfully but incompletely on:
   - case-folded duplicates;
   - `NULLABLE`;
   - `OtherFlags` aliases and response files;
   - item references in task parameters;
   - whitespace-padded flags and embedded line breaks;
   - unrestored package imports;
   - hooks in `Build` but outside `Compile` (`BeforeBuild`).

   Each was either a re-implementation of the build or a step the build takes
   that the op skipped, which is why the op now runs the build and only reads
   its output.
2. **One consumed value, one comparison.** `ParsedProject::define_constants`
   becomes the value `Fsc` receives. It is the sum of:
   - the evaluation-time value (Stage 3);
   - the modelled target phase (Stage 4).

   It is certain only when both layers are certain. Every harness compares
   exactly that field against the `defines` op, and the leniency is deleted.
3. **Model the target phase as a pure function over evaluated inputs.**
   `implicit_define_constants(&TargetPhaseInputs) -> Result<Vec<String>,
   ImplicitDefinesDecline>`, with a closed envelope, where each exit is a named
   decline cause:
   - inside the envelope: `.NETCoreApp`, `.NETStandard` and `.NETFramework`
     (the vocabulary is required even though net4x is unsupported);
   - declined for now: any `TargetPlatformIdentifier` (so `net8.0-windows`
     declines);
   - declined: any untrusted input;
   - declined: any **user-authored** target (outside the SDK subtree) that
     writes `DefineConstants` or `_ImplicitDefineConstant`, or that hooks one
     of the six targets above through `BeforeTargets` / `AfterTargets` /
     `DependsOnTargets`;
   - `Fsc`'s two other symbol sources, which the oracle handles the same way:
     - `NULLABLE` is appended when `$(Nullable)` is exactly `enable` (the `Fsc`
       setter's match is case-sensitive);
     - the project is declined when `$(OtherFlags)` could carry a
       `--define:`/`-d:`. It is passed to fsc verbatim, and tokenising a
       command line is not worth modelling.

   This deliberately duplicates SDK logic in Rust. `#263` avoided doing that for
   `Link` because nothing consumed `Link`. Here the value is consumed and
   decides which code the whole semantic layer sees, so the check from memory
   note "check for a consumer before paying for a trust verdict" comes out the
   other way.
4. **The LSP reads the served build's value.** A multi-targeted project serves
   one inner TFM (`select_target_framework`). The symbols must come from that
   inner build's evaluation, never from the outer one's.

The `Configuration=Debug` global is a product guess, the same default FSAC
makes. The `DEBUG` symbol is only as right as that guess. That is out of scope
here, but the perturbation census should keep reporting it.

## Stages

### Stage 1: the `defines` oracle op, calibrated against real fsc arguments

**Dependencies**: none.

**Implements**: Design §1.

**Correctness oracle** (landed as `crates/msbuild/tests/defines_oracle_calibration.rs`):
- The `defines` op, asked first on the unrestored project, returns exactly the
  `--define:` arguments of a real, restored, compiling build's
  `FscCommandLineArgs` under the same globals, in order. The only exception is
  a decline where the fixture demands one.
- Calibration set (33 cases):
  - a `net10.0;net6.0;netstandard2.0` project, per inner TFM, crossed with
    `Configuration` ∈ {Debug, Release, `My-Config.1`};
  - `DisableImplicitFrameworkDefines`, `DisableDiagnosticTracing`, both
    together, and `DisableImplicitConfigurationDefines`;
  - a user value with whitespace and empty fragments, duplicate and
    case-distinct symbols, and an overwrite that drops the self-reference;
  - `Nullable` as `enable`, as `Enable`, and through an item reference;
  - `OtherFlags` with no define, with `--define:`, and with `--define:`
    through an item reference;
  - user targets that append before `CoreCompile` and before `BeforeBuild`;
  - a project reference;
  - a locally packed package whose `build/*.props` appends `FROM_PACKAGE`;
  - declines, each of which must also carry the symbol to fsc: `OtherFlags`
    with `-d:`, with `/d:`, with a response file, with an embedded line break,
    and with each define spelling padded with whitespace;
  - a user target that appends only when fsc really runs. The op must
    *disagree* here, which proves the calibration can see the op's genuine
    boundary.
- `net472` and `net8.0` are absent because the devshell's offline package set
  carries neither targeting pack.
- It takes about a minute and runs with the crate's ordinary tests.
- Mutation checks confirmed it discriminates:
  - classifying no token as declinable fails the decline cases;
  - a stale `IntermediateOutputPath` (`CoreCompile` skipped as up to date)
    fails every case;
  - skipping the restore fails every unrestored case;
  - running `Compile` alone instead of the `Build` prefix fails the
    `BeforeBuild` case.
- Out of scope, and why: build logic conditioned on fsc really running
  (`SkipCompilerExecution`), which the boundary fixture pins.

### Stage 2: census, not gate

**Dependencies**: Stage 1.

**Implements**: Design §2 (measurement half).

**Correctness oracle**:
- `fsproj_msbuild_corpus_diff` gains a report-only `define_constants`
  comparison against the `defines` op. It uses no leniency, and it covers the
  gate sample, the exhaustive pinned corpus and the local project corpus.
- It prints a census bucketed by **cause**, not by symptom:
  - a missing evaluation-layer symbol (`TRACE`/`DEBUG`);
  - a missing target-layer symbol;
  - an extra symbol;
  - an order-only difference, which `Fsc` ignores (it treats the list as a
    set).
- The census is a named, unwired row in `docs/continuous-measurements.md`.
  Do **not** land a ceiling equal to today's divergence count; that doc
  records why.

### Stage 3: the evaluation layer matches MSBuild

**Dependencies**: Stage 2 (for the census), though not in code.

**Implements**: "Where the symbols come from", rows `TRACE` and `DEBUG`.

**Correctness oracle**:
- Establish first why our evaluation of the real SDK chain lacks
  `TRACE`/`DEBUG`: a `project`-op diff of `DefineConstants` on one real SDK
  project, with the import trace. Fix the cause, not the symptom. Memory note
  "walker already runs the SDK targets": the bug is usually import position.
- Delete `sdk_injected_define_constants_extra`'s acceptance of
  `DEBUG`/`TRACE`. The corpus diff then requires evaluation-time exactness on
  every compared project.
- Add a generated sweep to `fsproj_global_perturbation_diff` covering:
  - `Configuration` ∈ {Debug, Release, `My-Config.1`, empty};
  - the `Disable*` flags;
  - user `DefineConstants` writes before and after the SDK props, with
    whitespace, empty fragments and `%3b` in the values.

  The whitespace case closes the audit's missed mutation: "fragments not
  trimmed" passed every harness.
- The sweep asserts certain-implies-exact against `defines`, with a per-cell
  must-commit obligation for the plain cells, so declines cannot creep (memory
  note "commit-count floors rot").

### Stage 4: the target phase as a pure, oracle-tested function

**Dependencies**: Stage 1. This can proceed in parallel with Stage 3.

**Implements**: Design §3.

**Correctness oracle**:
- A generated differential over the TFM vocabulary:
  - `net5.0` … `net10.0`;
  - `netcoreapp1.0` … `netcoreapp3.1`;
  - `netstandard1.0` … `netstandard2.1`;
  - `net20` … `net481`;
  - platform TFMs, which must decline;

  crossed with the `Disable*` flags. The inputs are read from a real
  evaluation (the `project` and `items` ops), so the Supported* item lists are
  the SDK's own, not hand-copied.
- The contract is certain-implies-exact against the `defines` op minus the
  evaluation-layer value. Every non-platform cell carries a must-commit
  obligation.
- A property test, with no oracle: the output is a pure function of
  `TargetPhaseInputs`. Permuting the order of the input items changes nothing
  but order, and adding an unrelated property changes nothing.
- Metamorphic user-target sweep: inject each of the hook shapes in Design §3
  into a fixture, and assert that each one produces the named decline and never
  a value.

### Stage 5: `define_constants` is the value fsc receives

**Dependencies**: Stages 3 and 4.

**Implements**: Design §2 (gate half).

**Correctness oracle**:
- `ParsedProject::define_constants` is the evaluation value followed by the
  modelled target phase. `define_constants_uncertain` is raised, with a new
  `DefineConstantsUncertaintyCause` variant, when Stage 4 declines.
- The Stage 2 census becomes a gate in `ci.yml` at zero divergences over the
  exhaustive pinned corpus. The exhaustive run is cheap: 92 s under load 160
  in the audit.
- Delete the "Accepted limitation" paragraph from
  `define_constants_uncertain`'s doc comment, and update
  `docs/completed/fsproj-tfm-selection-plan.md`'s claim about the flag.
- Mutation check before landing: re-apply the audit's M8 ("uncertain never
  set") and a "drop the `_OR_GREATER` family" mutation. Both must turn this
  gate red, not only unit tests.

### Stage 6: the LSP folds under the served build's symbols

**Dependencies**: Stage 5.

**Implements**: Design §4.

**Correctness oracle**:
- An E2E test on a restored multi-targeted fixture (`net8.0;netstandard2.0`).
  A file with `#if NET8_0_OR_GREATER` / `#if NETSTANDARD2_0` / `#if DEBUG`
  resolves the branch fsc compiles for the served TFM. Go-to-definition on a
  name bound only in that branch lands in it.
- A corpus differential over the six pinned `corpus-diff` projects. The LSP's
  symbol set for each file (`Workspace::symbols_for`) must equal the
  design-time `FscCommandLineArgs` defines ∪ `{COMPILED, EDITING}` for the
  served TFM. The contract is certain-implies-exact, and the gate is per
  project.
- `corpus-diff` stops feeding FCS our own `BORZOI_FCS_DEFINES`. It takes them
  from the same `FscCommandLineArgs`, so the whole-project oracle no longer
  inherits our answer (audit finding B).

## Out of scope

- Whether `Configuration=Debug` is the right guess (see Design).
- Platform TFMs (`net8.0-windows`, `-android`, …). Stage 4 declines them;
  widening the envelope is a later, census-driven decision.
- `.NETFramework` quality. The vocabulary is modelled because the SDK logic is
  shared, but net4x is an unsupported target (AGENTS.md).
