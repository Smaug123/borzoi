# AST versioning plan

> **Status (2026-07-15).** Partially landed. Stages 0–2 (the `LanguageVersion`
> value type #592/#597, threading it into `parse` with the first `#elif`
> legality gate #606, and the LSP `<LangVersion>` wiring #613) and the Stage 3
> typed-facade *mechanism* (generated `syntax::v8` / `syntax::v9` facades
> projecting the F# 9.0 `WithNull` delta over a runtime interval-table node gate)
> have landed. **Still open:** `Parse::view()`, the public-vs-private union API
> decision, unifying the duplicated interval source between `syntax/kinds.rs` and
> `tools/astgen`, and broadening interval rows below 8.0 — see **Still to do**.
> Companion to [`parser-plan.md`](parser-plan.md), which owns the parser; this
> doc owns the *public typed-AST surface* and how it stays stable as F# evolves.

## Problem

The typed AST is a public artifact: third-party tooling (and LLM agents) will
write code that matches on it. F# the language grows — new syntax, occasionally
relaxed restrictions, rarely a gated source-break — so our model of it must grow
too. The tension is the usual one for a closed sum type: adding a variant to a
dispatch enum (`Expr`, `Pat`, `Type`, `ModuleDecl`, `SigDecl` in
[`syntax/mod.rs`](../crates/cst/src/syntax/mod.rs)) is a breaking change for
every exhaustive `match` downstream. We want consumers to be able to:

1. pin to an F# language surface (e.g. 8.0) and get a typed AST that is **both
   stable** (never breaks under our upgrades) **and exhaustive** (the compiler
   tells them they've handled all of it); and
2. opt into a newer surface when they choose, without that being forced on them
   by an unrelated crate upgrade.

FCS and Roslyn both decline to solve this — FCS lets `SynExpr` grow and breaks
consumers on upgrade; Roslyn sidesteps closed sums with open virtual dispatch.
We can do better because we sit on rowan, where the untyped tree is already
stable and additive and the typed layer is a thin, regenerable projection.

## The core insight

A versioned typed AST is a **projection** with two directions, and they have
different totality:

- **Reading** parsed source — `private_union → vN`. **Partial** in general: a
  parsed tree may contain post-N nodes `vN` cannot represent.
- **Constructing / rewriting** — `vN → private_union`. **Total**: the supported
  surface only grows, so every `vN` tree embeds in the union.

The reading direction is the one that bites, and the *only* thing that makes it
total is a guarantee that the tree contains no post-N nodes. That guarantee is
exactly **LanguageVersion pinning**: parse at langversion N, and `union → vN` is
total by construction. Language-version support and versioned facades are
therefore not two independent features — the langversion is the **index that
makes the read-projection sound**. Everything language-evolution can throw at us
— new syntax, new legal positions for existing kinds, a langversion-gated parse
divergence — is absorbed by keying on the version. The one risk the version
index does *not* cover is **us re-modelling an already-frozen version**
(representation drift with no language change); §D6 / the round-trip property
contain that residual.

## Scope

- **In scope.** A first-class `LanguageVersion`; making the current typed facade
  the *private union*; public, frozen, per-version typed facades projected from
  it; the soundness invariant and the property suite that enforces it.
- **Out of scope (by choice).** Trivia and ranges are **not** versioned — the
  differential normaliser already elides them, so the versioned surface is
  *structural* only. The untyped rowan layer (`SyntaxNode`/`SyntaxToken`) is not
  versioned; it is the stable substrate (D2).
- **Free hand.** The repo is private; we may make arbitrary breaking changes to
  land this. The versioning we build is for *future external* consumers, not for
  today's callers.
- **Current typed-facade scope: `v8` + `v9`.** The original `v10` + `preview`
  starting scope proved insufficient because the only post-10.0 feature we model,
  `#elif`, is trivia rather than a typed AST node. The production slice therefore
  uses the first real structural delta we model, F# 9.0 nullness: `syntax::v9` is
  the union surface today; `syntax::v8` is the frozen facade that excludes
  `WithNull`. F# 10.0, 11.0, and `preview` remain parse-version values, not
  typed facade modules. We do **not** yet backfill 4.6–7.0 as separate facades;
  their interval rows are an open data-completeness task (Still to do).

## Settled decisions

### D1. Two orthogonal version axes — never conflate them

Two clocks, kept independent:

- **F# language version** (8.0 vs 9.0) — a property of the *source*. This indexes
  the public facades and is what gets *frozen*.
- **Crate SemVer** — a property of *our code* (we model `match` more precisely,
  fix a projection bug, add an accessor). Ordinary library versioning.

We must be able to ship F# 9.0 parsing support (a crate upgrade) **without**
forcing consumers off `ast::v8`, and to fix a modelling bug **without** inventing
a new F# version. Facades live at `ast::v8`, `ast::v9`, …; bug-fixes and new
accessors are SemVer events on the crate; a new `ast::vN` module appears only
when F# ships a syntax-affecting language version.

### D2. The private type is the *union of supported surfaces*, not "latest"

"Always the latest" is subtly wrong, and the wrongness is the *removal* case: if
F# 11 drops a construct 8.0 had, "latest" stops modelling it and `v8 → latest`
ceases to be total. Fix: the private representation is the **union over all
currently-supported surfaces**. Each construct carries an *interval*
(`introduced`, optional `removed`) and the union retains it as long as *any*
supported version needs it. Removal then needs no special case — it is one more
interval bound on the same table (D5).

The current [`syntax`](../crates/cst/src/syntax/mod.rs) facade *becomes* this
private union. The union is `syntax::v9` today; `syntax::v8` is the first frozen
projection, and the split is load-bearing because `v8::Type` excludes the F# 9.0
`WithNull` node the union accepts. The union facade is still publicly
re-exported from `syntax`; privatizing that boundary is an open decision (Still
to do). `removed` intervals stay unreachable for now, but the table carries the
field from the start so the format does not churn when a future version removes a
construct. The rowan substrate stays public — it is the backstop (D7).

### D3. LanguageVersion is a first-class parse input (the index)

Model it after FCS's
[`LanguageFeatures.fs`](../../fsharp/src/Compiler/Facilities/LanguageFeatures.fs):
a closed enum of concrete versions (`4.6 … 11.0`), plus `Default` (currently
10.0) and `Preview`. `parse` takes it explicitly and `Parse` records it:

```rust
pub enum LanguageVersion { V4_6, /* … */ V10_0, V11_0, Preview }
impl LanguageVersion { pub const DEFAULT: LanguageVersion = LanguageVersion::V10_0; }

pub struct Parse {
    pub root: SyntaxNode,
    pub errors: Vec<ParseError>,
    pub warnings: Vec<ParseError>,
    pub lang: LanguageVersion, // the surface this tree was parsed against
}
```

LanguageVersion has two parser roles:

- **Legality gate.** Reject post-N constructs under pin N, emitting a diagnostic
  — this is what keeps the parsed tree within `surface(N)` and makes `union → vN`
  total. The gate is **per-feature**, not per-surface: each feature is gated
  against its own introduction version (mirroring FCS's
  `langVersion.SupportsFeature`), so an unusual pin like 8.0 is gated correctly.
  The gate never alters the green tree (asserted by a property test): FCS
  feature-checks *recognised directives*, not emitted markers, so the driver
  records the checked spans and the parser turns them into diagnostics —
  matching FCS's `CheckLanguageFeatureAndRecover` (report, then parse anyway).
- **Parse divergence.** A handful of constructs *parse differently* by version
  (e.g. the int-overflow fallback in `directives/line.rs`); version-correct
  parsing. Not yet engaged — the modelled deltas are additive so far.

This is *parse, don't validate*: the langversion is part of the parse
configuration that defines the correctness envelope, not a post-hoc check.
Resolving a pin to the *nearest modelled facade* is a **view-layer** concern
(`Parse::view()`, Still to do), not a parse concern. The known view-layer
limitation: for an unmodelled older pin the LSP may *under*-report errors a
pinned compiler would raise, narrowed as we model more versions.

### D4. Public facades are frozen projections; the canonical view matches the parse version

For each supported version, `ast::vN` is a typed facade — the same
newtype-over-`SyntaxNode` pattern as today, but with **frozen dispatch-enum
variant sets and accessor signatures**. Because every node is a newtype over the
same `SyntaxNode`, a facade is really a *classification*:
`ast::v8::Expr::cast(node)` accepts the node iff its kind is in `surface(8.0)`,
and rejects (→ `None`) post-8.0 kinds.

The **canonical, always-sound** operation is to view a tree through the facade
matching its parse version:

```rust
let p = parse(src, LanguageVersion::V8_0);
let file: ast::v8::ImplFile = p.view();   // total: tree ⊆ surface(8.0) by the gate
```

`view()` is well-typed because `Parse::lang` records the version. Cross-version
reinterpretation (viewing an 8.0 tree through `ast::v9`) is sound only when
`surface(8.0) ⊆ surface(9.0)`, which *fails under removal* — so it is **not** the
main path; defaulting to exact-match sidesteps removal entirely. What "frozen"
means concretely: the **only** things frozen are (a) the dispatch-enum variant
*sets* and (b) accessor *signatures*. Adding an accessor, or a field FCS
exposes, is non-breaking and lands in the union.

### D5. Single source of truth: an interval-annotated grammar table → codegen

Do **not** hand-maintain N parallel facade hierarchies. Annotate each construct
once with its `(introduced, removed?)` interval — the natural home is alongside
the `SyntaxKind` enum in
[`syntax/kinds.rs`](../crates/cst/src/syntax/kinds.rs). From that one table
derive the **legality predicate** `is_legal(kind, version)` (the parser gate) and
each **frozen facade** `ast::vN` = union minus constructs whose interval excludes
N. Mirror FCS's `featureVersionMap` in `LanguageFeatures.fs` as the authority for
which feature landed in which version, and differentially test our gate against
FCS `--langversion` rejection. Codegen has landed for `syntax::v8` / `syntax::v9`;
the single-source-of-truth part has **not** — see Still to do.

### D6. The freeze cost is real, bounded, and paid consciously

Freezing `ast::v8` means we cannot fix a *modelling* bug in the 8.0 surface
without it being a breaking change. The discipline: freeze the **shape** (variant
sets + accessor signatures) of a published `vN`; land modelling fixes in the
**union** and in *unpublished* future facades; document `vN` as preserving its
original, possibly-imperfect shape for compat. This is the v1→v2→v3
edge-compatibility pattern applied to a library surface, where the "edge"
projects *outward* from the union and consumers are external — precisely the case
where once-legal-stays-legal is the correct default. The cost cannot sneak up on
us: the round-trip property (P3) fails the build the moment a union refactor
*coarsens* a distinction a frozen facade relies on.

### D7. rowan is the floor: incomplete, never wrong

The untyped green tree can represent any tree the parser produces. So the typed
facade's failure mode for anything unanticipated is **incompleteness, never
wrongness**: a consumer can always drop to `.syntax()` and walk rowan. A
construct we have not yet modelled is a documented *gap*, not a *lie*. Corollary:

- The untyped substrate (`SyntaxKind`) is **`#[non_exhaustive]`** — the one place
  non-exhaustiveness is correct (you match the kinds you know and ignore the
  rest). The honest, additive, lossy layer.
- The typed `ast::vN` dispatch enums are **closed and exhaustive** — the precise,
  frozen layer where consumers *want* total-coverage enforcement.

Do **not** add an `Unknown(SyntaxNode)` arm to a frozen facade enum — that
re-introduces the wildcard. New-version syntax under an old pin is a **parse
error** from the gate (D3), not an `Unknown` node; `.syntax()` is only for our
own modelling gaps.

## The central invariant (and how it is enforced)

Soundness reduces to one property of the private union: **it only ever *refines*
distinctions, never *coarsens* them.** As long as the union keeps every
distinction any supported `vN` relies on, every projection stays well-defined and
the round-trip is the identity. This is testable cheaply, because the facades are
kind-driven classifications over a shared `SyntaxNode`:

- **P1 — Gate completeness.** For every construct introduced after N, parsing it
  under pin N produces a diagnostic (the node never reaches a `vN` tree). Driven
  from per-feature fixtures and, differentially, from FCS `--langversion`
  rejection.
- **P2 — Projection totality.** For a tree parsed at P, `ast::vP::*::cast(root)`
  succeeds on every node.
- **P3 — Round-trip identity.** For any `vP` node `n`, lowering to the union and
  re-projecting to `vP` yields `n` (same underlying `SyntaxNode`) — the
  executable statement of "the union has not coarsened."
- **P4 — Interval consistency.** The union's classification refines every
  supported `vN`'s: no two kinds distinct in some `vN` collapse in the union.

With **two** typed surfaces these have teeth today: P1/P2 reject or locate
`WithNull` under `v8`; P3 holds for the `v8` projection; P4 guards that the union
(`v9`) keeps every distinction `v8` relies on. The nullness delta was chosen
deliberately small — a concrete, cheap delta on which to get the property suite
right before the surface count (and the cost of a mistake) grows.

## Landed stages (one line each)

- **Stage 0 — `LanguageVersion` value type** (#592/#597) — the enum (D3),
  [`from_lang_version_text`](../crates/cst/src/language_version.rs) mirroring FCS
  `getVersionFromString`, `DEFAULT = V10_0`, `Display`, ordering; exhaustive +
  proptest coverage.
- **Stage 1 — thread the version into `parse`; the first gate** (#606) —
  [`ParseOptions`](../crates/cst/src/parser/mod.rs) / `parse_with_options` (the
  four `parse*` wrappers default to `Preview`, zero caller churn), `Parse::lang`,
  and the per-feature `#elif` legality gate emitting FS3350 under any sub-11.0
  pin (recognised-directive oracle covering dead arms, nested-inactive branches,
  and the bare-vs-malformed edge cases; tree left unaltered). P1 landed in
  `crates/cst/tests/all/langversion_gate.rs`.
- **Stage 2 — consume the gate in the LSP** (#613) — `<LangVersion>` surfaced on
  `borzoi_msbuild::ParsedProject`, resolved in the LSP
  (`Workspace::lang_version_for{,_project,_linked}`; absent → 10.0, unrecognised
  → 10.0 + log, orphan → `Preview`) and threaded through every parse entry, so a
  default project with `#elif` now reports FS3350, matching FCS.
- **Stage 3 (mechanism) — typed-facade projection proof** (#618,
  [`ast-versioning-nullness-proof.md`](completed/ast-versioning-nullness-proof.md))
  — a distinct exhaustive `v8::Type` (union `Type` minus `WithNull`), its `cast`,
  the surface predicate, and `first_out_of_surface_type` in
  `crates/cst/src/syntax/projection.rs`, with the P-exclude/totality,
  round-trip, no-coarsen, and gate≡totality property suite.
- **Stage 3 (productionized for nullness)** — `tools/astgen` emits distinct
  generated `syntax::v8` / `syntax::v9` facades and the parser uses the runtime
  interval gate to report typed-node surface violations.
- **Stage 4 (codegen)** — generated per-version facades landed (the codegen half
  of D5); the single-source-of-truth half has not (Still to do).

## Still to do

### Stage 3 remainder

- **`Parse::view()`.** Add the canonical always-sound view (D4) that hands back
  the frozen facade matching `Parse::lang`. Nothing named `view` exists on
  `Parse` yet; this is where the "nearest modelled facade" resolution and the
  view-layer limitation noted in D3 live.
- **Union privacy decision.** The union facade is still publicly re-exported from
  `syntax` (no `pub(crate) mod union`). Decide whether to make that boundary
  private and, if so, how without breaking the rowan-substrate backstop (D7).
- **Broaden interval data below 8.0.** The current rows cover only the 8.0/9.0
  nullness slice; 4.6–7.0 are not yet modelled as separate facades. This is a
  data-completeness task, not a mechanism change.

### Stage 4 remainder — single source of truth for intervals

Codegen landed, but the interval data is **duplicated**: `syntax/kinds.rs`
carries the runtime interval table driving legality, while `tools/astgen`'s
`kind_introduced` carries the introduction data used for code generation. Promote
the `(introduced, removed?)` annotations to a single declarative source of truth,
seeded from FCS `LanguageFeatures.fs`, so the legality predicate and the facade
codegen both desugar from one table (D5). Optionally add the compile-time
`parse::<V>` type-tag strengthening (see Open questions).

## Open questions

- **Type-tag vs runtime-checked views.** The LSP reads langversion from the
  `.fsproj` *at runtime*, so the core `view()` must be runtime-grounded
  (`Parse::lang`). A compile-time-tag layer (`parse::<V8_0>(src) -> Parse<V8_0>`,
  with `view` bounded by a sealed `surface(P) ⊆ surface(N)` trait) would make
  some misuse a compile error — but encoding surface-subset over a finite version
  set is O(versions²) trait impls (codegen-able). Proposed: runtime-checked
  baseline; type-tag as an *optional* convenience for compile-time-known
  versions. Decide when the type-tag work lands.
- **`Preview` semantics.** FCS treats `preview` as version `9999`. Whether
  `ast::preview` is a published, frozen facade or an explicitly-unstable view is
  a policy call; lean unstable (never frozen) so preview features can re-shape
  freely.
- **Granularity of the gate.** Some evolution is "new legal *position* for an
  existing kind" rather than a new kind (relaxed-whitespace, nested updates). The
  gate predicate is therefore over *trees/positions*, not just the kind set — so
  part of the interval metadata lives in the parser/grammar, not only the kind
  enum. The single-source-of-truth table format (Stage 4 remainder) must carry
  positional features, not just per-kind introductions.
