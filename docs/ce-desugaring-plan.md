# Computation-expression desugaring plan — sema Phase 3.x

> **Status: design, no stages landed** (written 2026-07-18). This is the
> sub-plan [issue #30](https://github.com/Smaug123/borzoi/issues/30) asked for
> — the census ([type-checker-plan.md
> D9](type-checker-plan.md#d9-scoping-evidence-the-bucket-census)) found CEs
> statistically absent from the corpus and deferred this pile behind corpus
> demand, but the pile has been deliberately pulled forward. Everything here was
> verified against the FCS source checkout (`../fsharp`; all `FCS:` citations
> are paths under `src/Compiler/`) and against the empirical probes in §3, run
> 2026-07-18 through the real `fcs-dump uses` / `types` / `binder-types`
> oracles.
>
> **The headline dependency, stated up front:** every user-visible CE payoff is
> a *type* (hover on a `let!` binder, dot-completion on a CE-bound value), and
> every realistic builder's types are generic instantiations (`Async<'T>`,
> `option<'T>`, `Task<'T>`). `Ty::Named` carries no generic-argument list today
> ([`crates/sema/src/ty.rs`](../crates/sema/src/ty.rs), the documented
> "no-args-yet" decision), and member wakes defer generic methods
> ([overload-resolution-plan.md §5](overload-resolution-plan.md)). So the
> typing stages of this plan (CE-4 onward) are **hard-gated on the `Ty`
> generic-args substrate**, which the overload plan already names as needing
> its own pre-requisite plan. The stages before that gate (the oracle harness,
> the body-furniture typing, the pure desugarer core) are real, independently
> valuable, and unblocked today.

## 1. What already works, and what this plan actually adds

The name "desugaring" makes this sound like parser work. It is not — the
parser and name resolution are done:

- **Parser/CST: complete and FCS-diffed.** Every CE construct is modelled and
  covered by `parser_diff_compexpr.rs` (1000+ lines of diff cases):
  `COMPUTATION_EXPR`, `LET_OR_USE_EXPR` with `is_bang()` (and `and!` grouped as
  sibling `BINDING`s), `YIELD_OR_RETURN_EXPR`/`YIELD_OR_RETURN_FROM_EXPR`,
  `DO_BANG_EXPR`, `MATCH_BANG_EXPR`, `WHILE_BANG_EXPR`, `JOIN_IN_EXPR`, and the
  comprehension arrow (`for p in e -> b` lowering the arrow to an implicit
  yield, as FCS's parser does). Custom operations (`select`, `where`) are plain
  `APP_EXPR`s on both sides — FCS has no custom-op syntax node either. Known
  parser gaps, tracked elsewhere and **not** blocking this plan: non-block bang
  binders reject cleanly (`docs/fcs-divergences.md`), object-expression
  `with`-localBindings in braces defer.
- **Name resolution: complete and FCS-diffed.** `let!`/`use!`/`and!` binders
  push real scope frames with deconstruction-pattern semantics
  ([`resolve/exprs.rs`](../crates/sema/src/resolve/exprs.rs), the
  `Expr::LetOrUse(e) if e.is_bang()` arm); `match!`/`while!`/`for` scope like
  their non-bang forms; `and!` RHSs correctly cannot see each other's binders.
  Differentials in `resolve_diff.rs`, `resolve_assembly.rs`,
  `resolve_types.rs`, `use_rec.rs`. Probe P1/P5 (§3) confirm the FCS uses
  picture is already satisfied: go-to-def / find-refs / rename on CE-bound
  names **work today**.
- **Inference: nothing.** Every CE variant falls into `infer_expr_inner`'s
  catch-all (`mark_incomplete()` + `None`), and the builder application
  short-circuits even earlier: `infer_callee` on an ident with no in-file def
  defers, so the `Computation` argument is never walked. No `def_type`, no
  `type_at`, no `member_resolutions` inside any CE body. Sound (D5 silence),
  and completely dark.

**What this plan adds:** the member-directed desugaring core plus the
constraint generation over it, so that CE-typed bindings, `let!` binders, and
CE-body expressions get ground types — lighting up hover, dot-completion, and
member go-to-def inside CE bodies. The LSP consumers already exist and need no
change (they read `InferredFile`, which is exactly where the new types land).

## 2. FCS's algorithm (the reference; condensed, with citations)

Primary source: `Checking/Expressions/CheckComputationExpressions.fs`
(**CCE.fs**, 3124 lines); dispatch in
`Checking/Expressions/CheckExpressions.fs` (**CE.fs**).

### 2.1 Architecture: a member-directed syntax rewrite, builder type first

The desugaring is a **syntax→syntax rewrite run before type-checking the
result, whose shape is chosen by consulting the builder type's method table**.
It is neither purely syntactic nor fully type-directed.

- The builder expression is **checked first**: `TcApplicationThen`
  (CE.fs:8841–8844) sees `leftExpr { comp }` with `leftExpr` already typed, and
  hands `TcComputationExpression` (CCE.fs:2954) the checked builder expr and
  its type `builderTy`.
- Method-existence probes on `builderTy` are computed **up front** and include
  in-scope **extension members** (`hasMethInfo`, CCE.fs:181–184, via
  `AllMethInfosOfTypeInScope`): `Source` (2974), `Quote` ⇒ auto-quote (2978),
  the `[<CustomOperation>]` member scan (2980, §2.4), and
  `enableImplicitYield` = feature ∧ `Yield` ∧ `Combine` ∧ `Delay` ∧ body
  `YieldFree` (3015–3020).
- The core recursion (`TryTranslateComputationExpression`, CCE.fs:1024–2400) is
  CPS: `translatedCtxt` is the surrounding translated context with a hole;
  binding constructs recurse with an extended continuation.
- **Outer wrap** (CCE.fs:3068–3099), inside-out and each conditional on the
  method existing: `Delay` → `builder.Delay(fun () -> body)`; `Quote` →
  `<@ … @>`; `Run` → `builder.Run(…)`. Then the whole thing is a
  `fun builder -> …` lambda checked against `builderTy -> overallTy` and
  beta-reduced (3113–3122).

### 2.2 The translation table

Every synthesized call goes through `mkSynCall` (CCE.fs:90–100), which builds
`builder.M(args)` **with a synthetic range** (`m.MakeSynthetic()`); the user's
sub-expressions are spliced in with their original ranges. Required methods
error via `requireBuilderMethod` (CCE.fs:1008–1010) when absent.

| Construct | Emitted | Requires | Member-directed choice |
|---|---|---|---|
| `let! p = e in b` | `Bind(Src?(e), fun p -> b')` | `Bind` | **`BindReturn`** instead iff feature AndBang ∧ body is a "simple return" (`convertSimpleReturnToExpr`, CCE.fs:2704–2792) ∧ `BindReturn` exists (2648–2678) |
| `let! … and! …` | `BindNReturn` / `BindN` / `MergeSources(K)`-tree + `Bind` | per branch | fully member-directed ladder (2089, 2123, 2160–2169) |
| `use! p = e in b` | `Bind(Src?(e), fun p -> Using(p, fun p -> b'))` | `Using`+`Bind` (1942–1943) | `use! … and!` is an error |
| `use p = e in b` | `Using(e, fun p -> b')` | `Using` (1927) | |
| `do! e; rest` | rewritten to `let! () = e in rest` (1720–1767) | via `Bind` | |
| `do! e` (final) | `let! () = Src?(e) in (return () \| Zero)` (2854–2903) | | `Return ()` iff `Return` exists ∧ `Zero` isn't `[<DefaultValue>]`-marked; tail-call `ReturnFromFinal`/`YieldFromFinal` variants if present |
| `return e` / `yield e` | `Return(e)` / `Yield(e)` (2381–2398) | `Return`/`Yield` | |
| `return! e` / `yield! e` | `ReturnFrom(Src?(e))` / `YieldFrom(Src?(e))` (2326–2379) | ditto | `…Final` variant iff tailCall ∧ feature ∧ method present |
| `ce1; ce2` (ce1 a CE construct) | `Combine(c1, Delay(fun () -> c2))` (1665–1716) | `Combine`+`Delay` (1697–1698) | |
| `e1; rest` (e1 plain) | plain `Sequential` — no builder call (1770–1798) | — | under `enableImplicitYield`, a type-directed seq-vs-`Yield` node instead |
| `if g then t` (no else) | `if g then t' else Zero()` (1819–1830) | `Zero` (1821) | |
| `if/then/else`, `match`, plain `let` | translated branch-wise, **no builder call** (1800–1818, 2246–2255, 1832–1888) | — | |
| `match! e with cs` | `Bind(Src(e), function cs')` (2257–2279) | `Bind` (2265) | |
| `while g do b` | `While((fun () -> g), Delay(fun () -> b'))` (1375–1407) | `While`+`Delay` | |
| `while! g do b` | purely syntactic pre-rewrite into `let!`+mutable+`while` (1409–1543) | via Bind/While | |
| `for p in e do b` | `For(Src?(e), fun p -> b')` (1287–1352) | `For` (1313) | `for i = a to b` lowered to `ForEach` first |
| `try e with cs` | `TryWith(Delay(fun () -> e'), function cs')` (2281–2324) | `TryWith`+`Delay` | |
| `try e finally u` | `TryFinally(Delay(fun () -> e'), fun () -> u)` (1545–1588) | `TryFinally`+`Delay` | |
| empty / implicit zero | `Zero()` (1597–1613) | `Zero` | |

`Source` wrapping applies to the RHS of `let!`/`use!`/`and!`/`for`/`match!`/
`yield!`/`return!` **iff** the builder has a `Source` method (CCE.fs:103–112).

### 2.3 The paths that are *not* builder CEs

- **`seq { … }` uses no builder.** CE.fs:8770–8787 flips the
  `ComputationExpr` flag when the applied function is the library `seq` value
  and routes to `TcSequenceExpressionEntry`
  (`CheckSequenceExpressions.fs:456`), a direct type-check emitting `Seq.*`
  library calls. Probe P3 (§3) shows the observable difference: `seq` is a
  single (not doubled) `Operators.seq` use and the body lowers to
  `call:function` nodes.
- **List/array comprehensions** (`[ … ]` / `[| … |]`) are
  `TcArrayOrListComputedExpression` (`CheckArrayOrListComputedExpressions.fs`),
  sequence-expression checking plus a collector. Out of scope here; if ever
  needed they are their own plan.

### 2.4 Custom operations (query expressions)

Identified **by member attributes, before translation**:
`getCustomOperationMethods` (CCE.fs:186–266) scans builder members for
`[<CustomOperation>]` and reads the flag args (`MaintainsVariableSpace`,
`AllowIntoPattern`, `IsLikeZip/Join/GroupJoin`, `JoinConditionWord`,
`[<ProjectionParameter>]`). Their presence makes the CE query-like: clause
active patterns reinterpret the body (CCE.fs:596–898), `ConsumeCustomOpClauses`
(2402–2627) emits `builder.Op(prior, args)` with projection-parameter lambdas,
and the variable-space machinery re-runs pattern checking to learn bound names.
This plan **defers all of it** (§5); the sound gate is cheap — any
`[<CustomOperation>]` member (or `Quote`, or the builder being FSharp.Core's
`query`) ⇒ defer the whole CE.

### 2.5 Ranges and the symbol-use sink — why the oracle looks the way it does

Two FCS facts explain everything the probes (§3) observed:

- **Every synthesized builder call has a synthetic range** whose *coordinates*
  are keyword/construct-derived (`mkSynCall` does `m.MakeSynthetic()`; the
  synthetic bit is flag `code2` in `Utilities/range.fs:359–360`). User
  sub-expressions keep their real ranges.
- **The name-resolution sink drops synthetic ranges** (`allowedRange m = not
  m.IsSynthetic`, `NameResolution.fs:2195–2196`). Hence probe P1/P5: no
  `Bind`/`Return`/`Zero`/`Combine` symbol uses **ever** appear in
  `GetAllUsesOfAllSymbolsInFile` — the uses differential does not require us to
  resolve builder methods. What *is* recorded, at real ranges: the builder
  value itself (`Item.CustomBuilder`, CCE.fs:2966–2970 — the doubled head use)
  and custom-operation keywords (`Item.CustomOperation` at the operator token).
- The **typed tree** (the `fcs-dump types` population) is *not* filtered:
  synthesized calls appear at their coordinate ranges, and the oracle's
  outermost-per-range dedup means a synthesized call **shadows** a user
  expression sharing its span (probe P2: `a + 1` under `return` reports as the
  `Return` call typed `option<int>`, not `int`).

## 3. Empirical probe catalogue

Run 2026-07-18 through `fcs-dump` against net10/latest FSharp.Core. Each is a
regression the CE-0 harness encodes.

| # | Snippet (essence) | Finding |
|---|---|---|
| P1 | `async { let! x = async { return 1 }; let y = x + 1; return y }` | Uses: builder head `async` doubled at its ident range; `x`/`y` are DEFs; **no builder-method uses**. Types: head ident span = whole-CE type `FSharpAsync<Int32>` (the beta-reduced application, §2.1); `let!` binder span = `value : Int32`; the plain `x + 1` keeps its ordinary node; `Return(y)` sits at `y`'s range; `Delay` thunks appear as `Unit -> …` lambdas at synthesized spans |
| P2 | in-file `OptionBuilder` (`Bind`+`Return`), `opt { let! a = Some 1; return a + 1 }` | head span = `FSharpOption<Int32>`; binder `a : Int32`; **`Return` call shadows `a + 1`'s span** (reports `option<int>`, `call:instance`); the continuation lambda spans the `return a + 1` statement, typed `Int32 -> FSharpOption<Int32>` |
| P3 | `seq { for i in 1..3 -> i * 2 }`, `[ for … ]`, `[\| … \|]` | `seq` is a **single** `Operators.seq` function use; no builder machinery; bodies lower to `Seq`-shaped `call:function` nodes; array literal is a plain `new-array` |
| P4 | `task { let! x = …; return x + 1 }` | head = `Task<Int32>`, binder `x : Int32` clean; **interior spans report `ResumableCode<…>` state-machine types** — interior emission for `task` is a non-goal |
| P5 | `async` with `if`-no-else, `do!`; `use!`/`match!`/`for`/`while`/`try-with` sweep | still zero builder-method uses; all binders (`use! d`, `for i`) are DEFs. The `do!`-Bind call sits at the **`do!` keyword's** range; the no-else `if` node itself carries the builder type (`Zero` inserted invisibly) — synthesized-range conventions vary per construct |
| P6 | `OptionBuilder` + `MergeSources`, `let! a … and! b … return (a, b)` | works with `Bind`+`MergeSources`+`Return` only (no `BindReturn` needed); `MergeSources` call spans both sources; binders `a`, `b` DEFs |
| P7 | `query { for x in [1;2;3] do where (x > 1); select (x * 10) }` | **custom-op keywords ARE symbol uses** at their token ranges (`where`, `select`); `for x` binder is a DEF; the source list's span is shadowed by the `Source` wrap (`QuerySource<…>`) |
| P8 | `binder-types` on nested/CE binders | the oracle reports top-level binders and curried params only — CE-internal binder types must diff through the `types` oracle's `value` nodes at binder spans (P1/P2), not `binder-types` |

## 4. Design

### CE-D1. Desugar to an inert core IR, not synthetic CST

The desugarer is a pure function

```rust
fn desugar(body: &Expr, methods: &BuilderMethods) -> Result<CeCore, CeDefer>
```

producing a data description (`CeCore`): a tree of `Call { method, args }`,
`Lambda { pat, body }`, `MatchLambda`, `Splice(Expr)` (a borrowed user
sub-expression), `Sequential`, `If`, `Match` — mirroring §2.2's output shapes,
each node carrying the range FCS would give it plus its synthetic bit.
Constraint generation then interprets `CeCore` exactly as it interprets real
AST: a `Call` becomes the same suspended `HasMember { kind: Method }` the 3.3
machinery already wakes, a `Splice` recurses into ordinary `infer_expr`. No
rowan node synthesis, no framework: data in, data out, and the translation is
inspectable and property-testable in isolation (per `gospel.md`,
data descriptions over behavioural abstractions).

### CE-D2. `BuilderMethods`: probe the builder like FCS does, or defer

FCS's existence probes see **extension members in scope** (§2.1) — the same
landmine as overload resolution's P15. `BuilderMethods` is built from the
builder's resolved entity via the `AssemblyEnv`/project-item member model,
gated exactly like OV-6: if the `ExtensionScope` says an in-scope extension
*might* contribute a member with any name the translation would probe
(`Bind`, `Delay`, `Run`, `Quote`, `Source`, `BindReturn`, …), **defer the
whole CE**. This gate is what makes `task` defer naturally (its builder
surface lives in `TaskBuilderExtensions` priority extensions —
[`resolve_fsharp_core.rs`](../crates/sema/tests/all/resolve_fsharp_core.rs)
already pins them unmodelled) while `async`/`option`-style builders pass.
Skipped members on the entity (`Entity::skipped_members`) ⇒ defer likewise.

Member-directed choices are reproduced **exactly or not at all**:

- `BindReturn` present ∧ the body could be a "simple return"
  (`convertSimpleReturnToExpr` territory) ⇒ defer until CE-6 models the rule.
  Rationale: the choice changes which synthesized calls exist and hence the
  §CE-D3 range picture; guessing is wrongness, not incompleteness.
- `ReturnFromFinal`/`YieldFromFinal` present ⇒ defer the `return!`/`yield!`/
  final-`do!` forms (rare members; feature-gated, see langversion below).
- `Quote` present, or any `[<CustomOperation>]` member, or the builder is
  `query` ⇒ defer the CE wholesale (§2.4).
- `Delay`/`Run`/`Source` presence is *modelled* (they only add calls, they
  don't fork the translation), from CE-4 on.

**Langversion.** The translation is feature-gated in several places (AndBang,
ReturnFromFinal, ImplicitYield, …). We assume latest-langversion, matching the
oracle; every place where a *lower* real-world langversion would produce a
different translation is covered by a defer rule above (the feature-dependent
choices are exactly the `BindReturn`/`…Final`/implicit-yield/custom-op forms we
defer), so mis-assumption can cause deferral, never wrongness.

### CE-D3. The commit discipline: head + binders first, shadow-masked interiors later

The differential direction is the house one: iterate **our** emissions, FCS
must agree at that exact range (never over-claim). §2.5 makes interior spans
treacherous, so commits are staged:

1. **Head span** (CE-4): the whole-CE type, emitted at the builder
   expression's range — where FCS's beta-reduced application lands (P1, P2).
   The `App` node's own (whole-`b { … }`) span gets the same type only if the
   harness confirms FCS emits there; otherwise head-ident range only.
2. **Binder spans** (CE-4): `let!`/`use!`/`and!`/`for` pattern binders get
   `def_type` from the woken `Bind`/`For` continuation-parameter type; the
   `types`-oracle `value` nodes at those spans (P8) are the differential
   currency.
3. **Interior spans** (CE-7): a span may carry an ordinary user-expression
   type **only when no synthesized node shares it**. Because we synthesize the
   same nodes FCS does (CE-D1 carries their ranges), the shadow set is known:
   mask every user span that collides with a synthesized call/lambda span, emit
   the rest. `task`-style builders (P4) never reach interior emission — the
   resumable-code lowering is not modelled, and the head/binder commits don't
   need it.

Everything else about soundness is inherited machinery: undischarged CE
constraints poison via the existing `mark_incomplete`/deferred-poison backstop,
and read-off stays ground-only.

### CE-D4. When is the builder's type known?

FCS checks the builder expression before translating (§2.1). Our generation
pass mirrors that with two tiers:

- **Generation-time-known** (CE-4): the head resolves (via the resolver's
  `Resolution`) to an assembly module value / in-file annotated binder whose
  type bridges to a ground `Ty` without unification — `async` ⇒
  `AsyncBuilder`. Desugar immediately, generate constraints inline.
- **Solve-time-known** (CE-8): otherwise emit a suspended `CeExpand`
  constraint keyed on the head's type variable — the `HasMember` pattern —
  and desugar+generate at wake, when unification grounds the builder type
  (the in-file `let opt = OptionBuilder()` case). This makes the solver's
  constraint set grow mid-solve; the loop already tolerates that
  (`wake_member` pushes `Eq`s), but the termination argument must be restated,
  which is why it is its own stage.

## 5. What stays deferred (each sound, with its trigger)

- **Custom operations / `query`** — any `[<CustomOperation>]` member, `Quote`,
  or the `query` builder ⇒ whole-CE defer. Trigger to revisit: corpus demand
  for query hover. The *name-resolution* half (resolving `where`/`select`
  keyword uses to builder members, P7) is a separable, inference-free tail.
- **`seq { … }` and comprehensions** — a different FCS path (§2.3); their own
  plan if the corpus demands. The head `seq` use already resolves (it is an
  ordinary function value).
- **`task` / resumable builders** — defer via the extension gate (CE-D2);
  head/binder commits may later be recovered once extension-member *resolution*
  lands (extension-scope-enumeration-plan.md is the dependency), interior
  spans likely never (P4).
- **`BindReturn` / `BindN` / `MergeSources` ladder** — deferred until CE-6.
- **`ReturnFromFinal`/`YieldFromFinal`, `[<DefaultValue>]`-`Zero`, implicit
  yield** — defer on presence (CE-D2); revisit on corpus evidence.
- **Overloaded builder methods** — the `HasMember` wake already routes method
  groups through the OV engine; whatever it defers, the CE defers.
- **Non-block bang binders, object-expression braces** — parser gaps, tracked
  in `docs/parser-plan.md` / `docs/fcs-divergences.md`, unchanged here.

## 6. Stages

Implement this plan with each stage on its own branch, stacked as necessary on
previous branches, so that a reviewer can review each branch in isolation.
Oracle first, then infrastructure, then engine — the OV discipline.

### CE-P0 — prerequisite (separate plan): `Ty` generic args + generic-method wakes

**Dependencies:** none. **Blocks:** CE-4 onward.

The substrate the header names: `Ty::Named` grows an argument list;
unification, rendering, the assembly-signature bridge, and the member wake
learn instantiation (a generic method's typars unified from argument/receiver
types, its return instantiated). Needs its own design doc in the
overload-plan mould — it also unblocks the OV §5 generic deferrals and is the
single highest-leverage piece of this whole area. **Oracle:** the existing
member/overload differentials extended with generic-instantiation cases;
`AsyncBuilder.Bind`-shaped probes.

### CE-0 — probe pinning + the CE differential harness

**Dependencies:** none. **Implements:** §3, the harness for CE-D3.

A new `crates/sema/tests/all/infer_ce_diff.rs` case group (plus its `mod`
line): runs curated CE snippets through `resolve_file` + `infer_file` with the
BCL+FSharp.Core `AssemblyEnv` fixture, parses `fcs-dump types`/`uses`, and
asserts (a) every type we emit inside a CE agrees with FCS's node at that
range, (b) the §3 probe facts as regressions (no builder-method uses; binder
DEFs; head-span/binder-span FCS shapes), so an FCS upgrade that changes the
sink or range conventions fails loudly. Green today because we emit nothing.
**Oracle:** the harness itself (trivially green on all-defer, direction (a)
vacuous), plus the probe regressions (direction (b) non-vacuous immediately).

### CE-1 — body furniture: expression-level `let` and `Sequential` in infer

**Dependencies:** none (parallel with CE-0). **Implements:** the §1 gap that
CE continuations expose; independently valuable outside CEs.

`infer_expr_inner` learns plain (non-`use`, non-`rec`, non-bang)
`Expr::LetOrUse` — binder typed from its RHS (reusing the `let_binding`
machinery's monomorphic path, no generalisation for locals) — and
`Expr::Sequential` — typed as its continuation, first component walked in
check-mode. Today both catch-all-defer, which means *any* function body with a
local `let` loses its binder and body types (and its enclosing binding cannot
generalise); CE bodies are just the loudest victim.
**Oracle:** non-CE snippets through the existing types differential
(`let f x = let y = x + 1 in y * 2` ⇒ `y : Int32` at its span, body typed);
all existing suites green.

### CE-2 — builder-head callee typing (assembly value refs)

**Dependencies:** none (parallel). **Implements:** CE-D4 tier 1.

`infer_callee`/value-reference typing learns the case where the resolver
already resolved an ident to an **assembly module value** with a
non-generic, bridgeable type: emit that type instead of `mark_incomplete`.
Scoped deliberately to nullary `Ty` bridges (`AsyncBuilder`,
`OptionBuilder`-style classes); generic values stay deferred (CE-P0
territory). **Oracle:** `binder-types` differential — `let b = async` ⇒
`b : AsyncBuilder` on both sides; existing member-access differentials
unchanged.

### CE-3 — the desugarer core (pure, unwired)

**Dependencies:** none (parallel). **Implements:** CE-D1 + CE-D2 for the
non-query subset of §2.2 (everything except the CE-D2 defer list).

`crates/sema/src/ce.rs`: `BuilderMethods`, `CeCore`, `CeDefer`, and
`desugar`, with the §2.2 table transcribed — including the range each
synthesized node carries and its synthetic bit (§2.5), because CE-7's shadow
mask reads them. No inference wiring. **Oracle:** property tests —
(1) *splice preservation*: every user sub-expression of the body appears in
the output exactly once, range untouched; (2) *method-set closure*: every
emitted `Call.method` ∈ the probe set the `BuilderMethods` affirmed;
(3) *defer monotonicity*: removing a method from `BuilderMethods` either
leaves the output identical or turns it into `CeDefer` — never a different
successful translation; (4) *binder preservation*: the multiset of
binder-pattern ranges is invariant; (5) determinism. Plus fixture
transcriptions of §2.2 rows (`let!`+`return` ⇒ `Bind`+`Return` nesting, the
`use!` `Using`-inside-`Bind` shape, `while!`'s pre-rewrite, …).

### CE-4 — the straight-line engine: `Bind`/`Return`/`ReturnFrom`/`Delay`

**Dependencies:** CE-P0, CE-1, CE-2, CE-3, CE-0's harness.
**Implements:** CE-D3 commits 1–2, CE-D4 tier 1.

The `App(head, Computation)` route in generation: head's type ground and
`Named` ⇒ entity lookup ⇒ `BuilderMethods` probe (CE-D2 gates) ⇒ `desugar` ⇒
interpret `CeCore` into constraints (each `Call` a `HasMember` method wake on
the builder type; continuation lambdas typed with the existing lambda
machinery; binder `def_type`s from the continuation parameter). Commits: head
span + binder spans only. `async` and in-file-annotated builders with
`let!`/`do!`(non-final)/`return`/`return!` bodies. **Oracle:** CE-0's
differential goes non-vacuous — P1/P2-shaped snippets assert head + binder
agreement both against `types` and (for binders) the resolve-side DEF picture;
defer-shape tests pin every CE-D2 gate firing silently (task, query,
BindReturn-present, extension-surface builders).

### CE-5 — statement forms: `Zero`/`Combine`/`Delay`, loops, `try`, `use`

**Dependencies:** CE-4. **Implements:** the rest of §2.2's unconditional rows.

Sequential CE statements (`Combine`+`Delay`), `if`-no-else (`Zero`), `while`,
`for`, `try/with`, `try/finally`, `use`, `use!`, `match!`, final-`do!` (the
non-`Final`, non-`[<DefaultValue>]` branch), `while!`'s pre-rewrite.
**Oracle:** P5-shaped differential snippets per construct; each construct's
required-method absence defers silently (behaviour tests).

### CE-6 — the applicative ladder: `and!`, `BindReturn`, `MergeSources`

**Dependencies:** CE-4. **Implements:** §2.2 rows 1–2's member-directed
choices; lifts the CE-D2 `BindReturn` defer.

Model `convertSimpleReturnToExpr` (CCE.fs:2704–2792) and the
`BindNReturn`/`BindN`/`MergeSourcesK` selection. **Oracle:** P6-shaped
differentials; a builder-method-subset matrix (with/without `BindReturn`,
varying `MergeSourcesK` depth) asserting we pick FCS's translation — observable
through the range/type picture — or defer.

### CE-7 — interior spans via the shadow mask

**Dependencies:** CE-4 (worth doing after CE-5). **Implements:** CE-D3
commit 3.

Emit user-expression types at unshadowed interior spans. **Oracle:** the CE-0
differential tightened to assert agreement at every emitted interior span on
the curated corpus, including the P2 shadow case (we must *not* emit at
`a + 1`'s span) and the P1 pass-through case (we *must* still emit `x + 1`).

### CE-8 — solve-time builders + the generative differential

**Dependencies:** CE-4 (`CeExpand`), CE-5–7 (generative value).
**Implements:** CE-D4 tier 2; the systematic net.

The suspended `CeExpand` wake for inference-typed builder heads, with the
restated termination argument. Then the house endgame: a generative
differential in the `overload_corpus_diff` mould — a bounded grammar of CE
bodies × builder method subsets, rendered to source, both sides diffed via the
resident oracle pool; assert the two-sided property (we-commit ⇒ FCS-agrees;
plus the defer-shape floors so coverage is observed, not assumed).
**Oracle:** the sweep itself, with commit floors à la OV-9's `MIN_COMMITS`.

### Data-gated tail (no stage numbers; triggers in §5)

Custom-op keyword name resolution; `Source`/`Quote`/`Run` builders beyond the
`Delay`-only wrap; `task` head-span recovery behind extension-member
resolution; `seq`/comprehensions; langversion modelling from the `.fsproj`.

## 7. Checklist for the implementing agent

Before writing engine code for a stage, run its snippet through `fcs-dump
types` and `uses` and read what FCS actually produced — the §3 probes show the
conventions are not guessable. When a differential fails on a range, suspect
the shadow mask or a range-synthesis rule (§2.5) before the typing. When you
want to commit a translation choice, name the `BuilderMethods` fact that
selects it; if the fact could be perturbed by an in-scope extension or a
langversion, that is the CE-D2 defer, not a judgement call. Never desugar with
a partially-known method set. Keep every defer silent: a deferred CE is
invisible; a wrong hover type inside one ships to every user.
