# AST versioning plan

> **Status (2026-11-13).** A living plan. The decisions below are settled; the
> work is an eight-PR sequence ([PR sequence](#pr-sequence)), of which PR 1 (the
> feature table, #15), PR 2 (diagnostic classes), PR 4 (the union behind
> `unstable-union`, #13, #670) and PR 5 (astgen owns every node) have landed.
> Earlier stages that still stand: `LanguageVersion` as a parse input
> (#592/#597/#606), the LSP's `<LangVersion>` wiring (#613), and the generated
> `syntax::v8` / `syntax::v9` facades (#618 and after). The goal: before the next F# compiler upgrade (a new
> language version), every earlier language version has a stable, exhaustive
> typed facade, and the upgrade itself is the proof that the scheme extends.
> Companion to [`parser-plan.md`](parser-plan.md), which owns the parser; this
> doc owns the *public typed-AST surface* and how it stays stable as F# evolves.

## Problem

The typed AST is a public artifact: third-party tooling (and LLM agents) will
write code that matches on it. F# grows — new syntax, occasionally relaxed
restrictions, rarely a gated source break — so our model of it must grow too.
Adding a variant to a dispatch enum (`Expr`, `Pat`, `Type`, `ModuleDecl`,
`SigDecl` in [`syntax/mod.rs`](../crates/cst/src/syntax/mod.rs)) breaks every
exhaustive `match` downstream. Consumers should be able to:

1. pin to an F# language surface (e.g. 8.0) and get a typed AST that is **both
   stable** (never breaks under our upgrades) **and exhaustive** (the compiler
   tells them they've handled all of it); and
2. opt into a newer surface when they choose, without that being forced on them
   by an unrelated crate upgrade.

FCS and Roslyn both decline to solve this — FCS lets `SynExpr` grow and breaks
consumers on upgrade; Roslyn sidesteps closed sums with open virtual dispatch.
We can do better because the untyped tree is already stable and additive and
the typed layer is a thin, regenerable projection over it.

## The core insight

A versioned typed AST is a **projection** with two directions, and they have
different totality:

- **Reading** parsed source — `union → vN`. **Partial** in general: a parsed
  tree may contain post-N nodes `vN` cannot represent.
- **Constructing / rewriting** — `vN → union`. **Total**: the supported surface
  only grows, so every `vN` tree embeds in the union.

The reading direction is the one that bites, and what makes it safe is
**language-version pinning**: parse at N, and every post-N node in the tree lies
under a diagnostic that says so (D9). Language-version support and versioned
facades are therefore not two independent features — the version is the
**index that makes the read-projection sound**. The one risk the version index
does *not* cover is **us re-modelling an already-frozen version**
(representation drift with no language change); the lock files of D6 make that
a visible, reviewed event.

## Scope

- **In scope.** `LanguageVersion` as a parse input; the union as the maximal
  model behind a feature flag; one deep, frozen typed facade per language
  version; the feature table that drives gates and facades; the property suite
  and lock files that enforce it all.
- **Out of scope (by choice).** Trivia and ranges are **not** versioned — the
  differential normaliser already elides them, so the versioned surface is
  *structural* only. The untyped substrate (`SyntaxNode`, `SyntaxToken`,
  `SyntaxKind`, `AstNode`, `TextRange`) is versioned by crate SemVer, not by
  language version (D1, D7).
- **Free hand.** The repo is private; breaking today's internal callers is
  fine. The versioning is for *future external* consumers.

## Settled decisions

### D1. Two orthogonal version axes — never conflate them

- **F# language version** (8.0 vs 9.0) — a property of the *source*. It indexes
  the public facades and is what gets *frozen*.
- **Crate SemVer** — a property of *our code* (we model `match` more precisely,
  fix a projection bug, add an accessor). Ordinary library versioning; it also
  governs the substrate.

We must be able to ship a newer F# version's parsing (a crate upgrade)
**without** forcing consumers off `v8`, and fix a modelling bug **without**
inventing a new F# version.

### D2. The private type is the *union of supported surfaces*, not "latest"

"Always the latest" is wrong under *removal*: if F# drops a construct an older
version had, "latest" stops modelling it and `vOld → latest` stops being total.
So the private representation is the **union over all supported surfaces**. Each
construct carries an interval (`introduced`, optional `removed`), and the union
keeps it while *any* supported version needs it. Removal is then one more
interval bound (`KindInterval::removed`, carried and unused today).

The union is what sema and the LSP use (they need the maximal, preview tree).
It sits behind the cargo feature `unstable-union` (D11), which sema, the LSP,
corpus-diff and cst's own tests enable; without it, `syntax` exports only the
substrate (#13, landed in #670). The hand-written union lives in the private
`syntax::union`. The `v8`/`v9` facades sit behind the same feature for now,
because they re-export union node types; the deep facades of PR 6 own their
types and come out from behind it.

### D3. `LanguageVersion` is a first-class parse input (the index)

Modelled on FCS's `LanguageFeatures.fs`: a closed enum `V4_6 … V11_0, Preview`
with `DEFAULT = V10_0` (FCS's `defaultVersion`; `latest` and `latestmajor`
resolve to it) and `Preview` above every numbered version (FCS's `9999`).
`ParseOptions::lang` selects it and `Parse::lang` records it.

The version has two parser roles:

- **Legality gates.** A construct newer than the pin draws FCS's diagnostic
  (FS3350 and friends). Gates are **per feature**, each against its own
  introduction version, so an unusual pin is gated correctly. Which features
  are gated, and how, is the [feature table](#the-feature-table).
- **Parse divergence.** Some features change what the lex-filter produces:
  `RelaxWhitespace2` (6.0: the `+1` offside grace and `CtxtMatch` pops) and
  `StrictIndentation` (8.0: offside pushes abort rather than warn) reshape the
  token stream and so the tree. A parse records reaching such a point in
  `Parse::shape_depends_on_language_version`, and
  `LanguageVersion::SHAPE_REPRESENTATIVES` names one version per band. These
  are the table's `Shape` rows.

### D4. Deep facades: each `vN` owns its whole surface

For each language version, `syntax::vN` owns **every** node type and enum: a
node is a newtype over the union node, and each accessor is a generated one-line
delegate that calls the union accessor and re-casts the result into `vN`'s
types (`v8::ParenType::inner() -> Option<v8::Type>`). astgen's tables declare
every node newtype, dispatch enum and value enum of the union; the accessors
are hand-written in `syntax/union.rs`, and `tools/astgen` reads their
signatures back with `syn`. Each return type is a container (`T`,
`Option<T>`, `impl Iterator<Item = T>`, `Vec<T>`) around an element (a node,
a dispatch enum, a value enum, `SyntaxToken`, or one of `bool`, `usize`,
`i32`, `TextRange`, `&'static str`, or a tuple of those), and astgen
**fails**, naming the accessor, on a return type outside that set
(`tools/astgen/src/shape.rs`). `union.rs` may declare no public type, module or
item-level macro, so no accessor can hide from the reading.

There is no `Deref` or `From` to the union. The escapes are `.syntax()` (the
substrate, D7) and `to_union()`, the latter only under `unstable-union`. A union
refactor that breaks a frozen delegate needs an explicit hand-written compat
shim — the cost D6 accepts, made visible. Each facade sits behind a cargo
feature; sema and the LSP enable none; cst's tests enable all.

The canonical read is `Parse::view::<V>()`: it checks `parse.lang ∈
V::VERSIONS` and returns the root as `V`'s file node. A node out of `V`'s
surface reads as an absent child (its cast is `None`), and a legality
diagnostic covers it (D9). Viewing a tree through a different version's facade
is not the main path; exact match sidesteps removal entirely.

### D5. Single source of truth: the feature table (#15)

One hand-edited table,
[`crates/cst/language-features.tsv`](../crates/cst/language-features.tsv),
keyed by FCS feature, drives both the parser's version gates and the facades'
kind intervals; `tools/astgen` (never a dependency of `borzoi-cst`) generates
everything else from it. See [The feature table](#the-feature-table).

### D6. Freezing: a lock file that is the generator's input

`crates/cst/facades/vN.lock` lists a facade's enums (variants and their kind
sets), node types, and accessors (signature and union delegate).

The format is one fact per tab-separated line, tagged `node` (a node newtype
and one kind it casts from), `enum` (a dispatch-enum variant and its node),
`value` (a value-enum variant and its payload), `derive` (a trait a value enum
derives) or `accessor` (owner, method and return shape), sorted with no
duplicates, so a lock has one spelling (`tools/astgen/src/lock.rs`). A kind
set is the `node` lines of the variant's node. Because every fact is its own
line, what a frozen surface may take — a new accessor, a new kind for an
existing node — is an added line, and anything else removes or rewrites one.
The union's own lock, `crates/cst/facades/union.lock`, exists today and is
always regenerated; an accessor's delegate is the union accessor of the same
name until a facade needs a compat shim.

- An **unpublished** facade's lock is derived and regenerated freely.
- A **published** facade (astgen's `PUBLISHED` list) is generated *from* its
  lock: the generator reads it and never writes it, and the staleness test
  checks `code == gen(lock)`.

Checks:

1. a CI script, with its own test, diffs every published lock against the merge
   base and allows only appended accessor lines and additions to a variant's
   kind set;
2. generic P2/P3 over every (facade, enum) pair;
3. a frozen consumer fixture per published facade — an exhaustive,
   wildcard-free `match` on every enum and a typed call of every accessor —
   generated once, at publish.

The only re-baseline route is an explicit `FREEZE_EPOCH` bump, allowed until
the first external release. cargo-semver-checks was considered and rejected:
the lock is both narrower (only the facades) and stronger (it is the
generator's input).

What freezing costs: a *modelling* fix to a published surface is a breaking
change, so it lands in the union and in unpublished facades, and the published
`vN` keeps its original, possibly imperfect shape.

### D7. The substrate is the floor: incomplete, never wrong

The untyped tree represents anything the parser produces, so the typed layer's
failure mode for anything unanticipated is **incompleteness, never
wrongness**: a consumer can always drop to `.syntax()`.

- `SyntaxKind` is `#[non_exhaustive]` — the one place non-exhaustiveness is
  right.
- Facade enums are closed and exhaustive. There is **no `Unknown` arm**: a
  post-N construct under pin N is a hole covered by a legality diagnostic
  (D9), not a node.

### D8. One module per `LanguageVersion`

`syntax::{v4_6, v4_7, v5, v6, v7, v8, v9, v10, v11, preview}`. A version whose
surface equals its neighbour's is `pub use super::vX::*`, recorded as
`alias-of` in its lock — so shared type identity between aliases is part of the
contract (accepted). The expected distinct surfaces are {4.6, 4.7}, {5, 6, 7},
{8}, {9, 10, 11} and preview. A kind with no FCS `LanguageFeature` gets no
interval, so it is in every facade (`Expr::Typar` is in all of them).

### D9. Gate and surface are different questions; diagnostics carry a class

The table separates them: `introduced` alone drives the **surface**;
`fcs_stage`, `fcs_effect` and `borzoi` drive the **gate**. FCS checks some
features that have syntax kinds only in the type checker (`StringInterpolation`
5.0, `FromEndSlicing` preview), and one, which we have so far left ungated, in
the parser (`AccessorFunctionShorthand` 8.0, `_.M`). Each such kind still gets
a borzoi **parse-phase legality diagnostic**, graded against FCS's parse *plus
check* diagnostics, because without it `view::<V>()` would have an uncovered
hole.

Every diagnostic carries a class (`parser::DiagnosticClass`, on each
`ParseDiagnostic`):

- `Legality { feature }` — tree-neutral: "this construct needs a newer
  version";
- `Recovery` — ordinary errors, and the gates whose FCS effect is `Reshape`
  or `Severity`.

A gate's class is not chosen by hand: `DiagnosticClass::of_gate` reads it off
the table's `fcs_effect`, `Legality` exactly when every effect is `Diagnose`
(vacuously so for a `check` row, which is what PR 3's check-time gates will
be). Only the two gate producers (`parser::feature_gates` and the `#elif`
side channel) build a classed diagnostic; every other producer's problem
reaches `Parse` as `Recovery` by construction. So today the dotless `float32`
literal and the nullness annotation, whose trees FCS changes below their
versions, report `Recovery`, and the other gated parse-time features report
`Legality`.

`Parse::errors` minus the `Legality` ones stays FCS-*parse*-faithful
(`Parse::recovery_errors`). P2 is restated: `view::<V>()` is total when there
are no legality diagnostics, and otherwise every hole lies under one.

Held by cst's `diagnostic_classes` tests: every live parse-time row of the
table has a fixture, and at every version below the feature's a `Legality`
row's fixture parses to the tree it parses to at the feature's version, with
the same `Recovery` diagnostics and only `Legality` errors naming that
feature, while a `Reshape` or `Severity` row's fixture moves across its
version and never reports a `Legality` diagnostic.

### D10. Version invariance compares Recovery only

`SyntaxRecovery::of_guessed_version` (sema) keeps its full `[MIN, MAX]` range,
and `SyntaxRecovery` reads only `Recovery` errors.
`diagnostics_are_version_invariant` compares `Recovery` diagnostics at the two
ends of the ladder and the tree: an unflagged
`shape_depends_on_language_version` proves the tree the same everywhere, and a
flagged one is compared at both ends and at every shape band between them. So a
faithful legality gate (`#elif` today; `_.M` and interpolated strings in PR 3)
does not make every file using the construct version-dependent — the trade-off
that kept those gates unmodelled. A `Reshape` gate (nullness below 9.0; `_.`
patterns and `for _ =`, where FCS raises a parse error and recovers) is
`Recovery` and stays version-dependent, and so does a tree the lex-filter
reshapes with no diagnostic moving (a closer dedented to its `let`'s column,
across F# 6).

Two checks hold this:

- **The inference differential** (sema's `infer_legality_pin_diff`): for each
  gated `Legality` feature, a file with the construct inside an annotated
  declaration, checked by FCS at the version just below the feature's and at
  the feature's own. FCS's parse reports only FS3350 at the old pin, and every
  expression and binder type it reports is the same at both versions, and
  every type inference commits over our tree at the old pin is FCS's there.
  The oracle's FCS (the `FSharp.Compiler.Service` package `tools/fcs-dump`
  references) predates the `#elif` check, so for `PreprocessorElif` it shows
  the types only (`ORACLE_PREDATES`). The oracle needs
  `SKIP_VERSION_SUPPORTED_CHECK` to check below 8.0 at all: without it FCS
  refuses the version (FS3880) and checks at its default.
- **The corpus census** (cst's `version_dependence_census`): every corpus
  file parsed at every version, each version graded against preview as a
  moved tree, a moved `Recovery` diagnostic, a moved `Legality` diagnostic
  only, or no change, and every file that moves pinned in
  `tests/manifests/version_dependence_census.txt`. On the pinned corpus 345
  files are version-dependent and 59 move only in `Legality` diagnostics, so
  a guessed version keeps their reading. The census also asserts that two
  versions agreeing on every `Recovery`-class feature parse every file
  identically, and that `diagnostics_are_version_invariant` agrees with the
  verdict read off all ten versions.

### D11. Feature flags and the remaining issue decisions

- **#13** — the union sits behind `unstable-union`; paths are unchanged.
  Done (#670).
- **#17** — `syntax::preview` sits behind `unstable-preview`, is never
  published, and its lock is always regenerated.
- **#16** (compile-time type-tag views) stays deferred; `view::<V>()` over a
  sealed `Facade` trait is the baseline.

## The feature table

[`crates/cst/language-features.tsv`](../crates/cst/language-features.tsv) has
one row per FCS `LanguageFeature`, *including* the ones only the type checker
reads (`Unmodelled(checker-only)`), so that a compiler bump's new features all
show up as missing rows. Its columns (the file's header comment is the
reference):

| Column | Meaning |
|---|---|
| `feature` | FCS's case name, and the `LanguageFeature` variant name. |
| `introduced` | FCS's `features` map version (`4.7` … `11.0`, `preview`). It alone decides facade membership. |
| `fcs_stage` | Where FCS's parse pipeline consults it (`lex`, `lexfilter`, `parse`, `post-parse`, `+`-joined), or `check` when only the checker or a later phase does. |
| `fcs_effect` | What FCS does below `introduced`: `Diagnose` (CLFAR; the tree FCS builds is ours), `Reshape` (hard error with recovery, a relabelled token, a different token stream), `Severity` (a diagnostic appears or changes severity); `-` for `check`. |
| `borzoi` | `Kind(K)` (a node kind the feature introduced), `Positional(detector)`, `Shape`, `Trivia`, or `Unmodelled(reason)`; `+`-joined, except `Unmodelled`. |
| `engaged` | `live`, `deferred(reason)` (recorded, inert: no gate, no interval), or `-` for `Unmodelled`. |
| `fcs_sites` | `LanguageFeature.X` uses per file of FCS's parse pipeline, as `path=count`. |
| `display_name` | FCS's FS3350 name for it (`FSComp.txt`). |
| `notes` | Free text. |

**Generated from it** (`cargo run -p borzoi-astgen --example generate`, held by
astgen's staleness test `tests/up_to_date.rs`):

- `crates/cst/src/syntax/generated/features.rs`: the `LanguageFeature` enum
  (re-exported from `borzoi_cst::language_version`), `introduced()`,
  `display_name()`, `fcs_stages()`, `fcs_effects()`, `POSITIONAL_GATES` (the
  tree-walk gates' fast path), `kind_feature` (the live `Kind` rows), and
  `DEFERRED_KIND_FEATURES`;
- the facades' kind intervals (astgen reads the live `Kind` rows directly).

Every version question the parser asks is `lang.supports(LanguageFeature::X)`;
there are no per-feature predicates.

**Held to the compiler** by cst's `language_features` tests, which read the
pinned compiler source (`BORZOI_CORPUS`) and run in the normal suite:

1. every FCS feature has exactly one row, with FCS's version and display name,
   and FCS's version ladder and default are `LanguageVersion`'s;
2. each row's `fcs_sites` equals a census of `LanguageFeature.X` (outside `//`
   comments) over the parse pipeline — `lex.fsl`, `pppars.fsy`, `pars.fsy`,
   all of `SyntaxTree/`, `Facilities/prim-lexing.fs{,i}` and
   `Driver/ParseAndCheckInputs.fs` — and a row the pipeline never mentions is a
   `check` row;
3. the live rows are what the parser consults: every
   `supports(LanguageFeature::X)` / `gate(LanguageFeature::X, …)` in
   `crates/cst/src` names a live row, and every live row with a non-`Kind` item
   is consulted.

Each failure prints the cell to write. A compiler bump therefore lands as a
list of table edits: new rows (a ready-to-paste line for each), moved versions,
and new or moved parse-time sites to classify.

**Deferred rows.** `INTERP_STRING_EXPR` (`StringInterpolation`, 5.0),
`DOT_LAMBDA_EXPR` (`AccessorFunctionShorthand`, 8.0) and `INDEX_FROM_END_EXPR`
(`FromEndSlicing`, preview) are recorded as `Kind` rows with
`engaged = deferred(PR 3 …)`. They drive neither a gate nor a facade interval,
so the `v8`/`v9` facades and every gate are exactly what the hand-written
tables gave. They go live in PR 3, with D9's legality diagnostics, and reach
the facades in PR 6.

## The central invariant (and how it is enforced)

Soundness reduces to one property of the union: **it only ever *refines*
distinctions, never *coarsens* them.** The facades are kind-driven
classifications over a shared `SyntaxNode`, so this is cheap to test:

- **P1 — Gate completeness.** Every construct introduced after N draws a
  diagnostic under pin N. Per-feature fixtures, and differentially FCS at every
  version (`parser_diff_feature_gates`, 4.6 … 11.0 and preview).
- **P2 — Projection totality.** For a tree parsed at P, `view::<vP>()` is total
  when there are no legality diagnostics, and otherwise every hole lies under
  one.
- **P3 — Round-trip identity.** For any `vP` node, lowering to the union and
  re-projecting yields the same node — "the union has not coarsened".
- **P4 — Interval consistency.** The union's classification refines every
  `vN`'s: no two kinds distinct in some `vN` collapse in the union.

Today P2–P4 are tested for `Type` and nullness only (`ast_projection.rs`);
PR 6 makes them generic over every (facade, enum) pair.

## Landed stages

- **`LanguageVersion` value type** (#592/#597) — the enum,
  `from_lang_version_text` mirroring FCS's `getVersionFromString`, `DEFAULT`,
  `Display`, ordering.
- **The version threaded into `parse`, and the `#elif` gate** (#606) —
  `ParseOptions` / `parse_with_options`, `Parse::lang`, FS3350 on `#elif`
  below 11.0 from a driver side channel that leaves the tree alone.
- **The LSP's `<LangVersion>`** (#613) — absent → 10.0, unrecognised → 10.0 and
  a log line, orphan buffer → `Preview`.
- **The facade mechanism** (#618,
  [`ast-versioning-nullness-proof.md`](completed/ast-versioning-nullness-proof.md))
  and its productionisation: astgen generates `syntax::v8` (the union minus
  `Type::WithNull`) and `syntax::v9` (the union), and `syntax::projection` holds
  the runtime surface gate.
- **Parse-time feature gates** — `parser::feature_gates` (FS3350 for `while!`,
  constraint intersection, `static abstract`, attributes after `module`, typed
  `let!`/`return`, FS1153 for `1f`), and the lex-filter's version-aware offside
  gates.
- **PR 1 — the feature table** (#15): the TSV, astgen's parser and the
  generated `features.rs`, `#elif` as an ordinary row, the three
  compiler-source tests, and 11.0 in `parser_diff_feature_gates`.
- **PR 2 — diagnostic classes** (D9, D10): `DiagnosticClass` on every
  `ParseDiagnostic`, read off the table for the gates; `SyntaxRecovery` and
  `diagnostics_are_version_invariant` over `Recovery` diagnostics and the tree;
  the `diagnostic_classes` fixtures, the `infer_legality_pin_diff` inference
  differential, and the `version_dependence_census` corpus manifest.
- **PR 4 — the union behind `unstable-union`** (#13, #670): the hand-written
  union in a private `syntax::union`, re-exported only under the feature, with a
  featureless clippy and doc step in CI. `v8`/`v9` are behind it too until PR 6.
- **PR 5 — astgen owns every node** (D4): the standalone nodes and the value
  enums moved into astgen's tables (`generated::union_standalone`); astgen reads
  every accessor's signature out of `union.rs` and classifies its return shape,
  and emits the union's lock, `crates/cst/facades/union.lock` (D6). The `v8` /
  `v9` facades still re-export the union's dispatch-enum categories only.

## PR sequence

1. **Feature table (#15)**, this plan, stale-doc fixes. No behaviour change.
   *Landed.*
2. **Diagnostic classes** (D9): `Legality` / `Recovery`; `SyntaxRecovery` and
   version invariance over `Recovery` (D10); the version-dependence census.
   *Landed.*
3. **Faithful gates** (#21's remainder): `_.M`, interpolated strings, `^`
   from-end indexing, `_.` patterns, `for _ =`, `$$"""`, `>|}`
   (`BetterAnonymousRecordParsing`) — each gated, or `Unmodelled` with its
   reason. The three deferred `Kind` rows go live.
4. **Union behind `unstable-union`** (#13). Independent of 1–3. *Landed*
   (#670); the `v8`/`v9` facades are behind the feature too until PR 6.
5. **astgen owns every node** and reads accessor signatures (D4); it emits the
   union's lock only. *Landed.*
6. **Deep facades for every version and preview** (#14, #17),
   `Parse::view::<V>()` (#12), generic P2/P3/P4.
7. **Freeze**: `PUBLISHED = v4_6 … v10`, the CI lock check, consumer fixtures,
   and a permanent synthetic-feature test (a fake feature and kind at "12.0",
   in memory: every published facade regenerates byte-identically, and the new
   facade differs by exactly that variant).
8. **The compiler upgrade** — the proof; `v11` is published after it.

## Still to do

Everything in PRs 3 and 6–8 above. Beyond them:

- **Stale issues.** #21's text predates the relax-whitespace and `#nowarn`
  work; its remaining scope is PR 3's list. #14's "interval data below 8.0" is
  the table's job; what remains of it is PR 6.
- **`display_name` is FSComp.txt's text as FCS returns it**: the quoted text
  after `String.Format` with no arguments, which is what FCS's resource
  generator applies (so `builder {{ }}` reads `builder { }`). For the gated
  features that is the FS3350 text the differentials check; for a
  checker-only row it is checked only against that reading of FSComp.txt
  until the row is gated.
- **The oracle's FCS is older than the table's.** `tools/fcs-dump` references
  `FSharp.Compiler.Service` 43.12.204, which reports nothing for `#elif`
  below 11.0; the table transcribes the `fsharp-src` compiler, which reports
  FS3350. No differential holds the `#elif` gate to FCS until the package
  moves.
- **The project oracles below F# 8.0.** `uses-project-batch` and
  `xmldoc-batch` pass `--langversion` without `SKIP_VERSION_SUPPORTED_CHECK`,
  so a project pinned below 8.0 is checked at FCS's default (FS3880); only
  the single-file `file-batch` sets it.

## Open questions

- **Positional features and the surface.** A feature that gives an existing
  kind a new legal *position* (`Positional`) has no facade delta, only a gate.
  If a future positional feature needs a facade distinction, it needs a kind of
  its own first.
- **Type-tag views (#16).** A compile-time `parse::<V8_0>(src) -> Parse<V8_0>`
  layer, bounded by a sealed `surface(P) ⊆ surface(N)` trait, would make some
  misuse a compile error; it is O(versions²) trait impls (codegen-able).
  Deferred; the runtime-checked `view::<V>()` is the baseline.
