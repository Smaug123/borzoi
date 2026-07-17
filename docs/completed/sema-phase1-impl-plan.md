# Sema Phase 1 implementation plan — intra-file name resolution

> **Status: implemented.** All three stages have landed:
> - Stage A (crate skeleton + `Pat` binder extraction) — PR #183.
> - Stage B (FCS symbol-uses oracle harness; `fcs-dump uses` subcommand) — PR #201.
> - Stage C (intra-file scope tree + resolution, differentially tested) — PR #230.
>
> This was the implementation breakdown for Phase 1 of
> [`type-checker-plan.md`](../type-checker-plan.md) (intra-file
> name resolution, no inference). It decomposed that phase into
> individually-reviewable stages, each with its own correctness oracle.
> The LSP-facing payoff (go-to-definition etc.) was deliberately **out of
> this slice** — it is deferred until the project/assembly environment
> exists (design doc Phase 2), so this slice is pure, differentially-
> tested `sema` infrastructure.

Implement this plan with each stage on its own branch, stacked as
necessary on previous branches, so that a reviewer can review each branch
in isolation.

## Context

The deliverable of this slice is the `borzoi-sema` crate able to
resolve, *within a single file*, every name use that the current parser
subset can express (top-level `let`/`let rec` values and functions,
their parameters, and references to them in right-hand-side expressions)
to its defining binder — verified against FCS as the oracle. Nothing is
wired into the LSP server yet; consumption is via the differential test
harness, which is itself a form of consumption (per the incremental-
implementation skill).

Grounding facts established before planning:
- Parser entry point: `borzoi_cst::parser::parse(source) -> Parse {
  root: SyntaxNode, errors }`; the typed root is `ImplFile::cast(root)`.
- The `Pat` / `Expr` DUs and their accessors live in
  `crates/cst/src/syntax/mod.rs`; `LetDecl::is_rec()` already exists.
- The differential harness pattern lives in `crates/cst/tests/common/`
  (`invoke_fcs_dump(subcommand, path)` shells out to `tools/fcs-dump`,
  both sides project to a normalised form, `assert_eq!` diffs them;
  `LineIndex` maps FCS `(line, col)` → byte offsets). `proptest` is the
  property-test crate. Tests are integration-style under `tests/` with a
  shared `tests/common/mod.rs`.
- `tools/fcs-dump` currently exposes `ast`, `tokens-raw`,
  `tokens-filtered`, their `-batch` variants, and `entities`. It does not
  yet expose symbol uses.

## Stage A — `crates/sema` skeleton + binder extraction from `Pat`

**Dependencies**: none (parallelisable with Stage B).

**Implements**: design doc D1 (crate placement), D3 (`Def`/`DefKind`
model), Phase 1.1 (binder extraction).

**Scope**: Create the `borzoi-sema` crate (deps: `borzoi-cst`,
`borzoi-assembly`). Define `DefId` (newtype), `Def { name, kind,
range: TextRange }`, and the closed `DefKind` DU (`Value { is_function,
is_mutable } | Parameter | PatternLocal | Module`). Implement the pure
`binders(pat: &Pat) -> Vec<Def>` over the current `Pat` variants
(`Named` / `LongIdent` / `Wildcard` / `Paren` / `Const` / `Null` /
`Typed` / `Tuple`). This is dead code at the end of the stage — nothing
consumes it until Stage C — which is acceptable infrastructure.

**Correctness oracle** (self-contained; no FCS):
- Unit test per `Pat` variant.
- Property: every returned `Def.range` is an `IDENT_TOK` lying within the
  pattern node's source range.
- Property: `Wildcard` / `Const` / `Null` bind nothing; `Paren` and
  `Typed` are transparent (`binders(Paren p) == binders(inner)`,
  `binders(Typed(p, _)) == binders(p)`); `Tuple` binders are the
  in-source-order concatenation of element binders.
- Non-goal: duplicate-name detection (e.g. tuple `(x, x)`). Binder
  extraction is syntactic; duplicate/illegal-binding checking belongs to
  a later checking pass.

## Stage B — FCS symbol-uses oracle harness

**Dependencies**: none (parallelisable with Stage A).

**Implements**: design doc D7 (FCS differential oracle).

**Scope**: Add a `uses <sourcePath>` subcommand to
`tools/fcs-dump/Program.fs` that emits `GetAllUsesOfAllSymbolsInFile` as
JSON — per use: `{ symbolName, range, isFromDefinition, declRange }` —
following the existing `dumpEntities` shape. Add a `tests/common` helper
(`invoke_fcs_dump("uses", path)` plus a `NormalisedUse` projection that
reuses the existing `LineIndex` to convert FCS `(line, col)` positions to
byte offsets). No `sema` code depends on this stage; it is testing
infrastructure built ahead of its consumer (Stage C).

**Correctness oracle**: end-to-end smoke. For `let x = 1\nx`, assert the
harness produces exactly the expected normalised set — one definition use
of `x` at the binder, one non-definition use at the reference. This
proves the harness round-trips a known case before any resolver relies on
it.

## Stage C — intra-file scope tree + resolution, differentially tested

**Dependencies**: Stage A + Stage B.

**Implements**: design doc D2 (`resolve_file` signature), D4 (scope model
mirroring FCS's `Item` taxonomy), D5 (`Deferred` vs `Unresolved`),
Phase 1.2 (let/let rec scoping + position-ordered shadowing), Phase 1.3
(resolve uses → `Resolution`).

**Scope**: Implement the pure
`resolve_file(file: &ImplFile, preceding: &ProjectItems, assemblies:
&AssemblyEnv) -> ResolvedFile`. Build the parent-linked `Scope` tree by
walking `ImplFile → ModuleOrNamespace → ModuleDecl`, threading Stage A
binders into scopes with **position-ordered** bindings and correct `let`
vs `let rec` right-hand-side visibility (driven by `LetDecl::is_rec()`).
Resolve `IdentExpr` and single-segment `LongIdentExpr` uses to `Local` /
`Item`; bucket every shape we don't model (member access, multi-segment
paths, constructs the parser doesn't produce yet) as `Deferred`. Produce
the range→`Resolution` map and the `ExportedItems` the file contributes.
The `preceding` and `assemblies` parameters exist for signature stability
but are empty / unused this slice (consumed in design doc Phase 2).

**Correctness oracle**:
- **Headline differential property** over a single-file corpus bounded to
  the current parser subset (top-level lets, functions, parameters, RHS
  uses): for every FCS symbol use whose declaration is *in this file*,
  our resolution at that use's range is `Local` / `Item` pointing at a
  `Def` whose range equals FCS's declaration range. We **never** return
  `Unresolved` where FCS resolved in-file, and **never** point at the
  wrong binder. Uses FCS resolves into referenced assemblies or implicit
  opens (out of this slice's scope) are permitted to be `Deferred`.
- Targeted unit + property tests expressed *through resolution*: a
  recursive function references its own binder (`let rec`); a non-rec
  `let g = … g …` does **not** resolve the inner `g` to its own binder;
  sequential top-level shadowing resolves to the latest prior binder of
  that name. FCS agrees on each, so these double as oracle cases.

## Ordering and parallelism

- Stages A and B have the simplest oracles (A is FCS-free; B is
  harness-only) and share no dependencies, so they can be implemented in
  parallel.
- Stage C joins them: it is the first stage gated on the real FCS
  differential property.

## Deferred to the next slice (design doc Phase 2)

- The Compile-order fold over multiple files (`ProjectItems`), the
  `AssemblyEnv` name index over `EcmaView`, and the `ImportScope` for
  implicit/explicit opens.
- LSP wiring: `textDocument/definition`, find-references, rename,
  document symbols. The Phase 1 resolver is consumed only by the
  differential test harness in this slice; the user-visible payoff
  arrives once cross-file + assembly resolution exists.
