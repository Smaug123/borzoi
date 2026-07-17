# Type checker plan

> **Status: in progress** (verified 2026-07-15). Phase 1 (intra-file name
> resolution) and Phase 2 (project + assembly environment) are **complete**. Phase
> 3 inference is **well underway**: 3.1, 3.2a–c, R1, R2, 3.3a–d, 3.x-inh, and the
> **interface walk** (IW-0–IW-2: member resolution for an interface-typed receiver
> — own + transitively-inherited-interface + `System.Object` members) have landed,
> and of the hard piles the census singled out, **overload resolution** has
> landed through OV-7 + OV-9 — OV-8 (betterness) is the only outstanding overload
> slice. Remaining Phase 3 work is the rest of the hard pile: extension members,
> generics/static/indexer/unit-return coverage, inherited dot-completion, SRTP,
> computation expressions, units of measure, and the interface walk's optional
> IW-3 tail (re-declaration hiding, qualified-path precision).
> Phase 4 type-error diagnostics have not started, except for the separate
> always-sound `use rec` semantic diagnostic. This document is the design; the
> staged implementation breakdowns (each stage on its own branch, with an FCS
> oracle) live in
> [`completed/sema-phase1-impl-plan.md`](completed/sema-phase1-impl-plan.md) and
> [`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md). It is deliberately
> front-loaded on **name resolution** and sketches the later inference /
> diagnostics streams at lower resolution; expect it to keep growing
> phase-by-phase, as [`parser-plan.md`](parser-plan.md) did with its 7.1–7.9
> sub-phases.

Design doc for the semantic-analysis layer that turns a parsed F# project
into resolved symbols and (eventually) inferred types, so the LSP can
answer go-to-definition, find-references, rename, hover, and completion,
and ultimately report type errors. Captures decisions made before
implementation started so future work can resume from a cold pickup.

The headline tension the user raised: is there a *reasonable incremental*
path to an F# type checker, or is it all-or-nothing? The answer this plan
commits to is **yes, by separating LSP features along their soundness
requirement** (see [D5](#d5-soundness-policy-deferred-vs-unresolved)).
Navigation/hover/completion degrade gracefully and can be shipped
incrementally; sound diagnostics cannot, so they come last.

## Scope

- **Input.**
  - The typed-AST view from `borzoi-cst` (`ImplFile`,
    `ModuleOrNamespace`, `ModuleDecl`, `Binding`, the `Pat` / `Expr` /
    `Type` DUs). The parser is still being ported, so this surface is
    **partial** and grows underneath us — the resolver must degrade, not
    panic, on shapes that don't exist yet (see
    [D6](#d6-tolerate-the-partial-parser)).
  - The owned symbol model from `borzoi-assembly` (`Entity`,
    `Member`, `TypeRef`, behind the `EcmaView` trait) for everything
    coming from referenced `.dll`s.
  - The Compile order and reference set from `borzoi-msbuild` /
    `project_assets` (already resolved by the LSP workspace).
- **Output.**
  - Phase 1–2: per-file a map from each name-use `SyntaxToken` range →
    `Resolution`, plus the `ExportedItems` the file contributes to files
    later in Compile order.
  - Phase 3+: a best-effort typed view (expression range → inferred
    `Ty`), `None`/`Unknown` where inference can't reach.
  - Phase 4: type-error diagnostics, gated to the language subset proven
    complete against the oracle.
- **Soundness.** The result DU distinguishes **`Deferred`** ("in
  scope-shape but we lack the machinery / parser support — say nothing")
  from **`Unresolved`** ("genuinely not found — may drive a diagnostic").
  Only `Unresolved` is ever allowed to surface an error. This is the
  whole reason the layer can be incremental and still honest. See
  [D5](#d5-soundness-policy-deferred-vs-unresolved).
- **Reference.** `dotnet/fsharp/src/Compiler/Checking/` — chiefly
  `NameResolution.fs(i)` (the scope / `NameResolutionEnv` model and
  `Item` taxonomy), `CheckPatterns.fs` (binder extraction),
  `Expressions/CheckExpressions.fs` (the sequential checking order),
  `ConstraintSolver.fs(i)`, `InfoReader.fs`, and `MethodCalls.fs`
  (member / overload resolution, for the inference phase). As with the
  parser, FCS source is **documentation, not a porting target** — the
  data structures are entangled with the `TcGlobals` / `cenv` mutable
  context we are explicitly rejecting (see
  [D2](#d2-functional-core-fold-over-compile-order)).
- **Oracle.** FCS via `tools/fcs-dump`, extended to emit
  `GetAllUsesOfAllSymbolsInFile` (symbol-use list) and, later, the typed
  expression types. See [D7](#d7-fcs-as-differential-oracle).

## Settled decisions

### D1. Placement: a new self-contained `crates/sema/` crate

`borzoi-sema`, depending only on `borzoi-cst` and
`borzoi-assembly` (and their dep `rowan`). It does **no IO**: the
caller supplies the parsed files, the resolved reference set, and the
Compile order. This mirrors the "reusable outside this repo" framing of
`cst`, `msbuild`, and `assembly` per `AGENTS.md`, and keeps the LSP
binary as the only impure consumer.

Name resolution is the crate's first module (`resolve`); inference has landed as
`crates/sema/src/infer.rs` in the same crate. They are separated by module, not by
crate, because inference consumes the resolver's output directly and
there is no third-party consumer that wants one without the other (the
boundary cost analysis from `gospel.md` — module boundaries are cheap,
crate/serialisation boundaries need justification).

Rejected: putting this in `crates/lsp/`. The semantic layer is pure and
reusable; only the wiring (LSP request handlers, file watching, the
incremental recompute trigger) belongs in the binary.

### D2. Functional core: fold over Compile order

The core signature is the design:

```rust
// Pure. The shell decides where `preceding` and `assemblies` come from.
pub fn resolve_file(
    file: &ImplFile,
    preceding: &ProjectItems,   // exported items from earlier Compile-order files
    assemblies: &AssemblyEnv,   // name-indexed view over referenced Entities
) -> ResolvedFile;
```

Project resolution is a fold over Compile order (F# is order-sensitive
*across* files: a file may only reference definitions from itself and
earlier files):

```rust
let mut items = ProjectItems::default();
for file in compile_order {
    let resolved = resolve_file(&file.ast, &items, &assemblies);
    items.extend(resolved.exports());
    store.insert(file.id, resolved);
}
```

This is dependency rejection (values in, values out — no resolver
context object reaching into a symbol-table service), and it is also the
incremental story: on edit, re-resolve the edited file and everything
after it in Compile order; files before it are untouched. No `cenv`,
no mutable `TcGlobals`-equivalent threaded through every function.

### D3. Result type: a closed `Resolution` DU

```rust
pub enum Resolution {
    Local(DefId),                                 // a binding in this file's scope tree
    Item(ItemId),                                 // a top-level def (this or earlier file)
    Entity(EntityHandle),                         // into a referenced assembly
    Member { parent: EntityHandle, idx: MemberIndex },
    Deferred(DeferredReason),                     // see D5 — never an error
    Unresolved,                                   // see D5 — the only error-eligible variant
}
```

Per "data descriptions over behavioural abstractions": resolution is an
inspectable value, not a callback. The set of binder kinds is likewise a
closed DU (`DefKind::{ Value, Parameter, PatternLocal, Module }`), DU-
over-flags so illegal states (e.g. "function parameter that is also
mutable") are unrepresentable. IDs (`DefId`, `ItemId`, `EntityHandle`)
are newtypes, not bare indices, per "no primitive obsession."

### D4. Scope model mirrors FCS's `Item` taxonomy, not its representation

Variant *names* track FCS's `NameResolution.Item` where there's a clean
correspondence (maximises oracle leverage; same rationale as the
parser's D2), but the representation is a parent-linked `Scope` tree with
**position-ordered** bindings, because F# shadowing is position-
sensitive: a use resolves to the latest binding of that name whose
defining range *precedes* the use.

The one subtlety the current AST already lets us get right, and the first
property test: **`let` vs `let rec` scoping**. For `let x = e` the binder
`x` is in scope only for the continuation, not for `e`; for `let rec x =
e` it is in scope for `e` too. Driven entirely by `LetDecl::is_rec()`,
which exists today.

### D5. Soundness policy: `Deferred` vs `Unresolved`

This is the load-bearing decision for incrementality. The layer is
allowed to be incomplete, but never *wrong* in a way that produces a
false diagnostic. Therefore:

- `Deferred(reason)` whenever a name is in scope-shape but we cannot
  resolve it *yet* — e.g. member access `expr.Foo` whose receiver type
  needs inference, or any construct the parser doesn't model yet. The
  LSP shows nothing for these; no diagnostic, no wrong go-to-def.
- `Unresolved` only when the name is genuinely absent from every scope
  and import we *do* model. This is the only variant permitted to drive
  an "undefined name" diagnostic, and even then diagnostics stay off
  until Phase 4.

This is `gospel.md` principle 5 (correctness over availability) applied
to a tool: producing a wrong red squiggle is worse than producing none.

### D6. Tolerate the partial parser

The parser port is ongoing (`parser-plan.md` phases 1–10 are done; phase 11,
error recovery, plus assorted long-tail slices remain). The resolver must
still treat absent AST shapes — error-recovery regions, unparsed slices — as `Deferred`
regions, never panic, and never assume a node exists. Concretely: walk
what the `ModuleDecl` / `Pat` / `Expr` DUs currently expose, and bucket
everything else into `Deferred`. As the parser grows, phases here unlock
without rework — the slots are designed in now. `open` / `open type` parse and
feed `ImportScope`; parser recovery and long-tail unparsed shapes still defer.

### D7. FCS as differential oracle

Extend `tools/fcs-dump` with a mode that emits, per file, FCS's
`GetAllUsesOfAllSymbolsInFile` as normalised JSON (symbol display name,
defining range, use range, is-definition). The property (per the
property-based-testing skill and `gospel.md` principle 4):

> For every symbol use FCS resolves in a corpus file, our `Resolution`
> either agrees (same defining range / same assembly entity) or is
> honestly `Deferred`. We never emit `Unresolved` where FCS resolved,
> and never point at the wrong definition.

This both measures coverage and tells us precisely whether the partial
parser or the resolver is the current bottleneck. The **expression-type
oracle now exists** (2026-06-28): `fcs-dump types` (single file) and
`types-census-batch` (stdin paths, tolerant, per-file) type-check with
`keepAssemblyContents` and walk FCS's elaborated typed tree (`FSharpExpr`),
emitting per expression span `{ range, kind, inferred-type }`. `kind`
classifies the machinery a resolver needs to assign the type (literal /
value-ref / function-vs-member call / overloaded / extension / trait-call /
…); nodes sharing an identical source range are de-duplicated to the
outermost (so the `inline`-operator fan-out in the reduced tree does not
dominate). Its first consumer is the type-scoping census in
[D9](#d9-scoping-evidence-the-bucket-census). Phase 3 inference now uses this
oracle through targeted expression- and binder-type differentials; the remaining
validation milestone is scaling expression-type diffing over a large corpus.
Scaling it from curated snippets to a large corpus of *real* projects is
its own milestone — see [Validation milestone — large-corpus FCS
differential](#validation-milestone--large-corpus-fcs-differential).

### D8. Inference substrate: `ena` worklist, not SMT

Phase 3 inference is **constraint generation → worklist solve**, not F#'s
mutually-recursive elaborator and not an external solver. Generation is a pure
fold over the resolved AST producing an inert `Vec<Constraint>`; it needs lexical
resolution (Phases 1–2, done) but **not** types. Crucially, type-directed name
resolution (`expr.Foo`) is *not* resolved during the fold: it emits a *suspended*
`HasMember(receiver, name, result)` constraint. The solver discharges equality
constraints by union-find, and each suspended member constraint is **keyed by its
receiver type variable and woken when unification makes that variable concrete**,
at which point the member is looked up against the `Entity`/`Member` model. That
wake step is the *only* place inference and name resolution meet; everywhere else
is the standard Hindley–Milner spine. The coupling F# smears across `TcExpr`
(threading `cenv`/`TcEnv`) is thus contained in one inspectable place. This is the
same *shape* FCS uses — `CheckExpressions.fs`'s `DelayedDotLookup` reifies
dot-chains as data; `ConstraintSolver.fs`'s `ExtraCxs` is a postponed-member-
constraint map keyed by typar stamp — minus the mutable context we reject (D2).

The unification substrate is **`ena`** (the union-find / unification-table crate
`rustc` itself uses) — neither hand-rolled nor an **SMT solver**. SMT is the wrong
tool and strictly *more* work: (1) inference must yield a *most general* type /
substitution, not the ground model SMT returns (it would pick `int` for an open
`'a`, destroying generalisation); (2) the type-directed member lookup is a
`.NET`-metadata callback, not a formula in any theory — SMT cannot run it mid-solve
except via a theory plugin, which is more plumbing than a worklist; (3) overload
resolution is "most-specific-applicable", a preference problem layered on
satisfiability, not plain SAT; (4) union-find is intrinsically *partial* — read off
solved variables, leave the rest `Deferred` (the D5 contract) — whereas SMT is
whole-query SAT/UNSAT/timeout; (5) it is a heavy native dependency against the
crates' self-contained ethos, with unpredictable latency an interactive LSP cannot
absorb. The lone corner where a real decision procedure fits is **units of
measure** (measures form an abelian group ⇒ measure-equality is linear algebra
over ℤ) — isolated, optional, and Gaussian elimination, not SMT.

### D9. Scoping evidence: the bucket census

Before committing to an inference engine we *measured* how much inference is
needed, via the `uses-census-batch` fcs-dump mode and the
`crates/sema/tests/uses_census.rs` corpus sweep — bucketing every symbol use FCS
resolves over the corpus by the machinery it requires (and
`uses_census_project.rs` for the isolation-bias bound). Over a stratified
~400-file sample of the F# repo (per-non-definition use):

- **B1 — no inference** (scope / import / path / assembly index, plus name-index
  rules for union *and active-pattern* cases): **~88–93 %**. The single biggest
  lever for navigation coverage is therefore *finishing name resolution's long
  tail* (Phase 2.3's `[<AutoOpen>]` / `open type` / single-segment module opens,
  and a case index spanning union cases *and active-pattern cases*) — **not**
  inference. Union cases and same-file active-pattern cases have since advanced;
  cross-file active-pattern export and parameterized active-pattern arity remain
  deferred. Active-pattern *case* resolution lives here: `match x with Even ->`
  resolves `Even` to its `(|Even|Odd|)` function by a name lookup; the scrutinee
  type is needed to *check* the match, not to *resolve the case name* (go-to-def /
  find-refs work without it).
- **B2 — shallow inference** (single-candidate instance member / field on a
  value): the **bulk** of the ~7–12 % that needs inference.
- **B3 — hard piles** (overload resolution, extension members): a **~1.4–2.6 %**
  tail, dominated everywhere by **overloaded instance members** (overload
  resolution) with a small **extension-member** tail. **SRTP, computation
  expressions, units of measure — *and* active patterns — are not inference hard
  piles:** the first three are statistically absent (CE builder-method *calls* are
  B2 ordinary member access; their difficulty is desugaring, a parser concern),
  and active-pattern cases are B1 name resolution (above).

Two biases, both stated: each file is checked **in isolation**, so cross-file
member targets drop out → the member fraction is a **lower bound** (worst on
interconnected `src/`; near-zero loss on self-contained `tests/` snippets, which
are thus the ~unbiased anchor). The `uses_census_project` probe bounds the gap by
checking the same files in isolation vs. as one project: on FCS's first 60
Compile-order files (its near-self-contained utility layer, the only slice cheap
enough to check pairwise — the large interconnected core is prohibitively slow)
resolving cross-file recovers just **+3 %** more member uses, and the bucket
*shares barely move* (B1 90.1 → 90.2 %, B2 7.4 → 7.4 %, B3 2.3 → 2.4 %). The
denser core would lose more in isolation, so +3 % is a floor; but the load-bearing
finding is that the bias adds roughly **proportionally** across buckets, so the
bucket shares this scoping rests on are stable regardless of it. The hardness split
*within* resolved members is intrinsic and **unbiased** either way.

**Conclusion.** A deliberately incomplete solver — `ena` + single-candidate member
dispatch, deferring every hard pile to `Deferred` (D5) — resolves on the order of
~98 % of uses (B1 + B2). The one inference hard pile that earns investment is
**overload resolution** (the dominant B3 sub-tag everywhere), with extension
members a small second; SRTP / CE / units stay deferred until a real corpus demands
them. Active-pattern and union *case* resolution is **not** inference — it is a B1
name-index rule and belongs with Phase 2.3's name-resolution long tail. This is
what scopes Phase 3 below.

**Type-axis companion census (verified 2026-06-28).** The original census above
measures the *name-resolution* axis (the currency when a use is a *name*). The
new `types-census-batch` oracle ([D7](#d7-fcs-as-differential-oracle)) lets us
measure the *expression-type* axis directly — the hover currency for **any**
expression, literals and compound expressions included — via
`crates/sema/tests/types_census.rs`. Over a stratified ~350-file sample of the F#
repo (each file *elaborated in isolation*; **93 %** produced a typed tree; 36.7 k
de-duplicated typed expression spans), bucketed by the machinery needed to assign
the **type**:

- **Lit (literal) ≈ 17.5 %** + **Spine (lexical / HM: value refs, function &
  static calls, constructors, lambdas, control flow, tuples / records / unions)
  ≈ 76.4 %** → **≈ 94 % of typed expressions need no type-directed member lookup
  and no overload resolution.** This is the type-axis confirmation of the B1+B2
  result: the HM spine (Phase 3.1–3.2) carries the overwhelming bulk.
- **Member (single-candidate instance member / field; needs the receiver type)
  ≈ 3.5 %** overall (5.8 % in dense `src/`) — the Phase 3.3 (`expr.Foo`) payoff,
  dominated by instance method calls (≈ 73 %) and record/class field reads
  (≈ 26 %). A **lower bound**: isolated checking degrades member access on
  unresolved sibling types to a typar rather than a `call:instance`.
- **Hard (overload / extension / SRTP) ≈ 2.6 %**, of which **overloaded calls
  are ≈ 97 %** (instance-overloaded ≈ 73 %, static-overloaded ≈ 24 %), extension
  members ≈ 3 %, and **SRTP trait calls ≈ 0.4 %**. This *re-confirms on the type
  axis* that **overload resolution is the one inference hard pile worth
  investing in**, with SRTP / CE / units statistically absent — exactly the
  name-axis finding.

Two type-axis nuances the name census could not see, because *typing an
expression* differs from *resolving a name*: an **overloaded static call** is
name-`B1` (the method group resolves by a type path) but **type-hard** (24 % of
the hard pile) — its *return type* is unknown until an overload is picked; an
**overloaded constructor** is the reverse, name-`B3` but type-`Spine`, since
`new T(…)` has type `T` regardless of which `.ctor` wins. Biases mirror the name
census (isolation → Member/Hard are lower bounds; corpus = FCS repo) plus an
**elaboration bias**: the population is FCS's *reduced* typed tree (matches
lowered to decision trees, pipelines / CEs / `inline` ops desugared), range-
de-duped to the outermost node — elaborated source spans, not a 1:1 CST image
(≈ 9 % of nodes FCS itself left with an unsolved typar in isolation).

## Phased plan

Each phase is intended to land as its own branch, stacked on the
previous, reviewable in isolation (same discipline as the other plans).
"Parser-gated" marks work blocked on `parser-plan.md` progress.

### Phase 1 — intra-file name resolution (no inference) — done

The keystone. Builds the `Scope` tree from `ImplFile → ModuleOrNamespace →
ModuleDecl` + a `DefId`/`Def` model, extracts binders from the `Pat` DU, resolves
`IdentExpr` / `LongIdentExpr` uses against it (everything unmodelled → `Deferred`),
and locks in `let`/`let rec` scoping (D4) and position-ordered shadowing against
the D7 oracle. Delivers, single-file: go-to-definition, find-references, rename,
document symbols, and in-scope name completion for locals, parameters, and
top-level values/functions. Staged breakdown (1.1–1.4) in
[`completed/sema-phase1-impl-plan.md`](completed/sema-phase1-impl-plan.md).

### Phase 2 — project + assembly environment — done

- **2.1** — `ProjectItems` + the Compile-order fold (D2); cross-file qualified
  resolution.
- **2.2** — `AssemblyEnv`: a name index `(namespace: &[String], name) → Entity`
  built by enumerating `Entity`s through `EcmaView`, resolving fully-qualified
  `LongIdentExpr` paths (`System.Console.WriteLine`) to `Entity` / `Member`.
- **2.3** — `ImportScope`: F#'s implicit auto-opens (`Microsoft.FSharp.Core` etc.)
  plus explicit `open` and the over-defer fixes (#280); since extended with
  `open type` static-member opens (#515), single-segment `open M` module-member
  opens under one unified source-ordered open precedence (#535, #538) with module
  abbreviations (#545), and `[<AutoOpen>]` module opens (#539) — see
  [`open-precedence-unification-plan.md`](open-precedence-unification-plan.md). A
  project module header merges with a same-named assembly namespace (falling
  through to the assembly when the module lacks the tail). The residual long tail
  (same-file current-module fall-through, minor open-precedence follow-ups, noted
  in the impl plan) is left for the large-corpus validation milestone to
  prioritise from real data.

### Phase 3 — best-effort inference (hover + dot-completion) — in progress (scoped)

The first phase that needs a real (if partial) type system, structured as
**generate → solve** (see [D8](#d8-inference-substrate-ena-worklist-not-smt)),
best-effort: annotate expression ranges with `Ty` where it can,
`Unknown`/`Deferred` otherwise (hover/completion degrade gracefully, D5).
The work is *scoped by the census evidence* in
[D9](#d9-scoping-evidence-the-bucket-census): the payload is **B2 — shallow,
single-candidate member access**, not the hard piles.

**Groundwork landed (2026-06-28):** the **expression-type oracle** and the
**type-scoping census** that this phase diffs against now exist — `fcs-dump
types` / `types-census-batch` ([D7](#d7-fcs-as-differential-oracle)) and
`crates/sema/tests/types_census.rs` (the type-axis companion in
[D9](#d9-scoping-evidence-the-bucket-census)). The staged Phase 3 breakdown
(each sub-stage on its own branch) lives in
[`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md).

**Landed sub-stages** (terse; per-stage detail — PRs, examples, gate conditions —
in [`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md)):

- **3.1** (#633) — `Ty` representation + sound literal typing (`ConstExpr` in a
  no-expected-type position), zero unification.
- **3.2a–c** — the HM spine (generate → solve): the `ena` unification substrate +
  `Ty::Var` + generate→solve plumbing (3.2a, #643); value-reference propagation
  (#646) and paren/reference-tuples `Ty::Tuple` (#663); the R1 no-shadow signal
  (#668) and R2 annotated-binder typing (#851–#864, per
  [`r2-annotation-typing-plan.md`](completed/r2-annotation-typing-plan.md)); the
  bidirectional `if`/`then`/`else` typer (#678), function/lambda body traversal
  (#685), `Ty::Fun` + monomorphic function-type emission (#701),
  `let`-generalisation + instantiation with canonical typar rendering
  (`let f x = x` ⇒ `'a -> 'a`, #707), and function application v1 (#712).
- **3.3a–d** — `expr.Foo` member access, emitted as a **suspended `HasMember`
  constraint** keyed by the receiver type variable and woken when unification makes
  it concrete, then looked up against the `Entity`/`Member` model: single-candidate
  field / non-indexer property (3.3a, #715), LSP member-resolution
  (`InferredFile::member_resolutions`, the resolver's `Resolution::Member` shape)
  feeding hover / go-to-def plus a dot-completion handler (3.3b, #725), the
  completeness-gated, coercion-free `ArgCheck` application wake with deferred
  poisoning (3.3c, #727), and single-candidate method-call typing to the method's
  return type behind an arity gate (3.3d, #732). The ~7–12 % B2 payoff.
- **3.x-inh** (#735) — member inheritance: a base-chain walk lets inherited fields,
  properties, and single-candidate methods type and resolve to their declaring base
  entity. Dot-completion remains exact-entity-only (no inherited members).

**Outstanding — the F# inference hard piles**, each optional and
`Deferred`-on-failure, in the priority the census dictates.

- **Overload resolution** — the dominant B3 sub-tag everywhere — has **landed
  through OV-7 + OV-9** (instance and static calls, the applicability matcher,
  cross-assembly method-group dedup, the corpus differential); **OV-8 (betterness)
  is optional, data-gated, and the one outstanding overload slice**. The first
  attempt — a cheap *arity-unique* shortcut — was abandoned after four `codex`
  rounds with one root cause: the real method group includes inherited members
  (base classes, `System.Object`, interfaces) and optional/`params`/`out`
  expansions an exact-entity arity scan can't see, so arity is not a sound proxy;
  overload resolution needs the **complete method group** and argument-type
  matching. The full de-risked design — FCS's algorithm with citations, the
  empirical landmine catalogue, the two-sided sound commit rule, and the OV-0–OV-9
  stage breakdown — is
  [`overload-resolution-plan.md`](overload-resolution-plan.md).
- **Interface walk — landed (IW-0–IW-2, [`interface-walk-plan.md`](interface-walk-plan.md)):**
  member resolution for a receiver whose static type is an interface (its own +
  transitively-inherited-interface + `System.Object` members), the last structural
  gap the complete method group needed for an interface receiver. Class/struct
  receivers are unchanged — FCS does not walk their interfaces
  (`followInterfaces=false`). The optional **IW-3** tail (re-declaration hiding,
  `qualified_path_occupied` precision, inherited dot-completion for interfaces)
  stays deferred, data-gated.
- **Still deferred:** **extension members** (out of scope for all of 3.3),
  generics / indexers / statics on the member wakes, **inherited dot-completion**,
  and **SRTP** (`^T` member
  constraints) / **computation-expression desugaring** / **units of measure** —
  the last three until the oracle shows a corpus that needs them.
  [D9](#d9-scoping-evidence-the-bucket-census) found SRTP/CE/units statistically
  absent, and active-pattern / union *case* resolution to be B1 name resolution,
  not inference — it belongs with Phase 2.3's long tail, not here. Per-pile detail
  lives in [`sema-phase3-impl-plan.md`](sema-phase3-impl-plan.md).

### Phase 4 — sound diagnostics (type errors) — not started

Last, because it is the part that does **not** degrade gracefully. Enable
error reporting only for the language subset proven complete against the
oracle; suppress diagnostics inside any declaration containing a
construct we don't fully model (track a "completeness frontier" per
declaration). A `Deferred` anywhere in a declaration's subtree
suppresses that declaration's type-error diagnostics.

### Validation milestone — large-corpus FCS differential

The endgame the per-stage [D7](#d7-fcs-as-differential-oracle) oracles
build toward: run the resolver (and, once it lands, inference) against FCS
over a **large corpus of real F# projects**, not just curated snippets. A
corpus runner enumerates projects and, for each, evaluates the `.fsproj`
(`borzoi-msbuild`) for Compile order, resolves the reference set
(`project_assets`), and feeds the ordered files + refs to both sides — FCS
via `fcs-dump uses-project` (`BORZOI_FCS_EXTRA_REFS` for the refs) and
our `resolve_project` with an `AssemblyEnv` over the same DLLs — then diffs
with the D7 agree-or-`Deferred` property. Output: pass/fail on
**soundness** (never *wrong* at a use FCS resolves, never `Unresolved`
where FCS resolved) plus a **coverage** number (% of FCS-resolved uses we
agree on vs `Deferred`) — a metric to prioritise by, not a gate.

**Depends on the LSP project-loading** (the deferred LSP-wiring follow-up,
tracked in `docs/fsproj-consumption-plan.md`): the runner reuses the same
`.fsproj` → references → Compile-order machinery the LSP needs rather than
reimplementing it, so it lands *after* that wiring. The oracle tool,
byte-offset normalisation, and the property itself already exist (Phase 2
Stages A–D); what is new is the runner and a curated corpus (e.g.
the F# compiler source, FSharp.Core, the SDK samples, real repositories). FCS over
a large corpus is slow but offline and parallelisable; `uses-project` plus
build-once `fcs-dump` keep it tractable.

This is the systematic safety net that makes shipping the resolver with
possible edge cases acceptable: any *wrong* resolution on valid code — the
dangerous class (e.g. the project-vs-assembly shadowing edges hand-found in
Stage D review) — surfaces at the FCS use site across thousands of real
uses, rather than relying on a reviewer to imagine the shape.

**Caveat (calibration).** The property iterates *FCS's* uses, so it catches
a wrong or missing resolution **at a use FCS reports**; it does not catch a
*spurious* resolution at a spot where FCS reports no use at all — mostly
invalid / mid-edit buffers, which the LSP does see. Closing that needs
either a broken-code corpus or a stronger "never resolve where FCS does not
(modulo our known sub-range conventions)" property; both are additive
follow-ups, not blockers for the soundness net on valid code.

Phase 3 uses the same runner family for expression and binder types; the
remaining work is broad large-corpus expression-type diffing against FCS over
real projects.

## Open questions

- **`Ty` interning / representation** — the representation is settled enough for
  Phase 3's current solver. Interning remains a later optimisation only if
  profiling demands it.
- **Incremental granularity** — Compile-order file-level recompute (D2)
  is the v1; sub-file incrementality piggybacking on rowan's green-node
  sharing is a later optimisation, only if profiling demands it.
- **`EntityHandle` identity** — flattened per-project assembly indexing is the
  current design. Richer identity / collision handling is a future correctness or
  performance refinement if evidence demands it.
