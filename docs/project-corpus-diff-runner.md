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

- `BORZOI_PROJECT_EXPECT_DIVERGENCES`: the exact divergence counts this corpus
  is known to produce, spelled `assembly=<n>,project=<n>,reverse=<n>` in any
  order. All three categories are required. **Two-sided**: a run that diverges
  more fails, and so does a run that diverges less — so a fix cannot land
  without bringing the recorded number down with it, and the gate cannot decay
  into a rubber stamp. Per category rather than a total, because a change that
  trades a project wrong target for an assembly one moves the total by zero.
  This is what `ci.yml`'s `corpus-diff` job sets.
- `BORZOI_PROJECT_MAX_DIVERGENCES`: maximum allowed project, assembly, and
  reverse divergences combined. Defaults to `0`. The one-sided form; setting it
  alongside `BORZOI_PROJECT_EXPECT_DIVERGENCES` is a configuration error rather
  than a precedence rule, since the two are incompatible readings of the same
  quantity.
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
- `BORZOI_PROJECT_SUMMARY_JSON`: writes the continuous-measurements *generator
  contract* (`docs/continuous-measurements.md`) to this path. This is the
  compact, durable half of the same run — counts only, keyed so that every key
  means the same thing in every run of a series — and it is what
  `borzoi-stats record` publishes. The `REPORT_JSONL` above stays the full
  picture, worklists included; the two are written from one run and never
  disagree.
- `BORZOI_FCS_DUMP`: path to a prebuilt `fcs-dump` binary. If unset, the
  runner builds `tools/fcs-dump` and invokes the generated DLL.

Restore real corpora before running when package or framework references matter.
The runner records whether `obj/project.assets.json` was missing or resolved.

**Build the corpus, not just restore it, when a project has
`<ProjectReference>`s.** The oracle is handed exactly the reference set our own
`AssemblyEnv` is built from — `SemanticState::reference_dlls_for_project`: the
assets file's package and framework DLLs, each F# project reference's *built*
output DLL (`bin/<config>/<tfm>/<TargetName>.dll`), and the C# sidecar's
metadata DLLs. An unbuilt project reference is therefore absent from *both*
sides: we under-resolve every use of the referenced project's types, and FCS
answers FS0039 to each, so the project is skipped rather than compared.
A `dotnet restore` alone cannot produce those outputs, which is why the CI
measurement job (`.github/workflows/stats.yml`, restore-only) can only run
pinned projects that have no project references — admitting one that does needs
a build step there first.

## What The Runner Compares

For each visited project, the runner:

1. Evaluates the `.fsproj` through the LSP-facing MSBuild path.
2. Parses and resolves the compile files through `SemanticState`.
3. Hands FCS the same reference DLLs the semantic layer's `AssemblyEnv` was
   built from (see the reference-set note above), so neither side can resolve
   against an assembly the other cannot see.
4. Invokes `fcs-dump uses-project`.
5. Parses FCS ranges back to byte offsets using full path identity.
6. Compares every comparable FCS project declaration and assembly declaration
   against sema resolution.
7. Checks the reverse direction: every concrete sema resolution in a comparable
   file must be covered by an FCS use — except where the occurrence sits in
   *binding* position, which is the one place the oracle is free to say nothing.
   Those are counted rather than reported, in two separate buckets so neither
   count's meaning shifts under the other:
   - `unoracled_definitions` — our own defining occurrences at ranges FCS
     reports nothing about; the forward direction does not grade FCS's
     definitions either.
   - `unoracled_or_pattern_aliases` — a later or-pattern alternative's spelling
     of a name the first alternative binds. An or-pattern binds one name once,
     so `| Ldarg _n | Ldarga _n | …` makes the second `_n` a use of the first;
     FCS reports that for an ordinary name but **not** for one starting with
     `_`. Silence from the oracle is not a contradiction.

   "The oracle spoke here" is an **exact** span match, not enclosure: FCS
   synthesises an `_arg1` symbol spanning the whole of a non-simple lambda
   parameter, so in `fun (A _n | B _n) -> _n` every occurrence inside the
   pattern is enclosed by a use of an unrelated symbol. A reported divergence
   still lists every *overlapping* oracle use, which is what makes it
   diagnosable.

### Signature files

A `.fsi` Compile item is loaded like any other. Sema folds it into an inert
slot carrying its screen and exported surface (`resolve_project`), so it records
no resolutions of its own: every FCS use *inside* a `.fsi` is a deferral, never
a divergence, and a heavily signatured project therefore reads a lower coverage
percentage without that indicating a fault. What the signature work is actually
gated on is the **implementation** side — a cross-file use of a signature-exposed
`val` resolves to the `.fsi` ident (`docs/fsi-signature-restriction-plan.md`
conclusion 4: provenance = impl, def = sig), so it is compared against an FCS
declaration in the `.fsi`.

### Where FCS says a symbol is declared

A declaration outside the project's Compile set is **normal**, not a load
failure, and `UseDecl` records which case applies:

- `InProject` — a declaration in one of the project's own sources; the only
  form the project-declaration comparison can adjudicate.
- `Unlocated` — no declaration range, or one of FCS's pseudo-file sentinels
  (`startup`, `unknown`, `commandLineArgs`). `rangeStartup` is the range of the
  initial type-check environment, so *every* symbol imported from a referenced
  assembly declares "at startup".
- `OutsideProject` — a real file the project does not compile. An F# assembly
  carries its original source ranges in its signature data, so FSharp.Core's
  symbols declare at the build machine's paths
  (`D:\a\_work\1\s\src\fsharp\src\FSharp.Core\prim-types.fsi`).

The latter two are adjudicated by assembly identity, exactly as a missing
declaration range already was; only a use with neither an in-project
declaration nor an assembly identity lands in a skipped bucket.

Assembly *names* are compared up to corelib facade↔implementation
equivalence (`System.Private.CoreLib` is the same assembly as
`System.Runtime`: which one FCS reports depends on whether the `fcs-dump`
driving the run is framework-dependent or self-contained, while our side always
reads the ref-pack facade). Assembly full names are compared modulo FCS's
double-backtick *quoting* (``Operators.``not``` — delimiter pairs only, since a
quoted identifier may itself contain a lone backtick). FCS reports an F# *module*'s
`FullName` as the bare display name (`Seq`), which cannot witness which symbol
was bound; `fcs-dump` qualifies such a name from the entity's own `AccessPath`
(`Microsoft.FSharp.Collections.Seq`) before it reaches any consumer, so the
comparison here stays exact.

One more difference is normalised, and how says something about the currency.
FCS's full name for a member **renders** its enclosing type with type arguments
— `MethodReturnType<_>.Returns`,
`ImmutableArray<WoofWare.PawPrint.ConcreteTypeHandle>.Empty` — while our full
names carry no arity at all, so a correct resolution scored as a divergence in
both directions.

That decoration is not parsed back off. Its arguments carry commas that are not
separators (`ImmutableArray<Probe.A,B>` is *one* argument, of the type
``A,B`` — FCS drops the quoting) and `>`s that close nothing (an F# function
argument renders `(int -> string)`); both are measured, and either read as the
wrong arity. So `fcs-dump` emits the enclosing entity **structurally** beside
the rendering — `DeclaringFullName` (``Probe.Holder`1``) and
`DeclaringGenericArity` — and the comparison uses those.

The oracle's structural declaration is accepted only where our own resolution
certifies it (`certified_expected`): the enclosing chain we resolved must hold
an entity of that full name, that entity must not be a module (a module is never
generic), and its generic parameter count must equal the reported arity. The
chain rather than just the entity we resolved, because the declaring entity is
often an *encloser* — a union case carrying a field is a type nested in its
union. Without the arity check the acceptance would launder a wrong answer:
`Holder<'T>` and `Holder<'T,'U>` share a full name, as do a type and its
companion module. A use with no declaring entity is compared exactly as it
arrives. The structural fields themselves are pinned against the real oracle by
`crates/sema/tests/all/companion_head_diff.rs`.

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

- uncertain MSBuild compile items or define constants, including the first
  captured import/SDK/item/condition diagnostics that made them untrustworthy;
- projects over `BORZOI_PROJECT_MAX_FILES`;
- missing semantic project data;
- an **uncacheable reference set** — a transient C# sidecar transport failure
  means the LSP caches nothing, so the oracle's references and the env the fold
  resolves against would come from two separate resolutions that may differ;
  comparing across them is evidence of nothing either way;
- FCS invocation or JSON parse failures;
- FCS error diagnostics in one or more files. The reason quotes the leading
  errors with their sites — one per file before any file's second, so the
  diagnostic that names the cause is not crowded out by a noisier file — and
  counts the rest. A bare count of erroring files names no cause: the missing
  project reference above presented as 8473 errors across 113 files, whose
  *first* line said "The type 'DumpedAssembly' is not defined".

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
zero-width uses, non-project declarations, out-of-project declarations, and
oracle uses without declarations are counted separately under skipped uses,
alongside the count of our own defining occurrences the oracle said nothing
about.

The project section also reports the skipped-project rate. The JSON report
contains the same summary in a machine-readable form for ratchets or dashboards.
