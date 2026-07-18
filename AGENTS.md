This is an LSP for F#.
It's intentionally scoped tightly to what I think LLM agents might want, rather than e.g. for editor use.

You should find the F# compiler checked out in ../fsharp ; shout if it's not there and you need the F# compiler's source.

## Workspace layout

This is a Cargo workspace with nine members:

- `crates/cst/` — `borzoi-cst`. The lexer, lex-filter (offside rewriter),
  recursive-descent parser, and rowan-backed CST/AST. Self-contained: depends
  only on `logos` and `rowan`. Intended to be reusable outside this repo.
- `crates/msbuild/` — `borzoi-msbuild`. MSBuild `.fsproj` parser /
  evaluator: extracts the Compile order, expands `$(…)` substitution, evaluates
  the condition subset, detects/follows `Directory.Build.props|targets`,
  and — when an SDK resolves to the canonical dotnet layout — walks the
  real `Microsoft.Common.props` → `NuGet.props` chain, including the
  `Directory.Packages.props` central-package import.
  Self-contained: depends only on `roxmltree`. Intended to be reusable outside
  this repo. The evaluator is differentially tested against the
  real MSBuild evaluator via `tools/msbuild-condition-oracle` — a test-only
  JSONL batch oracle (in the `tools/fcs-dump` / `tools/nuget-oracle` mould)
  that evaluates in-process through the MSBuild API; it is never a
  runtime dependency of the LSP. Reached from the tests through the crate's
  `test-support`-feature `test_support` module. It has four ops, and the
  distinction matters: `eval` (a `Condition` string) and `expand` (a property
  *body*) both take their input *after* MSBuild's XML layer has run, so they
  are structurally blind to it; `project` hands MSBuild a whole document
  verbatim and reads back the evaluated property table, which is the only way
  to diff the XML layer itself (insignificant whitespace, entity decoding,
  CDATA, comment-split text); `items` is `project`'s item-side twin, reading
  back an item type's evaluated `FullPath`s (what `-getItem:` reports, but
  resident — a per-case `dotnet msbuild` pays .NET startup every time, which a
  generative sweep cannot afford). The four differentials that ride on them:
  `condition_diff.rs`, `property_expr_diff.rs`,
  `fsproj_property_table_diff.rs`, and
  `fsproj_item_escape_generative_diff.rs` (item specs over an escape-bearing
  alphabet — the harness that guards MSBuild's escaped-value domain, where the
  rule is *scan and split before you decode; trim in the domain; decode at the
  leaf*; see `docs/msbuild-escaped-value-plan.md`). All assert
  *certain-implies-exact*: when we commit a value, MSBuild must agree exactly;
  a decline makes no claim.
- `crates/assembly/` — `borzoi-assembly`. F#-flavoured reader for
  ECMA-335 assemblies: owned `Entity`/`Member`/`TypeRef` model, the
  `EcmaView` trait, and a projector over an *in-crate* ECMA-335 reader
  (no `dotnetdll` — it is GPL). Self-contained: depends only on `flate2`
  (pure-Rust deflate for F# compressed signature resources) and `sha1`
  (PublicKeyToken derivation). Intended to be reusable outside this repo.
  A single undecodable member or type is dropped and recorded rather than
  sinking the whole assembly (`Entity::skipped_members` /
  `EcmaView::enumerate_type_defs_with_skips`), so a modern BCL DLL
  projects all but the handful of types it genuinely cannot represent.
  Ships a `test-support` feature exposing the differential-test normaliser
  (`test_support` module) for callers that diff this projection against
  another (e.g. the C# sidecar's Roslyn-emitted metadata), and the
  `modifier_metamorphic` probe: ECMA-335 II.7.1.1's `modopt`/`modreq` rule
  checked as a *metamorphic property* over real assemblies (decorate every
  signature node with an unrecognised modifier and re-project — a `modopt`
  must move nothing, a `modreq` must leave no member standing). Any guard in
  the projector that inspects the *head* of a signature (`matches!(sig,
  TypeSig::ByRef(_))`) silently stops firing when a modifier sits in front of
  it, and the compiler cannot catch that; the probe can, so read it before
  writing such a guard.
- `crates/nuget/` — `borzoi-nuget`. NuGet primitives for the in-house
  warm-cache restore (`docs/nuget-restore-plan.md`): the `NuGetVersion` /
  `VersionRange` / `NuGetFramework` models, global-packages-folder and
  `.nuspec` reading, the conservative offline resolver, and compile-asset
  selection (NuGet's content model). Everything either resolves *identically*
  to `dotnet restore` or declines. Self-contained: depends only on `roxmltree`.
  Differentially tested against the real NuGet client libraries via
  `tools/nuget-oracle` — a test-only JSONL batch oracle in the same mould as
  `tools/fcs-dump`; it is never a runtime dependency of the LSP.
- `crates/spawn/` — `borzoi-spawn`. The one place in the workspace a child
  process is spawned: the process-global spawn lock (macOS leaks raw pipe
  descriptors between concurrent spawns, which hangs the reader of an *exited*
  child — see the crate docs), and `BoundedCommand`, which runs a child under a
  deadline with its stdin streamed and both output pipes drained. Everything that
  shells out goes through it — the LSP library, every test harness, the oracle
  drivers — because a per-crate lock is not an exclusion. `clippy.toml` bans
  direct `Command::{spawn,status,output}` to enforce that. No dependencies.
- `crates/oracle-harness/` — `borzoi-oracle-harness`. Test-only. `BatchChild`:
  a *resident* oracle (`fcs-dump …-batch`, `msbuild-condition-oracle`,
  `nuget-oracle`) driven as a lock-step request/response loop, so .NET startup is
  paid once per test binary rather than once per case. A wedged or crashed oracle
  is killed, respawned and the request retried, then panics — bounded and loud
  beats silent and forever. Builds on `borzoi-spawn`.
- `crates/sema/` — `borzoi-sema`. Semantic analysis (name resolution
  today, type inference later) over the `borzoi-cst` AST. `resolve_file`
  builds a position-ordered scope tree and resolves each name use to its
  defining binder; `resolve_project` folds that over the Compile order,
  threading each file's exports forward; `AssemblyEnv` resolves
  fully-qualified paths into referenced assemblies via the
  `borzoi-assembly` entity model. Depends on `borzoi-cst` and
  `borzoi-assembly`. Differentially tested against FCS; consumed by the
  LSP at runtime (`crates/lsp/src/semantic.rs`).
- `crates/lsp/` — `borzoi` (the package id is just `borzoi`, *not*
  `borzoi-lsp`; use `cargo test -p borzoi`). The LSP server (binary + library):
  diagnostics, project-assets resolution, the C# sidecar. Depends on
  `borzoi-cst` for tokens/parse trees and `borzoi-sema` for name
  resolution. Consumes `borzoi-msbuild`
  for `.fsproj` evaluation *at runtime* — every per-file lookup evaluates
  the owning `.fsproj` via `parse_fsproj_with_imports` (SDK + glob
  resolvers wired). The evaluation's `define_constants` and diagnostics,
  its ordered Compile `items` (the semantic layer's fold order), and its
  `project_references` (the inter-project graph: `.fsproj` diagnostics and
  the assembly env's reference edges) are all consumed
  (`docs/fsproj-consumption-plan.md` tracks the remaining consumers).
  `borzoi-assembly` is runtime-live too: referenced assemblies are
  read into each project's `AssemblyEnv` (`crates/lsp/src/semantic.rs`).
- `crates/corpus-diff/` — `borzoi-corpus-diff`. Test-only, unpublished: an
  empirical whole-project name-resolution differential. It loads real F#
  projects through the same runtime path the LSP uses (`Workspace` +
  project-assets closure + the sema fold), asks FCS for symbol uses, and
  compares the two — without letting skipped or erroring projects masquerade as
  agreement. Depends on `borzoi`, `borzoi-msbuild`,
  `borzoi-sema`, and `borzoi-spawn` (the `fcs-dump` driver lives in
  its library, not its tests, so it is a regular dependency); driven by
  `docs/project-corpus-diff-runner.md`.

Workspace-level files (`tools/`, `docs/`, `flake.nix`) sit at
the repo root. The LSP is the only crate with a binary, so `cargo run` from the
root still launches it.

## Differential testing against FCS (parser & lex-filter)

The parser and lex-filter are checked for byte-faithful behaviour against the
real F# Compiler Service by *differential* tests living in `crates/cst/tests/all/`:

- **What the parser diff tests.** FCS is asked for its `ParsedInput` via
  `tools/fcs-dump ast`; our parser produces a rowan CST/AST. Both are projected
  to the shared `crates/cst/tests/all/common/normalised_ast/` model and compared
  with `assert_eq!`. This tests the *modelled syntactic structure* and the
  caller-selected parse-status expectation (both clean, both erroring, or a
  known acceptance gap). The normaliser deliberately elides trivia and most FCS
  details/ranges; broad module/decl ranges are separately audited in the corpus
  sweep. The lex-filter diff compares normalised token streams after FCS's
  `UseLexFilter` against our offside rewriter.
- **Per-construct tests** — `parser_diff_*.rs` (e.g. `parser_diff_module_structure.rs`
  for members/types, `parser_diff_match.rs`, `parser_diff_let_bindings.rs`, …).
  Each is a `#[test]` calling a helper from `crates/cst/tests/all/common/mod.rs`:
  - `assert_asts_match(src)` — parse `src` with both our parser and FCS, project
    each to a shared *normalised AST*, assert equal **and** that neither side
    reported errors. The normaliser is the comparison currency: it deliberately
    *elides* trivia, ranges, and detail FCS keeps but we don't model yet, so a
    diff test passes as long as the modelled structure matches. To extend it
    when a new field becomes significant, edit the projections in
    `crates/cst/tests/all/common/normalised_ast/`: `model.rs` (the data types),
    `from_cst.rs` (our `Parse` → normalised), `from_fcs.rs` (FCS JSON → normalised).
  - `assert_asts_match_allow_errors(src)` — same, but permits parse errors on
    both sides (for recovery cases like illegal-but-recovered input).
  - `assert_sig_asts_match(src)` — the `.fsi` signature-file counterpart.
  - `assert_asts_match_with_defines(src, &["FOO"])` — with `#if` symbols defined.
- **Whole-corpus sweep** — `parser_corpus_diff.rs` (and `lexfilter_corpus.rs` for
  the lex-filter) walk a real F# source tree (the `BORZOI_CORPUS` env var,
  pinned to the `fsharp-src` flake input under `nix develop`) and diff every
  `.fs`/`.fsi`. These are `#[ignore]`d by default.
- **Categorised parser report** — `fcs_divergence.rs` is an ignored report
  generator over the same corpus. It writes one file per bucket under
  `fcs-divergence/` (or `BORZOI_DIVERGENCE_OUT`): both reject,
  we-reject/FCS-accepts, we-accept/FCS-rejects, clean-but-AST-divergent,
  unmodelled, parser panics, and other batch/IO failures. Use it when you need
  the full worklist rather than the ratcheted corpus gate's sample output.

Common commands:

Like `sema`, `assembly` and `lsp`, this crate has a *single* integration-test
binary, `--test all` (`tests/all/main.rs`), whose submodules are the case groups
— see "One test binary per crate" below. Select a group with a `module::` filter
rather than `--test <group>`.

```sh
# Run one per-construct parser diff case group, optionally filtering to a test.
nix develop -c cargo test -p borzoi-cst --test all parser_diff_let_bindings:: -- --nocapture
nix develop -c cargo test -p borzoi-cst --test all parser_diff_let_bindings::diff_ast_name -- --nocapture

# Run the full parser-vs-FCS corpus gate.
nix develop -c cargo test -p borzoi-cst --test all parser_corpus_diff:: -- --ignored --nocapture

# Regenerate the categorised parser divergence worklist.
BORZOI_DIVERGENCE_OUT=target/fcs-divergence \
nix develop -c cargo test -p borzoi-cst --test all fcs_divergence:: -- --ignored --nocapture

# Run the lex-filter corpus gate.
nix develop -c cargo test -p borzoi-cst --test all lexfilter_corpus:: -- --ignored --nocapture
```

FCS itself is invoked through `tools/fcs-dump/` (an F# program built on demand
via `dotnet build -c Release`; the first test run builds it). It serialises FCS's
`ParsedInput`/token streams to JSON; the Rust harness drives a *long-lived*
`fcs-dump …-batch` child over stdin/stdout to amortise .NET startup (so don't
switch the runner to cargo-nextest — separate test binaries defeat the batching).
Override the binary with `BORZOI_FCS_DUMP=/path/to/fcs-dump`. To inspect
the FCS side directly:

```sh
nix develop -c dotnet build tools/fcs-dump/fcs-dump.fsproj -c Release
tools/fcs-dump/bin/Release/net10.0/fcs-dump ast path/to/file.fs
printf '%s\n' path/to/file.fs | tools/fcs-dump/bin/Release/net10.0/fcs-dump ast-batch
tools/fcs-dump/bin/Release/net10.0/fcs-dump tokens-filtered path/to/file.fs
```

Workflow to add a parser feature: write the failing `assert_asts_match` test(s)
first, confirm they fail (our side errors or diverges), implement, re-run. To see
*why* a case diverges, dump our side with
`borzoi_cst::parser::parse(src).root` (`{:#?}`) and the filtered token stream
with `borzoi_cst::lexfilter::filter(src, borzoi_cst::lexer::lex(src))`.

## One test binary per crate

`cst`, `sema`, `assembly` and `lsp` each have exactly one integration-test
target: `tests/all/main.rs`, whose submodules are the case groups. Cargo
compiles and links every `tests/*.rs` as its own crate, so the old layout —
76 test binaries in `cst` alone — relinked all of them for a one-line change to
`src/`, and each FCS-driving one spawned its own `fcs-dump`. One binary pays
both costs once.

So: **add a case group as `tests/all/<group>.rs` plus a `mod <group>;` line in
`main.rs`**, not as a new `tests/*.rs`. Run one group with
`--test all <group>::`; `--test <group>` no longer resolves.

Forgetting the `mod` line is not a compile error — the file is simply not part
of the crate, so it is never compiled, its tests never run, and the suite stays
green. Putting the group outside `tests/all/` is the mirror image: Cargo
auto-discovers `tests/<group>.rs` *and* `tests/<group>/main.rs` as test
*targets*, so it runs — as a second binary with its own oracle child, which is
the cost the fold exists to avoid. Each binary therefore has an
`all_case_groups_are_declared` test (`oracle_harness::module_tree`) that fails,
naming the file, on either: an undeclared group at any depth (`main.rs`, and
each group's `mod.rs` in turn), or a stray test target beside `all/`.

Two things the single binary made sharp, both handled in
`borzoi_oracle_harness`, and worth knowing before you add to a harness:

- **The panic hook is process-global.** Sweeps that expect panics must silence
  them with `panic_silence::silence_panics_here()` (a per-thread guard), never
  by swapping the hook: concurrent swaps can leave the silent hook installed for
  good and swallow a *real* failure's message.
- **A resident oracle child cannot serve concurrent callers** — its requests and
  responses are matched positionally. One `BatchChild` behind a mutex serialises
  the whole crate's suite, so `cst` drives a pool of them. Note the cap is a
  process-wide budget, not per-pool.

## Comments

Comments describe the code's *current* state, not the history of how it got
there. Avoid phrasing framed as a delta from a previous version — "no longer a
submodule", "rather than the old X", "dropped Y because…". The diff and commit
message already record what changed; a comment that narrates the change
becomes stale noise the moment the next change lands. State the present
rationale instead (e.g. "`contents: read` covers `actions/checkout`", not
"dropped `pull-requests: read` because we removed dorny").

## Before you push

Iterate with `cargo test -p <the crate you touched>` — the workspace-wide sweep
is a pre-push gate, not an inner loop. A change to `cst` cascades to `sema` and
`lsp` (both depend on it), so those are usually the "affected crates".

Then commit to a non-`main` branch and run the full gate: `cargo fmt`,
`cargo clippy`, `cargo test`, and
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` (each runs on
the whole workspace by default; CI gates on doc warnings, so doc-link breakages
must be caught locally).

Note `oracle-harness`'s `batch_recovers_from_a_transient_wedge` hardcodes a
300 ms child deadline and flakes on a loaded machine (e.g. sibling worktrees
building concurrently). If it is the *only* failure, check `uptime` before
blaming your diff — it fails on `main` too under load.
