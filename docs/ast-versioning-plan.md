# AST versioning plan

> **Status (2026-11-14).** A living plan. The decisions below are settled; the
> work is an eight-PR sequence ([PR sequence](#pr-sequence)), of which PR 1 (the
> feature table, #15), PR 2 (diagnostic classes), PR 3 (faithful gates, #21),
> PR 4 (the union behind `unstable-union`, #13, #670), PR 5 (astgen owns every
> node), PR 6 (a deep facade per language version, `Parse::view`) and PR 7
> (the freeze: `v4_6` … `v10` published) have landed. Earlier stages that
> still stand: `LanguageVersion` as a parse input (#592/#597/#606) and the
> LSP's `<LangVersion>` wiring (#613). The goal:
> before the next F# compiler upgrade (a new language version), every earlier
> language version has a stable, exhaustive typed facade, and the upgrade
> itself is the proof that the scheme extends.
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
`syntax::union`. The per-version facades own their types and sit behind
features of their own (D4); only their `to_union` escape needs this one.

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
- **Parse divergence.** Some features change what the lexer or the lex-filter
  produces: `RelaxWhitespace2` (6.0: the `+1` offside grace and `CtxtMatch`
  pops), `StrictIndentation` (8.0: offside pushes abort rather than warn) and
  `ExtendedStringInterpolation` (8.0: below it the lexer reads `$$"""` as
  `$"""`) reshape the token stream and so the tree. A parse records reaching
  such a point in `Parse::shape_depends_on_language_version`, and
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

The canonical read is `parse.view::<vN::ParsedInput>()`. `ParsedInput` is
each facade's root enum (`ImplFile` / `SigFile`, after FCS's `ParsedInput`),
and it implements the sealed trait `syntax::Facade`, whose `VERSIONS` are the
facade's version and its aliases'. `view` returns `Result<V, ViewError>`: a
parse whose `lang` the facade does not serve is an error naming both, not a
silent projection, because the facade's exhaustiveness is a claim about its
own version's syntax only. Under a facade that serves the parse's version, a
node out of its surface reads as an absent child (its cast is `None`), and a
gate diagnostic covers it (D9). Exact match sidesteps removal entirely.

How a facade is built (`tools/astgen/src/facade.rs`):

- A node is `facade_node!(Name: KIND | …)` (`syntax/facade_support.rs`): a
  newtype over the union's node that casts exactly its lock's kinds, prints
  (`Debug`) as the union's node does, and has `to_union` under
  `unstable-union`. A dispatch enum carries only its surface's variants; a
  value enum is copied, with its docs from astgen's tables.
- An accessor is one line: call the union accessor its lock line delegates to
  and re-cast the result. Re-casting a node or a dispatch enum is taken to be
  able to fail, even where the union holds nothing more today: the union gains
  kinds and variants whenever a construct is modelled, and this keeps a
  facade's code a function of its own lock, so the union's growth leaves a
  published facade byte-identical. A value enum's re-cast can fail when the
  union's has a variant the facade lacks or one of its variants' bare fields
  can. A failed re-cast drops the element: an `Option` becomes `None`, an
  iterator or `Vec` skips it. So `v4_6::TupleType::segments()` leaves out a
  segment holding a `WithNull`, and an `InterpStringPart::Fill` over an
  out-of-surface expression is skipped.
- The generator refuses, naming it, what it cannot re-cast mechanically: an
  accessor returning a fallible type bare (no container to absorb the drop),
  and a facade node, variant, value variant or accessor delegate with no
  matching union counterpart. Each is where a published facade needs a
  hand-written compat shim; none exists today.

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
derives) or `accessor` (owner, method and return shape; in a facade's lock
also its delegate, `union.<method>`), sorted with no duplicates, so a lock has
one spelling (`tools/astgen/src/lock.rs`). An alias's lock is the single line
`alias-of <module>`. A kind
set is the `node` lines of the variant's node. Because every fact is its own
line, what a frozen surface may take — a new accessor, a new kind for a node
(or a new node type), a new trait on a value enum — is an added line, and
anything else removes or rewrites one.
The union's own lock, `crates/cst/facades/union.lock`, is always
regenerated. A facade's lock is *derived* from it: every node all of whose
kinds the table's live `Kind` rows admit at the version, the variants over
those, and every accessor whose owner and return type survive, delegating to
the union accessor of its own name. A delegate that names another union
accessor is how a published facade survives a rename.

- An **unpublished** facade's lock (`v11`, `preview`) is derived and
  regenerated freely.
- A **published** facade (astgen's `PUBLISHED`: `v4_6` … `v10`) is frozen.
  Its lock is derived and written once, at publication, stamped
  `# freeze-epoch: N` with astgen's `FREEZE_EPOCH`; from then on the generator
  reads it and never writes it, and the staleness test checks
  `code == gen(lock)`. A facade's code depends on the union only for whether
  the union still backs its lock (D4's re-casts), so publishing changes only
  who writes the lock, which astgen's `a_published_lock_round_trips` checks.

A frozen lock is still held to the surface its version would derive today,
on the principle that a published facade never breaks, not that it keeps up
with the union. On every generation astgen derives it and compares:

- A derived **accessor** or **derive** the lock lacks is *opt-in*: leaving it
  out breaks no caller and leaves no node unreadable, so the frozen facade
  simply does not offer it until someone appends the line. Routine work that
  adds union accessors touches no frozen lock; the generator prints a count
  of what each frozen facade may opt into (`facade::opt_in`).
- Everything that would break the frozen facade or open a hole fails:
  a derived **kind** the lock does not cast (a new kind for one of its nodes,
  or a new node type), whose nodes would read as holes no gate covers; a
  new **variant** of a frozen enum or value enum; a line **locked but no
  longer derived** (an accessor whose union delegate is gone or changed
  shape, a kind gone or moved to another node); and a frozen **alias**
  whose version's surface no longer equals its head's. Each must be
  appended to the lock (a kind) or recorded in `ACCEPTED_DIVERGENCES`
  (`tools/astgen/src/facade.rs`) with the reason the frozen facade keeps its
  surface. Until then astgen fails, listing each line and these choices; an
  acceptance that no longer matches a divergence fails too. What an
  acceptance touches (a kind, a value enum) is carried into the property
  suite's table, which holds it in that facade to the round trip alone. A
  compat line (an accessor whose delegate names another method) is not a
  divergence.

An unpublished version never aliases a published head: an alias's versions
are its head's `Facade::VERSIONS`, so the head's frozen code would otherwise
follow whatever the unpublished version derives. `v11` is therefore its own
facade until it is published, and only then may it become an alias of `v9`.

Checks:

1. `tools/ci/facade-lock-check.sh` (CI's `facade-locks` job; its test,
   `facade-lock-check.test.sh`, runs in the lint job) diffs every published
   lock against the pull request's base. It allows only added `accessor`,
   `node` and `derive` records, and an accessor's delegate may change (no
   caller sees it; it is how a published accessor survives a union rename);
   a removed or rewritten record, an added `enum` or `value` record, an
   `alias-of` added, dropped or retargeted (an alias's type identity is part
   of the contract), and a lock deleted, unstamped or restamped all fail. A
   consumer fixture present at the base must be byte-identical. A lock first
   stamped, or a fixture first added, is a publication and passes.
2. Generic P2/P3 over every (facade, enum) pair (`facade_properties`).
3. A frozen consumer fixture per distinct published facade,
   `crates/cst/tests/all/facade_consumers/<module>.rs`, written with the lock
   at publication and never regenerated: an exhaustive, wildcard-free `match`
   on every dispatch enum and value enum, every payload's type spelled out, a
   typed call of every accessor, each value enum's derives and each type's
   handle traits. cst's test binary compiles it. astgen's
   `every_published_facade_has_a_covering_consumer_fixture` checks that each
   distinct published facade has one, stamped at its lock's epoch, matching
   exactly the lock's variants and calling only its accessors (an accessor
   appended later is one the fixture never saw).
4. The **synthetic-feature test** (astgen's
   `a_new_language_version_leaves_every_published_facade_byte_identical`):
   in memory, a feature at a new "12.0" whose one `Kind` is a new expression,
   added to the table, the union and the version list, and a new union
   accessor on a node every facade has. Every published facade's code, lock
   and fixture regenerate byte-identically and nothing fails; each frozen
   head may opt into the accessor; the new facade's surface is the one before
   plus exactly that variant, and preview's its own plus the variant and the
   accessor. It is the compiler upgrade in miniature, run on every change.

The only re-baseline route is raising `FREEZE_EPOCH`, allowed until the first
external release: a lock or fixture stamped with an older epoch is derived and
written afresh, as at publication, and the CI check waives its rules for a
change that raises the constant. cargo-semver-checks was considered and
rejected: the lock is both narrower (only the facades) and stronger (it is the
generator's input).

What freezing costs: a *modelling* fix to a published surface is a breaking
change, so it lands in the union and in unpublished facades, and the published
`vN` keeps its original, possibly imperfect shape.

### Publishing a facade

The runbook, for the compiler upgrade and after it.

**Publishing a new version's facade** (`v11` after the upgrade). Once the
table and the union model everything the new compiler adds, add the module to
`PUBLISHED` in `tools/astgen/src/facade.rs` and run
`cargo run -p borzoi-astgen --example generate`. The lock is derived one last
time and written stamped, and if the version is a distinct facade its consumer
fixture is written beside it and declared in `facade_consumers/mod.rs`. If its
surface equals the previous published head's, it is published as an alias of
it, sharing those types for good. Commit the lock and the fixture; the CI
check treats them as a publication. A published alias needs a published head.

**Opting into union growth.** A frozen facade does not offer an accessor or
derive the union gained after its publication. `cargo run -p borzoi-astgen
--example generate` prints how many lines each frozen facade may opt into, and
`facade::opt_in` lists them; to offer one, copy it into the lock in its sorted
place and regenerate (the fixture stays as it is). Nothing requires it.

**Evolving a published facade.** When astgen fails on a frozen facade's
divergence, it lists each line under what can be done with it:

- *A kind it would read as a hole*: a new kind for one of its nodes, or a new
  node type. Append it to the lock (an allowed addition, which every consumer
  survives), or accept it with the reason.
- *No allowed evolution adds it*: a new variant of a frozen enum. The frozen
  facade does not get it, and the node reads there as an absent child. Record
  it in `ACCEPTED_DIVERGENCES` with the reason.
- *Locked but no longer derived*: the union or the table moved something the
  frozen facade keeps. If the union still backs it, the facade keeps reading
  it; accept it with the reason. If the union accessor was renamed, give the
  frozen accessor a delegate, `union.<new name>`, in place of an acceptance.
  If the union no longer backs it at all, the generator says the facade
  "cannot be generated from its lock"; that needs a hand-written compat shim.

**Re-baselining** (only before the first external release). Raise
`FREEZE_EPOCH` and regenerate: every published lock and fixture is rewritten
from today's derivation, the fixture of a facade that is no longer distinct is
removed, and `ACCEPTED_DIVERGENCES` should then be empty. The CI check passes
the change because the constant rose.

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

`syntax::{v4_6, v4_7, v5, v6, v7, v8, v9, v10, v11, preview}`, one per
`LanguageVersion` (astgen reads the version list from the feature table's
parser, so a new version brings its facade), each behind the cargo feature of
its name. A version whose derived lock equals the previous run's is
`pub use super::vX::*` (unless it is unpublished and the run's head is
published, D6), recorded as `alias-of` in its lock — so shared type
identity between aliases is part of the contract (accepted). An alias's
feature also compiles, and so exposes, the module it aliases. `preview` is
never an alias and never aliased, and sits behind `unstable-preview` (D11), so
no stable facade's `VERSIONS` names the preview version.

The distinct surfaces are {4.6, 4.7} (`v4_6`), {5, 6, 7} (`v5`, adding
`Expr::InterpString`), {8} (`v8`, adding `Expr::DotLambda`, `Expr::WhileBang`
and `Type::Intersection`), {9, 10, 11} (`v9`, adding `Type::WithNull`; it
serves 9.0 and 10.0, and the unpublished `v11` has the same surface but its
own types until it is published) and `preview` (adding `Expr::IndexFromEnd`).
A kind with no FCS `LanguageFeature` gets no interval, so it is in every facade
(`Expr::Typar` is in all of them).

### D9. Gate and surface are different questions; diagnostics carry a class

The table separates them: `introduced` alone drives the **surface**;
`fcs_stage`, `fcs_effect` and `borzoi` drive the **gate**. FCS checks two
features that have syntax kinds only in the type checker (`StringInterpolation`
5.0, FS3350; `FromEndSlicing` preview, FS3303 and only for a `^` index
argument). Each still gets a borzoi **parse-phase legality diagnostic**,
graded against FCS's check diagnostics (sema's
`check_time_gates_report_what_fcs_checks_report`), because without it
`view::<V>()` would have an uncovered hole. A `^e` that is not an index
argument, which FCS's checker rejects at every version (FS3534), draws that
error below preview as `FromEndSlicing`'s gate, so it lies under one too.

Every diagnostic carries a class (`parser::DiagnosticClass`, on each
`ParseDiagnostic`):

- `Legality { feature }` — tree-neutral: "this construct needs a newer
  version";
- `Recovery` — ordinary errors, and the gates whose FCS effect is `Reshape`
  or `Severity`.

A gate's class is not chosen by hand: `DiagnosticClass::of_gate` reads it off
the table's `fcs_effect`, `Legality` exactly when every effect is `Diagnose`
(vacuously so for a `check` row, as the two check-time gates are). Only the
gate producers (`parser::feature_gates`, the lex-filter's `FilterRun::gates`,
the `#elif` side channel, the `$$"""` lexer gate and
`parser::warn_directives`) build a classed diagnostic; every other producer's
problem reaches `Parse` as `Recovery` by construction. So the dotless
`float32` literal, the nullness annotation, `_.` patterns, `for _ =` and
`$$"""`, whose trees FCS changes below their versions, and the `#nowarn` /
`#warnon` features, which move other diagnostics' severity, report
`Recovery`; the other gated features report `Legality`.

`Parse::errors` minus the `Legality` ones stays FCS-*parse*-faithful
(`Parse::recovery_errors`). Every gate diagnostic also names its feature,
whatever its class (`ParseDiagnostic::gate`, set by the one constructor both
producers use), so a `Recovery` gate is told from a syntax error structurally.
P2 is restated over that field: `view::<V>()` is total when there are no gate
diagnostics, and otherwise every hole lies under one for its own kind's
feature.

Held by cst's `diagnostic_classes` tests: every live row of the table has a
fixture, and at every version below the feature's a `Legality` row's fixture
parses to the tree it parses to at the feature's version, with the same
`Recovery` diagnostics and only `Legality` errors naming that feature, while a
`Reshape` or `Severity` row's fixture moves across its version and never
reports a `Legality` diagnostic naming its feature.

### D10. Version invariance compares Recovery only

`SyntaxRecovery::of_guessed_version` (sema) keeps its full `[MIN, MAX]` range,
and `SyntaxRecovery` reads only `Recovery` errors.
`diagnostics_are_version_invariant` compares `Recovery` diagnostics at the two
ends of the ladder and the tree: an unflagged
`shape_depends_on_language_version` proves the tree the same everywhere, and a
flagged one is compared at both ends and at every shape band between them. So a
faithful legality gate (`#elif`, `_.M`, interpolated strings, `>|}`) does not
make every file using the construct version-dependent. A `Reshape` gate
(nullness below 9.0; `_.` patterns and `for _ =`, where FCS raises a parse
error and recovers; `$$"""`, which FCS's lexer reads differently) is
`Recovery` and stays version-dependent: every file holding a `member _.M` is
one, so a guessed version retains no diagnostics for it. So does a tree the
lex-filter reshapes with no diagnostic moving (a closer dedented to its
`let`'s column, across F# 6).

Two checks hold this:

- **The inference differential** (sema's `infer_legality_pin_diff`): for each
  gated `Legality` feature, a file with the construct inside an annotated
  declaration, checked by FCS at the version just below the feature's and at
  the feature's own. FCS's parse reports only FS3350 at the old pin, and every
  expression and binder type it reports is the same at both versions, and
  every type inference commits over our tree at the old pin is FCS's there.
  For a check-time feature FCS's parse reports nothing and its check only the
  feature's error. FCS's check of `StringInterpolation` abandons the string
  (`checkLanguageFeatureError` raises), typing it and what binds it as `obj`,
  so there our inference is held to FCS at 5.0 instead (`CHECK_ABANDONS`).
  The oracle's FCS (the `FSharp.Compiler.Service` package `tools/fcs-dump`
  references) predates the `#elif` check, so for `PreprocessorElif` it shows
  the types only (`ORACLE_PREDATES`). FCS refuses a version below 8.0
  (FS3880) unless `SKIP_VERSION_SUPPORTED_CHECK` is set, so every
  `tools/fcs-dump` oracle that pins a version sets it (the LSP crate's
  `fcs_dump_langversion_pin` holds each one to checking at 7.0).
- **The corpus census** (cst's `version_dependence_census`): every corpus
  file parsed at every version, each version graded against preview as a
  moved tree, a moved `Recovery` diagnostic, a moved `Legality` diagnostic
  only, or no change, and every file that moves pinned in
  `tests/manifests/version_dependence_census.txt`. On the pinned corpus
  691 files are version-dependent and 274 move only in `Legality`
  diagnostics, so a guessed version keeps their reading. The census also
  asserts that two versions agreeing on every `Recovery`-class feature parse
  every file identically, and that `diagnostics_are_version_invariant` agrees with the
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

**Deferred rows.** A row recorded `engaged = deferred(reason)` drives neither
a gate nor a facade interval (`DEFERRED_KIND_FEATURES` lists its kinds). Every
row is live or `Unmodelled` today: `INTERP_STRING_EXPR` (`StringInterpolation`,
5.0), `DOT_LAMBDA_EXPR` (`AccessorFunctionShorthand`, 8.0) and
`INDEX_FROM_END_EXPR` (`FromEndSlicing`, preview) went live in PR 3, so each
facade leaves out the kinds introduced after its version (PR 6).

**What each parse-relevant feature does** (PR 3; the `Unmodelled` rows are all
`checker-only`):

| Feature | Version | FCS | borzoi | Class |
|---|---|---|---|---|
| `SingleUnderscorePattern` | 4.7 | FS0010 at the `.` of `_.M` in a pattern, parse error and recovery | the error, over the 4.7 tree | `Recovery` |
| `WildCardInForLoop` | 4.7 | FS0525 at the `_` of `for _ =`, parse error and recovery | the error, over the 4.7 tree | `Recovery` |
| `StringInterpolation` | 5.0 | FS3350 at each interpolated string, in the checker | the same, while parsing | `Legality` |
| `ExtendedStringInterpolation` | 8.0 | lexes `$$"""` as `$"""`, FS1251 at the lexeme | the same | `Recovery` |
| `AccessorFunctionShorthand` | 8.0 | FS3350 at the `_.` of `_.M` | the same | `Legality` |
| `ParsedHashDirectiveArgumentNonQuotes` | 9.0 | FS3350 at an unquoted `#nowarn` argument, FS0203 at an `FS`-prefixed one, severity of FS0203 | the same | `Recovery` |
| `BetterAnonymousRecordParsing` | 10.0 | FS3350 as the lex-filter splits `>|}` / `@>|}`, at its lexbuf lexeme | the same (a model of the lexbuf lexeme) | `Legality` |
| `ScopedNowarn` | 10.0 | FS3350 at a top-level `#warnon`, FS0236 in a nested module, severity of FS0203 | the same | `Recovery` |
| `FromEndSlicing` | preview | FS3303 at a `^` index argument, in the checker (elsewhere FS3534 at every version) | the same, while parsing, and FS3534 elsewhere | `Legality` |

## The central invariant (and how it is enforced)

Soundness reduces to one property of the union: **it only ever *refines*
distinctions, never *coarsens* them.** The facades are kind-driven
classifications over a shared `SyntaxNode`, so this is cheap to test:

- **P1 — Gate completeness.** Every construct introduced after N draws a
  diagnostic under pin N. Per-feature fixtures, and differentially FCS at every
  version (`parser_diff_feature_gates`, 4.6 … 11.0 and preview).
- **P2 — Projection totality.** For a tree parsed at P, `view::<vP>()` is total
  when there are no gate diagnostics, and otherwise every hole lies under a
  gate diagnostic for its own kind's feature.
- **P3 — Round-trip identity.** For any `vP` node, lowering to the union and
  re-projecting yields the same node — "the union has not coarsened".
- **P4 — Interval consistency.** The union's classification refines every
  `vN`'s: no two kinds distinct in some `vN` collapse in the union.

P2–P4 are held over every facade, node type, dispatch enum and accessor by
cst's `facade_properties` tests, from a table (`matrix.rs`) astgen generates
from the locks. On fixtures (one per live `Kind` row), generated programs and
a sample of the corpus in the normal suite, and on the whole corpus in an
ignored sweep CI runs:

- P2: a *hole* (a node some union node type casts and no facade one does) has
  exactly a kind the table puts outside the facade's versions, and in a parse
  the facade serves it lies under a gate diagnostic for its own kind's
  feature, inside the hole's range: one whose `gate` is
  `Some(kind_feature(kind))` and whose class is `DiagnosticClass::of_gate` of
  that feature. Every diagnostic is held to the field's contract besides: a
  gate is on a feature newer than the parse's version and classed `of_gate`
  of it, and a `Legality { feature }` diagnostic has `gate == Some(feature)`.
- P3: a facade cast lowers, to the substrate and through `to_union`, to the
  same node, and away from holes each facade type casts what the union's does.
- P4: a facade value prints exactly as the union's (same variant, same
  node), so nothing a facade tells apart collapses in the union; and on an
  owner with no hole beneath it every facade accessor returns what the
  union's does. The normal suite must run an accessor of every return shape in
  every facade, and the corpus sweep every accessor (a construct no corpus
  file holds gets a fixture).

The gate diagnostic is found by its `gate` field, never its text. Its class
is `Legality { feature }`, except for nullness: `NullnessChecking`'s FCS effect
is `Reshape`, so its gate is a `Recovery` diagnostic, and a `WithNull` hole
under F# 8 lies under that. So P2 reads "every hole lies under a gate
diagnostic for its feature" rather than "under a `Legality` one".

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
- **PR 3 — faithful gates** (#21): every parse-relevant feature gated (the
  table above), the three deferred `Kind` rows live, the lex-filter's lexbuf
  model, the version-aware lexer (`tokens::lex_full_trivia_at`), the
  `#nowarn` / `#warnon` diagnostics (`parser::warn_directives`), and their
  differentials: `parser_diff_feature_gates` (including the `Reshape` gates'
  recovered trees), `diagnostic_classes` over every live row, and sema's
  check-time span differential.
- **PR 4 — the union behind `unstable-union`** (#13, #670): the hand-written
  union in a private `syntax::union`, re-exported only under the feature, with a
  featureless clippy and doc step in CI.
- **PR 5 — astgen owns every node** (D4): the standalone nodes and the value
  enums moved into astgen's tables (`generated::union_standalone`); astgen reads
  every accessor's signature out of `union.rs` and classifies its return shape,
  and emits the union's lock, `crates/cst/facades/union.lock` (D6).
- **PR 6 — deep facades for every version** (#14, #17, #12): one module per
  `LanguageVersion` (D8), each generated from its derived lock (D4, D6);
  `Parse::view` over the sealed `Facade` trait; `syntax::preview` behind
  `unstable-preview`; P2–P4 over every facade (`facade_properties`).
- **PR 7 — the freeze** (D6): `v4_6` … `v10` published, their locks stamped
  at `FREEZE_EPOCH` 1 and read, never written, with each frozen lock held to
  its version's derivation; a consumer fixture per distinct published facade;
  `tools/ci/facade-lock-check.sh` in CI; the synthetic-feature test; node and
  dispatch-enum re-casts taken to be fallible, so a facade's code is a
  function of its own lock.

## PR sequence

1. **Feature table (#15)**, this plan, stale-doc fixes. No behaviour change.
   *Landed.*
2. **Diagnostic classes** (D9): `Legality` / `Recovery`; `SyntaxRecovery` and
   version invariance over `Recovery` (D10); the version-dependence census.
   *Landed.*
3. **Faithful gates** (#21's remainder): `_.M`, interpolated strings, `^`
   from-end indexing, `_.` patterns, `for _ =`, `$$"""`, `>|}`
   (`BetterAnonymousRecordParsing`) — each gated, or `Unmodelled` with its
   reason. The three deferred `Kind` rows go live. *Landed.*
4. **Union behind `unstable-union`** (#13). Independent of 1–3. *Landed*
   (#670).
5. **astgen owns every node** and reads accessor signatures (D4); it emits the
   union's lock only. *Landed.*
6. **Deep facades for every version and preview** (#14, #17),
   `Parse::view::<V>()` (#12), generic P2/P3/P4. *Landed.*
7. **Freeze**: `PUBLISHED = v4_6 … v10`, the CI lock check, consumer fixtures,
   and a permanent synthetic-feature test (a fake feature and kind at "12.0",
   in memory: every published facade regenerates byte-identically, and the new
   facade differs by exactly that variant). *Landed.*
8. **The compiler upgrade** — the proof; `v11` is published after it
   ([Publishing a facade](#publishing-a-facade)).

## Still to do

Everything in PR 8 above. Beyond them:

- **Stale issues.** #14 (facades below 8.0) and #12 (`Parse::view`) are done
  by PR 6, and #17 (preview) is decided (D11).
- **The upstream `_.` check is gone.** dotnet/fsharp#20222 removes
  `SingleUnderscorePattern`'s parse error; once the table transcribes a
  compiler with it, the row's `fcs_sites` empties, `language_features` says to
  make it a `check` row, and every `member _.M` file stops being
  version-dependent.
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
  moves. It also starts an indented `#nowarn`'s directive range at the start
  of the line, where the table's compiler starts it at the `#`, which we
  follow (`an_indented_directive_range_starts_at_its_hash`).

## Open questions

- **Positional features and the surface.** A feature that gives an existing
  kind a new legal *position* (`Positional`) has no facade delta, only a gate.
  If a future positional feature needs a facade distinction, it needs a kind of
  its own first.
- **Type-tag views (#16).** A compile-time `parse::<V8_0>(src) -> Parse<V8_0>`
  layer, bounded by a sealed `surface(P) ⊆ surface(N)` trait, would make some
  misuse a compile error; it is O(versions²) trait impls (codegen-able).
  Deferred; the runtime-checked `view::<V>()` is the baseline.
