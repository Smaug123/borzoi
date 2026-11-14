This is an LSP for F#.
It's intentionally scoped tightly to what I think LLM agents might want, rather than e.g. for editor use.

You should find the F# compiler checked out in ../fsharp ; shout if it's not there and you need the F# compiler's source.

## Workspace layout

This is a Cargo workspace with nine members:

- `crates/cst/` — `borzoi-cst`. The lexer, lex-filter (offside rewriter),
  recursive-descent parser, and the CST/AST: a flat preorder table per file
  (`docs/flat-syntax-tree-plan.md`) behind rowan's red-tree API, written
  directly by the parser. Its tests turn on the `rowan-reference` feature,
  under which every parse also builds rowan's green tree and asserts the two
  trees are the same, and `token-reference`, under which every lex asserts
  that the raw token table rebuilds the full-trivia driver iterator's stream
  (`docs/token-pipeline-soa-plan.md`). Self-contained: depends only on `logos` and
  `text-size` (and, behind that test-only feature, `rowan`). Intended to be
  reusable outside this repo, through its *substrate*: the untyped tree
  (`SyntaxNode`, `SyntaxKind`, `AstNode`) and the version gate
  (`syntax::projection`), and through one typed facade per language version,
  `syntax::{v4_6, v4_7, v5 … v11, preview}`, each behind the cargo feature of
  its name (`unstable-preview` for `preview`) and read through
  `Parse::view::<syntax::vN::ParsedInput>()`. A facade owns every node type
  and enum of its version's surface; `tools/astgen` generates each from its
  lock (`crates/cst/facades/vN.lock`), and a version whose surface equals an
  earlier one's is an alias of it. An unpublished facade's lock (`v11`,
  `preview`) is derived from the union's and regenerated freely. The facades
  up to `v10` are **published**: frozen, their locks astgen's input rather
  than its output, changing only by the additions
  `tools/ci/facade-lock-check.sh` allows, each distinct one with a frozen
  consumer fixture (`tests/all/facade_consumers/`) that must keep compiling.
  A union accessor or derive added later is opt-in for a frozen facade, but
  a union change that would break one or open a hole in it fails astgen until
  the lock takes it or `ACCEPTED_DIVERGENCES` records why not; see "Publishing a
  facade" in `docs/ast-versioning-plan.md`, which is also the runbook for
  publishing a new language version's facade. The
  typed AST the facades re-cast — the *union* of every language version's
  surface (`ImplFile`, `Expr`, `Pat`, `Type`, `ModuleDecl`, …, in
  `syntax/union.rs` and `syntax/generated/`) — gains a variant whenever a
  construct is modelled, so it is exported, at unchanged `syntax::` paths, only
  under the `unstable-union` feature. `borzoi-sema`, the LSP,
  `borzoi-corpus-diff` and cst's own tests turn it on, and sema and the LSP
  turn on no facade; workspace builds therefore always have the union, so CI's
  lint job also builds, lints and documents the crate without it.
  `tests/all/facade_properties` holds every facade's node types, enums and
  accessors to the union's (P2–P4 of `docs/ast-versioning-plan.md`) over a
  table astgen generates from the locks.
- `crates/msbuild/` — `borzoi-msbuild`. MSBuild `.fsproj` parser /
  evaluator: extracts the Compile order, expands `$(…)` substitution, evaluates
  the condition subset, detects/follows `Directory.Build.props|targets`,
  and — when an SDK resolves to the canonical dotnet layout — walks the
  real `Microsoft.Common.props` → `NuGet.props` chain, including the
  `Directory.Packages.props` central-package import.
  Self-contained: depends only on `roxmltree` and `self_cell` (which holds a
  parsed document beside its text, so the crate forbids `unsafe`). Intended to
  be reusable outside this repo. Every file-system query an evaluation makes
  goes through `observed`, which records it in the result's `read_set` (absences
  included); the crate's own `clippy.toml` bans the `std` calls elsewhere, and
  test code reaches them through `observed::host_fs`. A resolver the caller
  passes must query through the `Observer` it is handed. The read set is held
  to `observed::soundness::sweep` (perturb each file; a read set that still
  validates must mean an unchanged evaluation), which every
  `with_imports_tests` helper runs over its test's tree, so a new reader that
  skips the observer fails there or in clippy. An `Observer` asks each query
  once per evaluation, and `parse_fsproj_with_imports_reusing` reuses an
  earlier evaluation's parse of an imported file whose bytes, read again, are
  the same (`observed::ImportDocuments`); `observed::reuse` holds both to the
  reference that asks every time (`Observer::asking_every_time`) at every step
  of every helper's sweep and on every integration test's evaluation
  (`tests/common`'s `parse_fsproj_with_imports`). The evaluator is
  differentially tested against the
  real MSBuild evaluator via `tools/msbuild-condition-oracle` — a test-only
  JSONL batch oracle (in the `tools/fcs-dump` / `tools/nuget-oracle` mould)
  that evaluates in-process through the MSBuild API; it is never a
  runtime dependency of the LSP. Reached from the tests through the crate's
  `test-support`-feature `test_support` module. It has four ops, and the
  distinction matters: `eval` (a `Condition` string) and `expand` (a property
  *body*) both take their input *after* MSBuild's XML layer has run, so they
  are structurally blind to it. Their stubs write a tab, line feed or
  carriage return as a character reference, so MSBuild reads each as itself,
  as it does in a project file (it normalises neither attribute values nor
  line ends); `condition_diff` and `property_expr_diff` each grade their
  evaluator on such text with a whitespace axis. `project` hands MSBuild a
  whole document verbatim and reads back the evaluated property table, which is the only way
  to diff the XML layer itself (insignificant whitespace, entity decoding,
  CDATA, comment-split text); `items` is `project`'s item-side twin, reading
  back an item type's evaluated `FullPath`s (what `-getItem:` reports, but
  resident — a per-case `dotnet msbuild` pays .NET startup every time, which a
  generative sweep cannot afford). `project` and `items` also carry the
  evaluation's **global properties** — the `-p:` set, which is *not* a
  property-group write: it is read-only to the document, outranking every write
  of that name unless `TreatAsLocalProperty` opts out. The five differentials
  that ride on these ops:
  `condition_diff.rs`, `property_expr_diff.rs`,
  `fsproj_property_table_diff.rs`,
  `fsproj_item_escape_generative_diff.rs` (item specs over an escape-bearing
  alphabet — the harness that guards MSBuild's escaped-value domain, where the
  rule is *scan and split before you decode; trim in the domain; decode at the
  leaf*; see `docs/completed/msbuild-escaped-value-plan.md`), and
  `fsproj_global_perturbation_diff.rs`. All assert
  *certain-implies-exact*: when we commit a value, MSBuild must agree exactly;
  a decline makes no claim.
  The last one exists because the other four are structurally blind to one
  defect class. They evaluate both sides under the *same* globals, so a value
  whose evaluation depends on a global the caller supplied — through a route we
  do not model — agrees with MSBuild exactly and passes every one of them. The
  LSP injects `Configuration=Debug, Platform=AnyCPU` as a guess and publishes
  what comes back as fixed fact, so that class is live. Sweeping the global set
  turns it back into an ordinary certain-implies-exact violation; no new
  contract was needed, only a harness that varies what the others hold fixed.
  It runs the corner list of routes (gates, `<Choose>` arms, property-selected
  imports, `TreatAsLocalProperty`), a generated sweep, the Compile item set, and
  the **real SDK chain**, and prints a movement census — how many values MSBuild
  moves across the sweep, and for each whether we tracked it, declined it as
  untrusted, or never reached it. Read that census before investing in a
  "depends on a caller-supplied global" provenance dimension: today the
  evaluator is sound on this axis and pays for it in declines
  (`OutDir`/`OutputPath`/`DefineConstants` are untrusted under every global set
  on a real SDK project), which makes such a dimension a coverage investment,
  not a soundness fix.
  Three ops execute rather than evaluate. `targetItems` runs one target on an
  evaluation and reads back an item list: `fsproj_compile_order_generative_diff.rs`
  runs F#'s `FSharpSourceCodeCompileOrder` (its text lifted from the installed
  SDK into a synthetic SDK) and holds the compile order we commit to
  certain-implies-exact, since the order the compiler sees is not the evaluated
  `@(Compile)`. `restoreCone`
  builds `GenerateRestoreGraphFile` in-process and reports every target and
  task that ran, with file hashes and task-assembly identities.
  `restore_cone_census.rs` pins that cone per project and per installed SDK
  (stage 8 of `docs/nuget-dgspec-boundary-plan.md`), under the global
  properties `borzoi_oracle_harness::restore_globals` holds to `restore.rs`'s
  command line.
  `restoreCone` also returns the `_RestoreGraphEntry` items
  `WriteRestoreGraphTask` was handed, logged as its input. The census
  puts each cone's entries through `borzoi_nuget`'s `write_restore_graph`
  and pins the item seam's verdict on each cone of SDK targets alone: the
  cone's own spec, byte for byte, or a decline.
  It also reads every property and item type the cone's targets reference,
  in two places. One is the end of our evaluation, through a probe on the walk
  (`test_support::parse_fsproj_reads`), which must leave the `ParsedProject`
  unchanged. The other is the end of MSBuild's, through the `evaluated` op,
  which evaluates without building. A trusted value MSBuild disagrees with
  panics. Every other verdict (unset, inexact, untrusted, an item table's
  identities and taint, and their metadata) is pinned in
  `restore_cone_reads_*.txt`. That is the worklist for A₁.3's executor.
  The probe also answers where each target the cone ran is defined, and an
  exact answer that is not the definition MSBuild's target table keeps
  panics. `fsproj_target_definitions_generative_diff` holds those answers to
  the same table on generated import graphs.
  `fsproj_target_execution_generative_diff` holds A₁.3c's executor (the
  probe's `execute`) to the oracle's `runTargets` op, which builds targets
  in-process and reports the targets that started and the properties and
  items left; the census pins where the executor stops on each cone.
  `fsproj_generic_items_generative_diff` holds the generic item tables the
  probe reads, metadata included, to the `itemsMeta` op on generated
  projects.
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
  must move nothing but the member's `drops_optional_modifier` record, a
  `modreq` must leave no member standing). Any guard in
  the projector that inspects the *head* of a signature (`matches!(sig,
  TypeSig::ByRef(_))`) silently stops firing when a modifier sits in front of
  it, and the compiler cannot catch that; the probe can, so read it before
  writing such a guard.
  A *whole-assembly* skip is a different grade of failure, gated by
  `projection_skip_sweep` (`#[ignore]`d, `BORZOI_DLL_SWEEP_ROOT`): with an
  assembly's types missing, no reading into any *other* assembly is provably
  unshadowed, since the missing one could declare a colliding type or an
  assembly-level `[<AutoOpen>]`. So the reader localizes wherever it can — a
  `TypeRef` whose resolution scope is outside the modelled subset rides on the
  row (`Result<RefScope, UnsupportedScope>`) and costs only the types that walk
  out through it. The sweep classifies each loss **by cause, not by path**:
  every `.dll`/`.exe`/`.winmd` under the roots is probed, and any loss whose
  cause is not on the sweep's short `EXEMPT` list fails the gate. Filtering to
  "what NuGet could select as a compile asset" instead would mean rebuilding
  `borzoi_nuget::assets` here, and an approximation that wrongly *excludes* a
  file hides exactly the regression the gate exists to catch.
  **net4x is an unsupported target**, so the two targeting-pack shapes the
  reader refuses whole (a netmodule, and an `mscorlib` whose manifest resource
  is linked rather than embedded) are exempt rather than tracked work.
  Unsupported means "will not be invested in", not "refused" — nothing declines
  a `net472` project today, so a user who opens one gets less of it. Nothing
  follows for the TFM vocabulary: parsing `net472` and modelling NuGet's
  acceptance of `.NETFramework` are requirements of the MSBuild/NuGet
  differentials, which reproduce those tools regardless of what borzoi serves
  well. Run the sweep before widening any parse-time refusal:

  ```sh
  BORZOI_DLL_SWEEP_ROOT=~/.nuget/packages \
  nix develop -c cargo test -p borzoi-assembly --test all projection_skip_sweep:: \
    -- --ignored --nocapture
  ```
- `crates/nuget/` — `borzoi-nuget`. NuGet primitives for the in-house
  warm-cache restore (`docs/nuget-restore-plan.md`): the `NuGetVersion` /
  `VersionRange` / `NuGetFramework` models, global-packages-folder and
  `.nuspec` reading, the conservative offline resolver, and compile-asset
  selection (NuGet's content model), and the dependency-graph spec reader
  (`dgspec`, `docs/nuget-dgspec-boundary-plan.md`) and its resolver
  (`dgspec::resolve_dgspec`), and NuGet's settings as far as the global
  packages folder they choose (`settings::global_packages_folder`, which the
  LSP's on-demand restore pins its package folder to; graded by
  `settings_diff`). Everything either resolves
  *identically* to `dotnet restore` or declines. `resolve_dgspec` commits only
  for a NuGet on its verified list (`src/dgspec/verified_nuget.txt`), which
  `dgspec_resolve_diff` derives from the oracle, so a devshell SDK bump fails
  that test until the list is regenerated (the failure prints the command), and
  regenerating it is the claim that the differential has graded the new NuGet.
  It also takes the environment `restore.rs` spawns with
  (`restore::restore_environment`): NuGet reads variables while restoring, and
  an oracle child shares the test's environment, so only grading under each
  variable (`restore_is_graded_under_every_environment_b1_admits`) and scanning
  the verified NuGet for the names it reads makes that dependence visible.
  The LSP runs `resolve_dgspec` in shadow on every on-demand restore
  (`crates/lsp/src/restore/shadow.rs`): it serves the restore's answer and
  logs a B₁ commit that differs from it as an error, so a B₁ bug on a real
  project shows up in the server's log.
  The other side of the spec, the start of A₁, is
  `dgspec::write_restore_graph`. It takes the `_RestoreGraphEntry` items the
  restore targets produce and returns the file `WriteRestoreGraphTask` would
  save, as text. `restore_items_diff` holds it byte for byte to the task's
  own code, through the oracle's `restoreGraphItems` op. Its text is NuGet's
  writer output, which `parse_dgspec` then reads, so the writer's sorting,
  dropping and grouping are modelled once, as text, and never re-derived as
  values. It shares B₁'s verified list.
  Self-contained: depends only
  on `roxmltree` and `serde`/`serde_json`.
  Differentially tested against the real NuGet client libraries via
  `tools/nuget-oracle` — a test-only JSONL batch oracle in the same mould as
  `tools/fcs-dump`; it is never a runtime dependency of the LSP. The oracle
  runs the NuGet *of the SDK `dotnet` resolves*, loaded from that SDK's
  directory, not a package build: SDKs bundle unpublished release candidates,
  so no package version is the NuGet `dotnet restore` runs.
  `oracle_identity.rs` pins it.
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
  beats silent and forever. Builds on `borzoi-spawn`. Also `manifest`: the
  exact per-item pin a deterministic corpus gate compares its run against (a
  sorted line per item, a line diff on any movement, `BORZOI_UPDATE_MANIFESTS=1`
  to regenerate) — the replacement for one-sided count ratchets — and
  `corpus_key`, the host-independent spelling of a corpus file and position in
  an entry (Nix's macOS `~nix~case~hack~` suffix decoded). And `nuget_oracle`,
  the `tools/nuget-oracle` driver, which lives here because both
  `borzoi-nuget`'s differentials and the LSP's restore-graph oracle drive it.
  And `xml_pump`: every short element path of an XML input repeated thousands
  of levels deep, run on a 2 MiB stack. `roxmltree` (≥ 0.21.2) parses
  iteratively, so a deep file overflows only through a walk of ours that
  recurses per element; `borzoi-msbuild`'s `xml_nesting_pump` and
  `borzoi-nuget`'s `dgspec::xml_nesting_pump` hold every reader of untrusted
  XML to that without naming the recursive paths. A new reader belongs in one.
- `crates/sema/` — `borzoi-sema`. Semantic analysis (name resolution
  today, type inference later) over the `borzoi-cst` AST. `resolve_file`
  builds a position-ordered scope tree and resolves each name use to its
  defining binder; `resolve_project` folds that over the Compile order,
  threading each file's exports forward; `AssemblyEnv` resolves
  fully-qualified paths into referenced assemblies via the
  `borzoi-assembly` entity model. Depends on `borzoi-cst` and
  `borzoi-assembly`. Differentially tested against FCS; consumed by the
  LSP at runtime (`crates/lsp/src/semantic.rs`).
  The use differentials only see a binder the walk missed when some graded
  use sits in its scope with an outer target to commit to, so **binder
  completeness** checks the binders themselves
  (`test_support::binder_completeness`): every binder FCS declares — a
  definition record, or the declaration a use names — must be one the walk
  interns (a `Def`) or knows it declines (`ResolvedFile::opaque_binders`,
  `ResolvedFile::opaque_regions`). `tests/all/binder_site_grid.rs` is the
  closed table of F#'s binding constructs, each FCS-graded under an empty
  and a BCL environment with each row's status pinned; the FCS-graded grids,
  `resolve_corpus_diff` and the whole-project gate run the same check on
  every cell and file. A binding construct the walk does not know about
  starts as a row in that table, not a review finding.
  Inferred types are not bounded by the tree (each line of `let gK x =
  g(K-1) (g(K-1) x)` doubles one), so every walk of the unification table
  counts against a type budget (`MAX_TYPE_NODES`, `MAX_TYPE_DEPTH`), and a
  file whose inference reaches it publishes no inferred type at all
  (`InferredFile::type_budget_exceeded`). `tests/all/infer_type_budget.rs`
  runs the growth families under a memory, time and stack limit
  (`borzoi_oracle_harness::alloc_limit`, `stack_probe::run_case_limited`);
  `infer_corpus_diff` fails if real code comes within a factor of 8 of it.
  Resolution and inference recurse once per level of the tree, so what
  bounds their stack is the parser's `MAX_TREE_HEIGHT`: `WALK_STACK_BYTES`
  is the stack they need for any tree it returns, the LSP's request thread
  is spawned with it, and `tests/all/walk_stack_bound.rs` (and the LSP
  crate's twin, over the whole request loop) holds every repetition form of
  the grammar to half of it, each case in a child process. A change that
  fattens a frame on a walk's recursion fails there, naming the shape. A walk
  that declines at the top of a tree survives any stack, so how deep each
  walk reached into each case is pinned in
  `tests/manifests/walk_stack_bound.txt`.
- `crates/lsp/` — `borzoi` (the package id is just `borzoi`, *not*
  `borzoi-lsp`; use `cargo test -p borzoi`). The LSP server (binary + library):
  diagnostics, project-assets resolution, the C# sidecar (run only for a host
  that sets the `enableCSharpProjectReferences` initialization option, as
  on-demand restore needs `enableOnDemandRestore`: both execute the workspace's
  MSBuild targets). Depends on
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
  The crate observes the file system only through `crates/lsp/src/observed.rs`
  (`docs/incremental-lsp-plan.md`, D3): source text is owned by the `Vfs`
  (`crates/lsp/src/vfs.rs`), whose disk is `observed::HostDisk` and which
  counts its opens and `stat`s per generation of the dispatch loop, and every
  other read is a method of `Unobserved`, whose variant names the reader and
  the plan stage that will route it. The `Vfs` re-reads a disk file only when
  its stamp moves or is racy (git's rule, a 2 s window), and every cached fold
  is validated against it in each generation that reads it, so a closed
  source rewritten with no watch event is still seen
  (`incremental_state_machine`'s `WITNESSED`). `crates/lsp/clippy.toml` bans the `std` calls
  everywhere else; test code reaches them through `host_fs` (the lib's
  `observed::host_fs`, `tests/all/common/host_fs.rs`), never through a
  wholesale allow, which would also lift the spawn ban.
  Hover's XML documentation lives in `crates/lsp/src/xml_doc/`. Its
  `<inheritdoc>` work is held to Roslyn's IDE through
  `tools/inheritdoc-oracle`, a test-only C# JSONL batch tool (the
  `nuget-oracle` mould) that calls Roslyn's internal expansion by
  reflection, evaluates Roslyn's XPath selection, and compiles purpose-built
  C# fixtures (rewriting a method name or an `AssemblyRef` in one, for
  metadata no compiler writes) or emits assemblies from a description
  (System.Reflection.Metadata). `xml_doc_inheritdoc_xpath_diff` holds the
  XPath subset to certain-implies-exact against it; `xml_doc_inheritdoc_diff`
  (handwritten fixtures), `xml_doc_inheritdoc_cref_diff` (generated reference
  sets whose dimensions are the ways Roslyn's importer and declaration-ID
  lookup see metadata differently from the env's index, every documented ID
  cited by a `cref`), `xml_doc_inheritdoc_generated_diff` (generated C#),
  `xml_doc_inheritdoc_metadata_diff` (generated metadata: accessor names,
  reference versions/cultures/case, facades, core-library shapes, custom
  modifiers, `MethodImpl` rows, doc-file layouts — with a per-dimension
  census and liveness) — the generators run a few dozen cases each in
  the normal suite, hundreds in their ignored `deep_*` twins,
  `BORZOI_INHERITDOC_CASES` overriding them all — and the ignored
  `xml_doc_inheritdoc_sweep` (the ASP.NET Core pack, plus a NuGet cache under
  `BORZOI_INHERITDOC_SWEEP_ROOT`) hold every `<inheritdoc>` expansion to it.
  The oracle's `type-ids` op grades a type's own ID: `xml_doc_type_id_diff`
  emits rows whose `` `n `` suffix need not count their parameters (Roslyn
  then appends the count; fsc, whose side `borzoi-assembly`'s
  `doc_id_fsharp_diff` grades, does not) and holds every one to Roslyn's
  `GetDocumentationCommentId`.
  The expansion certifies only metadata shaped as C# writes it: every type it
  reads must pass `borzoi_assembly::shape::compiler_shape`, or it declines.
  So a disagreement with Roslyn over metadata outside that shape (an event's
  `fire` accessor, accessors named apart from their property, a `modopt`) is
  a bug in the predicate, which must reject it, not a Roslyn rule to model;
  and a review finding on such metadata that already declines is closed. The
  metadata generator asserts the decline for its out-of-domain dimensions
  without the oracle. What the predicate rejects in real packages is measured,
  not guessed: `compiler_shape::package_cache_shape_census` (ignored,
  `BORZOI_SHAPE_SWEEP_ROOT`) counts it by cause over a NuGet cache, and the
  reference pack and csc output must pass it whole.
- `crates/corpus-diff/` — `borzoi-corpus-diff`. Test-only, unpublished: an
  empirical whole-project name-resolution differential. It loads real F#
  projects through the same runtime path the LSP uses (`Workspace` +
  project-assets closure + the sema fold), asks FCS for symbol uses, and
  compares the two — without letting skipped or erroring projects masquerade as
  agreement. Each comparable project is then put to the real request handlers
  over the protocol (`handler_diff.rs`): definition, hover and references at
  the first, middle and last character of FCS's records, positions converted by
  an independent UTF-16 implementation, and again on a copy with non-ASCII text
  substituted into comments and strings, which FCS checks too. Depends on
  `borzoi`, `borzoi-cst`, `borzoi-msbuild`,
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
  `tools/fcs-dump ast`; our parser produces a CST/AST. Both are projected
  to the shared `crates/cst/tests/all/common/normalised_ast/` model and compared
  with `assert_eq!`. This tests the *modelled syntactic structure* and the
  caller-selected parse-status expectation (both clean, both erroring, or a
  known acceptance gap). The normaliser deliberately elides trivia and most FCS
  details/ranges; module/decl ranges, and the ranges of the names below them
  (type, union-case, field, member and `let` binder names), are separately
  audited (`common/range_audit.rs`). The lex-filter diff compares our offside
  rewriter's output with the stream FCS's parser reads, block ends included
  (`fcs-dump tokens-lexfilter-internal-batch`, read with
  `parse_fcs_parser_stream`).
- **What the currency must cover.** Eliding is only sound for detail no consumer
  reads. Every public accessor in `crates/cst/src/syntax/union.rs` opens with
  `accessor!("Type::method")`, which under the crate's `accessor-trace` feature
  (on for its own tests) records the call. `parser_corpus_diff` records what the
  projection reads on the files that match FCS and pins it in
  `tests/manifests/accessor_coverage.txt`; `accessor_coverage::
  consumers_read_only_compared_accessors` scans `crates/sema/src` and
  `crates/lsp/src` with `syn` and fails if they call an accessor the projection
  never compares. So when sema or the LSP starts reading a new accessor, project
  it (`normalised_ast/`), add a graded fixture (`common/accessor_coverage.rs`
  `FIXTURES`) if no corpus file holds the construct, or exempt it in
  `NOT_PROJECTED` with the reason FCS has nothing to compare it with. The scan
  matches calls by method name (it has no types), so it can over-ask; and it
  cannot see a consumer that walks raw `SyntaxNode`s.
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
  `.fs`/`.fsi`. These are `#[ignore]`d by default. `parser_corpus_diff` is a
  gate (see "Before you push"); `lexfilter_corpus` is not. It asserts nothing,
  printing a histogram of divergence buckets that points at the next LexFilter
  arm worth porting, and its FCS side costs ~1.65 s per file, well over an hour
  across the corpus. CI does not run it and it is not a pre-push step: run it
  when you are porting LexFilter arms.
- **Categorised parser report** — `fcs_divergence.rs` is an ignored report
  generator over the same corpus. It writes one file per bucket under
  `fcs-divergence/` (or `BORZOI_DIVERGENCE_OUT`): both reject,
  we-reject/FCS-accepts, we-accept/FCS-rejects, clean-but-AST-divergent,
  unmodelled, parser panics, and other batch/IO failures. Use it when you need
  the full worklist rather than the ratcheted corpus gate's sample output.
- **Recovered trees** — where either parser rejects a file, the corpus sweep
  also grades the two recovered trees against each other
  (`common/recovery.rs`): every declaration outside the damaged region (both
  sides' error spans) must match, key for key and shape for shape, and the
  verdict is pinned per file in the manifest. `recovery_sweep.rs` manufactures
  more broken input by deleting one token (or the member name after a `.`, the
  `foo.` shape completion runs on) from clean corpus files. Write a recovery
  case as `common::recovery::assert_recovered_trees_agree(src, n)`. See
  `docs/parser-recovery-oracle.md`, which also lists what remains divergent.
  Recovery is graded more weakly than clean input: losing a declaration FCS
  keeps, where `main` keeps it, blocks; keeping it in a different module than
  FCS does is pinned in a grid manifest and allowed, so answer such a review
  finding with a grid case (that doc's "What recovery must match").

Common commands:

Like `sema`, `assembly`, `nuget` and `lsp`, this crate has a *single* integration-test
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

# Print the lex-filter divergence histogram (not a gate; over an hour; not in CI).
nix develop -c cargo test -p borzoi-cst --test all lexfilter_corpus:: -- --ignored --nocapture
```

FCS itself is invoked through `tools/fcs-dump/` (an F# program built on demand
via `dotnet build -c Release`; the first test run builds it). It serialises FCS's
`ParsedInput`/token streams to JSON; the Rust harness drives a *long-lived*
`fcs-dump …-batch` child over stdin/stdout to amortise .NET startup (so don't
switch the runner to cargo-nextest — separate test binaries defeat the batching).
Override the binary with `BORZOI_FCS_DUMP=/path/to/fcs-dump`; it must be a
**framework-dependent** build (`dotnet publish --self-contained false`, or the
`dotnet build` output). FCS's script checks take their framework references from
the runtime fcs-dump runs on, so a self-contained bundle would check every file
against its own directory — implementation assemblies plus
FSharp.Compiler.Service itself — and fcs-dump refuses to start that way. To
inspect the FCS side directly:

```sh
nix develop -c dotnet build tools/fcs-dump/fcs-dump.fsproj -c Release
tools/fcs-dump/bin/Release/net10.0/fcs-dump ast path/to/file.fs
printf '%s\n' path/to/file.fs | tools/fcs-dump/bin/Release/net10.0/fcs-dump ast-batch
tools/fcs-dump/bin/Release/net10.0/fcs-dump tokens-filtered path/to/file.fs
printf '%s\n' path/to/file.fs | tools/fcs-dump/bin/Release/net10.0/fcs-dump tokens-lexfilter-internal-batch
```

`tokens-filtered` is FCS's public tokenizer, which drops every token without an
`FSharpTokenKind`, block ends among them; `tokens-lexfilter-internal-batch` is
the stream the parser reads (via reflection on FCS internals), and is what the
lex-filter differential compares against.

Workflow to add a parser feature: write the failing `assert_asts_match` test(s)
first, confirm they fail (our side errors or diverges), implement, re-run. To see
*why* a case diverges, dump our side with
`borzoi_cst::parser::parse(src).root` (`{:#?}`) and the filtered token stream
with `borzoi_cst::lexfilter::filter(src, borzoi_cst::lexer::lex(src))`.

## One test binary per crate

`cst`, `sema`, `assembly`, `nuget` and `lsp` each have exactly one integration-test
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
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --document-private-items`
(each runs on the whole workspace by default; CI gates on doc warnings, so
doc-link breakages must be caught locally).

`--document-private-items` is load-bearing: nearly every doc comment here hangs
off a private item, so without it rustdoc never resolves their intra-doc links
and a link to a since-renamed function stays green forever.

Workspace-wide commands unify features, so they always build `borzoi-cst` with
`unstable-union` on. After touching `crates/cst`, also run CI's featureless
pair: `cargo clippy -p borzoi-cst --lib -- -D warnings` and
`RUSTDOCFLAGS="-D warnings" cargo doc -p borzoi-cst --no-deps --document-private-items`.

Note `oracle-harness`'s `batch_recovers_from_a_transient_wedge` hardcodes a
300 ms child deadline and flakes on a loaded machine (e.g. sibling worktrees
building concurrently). If it is the *only* failure, check `uptime` before
blaming your diff — it fails on `main` too under load.

### The corpus sweeps gate on the pull request

`cargo test` does not run them: they are `#[ignore]`d because they walk the
whole pinned corpus. `ci.yml` runs them anyway, gated by the same change filters
as the crate they belong to, so a change to `cst` or `sema` will have its
ratchets checked whether or not you ran them locally. Run the affected ones
before pushing rather than discovering it in CI:

```sh
nix develop -c cargo test -p borzoi-cst  --test all parser_corpus::       -- --ignored  #  ~30 s
nix develop -c cargo test -p borzoi-cst  --test all parser_corpus_diff::  -- --ignored  # ~5.5 min
nix develop -c cargo test -p borzoi-cst  --test all parser_diff_feature_gates:: -- --ignored  # ~2 min
nix develop -c cargo test -p borzoi-cst  --test all version_dependence_census:: -- --ignored  # ~1.5 min
nix develop -c cargo test -p borzoi-cst  --test all parser_panic_sweep::fresh_seed -- --ignored  # ~10 s
nix develop -c cargo test -p borzoi-cst  --profile gate --test all layout_perturbation_sweep:: -- --ignored  # ~1.5 min
nix develop -c cargo test -p borzoi-cst  --test all recovery_sweep::       -- --ignored  # ~3 min
nix develop -c cargo test -p borzoi-cst  --test all parser_diff_decl_sequence_grid::every_declaration_pair_streams -- --ignored  # ~3 min
nix develop -c cargo test -p borzoi-cst  --test all flat_tree_reference::corpus -- --ignored  # ~2.5 min
nix develop -c cargo test -p borzoi-cst  --test all facade_properties::corpus -- --ignored  # ~4 min
nix develop -c cargo test -p borzoi-sema --test all resolve_corpus_diff:: -- --ignored  #  ~20 s
nix develop -c cargo test -p borzoi-sema --test all attr_resolution_sweep:: -- --ignored  #  ~25 s
nix develop -c cargo test -p borzoi-sema --test all silent_region_sweep:: -- --ignored  #  ~1.5 min
nix develop -c cargo test -p borzoi-sema --test all infer_corpus_diff:: -- --ignored    #  ~30 s
nix develop -c cargo test -p borzoi-sema --test all infer_corpus_dump:: -- --ignored    #  ~40 s
nix develop -c cargo test -p borzoi-sema --test all infer_incremental_diff::incremental_equals_batch_over_a_corpus_sample -- --ignored  #  ~40 s
nix develop -c cargo test -p borzoi-sema --test all auto_open_member_sweep:: -- --ignored  # ~10 min
nix develop -c cargo test -p borzoi      --test all parser_corpus_sweep:: -- --ignored  #  ~50 s
nix develop -c cargo test -p borzoi      --test all xml_doc_shipped_sweep:: -- --ignored  #  ~10 s
nix develop -c cargo test -p borzoi      --test all xml_doc_source_diff::corpus:: -- --ignored  #  ~20 s
nix develop -c cargo test -p borzoi      --test all -- --ignored xml_doc_inheritdoc_sweep:: xml_doc_inheritdoc_cref_diff::deep_ xml_doc_inheritdoc_generated_diff:: xml_doc_inheritdoc_metadata_diff::deep_ xml_doc_type_id_diff::deep_  #  ~4 min
nix develop -c cargo test -p borzoi-nuget --test all soak:: -- --ignored               #  ~15 s
nix develop -c cargo test -p borzoi-nuget --test all resolver_diff::randomised_soundness_soak -- --ignored  # ~50 s
nix develop -c cargo test -p borzoi-msbuild --test fsproj_msbuild_corpus_diff -- --ignored  # ~40 s
nix develop -c cargo test -p borzoi-msbuild --test sdk_chain_decline_attribution -- --ignored  # ~1 s
```

The cst and sema sweeps, `borzoi-assembly`'s reference-pack sweep (an
ordinary test rather than an ignored one), the MSBuild corpus differential, and
the whole-project gate
(`ci.yml`'s `corpus-diff` job) pin every graded *item*, not a count: each
compares its run against a checked-in manifest (`crates/<crate>/tests/manifests/`,
`crates/corpus-diff/manifests/`) and fails on any movement **in either
direction**, printing a line diff. So **improving the parser fails
`parser_corpus`**: parse one more file cleanly and it goes red until the
manifest records it. That is deliberate — the corpus is content-addressed and
the parser deterministic, so there is no drift for slack to absorb, and a
one-sided floor is exactly what rotted 2,401 files deep while nothing ran the
sweep. If every moved line is intended, regenerate with
`BORZOI_UPDATE_MANIFESTS=1` on the same command (the failure message prints it;
for the whole-project gate,
`BORZOI_UPDATE_MANIFESTS=1 bash tools/ci/project-corpus-gate.sh` regenerates
every manifest its steps check) and commit the manifest diff, so the reviewer
sees exactly which items moved. The hard gates (zero divergences, no parser
panic or round-trip failure, no dropped reference-pack type) are separate
assertions that regeneration cannot bless. `docs/continuous-measurements.md` ("Exact
manifests") has the format and the helpers: `borzoi_oracle_harness::manifest`,
and `borzoi_oracle_harness::corpus_key`, which spells a corpus file the same way
on macOS (where Nix renames case-colliding names) and on Linux CI.

A manifest whose every entry is keyed (`<key><TAB><value>`) — the
whole-project `project_corpus.txt`, and the `parser_corpus`,
`parser_corpus_diff`, `recovery_sweep`, `version_dependence_census`,
`resolve_corpus_diff`, `infer_corpus_diff` and `xml_doc_source_*` sweeps' —
merges key by key rather than as text, so a rebase past another branch's
manifest movement needs no regeneration when the two moved different items. Enable the driver once per clone:

```sh
git config merge.borzoi-manifest.driver 'bash tools/manifest-merge.sh %O %A %B %P'
```

It fails, marking the entries, where both sides moved one item differently;
regenerate then. CI's gates still check the merged manifest. `.gitattributes`
names the keyed manifests, and a test holds it to them: an unkeyed manifest
merged as a set would keep both sides' rewrite of one line rather than conflict.
A line that counts the sweep's own verdicts conflicts whenever two branches move
any item it counts, so a manifest should count only its inputs and list its
verdicts (`docs/continuous-measurements.md`, "Exact manifests").

The remaining sweeps' floors are one-sided and documented as such. What must
never happen to any of them is loosening one to make a red run green without
first establishing which direction it moved and why.

`docs/continuous-measurements.md` has the full gate/measurement split, and the
record of why the MSBuild-on-real-projects row stayed unwired-and-named while it
had known divergences rather than being green-lit at a ceiling equal to them.

### The whole-project gate

CI's `corpus-diff` job (Whole-project resolution) runs on every pull request,
ungated by the change filters. Its gates are the steps of
`tools/ci/project-corpus-gate.sh`, which is the one list of them: CI runs
each as `project-corpus-gate.sh STEP`, and `project-corpus-gate.test.sh`
fails if the job runs anything else. So running the script runs the job:

```sh
bash tools/ci/project-corpus-gate.sh                    # every step
bash tools/ci/project-corpus-gate.sh xml-doc-source     # just one; --list names them
```

Run it outside `nix develop` (it enters the devshell itself) before pushing a
change that could move what the LSP answers, which is most changes to `cst`,
`sema`, `assembly`, `msbuild`, `nuget`, `lsp` or `corpus-diff`. The steps are
the project-corpus resolution differential and its exact manifest, the
`borzoi-corpus-diff` crate's tests (its FCS-driven fixtures included), the
whole-assembly projection sweep over the corpus's packages, and the
restore-graph and project source-doc differentials and the restore-cone census
with their exact manifests (`crates/lsp/tests/manifests/`,
`crates/msbuild/tests/manifests/`). With every step it keeps going past a
failure and names the failed steps at the end.

Run locally, it first materialises the pinned project corpus (clones, and
restores from nuget.org) into a cache shared by every worktree,
`${BORZOI_PROJECT_CORPUS_CACHE:-~/.cache/borzoi/project-corpus}/v2`. Concurrent
runs are safe: each has its own plan and env files, a lock serialises the
checkout and restore, and no run rewrites the corpus while another run's steps
read it, so a run of a different pin waits for them. A run whose pin is
already materialised skips the restore. A run killed outright leaves its lock
or its reader registration behind; the next run that needs it gone fails and
says what to remove once no step of the killed run is still going.
